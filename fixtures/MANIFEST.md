# fixtures — golden 夹具与对照向量清单（R0.4）

> **来源纪律**：一切 golden 拷自 baseline 克隆（基线 `621fe0e`，见 `docs/BASELINE.md`）。
> 升级基线后重拷 + 重跑 `tools/gen-vectors.sh`，diff 非空即语义漂移，必须复核 Rust 侧与判据文档。

## vectors/（生成物，确定性——重跑 diff 应为空）

| 文件 | 族 | 生成方式 |
|---|---|---|
| `vectors/token.json` | hmw1 token 编解码 + 三类错误（corrupted/unsupported_version/malformed） | `tools/gen-vectors.sh`（模板 `tools/vector-gen/`，克隆内 `pkg/proto` 真源产出） |
| `vectors/tunnel_addr.json` | DeriveTunnelIP / DeriveTunIP（含守卫命中样本钉死 hw-app.N 再散列路径） | 同上 |
| `vectors/identity.json` | master+peerID → WG 私钥/公钥；master → devTag（含 LoadOrCreateIdentity 全路径交叉验证） | 同上（克隆内直调 `clientcore/internal/wtransport` 未导出派生函数） |
| `vectors/psk.json` | token secret → WG PSK（HKDF 域分离 `homeway/wg-psk`；R1 技术评审 S1 补——握手热路径派生错 = AEAD tag 失败难归因） | 同上（克隆内 `pkg/proto.DerivePSK` 真源产出） |

## control 控制面契约夹具（拷贝，R0.6 评审 M9 补拷）

Go 侧**明确冻结的跨语言线协议夹具**（`internal/control/testdata/fixtures/v1/`）——43 条
daemon 控制面帧向量 + `decode_check.py`（**语言无关**的解码对拍器：不依赖任何 Go 代码、
仅凭 spec 实现解码即可自证）。Rust 侧落 daemon 面时**必须跑 `python3 decode_check.py` 自证**。

| 本仓路径 | 来源（baseline 克隆内） | 说明 |
|---|---|---|
| `control-cp-v1/frames.jsonl` | `internal/control/testdata/fixtures/v1/frames.jsonl` | 43 条帧向量（含 expect 语义断言） |
| `control-cp-v1/decode_check.py` | 同目录 | 跨语言解码对拍器（独立实现参照） |
| `control-cp-v1/README.md` | 同目录 | 冻结契约说明 |

## term 检测规则本体（拷贝，R0.6 评审 M10 补拷）

`pkg/term/manifest/manifests/` 整目录（24 文件 = **22 份 agent 规则 toml** + `index.toml` +
`README.md`）——E15 判据「检测规则已加载 22 份」的**计数与内容真源**（Go 侧 `go:embed`
内嵌随二进制分发）；R6 检测引擎必须吃同一份规则数据。`index.toml` 的 id 集合 = 22 份计数真源。

## 机器校验

`SHA256SUMS` 覆盖本目录全部文件（45 项，向量 JSON 亦入册）；`shasum -c SHA256SUMS`（在
`fixtures/` 下执行）应全 OK——升级基线重拷/重生成后必须重跑并重写（R5 本地 CI 挂门）。

## term golden（拷贝，来源 hash 锚定）

| 本仓路径 | 来源（baseline 克隆内） | 用途（R6） | sha256 |
|---|---|---|---|
| `term/frames.v1.jsonl` | `pkg/term/testdata/frames.v1.jsonl` | term 协议帧解析 golden（13 案：greeting/hello/…） | `558cc64a0fdc23dde0f8b7c01a306e904e3631e96d8e4cb7691cd1bc947ba662` |
| `term-vt/session-cjk.bin` | `pkg/term/vt/testdata/session-cjk.bin` | vt 会话回放（CJK） | `5a60ceb2cfaafc566efd595f2835aa772159503364c692e3e179bb9a7e3d6ff8` |
| `term-vt/session-git-log.bin` | `pkg/term/vt/testdata/session-git-log.bin` | vt 会话回放（git log） | `688c69bafbbfb4eba14f46da4ef8c22fbc93f77318e95b4ce5449723c32a59b3` |
| `term-vt/session-hexdump.bin` | `pkg/term/vt/testdata/session-hexdump.bin` | vt 会话回放（hexdump） | `c60ffb2e49a6cd09ca565e0de14817ad963801975fb9ca603c51b640fb12d07e` |
| `term-manifest/codex-startup.txt` | `pkg/term/manifest/testdata/codex-startup.txt` | agent 检测规则证据样本 | `0ca1ff83a06dac22167f05f3ee43443d2c4c3ac2f5fe8ecb071fe5c52d4a31f0` |
| `term-manifest/opencode-startup.txt` | `pkg/term/manifest/testdata/opencode-startup.txt` | 同上 | `7199a7e94161b4f0e5aa4e1469b29605637778617c85a468947c8b1c15fc61bf` |

## surface golden（拷贝，R6 surface v4 产出端钉字节）

`surface/test/golden/`（C++ 测试的静态 golden：快照 + 差分 + 样式向量）与
`surface/test/host/surface_input_cases.tsv`（上行输入用例表）整体拷入 `surface-golden/`：

| 本仓路径 | 来源（baseline 克隆内） | sha256 |
|---|---|---|
| `surface-golden/manifest.tsv` | `surface/test/golden/manifest.tsv` | `6ec8e50a7926bec06b28033e4385b192d31f4f954af47e9d6a827c97b87278fc` |
| `surface-golden/session-cjk.bin` / `-diff.bin` | 同名 | `e6af9617a25ca866f71ea5602752f23a59cd1f749b26438b51a61938a5f43bda` / `4279fc1c9c94577963b1e8648bf84dccb67731b24b1cae9b912b99489c48fd46` |
| `surface-golden/session-git-log.bin` / `-diff.bin` | 同名 | `ed2dc7a2cc53cf18dca19c07abaa3e89ba17e7bbc9753fb1ecb5527a186c04b4` / `9b2b96619a8ebc96a9b4e2c5624c06036a73a75b9db528e75108e6f5c957cafd` |
| `surface-golden/session-hexdump.bin` / `-diff.bin` | 同名 | `ddc4c098dacd159273cdcdac86bc40c0a75bd9fefb1000a5325dee4cdd2c6745` / `2685218575526e0d838bb8f007be004b9d064a4eef21fbd6540bbc3f282a688c` |
| `surface-golden/session-styles.bin` / `-diff.bin` | 同名（**样式向量**） | `e2378260132a886f510aca9d6902246775edf2ba5e351294d60cbbb727c029ad` / `3a1b9938073bab46044e0299757413a54d882416d2f59753fb4b9d67fc334510` |
| `surface-golden/surface_input_cases.tsv` | `surface/test/host/surface_input_cases.tsv`（**上行字节表**用例） | `4a50078f74dd98fb159c6ac537b3254c7b3ee927c6e7010e4593a79c8f964f3b` |

**说明**：`pkg/proto/gen_golden.go` 是 token 向量的一次性生成器（输出钉进 token_test.go），
与本清单 vectors/ 的持续生成器是两回事。files 帧无静态 testdata（Go 侧 files 契约在测试
内联断言）；R2 files 客户端对齐时用 `fixtures/vectors` 的 token 族 + 克隆内 Go 测试同款字节构造。
**有意不拷**：`third_party/libghostty-vt/**/snapshot/testdata`（20 个 `.hex`）——那是
libghostty-vt 自身的快照格式测试数据，Rust 侧走 alacritty_terminal、不实现 ghostty 快照
格式，拷来无用（R0.6 评审注记，免得 R6 反复讨论）；`facade/golden_test.go` 是行为脚本
测试非字节 golden，同样无拷贝物。

**采集日期**：2026-10-02（R0.4）。
