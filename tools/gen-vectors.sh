#!/bin/zsh
# gen-vectors.sh — 从 baseline 克隆生成对照向量（R0.4；可重跑，产物字节确定）
#
# 做法：基线门（克隆 HEAD == BASELINE.md 锚定）→ 把 tools/vector-gen/vecgen_vectors_test.go
# 拷进克隆的 clientcore/internal/wtransport/（测试文件形态 ⇒ 直调包内未导出派生函数，
# 向量走生产真源），`go test -run` 触发，HOMEWAY_VECGEN_OUT 指 fixtures/vectors/；
# 跑完删拷贝（克隆不留痕、绝不 commit）→ 确定性 diff 门（重跑后 git diff 应为空）。
set -euo pipefail

REPO_ROOT="${0:h:A:h}"
CLONE="$REPO_ROOT/baseline/homeway"
TPL="$REPO_ROOT/tools/vector-gen/vecgen_vectors_test.go"
TARGET="$CLONE/clientcore/internal/wtransport/vecgen_vectors_test.go"
OUT="$REPO_ROOT/fixtures/vectors"

"$REPO_ROOT/tools/check-baseline.sh"

cp "$TPL" "$TARGET"
cleanup() { rm -f "$TARGET"; }
trap cleanup EXIT

cd "$CLONE"
echo "==> 克隆内运行向量生成（GOTOOLCHAIN=go1.24.5 go test -run TestVecgenVectors）"
# 输出落盘再筛（管道里 head/grep 提前关断会撞 SIGPIPE 假失败，L7）
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 \
  go test ./clientcore/internal/wtransport/ -run 'TestVecgenVectors' -count=1 -v > /tmp/vecgen-test.log 2>&1
grep -E '^(=== RUN|--- |ok|FAIL|PASS)' /tmp/vecgen-test.log | head -5

cd "$REPO_ROOT"
echo "==> 产物："; find "$OUT" -type f -name '*.json' -exec stat -f '    %N (%z 字节)' {} \;
# 确定性 diff 门（M8）：重跑对已入库向量 diff 应为空——非空即语义漂移，必须复核
if ! git diff --quiet -- fixtures/vectors/; then
  echo "!! fixtures/vectors/ 与入库版本有 diff——要么生成器改了语义，要么基线升级引入漂移；" >&2
  echo "   核对后 git add 固化，并在 docs/BASELINE.md 记录原因。" >&2
  git --no-pager diff --stat -- fixtures/vectors/ >&2
  exit 1
fi
echo "==> 确定性门通过（fixtures/vectors/ 与入库版本字节一致）"
