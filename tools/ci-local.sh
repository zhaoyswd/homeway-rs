#!/bin/zsh
# ci-local.sh — 本地 CI 一键门（R5-5f；设计 §7，评审 G-10 整改）
#
# 编排（顺序，前者失败即停）：
#   1. tools/check-baseline.sh            基线门（克隆 HEAD == BASELINE.md 锚定）
#   2. cargo test --workspace             单测+集成（fuzz_replay #[ignore] 跳过——quick 档）
#   3. cargo clippy --all-targets -D warnings + OHOS 交叉 check（r1-F18）
#   4. tools/gen-vectors.sh + shasum -c + git diff   向量确定性门（含 SUMS 校验——G-11）
#   5. tools/check-vocab.sh               词表三方门
#   6. cargo build --release -p homeway-cli           smoke 前置（G-10）
#   7. tools/matrix.sh --smoke            RRR 基础段冒烟
# 全量档：--full 加 cargo test --ignored（fuzz_replay 9 目标 × 100k）。
# 预算：quick 热 target ≈12–15 分钟（冒烟档实测 399s——R5-5f 收口实测值；冷构建首轮
# 显著更长）。另：第 7 步依赖 bin/homeway-go（gitignore）——干净 clone 先
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
  echo "  --full：fuzz_replay 全量档（9 目标 × 100k）+ 性能 harness（串行——R8-3 r2-5.1："
  echo "        ignored 面含墙钟令牌续水的性能臂，并行跑互相拖拍频会把批形态打歪、硬门随机红）"
  (cd "$REPO_ROOT" && cargo test --workspace --ignored -- --test-threads=1) || fail 2 "fuzz_replay"
fi

echo "==> [3/7] clippy（-D warnings）+ OHOS 交叉面（评审 r1-F18：macOS 编译不到 sendmmsg 支——交叉 check 补覆盖）"
(cd "$REPO_ROOT" && cargo clippy --workspace --all-targets -- -D warnings) || fail 3 "clippy"
(cd "$REPO_ROOT" && cargo check --target aarch64-unknown-linux-ohos -p homeway-core -p homeway-cli -p homeway-capi) || fail 3 "OHOS 交叉 check"

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
