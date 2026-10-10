# homeway-rs 传输层换代 Roadmap（WG → QUIC · 跨期进度真源）

> **新会话续接协议（三步）**：①读本文件；②按「状态总览」找到第一个未完成期，读该期小节
> （目标/范围/判据/评审门/退出口）；③按「每期执行协议」派发子 agent 执行，主会话只做调度
> （读指针→派发→收摘要→更新本表→本地 commit）。用户说「继续 QUIC 换代 / 接着干」即指此协议。
>
> **用户拍板（2026-10-08）**：①**无兼容包袱**——产品未发布，不与旧后端 / 旧 APP / 旧 wire 互操作
> （token 格式、wire 协议、身份体系均允许破坏性变更）；②**功能全保**——全局代理 / 文件管理 /
> 终端 / 端口转发一个不能少；③方案原则 = **简洁、高效、简单**。
>
> **口径重申（2026-10-09，用户：「不用考虑和之前的兼容性」）**：①**常设有效**——任何阶段的 wire / token /
> 格式 / 判据行变更都**不需要为旧形态留兼容路径**（含 M2 设计 §4.2 的 **token 候选 B**：其唯一前置
> 「`_wg` 档 token 逐字节同」按本条**不再成立** ⇒ B 解封，是否实施由 **M5 设计门**定〔M5 = 格式与登记
> 收束期；M2 已按候选 A 收口，不在已收官期回改〕）；②**Go 客户端相关面**（本地矩阵的 L4/L5 等
> 「Go 客户端 × Rust 出口」行在 token 变更后会断）**不再构成约束**——M5 一并按「退役登记」处理，
> 不必为它保留形态。
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
| **M2** | 身份、设备表与准入（RPK + token 证明 + 抗放大） | **已完成**（S6 收口；代码门 r15 无高危；**1 项未过**：产品形态单连接内存；真机 S5-4 待用户点头） | 4/4 |
| **M3** | 服务流迁移（STREAM tag；客户端 stackb 退役） | **已完成**（S1–S9c 落地；设计门 r16/r17 + **代码门 r18**：两条高危〔阶梯活性洞 / 窗口整改未落登记〕已整改与补登；**吞吐门槛已过 1.35–1.37×**〔改前 0.46×〕；**2 项未过**：真机 R5 瞬时黑洞未做〔挂 M6〕、真机 R1 上传面不可自动化〔tier 触点〕；1 项 e2e 时点断言未做〔归 M5〕；待用户指令合回 main） | 4/4 |
| **M4** | portfwd 承载适配（dial 缝换 STREAM；spec 不回退） | **已完成**（S1–S6 落地；设计门 r19/r20 + **代码门 r21**：中危 4 条全处置、无高危；spec 5/5 requirement（10/10 scenario）本机全核 + 真机 6 场景（2 项未取形态如实登记）；`.so` 4,867,168 B；待用户指令合回 main） | 3/3 |
| **M5** | WG 路径删除与收束（大删码期） | **已完成**（实现 + 代码门 r27 整改 + 收口；1 格未过已上报） | S0/S0b/S1/S1b/S2a/S2/S3a/S3b/S4/S5/S5t/S6/S7a/S7b/S7c/S8 全落 |
| **M6** | 真机与性能终验（2×2 + 归因 + PERF 报告） | **已完成；差异已获用户接受**：真机 2×2 + 逐环归因 + **根因修复（内存接收缓冲溢出）** ⇒ T2 **0.484× → 0.893×**、T6 2.23× → **1.36×**；**差异经用户 2026-10-10 接受并登记**；5 项顺延 | 3/3 |
| **M7** | 生产切换与文档收束（用户触点） | 未开工 | 0/3 |

## 下一步（当前指针）

> **本节 = 唯一的「现在该干什么」指针。** 主会话只认这里。

1. **M6 已收口（测量与归因）**（规格 = `docs/reviews/M6-design.md`（r28/r29 两轮过门 + §12 裁决）；
   实现与读数 = `docs/reviews/M6.md`；报告 = `docs/PERF-AB.md` §9.20）。**M7（生产切换与文档收束）等待
   用户「开工」指令**——但其**前置是一个决策点**（见下条）。
2. **M7 决策点 = 已裁（用户 2026-10-10「先接受，然后继续，先把这个项目做完」）**：**接受 T2 差异并登记**
   —— 经 M6.6（差分量测推翻 M6.5 归因）与 M6.7（**根因修复：岛侧 UDP socket 默认 `SO_RCVBUF` ⇒ 设备内核
   接收缓冲溢出 ⇒ 丢包被当拥塞 ⇒ CUBIC 退避**；显式 2 MiB/1 MiB 后 `RcvbufErrors→0`），主判据已从
   **0.485× → 0.893×**（T6 2.23× → 1.36×）；**剩余 11% 成因量化**（修后发送瞬时 47–59 MB/s 超空口 L0 锚
   44.8 MB/s ⇒ 过载 ⇒ 丢包退避 + 内层 RTO），**接受并登记**。**未采纳**：为剩余 11% 开设计批（留作收口后
   可选改进项：自定义 CC factory / 出口面整形 / BBR 类；实施前须设计门 + 判据变更）。**保留**：revert M5
   作为回滚预案里的唯一回退路径（WG 已从产品删除）。
3. **M6 顺延项（M7 前补）**：T5（pf bulk）/ T11（路径变更+腿切换）/ S7 整片（DNS 回复 e2e、pf 形态 1·3、
   旧 token 归因行〔§12-1 留 M7〕）/ T9 单连接复跑 / T3 旧臂（参照臂不可得——旧核 LAN 下自动升级直连）。
3. **待用户拍板**：①token 候选 B（设计 §4.2，本批按候选 A 不动 token，窗口未关）；②M1 遗留的内存
   门槛数值（产品形态 512K vs ≤+320K，M2 后无恶化）与体积预算口径（双栈 4.685MB = 1.233× 3.8MB，
   按设计属 M5 判）；③tier `log-index.md` 陈旧拦出包门（tier 侧触点）。**真机 S5-4 已做完**
   （2026-10-09 测试机授权后一轮跑完，见 M2 收口证据与 M1 真机面）。
4. **观测面**：出口 quic 段的外部只读面**已随 M3 S2 落**（`serve status --json` 的 additive `quic` 段，
   28 键）；M5 仍需补 A10 的「出口侧每连接收线」判据行（归 M5 出口观测面）。
5. **交付位置（用户触点）**：M0–**M3** 全部工作在**独立 worktree** `~/Documents/projects/homeway-rs-quic`
   的分支 `quic` 上。**合回 main / push / 发 tag 均等用户显式指令**——主检出的 Q 批已收官
   （`4841b20`），合回**会有 2 个文档冲突**（`docs/INTEROP-CRITERIA.md` 与 `docs/QUIC-ROADMAP.md`
   两边都追加过条目，按并集解）。
6. **Q-L 交接（Q1–Q12）已补处置表**（见 M1 节末小节；流程缺口同处如实登记）——**M5 开工任务书须逐条点名
   已认领的 5 条**（Q2 中继 v6 双栈 / Q3 public_endpoint 写失败告警 / Q4 listen_port 非致命对齐 /
   Q5 `--public-endpoint` 值域校验 / Q8 reactor 每拍固定税）+ `udpbatch.rs` 补登删除清单。
7. **M4 开工前置照旧**：复验工作树（干净）+ `cargo test --workspace` 绿基线 + 读 M3 的
   `docs/INTEROP-CRITERIA.md` 登记条目与 `docs/reviews/M3.md` 差异登记。

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
**真机面（2026-10-09 两轮补验；OHOS 测试机 `FMR0224116011480` / ALN-AL00；读数 `/tmp/m1dev-res/` +
`/tmp/m2dev-res/`；用户已授权该机「尽情测试」）**：
**已验 ✓**：**层 0 全通**（设备浏览器经 QUIC 隧道渲染 example.com）/ **OHOS 运行期**（tokio/mio/epoll
可用；累计 17 个世代起停全干净、零 panic/abort/SEGV）/ **NAT 重绑**（出口面：五元组变、连接 ID 不变、
`quic: 路径变更` ×3、世代未重建、变更后双向可用）。
**部分**：**真机吞吐**（App 测速口径同刻 A/B 无数量级回退：上行 1.03/0.95×、下行区间重叠；
**TUN 全量与冷/热门未做**——设备无 curl/wget，归 M6）；**UPnP 真 IGD**（**映射建成**
`UPnP：QUIC 端口 … 映射 已建立`，但外网 IP 不可得/STUN ≠ upnp ⇒ **fail-closed 不公布公网端点**——
「有真 IGD + 映射成功」已验，「公网端点可用」属环境不可得）。
**未验（阻塞，归 M6/用户触点）**：**WiFi→蜂窝迁移** —— 双链均断：①设备 `rmnet0–11` 无 IPv4、
Settings 实读 `enabled=false/clickable=false`（**无 SIM 数据服务**）；②出口侧无蜂窝可达公网端点
（本机在代理后）。**`panic="abort"` 跨仓**归 M7（已取事实：构建路径无 `[profile.release]` ⇒ 缺省
unwind，岛内 `catch_unwind` 有效）。
**已定性（原登记的真机发现）**：出口 `quic: 源校验拒` 首次 attach 后约 1s 的 3 条 —— 用探针取 `src=`
决定性证据 = **IPv6 MLDv2 组播报告**（`src=:: / fe80::… dst=ff02::16`，OHOS 内核在 vpn-tun 建起时
自发），**不是**上一世代遗留 IPv4 排队包（M1 猜测证伪）；被拒属设计内（QUIC `src_allowed` 只看 v4，
WG 面 `WriteToTunnelV6 => Done` 同义）⇒ **两档行为等价**，QUIC 档只多记行。

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

#### B 棒实测承接（2026-10-09，M5 S7a 落地后）

- ①**island 侧中继候选类型 = `SocketAddrV4`（v4-only）⇒「客户端经 v6 中继」全链不可达**（v6 形态 island
  e2e 确定性失败）。这是 Q2（中继侧已双栈）的**客户端面另一半** ⇒ **登记为已知能力缺口**；成本评估并入
  D 棒（若 M6 真机无 v6-only 场景则降级为长期登记）。
- ②**中继控制面 TCP 仍 v4-only**（`lsof` 实证：`TCP *:port (IPv4)` vs `UDP *:port (IPv6)`）⇒ `--advertise`
  为 v6 字面时出口拨腿的控制面连不上（**生产默认 v4 公布形态不受影响**）⇒ 登记 + 交 D 棒评估。

#### C2/C3/C4 落地与两项主会话裁决（2026-10-09）

- **落地**：C2（S2a 换源；`ConnErr` 43/55 闭合）→ C3（**S2 客户端 WG 面删除：净删 −9,534 行**，
  `session`/`wgcore`/`wtransport` 主体删；`.so` 2,959,520 B = **0.779× 判据**，已达标）→ C4（S3a/S3b/S1b：
  **净删 −3,735 行**；残留扫查八条清零；`[patch]`/`boringtun`/`ring-shim` 退役完成、`Cargo.lock` 无 ring 0.16/
  无 boringtun）。M5 累计净删 **≈ −13,269 行**（不含 S2a 新增面）。
- **裁决①：RRR 冒烟的 `C-via-direct` 断言放宽**——赛跑的设计语义 = 取最快者（本地中继腿 2ms vs 直连
  ~1.0s ⇒ 落中继是**特性**不是缺陷；WG 时代的「直连必胜」期望在中继腿在场时不成立）。改为
  `C-via`（胜者 ∈ {直连,中继} + 记录胜者与耗时）+ **新增独立断言 `C-direct-usable`**（落中继时打死中继
  ⇒ 重开会话必须直连成功）。同批登记：**M6 归因观察项 =「有中继腿时直连首飞 ~1s（本机形态）」+ G8
  （中继升直连整面已登记退役）**。
- **裁决②：matrix 行集退役 L2/L3/L4/L5**（L2/L3 = Go 出口 + Rust 客户端；L4/L5 = Rust 出口 + Go 客户端）。
  依据（实测）：Go 出口 token 无 RPK/QUIC 端点 ⇒ QUIC-only 客户端必失败；Rust 出口 token 带 `Quic` 类端点
  ⇒ **Go 解析器不认**（`token 非法（hmw1…）：格式非法`）——按**无兼容包袱**口径，Go 侧退役（2026-10-05）
  + 新 token/wire ⇒ 这两个组合**不存在**。`L2_L3_RETIRED=(L2 L3 L4 L5)`（**一行可回退**）；剩余行（RRR /
  L1 / L6 等）按实际可跑性在 D 棒 S5 登记定稿。
- C4 顺带修一条真回归（S3a 漏 `unmap_v4_in6` ⇒ 中继注册腿回执被静默忽略；S2b e2e 抓出，已修）。
- **如实登记**：C4 的删除面**不在 `.so` 可达集内** ⇒ 体积与 C3 逐字节同值（2,959,520 B），**不据此宣称
  体积收益**；出口侧体积终值须在 E 棒按「出口二进制」单独量。

#### C 棒拆分登记（2026-10-09，主会话裁决）

C 棒按设计 §1.5「宁分步不原子」停在**可编译中间态**（S2a 岛侧 + 模块侧落地；`session::Session` 与
`HostSession` 并存 = 设计的「先加后删」中间态），并如实上报：**全量删除 ≈11.2–12.1 千行 +
新会话面 600–900 行 + 出口公共端点五类迁址（必然改岛公面）超出单会话预算** ⇒ **主会话裁决：拆三棒**
（`C2 = S2a 换源` → `C3 = S2 删除` → `C4 = S3a/S3b + S1b`），每棒仍按 §1.5 保持可编译/可测。
C 棒已落读数：`.so` **3,557,736 B**（0.936× 判据）、workspace 测试全绿、三目标 check 全过、
隔离门 11/11、词表门 PASS。

#### Q1–Q12 处置表（**主会话补登，2026-10-09**）

> **⚠️ 流程缺口如实登记**：本清单由 Q-L 批于 `4841b20`（2026-10-08）落在 **main**，而 QUIC 程序全程在
> 兄弟 worktree 的 `quic` 分支推进（基于 `082e120` 快照）⇒ **M1 设计门/收口时未见过本清单**
> （清单自身预见的「若 M1 设计门已过 ⇒ 主会话并入实现任务书或顺延 M2 前置」两条路都未发生）。
> 该缺口在 **2026-10-09 合回 main 时发现**，按「**绝不允许静默**」纪律，现在一次性补处置表；
> 四条「须显式立条」（Q2/Q3/Q4/Q5）**逐条认领**（去向见下表），并已排入 M5 批次。

| # | 处置 | 去向 / 理由 |
|---|---|---|
| Q1 中继 assoc 上限字节预算化 | **不接（登记差异）** | 属 relay 代码改动，与「中继零改动」红线冲突；M1 实测（`M1-S5-evidence.md`：迁移腿峰值 2、pend 窗丢包 0、assoc 未触界）不支持当前必要性 ⇒ 登记差异；若将来 relay 因其它理由开批，一并重估 |
| **Q2 中继客户端方向 v6（双栈）** | **接（显式立条）** | 「功能全保」原则 + 与 QUIC 无关（不借「中继零改动」逃避）；**须改 `relay/**`（红线面）⇒ 排 M5 期内的独立小批 + 上报用户** |
| **Q3 `public_endpoint.txt` 写失败静默** | **接（显式立条）** | 对齐 Go（加告警行）；公共端点面保留 ⇒ 排 M5 期内小批 + 判据行登记 |
| **Q4 `listen_port.txt` 写失败致命 vs Go 非致命** | **接（显式立条）** | 对齐 Go（非致命 + 告警）；触出口启停语义 ⇒ 排 M5 期内小批 + 判据行登记 + 变更登记 |
| **Q5 `--public-endpoint` CLI 值域零校验** | **接（显式立条）** | 对齐 Go（值域校验 + 报错）；排 M5 期内小批 + 登记 |
| Q6 UDP 上行门 / `deliver_udp53→udp_drop` e2e | **登记差异（部分随 M5 消失）** | 入口已换 DATAGRAM（M1 完成）；残留 e2e 面随 M5 删码消失 |
| Q7 性能残余三处（wgcore/bind） | **随 M5 删码消失**（无动作） | `wtransport/bind.rs:287`、`server/bind.rs:757/1020` 在删除清单内 |
| Q8 `reactor_turn→poll(0)` 每拍固定税 | **接（立条，M5/M6）** | **intercept 保留面，不得按删码消失处理**；M5 复测后重定动作（或 M6 归因） |
| Q9 `wgcore` Engine 级测试空档 | **随 M5 删码消失**（无动作） | `wgcore` 删除、共用类型保留 |
| Q10 PERF-AB §9.15.6 四条 | **拆分处置** | 端点竞速条 = M1 已重定（赛跑落地）；发送线程/管线/`sendmmsg` = M5 删码消失；**`udpbatch.rs`（372 行）补登进 M5 删除清单**（代码门 低5） |
| Q11 `files --host` 失败 | **已闭合（Q-L L7）+ 本程序登记知悉** | 判定非缺陷（双向文档化的盲节流边界）⇒ M3 无需承接（M3 已收口，未触碰该面） |
| Q12 `tx_frag` 跨报文乱序 e2e | **登记差异（可选未做）** | intercept 保留面；M5/M6 若重测分片可顺带 |

> **汇总**：接 5 条（Q2/Q3/Q4/Q5/Q8）+ Q10 的部分（`udpbatch.rs` 补登）⇒ **排入 M5（M5 开工任务书须
> 逐条点名）**；登记差异 3 条（Q1/Q6/Q12）；随删码消失 3 条（Q7/Q9/Q10 部分）；已闭合 1 条（Q11）。

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

**S6 收口（2026-10-09）**：
- **高危整改**：S5 实测证伪 **D1**（双栈出口 ⇒ 每源闸键恒 `::/64` ⇒ 异 /32 源共享预算，源 A 用满后源 B
  第 1 次即被拒）⇒ `SrcKey::of` 归一 v4-mapped（`ad03631`）；修复前后对比读数（源 B 由「第 1 次即被拒」
  →「握手 1.5ms + A4」）与不回归读数（`k=24 ⇒ refused=8=K−16`、同源打满后仍拒、11s 窗清后成功）见
  `docs/reviews/M2.md` §1.4。**D1 的方法论意义**：这类「本地形态掩盖的键归一缺口」会以实测证伪形式暴露
  ⇒ 代码门 G3 列出的同类缺口（`probe.rs:230` 安全卫兵 / `relayleg` 归一）**建议紧随 M3 开工前一批修**。
- **代码门 r15**：dsh `exit 0`；**无高危**（18 条：高 0 / 中 4 / 低 14）；中危 G1（隔离门判据不实）/
  G2（fail-visible 串零用例）/ G4（S5-5 判据未按形态落实）随批整改，G3/G6/G7 等登记
  （`docs/reviews/M2.md` §2.5/§7）。
- **隔离门加固**：新增「豁免自证 + 扫描器自校准」——「把纯 std 文件挪进 `ASYNC_FILES` 逃过扫描」现在
  **确定性红**（双向负例实测 exit=1）。
- **威胁模型**：设计 §5 的 14 条逐条验证（`docs/reviews/M2.md` §3：12 条有代码落点、1 条显式未加固
  =「强制回落 WG」、3 条仅结构性证据）。
- **门槛**：M2 后 `quic` 每包 CPU **12.250µs**（对 M1 登记 −5.0%，0.833× 于现役 wg-shim）/ 线开销
  1309.9B / 稳态 footprint 1216K（−2.6%）/ `.so` 4,685,472 B（M2 累计 +25,600 B ≤ +32KB 预算）；
  **单连接内存格 512K vs ≤+320K 仍不达标**（+3.2% 对 M1，**无恶化**）。
- **M2 真机验证（2026-10-09，测试机授权后一轮跑完，读数 `/tmp/m2dev-res/`）**：设备实读核版本 =
  本分支 HEAD（`26c8fa8d593a-rust`）。**六项全过**：①**四帧准入走通**（核 `准入已发起→准入完成(17ms)`
  + 出口 `准入挑战已发`/`连接采纳`/`peer: +`）；②**错 token 拒**（出口 `准入被拒（… hr-reg4 MAC 不符）`
  + WG 腿 `reject reason=no-token`）；③**重连不新增表条目**（`peer: +` 恒 1，两轮重连只 `peer: ~ refresh`）；
  ④**吊销即拒**（`注册被拒（原因=revoked）` + `reject reason=revoked`）；⑤**表满淘汰**（在线超限
  `reason=table-full n=1/1`；空闲过期 `reason=stale` 后新设备补位）；⑥**`_wg` 回退**（回落行 + `判据=wg`
  + C 族原串 + `quic:` 族零输出）。**第七项部分**：出口侧**路径变更已验**（经自写 UDP 中继换源端口 ⇒
  `quic: 路径变更` ×3、世代未重建、变更后 `↑33MB/s ↓24MB/s`）；**客户端 `rebind()` 真迁移未验**（无蜂窝）。
- **真机新发现六条（真机特有，M3 的输入）**：①**准入失败归因不回传客户端**（**最有价值**）——出口能分
  `MAC 不符`/`revoked`/`table-full`，核侧三种一律只打「登记失败（连接在登记窗内关闭）」，随后 WG 回落把
  世代撑成 `state=attached, readyBy=wg` ⇒ **黑洞期设备侧三层原因全不可见**（归 M2 设计面/M3）；
  ②**出口重启自愈 ≈32s / 41s** vs 门槛 3.5s ≈ **10×**（**归因经 M3 实测订正**：驱动源不是 60s 巡检拍，
  而是 **QUIC 空闲回收 30s + keep_alive 相位**，本机复现 40.028s——见 `docs/reviews/M3-design.md` §13/§15-5；
  处置归 M3 阶梯重写/M6）；
  ③**UPnP 成功 ≠ 公网端点可用**（映射建成但 IP/端口证据不合格 ⇒ fail-closed 不公布——部署文档该点名）；
  ④设备身份**设备持久**（错 token 档 dev/pub 与正常档逐字节同，只换 MAC 输入）⇒ 解释了重连只落 refresh；
  ⑤表满闸「在途未认证」分母**随 cap 缩放**（32→`/64`、1→`/2`），压测读数别当常量；
  ⑥WG 档出口**不记**「源校验拒」（同一现象只在 QUIC 档可见 = 观测面差异，非行为差异）。
- **未决**：token 候选 B（待拍板）；N4（`RETRY_AFTER_FAILS=5` 与 `per_src_fails=16` 不同步）
  **裁定 = 保持独立**（5 = 施加压力阈值、16 = 拒绝阈值，构成单调升级，非缺陷）。

## M3 服务流迁移（files / term / speedtest / 巡检）（估 4–6 会话日）

**目标**：核心自连全走 STREAM；客户端 smoltcp（stackb）退役。

**范围**：

- STREAM 协议：首字节 tag（1=files / 2=term / 3=speedtest / 4=dial / 5=probe）+ 既有应用层帧
  **原样**（term HSP、files proto、speedtest 帧逐字节不变）；出口按 tag 分发到对应服务（服务
  入口从 UDS accept 换成 stream 适配器，**应用层零改动**）。
- 客户端：`bridge_host` 的 `DialFn` 改开 STREAM；`session/recover` 阶梯重写（断线 = 重连/迁移；
  不再有 R1/R2/R3 档位语义）；巡检 = `STREAM[probe]`；**清零客户端 QUIC 档的 stackb 消费点**
  （路线文件原记 5 处〔`tun_exec.rs:238/334/349`、`session/mod.rs:648/667`〕，设计门实测**共 12 处 / 7 文件**
  ——多出的 6 处在 daemon/CLI 的 host 会话面，属 WG-only 路径，随本体留 M5）；「虚拟端口」（7802/7724/7803）→ tag。
- 出口：**QUIC 档不再经** intercept 的「豁免命中端口 → UDS」分支（分支本体保留至 M5——消费者 = WG 服务腿）。
- 地址派生收窄：保留 `tunIp`（App 接口地址）与出口常量 IP（DNS 目标）；**栈 B 派生地址在 QUIC 档退役**。

**判据**：App 真机 files / term / speedtest 全绿（matrix 冒烟 + E2E）；term 帧 / 键编码 /
fixtures 向量逐字节不变；**客户端 QUIC 档零 `stackb::` 可达引用**（`tools/check-quic-isolation.sh` 新断言；
原「依赖树不再含 smoltcp」不可判——出口 intercept 面共用 smoltcp，**已订正**，见
`docs/reviews/M3-design.md` §15-1）；DC14 / DC15 / CA1 / CA4 / CA5 语义对照。

**评审过程**：

- 设计门：`docs/reviews/M3-design.md`（tag 分发 / 背压 / 错误面 / 阶梯重写 = 连接策略变更）→ dsh
  （**已走两轮**：r16 首轮 27 条含 9 高危判「阻塞」→ 全部改设计；r17 复审 16 条 → 有条件通过）。
- 代码门：`docs/reviews/M3.md`（专项 = 删 stackb 后的遗留假设〔设计已列 A1–A14〕，如「环回不经隧道」语义是否仍成立）。

**退出口**：服务面全绿 + **QUIC 档 stackb 消费点清零合入**（本体删除移 M5，与 `wgcore` 同批）+ tier
`connection-lifecycle` 修订稿交付（tier 侧触点，M3 只出草案）。

**收口证据（2026-10-09；真源 = `docs/reviews/M3.md` 的 S1–S9c 各节 + `docs/reviews/M3-S8-evidence.md` 台账）**：

- **规格**：`docs/reviews/M3-design.md`（§14 设计门 r16 27 条 / r17 16 条逐条处置；§15 实施期订正 8 条；
  **§16 流控窗整改回填**〔S9 实测把「每流接收窗」定为瓶颈：量化闭合到 1.2% 差〕）。
- **实现记录**：`docs/reviews/M3.md`（S1 / S2 / S3+S5 / S4 / S6+S7 / S8 / 吞吐整改 / **S9c 代码门 + 收口**）。
- **读数台账**：`docs/reviews/M3-S8-evidence.md`（§1 本地门槛 cpu/overhead/size/mem；§2 吞吐与共存 A/B；
  §3 真机 R1–R7 逐格；§4 新发现；§5 收口门；**§7 吞吐整改定位/对照/回归 + 出口死亡随访**）。
- **判据登记**：`docs/INTEROP-CRITERIA.md` 的 M3 主表（S7 的 32 行 + **S9 整改 5 行 + 差异登记 1 行**）
  与数值语义表（+3 行）。
- **门**：workspace 测试全绿 / clippy `-D warnings` 0 / 三目标（OHOS + musl×2）check 0 error /
  `build-app-core.sh` 三门 / 隔离门 **11/11** / 词表门 PASS / e2e 三件套全绿。
- **门槛**：服务流吞吐 **1.347–1.366×**（门槛 ≥0.95×，改前 0.464×）；`T_recv` 本地两相位
  **2.45s / 1.29s**（≤3.5s）、真机 **2296ms / 847ms**（在用档）；`.so` = **4,850,368 B**
  （M3 累计 +164,896 B，**M5 删码余量重算输入**）。
- **真机（R1–R7）**：files 列目录/下载 ✓（下载件 sha256 = 源件）、term ✓（attach/分离/re-attach 同会话号）、
  speedtest ✓、`T_recv` ✓、伪造 token ⇒ `0x11` + 回落 WG ✓、`_wg` 原串 ✓；**上传面不可自动化**（系统 picker
  完成钮，tier 触点）；**R5 瞬时黑洞未做**（缺 token QUIC 端点改写面，挂 M6）。
- **未过/未做（如实）**：上述两项真机项；A10 出口侧关闭时点 e2e 断言（归 M5 出口观测面）；出口进程
  一次死亡**未复现**（随访仪器 `tools/m3-s9-bulk.sh`，进 M5 风险表）；残余共享段上限 73–76 MiB/s
  **未定论**（交 M5）；NAPI `ClientCoreTunRecover` 未分档（**接受现状，归 M4 设计门**）。

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

**收口证据（2026-10-09；真源 = `docs/reviews/M4.md` 的 S1–S6 各节）**：

- **规格**：`docs/reviews/M4-design.md`（§13 设计门 r19 13 条 + r20 3 条逐条处置；**§15/§16 实施期订正
  = 主会话裁定**〔本次收口同批落 §16：耗时上界 7.1s→12.1s 与 `-2` 构成句〕）。
- **实现记录**：`docs/reviews/M4.md`（S1–S3 / S4–S5 / **S6 判据登记 + 代码门 r21 + 收口**）。
- **判据登记**：`docs/INTEROP-CRITERIA.md` 的 M4 主表 **17 行 + 追加更正行 1 条** + 数值语义表 3 行
  （覆盖设计 §8.2 十三条「原样落」+ 各切片「交 S6」补充；全部追加式）。
- **门**：workspace 测试全绿（974 passed / 0 failed / 35 ignored）/ clippy `-D warnings` 0 /
  三目标（OHOS + musl×2）check 0 error / `build-app-core.sh` 三门（`[sym] 20/20`、`[ver] 49aad9e66525-rust`、
  `[size] **4,867,168 B**`）/ 隔离门 **11/11** / 词表门 PASS / e2e 四件套全绿（island / wg / ladder / pf）。
- **判据读数**：`-1` 误判率 **0/5**、QUIC 档 `RECOVER` 族行 **0**、下推最坏 **2.108s**（预登记 ≤8s）；
  `0x26` 真 socket 面 **10.005s**；泄漏判据 `pfFails=80`（第 80 次仍「目标拒绝」而非「入口队列满」，
  **双向负例**：把失败路径改回 H1 原形即红）；四形态 × 三失败归因逐格（形态 1 本机不可测，转真机/结构性）。
- **真机（6 场景，`FMR0224116011480`）**：R1-S① 浏览器 `http://127.0.0.1:18081` 命中出口回环服务 ✓；
  R1-S② LAN 目标命中 ✓；R2-S① 删主机 ⇒ 映射不复活 ✓；R2-S② 热替换 rc=0 ⇒ 浏览器命中新服务 ✓；
  R3-S① 占用端口单条失败、其余正常 ✓；R3-S③/负例 无服务 ⇒ **7ms** 立刻失败 + 出口归因 ✓。
  **未取**：R1-S③（分流打开态）、R3-S②（未连接进页面被 tier 门控）、S4 三指标（需人工换网）。
- **未过/未做（如实）**：上列真机三项；W1 并发打点（62 上界）；热替换后状态列「启动中…」（**tier 侧**
  观察，核侧 JSON 真值已验）；真机后段日志未存档（只留 offset，r21 F2）。

## M5 WG 路径删除与收束（估 3–5 会话日；大删码期）

**目标**：WG / wtransport / frame / reg / ring-shim 全量退役；intercept 收窄；判据全表登记；
代码净减可审计。

**范围**：

- 删：`wgcore`（除 QUIC 岛共用类型）、`wtransport`（bind / reg / endpoint_cache / domain_eps ——
  **`frame.rs` 不可删**：它是中继线协议（`relay/**` 在用）⇒ 迁址保留）、`server/device.rs`、
  `session/recover` 旧档、`tools/ring-shim`、`udpbatch.rs`（Q10 补登）、`session_connect_target` /
  `wg_dial_addr`（M4 交下）、fixtures（identity / psk / reg / endpointcache 退役登记；relay / stun 按实际
  改动画线）。**`wgcore/stackb.rs` 不可删**（其 `TunDevice` 是 intercept 的生产件）⇒ 迁址保留。
  **⚠️ 订正（M5 设计门 r22 A1 高危，2026-10-09）**：本行原文写的「删 `server/bind` 腿表族 +
  `server/relayleg.rs`」是**事实错误**——腿表族是 **QUIC 经中继的唯一通路**（`homeway-quic/src/exit/socket.rs`
  的两条物理路径 + `engine.rs::sync_quic_legs` + `relayleg.rs` 拨腿），整件删 = QUIC 经中继死，且补它
  必须改中继（反破红线）⇒ 二者改**「裁剪」**（保留与 QUIC/中继相关的腿面，删 WG-only 分支），详见设计 §1.3。
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

**红线首次触碰登记（2026-10-09，M5 A 棒）：**「中继零改动」在本程序期内**首次显式扩范围**——
M5 的 S0/S0b 迁址连带触碰 `relay/**`：`relay/mod.rs`（import 1 + 同文件 74 处路径限定符
`frame::`→`legframe::`）+ `relay/ctlface.rs`（import 1）+ `relaywire.rs`（import + 1 处测试内全路径）
= **+64/−64 行，除路径限定符外零改动**（未碰组帧/`parse_listen`/`rl1`）；独立 commit `0c42d9e`
（`docs/reviews/M5.md` §2.2 有逐行佐证）。另：S0 连带触碰 `server/intercept/**` 的 **2 个 import 行**
（`wgcore::stackb` → `stackb`，迁址必连改动），除这 2 行外 intercept 零改动。**Q2 中继 v6 双栈仍按
设计走 B 棒 S7a（独立小批 + `R1` 行登记）。**

**§0 删码余量实测（2026-10-09，M5 设计门前置实验；读数仓外 `/tmp/m5lab/`）**：
三 `git worktree` × 同一 `build-app-core.sh`/NDK strip 口径，四格矩阵（均有 20/20 符号门，评审独立复现）：

| 形态 | 无 `[profile.release]`（M0–M4 全程口径） | `lto=true`+`codegen-units=1` | 再 `opt-level="s"` |
|---|---|---|---|
| WG-only（`4841b20`） | 2,213,744 B | **1,685,144 B** | 未测 |
| 双栈（HEAD `0cf68b4`） | **4,891,776 B** | **3,556,520 B** | **2,899,368 B** |

**结论（与「删码才能达标」的预判不同）**：**3.8MB 判据的主杠杆是构建档位不是删码**——双栈 + LTO 已
**3.39MB = 0.936× 判据**（**一行 WG 未删**）；叠加删码（符号归因：wgcore 163.7K / wtransport 77.7K /
session 64.5K / boringtun 59.8K / service_exec 31.9K + facade 份额）预计终值 **≈3.0–3.25MB**。
**若不改档位则判据不可达**（4.39–4.54MB > 3.8MB）⇒ **改发布档位是唯一低成本达标路径**（属构建口径
变更，**待用户点头**）。不确定性已落纸：粗删终点直读缺失（`tun_exec` 的 WG 档与岛装配方法级纠缠、
17 处编译点 ⇒ 全量删除即 S2/S3 实现本体）、符号归因 ≠ 删后读数、`opt-s` 是已实测的第二条路径。


### 收口证据（E 棒 S6/S8，2026-10-10）

- **体积终值（product 档 = LTO + cgu=1 + NDK strip）**：`.so` = **2,958,896 B = 0.779× 判据（≤3,800,000 B）**；
  `[sym]` 20/20、`[ver]` 过。**档位口径 = L-1**（同档判、跨档不互引；M0–M4 读数均为无 profile 档）。
  出口二进制单独量 = `homeway-cli` **8,758,816 B**（同档 LTO 未 strip；**不设判据**）。
- **门槛复测（quic-ab lab 档）**：每包 CPU quic **12.929 µs**（raw 地板 4.885 µs）；线开销 **1310.194 B/包
  （30.19B）**；`max_datagram_size` 1162/1362（精确同 M0）。**三条 WG 相对列随 WG 臂退役失去同刻参照 ⇒
  只给绝对列 + M0–M3 跨期历史锚，不假装同刻 A/B。**
- **内存终值**：稳态 1216K / 负载态 max 1248K / **每连接边际（五点）65.60K ≤96K** / **32 设备 +1.386 MiB
  ≤+3.1 MiB** / 负载态（32 连接）**+656K ≤64 MiB** —— **五格全过**；**产品形态单连接 = 未过（≤+320K）
  + 已上报待裁决**。
- **真机（设备 FMR0224116011480，M5 核 + M5 私有出口）**：①层 0 全通（QUIC 单承载：赛跑→准入→
  `L3 承载 = 岛`→`判据=quic`→`attached`）②files/term/speedtest **均经 STREAM** ③portfwd 抽查 2 形态
  （回环 + LAN）④**收工/重启链**（客户端 `岛收工` + 出口 **`出口收线（连接数 1 → 0）`** = E25 行真机在册；
  force-stop 后重启自愈成功）⑤**零 panic**（hilog + 核日志 + faultlog 三面 0 命中）。
- **代码门 r27**：`exit=0`，**高 3 / 中 10 / 低 11**；高 3 全整改，中 8 改 + 2 登记，低 6 改 + 5 登记；
  **不认同 0**。
- **门（九项全过）**：workspace 测试全绿 / clippy 0 / 三目标 check / `build-app-core.sh` 三门 /
  隔离门十一条 / **`check-wg-removed.sh` 十条**（WG 残留扫查）/ 词表门 PASS / e2e 四件套 rc=0 ×4 +
  中继烟囱 / `tools/ci-local.sh` 八步全绿。
- **M5 净量（两种口径都留）**：删除面口径 **净删 ≈ −13,269 行**（C/C2/C3/C4 合计，不含新增面）；
  `git diff --numstat` 口径 `4bf8ba1..HEAD` = **+9,199 / −18,561（净 −9,362）**，共 **37 commit**。

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

**收口证据（2026-10-10；真源 = `docs/reviews/M6.md`（398 行）+ `docs/PERF-AB.md` §9.20）**：

- **2×2 矩阵**（原「核×出口」四格**构成性不存在**——WG 面 M5 已删、token/wire 换代 ⇒ 交叉两格不可做，
  已登记；实际矩阵 = **2 栈 × 2 路径**，腿 = WiFi 唯一）。旧栈锚 = **`4841b20`**（核+出口同 commit 构建）。
- **主判据（T2 TUN 直连吞吐，256 MiB 服务端 REQ→END 中位）**：热 **NEW 11.41 MB/s vs OLD 23.53 MB/s =
  0.485×（不达标；判据 ≥0.95×）**；冷/热各臂 ≥0.70 **过**（NEW 0.891 / OLD 1.050）。
- **归因（S6 触发，L0–L6 判据式）＝ 命中 L3+L4+L5**：出口清白（0.16–0.17 核、零满丢）；**每包 CPU
  NEW 114.3 µs vs OLD 51.3 µs = 2.23×**（判据 ≤1.10×，不达标）；设备 `lost_packets/congestion_events/drops`
  全 0（拥塞与 ACK 时钟清白）；同臂同机**自连面 ↑52/↓39 MB/s 无损（方向相反）** ⇒ 结论 =
  **QUIC 岛在真机 TUN 数据面的逐包成本 ≈ WG 档 2.2×，设备单核贴顶（1.00–1.12 核）下吞吐反比下降**。
- **其余判据**：T6 CPU 2.23×（**不达标**）；T9 内存四格过（每连接 96.00K 贴线上界；与 M5 65.60K 差异
  待复核）；T10 断线恢复 NEW **2.447s / 0.918s ≤3.5s 过**（旧臂两口径并报：E1 口径 26.7/46.2s、仪器
  口径 3.05/3.08s，不粉饰）；T3 经中继**未验**（旧臂在 LAN 下自动升级直连 ⇒ 参照臂不可得，非结果挑选）；
  T5/T11/T12 真机面**顺延**。
- **未验/降级（固定句法）**：蜂窝腿（无 SIM ⇒ 未验，归 M7/用户触点）、真 rebind 迁移（降级 + 替代注入
  顺延）、耗电（降级为 CPU 代理）、窄路径真机（结构不可得）、speedtest（无判据）。
- **门**：`ci-local` 八步全过（677 passed / 0 failed）、clippy 0、三目标 check、`.so` = **2,958,896 B**
  （B 臂 = 本树）、e2e 四件套 rc=0 ×4、隔离门 11/11、WG 残留门十条、词表门 PASS、**插桩零残留**（脚本核实）。
- **M6.5 有界优化批（2026-10-10，唯一性能修复窗口）→ 结论 = 结构性不可及（量化）**：
  - **逐段成本表（真机，256 MiB TUN 直连，单位 = 1252B 单元）**：TUN `write` 35.2 + TUN `read/poll` 23.2 +
    UDP `recvmsg` 22.5 + UDP `sendmsg` 10.1 = **81% 在系统调用面**；岛内用户态仅 19%（quinn proto/crypto 9.8 +
    通道/投递/预检 ~15）。**无单一热点** ⇒ 2.23× 是「逐项差」之和（UDP 面 +24 / 回程多一跳 +10 / 读面 +9.5 /
    quinn +10 / ACK 面 +8）。**「为何自连面无损而 TUN 面 2.23×」已解释**：自连面不经 TUN fd（≈32 µs/单元），
    TUN 面每包多付 4 次 TUN 系统调用 + 一跳线程 ≈ +70 µs。
  - **整改（已落）**：①回程批化（泵抽干 ≤16 条一次投递 + 写线程整流 + 队列改**包级** 2048；泵投递 5.0→3.8 µs/包）
    ②热路径小件（`packets_in`/`send_buffer_used` 锁→原子；丢弃上报口外提）③**TUN 读面阻塞化**（读线程
    6.11→2.29 s/轮，每上行包 179→66 µs）④多段接收 `recvmmsg` **实测无增益已撤回**（22.5 vs 28.1 µs/包）。
    **合计整机 CPU/单元 112 → 92 µs（−17%）**；`.so` = 2,966,608 B（+0.26%）。
  - **真机复测（同会话比值判）**：T2 热 NEW 10.54 vs OLD 20.02 MB/s = **0.527×**（仍不达标；会话漂移 ±17%，
    OLD 臂本会话慢 17% ⇒ 绝对值只作登记）；T6 每包 CPU NEW **97.9 µs** vs OLD {51.3, 84.0} = **1.45×**（仍不达标，
    目标 ≤1.10×）。
  - **量化结论**：目标 ≤56.4 µs/单元，现状 92–98 ⇒ **仍需再降 ~42%**；卡在「TUN 写 + 读面 + UDP 收发」
    的同内核系统调用面；三条结构性前提（内核 UDP 真批化 / 非每包 TUN write / 增大内层 MTU 与 1280 判据冲突）
    **本批均不可得** ⇒ **0.95× 结构性不可及**，客户端侧可动份额已取尽。
- **M6.6 差分量测（2026-10-10，**用户质疑触发**）→ **推翻 M6.5 的归因**（重要订正）**：
  用户指出「TUN 与 UDP 的接口两个方案都在用，为什么会有明显差异」⇒ 主会话复核认为 M6.5 的
  「系统调用面 81% ⇒ 结构性不可及」**不成立（只剖了一臂 = 构成分析，不是差分析）** ⇒ 补做两臂同源差分：
  - **同速率档对照**（把旧臂限速到新臂的 ~11 MB/s）：旧臂每单元 CPU 51.3 → **93.3/97.9 µs**（与新臂
    93.3/93.3/88.6/93.3/97.9 **同一带**）⇒ **同速率跨栈 = 1.00×（±5%）**；同栈跨速率 = **+1.8×**
    ⇒ **M6.5 的 2.23× 里「速率档」项 ≈100%、「真多做」≤5%（方向：新臂略省）**。
  - **B1 假设（旧臂批量收发）不成立**：`udpbatch`（sendmmsg）只用于外层（`wtransport/bind`、`server/bind`、
    `relay`），**客户端数据面零用**；两臂系统调用计数/单元几乎相同（TUN write 1.000、UDP recv ~1.01）。
  - **每单元 CPU ≈ a + b/速率**（旧臂两档拟合 a≈3.5 µs、b≈0.79 s/s）⇒ 速率减半即 CPU 翻倍；设备核占空
    恒 **0.7–1.0 核/秒不随速率变** ⇒ 低速率下固定开销摊薄差。
  - **新结论（订正）**：**「0.95× 结构性不可及」不成立**；真问题是**新臂达成的包率只有旧臂一半**
    （~8.5k vs 16–18k 包/s）**且根因未定位** ⇒ 立为 **M7 头号待验**（强线索：**同一栈同一链路，
    STREAM 自连面 ↓39 MB/s vs TUN DATAGRAM 10 MB/s** ⇒ 嫌疑集中在 **DATAGRAM 专用面**
    （quinn 数据报调度/发送侧窗口与 pacing/ACK 时钟 vs WG 哑管道无拥塞控制））。判据现状仍**不达标**。
- **M6.7 根因定位 + 修复（2026-10-10，**由 M6.6 的「包率差」线索追出**）→ 真 bug 已修**：
  - **根因**：岛侧 UDP socket 用**默认 `SO_RCVBUF`** ⇒ **设备内核接收缓冲溢出**（`/proc/net/snmp Udp` 的
    `RcvbufErrors +17/+21/+32`/轮）⇒ 丢包被外层当成拥塞（`lost=24 cong=7`/轮）⇒ CUBIC 退避 ⇒ 吞吐塌陷。
    （M6.5 的「系统调用面」与 M6.6 的「速率档」都只到现象层；**真因在这一环**。）
  - **修复**：`ClientSock::open` 显式 **`SO_RCVBUF=2 MiB` / `SO_SNDBUF=1 MiB`**（读回实际值记行）
    ⇒ `RcvbufErrors` **→ 0**、`lost/cong` **24/7 → 0/0**、T2 热 **23.81 s → 13.33 s（11.27 → 20.14 MB/s）**。
  - **复测**：T2 热 = **0.893×**（M6 0.485× / M6.5 0.527× / M6.6 0.484×）；T6 = **1.36×**（70.0 vs 51.3 µs）；
    冷/热比 1.027/1.021 过。**主判据仍未达 0.95×（差 11%）**，如实登记。
  - **剩余 11% 的量化结论**：修后无丢轮里 quinn cwnd 长到 12–45 MB、瞬时 **36k–45k 包/s（47–59 MB/s）
    超过空口 L0 锚 44.8 MB/s** ⇒ 过载 ⇒ 下一轮 `lost=64–79` ⇒ CUBIC 连乘退避 + 内层 RTO ⇒ 双峰
    （12.6–13.3 s vs 15–18 s）。**缺一条「按带宽而非按丢包」的发送速率控制**；候选（须设计/判据变更）=
    自定义 `congestion_controller_factory` / 出口面整形 / BBR 类 ⇒ **交 M7**。
  - **连带订正**：「STREAM 自连 39 MB/s」是 **3 并发之合**（单流 ≈13 MB/s），机制差 = **STREAM 有接收侧流控、
    DATAGRAM 没有**（超量压进内核缓冲 ⇒ 溢出丢包直接变内层 TCP 丢段）；上行包率两臂差 = **设备侧延迟 ACK
    两态驻留比例**（下游约束的镜像，非独立差异）。
  - **登记**：内存账 **+3 MiB（显式 socket 缓冲，2 MiB 收 + 1 MiB 发）** 进判据/内存矩阵（实现棒草案已备）。
- **（历史指针，已被 M6.6/M6.7 与上方裁决取代）M7 决策点原版本**：①接受差异并登记后进 M7（当时读数 ~0.53×；
  代价与收益并列：系统调用面固有 + QUIC 的迁移/无自研承载补偿/代码 −13k 行/体积 0.779×/自连面无损）；
  ②另开承载结构批（需内核面能力，用户态已证不可得）；③其它（含 revert M5 = 唯一回退路径）。
  **主会话建议 = ①**（结构化收益已兑现、客户端侧可动份额取尽；把差异如实登记并让 M7 的滚动升级承担可回退面）。
  另：T_recv 口径统一／T9 每连接差异复核／T5/T11/S7 补测安排。

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
| 每包 CPU | **绝对值登记**：quic **12.929 µs / 包**（M5 终值；raw 地板 4.885 µs）〔M5 后 WG 参照臂随 `boringtun` 退役构不出 ⇒ 原「≤ 现役 WG+shim ×1.0」的相对口径**构成性不可得**，改为绝对列 + M0–M3 跨期历史锚（12.7–12.9 µs）；**不假装同刻 A/B**〕 | harness（`quic-ab.sh`，lab 档；**跨档不互引**） |
| 线开销 | ≤ 40B/包 | QUIC oneway 精测（服务端 `udp_rx` 口径） |
| 真机吞吐 | 热态 ≥ 0.95× 现役；冷/热 ≥ 0.70 | 同刻交替 A/B（PERF-AB §1/§9 口径） |
| 换网迁移 | 连接保持（无重连）；出口设备表不新增条目 **且中继腿表峰值 ≤ N（实测登记，防「腿表增长被误判为通过」）** | 真机 WiFi→蜂窝 |
| 断线恢复 | 出口重启恢复 ≤ 3.5s（现役 R1 命中 3.126s 量级） | 故障注入（沿用 R2 批手法） |
| 体积 | OHOS `.so` **≤ 3,800,000 B（product 档 = LTO + cgu=1 + NDK strip）**〔**M5 落**：原判据在同档下**构成性不可达**（无 profile 档双栈 4,891,776 → 删完仍 ≈4.4MB）；改档后双栈即 3,556,520 B。**终值 = 2,958,896 B = 0.779×**。**档位口径 = L-1：同档判、跨档不得互引**（M0–M4 全部读数为无 profile 档）；同档历史锚 = WG-only+LTO 1,685,144 B〕 | size 矩阵（OHOS cdylib）+ `build-app-core.sh` |
| 内存 | **单连接 ≤ +640K；每设备 ≤ 96K；32 设备 ≤ +3.1MiB（稳态）；出口 32 连接持续流量增量 ≤ 64MiB + 自有队列上限（负载态）**〔M1 设计门 + 用户拍板 2026-10-08 修订：原 256K/64K/+2MB 的每连接锚来自附录 A 手抄 37.6K，无原始证据链，实测五点拟合 81.6K / 三点 96.0K〕〔**2026-10-10 用户批准再修订**：单连接 320K → **640K**（M5 设计门事后拟合 = 实测上界 608K ×1.05；**失效条件**：若后续帧尺寸/连接模型变化（如内层 MTU 调整、回程队列默认值变更）则须重测重订）。**M5 终值五格全过**；该格自 M1 起的历史读数（+496K/+608K）按新门槛**转为达标**〕 | footprint 三轮下中位（harness；**拟合口径 = 五点 N=1..5**，三点作对照） |
| 丢包可观测 | DATAGRAM 超限 / 丢弃有计数行，不静默 | 窄路径注入（MTU<1340） |
| 中继承载 | **同刻 A/B 相对判据**（经中继 vs 直连的比值；绝对吞吐数字只作登记、标注不可比）——并同时记 `congestion_events`/`lost_packets`（分辨「限速器静默丢被 QUIC 当拥塞」）〔M1 设计门 + 用户拍板 2026-10-08〕 | 本地中继（`tools/local-rust-relay.sh`）+ 真机复测 |

- **门槛表的 M5 终值口径（2026-10-10，含用户批准的内存格修订）**：内存四条 = 每设备 ≤96K /
  **单连接 ≤+640K** / 32 设备 ≤+3.1MiB / 负载态 ≤64MiB+队列 ⇒ **M5 终值六格全过**（单连接 +496–608K
  按新门槛达标 / 每连接 65.60K / 32 设备 +1.386MiB / 负载 +656K / 稳态 1216K / 负载态峰值 1248K）。
  体积行已按 L-1 改档位限定（见上）。**内存格口径变更的登记条目 = `docs/INTEROP-CRITERIA.md` 的 M5 批
  追加条（实现棒落）**。

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
  （M2 S2 批实测 1 红、隔离复跑同二进制内翻转 = 已在册签名）；
  **M2 S6 复跑读数**：`daemon::tests::handshake_deadline_beats_slow_drip` /
  `server_bad_frame_gets_goodbye_and_disconnect` 在全量并行跑 4 次里 3 次各红 1 例、隔离全绿、
  基线对照（stash 到 `779251d`）2/2 绿、两文件本批零 diff ⇒ 归在册 `daemon::tests` 实 socket 时序族
  （**本树红频率高于基线，未解释，如实登记**）。
- **工具坑登记（M2 实测，防后续棒踩）**：①`tools/quic-island-e2e.sh` 与 `tools/quic-wg-e2e.sh`
  **共用实例号 state**（`/tmp/homeway-rs-rustexit-1`）且 `serve token` 读台账末行 ⇒ 先跑 island
  再跑 wg 会读到上一轮铸的 token 而**假红**；解法 = wg 跑前先 `tools/local-rust-exit.sh wipe 1`。
  ②`tools/local-rust-exit.sh start` 在 `target/release/homeway-cli` 存在时**不重建** ⇒ 改完核必须先
  `cargo build --release -p homeway-cli` 再起实例，否则跑到旧二进制（现象 = 新行/新配置缺席）。
- **本程序新增（M3 实现时登记，2026-10-09）**：①`term::service::tests::attach_size_applies_to_pty` 在
  M3 各批全量并行跑里**多次**红（S6/S7/S9/S9c 各一次；**隔离复跑恒绿**，M3 对该文件仅「入口形参/在册闸」
  类改动、与 60s 硬期限无关）⇒ 归**在册 load-sensitive 族**，红了先隔离复跑；②
  `wgcore::tests::stop_within_detaches_and_reaper_closes_wake_fd`（S6 首轮全量并行 1 例红；隔离复跑绿）；
  ③`client::tests::stream_write_reports_backpressure_with_original_buffer`（S9 吞吐棒期间**真红**：该用例
  依赖「出口缺省接收窗 256 KiB」这一自变量 ⇒ 已按「自钉自变量」修，**修后三轮全绿、非 flake**）；
  ④`client::tests::send_buffer_used_grows_under_load` 的交叉证据断言已按代码门 r18 C7② 改写为不依赖
  排空速率的判据（原形态随调度翻转）——该用例**从在册 flake 面移出**（主判据不变）；
  ⑤`daemon::tests::handshake_deadline_beats_slow_drip` 在 S8/S9/S9c 批继续复现（在册族）。
  **口径照 M0 §9.2 ④**：红了先隔离单跑再判回归，**不静默重跑**。

- **本程序新增（M4 实现/收口时登记，2026-10-09）**：**零新增 flake**——M4 各批（S1–S5 门、S6 最终全门）
  的 workspace 全量并行跑未触发任何在册族。**工具坑新增一条**：`tools/quic-pf-e2e.sh` 依赖
  「本机 `169.254.169.254` 是黑洞」（`0x26` 格）与「本机有直连网段地址」（LAN 形态，可用
  `HOMEWAY_PF_E2E_LAN_IP` 覆盖）——换机器/换网络要先看这两条。

- **M5 E 棒（2026-10-10）**：`term::service::tests::attach_size_applies_to_pty` 全量并行跑一次红
  （60s 硬期限护栏 = 环境性挂死），`--test-threads=1` 隔离复跑绿 ⇒ **在册 flake（不新增类别）**。
  另有既存两项（`stackb` 墙钟门、`daemon::tests::server_bad_frame_gets_goodbye_and_disconnect`）——
  代码门用 `git archive 4bf8ba1` 基线对照证明为**既存**（非 M5 引入）。

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
