# homeway-rs 传输层换代 Roadmap（WG → QUIC · 跨期进度真源）

> **新会话续接协议（三步）**：①读本文件；②按「状态总览」找到第一个未完成期，读该期小节
> （目标/范围/判据/评审门/退出口）；③按「每期执行协议」派发子 agent 执行，主会话只做调度
> （读指针→派发→收摘要→更新本表→本地 commit）。用户说「继续 QUIC 换代 / 接着干」即指此协议。
>
> **用户拍板（2026-10-08）**：①**无兼容包袱**——产品未发布，不与旧后端 / 旧 APP / 旧 wire 互操作
> （token 格式、wire 协议、身份体系均允许破坏性变更）；②**功能全保**——全局代理 / 文件管理 /
> 终端 / 端口转发一个不能少；③方案原则 = **简洁、高效、简单**。
>
> **本文件定位**：传输层换代程序的唯一进度真源；与 `ROADMAP.md`（R0–R8 平移程序，已收官）、
> `docs/REVIEW-ROADMAP.md`（Q 批整改）并列。硬规则 / 工程原则 / 评审两道门继承 `AGENTS.md`（勿重述）。
>
> **隔离条款（沿用，最高优先级）**：现役出口不碰（测试一律本地私有实例）；`homeway` / `tier`
> 两仓跟踪文件只读；`baseline/` 只读（冻结）；不发 tag / 不发 Release / 不建 PR（需用户显式指令）；
> `target/` / `baseline/` / `bin/` 不入库。
>
> **判据政策**：判据行非冻结物——任何变更必须登记 `docs/INTEROP-CRITERIA.md`「判据变更记录」
> 并随同批 commit。本程序是**整表重写级**变更，登记预算见「判据与登记预算」节。
>
> **更新协议（硬规矩）**：每期收口后，主会话**同一次提交**里更新「状态总览」勾选与本文件
> 「下一步指针」；期完成时把判据证据（日期 + 实测/测试输出摘要）写进该期小节。
>
> **开工基线**：本文件立稿时 HEAD = `e3b1d8b`（Q-I 收口）；落稿时已推进至 `72f1325`
> （**Q-F-B 收口**：portfwd 真监听器落地，tier `port-forwarding` SHALL 转达标；`Q-K`/`Q-L`
> 进行中）——**每期开工前先复验工作树**（见「每期执行协议」第 1 条）。

---

## 总体目标

把三角色的传输层从自研 WG（boringtun + 自管 UDP 候选/镜像/腿表/恢复阶梯）整体替换为**标准 QUIC
承载**：每条设备一条 QUIC 连接，`DATAGRAM` 走 IP 包（全局代理），`STREAM` 走服务（files / term /
speedtest / portfwd / 巡检），控制流走登记与保活。功能语义（拦截、DNS 代答、分流、端口转发、
终端 / 文件、漫游 / 恢复、设备表、Token 凭证）全部保留；形态上把「自研传输补偿代码」替换为
协议原生能力（迁移 / 0-RTT / 可靠流 / 拥塞控制）。

### 非目标

- 不做双栈长期共存（WG 路径仅在 M1–M5 作为**本仓内 A/B 对照**存在，M5 删除）；
- 不追新功能（Q-F-B 的 portfwd 真监听器已在 `72f1325` 落地；M4 只做**承载适配**与 spec 不回退核验）；
- 不改中继控制协议（`relaywire` 原样保留；中继侧**不引 TLS/QUIC 依赖**）；
- 不动 tier App 的产品结构（C-ABI 导出面与 VpnConfig 语义按现状保留，必要调整由 tier 跟做；
  无兼容包袱，但不主动扩大改动面）；
- 不重做已归档的术语/文档体系（README/AGENTS 的技术底座行在 M7 统一更新）。

## 立项依据（已验证事实，勿重复调研）

本地实验台实测（2026-10-08，Mac M2 / 8 核 / loadavg 1.0–2.7；三臂同刻交替、每臂 2–3 轮，
波动 <1%；原始数据与复现脚本见附录 A）：

| 维度 | 实测 | 结论 |
|---|---|---|
| 每包 CPU（1280B 往返，release+LTO） | raw 4.9 / WG+ring 10.7 / **WG+shim 14.6** / **QUIC 12.5 µs** | QUIC 比现役（WG+RustCrypto 垫片）**省 ~15%**；比 WG+真 ring 贵 ~17% |
| 线开销 | WG **32B**；QUIC **30.2B**（+ACK ≈ +6%） | 基本持平 |
| QUIC DATAGRAM 上限 | MTU1200→**1162**（装不下 1280 内层）；MTU1400→**1362**（可） | 必须 `initial_mtu=1400`+DPLPMTUD；窄路径需降级策略 |
| OHOS 交叉（quinn+rustls+ring0.17） | **编译通过**（ELF aarch64；须显式 `CC=NDK clang`） | 依赖面可行；ring0.17.9+ 已支持 OHOS ⇒ **ring-shim 可删** |
| 体积（OHOS cdylib） | 空壳 323.6KB；+QUIC（死码消除后）778.3KB；+真实引用 1825KB；现役 `.so` **2213.7KB** | QUIC 栈绝对量 ≈0.45–1.5MB（规划取 1.5MB 保守值） |
| 内存（`vmmap` footprint 三轮中位） | raw 944K / WG+shim 1008K / **QUIC 1216K**；负载期不增长；每连接 QUIC ≈37.6K / WG peer 11.0KB | 单连接 +208K；32 设备 +1.2MB（可控） |

产品侧既有锚点（勿重复测量）：`PERF-AB` §9.12（TUN 路径 Rust/Go 0.87–0.94，**无独立缺口**）；
§9.7-bis（残余瓶颈 = 手机侧单核 + ACK 时钟）；§3（client 常驻 3.4MB）。

源码级依据：

- 出口明文唯一接缝 = `server/device.rs::decapsulate → StepOut::PlainV4`；客户端唯一发送收口 =
  `wtransport::Bind::send_wg` ⇒ **承载替换的耦合面很窄**（拦截与应用层消费的是「明文 IP 包」类型）；
- `rustls 0.23` 支持 RFC 7250 裸公钥（`requires_raw_public_keys` + `AlwaysResolves*RawPublicKeys`
  + `verify_tls13_signature_with_raw_key`）⇒ 身份不需要 X.509，**token 里的公钥直接钉定**；
- `quinn 0.11`：服务端 `migration` 默认 `true`、客户端 `Endpoint::rebind()`、`max_datagram_size()`、
  DPLPMTUD 默认开 ⇒ 换网迁移与 MTU 探测是**协议原生**，替代自研镜像 / 采纳 / 双发宽限。

## 目标架构（摘要；图示见附录 E）

```
App（tier，结构不变）
 └─ libclientcore.so
      ├─ 同步面（现状保留）：TUN fd 直通 / 服务桥 UDS / portfwd 监听 / 巡检 / 状态 JSON
      └─ QUIC 岛（新增；单线程 tokio current_thread + 命令通道）
           ① 连接管理（并行赛跑 / rebind 迁移 / 重连）
           ② 控制流（登记 + 保活）
           ③ 流分发（1B tag：files / term / speedtest / dial / probe）
                    │
        QUIC over UDP（一条连接 = 一台设备）
                    │
中继（relay 角色）——【零改动，纯不透明 UDP 转发；不引 QUIC 依赖】
                    │
出口 homeway-cli
 ├─ quinn server 端点（单 UDP 端口；migration=true；MTU1400 + DPLPMTUD）
 ├─ DATAGRAM → intercept【保留】→ DNS 代答 / 过境重拨（全局代理）
 ├─ STREAM[tag] → files / term / speedtest / dial（portfwd 目标）/ probe
 └─ 控制流 → 设备表（连接 = 设备；按 devTag 采纳/替换/吊销）
```

关键决策：①身份 = 出口 RPK（RFC 7250）+ 客户端 token 证明（Hello/Challenge/Proof）；
②迁移 = `rebind()` + 服务端 migration，替代候选镜像/采纳/双发宽限；③同步/异步隔离 = QUIC 岛
独占一个 runtime，对同步面只暴露命令通道（与今日 `wgcore` 单驱动线程同构）；④内层 MTU 1280 保留，
QUIC MTU 1400 + DPLPMTUD，窄路径超限丢弃 + 计数；⑤中继零改动；⑥客户端 smoltcp（stackb）退役，
出口 smoltcp（intercept）保留——它服务的是「任意目的地址」的全局代理，与承载方式无关。

## 状态总览（手维护）

| 期 | 内容 | 状态 | 进度 |
|---|---|---|---|
| **M0** | 骨架与依赖面（quinn/rustls/tokio 落地 + QUIC 岛设计 + 实验台转正 + 基线登记） | 未开工 | 0/4 |
| **M1** | QUIC 承载 + 全局代理（DATAGRAM + 迁移/赛跑） | 未开工 | 0/4 |
| **M2** | 身份、设备表与准入（RPK + token 证明 + 抗放大） | 未开工 | 0/4 |
| **M3** | 服务流迁移（STREAM tag；客户端 stackb 退役） | 未开工 | 0/4 |
| **M4** | portfwd 承载适配（dial 缝换 STREAM；spec 不回退） | 未开工 | 0/3 |
| **M5** | WG 路径删除与收束（大删码 + 判据全表登记） | 未开工 | 0/4 |
| **M6** | 真机与性能终验（2×2 + 归因 + PERF 报告） | 未开工 | 0/3 |
| **M7** | 生产切换与文档收束（用户触点） | 未开工 | 0/3 |

## 下一步（当前指针）

> **本节 = 唯一的「现在该干什么」指针。** 主会话只认这里。

1. **程序未开工**——M0 等待用户「开工」指令（本程序节奏 = **逐期等指令**，与 Q 批同款；
   每期收口后停一条等指令，不自动接棒）。
2. 开工前置：并发在途批（`Q-K` / `Q-L`）落地并复验工作树；`cargo test --workspace` 绿基线。
   **Q-L 已落地并随本批 commit 入库**（批记录 = `docs/reviews/QL.md`）；待主会话在 `REVIEW-ROADMAP.md`
   补 Q-L 登记行后视为完全收口（M1 开工前用 `git log` 复核）。**M1 开工须逐条处置「M1 开工前置检查项（Q-L 交接）」**
   （见 M1 节末小节；**Q2/Q3/Q4/Q5 四条「须显式立条」不接则上报主会话裁决，绝不允许静默**；
   M1 收口记录须含 Q1–Q12 逐条处置表）。
3. 每期首棒任务书 = 该期小节 + 「每期执行协议」（M1 另加「M1 开工前置检查项（Q-L 交接）」）。

## 每期执行协议（子代理按此跑，主会话按此核对）

1. **复验**：读本文件该期小节 + 回源码重定位（行号会漂）；设计棒开工时工作树必须干净
   （或已知并发批在途并明确隔离面）。
2. **设计门（第一道）**：产出 `docs/reviews/M<N>-design.md`（方案 / 涉及文件 / 风险 / 测试计划 /
   **判据行影响** / 体积·性能·内存预算）→ 调 `reviewer` skill 走 dsh 外部评审（prompt 指路 =
   设计文档 + 代码路径，**不喂结论**；结果落 `/tmp/dsh-review/r<N>.XXXXXX/`）→ 逐条处置
   （认同改 / 不认同给证据）→ 过门记录（评审原文摘要 + 处置表）写入设计文档。
3. **实现**：按设计实施 + 新增/修改测试；`cargo test --workspace` 全绿 +
   `cargo clippy --all-targets -D warnings` 无新告警；涉判据行变更**必须**同步
   `docs/INTEROP-CRITERIA.md` 变更附录；涉门槛的期**必须**跑 `tools/quic-ab.sh`（M0 转正）留证。
4. **代码门（第二道）**：调 `reviewer` skill 对 commit 范围做第二轮 dsh 评审 →
   **高危必改或显式豁免登记** → 记录 `docs/reviews/M<N>.md`（两轮意见原文摘要 + 逐条处置表 +
   测试/判据/实测证据）。
5. **收口**：更新本表状态 + 中文 commit（每工作单元一 commit，main 直推按 AGENTS）；
   主会话核对「评审记录在 + 证据在」后勾选，并**停一条等指令**。
6. **失败/升级**：dsh 不可用 → 结构化双角色自评并注明；实测/源码与设计矛盾 → 先在设计文档
   登记再改；重大分叉上报主会话裁决。

## 评审协议（两道门，每期必过；与 AGENTS / ROADMAP 同源）

1. **设计门**（每期开工前）：技术评审。checklist 恒含——功能等价面（全局代理 / 文件 / 终端 /
   端口转发逐项）、边界与错误面、并发与生命周期、**残留 WG 语义隐含依赖**、地道 Rust（禁直译
   痕迹 / 多余锁 / 字符串错误）、安全面（身份 / 准入 / 重放）、预算（体积 / 性能 / 内存）可测性。
2. **代码门**（实现 + 自测绿后）：对 commit 范围评审；清单同上 + 「测试是否真覆盖判据 +
   门槛是否有实测证据」；高危项必须整改或登记豁免理由后才算该期完成。
3. **记录位置**：`docs/reviews/M<N>-design.md`（设计门）、`docs/reviews/M<N>.md`（代码门）。
4. **dsh 仓内副作用文件**（评审 agent 自行落盘）：按 Q 批先例**转存 `/tmp` 后删除**，不进库。
5. 主会话只核对「评审记录存在 + 判据/实测证据在报告里」，不重读代码。

## 用户触点清单（须显式点头，其余全自动）

- **每期开工**（逐期等指令）；
- 动现役出口（M7 滚动升级；测试一律本地实例，永不）；
- tier 侧动作（pin 前进、App 侧适配、tier 文档修订交付）；
- **拍板点**：token 格式（M2 设计门后）、内层 MTU 降级开关的形态（M1）、0-RTT 是否启用（M2/M3）；
- 发 tag / Release / PR（本程序预期不需要，如需要另行指令）。

---

## M0 骨架与依赖面（估 2–4 会话日）

**目标**：依赖面就位、QUIC 岛结构定稿、实验台转正、三基线（体积/性能/内存）登记——
**不改产品行为**。

**范围**：

- 依赖：workspace 引入 `quinn`（`default-features=false` + `rustls-ring` + `runtime-tokio`）、
  `rustls`（ring provider）、`tokio`（rt/net/time）；`ring 0.17` 升级与 `ring-shim` 退役预案
  （boringtun 0.6 钉 0.16——先共存，M5 随删除收口）；`.cargo/config.toml` / CI 补
  `CC_aarch64_unknown_linux_ohos`（实测踩坑：不设 = cc-rs 用系统 cc ⇒ ring C 代码找不到 `assert.h`）。
- **QUIC 岛设计**（本程序核心结构决策）：单线程 `current_thread` runtime；命令通道协议（沿用
  `Cmd` 形态：TunAttach / TunPacket / StreamOpen / StreamWrite / …）；与世代生命周期对接
  （`finish_generation` / 停止预算 / panic 边界）；明令**禁止 async 泄漏进同步面**。
- 实验台转正：`tools/quic-ab.sh`（三臂 CPU / 包开销 + 体积矩阵 + RSS/footprint 采样；口径 =
  附录 A）+ CI 门（OHOS 交叉 check 含新依赖）。
- 基线登记：现役 `libclientcore.so`（2,213,744B，2026-10-07 构建）/ 每包 CPU / 内存三值入册。

**判据**：`cargo test --workspace` 全绿（行为零改动）；clippy 0；OHOS/musl/linux 三目标 check
全绿；`tools/quic-ab.sh` 一键复现附录 A 数字（±10%）；体积/内存基线入册。

**评审过程**：

- 设计门：`docs/reviews/M0-design.md`（依赖矩阵 + 岛结构 + 生命周期对接 + 预算；专项 =
  异步/同步边界与 panic 面）→ dsh → 处置表 → 过门记录。
- 代码门：骨架 + harness 提交范围 → dsh（专项 = harness 口径是否与附录 A 一致）→
  `docs/reviews/M0.md`。

**退出口**：依赖与骨架合入 main；三目标 CI 绿；基线与实验台数字对齐；无产品行为变更。

## M1 QUIC 承载 + 全局代理（估 5–8 会话日）

**目标**：TUN 流量经 QUIC DATAGRAM 到出口 intercept，功能等价全局代理；换网迁移原生可用；
WG 路径保留为 A/B 开关（默认新路径）。

**范围**：

- 出口：quinn server 端点（单 UDP 端口；`migration(true)`；`initial_mtu(1400)` + DPLPMTUD；
  datagram 缓冲上限对齐今日 `SO_SNDBUF/SO_RCVBUF=4MB` 量级）；DATAGRAM ↔ intercept 直通
  （接缝 = 今日 `device.rs` 的明文面）；回程 intercept → DATAGRAM（ACK/窗口背压语义对齐）。
- 客户端：QUIC 岛并行 `connect()` 赛跑（LAN / 公网 / 中继候选，替代候选镜像）；`rebind()` 迁移
  （WiFi→蜂窝）；TUN fd ⇄ DATAGRAM 直通；内层 1280 保留 + `max_datagram_size()` 检包
  （超限丢 + 计数行）；巡检/需求信号以 `STREAM[probe]` 占位（M3 定型）。
- 中继：控制面零改动；数据面复测透明转发（含 DPLPMTUD 探测包与 1280+ 数据报；**200pps/句柄
  预算与隧道包尺寸**须实测复核）。
- 观测面：`link:` 行 / 状态 JSON 适配（via=direct/relay 语义保留）；A/B 开关（env/config）。

**判据**：

- 真机：浏览器 / 任意 App 经隧道上网全通（层 0 对照）；**WiFi→蜂窝切换连接保持**（无重连、
  出口设备表不新增条目）；
- `quic-ab.sh`：每包 CPU ≤ 现役 WG+shim（实测有 15% 余量）；线开销 ≤ 40B/包；
- 真机吞吐（同刻 A/B）：热态 ≥ 0.95× 现役；冷/热 ≥ 0.70（PERF-AB 门）；
- 窄路径（<1340）：超限丢弃可观测、不静默；DNS 经隧道路径全通（`dnsAddresses` 指向出口 IP 不变）。

**评审过程**：

- 设计门：`docs/reviews/M1-design.md`（迁移/赛跑裁决/MTU 降级/中继预算/背压语义）→ dsh → 处置。
- 代码门：`docs/reviews/M1.md`（岛实现 + 出口端点 + A/B 开关；专项 = DATAGRAM 丢包与
  「全局代理跑的是端到端 TCP」的相互作用——确认丢包语义与 WG 同档）。

**退出口**：全局代理等价 + 迁移通过 + 三项性能门过（或差异登记）；A/B 一键回退 WG 可用。

### M1 开工前置检查项（Q-L 交接，2026-10-08）

> **来源** = **Q-L 批**（治理批）的**显式交接清单**：Q 批把「说好下一批做但没做也没说不做」的挂空项
> 逐条判死；本清单 = 其中「**不在 Q-L 修**」的全部条目——**多数是传输耦合项**（会被 M5 删除/重写，
> 或属 WG·relay·公共端点·wire 面者），另有两条（Q8/Q12）属 intercept **保留面**（非耦合）但需
> 交接定夺。**完整理由与一手证据 = `docs/reviews/QL.md` §QUIC 交接 + `docs/reviews/QL-design.md` §3**
> （本小节只给可执行摘要）。
>
> **收件人与纪律（2026-10-08 订正——M0 已完成）**：M0（骨架/依赖/实验台/基线）已由 QUIC 程序完成
> （见该程序 worktree 的 `docs/reviews/M0.md`；**本仓 main 副本的状态总览/「下一步」第 1 条尚未
> 反映——该程序在兄弟 worktree 推进，合回 main 时由其统一更新，此处以程序侧记录为准**），
> 本清单的落点因此是 **M1**：① M1 设计门（`docs/reviews/M1-design.md`）
> **逐条点名处置**——三种落点：**接**（写进 M1 范围）/ **不接**（登记差异/不做，写明理由）/
> **上报主会话裁决**；② 若 M1 设计门在本清单合入前已过，则主会话把本清单并入 **M1 实现任务书**
> （或顺延为 M2 前置）逐条点名，不得静默跳过。**标「须显式立条」的四条（Q2/Q3/Q4/Q5）尤其不许
> 静默**：M1/M2 现范围小节**没有**对应 bullet，且它们**不会**被 M5 删码自然消除——不显式认领就
> 退化成新一轮挂空（这正是 Q-L 要消灭的形态）。
>
> **可验收点（M1 收口必查）**：`docs/reviews/M1.md`（代码门记录）须含 **Q1–Q12 逐条处置表**；
> 缺任一「须显式立条」条目的处置 = M1 不收口。

| # | 项 | 建议归属期 | M5 删码是否自然消失 | 备注 |
|---|---|---|---|---|
| Q1 | 中继全局 assoc 上限 `MAX_ASSOCS_TOTAL=1024` 的**字节预算化**（`relay/mod.rs:61/247/939`） | **M1**（200pps/句柄预算实测复核点） | **否**（中继零改动） | 裁决 = 字节预算化 ⇒ 属 relay 代码改动（与「中继零改动」冲突）⇒ 须 M1 显式扩范围或另立期 |
| **Q2** | 中继**客户端方向 v6**（主监听口 v4-only；`relay_cli.rs:103-105` + `relay/mod.rs:388-403`；Go `relay.go:289-319` 双栈） | **M1** | **否** | **须显式立条 + 范围确认**：① 扩范围承接双栈，或 ② 退回「登记差异（不做）」——二选一，上报主会话 |
| **Q3** | `public_endpoint.txt` 两处**写失败静默**（`engine.rs:1476/1630`；Go `publicendpoint.go:126/226` 有告警） | **M1** | **否**（公共端点面保留） | **须显式立条**；退路 = 主会话裁决拉回任一批次 |
| **Q4** | `listen_port.txt` 写失败 **Rust 致命（`?`）/ Go 非致命**（`engine.rs:418`；Go `role.go:95`） | **M1** | **否** | **须显式立条**（触出口启停语义，非纯日志文本） |
| **Q5** | `--public-endpoint` **CLI flag 值域零校验**（`serve_cli.rs:694-695`；Go 前端报错 `cli.go:98-105`、守护期告警清空 `serve.go:113-121`） | **M1/M2**（端点面复核；token 口径随 M2 定稿） | **否** | **须显式立条** |
| Q6 | Q-B F3/F10 的 UDP 上行门 / `deliver_udp53→udp_drop` 链 **e2e 未做**（`QB.md` §6-1） | **M1**（风险 #5 背压专项测试一并） | 部分（intercept 保留、入口换 DATAGRAM） | M1 背压专项测试面 |
| Q7 | 性能残余（M5 删除面）：`wtransport/bind.rs:287`、`server/bind.rs:757`、`server/bind.rs:1020` | **M5** | **是** | 无动作，随删码消失 |
| Q8 | `reactor_turn→poll(0)` 每拍固定税（`server/intercept/mod.rs:2211`） | **M1（复测后重定动作）** | **否**（intercept 保留） | 不得按「M5 消失」处理（设计门 Q-1 订正） |
| Q9 | `wgcore` 站点 Engine 级测试空档（`QI.md:194`） | **M5** | **是**（`wgcore` 删除；共用类型保留） | 无动作 |
| Q10 | `PERF-AB` §9.15.6 四条（E13 `--loopback-only` token / 发送线程默认 on 重开条件 / 8s 口径 / Linux `sendmmsg`+五跳管线合一） | **M1/M5** | 是（端点竞速条除外） | 端点竞速 ⇒ M1 重定；发送线程/管线/`sendmmsg` ⇒ M5 删码即消失；「8s」为矩阵口径（非缺口）；**`udpbatch.rs`（sendmmsg 载体，372 行）未在 M5 删除清单/附录 B** ⇒ 建议 M5 一并登记（代码门 低5） |
| Q11 | `files --host` 失败（原 QIt §7.2-4 挂点） | **已闭合（Q-L L7，2026-10-08）** | —（不涉及） | 复验判定 = **非缺陷**：`files --host` 默认形态全绿（list/stat/put/get × 1 MiB/64 MiB，sha256 一致）；失败只在 `--rate-limit 0` 下复现，属双向文档化的盲节流边界（Rust `files.rs` 发送端速率义务 + daemon 上行工位 32 帧/512 KiB；Go 同形 `files_cli.go` + `internal/control/stream.go` + 单测 `TestStreamUpstreamOverToleranceStillGone`）⇒ **M3 无需承接**，登记知悉即可 |
| Q12 | `tx_frag` / 分片感知的跨报文乱序 E2E（`QK.md` §6-8） | **M1（可选，不作为立条）** | **否**（intercept 保留） | M1 若重测分片可顺带 |

> **明确不进交接清单**（同样判死，避免误领）：`relay.listen` 接受集放宽（A5）、中继 JSON 遥测
> 通道（N5）、中继状态面字段（N11）——都在「中继零改动」的不动面上，属**登记差异/不做**；
> `dnsface` 两处小 `Vec`（N9-③）属 intercept 保留面的**量小**项，登记不做。

## M2 身份、设备表与准入（估 3–5 会话日）

**目标**：出口 RPK 身份 + 客户端 token 证明落地；设备表（连接 = 设备）语义与今日等价。

**范围**：

- 出口身份：Ed25519 RPK（RFC 7250，rustls `requires_raw_public_keys` + `AlwaysResolves*`）；
  公钥进 token（**token 格式变更**——无兼容包袱，登记即可）；客户端钉定校验。
- 客户端证明（控制流）：`Hello{devTag, pub}` → `Challenge{nonce}` → `Proof{HMAC(secret,…)}`
  （MAC 纪律参考 `relaywire` 四族）；出口设备表按 devTag 采纳/替换/轮换/吊销（`table.rs` 语义
  沿用，键 = devTag；TTL / grace / max_devices 保留）。
- 准入：抗放大（启用 quinn Retry/token 或等价面）；握手限流迁移；未认证连接不占设备额度。
- 威胁模型文档（token 泄露 / 重放 / MITM / 重连洪泛 / 资源耗尽）随设计定稿。

**判据**：采纳/替换/吊销/表满/淘汰对照测试全绿；错 token / 错 nonce 拒绝；secret 不落盘；
重连洪泛有界（限流行可观测）；E6–E9 / E18 语义对照（行文可重写，语义等价或登记差异）。

**评审过程**：

- 设计门：`docs/reviews/M2-design.md`（**安全专项**：RPK 取舍 / nonce 与重放窗 / 抗放大 /
  token 格式 / 威胁模型）→ dsh → 处置。
- 代码门：`docs/reviews/M2.md`（含威胁模型逐条验证）。

**退出口**：安全门过；设备表对照全绿；token 格式变更已登记 + 上报 tier。

## M3 服务流迁移（files / term / speedtest / 巡检）（估 4–6 会话日）

**目标**：核心自连全走 STREAM；客户端 smoltcp（stackb）退役。

**范围**：

- STREAM 协议：首字节 tag（1=files / 2=term / 3=speedtest / 4=dial / 5=probe）+ 既有应用层帧
  **原样**（term HSP、files proto、speedtest 帧逐字节不变）；出口按 tag 分发到对应服务（服务
  入口从 UDS accept 换成 stream 适配器，**应用层零改动**）。
- 客户端：`bridge_host` 的 `DialFn` 改开 STREAM；`session/recover` 阶梯重写（断线 = 重连/迁移；
  不再有 R1/R2/R3 档位语义）；巡检 = `STREAM[probe]`；删除 stackb 与 5 处消费点
  （`tun_exec.rs:238/334/349`、`session/mod.rs:648/667`）；「虚拟端口」（7802/7724/7803）→ tag。
- 出口：intercept 的「豁免命中端口 → UDS」分支退役（只剩 DNS:53/:5300）。
- 地址派生收窄：保留 `tunIp`（App 接口地址）与出口常量 IP（DNS 目标）；**栈 B 派生地址退役**。

**判据**：App 真机 files / term / speedtest 全绿（matrix 冒烟 + E2E）；term 帧 / 键编码 /
fixtures 向量逐字节不变；客户端依赖树不再含 smoltcp（脚本验证）；DC14 / DC15 / CA1 / CA4 / CA5
语义对照。

**评审过程**：

- 设计门：`docs/reviews/M3-design.md`（tag 分发 / 背压 / 错误面 / 阶梯重写 = 连接策略变更）→ dsh。
- 代码门：`docs/reviews/M3.md`（专项 = 删 stackb 后的遗留假设，如「环回不经隧道」语义是否仍成立）。

**退出口**：服务面全绿 + stackb 删除合入 + tier `connection-lifecycle` 修订稿交付。

## M4 portfwd 收口（估 1–2 会话日）

**目标**：端口转发全形态经 `STREAM[dial]`；Q-F-B（`72f1325`）已落地的真监听器面**只做承载适配
与 spec 不回退核验**（监听器 / 阀 / 两阶段 install / `FlowGuard` 等结构逐字保留）。

**范围**：

- 拨号缝换轨：`PfRuntime` 的 dial 缝（今为 `session_connect_target` → stack B `connect_deadline`）
  改开 `STREAM[tag=dial]{dst}` → 出口拨号（「出口自己」= 回环 / 出口 IP 语义保留）；
- 四形态 target 文案（`pf_target_text`）与失败归因（回环拒绝 / 未监听 / 拨号失败）在新承载下核验；
- 状态 JSON / rc 对 tier `port-forwarding` spec 全条复核（Q-F-B 已达标，**不得回退**）。

**判据**：tier `port-forwarding` spec 全条复核**不回退**（Q-F-B 达标态保持）；端到端（本机 + 真机）全绿；
`STREAM[dial]` 与 Q-F-B 的阀 / 计数 / 热替换语义逐条对照。

**评审过程**：

- 设计门：`docs/reviews/M4-design.md`（若仅适配套用 M3 设计门附条，否则独立）→ dsh。
- 代码门：`docs/reviews/M4.md`。

**退出口**：拨号缝换轨完成；spec 达标态不回退；Q-F-B 残余 14 条中与承载相关的条目对照登记
（`docs/reviews/QFB.md`）。

## M5 WG 路径删除与收束（估 3–5 会话日；大删码期）

**目标**：WG / wtransport / frame / reg / ring-shim 全量退役；intercept 收窄；判据全表登记；
代码净减可审计。

**范围**：

- 删：`wgcore`（除 QUIC 岛共用类型）、`wtransport`（bind / frame / reg / endpoint_cache /
  domain_eps）、`server/bind` 腿表族、`server/device.rs`、`server/relayleg.rs`、
  `session/recover` 旧档、`tools/ring-shim`、fixtures（identity / psk / reg / endpointcache
  退役登记；relay / stun 按实际改动画线）。
- intercept 收窄：DATAGRAM → 过境 + DNS 两径；`served_ports` 简化。
- 观测面重写：`tunStatusJSON` / link 行 / 判据行按新语义定稿 + `INTEROP-CRITERIA` 全表登记
  （C/E/X/DC/CA 家族逐行：改/删/新增；见「判据与登记预算」节）。
- 体积 / 内存终值实测入册（`quic-ab.sh` + 真机构建）。

**判据**：脚本化残留扫查（无 WG / wtransport 引用）；全量测试绿 + clippy 0；净删行数登记；
体积 ≤ 3.8MB（预算 = 现役 2.21MB + QUIC ≈1.5MB − 删除面；超预算须设计门显式登记理由）。

**评审过程**：

- 设计门：`docs/reviews/M5-design.md`（删除清单 + 残留依赖排查法 + 判据登记表）→ dsh。
- 代码门：`docs/reviews/M5.md`（专项 = **WG 语义隐含依赖扫查**：任何「以前靠 WG 特性兜住」的
  假设，如源校验 / 漫游学习 / 腿回程）。

**退出口**：删除清单全清；判据登记全量同步；体积 / 内存终值入册。

## M6 真机与性能终验（估 2–4 会话日）

**目标**：真机 2×2 终验（WG 旧核 vs QUIC 新核）+ 归因 + 报告入库。

**范围**：

- 真机 2×2 终验（WG 旧核 vs QUIC 新核；FMR0224116011480 沿用；冷/热、WiFi / 蜂窝、经中继）；
- 层 0 对照 + CPU / 耗电采样；若不及 WG，逐层归因（口径 = PERF-AB §9 方法论）；
- 漫游 / 恢复实录（行样例）+ `PERF-AB` 新增节入库。

**判据**：门槛表（见「性能/体积/内存门槛」节）全过或差异登记；`PERF-AB` 新增节入库；
漫游 / 恢复实录（行样例）。

**评审过程**：

- 设计门：`docs/reviews/M6-design.md`（测量口径**预登记**、可 falsify）→ dsh。
- 代码门：`docs/reviews/M6.md`（插桩清理）。

**退出口**：报告过门；残余登记 + 进入 M7 的决策点。

## M7 生产切换与文档收束（估 2–3 会话日 + 用户触点）

**目标**：两台生产出口滚动升级（用户触点）+ tier pin 前进 + 文档收束。

**范围**：

- 滚动升级两台生产出口（`DEPLOY-RUST-EXIT.md` 形态；含回滚步骤与失败预案）；
- tier 触点：pin 前进 + `connection-lifecycle` 定稿交付 + App 侧适配清单；
- 文档收束：README / AGENTS「技术底座」更新（boringtun→quinn、ring-shim 退役）+ CHANGELOG +
  本文件标记收官。

**判据**：生产真机烟囱全绿；文档指针一致（AGENTS 速查表 / README / 本文件）；判据登记全。

**评审过程**：

- 设计门：`docs/reviews/M7-design.md`（切换步骤 / 回滚步骤 / 失败预案）→ dsh。
- 代码门：`docs/reviews/M7.md`（文档一致性）+ 主会话终检。

**退出口**：换代完成；本文件标记收官。

---

## 判据与登记预算（INTEROP-CRITERIA / fixtures / tier）

| 面 | 涉及 | 期 | 动作 |
|---|---|---|---|
| **C 系列**（客户端） | C2(wgcore 就绪)/C4(MIRROR)/C5(赛跑)/C6(路径)/C10(link)/C11(RECOVER 族)/C13(候选)/C14(出口能力)/C15(RREG) | M1/M3 | 重写（语义保留：via / endpoint / rtt / 恢复时间线）；C1/C3/C8/C9 视语义改词 |
| **E 系列**（出口） | E5(拦截就绪)/E10–E12(拦截行)/E14/E17(服务就绪)/E21(绑卡)/E23(新源) | M1/M5 | 重写（拦截语义不变，措辞去 WG）；E4/E6–E9/E18/E22 复核（预期不变或微调） |
| **X1**（relay） | 注册腿 / 控制面 | M5 | 复核（预期不变——中继零改动） |
| **DC/CA 族** | DC14/DC15（term 远程）、CA1/CA4/CA5（forward/socks） | M3 | 复核（应用面行为保留，行文可能不变） |
| **fixtures** | identity / psk / reg / endpointcache | M5 | 退役（登记）；`tunnel_addr` 部分样本（栈 B 地址）退役；relay / stun / term / files / surface 保留 |
| **tier 文档** | `connection-lifecycle.md`（恢复阶梯节） | M3/M7 | 重写（连接策略变更须同步——tier 侧触点） |
| **词表** | `tools/check-vocab.sh` 五族 | M0 | 预判不受影响（传输面不在五族内）——M0 实测确认 |

## 性能 / 体积 / 内存门槛（预登记；方法 = PERF-AB 口径 + `quic-ab.sh`）

| 维度 | 门槛 | 方法 |
|---|---|---|
| 每包 CPU | ≤ 现役 WG+shim ×1.0 | harness 三臂（附录 A 口径） |
| 线开销 | ≤ 40B/包 | QUIC oneway 精测（服务端 `udp_rx` 口径） |
| 真机吞吐 | 热态 ≥ 0.95× 现役；冷/热 ≥ 0.70 | 同刻交替 A/B（PERF-AB §1/§9 口径） |
| 换网迁移 | 连接保持（无重连）；出口设备表不新增条目 | 真机 WiFi→蜂窝 |
| 断线恢复 | 出口重启恢复 ≤ 3.5s（现役 R1 命中 3.126s 量级） | 故障注入（沿用 R2 批手法） |
| 体积 | OHOS `.so` ≤ 3.8MB（净增 ≤ +1.5MB，删码后复测） | size 矩阵（OHOS cdylib） |
| 内存 | 单连接 ≤ +256K；每设备 ≤ 64K；32 设备 ≤ +2MB | footprint 三轮中位（harness） |
| 丢包可观测 | DATAGRAM 超限 / 丢弃有计数行，不静默 | 窄路径注入（MTU<1340） |

## 已知 flake 登记（沿用 Q 批表；本程序增量）

- **随 M5 删除除名**：`wgcore::stackb::*`（墙钟断言）——WG 退役后该测试移除；
- **沿用有效**：`daemon::tests::*` 时序族、`term::service::tests::attach_size_applies_to_pty`、
  双 `cargo test` 并发撞固定端口族（判回归前先隔离复跑 + 看 loadavg）；
- **本程序新增（实现时登记）**：QUIC 岛测试（tokio 单线程 + 回环端口 + 时间断言）的 flake 口径；
  DPLPMTUD / 迁移用例的墙钟依赖。

## 附录 A：实验台与原始数据（2026-10-08）

- 实验台：`/tmp/quic-lab/`（**临时**——M0 转正为 `tools/quic-ab.sh` 并复刻口径；`/tmp` 会被清理）：
  `raw` / `wg-ring` / `wg-shim` / `quic` 四臂 + `rss_probe2.sh` / `fp_probe.sh` / `peak_probe.sh` +
  `size-probe2`（体积矩阵）+ 多连接标定（`multiconn`）；汇总 = `results/SUMMARY.md`。
- 关键原始数字（「立项依据」表）出处：`results/m-*.out`（三臂矩阵）、`q-oneway*.out`（线开销）、
  `fp-*.med`（footprint）、`size-probe2`（体积）。
- 复现要点：① `CC_aarch64_unknown_linux_ohos` 必须显式设 NDK clang；②主指标用每包 CPU
  （墙钟受 loadavg 漂移，PERF-AB §9.15.1 教训）；③内存口径用 `vmmap` physical footprint
  （`ps RSS` 在同机两臂差 4.3MB 而二进制仅差 176B——该口径不可用）。

## 附录 B：删码 / 改码规模盘点（2026-10-08 实测行数）

| 类 | 模块 | 行数 | 去向 |
|---|---|---|---|
| 删 | `wgcore/{mod,stackb}` | 2273+548 | QUIC 岛替换；共用类型保留 |
| 删 | `wtransport/{bind,frame,reg,endpoint_cache,domain_eps,mod}` | 1582+245+116+655+802+14 | 整体退役（迁移/赛跑 = QUIC 原生） |
| 删 | `server/{bind,device,relayleg}` | 1576+760+660 | 腿表/设备面由「连接 = 设备」替代 |
| 删/改 | `session/{recover,mod}` | 878+1834 | 阶梯重写（断线 = 重连/迁移） |
| 改 | `facade/{tun_exec,bridge_host,portfwd}` | 2416+1360+1865 | 适配 STREAM；App 生命周期保留 |
| 保留 | `server/intercept/{mod,nat,dnsface}` | 5333+497+618 | 收窄（豁免只剩 DNS） |
| 保留 | `relay/mod` + `relaywire` | 2349+518 | 零改动（不引 QUIC 依赖） |
| 删 | `tools/ring-shim` | 103 | ring 0.17 直用（OHOS 实测通过） |
| 删/改 | `wgcore/udpbatch.rs`（2026-10-08 Q-L 代码门 低5 补） | 372 | `sendmmsg` 载体；两个消费方（`wtransport/bind.rs`、`server/relayleg.rs`）均在删除面 ⇒ M5 一并登记（净删以 M5 实测为准） |

（行数含各文件内嵌测试；实施时的净删/净增以 M5 实测登记为准。）

## 附录 C：指针地图

- 本仓：`AGENTS.md`（硬规则 / 工程原则）、`ROADMAP.md`（平移程序）、`docs/REVIEW-ROADMAP.md`
  （Q 批）、`docs/INTEROP-CRITERIA.md`（判据 + 变更登记）、`docs/PERF-AB.md`（性能口径与历史）、
  `docs/reviews/`（评审记录）、`fixtures/`、`tools/`（ci-local / matrix / perf-ab / qi-ab 先例）。
- tier 仓（只读）：`docs/agents/connection-lifecycle.md`（连接策略真源——M3/M7 触点）、
  `openspec/specs/`（`port-forwarding` = M4 达标目标）、`tools/tailcat/build-core.sh` +
  `homeway-rs.pin`（核侧构建与钉定——前进 = 用户触点）。
- 实验台（临时）：`/tmp/quic-lab/`（M0 转正前只读引用）。

## 附录 D：风险与未决（随推进更新）

| # | 风险 / 未决 | 处置 |
|---|---|---|
| 1 | tokio 进核的线程模型 / panic 边界与现有世代生命周期冲突 | M0 设计门专项；边界测试 |
| 2 | 体积净增（QUIC ≈1.5MB − 删除收益）超预算 | M0 基线 + M5 终值；超预算设计门登记 |
| 3 | 真机弱网（蜂窝/空口）QUIC 表现劣于 WG | M6 终验；按 PERF-AB 归因路径处置 |
| 4 | 中继 200pps / 句柄预算放大（QUIC 控制包 + 数据报） | M1 实测复核；必要时登记调预算 |
| 5 | QUIC DATAGRAM 丢包与全局代理 TCP 的相互作用（背压信号路径变更） | M1 专项测试（丢包 / 窄路径注入） |
| 6 | 0-RTT 重放语义（重连时是否启用 early data） | M2 设计门拍板；默认保守（关） |
| 7 | DPI / 抗封收益未验证（若为动机之一） | 真机实网测；非本程序范围时可另立项 |
| 8 | 并发在途批（`Q-K` / `Q-L`）与 M0/M1 的树冲突 | 开工前置复验；必要时 rebase（**Q-L 已落地并入库，2026-10-08——见 `docs/reviews/QL.md`**） |
| 9 | **Q-L 交接清单的 12 条（Q1–Q12）无人承接 ⇒ 退化成新一轮挂空** | **M1 开工前置检查项（Q-L 交接）逐条点名处置**（见 M1 节末小节；**Q2/Q3/Q4/Q5 四条「须显式立条」不接 = 上报主会话**；M1 收口记录须含 Q1–Q12 逐条处置表）；完整清单 = `docs/reviews/QL.md` §QUIC 交接 / `docs/reviews/QL-design.md` §3 |

## 附录 E：目标架构全图

```
                       ┌─────────────────────────────────────────────────────┐
                       │  手机 App 进程（tier 仓，接口面不变）                 │
                       │  Connection / VpnService / 分流开关 / 状态 UI         │
                       └───────────────────────┬─────────────────────────────┘
                                               │ VpnExtensionAbility（不变）
                                               ▼
┌─────────────────────────────────────────────────────────────────────────────────────┐
│  libclientcore.so（homeway-core 客户端档）                                           │
│                                                                                     │
│   同步面（保留今天形态：线程 + 通道 + 阻塞 IO）        QUIC 岛（新增）                 │
│   ┌───────────────────────────────────┐            ┌──────────────────────────────┐ │
│   │ TUN fd（App 递入，L3 直通）        │            │ ① 连接管理                    │ │
│   │   └─► IP 包 ⇄ DATAGRAM 直通        │  命令通道   │   并行握手赛跑（LAN/公网/中继）│ │
│   │                                    │ ─────────► │   迁移：rebind() + 新路径     │ │
│   │ 服务桥 UDS（files/term/speedtest）  │            │   重连：1-RTT（可选 0-RTT）   │ │
│   │   └─► STREAM[tag] 拨号             │ ◄───────── │ ② 控制流：登记/保活           │ │
│   │                                    │  事件/队列  │ ③ 流分发：files/term/speed/   │ │
│   │ portfwd 监听 127.0.0.1:<listen>    │            │   dial/probe（1 字节 tag）    │ │
│   │   └─► STREAM[tag=dial,目标]        │            └───────────────┬──────────────┘ │
│   │                                    │                            │                │
│   │ 巡检 / 需求门控 / 状态 JSON         │                            │                │
│   │   └─► STREAM[tag=probe] 探活        │                            │                │
│   └───────────────────────────────────┘                            │                │
└────────────────────────────────────────────────────────────────────┼────────────────┘
                                                                     │
                                             QUIC over UDP（一条连接 = 一台设备）
                                                                     │
              ┌──────────────────────────────────────────────────────┼─────────────────┐
              │  中继（relay 角色）—— 【代码零改动】                    │                 │
              │  纯不透明 UDP 转发：注册/挑战/证明 + 标签信封           │                 │
              │  · 完全不解析 QUIC 内部（QUIC 数据报对它是普通载荷）     │                 │
              │  · 直连与中继对 QUIC 同构 ⇒ 不再需要出口"腿表/LEGUP"    │                 │
              │  · 中继侧不需要 TLS/quinn 依赖（体积零增量）            │                 │
              └──────────────────────────────────────────────────────┼─────────────────┘
                                                                     ▼
┌─────────────────────────────────────────────────────────────────────────────────────┐
│  出口 homeway-cli（serve 角色）                                                      │
│                                                                                     │
│  QUIC 端点（quinn server：单 UDP 端口，migration=true，一端口挂多条设备连接）          │
│    │                                                                                │
│    ├── DATAGRAM ──► 连接→设备 demux ──► intercept 【保留，5333 行】                    │
│    │                                    ├ dst==出口IP:53/:5300 ──► dnsproxy（不变）   │
│    │                                    └ 其余 ──► transit：本机 socket 重拨         │
│    │                                       （全局代理 / portfwd 目标 / 任意 App 流量）│
│    │                                                                                │
│    ├── STREAM[tag=1 files]     ──► files_server（协议帧不变）                         │
│    ├── STREAM[tag=2 term]      ──► term/service（HSP 帧不变）                         │
│    ├── STREAM[tag=3 speedtest] ──► speedtest_server（不变）                           │
│    ├── STREAM[tag=4 dial]      ──► 出口本机拨号（portfwd 目标）                        │
│    └── STREAM[tag=5 probe]     ──► 回显（替代 path_probe → tunnel_ip:1）              │
│                                                                                     │
│  控制流（首条双向流）──► 设备表：token 证明 + devTag 采纳/替换（连接 = 设备）            │
└─────────────────────────────────────────────────────────────────────────────────────┘
```
