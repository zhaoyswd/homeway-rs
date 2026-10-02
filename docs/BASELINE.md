# BASELINE — 基线锚定（R0.1）

> 对齐标尺的单一真源。升级基线（dev 仓前移后 rebase 快照克隆）时**必须**更新本文件，
> 并在 roadmap 提交信息里注明。

## 锚定值

| 项 | 值 | 采集方式 / 备注 |
|---|---|---|
| **锚定日期** | 2026-10-02 | 本文件创建时点 |
| **homeway dev HEAD** | `621fe0e173e13b7a0a58da657860a615e0204664`（`621fe0e`） | `git -C ~/Documents/projects/homeway rev-parse HEAD`，与 ROADMAP 立项值一致，**未前移** |
| 同 commit | `app-logic-refactor §6 exec-r1 低-6：stalled 例外知识入台账 note + 测试断言`（2026-10-02 06:45:33 +0800） | dev 仓工作区 clean（`git status --porcelain` 空） |
| **tier submodule pin**（`third_party/homeway`） | `621fe0e173e13b7a0a58da657860a615e0204664`（`v0.15.0-48-g621fe0e`） | `git -C tier submodule status`，与 dev HEAD 同 hash ⇒ 两值合一 |
| **Go 版本** | go.mod：`go 1.24.0` + `toolchain go1.24.5`；本机构建一律 `GOTOOLCHAIN=go1.24.5`（离线 toolchain 已在 `~/go/pkg/mod/golang.org/toolchain@v0.0.1-go1.24.5.darwin-arm64`） | `/usr/local/go` 基底 1.21.6，不带 GOTOOLCHAIN 必失败 |
| **契约台账** | `contracts/ledger.jsonl` **422 行 = 422 单元**（每行一个词表单元，字段 family/unit/value/faces/status/spec） | ROADMAP 附录 A 写 349 为立项盘点时旧值，**以本表 422 为准**；spec 字段为台账内逻辑分组名（如 `daemon-control-plane`），不是文件路径 |
| **term golden** | `pkg/term/testdata/frames.v1.jsonl`（唯一 testdata 文件） | 拷贝入 `fixtures/` 时记来源 hash |
| **关键输入 sha256** | `contracts/ledger.jsonl` = `0ae1d104ec1eeb546d61d7352052fccb062d654232e17f0b03adbe4323c2469f`；`third_party/libghostty-vt/prebuilt/darwin-arm64/lib/libghostty-vt.a` = `7202ac3bf6bff5259493fffd54e964c9e27feedf4d06d445f477ef7c3517ad21` | 升级基线时核对（vt 库换了 = 需重验构建） |
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

## 快照克隆

`baseline/homeway`（gitignore 不入库）：**本地 clone（非 `--shared`）** 自 dev 仓 +
`remote remove origin`（快照只读、无 push 面）+ checkout `621fe0e`。对象库必须**自足**
（`--shared` 的 alternates 借 dev 仓对象，发版会话 repack/gc 会损坏快照——R0.6 评审 M7 整改）。
Go 侧一切构建/测试/向量生成只从它走；`tools/check-baseline.sh` 是基线门（克隆 HEAD ==
本文件锚定 hash + 无远程 + vt 在位），`tools/gen-vectors.sh` 与 R5 CI 都先过它。

## 升级基线流程（dev 仓前移后）

1. `git -C ~/Documents/projects/homeway rev-parse HEAD` 记新值 → 更新本文件锚定行与关键输入 sha256；
2. `tools/make-baseline.sh --force <新hash>`（重建自足克隆 + 移除远程 + 拷 vt）；
3. `tools/gen-vectors.sh` —— **diff 门非空即语义漂移**，逐文件复核（token/地址/身份三族），
   确认后 `git add fixtures/vectors` 固化，并在提交信息与本文件记录原因；
4. 重跑 `tools/local-exit.sh start/client-add` 复核 INTEROP-CRITERIA 判据行未变措辞；
5. 重跑 `cargo test`（Rust 侧对照必须跟着新向量绿）。
（`bin/homeway-go` 会被 local-exit.sh 按 `bin/homeway-go.baseline` 标记自动重建，勿手拷旧件。）
