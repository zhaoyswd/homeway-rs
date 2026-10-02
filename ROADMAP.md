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
| **R0** | 基线锚定 + 互操作基建 + 仓库骨架 | **完成**（2026-10-02） | 6/6 |
| **R1** | 客户端垂直切片（直连数据面 ↔ Go 出口） | **完成**（2026-10-02） | 7/7 |
| **R2** | 客户端全量（行为对齐 + 中继腿 + files/portfwd + facade 预留） | **完成**（2026-10-02） | 7/7 |
| **R3** | 出口（多 peer WG device + 拦截层 + files/DNS/STUN/UPnP + servercore） | **完成**（2026-10-02：两道门全过 + 判据全量实测入册） | 6/6 步 |
| **R4** | 中继（信封 + 准入 + 升级条纹） | **完成**（2026-10-03：两道门全过 + 三链路判据实测 + 升级条纹实测） | 4/4 步 |
| **R5** | 互操作矩阵全量 + fuzz + 性能 A/B + 台账三方门 | 未开始 | — |
| **R6** | term 服务面（协议/surface 产出/检测引擎 + 自建编码器/应答器） | 未开始 | — |
| **R7** | APP 接入（napi-rs 或 C-ABI 胶水 + OHOS 交叉 + 20 导出面 + hostsession） | 未开始 | — |
| **R8** | 终测收官（包体/性能终测 + 共存定案 + 文档指针补录） | 未开始 | — |

**下一步（当前指针）**：**R5 互操作矩阵 + 治理收口**（matrix.sh 全组合编排 + fuzz +
性能 A/B + 台账三方门；R4 顺带的登记项一并入批：enum Auth 类型化、出口侧测试扩展到
Go 对照面全套、中继吞吐量化——同机 200pps 防放大限速为 Go 同值的固有锚）。R4 已收官
（2026-10-03 两道门全过）：第 1 会话完成技术评审 v2（1 高/6 阻塞全整改）+ 4a 全量
（relaywire golden 向量族/relay 本体单驱动线程/TCP 控制面/rl1 CLI）+ 4b 条纹两处实错
修正 + 4c exit 注册腿全量接线 + 4d 三链路实测（联调修三个实测 bug：leg_readable 丢弃
Inbound/控制面读循环阻塞保活/cookie 认证借用）+ 升级条纹实测 + 判据入册 + 第二道门
（1 高/4 中/10 低全处置）。完成证据见下方 R4 节；评审记录 = `docs/reviews/R4.md`。
R5 批登记（R4 豁免/顺带）：M3 TCP DNS 腿、M8 残余（SSDP 组播三件套）、M11 UPnP ctx、
M20 STUN/SPED golden、M23 错误类型化、M24 客户端 speedtest 归因、R4-§7.8 enum Auth、
中继吞吐量化。R2 登记：tunStatusJSON 完整键面归 R7、低-4/低-7 残余/低-10 精确形态/
中-10③ 挂账归属期见 docs/reviews/R2.md 豁免表。

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

**完成证据（2026-10-02）**：
- 0.1 `docs/BASELINE.md`（两仓均 `621fe0e` 未前移；登记 ROADMAP 两处漂移）；
- 0.2 克隆可构建（`bin/homeway-go` 22MB，GOTOOLCHAIN=go1.24.5；vt 三级回退，R0.6 评审
  整改后自足克隆 + 无远程）；
- 0.3 烟囱全链实测：`serve 就绪`/`客户端 token 铸出`/`intercept: 过境拦截就绪`/
  `peer: + dev=37a8115c…`/`link: via=direct ep=127.0.0.1:42641`/`speedtest: 会话 #8
  role=send bytes=203355105`/`intercept: tcp exempt …（dialok）` 等 20+ 真实行入
  `docs/INTEROP-CRITERIA.md`（出口 23 条 / 客户端 17 条 / 命令面 1 条，出处 = 克隆内
  文件:行号；隧道流量旁证 收 858MB/发 951MB）；
- 0.4 `fixtures/vectors/` 三族（token 8 正 10 负含哨兵文案段 / 隧道地址含守卫命中样本 /
  身份派生含 store 全路径交叉验证），生成器入克隆直调生产真源、重跑字节确定（diff 门）；
  golden 拷贝 6 目录 + `SHA256SUMS` 45 项；
- 0.5 `cargo test` 全绿（10 用例：7 单测 + 3 对照）；ring 垫片 `cargo tree` 验证
  boringtun→本地 shim；
- 0.6 外部技术评审（dsh）1 高/14 中/13 低 → 全部整改或登记豁免（`docs/reviews/R0.md`），
  高危 H1/M12/M13 均已复测（恶意 config 覆盖、假就绪消除、占端口硬失败）。

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

**完成证据（2026-10-02，fresh pairing = local-exit wipe 后首连）**：
- 判据四条全采：出口 `peer: + dev=88d6c8ca pub=f7d1232a ip=100.64.213.172 n=1/32`
  （三指纹与客户端 identity 派生逐项一致）；客户端 `warmup pong: 就绪（判据=wg）` →
  `link: via=direct ep=127.0.0.1:42641 rtt=0ms（服务会话巡检）`；speedtest 双向
  **down 291–779Mbps / up 223–488Mbps**（多轮；Go 客户端同时刻 A/B 232/249Mbps、
  R0 峰值口径 690/760Mbps——±50% 界内，最好轮超 Go 峰值），下行对账偏差 0.14–0.52%；
  出口 `intercept: tcp transit 192.168.3.12:9999 ← 100.64.213.172:34321（dialok）`；
- 顺手补采：E10-transit/E22/E23/C8（Rust 同串）/C17 入 INTEROP-CRITERIA；
- 两道门：技术评审 8 高/21 中/15 低全处置（R1-design v2）；代码评审 2 高/12 中/
  15 低，必修面全整改 + 复验（`docs/reviews/R1.md`）；
- 实测抓出并修复：make_tunn peer 位真公钥 bug、TCP 流跨帧读丢字节（FrameReader）、
  send_slice Ok(0) 语义、CloseWait EOF 判据、确定性四元组撞出口半开连接（端口随机化）；
- **R2 专项移交（差异分析已记录）**：同身份对「含旧 peer 会话状态的出口」快速重连，
  WG 数据包在出口侧静默丢弃（握手可完成、先到的包解密成功、后续消失；客户端侧
  decap 零错、发包零错）——wipe 配对即愈。嫌疑面 = wireguard-go 同 pubkey 快速重连的
  keypair/时戳窗 × boringtun 时戳戳记；Go 客户端同场景走恢复阶梯（ResetPeerSession+
  RefreshReg，正是 R2 范围）。R1 测试纪律 = 每测量批 wipe 出口（fresh pairing）。

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

**完成证据（2026-10-02，全部 release 构建实测；设计/评审/判据细节 = `docs/reviews/R2.md`）**：
- 恢复阶梯四档时间窗实测（注入：出口重启 / poison-socket 测试缝【test-seams】/
  出口换端口+中继兜底 / 停机）：**R1 命中 3.126s（≤4s ✓）/ R2 命中 18.4s（≈16s ✓）/
  R3 命中 38.7s（≈29s 带，含 DirectFirst 2s 窗+握手重传——首发丢包形态，Go 口径已登记）/
  最坏 39.8s（≈45s 带，纯预算满烧 3×13s）**；RECOVER 全族判据行同串（评审脚本机械对拍）；
- 2a WG 重连专项：五轮注入（clean stop/SIGKILL/背靠背×3）**零复现**——R1 现象未再现，
  登记未能复现（判定三条件全绿），恢复语义已由阶梯实装覆盖（ResetPeerSession = 统一
  rebuild_tunn 唯一重建点，与 expired 兜底共用）；
- 中继腿全链：`link: via=relay ep=127.0.0.1:42741` + `MIRROR 直连窗口 2s 内无响应 →
  解锁中继候选 1 个并补发一次`（(pkt,reg) 捕获对重投）+ 赛跑结算胜出中继 + ⚠️ 告警行 +
  RREG 中继=true + 经中继 speedtest 跑通（local-relay.sh 本地 Go 中继拓扑）；
- files 100MB 上传 3.4s / 下载 2.96s **字节对账偏差 0**（sha256 双侧一致）；实测抓出并修
  critical bug：帧解析 drain 边界混入 4B 前缀（合成流单测 5 种切块 + 真实对账双证）；
- 状态 JSON 对照 **PASS**（Go `host status --json` vs Rust `--status-json`：link/identity/
  state/stats 键集与取值一致、键序字典序）；tunStatusJSON 完整键面归 R7（登记）；
- portfwd 烟囱（监听行同串 + 经转发收到载荷）；E9 已采（`--peer-ttl 15s` 注入：
  `peer: - dev=… reason=ttl (idle=7m47s)`）；E12 归 R3（客户端无 UDP 拨号面）；
- vecgen 三族新向量（reg 报文/端点缓存 JSON 字节/files 帧含 70KB 跨 u16 样本）+ Rust
  对照测试逐字节绿；吞吐锚 release speedtest down 393–404Mbps / up 265–359Mbps（R1 界内；
  debug 构建假回归 25Mbps 的排查记录在案——判据测量一律 release）；
- 两道门：技术评审（dsh，1 高/15 中/20 低全处置——高-1 解锁补发 reg 搭车/前缀口径 R1
  实错修正等）；代码评审（dsh，0 高/9 中/16 低——必修面全整改 + 复验，豁免逐条登记）；
- 单测 68 全绿（59 lib + 9 向量/集成），clippy 0 warning（基线清零）。

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

**完成证据（2026-10-02，两道门全过；判据实采行全量 = `docs/INTEROP-CRITERIA.md`「Rust 出口侧实采」节）**：
- 判据全量实测（fresh state、release 构建、`tools/local-rust-exit.sh` 端口 42651）：
  E1-E14/E17-E20/E22/E23 全串打出（含 `--peer-ttl 15s` 注入的 `ttl=15s` 与
  `peer: - reason=ttl (idle=20s)`、revoked 拒绝族 `peer: ! reject reason=revoked`+
  首大声、多 peer 混跑 `n=2/32`（Go+Rust 客户端并发）、E12 `udp intercept: 会话 #1
  dns 建立（8.8.8.8:53 ← …）`、E22 三面计数 `q=1 qtcp=1 resp=3`、同 socket STUN
  真观测 `STUN：监听 socket（本地 42697）在 162.159.207.1:3478 眼里是
  203.175.12.191:29397` + 「暂不公布」保守分支同串）；E21 绑卡指纹行本地形态未采
  （`--bind-interface none`）——CIDR/排序面已按 Go 修正。
- 吞吐 A/B（同一时刻）：Rust↔Rust speedtest **down 250-263Mbps / up 398-408Mbps**
  （对账偏差 0.42-1.83%）；Rust↔Go 同时刻 down 406/up 367（down 为 Go 的 ~62%，
  ±50% 界内）；Go↔Rust down 210/up 460。files 100MB 双向 **对账偏差 0**（sha256
  双侧一致；Rust 上传 2.6s）；Go 客户端 put/get 100MB 同样偏差 0。
- DNS 代答实测：dnstest 三面全通（tcp5300 解析腿 rcode=0 / udp53 隧道栈 listener /
  leg 拦截进程内腿 8.8.8.8:53——应答源反重写正确）。
- 拦截层四连修（实测抓出，commit d5bfbe1）：① `send_slice` 部分写静默丢字节→
  tx_backlog 回补；② FIN 先于 backlog 排队挤丢尾数据→fin_pending；③ worker
  `Written` 只在立即写尽才回执→flush_and_report 差额补报（上行卡死根因）；④ `Ack`
  只认进栈内 socket 的字节——端到端背压重建（UDS 拥塞 → 服务端 write_all 墙钟限速，
  Go gVisor 端点缓冲反压等价物；此前 2s 预热被 0.2s 泵完、窗口计数全废）；另
  WG socket SO_SNDBUF/RCVBUF 4MB + 流发送缓冲 1MB。
- 两道门：技术评审 v2（第 1 会话，dsh 4高/15中/11低）；代码评审（第 2 会话，dsh
  4高/24中/低择要——**高危 4 条全修**：UDP fd 泄漏 EMFILE/DNS TCP 槽位复用
  panic/speedtest 会话号错位/SSDP 字节切 panic；中危 18 修 6 登记豁免归 R5，
  逐条表 = `docs/reviews/R3.md` 第二道门节）。
- 测试面：107 单测全绿 + clippy all-targets 0（连续多轮）。
- 交付件：`homeway-cli serve`（config.toml 同 schema deny_unknown + flag>config>默认
  覆盖序 + SIGTERM D5 有序收工）+ `serve token [list|revoke]`（reveal 一轮制/台账/
  吊销秒级跟随）+ `dnstest`（E12/DNS 判据产出步骤）+ 客户端 UDP 拨号面 +
  `tools/local-rust-exit.sh`（端口 4265x 隔离 + Go 客户端通道）。

**进度注记（2026-10-02，第 1 会话末）**：
- 技术评审第一道门完成（dsh `r3d.3hsyns` 轮次；4 高/15 中/11 低全处置，v2 定稿）。
  高危整改全部落地：H1 容器帧（bind.rs handle_batch）、H2 拨号先行（SYN 缓存 +
  DialOk 后 listen 注入 + DialFailed RST|ACK）、H3 DNS 专用线程（设计落位，dnsproxy
  待 3d 剩余实装）、H4 固定 worker 池（8×poll(2)，含 Written/Ack 背压闭环）。
- 3a：`server/device.rs`（两表分发——M1 删 pending-init 表落地；base 跨握手稳定
  单测钉死；漫游「先更新后应答」；expired 不重建）+ `server/bind.rs`（腿帧分发含
  容器、probe 应答防放大约束、端口退让、E23 新源日志）。
- 3b：`server/table.rs`（register 返回 DevOp 序列——数据与副作用分离，天然消 Go 的
  opCh FIFO 队列；stale/ttl/rotate/拒绝归因全语义 + 判据行）+ `server/state.rs`
  （key/tokens/revoked 台账，JSON 键序与 Go 字节对齐单测）。
- 3c：`server/intercept/`——nat.rs（校验和族 + RST|ACK/ICMP responder + MSS 形态
  SYN 构造）、pool.rs（worker 池）、mod.rs（Interceptor 主体：RX 分流 served-port
  demux 优先/未登记走 NAT、UDP pending 纯载荷 ≤16 丢最新、TX 反重写、水位背压、
  idle 看门狗、Drain/HaltNew、Stats 与 E5/E10/E11/E12/拒绝判据行）。实测抓出并修
  三个关键 bug：L4 校验和双取反（标准形独立算法钉死）、CloseWait 推进误关 Listen
  socket（未连接态 may_recv 恒 false）、worker 池 pollfd 构建把 flow id 误当 fd。
  端到端测试：豁免流建连+数据往返 / 拨号失败 RST / UDP 会话回投反重写。
- 3d-files：`files_server.rs` 六动词全实现（每命令一流/沙箱/tierpart/UDS 死活判别
  chmod0600），R2 客户端同协议对拍测试绿。
- 测试面：85 lib 全绿 + clippy 0（连续多轮）。
- **遗留给第 2 会话**：3d 剩余（dnsproxy.rs——上游跟随/ID 重写/TC→TCP/过滤类，
  设计 H3 的 DNS 专用线程待接；speedtest_server.rs——SPED 帧受理/结算判据行 E13，
  顺手修 L10 客户端 first_frame bug）、3e（egress.rs/upnp.rs）、3f（serve 装配 +
  判据实测：Go client ↔ Rust exit 全判据/Rust 闭环/多 peer 混跑 n=2/32/TTL+吊销
  注入/DNS 代答实测）、R2 移交微项（低-7 载荷收窄在 3b 错误面已就位可顺手、低-10
  set_timeout 已在 3c 落地、低-5/6/8/12/16）、第二道门代码评审。

## R4 中继（估 3–4 会话日）

**目标**：Rust 中继对全 Go 链路透明替换。
范围：rl1 准入（X25519 挑战）、`[0xAA][relayID]` 信封换壳、per-client socket、升级条纹
（`relayUpgradeStreak`，baseline 克隆 `tunmode.go` 语义）。
判据：**全 Go 链路（Go exit ↔ Go client）只把 relay 换成 Rust 版**，手机式客户端测试腿仍
`via=relay` 全判据绿；这是最干净的单变量互操作证明。
**退出口**：无（范围小；若准入协议细节缺失，回 baseline 克隆源码补读）。

**完成证据（2026-10-03，两道门全过；实采行全量 = `docs/INTEROP-CRITERIA.md`「Rust 中继侧实采」节）**：
- **链路 1（全 Go 只换 relay——单变量互操作证明）**：Go exit 向 Rust relay 双路注册
  （UDP 腿 `中继：后端 … 注册成功（腿 127.0.0.1:42645）` + TCP 控制面
  `中继：后端 … 控制面就绪（…；SESSION 通告启用拨腿模式）`；exit 侧 OK-MAC 双向认证行
  `中继控制面：中继身份已认证（OK-MAC 通过）`）；Go client 经 Rust relay 达
  `赛跑结算：胜出 中继 127.0.0.1:42781` + `路径确立：中继 127.0.0.1:42781` + WG 握手
  经中继往返（转发 上/下 计数）+ `暖机就绪`；随后 hint→盲打自愈回直连（Go 设计行为，
  真机 NAT 下不会发生——测试形态口径已注记）。出口换端口注入的
  `中继：后端 … 注册腿地址变化 → …（旧分配 0 条已作废…）` 与
  `中继：客户端 … 起会话 #1（拨腿模式）→ …（数据口 …）` 全串打出。
- **链路 2（Rust 全栈）**：`link: via=relay ep=127.0.0.1:42781 rtt=7ms（服务会话巡检）` +
  `RREG 注册刷新 → …（中继=true）` + 经中继 speedtest 下行 23.7MB 偏差 **-0.28%** +
  files 5MB 上传/下载经中继 **sha256 双侧一致**（952e76af…）。上行为中继 200pps
  防放大限速所囿（**Go 同值**——R2 经中继上行 2.7Mbps 即此上限实测锚）。
- **链路 3（Go exit + Rust relay + Rust client）**：via=relay + RREG 中继=true + 经中继
  数据（Go 出口 speedtest 服务端收上行 3.2MB `speedtest: 会话 #8 role=recv bytes=3211215`）；
  speedtest 全窗跑满受混合链路吞吐限制（量化归 R5）。
- **升级条纹（4b 两处实错修正的实证）**：5 拍 via=relay 驻留 →
  `RELAY-UPGRADE：已在中继停留 5m0s，重新武装赛跑试直连（下一发出站包镜像到全部候选）` →
  `RARM 软赛跑（中继立即参与，同时试直连）`（修正①：此前误用硬 rearm）→
  `RELAY-UPGRADE：升级成功 → via=direct ep=192.168.3.12:42811 rtt=6ms`（修正②：此前缺失）→
  `link: via=direct`（直连恢复 = 出口搬回 token 原端口；中继 --no-hints 测试形态）。
- **测试面**：136 单测全绿（relay 27：注册状态机/转发闭环/拨腿端到端/回收/握手
  DH 恒校负例/慢滴绝对期限/保活回显 + bind 腿表 3 + 条纹纯函数）+ relay 向量族
  golden（vecgen 产自 baseline 真源，含 DH 定值/256B 分帧边界）+ clippy all-targets 0。
- **交付件**：`homeway-cli relay`（前台单角色/两级日志 2MB×3/rl1 铸出）+ exit 侧
  `--relay`（flag>config/relay_ep 四块语义/token 中继端点恒标 relay）+ `relaywire`
  中立模块 + `tools/local-rust-relay.sh`。
- **实测抓出并修**：leg_readable 丢弃 process_packet 的 Inbound（腿上 WG 载荷进不了
  device——有握手无数据根因）、exit 控制面读循环无期限预算阻塞保活（90s 判死循环
  重连）、出口 hint 盲打与客户端采纳的同机耦合（relay-lock/no-hints 测试缝的依据）。
- **两道门**：技术评审 v2（1 高/6 阻塞 + 中低全处置——TCP DH 恒校/读侧四件套/
  try_clone 窗口/rearm 复合/MaxLegs 断连形态/token 四块语义）；代码评审
  （1 高/4 中/10 低——慢滴握手期限/punch 空转/控制连接回收/出口侧测试/身份私钥入库
  + 低危清账，全处置表 = `docs/reviews/R4.md` 第二道门节）。

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
   checklist 恒含「**Go 直译痕迹**」检查（多余 Arc/Mutex、字符串错误、接口仿写、无谓拷贝、
   包结构 1:1 强映射——地道 Rust 口径见仓 AGENTS「工程原则」节），高危项必须整改或登记豁免
   理由后才算该期完成。
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
  1. **G1（R0.4，2026-10-02）**：`pkg/proto.DecodeToken("hmw")` 对恰 3 字节 hmw 前缀串
     panic（`token.go:105` 的 `s[:4]` 越界，实测 `slice bounds out of range [:4] with length 3`）。
     对抗性输入面（用户粘贴残串可触）；Rust 侧已按安全语义返回 UnsupportedVersion
     （`crates/homeway-core/src/token.rs` 头注记）。修在 Go 仓自己的流程。
