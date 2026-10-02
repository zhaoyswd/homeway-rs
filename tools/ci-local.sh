#!/bin/zsh
# ci-local.sh — 本地 CI 一键门（R5-5f；设计 §7，评审 G-10 整改）
#
# 编排（顺序，前者失败即停）：
#   1. tools/check-baseline.sh            基线门（克隆 HEAD == BASELINE.md 锚定）
#   2. cargo test --workspace             单测+集成（fuzz_replay #[ignore] 跳过——quick 档）
#   3. cargo clippy --all-targets -D warnings
#   4. tools/gen-vectors.sh + shasum -c + git diff   向量确定性门（含 SUMS 校验——G-11）
#   5. tools/check-vocab.sh               词表三方门
#   6. cargo build --release -p homeway-cli           smoke 前置（G-10）
#   7. tools/matrix.sh --smoke            RRR 基础段冒烟
# 全量档：--full 加 cargo test --ignored（fuzz_replay 9 目标 × 100k）。
# 预算：quick 热 target ≈3–5 分钟（smoke ≈2 分钟）；冷构建首轮显著更长。
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
  echo "  --full：fuzz_replay 全量档（9 目标 × 100k）"
  (cd "$REPO_ROOT" && cargo test --workspace --ignored) || fail 2 "fuzz_replay"
fi

echo "==> [3/7] clippy（-D warnings）"
(cd "$REPO_ROOT" && cargo clippy --workspace --all-targets -- -D warnings) || fail 3 "clippy"

echo "==> [4/7] 向量确定性门（gen-vectors + SUMS + git diff）"
"$REPO_ROOT/tools/gen-vectors.sh" >/dev/null || fail 4 "向量生成"
(cd "$REPO_ROOT/fixtures" && shasum -c SHA256SUMS >/dev/null) || fail 4 "SHA256SUMS 校验"
git -C "$REPO_ROOT" diff --quiet -- fixtures/vectors/ || fail 4 "向量 diff 非空（语义漂移）"

echo "==> [5/7] 词表三方门（check-vocab.sh）"
"$REPO_ROOT/tools/check-vocab.sh" || fail 5 "词表漂移"

echo "==> [6/7] release 构建（smoke 前置）"
(cd "$REPO_ROOT" && cargo build --release -p homeway-cli) || fail 6 "release 构建"

echo "==> [7/7] 矩阵冒烟档（RRR 基础段）"
"$REPO_ROOT/tools/matrix.sh --smoke" || fail 7 "矩阵冒烟"

echo ""
echo "本地 CI 全绿（$(date '+%Y-%m-%d %H:%M:%S')；档位：$(( FULL )) ? full : quick）"
