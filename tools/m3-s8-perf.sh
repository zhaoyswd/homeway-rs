#!/bin/zsh
# m3-s8-perf.sh — M3 S8 **服务流吞吐相对门槛**本地 A/B（设计 §7/§15-3）。
#
# 做什么：同一台机器、同一个本地私有出口实例、**同一服务操作**（files 下载同一文件）：
#   · quic 臂 = App 核世代 → STREAM[tag=files] → intake → 泵 → FilesServer
# **M5 C3 单臂化（设计 §9.2 默认 (c)）**：原 A/B 的 wg 参照臂（WG 会隖 → intercept 豁免
# → UDS）随 WG 面退役 ⇒ 相对门槛（quic/wg ≥ 0.95×）失去参照臂，只采绝对读数（逐轮落盘）。
#
# 用法：tools/m3-s8-perf.sh [实例号=5] [轮数=3] [文件字节=67108864]
# 读数：/tmp/m3s8-res/perf/（SUMMARY.txt / perf-<arm>-r<N>.log / exit-lines.txt）
set -uo pipefail
REPO_ROOT="${0:h:A:h}"
n="${1:-5}"
ROUNDS="${2:-3}"
BYTES="${3:-67108864}"
RES=/tmp/m3s8-res/perf
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
SRC="$HOME/m3s8-perf.bin"
FNAME="m3s8-perf.bin"
mkdir -p "$RES" || exit 1

echo "==> 源文件：$SRC（$BYTES B）"
if [[ ! -f "$SRC" ]] || [[ "$(wc -c < "$SRC" | tr -d ' ')" != "$BYTES" ]]; then
  head -c "$BYTES" /dev/urandom > "$SRC" || exit 1
fi
shasum -a 256 "$SRC" | tee "$RES/src.sha256"

echo "==> 起干净出口 #$n（wipe + start；QUIC 面默认开）"
"$REPO_ROOT/tools/local-rust-exit.sh" wipe "$n" > "$RES/exit-wipe.txt" 2>&1 || true
"$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
  echo "!! 出口起不来（见 $RES/exit-start.txt）" >&2; exit 1; }
TOKEN="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
[[ -n "$TOKEN" ]] || { echo "!! token 取不到" >&2; exit 1; }
print -r -- "$TOKEN" > "$RES/token.txt"

echo "==> 构建测试二进制（release）"
(cd "$REPO_ROOT" && cargo build --release -p homeway-core --tests) > "$RES/build.log" 2>&1 || {
  echo "!! 构建失败（见 $RES/build.log）" >&2; tail -20 "$RES/build.log"; exit 1; }

run_one() {
  local arm="$1" r="$2" out="$RES/perf-$arm-r$r.log"
  HOMEWAY_PERF_TOKEN="$TOKEN" HOMEWAY_PERF_TRANSPORT="$arm" HOMEWAY_PERF_FILE="$FNAME" \
  HOMEWAY_PERF_BYTES="$BYTES" \
    cargo test -p homeway-core --release --test quic_stream_perf \
      stream_files_download_throughput_by_bearer -- --ignored --nocapture --test-threads=1 \
    > "$out" 2>&1
  local rc=$?
  grep -E "\[perf\]" "$out" || echo "!! $arm r$r 无读数（rc=$rc，见 $out）"
  return $rc
}

rc=0
for r in $(seq 1 "$ROUNDS"); do
  # M5 C3：WG 参照臂退役 ⇒ 单臂（只跑 quic）
  order=(quic)
  for arm in "${order[@]}"; do
    echo "==> 轮 $r / 臂 $arm"
    run_one "$arm" "$r" || rc=1
  done
done

{
  echo "# M3 S8 服务流吞吐相对门槛（$(date '+%F %T')；出口实例 #$n = $EXIT_STATE）"
  echo "# 源文件 sha256（上方 src.sha256）；文件 $FNAME（$BYTES B）"
  echo "## 逐轮读数"
  grep -hE "\[perf\]" "$RES"/perf-*.log 2>/dev/null || true
  echo "## 出口侧本轮行（服务流受理/结束，节流面：首 3 + 每 100）"
  grep -aE "服务流|files 就绪" "$EXIT_LOG" 2>/dev/null | tail -12 || true
} > "$RES/SUMMARY.txt"
cat "$RES/SUMMARY.txt"
exit $rc
