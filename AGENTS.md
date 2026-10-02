# AGENTS.md — homeway-rs（Rust 平移 homeway）

本仓是 homeway（Go，`~/Documents/projects/homeway`）的 **Rust 平行实现**：三角色（出口/客户端/中继）
与 Go 版任意组合互操作，最终接入鸿蒙 APP。**唯一进度真源 = `ROADMAP.md`**——新会话先读它
（续接协议/隔离条款/各期范围与判据都在那里）。

## 硬规则（违反就出事）

1. **只本地，不推远端**：不 push、不建 GitHub 仓（用户显式点头前）。
2. **两仓只读**：`~/Documents/projects/homeway` 与 `~/Documents/projects/tier` 的跟踪文件一律
   不改；Go 侧源码/构建/测试向量只从 `baseline/homeway` 快照克隆走（见 ROADMAP 隔离条款）。
   发版会话收官信号由用户给出，在那之前不进 R7。
3. **现役出口不碰**：互操作测试一律 `tools/local-exit.sh` / matrix 脚本起的本地私有实例。
4. **对齐三件套**：线协议字节（golden 夹具 + 向量，`fixtures/`）、行为常量
   （`tier:docs/agents/connection-lifecycle.md` 为真源）、契约词表
   （`tier:tools/gen/vocab-manifest.json` 生成对账）。三者对不上 = 没对齐。
5. **评审两道门**（技术评审/代码评审，子任务内完成，记录入 `docs/reviews/`），见 ROADMAP
   「评审协议」。
6. 提交信息中文、每工作单元一 commit；`target/`、`baseline/`、`bin/` 不入库。

## 技术底座（已定，勿重新调研）

- 依赖面：wireguard-go→**boringtun 0.6**（ring 0.16 无 OHOS 支持 ⇒ `tools/` 内 vendored
  ring 垫片 `[patch.crates-io]`，配方源自 `tier:tools/spikes/rust-ohos-poc/ring-shim/`）；
  netstack→**smoltcp 0.11**；x/crypto→x25519-dalek/hkdf/sha2；toml→serde。
  OHOS 交叉（R7）：`rustup target aarch64-unknown-linux-ohos` + DevEco NDK
  `aarch64-unknown-linux-ohos-clang` 链接器（`.cargo/config.toml` 已备，PoC 实测一次过）。
- 终端栈（R6）：libghostty-vt → **alacritty_terminal 0.26**（仿真+Damage+kitty 模式位）+
  **自建应答器（~200 行）与键/鼠标/焦点编码器（数百行）**——Rust 生态无服务端编码器库，
  这是已知缺口不是调研项。
- 已实测锚点：Rust 全依赖面 dylib strip 1.0MB（Go 核 9.2MB）；smoltcp 栈对栈 22Gbps vs
  netstack 5.3Gbps（同机）；详见 `tier:tools/spikes/rust-ohos-poc/RESULTS.md`。

## 速查

| 事 | 去 |
|---|---|
| 干什么/干到哪/怎么接棒 | `ROADMAP.md` |
| 基线 hash / 台账 / 在途 change | `docs/BASELINE.md` |
| 互操作判据行 | `docs/INTEROP-CRITERIA.md` |
| golden 夹具 / 派生向量 | `fixtures/` |
| 起本地 Go 出口/矩阵 | `tools/local-exit.sh`、`tools/matrix.sh`（R5 建） |
| 连接行为常量 | `tier:docs/agents/connection-lifecycle.md`（只读） |
| 需求语义 | baseline 克隆里 `openspec/specs/`（45 份，只读） |
