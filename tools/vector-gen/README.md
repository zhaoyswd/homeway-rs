# vector-gen — 对照向量生成（R0.4）

Rust ↔ Go 互证的字节级向量。**语义真源是 baseline 克隆里的生产代码**，不是本目录的旁路重写：
生成器以测试文件形态拷进克隆的 `clientcore/internal/wtransport/`，直调包内未导出的
`deriveKey/deriveDevTag`（identity 族）并交叉验证 `LoadOrCreateIdentity` 全路径；
token/隧道地址族经公开包 `pkg/proto`（同模块内直接 import）。

## 文件

| 文件 | 说明 |
|---|---|
| `vecgen_vectors_test.go` | 生成器模板（拷进克隆后为 `package wtransport` 的测试；未设 `HOMEWAY_VECGEN_OUT` 时 skip，残留零影响） |
| `../gen-vectors.sh` | 编排：拷入 → `go test -run TestVecgenVectors` → 删拷贝（克隆不留痕） |

## 产物（fixtures/vectors/，确定性——无时间戳，重跑 diff 应为空）

| 文件 | 族 | 覆盖 |
|---|---|---|
| ~~`token.json`~~ | **已退役（M5 S5t）** | Go 冻结向量是 **hmw1 布局**的 oracle；M5 换 `hmw2` 段容器（无兼容包袱）⇒ 本管线**不再产出**。新向量 = **本仓自产** `fixtures/vectors/token_hmw2.json`（生成器 = `crates/homeway-core/tests/token_vectors.rs::bless_token_hmw2_vectors`）；登记见 `docs/INTEROP-CRITERIA.md` 判据变更记录 |
| `tunnel_addr.json` | 隧道地址 | DeriveTunnelIP（hw-tun）/ DeriveTunIP（hw-app）＋**守卫命中样本**（搜索 ~65k 次 HMAC 得到撞 v 输入，钉死 hw-app.N 再散列路径） |
| `identity.json` | 设备身份 | master+peerID → WG 私钥（HKDF，未钳位）/公钥（ScalarBaseMult，钳位在乘内部）；master → devTag 8B；4 组隔离性样本（同同/同换/换同/烟囱真钥）＋ store 全路径交叉验证 |

## 重跑

```bash
tools/gen-vectors.sh        # 内含：基线门（克隆 HEAD == BASELINE.md）+ 确定性 diff 门
```

脚本自身已断言「重跑后 `git diff --quiet -- fixtures/vectors/` 为空」（在仓根执行）；
手工核对用 `git diff --exit-code -- fixtures/vectors/`。

升级基线后必须重跑——**diff 非空即语义漂移**（Go 侧改了派生/编码），Rust 侧与
INTEROP-CRITERIA 都要跟着复核，核对后 git add 固化并在 BASELINE.md 记录原因。
