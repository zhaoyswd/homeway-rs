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
| **M0** | 骨架与依赖面（quinn/rustls/tokio 落地 + QUIC 岛设计 + 实验台转正 + 基线登记） | **已完成**（worktree 分支 `quic`，待用户指令合回 main） | 4/4 |
| **M1** | QUIC 承载 + 全局代理（DATAGRAM + 迁移/赛跑） | **已完成**（实现 + 代码门 r13 有条件通过；**1 格未过已上报**：产品形态单连接内存；待用户指令合回 main） | 4/4 |
| **M2** | 身份、设备表与准入（RPK + token 证明 + 抗放大） | 未开工 | 0/4 |
| **M3** | 服务流迁移（STREAM tag；客户端 stackb 退役） | 未开工 | 0/4 |
| **M4** | portfwd 承载适配（dial 缝换 STREAM；spec 不回退） | 未开工 | 0/3 |
| **M5** | WG 路径删除与收束（大删码 + 判据全表登记） | 未开工 | 0/4 |
| **M6** | 真机与性能终验（2×2 + 归因 + PERF 报告） | 未开工 | 0/3 |
| **M7** | 生产切换与文档收束（用户触点） | 未开工 | 0/3 |

## 下一步（当前指针）

> **本节 = 唯一的「现在该干什么」指针。** 主会话只认这里。

1. **M1 已收口**（实现 + 代码门记录 = `docs/reviews/M1.md`；规格 = `docs/reviews/M1-design.md`（含 §12
   拍板与 §12.6/§12.7 订正）；门槛读数 = `docs/reviews/M1-S5-evidence.md`）。**两件待用户裁决**：
   ①产品形态单连接内存格未过（+496K/+608K vs ≤+320K）——修订门槛 / 降级登记 / 限期归因 profiling；
   ②双栈期体积 1.2264× 于 3.8MB（按设计属 M5 判，删码余量 ~0.86MB **未实测**）。
   **M2 等待用户「开工」指令**（本程序节奏 = 逐期等指令，不自动接棒）。
2. M2 开工前置：M1 的 `docs/INTEROP-CRITERIA.md` 补登条目与 `docs/reviews/M1.md` 的差异登记**必读**；
   真机面七项仍归用户/硬件触点（`docs/reviews/M1.md` §3.6 清单：层 0 全通 / WiFi→蜂窝 / 真机吞吐 /
   路径 MTU·丢包·NAT / OHOS 运行期 / UPnP 真 IGD / `panic="abort"` 跨仓）。
3. **交付位置（用户触点）**：M0 + M1 全部工作在**独立 worktree** `~/Documents/projects/homeway-rs-quic`
   的分支 `quic` 上（M0 = `3f16471`…`1de72a2`；M1 = 设计门 `cabc762` 起至本收口 commit，共 30+ 实施
   commit）。**合回 main / push / 发 tag 均等用户显式指令**——主检出的 Q 批已全部收官（Q-L 收口
   `4841b20`），合回已无在途冲突（`git merge-tree` 只读预检过）。
4. M2 之后的期开工前置照旧：复验工作树 + `cargo test --workspace` 绿基线。

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
- **拍板点**：token 格式（M2 设计门后）、内层 MTU 降级开关的形态（M1——**已拍板 2026-10-08：
  候选 A**〔本地检包丢 + 计数 + 显式上限旋钮，`HOMEWAY_QUIC_MTU`/`tunConfig.quicMtuCap`，
  区间 [1320,1400]〕；B 留 M6 真机后候选、C 因与「不产出 ICMP 不可达」口径冲突不做）、
  0-RTT 是否启用（M2/M3）；
  **另：M1 阶段追加拍板四项（2026-10-08）**——①**出口 RPK 身份提前到 M1**（范围变更：M2 的
  「出口身份」半边前移，理由 = 不做则 M1 客户端只能用 `SkipVerify`、违反零隔离门命中纪律，
  M1 无法安全收口；客户端证明半边仍在 M2）；②内存门槛四条修订；③中继判据改同刻 A/B 相对；
  ④MTU 降级 = 候选 A（见上）。四项均记入 `docs/reviews/M1-design.md` §12。
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

**M0 收口证据（2026-10-08，实现棒 + 代码门 dsh r11）**：`cargo test --workspace` = 735 passed /
0 failed / 17 ignored；clippy `-D warnings` = 0；三目标 `cargo check` 全绿（物证 = ring 0.17.14
为三目标各产真 ELF 目标对象）；`tools/build-app-core.sh` 出 `.so`（20/20 符号 + 版本注入过，
2,339,344 B）；**M0 增量 ≤ 128 B**（产物内 `ring_core_0_17_14` / `tokio` / `quinn` / `rustls`
符号计数全 0 = 死码消除的构造性证据）；`tools/quic-ab.sh all` 一键复现附录 A（每包 CPU 偏差
−0.1%…+1.3%、线开销 WG 精确 / QUIC −0.013%、体积 −0.03%…−4.9%、footprint ±1.7% 内）；
本地-only 门（基线/向量/种子/词表/矩阵冒烟）在 **worktree 内**全绿（设计 §6 的 R-J 前提已失效
——`baseline/` / `bin/` 已就位于 worktree）；判据行零变更（词表门 PASS）。**三基线登记 =
`docs/QUIC-BASELINE.md`**（数字已按 harness 实测**重新登记**，附录 A 旧值只作量级对照）。

**交付形态**：依赖落地 + `crates/homeway-quic` 岛骨架（**零接线**——`homeway-core` / `cli` / `capi`
源码零 `homeway_quic::` 引用，行为零改动）+ `tools/quic-ab.sh` 实验台 + 三基线入册；设计门
`docs/reviews/M0-design.md`（dsh r10，39 条意见全处置）、代码门 `docs/reviews/M0.md`（dsh r11，
23 条：7 中全整改 + 13 低整改 + 3 低登记豁免）。

**范围登记（设计门 9.3 + 实现棒补四条，非范围扩张 = 必要细化）**：①`tools/cc-check-shim/stdlib.h`
（本节只写「补 CC」，未写「C 侧无 sysroot 怎么办」；该头仅用于 check-only 目标，**不进真实构建**）；
②`tools/quic-ab/` 用**两个独立 workspace**（`[patch]` 是 workspace 级，一个 workspace 装不下
「真 ring」与「垫片」两臂）；③新建 `docs/QUIC-BASELINE.md`（三基线的落点）；④`tools/check-quic-isolation.sh`
（隔离门）；⑤`tools/build-app-core.sh` / `ci-local.sh` 的 CC 导出与真实 OHOS link 门（App 出包路径
不修就断——设计 §2.5 的必修项）。

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

**M1 设计门记录（2026-10-08）**：`docs/reviews/M1-design.md`（884 行 + §12 拍板记录）；
dsh `--profile headless` 轮次 `r12.CrR3qv`，意见 **52 条**（高 14〔含 6 条阻塞项〕/ 中 25 / 低 13），
处置 **认同 51 / 部分认同 1 / 不认同 0**——高危全部改设计（服务端身份提前到 M1、reg 帧加 TLS exporter
连接绑定 `hr-reg3` 杀回放、巡检只探当前承载、缓冲预算统一 1 MiB、中继判据改相对 A/B、
客户端包封/剥壳切片 `S2-7`；并把 `min_mtu` 从协议地板 1200 提到 **1320**——1200 时
`max_datagram_size=1162` 会让 1280 内层包**全丢**）。评审独立推翻设计稿三处断言
（DPLPMTUD 实际关闭 / 输家 drop 会发 CONNECTION_CLOSE / ACK 预算算错 5× 且锚无出处）。
用户拍板四项见 `docs/reviews/M1-design.md` §12（本文件「用户触点清单」同步登记）。
**实施清单 = 该文档 §10 的 S1–S6 六切片**（S1 出口 QUIC 面 → S2 客户端岛 → {S3 观测面与 A/B,
S4 判据登记} → S5 门槛实测〔须独占机器〕→ S6 代码门）。

**退出口**：全局代理等价 + 迁移通过 + 三项性能门过（或差异登记）；A/B 一键回退 WG 可用。

**M1 收口证据（2026-10-09，实现棒 + 代码门 dsh r13）**：`cargo test --workspace` = 688 passed /
0 failed / 4 ignored（`homeway-core --lib`）+ `homeway-quic` 76 passed（**全绿**）；clippy `-D warnings` = 0；
三目标 `cargo check` 全绿（OHOS 真链路档 + musl 双架构 clang 垫片档；OHOS 档 1 条**既有**
`libc::time_t` deprecated 警告，非 M1 面）；`tools/build-app-core.sh` 三道门过（20/20 符号 + 版本注入 +
`.so` = **4,660,320 B**，对 3.8MB 阈值 **1.2264×**，按设计属 **M5 判**）；`tools/check-quic-isolation.sh`
**九条全绿**（M1 S6 由五条扩到九条：裸 `send_datagram(` / `send_datagram_wait` / 中继整文件零异步名 /
单线程前提三条可判定事实 + harness `SECURITY` 标记；新断言已用四形态负例验证各自确定性红）；
`tools/check-vocab.sh` PASS（词表面零改动）。
判据行 = `docs/INTEROP-CRITERIA.md` 的 M1 S3/S4 批 **17 条 + 计数输入集 3 行**，**S6 代码门补登 7 + 1 行**
（Q-O 三闸拒绝族 / 准入被拒族 / 装配·生命周期归因行族 / N-c 节流文案订正 / `L3Bearer` 接受集 /
隔离门断言面扩展 / S6 整改两条实现偏离）；`_wg` 档**逐字节回退**（token 逐字节 + C 族原串 +
`quic:` 族零输出）。门槛实测 = `docs/reviews/M1-S5-evidence.md`（每包 CPU **0.895×**、线开销 30.152B、
每连接边际 80.0K/84.0K、32 设备 1.44MiB、负载态 +704K、中继下行 A/B **1.00×**、迁移 `migrations=1`
腿峰值 2）——**10 格通过 / 1 格未过**（产品形态单连接内存 +496K/+608K vs ≤+320K，**已上报待裁决**）
+ 1 格按设计属 M5 判（体积）。代码门 = `docs/reviews/M1.md`（r13，`/tmp/dsh-review/r13.i3rMst/`，
exit 0，有条件通过 → 条件 C1/C3 已整改、C2 上报；28 条意见 高 2 / 中 12 / 低 11，**不认同 0**，
高危 H1 = 噪声门控误读 WG 腿信号已修）。
**真机面（2026-10-09 补验，OHOS 设备 `FMR0224116011480` / ALN-AL00，读数 `/tmp/m1dev-res/`）**：
**层 0 全通 ✓ 已验**（设备浏览器经 QUIC 隧道渲染 example.com；核侧 `warmup pong: 就绪（判据=quic）` /
`attached（数据面已接管 fd=94）` / `link: via=direct ep=192.168.3.12:42652 rtt=26ms`，出口侧
`quic: 连接采纳` + `intercept: tcp transit …（dialok）`）；**OHOS 运行期 ✓ 已验**（tokio/mio/epoll 在
设备上可用；4 个世代起停全干净、零 panic）；**真机吞吐 部分**（App 测速口径同刻 A/B 无数量级回退：
上行比 1.03 / 下行比 1.00；TUN 口径全量与冷/热门未做）；**未验**：WiFi→蜂窝（阻塞：设备无蜂窝 IPv4 +
本地出口无蜂窝可达端点）、NAT 重绑、UPnP 真 IGD、`panic="abort"` 跨仓（已取当前事实：构建路径
**无** `[profile.release]` ⇒ 缺省 unwind，岛内 `catch_unwind` 当前有效）⇒ 余项归 M6/M7 触点。
**另登记两条真机发现**：①出口 `quic: 源校验拒` 在首次 QUIC attach 后约 1s 出现 ≥3 次（之后未见；
核侧 drops 全 0 ⇒ 出口侧单向计数，归属待包级探针）；②经中继未在本轮真机覆盖（本地出口形态无公网端点）。

## M2 身份、设备表与准入（估 3–5 会话日）

> **范围调整（M1 用户拍板 2026-10-08）**：下面第一项「出口身份（Ed25519 RPK + 公钥进 token +
> 客户端钉定校验）」**已前移到 M1**（理由 = 不做则 M1 客户端只能用 `SkipVerify`/`dangerous()`，
> 违反零跳过验证纪律、M1 无法安全收口）。M2 剩「客户端证明半边 + 设备表完整语义 + 抗放大 +
> 威胁模型」——M2 设计门开工时以 `docs/reviews/M1-design.md` §1.3/§12 的实际落地面为准重划范围。

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
| **词表** | `tools/check-vocab.sh` 五族 | M0 | **已完成：PASS**（Rust 声明 5 单元 / 26 值；ledger sha256 与 `docs/BASELINE.md` 锚定一致；缺席表 4 项在册）——预判「不受影响」已实测确认 |

## 性能 / 体积 / 内存门槛（预登记；方法 = PERF-AB 口径 + `quic-ab.sh`）

| 维度 | 门槛 | 方法 |
|---|---|---|
| 每包 CPU | ≤ 现役 WG+shim ×1.0 | harness 三臂（附录 A 口径） |
| 线开销 | ≤ 40B/包 | QUIC oneway 精测（服务端 `udp_rx` 口径） |
| 真机吞吐 | 热态 ≥ 0.95× 现役；冷/热 ≥ 0.70 | 同刻交替 A/B（PERF-AB §1/§9 口径） |
| 换网迁移 | 连接保持（无重连）；出口设备表不新增条目 **且中继腿表峰值 ≤ N（实测登记，防「腿表增长被误判为通过」）** | 真机 WiFi→蜂窝 |
| 断线恢复 | 出口重启恢复 ≤ 3.5s（现役 R1 命中 3.126s 量级） | 故障注入（沿用 R2 批手法） |
| 体积 | OHOS `.so` ≤ 3.8MB（净增 ≤ +1.5MB，删码后复测） | size 矩阵（OHOS cdylib） |
| 内存 | **单连接 ≤ +320K；每设备 ≤ 96K；32 设备 ≤ +3.1MiB（稳态）；出口 32 连接持续流量增量 ≤ 64MiB + 自有队列上限（负载态）**〔M1 设计门 + 用户拍板 2026-10-08 修订：原 256K/64K/+2MB 的每连接锚来自附录 A 手抄 37.6K，无原始证据链，实测五点拟合 81.6K / 三点 96.0K〕 | footprint 三轮下中位（harness；**拟合口径 = 五点 N=1..5**，三点作对照） |
| 丢包可观测 | DATAGRAM 超限 / 丢弃有计数行，不静默 | 窄路径注入（MTU<1340） |
| 中继承载 | **同刻 A/B 相对判据**（经中继 vs 直连的比值；绝对吞吐数字只作登记、标注不可比）——并同时记 `congestion_events`/`lost_packets`（分辨「限速器静默丢被 QUIC 当拥塞」）〔M1 设计门 + 用户拍板 2026-10-08〕 | 本地中继（`tools/local-rust-relay.sh`）+ 真机复测 |

- **门槛表（内存/体积行）的 M1 状态指针**：M1 判据按 `docs/reviews/M1-design.md` §9.1 的四条修订 +
  §12-③ 用户拍板执行（每设备 ≤96K / 单连接 ≤+320K / 32 设备 ≤+3.1MiB / 负载态 ≤64MiB+队列）；
  **产品形态单连接格实测未过**（+496K/+608K，见 `docs/reviews/M1.md` §3.1）⇒ 该格数值**待用户裁决**，
  **勿按旧值复述**；体积行按设计属 M5 判（M1 双栈期实测 4,660,320 B = 1.2264× 于 3.8MB，
  **M5 删码余量 ≈0.86MB 未实测**——M5 设计门须先实测删码余量再判）。

---

## 已知 flake 登记（沿用 Q 批表；本程序增量）

- **随 M5 删除除名**：`wgcore::stackb::*`（墙钟断言）——WG 退役后该测试移除；
- **沿用有效**：`daemon::tests::*` 时序族、`term::service::tests::attach_size_applies_to_pty`、
  双 `cargo test` 并发撞固定端口族（判回归前先隔离复跑 + 看 loadavg）；
- **本程序新增（M0 实现时登记，2026-10-08）**：QUIC 岛（`crates/homeway-quic`）与 `tools/quic-ab.sh`
  的 flake 口径五条：
  ① **不钉固定端口**：岛内一切回环端点 `bind("127.0.0.1:0")` + 读回实际端口；harness 四臂同样全 `:0`；
  ② **时间断言禁精确墙钟**：只断言上界（`elapsed < 预算 × 4`）与「预算内收工」形态；panic/卡死注入
     用例不设墙钟下界（只判「回执不挂死 + 记行到达 + 返回值形态」）；**纯定时语义用例（M1 起）用
     `tokio::time` + `start_paused`，并同批给 tokio 加 dev-dependency `["test-util"]`**（M0 未引入
     ——当时无用例，防「声明了不用」）；
  ③ **不依赖 loadavg**：岛内零吞吐断言（性能判据全在 `tools/quic-ab.sh`，用每包 CPU 口径）；
     ⚠️ harness 复现判据时**须独占机器**——与交叉编译并发那一轮实测四臂整体上抬（wg-ring +12.1%，
     越出 ±10% 带），读数一律对照同目录 `loadavg.tsv`；
  ④ **隔离复跑纪律**：红了先隔离单跑（`--test-threads=1` 独占）再判回归；`daemon::tests::*` 时序族与
     双 `cargo test` 并发撞固定端口族（本批实测：并发会话跑 `cargo test -p homeway-core` 会让
     `daemon::carriers::forward::tests::*` 报 `bind 127.0.0.1:20004/20010 already in use`）不与本程序混判；
  ⑤ **登记动作**：以上四条随 `docs/QUIC-BASELINE.md` / `tools/quic-ab/README.md` 同源，变更须同批更新。
  另（M0 移植事实，非 flake）：DPLPMTUD / 迁移用例的墙钟依赖仍属 M1 起的登记面。
- **本程序新增（M1 实现时登记，2026-10-09；M2 S1 补一例）**：`wtransport::bind::tests` 的**实 socket 时序族**
  （S2b 实测 `mirror_then_adopt_then_single_send` 一次、M1 S3/S4 批全量并行跑 `relay_envelope_and_adoption`
  一次、**M2 S1 批全量并行跑 `unknown_source_hint_filtered` 一次**；均 `--test-threads=1` 隔离复跑绿、
  相关文件零 diff），与既登记族两例（`daemon::tests::handshake_deadline_beats_slow_drip`、
  `term::service::tests::attach_size_applies_to_pty`）同批登记；代码门 r13 又实测 `daemon::tests::*` 一例
  （隔离复跑绿、与 M1 改动面无交集）。**flake 口径照 M0 §9.2 ④**（红了先隔离单跑再判回归；**不静默重跑**）。
  另：M1 收口期间新增的两条用例（岛回程泵身份防重 / 消费者已退归因）已做**负例有效性**验证
  （旧语义确定性红），不属 flake 面。
- **本程序新增（M2 实现时登记，2026-10-09）**：`client::tests::send_buffer_used_grows_under_load`
  （S2-5 新增用例；全量并行跑红 2/2 含干净树基线、隔离复跑 13 次 1 红 ⇒ 负载/时序敏感，
  **不作回归判据**，红了先隔离复跑）。既登记族复现：`daemon::tests::server_bad_frame_gets_goodbye_and_disconnect`
  （M2 S2 批实测 1 红、隔离复跑同二进制内翻转 = 已在册签名）。
- **工具坑登记（M2 实测，防后续棒踩）**：①`tools/quic-island-e2e.sh` 与 `tools/quic-wg-e2e.sh`
  **共用实例号 state**（`/tmp/homeway-rs-rustexit-1`）且 `serve token` 读台账末行 ⇒ 先跑 island
  再跑 wg 会读到上一轮铸的 token 而**假红**；解法 = wg 跑前先 `tools/local-rust-exit.sh wipe 1`。
  ②`tools/local-rust-exit.sh start` 在 `target/release/homeway-cli` 存在时**不重建** ⇒ 改完核必须先
  `cargo build --release -p homeway-cli` 再起实例，否则跑到旧二进制（现象 = 新行/新配置缺席）。

## 附录 A：实验台与原始数据（2026-10-08）

- 实验台：`/tmp/quic-lab/`（**临时**——M0 转正为 `tools/quic-ab.sh` 并复刻口径；`/tmp` 会被清理）：
  `raw` / `wg-ring` / `wg-shim` / `quic` 四臂 + `rss_probe2.sh` / `fp_probe.sh` / `peak_probe.sh` +
  `size-probe2`（体积矩阵）+ 多连接标定（`multiconn`）；汇总 = `results/SUMMARY.md`。
- 关键原始数字（「立项依据」表）出处：`results/m-*.out`（三臂矩阵）、`q-oneway*.out`（线开销）、
  `fp-*.med`（footprint）、`size-probe2`（体积）。
- 复现要点：① `CC_aarch64_unknown_linux_ohos` 必须显式设 NDK clang；②主指标用每包 CPU
  （墙钟受 loadavg 漂移，PERF-AB §9.15.1 教训）；③内存口径用 `vmmap` physical footprint
  （`ps RSS` 在同机两臂差 4.3MB 而二进制仅差 176B——该口径不可用）。
- **M0 复测订正（2026-10-08）**：附录 A 的四位数已在 `tools/quic-ab.sh` 下复测并**重新登记**于
  `docs/QUIC-BASELINE.md`（本附录旧值只作量级对照）；两处口径订正随 M0 落库：①CPU 基线的原始
  证据链只有手抄 `SUMMARY.md`（`m-*.out` 全为 11 字节空壳；第二份独立测量 = `/tmp/pk-*-cli.out`，
  N=300k，与手抄值差 1.3–5.9%）；②`wg-ring` 臂的 ring 实为 **0.16.20**（非 0.17）。
- **M1 复测（2026-10-09）**：每包 CPU `quic` **12.895µs**（对 M0 基线 +1.8%）/ 线开销 **30.152B** /
  `mds` 1162·1362 精确——逐格对照见 `docs/reviews/M1-S5-evidence.md` §1（含首轮 wg-ring 越界
  = 测量窗口污染的归因与复跑消解）。设计 §5.1 的「下行 1402B」实测不可达（实得 **1322B**）=
  **算术上界**，已按 M1 设计 §12.7-1 补记。

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
| 8 | 并发在途批（`Q-K` / `Q-L`）与 M0 的树冲突 | 开工前置复验；必要时 rebase |

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
