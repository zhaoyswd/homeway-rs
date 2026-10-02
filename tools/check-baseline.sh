#!/bin/zsh
# check-baseline.sh — 基线一致性门（M5）：克隆 HEAD 必须与 docs/BASELINE.md 锚定 hash 一致。
# 供 gen-vectors.sh（生成前）与 R5 本地 CI 调用；不一致即非 0 退出。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
BASELINE_MD="$REPO_ROOT/docs/BASELINE.md"
CLONE="$REPO_ROOT/baseline/homeway"

anchored=$(grep -m1 -oE '`([0-9a-f]{40})`' "$BASELINE_MD" | tr -d '\`')
if [[ -z "$anchored" ]]; then
  echo "!! $BASELINE_MD 里找不到 40 位锚定 hash（首个反引号包裹的 40 hex）" >&2; exit 1
fi
if [[ ! -d "$CLONE" ]]; then
  echo "!! baseline 克隆不在（tools/make-baseline.sh）" >&2; exit 1
fi
head=$(git -C "$CLONE" rev-parse HEAD 2>/dev/null) || { echo "!! 克隆损坏（rev-parse 失败）" >&2; exit 1; }
if [[ "$head" != "$anchored" ]]; then
  echo "!! 克隆 HEAD $head ≠ BASELINE.md 锚定 $anchored——先 tools/make-baseline.sh --force $anchored" >&2
  exit 1
fi
if ! git -C "$CLONE" remote get-url origin >/dev/null 2>&1; then
  : # 无远程 = 隔离就位（正常态）
else
  echo "!! 克隆仍带 origin 远程（应为只读快照，make-baseline.sh 会移除）" >&2; exit 1
fi
if [[ -f "$CLONE/third_party/libghostty-vt/prebuilt/darwin-arm64/lib/libghostty-vt.a" ]]; then
  echo "基线门通过：HEAD=$head（锚定一致、无远程、vt 静态库在位）"
else
  echo "!! vt 静态库缺失（构建会失败）——重跑 tools/make-baseline.sh" >&2; exit 1
fi
