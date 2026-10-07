#!/bin/zsh
# make-baseline.sh — 重建 baseline/homeway 快照克隆（R0.2；克隆 gitignore 不入库，本脚本可重跑）
#
# 用法：tools/make-baseline.sh [--force] [hash]
#   hash 缺省 = docs/BASELINE.md 锚定的 d4148f6 —— **基线已冻结（2026-10-07）：Go 仓退役，
#   快照不再前移**，此缺省值即长期值；仅在「为复现历史判据而重建快照」等特殊情况下显式传 hash
#   （并同步 docs/BASELINE.md 说明）。
#
# 步骤：本地 clone（**不用 --shared**：alternates 借 dev 仓对象库，发版会话一旦 repack/gc
#   快照即损坏——本地 clone 默认硬链接对象，自足且省盘）→ **移除 origin 远程**（push URL
#   指向 dev 仓，留着就是误 push 事故面）→ checkout 基线 hash → 补 vt 静态库
# （克隆不含未跟踪文件 ⇒ prebuilt/ 空；来源优先级 = homeway dev 仓 → tier submodule 检出
#   → 都没有则在克隆里跑 tools/build-vt.sh darwin-arm64，需 zig 0.16.0 在 ~/zig-0.16.0）
set -euo pipefail

REPO_ROOT="${0:h:A:h}"
DEV_HOMEWAY="$HOME/Documents/projects/homeway"
TIER_VT="$HOME/Documents/projects/tier/third_party/homeway/third_party/libghostty-vt/prebuilt/darwin-arm64"
ARG1="${1:-}"
if [[ "$ARG1" == "--force" ]]; then HASH="${2:-d4148f658513c10e8cb7f67a1096b0c080f5f79c}"; else HASH="${ARG1:-d4148f658513c10e8cb7f67a1096b0c080f5f79c}"; fi
CLONE="$REPO_ROOT/baseline/homeway"

if [[ -d "$CLONE" && "$ARG1" != "--force" ]]; then
  echo "已存在 $CLONE（重建先 rm -rf 或传 --force 作首参）" >&2
  exit 0
fi
[[ "$ARG1" == "--force" ]] && rm -rf "$CLONE"

echo "==> 本地克隆（自足对象库）$DEV_HOMEWAY -> $CLONE"
mkdir -p "$REPO_ROOT/baseline"
git clone "$DEV_HOMEWAY" "$CLONE"
git -C "$CLONE" remote remove origin   # 隔离条款：快照只读，杜绝任何 push 面
git -C "$CLONE" checkout "$HASH"
echo "==> 已钉基线：$(git -C "$CLONE" rev-parse HEAD)"

VT_DST="$CLONE/third_party/libghostty-vt/prebuilt"
if [[ -f "$VT_DST/darwin-arm64/lib/libghostty-vt.a" ]]; then
  echo "==> vt 静态库已在，跳过"
elif [[ -d "$DEV_HOMEWAY/third_party/libghostty-vt/prebuilt/darwin-arm64" ]]; then
  echo "==> 拷 vt：dev 仓 prebuilt/darwin-arm64"
  mkdir -p "$VT_DST"
  cp -R "$DEV_HOMEWAY/third_party/libghostty-vt/prebuilt/darwin-arm64" "$VT_DST/"
elif [[ -d "$TIER_VT" ]]; then
  echo "==> 拷 vt：tier submodule 检出 prebuilt/darwin-arm64"
  mkdir -p "$VT_DST"
  cp -R "$TIER_VT" "$VT_DST/"
else
  echo "==> 两处 prebuilt 都不在，克隆内构建 vt（需 zig 0.16.0）"
  (cd "$CLONE" && ./tools/build-vt.sh darwin-arm64)
fi
echo "==> 完成。构建验证：cd $CLONE && GOTOOLCHAIN=go1.24.5 go build -o ../../bin/homeway-go ./cmd/homeway"
