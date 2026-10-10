# BASELINE — 基线锚定（R0.1）

> **基线冻结声明（2026-10-07）**：**Go 仓已退役（2026-10-05，tier 侧 C 批转正），本快照
> 锚定 `d4148f6` 且不再前移**——它自此只是**只读历史参照 / oracle**（对照向量与判据行的
> 出处锚），不是「待升级的 dev 仓镜像」。本文档下方「升级基线流程」一节降级为**历史说明**
> （记录当初怎么升级、将来若有需要怎么重做），**不再有例行前移动作**；`tools/make-baseline.sh`
> 的缺省 hash 已与本文件锚定值对齐（`d4148f6`）。
> 对齐标尺的现行真源 = 本文件锚定值 + `fixtures/`（golden/向量）+ `docs/INTEROP-CRITERIA.md`
> （判据行，含「判据变更记录」）。

> （历史口径，R0.1 时点）对齐标尺的单一真源。升级基线（dev 仓前移后 rebase 快照克隆）时
> **必须**更新本文件，并在 roadmap 提交信息里注明。升级前此节描述的流程已执行两次
> （`621fe0e` → `d4148f6`），见下方「R7 重锚记录」。

## 换代后对齐义务范围（2026-10-10，M7 文档收束）

**Go 基线 = 只读历史参照 / oracle**；**token 与 WG 承载面**的对齐义务随 QUIC 换代（M5）终止——
`fixtures/vectors/token.json`（Go 冻结向量）已退役，代之以本仓自产 `token_hmw2.json`；
仍在对齐面内的 = term / files / 控制面 / relaywire 等**保留面** + 判据行政策（登记制，
见 `docs/INTEROP-CRITERIA.md`「判据变更记录」）。

## 锚定值

| 项 | 值 | 采集方式 / 备注 |
|---|---|---|
| **锚定日期** | 2026-10-04 | R7 第 1 棒基线重锚（前值 2026-10-02 = `621fe0e`） |
| **homeway dev HEAD** | `d4148f658513c10e8cb7f67a1096b0c080f5f79c`（`d4148f6`） | `git -C ~/Documents/projects/homeway rev-parse HEAD`，R7 前移（`621fe0e..d4148f6` 共 8 commit：4 个测试 flake 修复 + 4 个 surface 平移型回落修复〔服务端 noteScrollbar 判据 + 消费端 surface_scroll 缓存〕） |
| 同 commit | `surface 回退「近似平移」放宽（内容正确性优先）…`（2026-10-04） | dev 仓工作区 clean（`git status --porcelain` 空） |
| **tier submodule pin**（`third_party/homeway`） | `d4148f658513c10e8cb7f67a1096b0c080f5f79c`（`v0.16.0-4-gd4148f6`） | `git -C tier submodule status`，与 dev HEAD 同 hash ⇒ 两值合一 |
| **Go 版本** | go.mod：`go 1.24.0` + `toolchain go1.24.5`；本机构建一律 `GOTOOLCHAIN=go1.24.5`（离线 toolchain 已在 `~/go/pkg/mod/golang.org/toolchain@v0.0.1-go1.24.5.darwin-arm64`） | `/usr/local/go` 基底 1.21.6，不带 GOTOOLCHAIN 必失败 |
| **契约台账** | `contracts/ledger.jsonl` **422 行 = 422 单元**（每行一个词表单元，字段 family/unit/value/faces/status/spec） | ROADMAP 附录 A 写 349 为立项盘点时旧值，**以本表 422 为准**；spec 字段为台账内逻辑分组名（如 `daemon-control-plane`），不是文件路径 |
| **term golden** | `pkg/term/testdata/frames.v1.jsonl`（唯一 testdata 文件） | 拷贝入 `fixtures/` 时记来源 hash |
| **关键输入 sha256** | `contracts/ledger.jsonl` = `0ae1d104ec1eeb546d61d7352052fccb062d654232e17f0b03adbe4323c2469f`（**仍有效**——词表门 `tools/check-vocab.sh` 引用）；`third_party/libghostty-vt/prebuilt/darwin-arm64/lib/libghostty-vt.a` = `7202ac3bf6bff5259493fffd54e964c9e27feedf4d06d445f477ef7c3517ad21` | 升级基线时核对（vt 库换了 = 需重验构建） |
| **已退役的输入** | `fixtures/vectors/token.json`（Go 冻结 token 向量）= **已退役（M5 S5t）**——`hmw1` 载体换代后不再有产出面，代之以本仓自产 `fixtures/vectors/token_hmw2.json` | 登记见 `docs/INTEROP-CRITERIA.md` L-12 |
| **vt 静态库来源** | dev 仓 `third_party/libghostty-vt/prebuilt/darwin-arm64/`（2026-10-01 构建，存在）⇒ 直接拷入 baseline 克隆 | tier submodule 检出里同款也在，互为备份 |

## tier 侧在途 openspec change 清单（11 个，2026-10-02）

`ls tier/openspec/changes/ | grep -v archive`：

`app-logic-refactor`、`dns-tunnel-listener`、`files-server-bounds`、`napi-payload-contract`、
`relay-v2-only`、`resync-op-retire`、`state-migration-retire`、`term-ime-context`、
`term-proto-version-gate`、`token-revocation`、`unified-leg-framing`

对 R0/R1 的直接影响：无（词表门在 R5、napi 词表在 R7 才消费 tier 资产）；其中
`files-server-bounds`/`token-revocation`/`term-*`/`relay-v2-only` 若在 R2+ 期间合入并前移基线，
升级基线时需重扫 `docs/INTEROP-CRITERIA.md` 与向量。

## 与 ROADMAP 的已登记漂移（本文件为准）

1. **homeway 仓没有 `openspec/` 目录**——ROADMAP 附录 C「openspec/specs/（45 份，需求真源）」
   不成立（立项时误记，dev 与 tier submodule 检出均无此目录）。homeway 的需求/契约真源 =
   **`contracts/`**（ledger.jsonl 词表 + cleanroom 对照 + Go 测试内联断言）；
   跨仓需求规格实际在 **tier 仓 `openspec/specs/`（37 份）**——R2+ 消费时经 tier 只读路径取。
2. **契约台账 422 单元**（ROADMAP 附录 A 的 349 为旧盘点值）。
3. **（已撤销，v0.2.2 简洁化批 2026-10-07）tunConfig.mtu 上限 clamp**——P2 批
   曾把 cfg.mtu 起为升档门请求值并按 `clamp_inner_mtu` 收口到 [1280,1400]；P2
   终验无增益后升档机器整体删除，cfg.mtu 回到「≤0 → 1280、仅进日志行」的
   Go Normalize 旧口径，偏离不再成立。

## 快照克隆

`baseline/homeway`（gitignore 不入库）：**本地 clone（非 `--shared`）** 自 dev 仓 +
`remote remove origin`（快照只读、无 push 面）+ checkout `d4148f6`。对象库必须**自足**
（`--shared` 的 alternates 借 dev 仓对象，发版会话 repack/gc 会损坏快照——R0.6 评审 M7 整改）。
Go 侧一切构建/测试/向量生成只从它走；`tools/check-baseline.sh` 是基线门（克隆 HEAD ==
本文件锚定 hash + 无远程 + vt 在位），`tools/gen-vectors.sh` 与 R5 CI 都先过它。

## R7 重锚记录（2026-10-04，`621fe0e` → `d4148f6`）

`621fe0e..d4148f6` = **8 commit**，逐项影响评估（对已实现 Rust 面的 drift 核对）：

| commit | 内容 | 影响 |
|---|---|---|
| `d4d5622` / `a66d71b` / `0d261d8` / `fd18625` | CI 发版预跑 flake 修复（wtransport 测试预算/夹具锁、facade 表用例注入缝、carriers_test enterWait/WaitingExpiry） | **纯测试文件 + `internal/server/role.go` tokenFallbackWait 原子化（生产行为不变：仍是 15s 兜底窗）**——Rust 侧无 drift（serve 面同语义）；facade 生产代码零变化 ⇒ 20 导出面语义真源稳定 |
| `d457491` | surface_codec.cpp onFrame→applyFetchRows statsMu 自死锁修复 | 消费端（tier App C++ 宿主 + homeway 仓 surface/ 镜像）——Rust 是 surface **产出端**，wire 协议零变化，无 drift |
| `8167cb7` | **服务端 `pkg/term/term_surface_leg.go`/`term_surface_session.go`：noteScrollbar 平移判据分流 + noteSentScrollbar 基线随成功入队帧推进 + 判据行改文** | **需补（R7 第 1 棒 7a 处置）**：Rust `term/service.rs` surface 投递仍是旧「total 变小 ⇒ 强制全量」判据 + 快照时刻记基线——与新基线语义漂移（详见 7a 影响清单） |
| `463eff6` / `d4148f6` | surface_scroll.cpp 消费端平移保缓存放宽→回退收紧（终态 = 纯平移严格判据） | 消费端逻辑，协议零变化；Rust 产出端无需动，但真机组合行为依赖服务端平移判据先行（`8167cb7`） |

- 关键输入 sha256：`contracts/ledger.jsonl` = `0ae1d104…`（**422 单元不变**，前移未触契约台账）、
  vt 静态库 = `7202ac3b…`（不变）⇒ 词表门与 term golden 锚不漂。
- `clientcore/cmd/clientcore/*.go`（20 个 `//export` 真源）零变化 ⇒ 7c 语义面无重读成本。

## 升级基线流程（dev 仓前移后）——**历史说明（2026-10-07 降级；冻结不再前移）**

> 以下流程为 2026-10-04 前移（`621fe0e` → `d4148f6`）当时的操作记录，保留供将来万一
> 需要重做时参照（例如为复现某条历史判据而重建快照）；**例行前移已随 Go 退役取消**。

1. `git -C ~/Documents/projects/homeway rev-parse HEAD` 记新值 → 更新本文件锚定行与关键输入 sha256；
2. `tools/make-baseline.sh --force <新hash>`（重建自足克隆 + 移除远程 + 拷 vt）；
3. `tools/gen-vectors.sh` —— **diff 门非空即语义漂移**，逐文件复核（token/地址/身份三族），
   确认后 `git add fixtures/vectors` 固化，并在提交信息与本文件记录原因；
   **向量文件增删必须同步 `fixtures/SHA256SUMS`**（`cd fixtures && shasum -a 256 <文件> >>
   SHA256SUMS`，路径统一 `./vectors/<名>`）——ci-local.sh 第 4 步的 `shasum -c` 会对账；
4. 重跑 `tools/local-exit.sh start/client-add` 复核 INTEROP-CRITERIA 判据行未变措辞；
5. 重跑 `cargo test`（Rust 侧对照必须跟着新向量绿）。
（`bin/homeway-go` 会被 local-exit.sh 按 `bin/homeway-go.baseline` 标记自动重建，勿手拷旧件。）
