# AGENTS.md — homeway-rs（homeway 的 Rust 实现，已转正为唯一实现）

本仓是 homeway（Go，`~/Documents/projects/homeway`）的 Rust 实现，**Go 版已于 2026-10-05
整体退役（tier 侧 C 批转正）——本仓即唯一实现**。三角色（出口/客户端核/中继）同一二进制
`homeway-cli`，OHOS 侧以 `crates/homeway-capi` 的 C-ABI 壳供给鸿蒙 App。
**唯一进度真源 = `ROADMAP.md`**——新会话先读它（续接协议/隔离条款/各期范围与判据都在那里）；
2026-10-07 起的整改批真源 = `docs/REVIEW-ROADMAP.md` + `docs/reviews/AUDIT-2026-10-07.md`。

## 硬规则（违反就出事）

1. **远端 = `github.com/zhaoyswd/homeway-rs`（公开仓，2026-10-05 用户建仓并授权推送——
   转正 A 批；推送前敏感扫描存档见 `docs/PUSH-PRECHECK.md`）**。CI = `.github/workflows/ci.yml`
   （push/PR：test 双 runner + clippy + OHOS/musl 交叉 check）；发版 = 推 `v*` tag 触发
   `release.yml`（四目标产物 + tag 纪律门 + SHA256SUMS）。**推 tag / 发 Release / 建 PR
   仍需用户明确指令**；日常 main 直推照旧。本地-only 门（基线/向量/词表/**WG 残留门**
   〔`tools/check-wg-removed.sh`〕/矩阵冒烟）走 `tools/ci-local.sh`，不在公开 CI 复刻
   （依赖私有 baseline/）。
2. **两仓只读**：`~/Documents/projects/homeway` 与 `~/Documents/projects/tier` 的跟踪文件一律
   不改——**Go 已退役（2026-10-05），该条现为纯隔离纪律**（历史原因：曾有一并发发版会话占着
   两仓）。tier 的 `tools/tailcat/homeway-rs.pin` 前进与 tier 文档指针仍由**用户触点**控制
   （本仓侧改 pin 不是本仓的事）。Go 侧源码/构建/测试向量只从 `baseline/homeway` 快照克隆走
   （见 ROADMAP 隔离条款）；该快照 = **只读 oracle，不再前移**（`docs/BASELINE.md` 冻结声明）。
3. **现役出口不碰**：互操作测试一律 `tools/local-exit.sh` / matrix 脚本起的本地私有实例
   （**Go 出口 = 历史 oracle（Go 已退役），仅供对照复现**；现役本地链路 = `tools/local-rust-exit.sh`）。
4. **对齐三件套**：线协议字节（golden 夹具 + 向量，`fixtures/`）、行为常量
   （`tier:docs/agents/connection-lifecycle.md` 为真源）、契约词表
   （`tier:tools/gen/vocab-manifest.json` 生成对账）。三者对不上 = 没对齐。
   **判据行政策见 `docs/INTEROP-CRITERIA.md`「判据变更记录」节**：判据行**不再是字节级冻结物**——
   任何变更必须在该节登记（日期/条目/从→到/原因/影响面）并随同批 commit；对齐验收 = 同串，
   或显式登记差异。**未登记的措辞改动仍是静默破坏对齐**。
   **WG/token 承载面的字节对齐义务随 M5 换代终止**（`hmw2` 载体、QUIC 单承载；保留面 =
   relaywire / term / files / 控制面仍在对齐面内；判据政策与登记照旧）。
5. **评审两道门**（技术评审/代码评审，子任务内完成，记录入 `docs/reviews/`），见 ROADMAP
   「评审协议」。
6. 提交信息中文、每工作单元一 commit；`target/`、`baseline/`、`bin/` 不入库。

## 工程原则：地道 Rust，不做 Go 直译（2026-10-02 用户要求，全程有效）

「形随手惯用，行随对齐」——代码形态按 Rust 习惯，行为字节仍逐一对齐 Go 基线。

1. **类型承担不变量**：newtype（`Token`/`DevTag` 等）代替裸字符串/字节数组；enum 替代
   int 常量与字符串模式匹配；错误一律 thiserror 类型 + `Result` 链，**不用字符串错误**；
   协议帧类型标 `#[non_exhaustive]`。
2. **借用优先**：token/帧解析走 `&[u8]` 借用零拷贝（必要时手写索引推进），不照 Go 习惯
   先 copy 成 Vec 再切。
3. **可见性最小**：`pub(crate)` 优先，模块边界按 Rust 惯例划分，不照 Go 包结构 1:1 映射。
4. **行为对齐不放松**：wire 字节/常量/判据行/时间窗仍必须与 Go 逐一对齐（对照向量与
   golden 夹具），形态重构不给行为漂移留口子。
5. 评审 checklist 恒含一条：**是否存在 Go 直译痕迹**（多余 Arc/Mutex、字符串错误、
   接口仿写、无谓拷贝、包结构 1:1 强映射）。

## 技术底座（已定，勿重新调研）

- 依赖面：**QUIC 承载：quinn 0.11 + rustls 0.23（ring 0.17 provider）**；`boringtun` /
  `tools/ring-shim` / `[patch.crates-io]` **已退役（M5）**；
  netstack→**smoltcp 0.14**（features `socket-tcp-cubic` + `socket-tcp-reno`——R8-8a 迁入
  CUBIC 替 CC 垫片、R8-2 加开 reno 消融臂；见顶层 `Cargo.toml`）——**客户端 stackb 退役**；
  出口 intercept 保留 smoltcp（服务「任意目的地址」全局代理，与承载无关）；
  x/crypto→x25519-dalek/hkdf/sha2；toml→serde。
  OHOS 交叉（R7）：`rustup target aarch64-unknown-linux-ohos` + DevEco NDK
  `aarch64-unknown-linux-ohos-clang` 链接器（`.cargo/config.toml` 已备，PoC 实测一次过）。
- 终端栈（R6）：libghostty-vt → **alacritty_terminal 0.26**（仿真+Damage+kitty 模式位）+
  **自建应答器（~200 行）与键/鼠标/焦点编码器（数百行）**——Rust 生态无服务端编码器库，
  这是已知缺口不是调研项。
- 已实测锚点：**OHOS `.so` = 2,958,896 B（M5 终值）/ 2,968,016 B（M6.7 修复后）**，
  判据 ≤3,800,000 B ⇒ **0.779×**（product 档 = LTO + `codegen-units=1` + NDK strip；
  同档判、跨档不互引，L-1）；出口二进制单独量 8,758,816 B（**不设判据**）；
  smoltcp 栈对栈 22Gbps vs netstack 5.3Gbps（同机）；详见 `docs/QUIC-BASELINE.md` §1、
  `tier:tools/spikes/rust-ohos-poc/RESULTS.md` 与 `docs/E2E-APP-RUST-CORE.md`。

## 速查

| 事 | 去 |
|---|---|
| **App 侧消费本仓（tier）**：pin 钉定 + 构建 | tier `tools/tailcat/build-core.sh`（HEAD 必须为 tier `tools/tailcat/homeway-rs.pin` 的相等或后代〔祖先语义〕；前进 = 同批改 pin；脏检出/未钉定逃生口在脚本头）——**核侧 commit 合入 main 后，若要出 App 包，记得让 tier 侧前进 pin** |
| 干什么/干到哪/怎么接棒 | `ROADMAP.md`（程序主体）；**整改批进度** = `docs/REVIEW-ROADMAP.md` |
| 整改批发现清单 / 用户快速上手 | `docs/reviews/AUDIT-2026-10-07.md` / `README.md` |
| 基线 hash / 台账 / 在途 change | `docs/BASELINE.md`（**冻结**：锚 `d4148f6`，只读 oracle，不再前移） |
| **传输层换代程序**（WG → QUIC，M0–M7；**M7 生产切换待 U1–U3**——路线文件状态由主会话收口） | `docs/QUIC-ROADMAP.md`；三基线 = `docs/QUIC-BASELINE.md`；真机手册 = `docs/DEVICE-TEST-OHOS.md` |
| 互操作判据行 | `docs/INTEROP-CRITERIA.md`（含「判据变更记录」——变更须登记） |
| golden 夹具 / 派生向量 | `fixtures/`（`SHA256SUMS` 覆盖全目录 + `MANIFEST.md` 口径） |
| 起本地出口/矩阵 | **主力** = `tools/local-rust-exit.sh`（Rust 出口，单公共端口 = QUIC 端口）、`tools/matrix.sh`；`tools/local-exit.sh` = **历史 oracle（Go 已退役，仅供对照复现）** |
| 连接行为常量 | `tier:docs/agents/connection-lifecycle.md`（只读；tier 侧真源） |
| 需求语义 | tier `openspec/specs/`（37 份，只读——跨仓需求规格真源）；baseline 快照里无 `openspec/`，Go 侧契约真源 = `baseline/homeway/contracts/`（422 单元台账），**快照为历史参照** |
