#!/bin/zsh
# gen-vectors.sh — 从 baseline 克隆生成对照向量（R0.4；可重跑，产物字节确定）
#
# 做法：把 tools/vector-gen/vecgen_vectors_test.go 拷进克隆的 clientcore/internal/wtransport/
# （测试文件形态 ⇒ 可直调包内未导出派生函数，向量走生产真源），`go test -run` 触发，
# HOMEWAY_VECGEN_OUT 指 fixtures/vectors/；跑完删拷贝（克隆不留痕、绝不 commit）。
set -euo pipefail

REPO_ROOT="${0:h:A:h}"
CLONE="$REPO_ROOT/baseline/homeway"
TPL="$REPO_ROOT/tools/vector-gen/vecgen_vectors_test.go"
TARGET="$CLONE/clientcore/internal/wtransport/vecgen_vectors_test.go"
OUT="$REPO_ROOT/fixtures/vectors"

[[ -d "$CLONE" ]] || { echo "!! baseline 克隆不在（先 tools/make-baseline.sh）" >&2; exit 1; }

cp "$TPL" "$TARGET"
trap 'rm -f "$TARGET"' EXIT

cd "$CLONE"
echo "==> 克隆内运行向量生成（GOTOOLCHAIN=go1.24.5 go test -run TestVecgenVectors）"
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 go test ./clientcore/internal/wtransport/ -run 'TestVecgenVectors' -count=1 -v 2>&1 | grep -Ev '^=== (RUN|PAUSE|CONT)' | head -30

echo "==> 产物："
ls -la "$OUT"
