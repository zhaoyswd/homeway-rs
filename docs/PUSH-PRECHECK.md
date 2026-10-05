# PUSH-PRECHECK —— 推送公开仓前的敏感信息扫描记录

> 2026-10-05，push `github.com/zhaoyswd/homeway-rs` 前执行（A 批：推送 GitHub + CI/发版工作流 + 首轮发版）。
> 本仓此前从未推过远端（AGENTS.md 硬规则 1 的「用户显式点头」已由用户建空仓 + 明确指令推送满足），
> 历史重写安全。本文记录扫描口径、命中与处置，作为公开仓红线的一次性存档。
> **文中真实基建指标一律掩码**（`123.56.x.x`/`114.x.x.x`/`2408:…`）——本文件自身也在推送面上。

## 扫描口径与结果

| # | 面 | 方法 | 结果 | 处置 |
|---|---|---|---|---|
| 1 | 现役出口 token 泄漏 | 全历史（`git rev-list --all` × `git grep`）扫 `hmw1`/`rl1` + ≥30 base64url 字符；另取两台现役出口（Mac `~/bin/homeway serve token`、阿里云 `--state /opt/homeway/data`）的**当值 token 前缀**做定向回扫 | 命中仅 3 文件：`fixtures/vectors/token.json`、`fixtures/control-cp-v1/frames.jsonl`、`crates/homeway-core/tests/fuzz_replay.rs`。逐条核验均为**合成数据**（peerId=`0x11`×32/`0x22`×32、secret=`0102…`/`deadbeef…`、`hmw1FIXTURETOKEN000…`、`hmw1AAAA…`；向量 JSON 里 input/decoded 明文并列可证）。现役 token 前缀（两台各 12 字符，本文掩码）零命中 | 无需处置 |
| 2 | 真实公网 IP | 全历史扫阿里云出口 `123.56.x.x`、Mac 公网 `114.x.x.x`、Mac v6 STUN 公布前缀 `2408:…`，及各自的 base64 三相位/hex 编码形态 | `114.x.x.x`、`2408:…` 零命中。阿里云中继端点 `123.56.x.x:41741` 以 raw/hex/base64 三种编码贯穿历史于 4 文件（`crates/homeway-core/src/token.rs`、`tools/vector-gen/vecgen_vectors_test.go`、`fixtures/vectors/token.json`、历史中的 `vecgen_main.go`）——token 测试向量把它当中继端点样本 | **已打码 + 历史重写**（见下节） |
| 3 | 域名/主机名 | 全历史扫 `catzhao`、`home.catzhao` | 零命中 | — |
| 4 | 私钥材料 | 全历史扫 `BEGIN … PRIVATE KEY`；`identity/`（含 master.key/key.bin）确认未跟踪 | 零命中；`identity/` 在 .gitignore（`/identity`）且 `git ls-files` 为空 | — |
| 5 | 长 base64 疑似凭证 | 全历史扫 ≥80 连续 `[A-Za-z0-9+/]`（排除 Cargo.lock） | 除上述向量文件外仅 2 处假阳性（斜杠分隔词表：`files_op.rs` 头注释、`docs/reviews/R6-gate1.md` 键名枚举） | 无需处置 |
| 6 | 大文件 | `git rev-list --objects --all` + `cat-file --batch-check` 找 >1MB blob | **零命中**（>500KB 也为零） | — |
| 7 | 推送内容 | baseline/、target/、bin/、identity/、fuzz 运行期产物 | 均在 .gitignore 且未跟踪；`git status` 干净 | — |

私有网段（`192.168.x`/`127.x`/`[::1]`）按口径排除（测试向量合法素材）。

## IP 打码处置（真实 IP → 198.51.100.212）

判定：端点地址在 hmw1 设计上**非凭证**（明文内嵌于每一枚签出的 token，安全模型即假定端点公开），
但公开仓披露真实基建拓扑（阿里云出口/中继所在 IP）没有必要暴露面，且等长替换（14 字符 →
TEST-NET-2 文档保留段 `198.51.100.212`，RFC 5737）可做到零结构扰动，故处置：

1. **HEAD 内容修正**：`token.rs`（2 处）、`vecgen_vectors_test.go`（1 处）字面量替换；
   `tools/gen-vectors.sh` 从基线克隆再生成 `fixtures/vectors/token.json`（raw/b64/hex/crc 四种编码
   一致再生，diff 仅 12 行——即 three-mixed-with-domain 向量）；`fixtures/SHA256SUMS` 与
   `fuzz/corpus.seeds.sha256` 同批更新。`cargo test --workspace` 全绿后提交。
2. **全历史重写**（`git filter-repo --replace-text`，等长字面量替换；旧值在本文掩码，替换规则
   三条对应 raw/ASCII-hex/base64-相位0 三种编码，相位 1/2 经扫描全历史不存在）：
   - 重写后复扫：三种形态在 `git rev-list --all` 全历史零命中（含 blob 与全部提交信息）。

已知副作用（接受）：
- 全部 commit hash 变更（历史内容变了）。ROADMAP/docs 行文里引用的旧短哈希是**重写前的
  历史记录**，不再对应新历史——按时间点理解即可，不追改。
- 历史版本 `token.json` 的 `crc_hex`/`token` 尾段与替换后的 body 不再自洽（重写只做等长串替换，
  不逐 commit 重算 CRC）。死历史不进 CI、无人解码，接受；打码 commit 起的版本由生成器再生、完全自洽。
- ⚠️ 经验登记：`filter-repo --replace-text` 只重写 **blob**，**不重写提交信息**——打码 commit 的
  提交信息若写明旧值会漏网（本批实测踩中，已 amend 处置）；后续同类操作提交信息也用掩码。

## 结论

扫描七面全过，唯一实质命中（真实公网 IP）已内容修正 + 历史重写双处置，可以推送。
