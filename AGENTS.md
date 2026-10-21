# AGENTS.md — homeway-rs

本仓 = **homeway**：一个「回家」网络工具，**三角色（出口 / 中继 / 客户端核）同一二进制**
`homeway`（包名 `homeway-cli`——`-p homeway-cli` 构建契约；产物名 = `homeway`）。
出口 = QUIC 端点 + 设备表 + 过境拦截 + files/term/DNS 等服务；中继 = 信封转发 + 准入；
客户端 = QUIC 连接岛（赛跑/准入/迁移/恢复阶梯）+ TUN 面，另经 `crates/homeway-capi`
的 C-ABI 壳（`libclientcore.so`）供鸿蒙 App 的 VPN 扩展进程加载。

**唯一进度真源 = `ROADMAP.md`**——新会话先读它（续接协议 / 各期范围与判据 / 下一步指针都在那里）；
2026-10-07 起的整改批真源 = `docs/REVIEW-ROADMAP.md` + `docs/reviews/AUDIT-2026-10-07.md`。

## 硬规则（违反就出事）

1. **远端 = `github.com/zhaoyswd/homeway-rs`（公开仓）**。CI = `.github/workflows/ci.yml`
   （push/PR：test 双 runner + clippy + OHOS/musl 交叉 check）；发版 = 推 `v*` tag 触发
   `release.yml`（五目标产物〔darwin-arm64/amd64、linux-amd64/arm64-musl、windows-amd64=zip〕
   + **tag 纪律门**〔tag 必须打在 `origin/main` 头上〕+ `SHA256SUMS`）；发版流程 =
   **改版本 → commit → 推 main → 打 tag 在 main 头上 → 推 tag → 等 GitHub Release 构建通过**。
   **用户明确说「部署」即授权这条完整链路**（含版本提交、推 main/tag、Release 产物下载校验
   与部署），不必拆项重复确认；仅评审/修代码不自动获得部署授权，建 PR 仍需明确指令。
   本地-only 门（测试/向量/词表/残留门/矩阵冒烟）
   走 `tools/ci-local.sh`，不在公开 CI 复刻。
2. **部署纪律（2026-10-13 起）**：**正式部署 = 系统 PATH 内的 `homeway`、参数最简**——
   零参 / 子命令直接跑，**不带 `--state`**（state 走缺省 `$HOME/.config/homeway`）。
   `--state <dir>` **只用于测试、多实例、临时验证**（私有实例一律显式 `--state /tmp/…`）。
   **产物纪律（2026-10-21 起）**：Mac/阿里云正式二进制一律用对应 tag 的 **GitHub Release**
   产物，禁止以本机 cargo、远端临时 GNU 构建或其他未发布件替代。先在本机下载并核对
   `SHA256SUMS`，阿里云件由本机 **scp 上传**后复核哈希，不能依赖阿里云直连 GitHub 下载。
   换装前备份现役二进制与 state，保留身份/config/凭证，走现行部署手册；私有测试构建不等于
   正式发布，不能因测试通过越过 tag/Release 门。
3. **生产实例不碰**：本地联调 / 互操作测试一律用 `tools/local-rust-exit.sh`（Rust 出口）与
   `tools/local-rust-relay.sh`（Rust 中继）起的**私有实例**（`/tmp` state + 独立端口）；
   矩阵/性能 A/B 同纪律（`tools/matrix.sh`、`tools/perf-ab.sh`）。
4. **线协议与判据的真源**：wire 字节 = `fixtures/`（golden 夹具 + 派生向量，`SHA256SUMS`
   覆盖全目录）；判据行 = `docs/INTEROP-CRITERIA.md`（**变更必须登记**在「判据变更记录」节：
   日期 / 条目 / 从→到 / 原因 / 影响面，随同批 commit——未登记的措辞改动 = 静默破坏对齐）。
   行为常量（时间窗/节流/阈值）以代码内注明的真源为准，改动同批登记。
5. **评审两道门**（技术评审 / 代码评审，子任务内完成，记录入 `docs/reviews/`），见
   `ROADMAP.md`「评审协议」。
6. 提交信息中文、每工作单元一 commit；`target/`、`bin/` 等构建产物不入库。

## 工程原则：地道 Rust，不做逐行移植（2026-10-02 用户要求，全程有效）

「形随手惯用，行随对齐」——代码形态按 Rust 习惯，行为字节逐一对齐规格与夹具。

1. **类型承担不变量**：newtype（`Token`/`DevTag` 等）代替裸字符串/字节数组；enum 替代
   int 常量与字符串模式匹配；错误一律 thiserror 类型 + `Result` 链，**不用字符串错误**；
   协议帧类型标 `#[non_exhaustive]`。
2. **借用优先**：token/帧解析走 `&[u8]` 借用零拷贝（必要时手写索引推进），不先 copy
   成 `Vec` 再切。
3. **可见性最小**：`pub(crate)` 优先，模块边界按 Rust 惯例划分。
4. **行为对齐不放松**：wire 字节 / 常量 / 判据行 / 时间窗必须与规格逐一对齐（对照
   `fixtures/` 向量与 golden 夹具），形态重构不给行为漂移留口子。
5. 评审 checklist 恒含一条：**是否存在逐行移植痕迹**（多余 `Arc`/`Mutex`、字符串错误、
   接口仿写、无谓拷贝、包结构 1:1 强映射）。

## 技术底座（已定，勿重新调研）

- 依赖面：**QUIC 承载：quinn 0.11 + rustls 0.23（ring 0.17 provider）**；
  netstack→**smoltcp 0.14**（features `socket-tcp-cubic` + `socket-tcp-reno`
  ——CUBIC 为默认、reno 为消融臂；见顶层 `Cargo.toml`）；
  出口 intercept 保留 smoltcp（服务「任意目的地址」全局代理，与承载无关）；
  加密原语：x25519-dalek / hkdf / sha2；配置：serde(toml)。
  OHOS 交叉：`rustup target aarch64-unknown-linux-ohos` + DevEco NDK
  `aarch64-unknown-linux-ohos-clang` 链接器（`.cargo/config.toml` 已备，PoC 实测一次过）。
- 终端栈：**alacritty_terminal 0.26**（仿真 + Damage + kitty 模式位）+
  **自建应答器（~200 行）与键/鼠标/焦点编码器（数百行）**——Rust 生态无服务端编码器库，
  这是已知缺口不是调研项。
- 已实测锚点：**OHOS `.so` = 2,968,016 B**，判据 ≤3,800,000 B ⇒ **0.779×**
  （product 档 = LTO + `codegen-units=1` + NDK strip；同档判、跨档不互引）；
  出口二进制单独量 8,758,816 B（**不设判据**）；smoltcp 栈对栈 22 Gbps（同机）；
  详见 `docs/QUIC-BASELINE.md` §1 与 `docs/E2E-APP-RUST-CORE.md`。

## 速查

| 事 | 去 |
|---|---|
| 干什么 / 干到哪 / 怎么接棒 | `ROADMAP.md`（程序主体）；整改批进度 = `docs/REVIEW-ROADMAP.md` |
| 整改批发现清单 / 用户快速上手 | `docs/reviews/AUDIT-2026-10-07.md` / `README.md` |
| 传输层程序真源（QUIC 换代 M0–M7）/ 三基线 / 真机手册 | `docs/QUIC-ROADMAP.md`、`docs/QUIC-BASELINE.md`、`docs/DEVICE-TEST-OHOS.md` |
| 互操作判据行 | `docs/INTEROP-CRITERIA.md`（含「判据变更记录」——变更须登记） |
| golden 夹具 / 派生向量 | `fixtures/`（`SHA256SUMS` 覆盖全目录 + `MANIFEST.md` 口径） |
| 起本地出口 / 中继 / 矩阵 | `tools/local-rust-exit.sh`、`tools/local-rust-relay.sh`、`tools/matrix.sh`、`tools/ci-local.sh` |
| 生产出口部署与滚动升级实录 | `docs/DEPLOY-RUST-EXIT.md` |
| App 核（`libclientcore.so`）构建与装机 | `tools/build-app-core.sh`（核侧自检门）+ `docs/DEVICE-TEST-OHOS.md` |
| 性能 A/B / 体积内存门槛 | `docs/PERF-AB.md`、`docs/QUIC-BASELINE.md` |
| 版本历史（每版改了什么） | `CHANGELOG.md` |
