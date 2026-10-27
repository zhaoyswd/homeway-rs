# fixtures — golden 夹具与对照向量清单（R0.4）

> **来源纪律**：历史 golden 拷自 baseline 克隆（基线 `d4148f6`，见 `docs/BASELINE.md`；
> **基线已冻结不再前移**）。当前协议另建独立定值夹具，不能靠解码旧 wire 再升级来保持测试。
> 重拷/重生成导致 diff 非空时，必须复核 Rust 侧与判据文档。

## vectors/（生成物，确定性——重跑 diff 应为空）

| 文件 | 族 | 生成方式 |
|---|---|---|
| ~~`vectors/token.json`~~ | **已退役（M5 S5t）**——Go 冻结向量是 **hmw1 布局**的 oracle；M5 换 `hmw2` 段容器（无兼容包袱）⇒ 不再是对齐面，登记为历史参照 | 原：`tools/gen-vectors.sh`（脚本已同批停产出该文件） |
| `vectors/token_hmw2.json` | **hmw2 段容器** token 编解码 + 三类错误（**本仓自产**；锚 = 入库文件字节） | `cargo test -p homeway-core --test token_vectors bless_token_hmw2_vectors -- --ignored`（生成器 = `crates/homeway-core/tests/token_vectors.rs`） |
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

`SHA256SUMS` **覆盖本目录全部文件（含本清单 `MANIFEST.md`；`SHA256SUMS` 自身除外）**；
`shasum -c SHA256SUMS`（在 `fixtures/` 下执行）应全 OK——重拷/重生成后必须重跑并重写
（R5 本地 CI 第 4 步挂门）。生成口径 = 遍历 `fixtures/` 下全部文件、排除 `SHA256SUMS` 自身，
行格式 `<sha256>  ./<相对 fixtures 的路径>`（与 `shasum -c` 兼容）。

**2026-10-07 Q-A 批修正**：原「45 项」为 R0.4 时点计数，此后 R2/R4/R5/R6/R7 陆续新增向量
与目录，入册条目实际长到 52 行但一直未补齐（8 个文件缺册：本清单、`vectors/surface_codec.json`、
`vectors/term_manifest_eval.json`、`vectors/term_mouseenc.json`、`vectors/term_keyenc.json`、
`vectors/term_responder.json`、`vectors/term_palette.json`、`vectors/tun_status.jsonl`〔且该行
缺 `./` 前缀〕）。本次按现行全量重生成：**45（文档口径）/52（实际行数）→ 59 项**，覆盖口径
自此字面成真。

## term golden（拷贝，来源 hash 锚定）

| 本仓路径 | 来源（baseline 克隆内） | 用途（R6） | sha256 |
|---|---|---|---|
| `term/frames.v1.jsonl` | 历史帧案例；2026-10-21 HELLO 完整尾；2026-10-27 GREETING/HELLO 新增 observer 能力 | term 协议帧解析 golden（14 案：greeting/hello/…；不再接受无版本/无ID HELLO） | `0da6ab734cbb920faab8ec0eaeaf84aa958392b7053ff67555fd53c509f4f038` |
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

## 中继客户端准入扩展（2026-10-21）

`relay-admission.json`：独立 Python stdlib HMAC-SHA256 编码的定值参照，覆盖
`CLADM`（45B，sid/cookie/域分隔MAC）及 `UDP_SESSION`（43B，会话字段/MAC）。
不改冻结的 `rl1`/旧中继消息向量；真实客户端准入提升和UDP控制降级另由生产组件E2E验证。

## 终端 surface v5 扩展（2026-10-21）

`vectors/surface_v5.json`：固定十六进制向量，覆盖 SNAPSHOT、DIFF 与 FETCH 请求；
公共头明确携带逐帧 seq、historyEpoch 与 historyBase，cell v2 明确携带软换行和 0/1/2 列宽。
样例以 seq 17→18、epoch 4、base 100、2 列中日韩宽字符与尾格验证跨端字节口径。
冻结的 `surface_codec.json`、`surface-golden/` 和 baseline 均不改，作为历史归档，不再由
当前终端 codec/VT 测试解码。新 v5 向量钉住身份、序号、wraps 与真实宽度。

## 当前单版本终端定值夹具（2026-10-21）

`surface-current/` 是独立当前夹具，不由 `tools/gen-vectors.sh` 的冻结 Go 基线生成，
当前生产/CI 中不保留旧 v4→v5 或 cell1→cell2 适配器：

| 文件 | 当前用途与来源 |
|---|---|
| `surface-current/codec.json` | 保留有价值的 cell/RLE/颜色/属性/体字段/分片案例语义，字节固定为 v5/cell2；负例直接针对当前头/尺寸/预算/游程构造，不转换旧字节 |
| `surface-current/frames.json` | 四段 ANSI 会话（CJK/git-log/hexdump/styles）由当前 VT 与编码器一次性采集，固定快照及快照+差分的 gzip/分片字节；当前测试直接消费 |
| `surface-current/manifest.tsv` | 上述八种当前最终画面的文本/样式/真实宽度/光标/回滚/模式摘要；宽头保留2，非skip样式空白保持1，不再迁就旧摘要 |
| `surface-current/input.tsv` | 当前 THEME/CLIPBOARD 四项定值；未消费的旧 HELLO/caps-only 行不进入当前测试 |

上述夹具在采集后固定入库并计入 SHA256SUMS；编码变化需显式审核/更新，不能让测试边读
旧 wire 边转换。标准 ANSI/DEC/kitty 的 VT 行为测试、键鼠编码、真实 ANSI 回放继续保留，
它们是终端标准语义，不是旧 App/出口兼容。跨端独立 v5 向量仍由 Rust 与 tier 共同消费。
当前cell2严格要求 `skip iff width==0`，非skip仅1或2；组合字符在VT内并入基格，不另造零宽格。
