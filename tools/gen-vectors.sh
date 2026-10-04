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
# R5-M20：STUN/SPED golden 第二落点（pkg/egress——STUN 本包真源 + import pkg/speedtest）
TPL2="$REPO_ROOT/tools/vector-gen/stun_sped/vecgen_stun_sped_test.go"
TARGET2="$CLONE/pkg/egress/vecgen_stun_sped_test.go"
# R6：term 编码/应答向量（pkg/term/vt——libghostty-vt 默认应答与键/鼠编码真源；
# aux 是伴随的普通包文件，因 Go 不支持 _test.go 里用 cgo 而存在，跑完一并删除）
TPL3="$REPO_ROOT/tools/vector-gen/term/vecgen_term_test.go"
TARGET3="$CLONE/pkg/term/vt/vecgen_term_test.go"
TPL3A="$REPO_ROOT/tools/vector-gen/term/vecgen_term_aux.go"
TARGET3A="$CLONE/pkg/term/vt/vecgen_term_aux.go"
# R6 6e：surface 体编码向量（pkg/term——encSnapshotBody 等未导出面在包内直调）
TPL4="$REPO_ROOT/tools/vector-gen/term/vecgen_surface_test.go"
TARGET4="$CLONE/pkg/term/vecgen_surface_test.go"
# R6 6f：manifest 检测引擎求值向量（pkg/term/manifest——region 逐函数 + startup 夹具全轨迹）
TPL5="$REPO_ROOT/tools/vector-gen/term/vecgen_manifest_test.go"
TARGET5="$CLONE/pkg/term/manifest/vecgen_manifest_test.go"
# R7 7c：tunStatusJSON 阶段机对照向量（clientcore/cmd/clientcore——cshared 面，
# 无 runner 期的全部可达形态；runner 键面由 Rust 侧键集合守卫钉）
TPL6="$REPO_ROOT/tools/vector-gen/tunstatus/vecgen_tunstatus_test.go"
TARGET6="$CLONE/clientcore/cmd/clientcore/vecgen_tunstatus_test.go"
OUT="$REPO_ROOT/fixtures/vectors"

"$REPO_ROOT/tools/check-baseline.sh"

cp "$TPL" "$TARGET"
cp "$TPL2" "$TARGET2"
cp "$TPL3" "$TARGET3"
cp "$TPL3A" "$TARGET3A"
cp "$TPL4" "$TARGET4"
cp "$TPL5" "$TARGET5"
cp "$TPL6" "$TARGET6"
cleanup() { rm -f "$TARGET" "$TARGET2" "$TARGET3" "$TARGET3A" "$TARGET4" "$TARGET5" "$TARGET6"; }
trap cleanup EXIT

cd "$CLONE"
echo "==> 克隆内运行向量生成（GOTOOLCHAIN=go1.24.5 go test -run TestVecgenVectors）"
# 输出落盘再筛（管道里 head/grep 提前关断会撞 SIGPIPE 假失败，L7）
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 \
  go test ./clientcore/internal/wtransport/ -run 'TestVecgenVectors' -count=1 -v > /tmp/vecgen-test.log 2>&1
grep -E '^(=== RUN|--- |ok|FAIL|PASS)' /tmp/vecgen-test.log | head -5
echo "==> 克隆内运行 STUN/SPED 向量生成（go test -run TestVecgenStunSped）"
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 \
  go test ./pkg/egress/ -run 'TestVecgenStunSped' -count=1 -v > /tmp/vecgen-stunsped.log 2>&1
grep -E '^(=== RUN|--- |ok|FAIL|PASS)' /tmp/vecgen-stunsped.log | head -5
echo "==> 克隆内运行 term 编码/应答向量生成（go test -run TestVecgenTerm）"
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 \
  go test ./pkg/term/vt/ -run 'TestVecgenTerm' -count=1 -v > /tmp/vecgen-term.log 2>&1
grep -E '^(=== RUN|--- |ok|FAIL|PASS)' /tmp/vecgen-term.log | head -8
echo "==> 克隆内运行 surface 体编码向量生成（go test -run TestVecgenSurface）"
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 \
  go test ./pkg/term/ -run 'TestVecgenSurface' -count=1 -v > /tmp/vecgen-surface.log 2>&1
grep -E '^(=== RUN|--- |ok|FAIL|PASS)' /tmp/vecgen-surface.log | head -5
echo "==> 克隆内运行 manifest 检测求值向量生成（go test -run TestVecgenManifest）"
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 \
  go test ./pkg/term/manifest/ -run 'TestVecgenManifest' -count=1 -v > /tmp/vecgen-manifest.log 2>&1
grep -E '^(=== RUN|--- |ok|FAIL|PASS)' /tmp/vecgen-manifest.log | head -5

cd "$REPO_ROOT"
rm -f "$OUT/tun_status.jsonl"
cd "$CLONE"  # 前面已 cd 回 REPO_ROOT——本段重进克隆
echo "==> 克隆内运行 tunStatusJSON 阶段机向量生成（go test -run TestVecgenTunStatus -tags cshared）"
HOMEWAY_VECGEN_OUT="$OUT" GOTOOLCHAIN=go1.24.5 CGO_ENABLED=1 \
  go test -tags cshared ./clientcore/cmd/clientcore/ -run 'TestVecgenTunStatus' -count=1 -v > /tmp/vecgen-tunstatus.log 2>&1
grep -E '^(=== RUN|--- |ok|FAIL|PASS)' /tmp/vecgen-tunstatus.log | head -5
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
