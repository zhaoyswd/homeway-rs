#!/bin/zsh
# ci-local.sh — 本地 CI 一键门（R5-5f；设计 §7，评审 G-10 整改）
#
# 编排（步骤 **1–7 + 4.5**，共八步；顺序，前者失败即停）：
#   1. tools/check-baseline.sh            基线门（克隆 HEAD == BASELINE.md 锚定）
#   2. cargo test --workspace             单测+集成（fuzz_replay #[ignore] 跳过——quick 档）
#   3. cargo clippy --all-targets -D warnings + OHOS 交叉 check（r1-F18）+ **OHOS 真链路
#      构建**（M0 设计 §2.4：tools/build-app-core.sh——全仓唯一能发现「OHOS 不可链接」
#      的路径；NDK 缺 ⇒ 显式 SKIP 并打档位，不静默绿）+ **QUIC 岛隔离门**（M0 §3.4 层 3）
#   4. tools/gen-vectors.sh + shasum -c + git diff   向量确定性门（含 SUMS 校验——G-11）
#   4.5 tools/gen-fuzz-seeds.sh + 摘要对账  fuzz 种子展开门（第二道门 中-13 整改）
#   5. tools/check-vocab.sh               词表三方门
#   6. cargo build --release -p homeway-cli           smoke 前置（G-10）
#   7. tools/matrix.sh --smoke            RRR 基础段冒烟
# 全量档：--full 加 cargo test --ignored（fuzz_replay **12** 目标 × 100k；Q-D 批新增
# ⑫⑬⑭ term 面——codec 目标带自产网格往返，全量档预计 **+2-4 min**）。
# **不在本脚本内的手工门（代码门 r21 F12 点名；防「门存在但无人跑」）**：
# `tools/quic-island-e2e.sh` / `quic-wg-e2e.sh` / `quic-ladder-e2e.sh` / **`quic-pf-e2e.sh`（M4 新）**
# ——四条都需**本地私有出口**（起真实例 + 真 token）且耗时较长，故不进 ci-local；改代码后
# 至少手工跑与本批相关的一条（`cargo test --workspace` 里这些用例是 `#[ignore]` = **不计入绿**）。
# 预算（实测口径，2026-10-07 Q-A 修订）：**冒烟档快步 ≈399s ≈ 6.6 分钟**（R5-5f 收口
# 实测值，热 target）；**冷构建首轮显著更长**（依赖全量编译，未见分钟级上界）。M0 起
# 第 3 步多跑一条 OHOS 真链路（NDK 在：+≈1-2 min 增量构建）。另：
# 第 7 步依赖 bin/homeway-go（bin 为 gitignore）——干净 clone 先
# tools/local-exit.sh start 1 触发构建。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
FULL=0
[[ "${1:-}" == "--full" ]] && FULL=1

fail() { echo "!! 步骤 $1 失败：$2" >&2; exit 1; }

echo "==> [1/7] 基线门（check-baseline.sh）"
"$REPO_ROOT/tools/check-baseline.sh" || fail 1 "baseline 克隆漂移"

echo "==> [2/7] 单测 + 集成测试（fuzz_replay 全量档跳过）"
(cd "$REPO_ROOT" && cargo test --workspace) || fail 2 "cargo test"
if (( FULL )); then
  echo "  --full：fuzz_replay 全量档（12 目标 × 100k；term 三目标 +2-4 min）+ 性能 harness（串行——R8-3 r2-5.1："
  echo "        ignored 面含墙钟令牌续水的性能臂，并行跑互相拖拍频会把批形态打歪、硬门随机红）"
  (cd "$REPO_ROOT" && cargo test --workspace --ignored -- --test-threads=1) || fail 2 "fuzz_replay"
fi

echo "==> [3/7] clippy（-D warnings）+ OHOS 交叉面（评审 r1-F18：macOS 编译不到 sendmmsg 支——交叉 check 补覆盖）+ 真 OHOS link + QUIC 岛隔离门"
(cd "$REPO_ROOT" && cargo clippy --workspace --all-targets -- -D warnings) || fail 3 "clippy"

# ---- C 侧前置（M0 设计 §2.5/§2.6）：ring 0.17 的 build script 在 check 与 build 下都编 C。
#   · 真构建/真链接：走 NDK 包装 clang（真 sysroot，**不带** -nostdlibinc）；
#   · NDK 不在：退到 §2.2 的 check-only 配方（clang + -nostdlibinc + 仓内 stdlib 垫片），
#     并在输出里**显式标注档位**（fail-loud，不静默降级）。
OHOS_NDK="${OHOS_NDK:-/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native}"
NDK_CC="${OHOS_NDK}/llvm/bin/aarch64-unknown-linux-ohos-clang"
SHIM="$REPO_ROOT/tools/cc-check-shim"
if [[ -x "$NDK_CC" ]]; then
  export CC_aarch64_unknown_linux_ohos="$NDK_CC"
  OHOS_CC_TIER="真链路（NDK clang 真 sysroot；无 -nostdlibinc）"
else
  export CC_aarch64_unknown_linux_ohos=clang
  export CFLAGS_aarch64_unknown_linux_ohos="-nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem $SHIM"
  OHOS_CC_TIER="check-only 档（clang + -nostdlibinc + 垫片；NDK 缺失）"
  echo "  ⚠️ 档位：$OHOS_CC_TIER"
fi
(cd "$REPO_ROOT" && cargo check -v --target aarch64-unknown-linux-ohos -p homeway-core -p homeway-cli -p homeway-capi -p homeway-quic 2>&1 | tee /tmp/ci-local-ccohos.log | tail -3) || fail 3 "OHOS 交叉 check"
# fail-closed 断言（同 ci.yml）：-nostdlibinc 只许落在 ring 的命令行上
BAD_FLAG="$(grep -- '-nostdlibinc' /tmp/ci-local-ccohos.log | grep -v -- '-ring-' || true)"
[[ -z "$BAD_FLAG" ]] || fail 3 "-nostdlibinc 落到非 ring 的 C 依赖上：$BAD_FLAG"

# ring 的 C 侧产物**正向自校准**（代码门 ③-2：热缓存下 -nostdlibinc 断言会「空过」——
# 没有编译器调用行可查 ⇒ 用「产物在位」补一条正向证据）
RING_A=( "$REPO_ROOT"/target/aarch64-unknown-linux-ohos/debug/build/ring-*/out/libring_core_*.a(N) )
if (( ${#RING_A} > 0 )); then
  echo "  -- ring 的 C 侧产物在位：${RING_A[1]:t}"
else
  echo "  ⚠️ ring 的 C 侧产物不在（热清缓存后首次 check 会重建；本步不判红，靠 check 自身）"
fi

echo "  -- OHOS 真链路构建（tools/build-app-core.sh；档位：$OHOS_CC_TIER）"
if [[ -x "$NDK_CC" ]]; then
  unset CFLAGS_aarch64_unknown_linux_ohos   # 真构建必须真 sysroot（M7：不靠人肉纪律）
  "$REPO_ROOT/tools/build-app-core.sh" > /tmp/ci-local-appcore.log 2>&1 || { tail -20 /tmp/ci-local-appcore.log; fail 3 "OHOS 真链路（build-app-core.sh）"; }
  grep -E '^\[(sym|ver|size)\]' /tmp/ci-local-appcore.log || true
else
  echo "  SKIP：OHOS NDK 不在（$NDK_CC）⇒ 真链路构建未跑（**显式登记，不静默绿**；见 M0 设计 R3.1）"
fi

echo "  -- QUIC 岛隔离源码门（五条断言）"
"$REPO_ROOT/tools/check-quic-isolation.sh" || fail 3 "QUIC 岛隔离门"

echo "==> [4/7] 向量确定性门（gen-vectors + SUMS + git diff）"
"$REPO_ROOT/tools/gen-vectors.sh" >/dev/null || fail 4 "向量生成"
(cd "$REPO_ROOT/fixtures" && shasum -c SHA256SUMS >/dev/null) || fail 4 "SHA256SUMS 校验"
git -C "$REPO_ROOT" diff --quiet -- fixtures/vectors/ || fail 4 "向量 diff 非空（语义漂移）"

echo "==> [4.5/7] fuzz 种子展开门（gen-fuzz-seeds + 摘要对账——第二道门 中-13 整改）"
SEEDS_DIGEST=$("$REPO_ROOT/tools/gen-fuzz-seeds.sh" 2>/dev/null | tail -1) || fail 4 "种子生成"
WANT_DIGEST=$(cat "$REPO_ROOT/fuzz/corpus.seeds.sha256" 2>/dev/null) || fail 4 "摘要基准文件缺失（fuzz/corpus.seeds.sha256）"
[[ "$SEEDS_DIGEST" == "$WANT_DIGEST" ]] || fail 4 "种子展开摘要漂移（得 $SEEDS_DIGEST 想要 $WANT_DIGEST——fixtures 变更须同批更新基准文件）"

echo "==> [5/7] 词表三方门（check-vocab.sh）"
"$REPO_ROOT/tools/check-vocab.sh" || fail 5 "词表漂移"

echo "==> [6/7] release 构建（smoke 前置）"
(cd "$REPO_ROOT" && cargo build --release -p homeway-cli) || fail 6 "release 构建"

echo "==> [7/7] 矩阵冒烟档（RRR 基础段）"
"$REPO_ROOT/tools/matrix.sh" --smoke || fail 7 "矩阵冒烟"

echo ""
echo "本地 CI 全绿（$(date '+%Y-%m-%d %H:%M:%S')；档位：$([[ $FULL -eq 1 ]] && echo full || echo quick)）"
