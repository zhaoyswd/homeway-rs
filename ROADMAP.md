# homeway-rs 平移 Roadmap（跨期进度真源）

> **新会话续接协议（三步）**：①读本文件；②按「状态总览」找到第一个未完成期，读该期小节
> （范围/判据/退出口/评审门）；③按「评审协议」派发子 agent 执行，主会话只做调度
> （读指针→派发→收摘要→更新指针/勾选→本地 commit）。用户说「继续 rust roadmap / 接着干」即指此协议。
> **推进方式沿用 `tier:docs/agents/pipeline.md` 八步流水线的形态**，但本程序只做两道评审门
> （技术评审 + 代码评审，都在子任务内完成），不做 openspec 载体（对齐目标是 homeway 现有
> specs/契约，不开新 change；若某期需要结构性决策再单独立 openspec）。
> **更新协议（硬规矩）**：子任务回报后，主会话**同一次提交**里更新本表勾选与「下一步指针」；
> 期完成时把判据证据（日期 + 判据行/测试输出摘要）写进该期小节。
>
> ⚠️ **隔离条款（2026-10-02 立项时点，最高优先级）**：
> 1. **另一会话正在做 homeway 发版与双出口部署**——本程序不得 push 任何远端、不得改动
>    `~/Documents/projects/homeway` 与 `tier` 两仓的**跟踪文件**（只读引用）；
> 2. **现役出口（Mac launchd / 阿里云）一律不碰**（不重启/不换装/不当测试对端）；
>    所有互操作测试的 Go 侧一律用 `baseline/homeway` 快照克隆起的**本地私有实例**；
> 3. Go 侧源码/构建/向量生成只从 `baseline/homeway`（`git clone --shared` 自 dev 仓、
>    钉基线 hash、gitignore 不入库）走——dev 工作区是发版会话的，禁止在其中跑重测试/构建落盘；
> 4. 升级基线（dev 仓前移后 rebase 快照克隆）是**显式动作**：更新 `docs/BASELINE.md` +
>    roadmap 提交信息里注明。
> 5. 发版会话收官的信号由用户给出；在那之前，任何要动 tier/homeway 跟踪文件的步骤（R7 起）
>    都不开工。

## 总体目标

实现一版**功能完全对齐**的 Rust homeway：同一线协议、同 token 形态、同行为语义（连接生命周期/
恢复阶梯/拦截/巡检逐常量对齐）、同契约（tunStatusJSON / surface v4 / 词表台账），三角色
（出口/客户端/中继）可与现役 Go 版任意组合互操作；最终（R7）接入鸿蒙 APP 替换 `libclientcore.so`。
Go 版**共存不替换**——Rust 版是平行实现，对齐验收全靠与 Go 版 A/B。

### 非目标（本程序范围外）

- 不改 Go 版行为（发现 Go 侧 bug 走 Go 仓自己的流程，登记到 roadmap 附录「发现的 Go 侧问题」）；
- 不做 Mac APP / web UI（那是旧 roadmap 的另立项方向）；
- 不追新功能——只对齐**基线 hash 上的**功能面（基线见 `docs/BASELINE.md`）。

## 立项依据（已验证事实，勿重复调研；细节见附录 A/B/C）

- **PoC 已实测**（`tier:tools/spikes/rust-ohos-poc/RESULTS.md`）：boringtun+smoltcp+dalek+serde
  全依赖面 OHOS cdylib strip 后 1.0MB vs Go 核 9.2MB；smoltcp 栈对栈 22Gbps vs netstack 5.3Gbps
  （同机同形态，4×）；OHOS 交叉一次过（NDK clang 链接器）；**boringtun 0.6 钉 ring 0.16 无 OHOS
  支持，已验证 `[patch]` 同算法垫片方案**（ring-shim 源码在 PoC 目录）。
- **终端缺口已核实**（源码级）：alacritty_terminal 0.26 = 仿真+Damage+kitty 模式位完整，但
  **应答器（~200 行）与键/鼠标/焦点编码器（数百行）须自建**；wezterm-term 不可引用；
  vt 绑定 Go 侧 3,543 行要换成「alacritty_terminal + 自建编码/应答」。
- **规模与成本实测**（附录 A/B）：全功能对齐估 47–72 专注会话日、Rust 新增 ~38–42k 行；
  数据面对齐（不含 term）35–54 会话日。

## 状态总览（手维护）

| 期 | 内容 | 状态 | 进度 |
|---|---|---|---|
| **R0** | 基线锚定 + 互操作基建 + 仓库骨架 | 进行中 | 0/6 |
| **R1** | 客户端垂直切片（直连数据面 ↔ Go 出口） | 未开始 | — |
| **R2** | 客户端全量（行为对齐 + 中继腿 + files/portfwd + facade 预留） | 未开始 | — |
| **R3** | 出口（多 peer WG device + 拦截层 + files/DNS/STUN/UPnP + servercore） | 未开始 | — |
| **R4** | 中继（信封 + 准入 + 升级条纹） | 未开始 | — |
| **R5** | 互操作矩阵全量 + fuzz + 性能 A/B + 台账三方门 | 未开始 | — |
| **R6** | term 服务面（协议/surface 产出/检测引擎 + 自建编码器/应答器） | 未开始 | — |
| **R7** | APP 接入（napi-rs 或 C-ABI 胶水 + OHOS 交叉 + 20 导出面 + hostsession） | 未开始 | — |
| **R8** | 终测收官（包体/性能终测 + 共存定案 + 文档指针补录） | 未开始 | — |

**下一步（当前指针）**：R0 基线锚定与互操作基建（首个子任务已派发）。

依赖：R0→R1→R2→{R3, R4 可并行}→R5→R6→R7（**需发版会话收官 + 用户点头**）→R8。
R4 最小可提前（不依赖 R1，只需 R0 夹具），但优先保 R1 主线。

---

## R0 基线锚定 + 互操作基建（估 3–5 会话日）

**目标**：把「对齐的标尺」全部钉死可复现，Rust 仓库骨架成型，本地 Go 出口能起能测。

| # | 任务 | 判据 |
|---|---|---|
| 0.1 | `docs/BASELINE.md`：基线 hash（homeway dev 与 tier submodule pin，2026-10-02 时点均 = `621fe0e`）、Go 版本、契约台账行数、tier 侧在途 openspec change 清单（files-server-bounds 等，影响 R7 词表门） | 文件入库，字段齐 |
| 0.2 | baseline 快照克隆：`git clone --shared ~/Documents/projects/homeway baseline/homeway` + checkout 基线 hash（gitignore；此后 Go 侧一切构建/向量都从它走） | 克隆可构建（vt prebuilt 缺则跑 `tools/build-vt.sh darwin-arm64`，或从 dev 仓拷 `prebuilt/`） |
| 0.3 | 本地 Go 出口烟囱：从 baseline 克隆构建 `homeway` 二进制，起 localhost 私有实例（临时 state），采判据行样例（`serve 就绪`/token 铸出/`intercept: 过境拦截就绪`）→ `docs/INTEROP-CRITERIA.md`（判据对齐清单：warmup/attached/intercept/peer 行/speedtest/files/term 各判据行 + 出处 `文件:行号`） | 脚本 `tools/local-exit.sh` 一键起停；判据文档含 ≥5 条真实采到的行 |
| 0.4 | 夹具与向量：golden 夹具清单（term surface 上行字节表/样式向量、files 帧）拷入 `fixtures/` 并记来源 hash；**测试向量生成**——token 解析/隧道 IP 派生走公开包 `pkg/proto`（外部模块 + replace 到 baseline 克隆）；identity 派生在 `clientcore/internal/wtransport`（internal 不可外部导入）⇒ 生成程序放进 baseline 克隆内运行（模板存 `tools/vector-gen/`，脚本拷入克隆再 `go run`），产 JSON 向量集 | `fixtures/vectors/*.json` ≥ token/IP/devTag 三族向量；生成脚本可重跑 |
| 0.5 | cargo workspace 骨架：`crates/homeway-core`（lib）+ `crates/homeway-cli`（bin 占位）+ `rust-toolchain.toml`（钉当前 stable）+ `.cargo/config.toml`（OHOS target 链接器配置，拷 PoC 配方、注释 R7 才用）+ ring 垫片 vendored（拷 PoC `ring-shim/`，`[patch.crates-io]` 就位）+ 首个对照测试（token 解析吃 0.4 向量） | `cargo test` 绿（token parse 对 Go 向量逐字节一致） |
| 0.6 | 评审门：技术评审（骨架/基线纪律/夹具策略/烟囱脚本）→ 整改 → 本地 commit（中文信息） | 评审记录入 `docs/reviews/R0.md` |

**退出口**：任何一步被发版会话冲突卡住（如 dev 仓不可读）→ 停止并上报主会话，不自行绕过。

## R1 客户端垂直切片（估 8–12 会话日）

**目标**：Rust 测试客户端与**本地 Go 出口**建立隧道并跑通数据面判据——wire、行为、性能三重对齐
路径的第一次实证。客户端形态 = 无 TUN 的栈内客户端（对齐 Go 侧「服务会话」形态：smoltcp 栈 B +
隧道 IP 拨号），APP 形态留给 R7。

范围：proto 底座（帧/编解码字节精确，吃 R0 向量与夹具）→ identity（master.key HKDF/devTag，
复用 621fe0e 的 `wtransport/identity*.go` 语义）→ wtransport **直连子集**（单源 Bind、候选表、
端点学习缓存内存版、**不做**漫游/中继腿/R1–R3）→ wgcore（boringtun noise + ring 垫片 + 自管
UDP socket + smoltcp 栈 B + speedtest/probe 客户端）→ CLI：`homeway-cli connect --token <hmw1>`
（token 从本地 exit 实例取）。

判据（对本地 Go exit 实测）：出口 `peer: +`（devTag 与 Go 客户端连出的一致性规则）；客户端
`warmup pong: 就绪（判据=wg）`；`link: via=direct ep=…`；speedtest 吞吐与 Go 客户端同量级
（±50%，host 环境差异容差）；Go exit 日志 `intercept: tcp transit` 行出现。
技术评审要点：ring 垫片 vendor 策略、boringtun 双向握手时序 vs wireguard-go 差异表、
栈 B 装配的会话语义。
**退出口**：boringtun 握手兼容性问题 → 记录现象，退到「先做 R4 中继（最小）」，主会话重排。

## R2 客户端全量（估 6–10 会话日）

**目标**：连接行为逐常量对齐（`tier:docs/agents/connection-lifecycle.md` 是单一真源，改任何
常量必须双向同步它——但本程序不改 Go 侧，只对齐）。

范围：恢复阶梯 R1–R3 全档位（节拍/阈值/门控/起跑点/时延记账逐常量移植 + 判据行同串）、
漫游/换源、端点缓存三层来源+落盘格式、60s 巡检 keepalive、**中继腿**（对本地 Go relay 实例测，
不必等 R4）、files 客户端（每命令一流+问候+4B 帧+write 关流即取消，golden 对齐）、portfwd、
facade trait 预留（tun prepare/attach 两阶段语义 + `tunStatusJSON` 契约产出——JSON 逐字段对
Go 快照测试）、hostsession 的非 APP 部分留接口桩。
判据：R1/R2/R3 命中时间窗（≤4s/≈16s/≈29s）在注入故障下实测吻合；files 上传下载字节对账；
tunStatusJSON 与 Go 版快照 diff 为空（去除时间戳类字段）。
**退出口**：行为对齐超支 → 允许先把「直连+巡检+files」闭环交付，恢复阶梯细节挂账到 R5 补。

## R3 出口（估 10–15 会话日）

**目标**：Rust 出口对 Go 客户端透明替换（本地 A/B）。

范围：**多 peer WG device 自建**（boringtun noise 原语之上：peer 表/index 分发/漫游跟随/
keepalive 语义——这是全程序唯一剩余的中风险技术点，技术评审必须先行）、devTag 设备表
（cap=32/TTL 7 天/活跃宽限 10 分钟/绝不淘汰在线）、token 台账（hmw1 铸/一轮制/吊销秒级生效/
serve token 命令面最小集）、L3 拦截层（smoltcp `tcp.Forwarder` 对应物 + UDP 五元组长会话 +
pending 重放 + 豁免规则/LocalServices UDS 映射）、DNS 代答（5300，上游跟主机解析）、
files 服务（`files.sock`、根=$HOME 恒读写）、STUN/UPnP（egress 平移，默认 stun.cloudflare.com）、
servercore 装配（config.toml 同 schema、serve.enabled 期望态）。
判据：Go 客户端（baseline 克隆的 clientcore 集成测试 harness / daemon stream 客户端）对
Rust exit 全判据绿；Rust exit ↔ Rust client 闭环；`intercept: …（dialok）` 计数语义一致。
**退出口**：多 peer device 自建受阻 → 用「Go exit + Rust 拦截层以外部件」分片验证，device 层
单独攻坚（允许 R3 拆成 3a/3b）。

## R4 中继（估 3–4 会话日）

**目标**：Rust 中继对全 Go 链路透明替换。
范围：rl1 准入（X25519 挑战）、`[0xAA][relayID]` 信封换壳、per-client socket、升级条纹
（`relayUpgradeStreak`，baseline 克隆 `tunmode.go` 语义）。
判据：**全 Go 链路（Go exit ↔ Go client）只把 relay 换成 Rust 版**，手机式客户端测试腿仍
`via=relay` 全判据绿；这是最干净的单变量互操作证明。
**退出口**：无（范围小；若准入协议细节缺失，回 baseline 克隆源码补读）。

## R5 互操作矩阵 + 治理收口（估 5–8 会话日）

范围：`tools/matrix.sh` 编排 {Go,Rust}×{exit,client,relay} 全组合（本地实例池，端口错开），
每链路跑判据集；fuzz（帧解析/token/拦截边界，`cargo-fuzz` 或结构化随机重放）；性能 A/B 报告
（吞吐/尾延迟/常驻 RSS，扩 PoC 基准）；契约台账三方门（`tier:tools/gen/vocab-manifest.json`
为共用真源，Rust 侧值集由它生成对账——只读消费 tier 资产）；本地 CI 脚本（无远端）。
判据：矩阵全绿脚本化可重跑；fuzz 无新破口；A/B 报告入库 `docs/PERF-AB.md`。

## R6 term 服务面（估 12–18 会话日，最大单项）

范围：alacritty_terminal 接入（Term + Damage + 模式位）→ **自建应答器**（DA1/DSR-CPR/DECRQM/
OSC 10/11，~200 行）→ **自建键编码器**（kitty protocol 全编码/modifyOtherKeys/legacy 键表，
数百行，对齐 herdr 补丁 0002 暴露的模式查询语义）+ 鼠标/焦点编码 → term 协议栈
（`[op:len2LE]` 帧、HELLO tail caps+ver+id、stateV2、ENDED 词表）→ **surface v4 产出端**
（快照+差分+样式向量+模式位，golden 夹具钉字节——夹具含样式向量/上行字节表）→ 检测引擎
（manifest 规则 + OSC 证据，`pkg/term/manifest` 语义）→ 会话面（多腿/attach 回放/尺寸哨兵/
有界输出环）。
判据：golden 全对齐（含样式向量）；**Go 的 term CLI（`homeway term attach`，baseline 克隆构建）
能作为客户端消费 Rust term 服务**，列表/新建/attach/重放/状态徽章全流程；Rust exit 整体
（R3+R6）对 Go 客户端全判据。
**退出口**：键编码器兼容面（vim/htop/kitty 查询）超支 → 分「基础编码先行 + kitty 全量挂 R6.5」，
不阻塞其它期。

## R7 APP 接入（估 8–12 会话日 + 真机；**开工前置条件：发版会话收官 + 用户点头**）

范围：napi-rs OHOS 支持评估（首要技术评审；**退路 = C-ABI + 手写 NAPI 胶水**，PoC 已验证
OHOS 交叉与链接配方）→ 20 个导出面实装（语义真源 = `tier:AGENTS.md` 原生契约四处同步清单 +
`tools/docs/check-napi-sync.sh` 的导出对账）→ hostsession/facade 实装（App 服务桥 UDS/桥 auth/
状态推送/预算挂起语义）→ HSP 集成（替换 `libclientcore.so` 的路径与并存策略——**动 tier 跟踪
文件，需用户触点**）→ 词表门三方化（tier `tools/gen` 生成物对齐）→ 真机判据全量
（`attached（数据面已接管 fd=N，L3 直通）`、恢复阶梯真机时间窗、冻结/挂起恢复）。
判据：真机全量判据 + 包体实测（对比 9.2MB，PoC 估 1.3–1.5MB）。

## R8 终测收官

性能/包体终测报告、双栈共存定案（合入 homeway 仓 `rust/` vs 独立仓——用户触点）、
tier 文档地图指针补录（`docs/agents/roadmap.md` 或 AGENTS.md 加一行指针——用户触点，发版
会话收官后做）、本仓 AGENTS/README 定稿、遗留项清账（附录「发现的 Go 侧问题」移交清单）。

---

## 评审协议（两道门，子任务内完成）

1. **技术评审（开工前）**：每期开工的子 agent 先产出该期设计要点（对齐表/风险/拆步），
   然后评审——优先调用 `reviewer` skill（dsh 外部评审，`~/.agents/skills/reviewer`）；
   不可用时用结构化自评（实现者/评审者双角色分离，按 checklist：对齐完整性/边界/并发/
   错误面/与基线漂移）。记录入 `docs/reviews/R<N>.md`。
2. **代码评审（完工后）**：实现+自测绿后，同一子 agent 内跑第二道评审（同上渠道），
   高危项必须整改或登记豁免理由后才算该期完成。
3. 主会话只核对「评审记录存在 + 判据证据在报告里」，不重读代码。

## 用户触点清单（须显式点头，其余全自动）

- push / 建远端仓（GitHub 公开化）；
- 动现役出口（测试一律本地实例，永不）；
- 动 tier / homeway 两仓跟踪文件（R7 起的 HSP 集成、R8 的文档指针）；
- R7 开工本身（发版会话收官确认后）；
- ring 垫片长期化方案（fork boringtun vs 维持 patch）与共存定案。

## 附录 A：规模盘点（2026-10-02 实测于基线 621fe0e 检出）

homeway 全仓 52,157 行非测试 + 46,997 行测试。分块：共享底座（proto 894 + nodeconfig 424 +
nodestate 1,091）；客户端核心（wtransport 1,698 + wgcore 1,374 + wgnet 336 + speedtest 1,461 +
probe 596）；客户端 App 桥（facade 3,209 + hostsession 2,047 + NAPI 壳 3,982，R7 前大部分留桩）；
出口（server 4,081 + servercore 1,806 + intercept 774 + dns 1,043 + egress 584）；files 1,966；
中继 1,957；term 协议/会话/检测 ~6,700 + vt 绑定 3,543 + CLI 1,852；daemon/control 8,443（MVP
只取最小命令面）。契约台账 349 单元。

## 附录 B：成本模型（会话日 = agent 专注工作日含自评/整改）

R0 3–5；R1 8–12；R2 6–10；R3 10–15；R4 3–4；R5 5–8；**数据面合计 35–54**；R6 12–18；
R7 8–12+真机；R8 2–3。全量 47–72。Rust 新增代码估 38–42k 行。

## 附录 C：指针地图

- 本仓：`docs/BASELINE.md`（基线）、`docs/INTEROP-CRITERIA.md`(判据)、`docs/reviews/`
  （评审记录）、`fixtures/`（golden+向量）、`baseline/homeway`（Go 快照克隆，gitignore）。
- tier 仓（只读）：`AGENTS.md`（原生契约/硬规则）、`docs/agents/connection-lifecycle.md`
  （连接行为真源）、`docs/agents/verification.md`、`tools/spikes/rust-ohos-poc/`（PoC 全套，
  含 ring-shim 与 .cargo/config 配方）、`tools/gen/vocab-manifest.json`（词表共用真源）。
- homeway（经 baseline 克隆读）：`AGENTS.md`、`openspec/specs/`（45 份，需求真源）、
  `contracts/ledger.jsonl`（契约台账）、`pkg/term/testdata`（surface golden）。
- 记忆（跨会话自动加载）：`tier-rust-port-poc-2026-10-01`、`tier-terminal-rust-port-gap`、
  `tier-rust-homeway-parallel-impl-cost`。
- 发现的 Go 侧问题（登记不修）：本节随推进追加。
