#!/bin/zsh
# m3-s8-coexist.sh — M3 S8 **bulk（L3/DATAGRAM）与服务流（STREAM）共存** A/B（设计 §7 风险行 5 / 设计门 M-1）。
#
# 做什么：同一本地私有出口、同一 QUIC 承载、同一 L3 产品路径（`tools/m1-ab` 的 ClientCore 世代
# + TUN socketpair + 合成 UDP 流），只变**服务流是否在跑**：
#   · 基线臂：空载，跑一轮 `m1-ab run --transport quic --rate 0 --window 64`（容量档）
#   · 共存臂：同刻后台跑一条 files 服务流（STREAM 大文件下载，复用 quic_stream_perf 用例）
# 判据（§7 登记为门槛/风险面）：**L3 吞吐在共存臂的相对跌幅**（登记读数；不设死判据）。
#
# 用法：tools/m3-s8-coexist.sh [实例号=5] [轮数=2]
# 读数：/tmp/m3s8-res/coexist/（SUMMARY.txt / base-*.log / coex-*.log）
set -uo pipefail
REPO_ROOT="${0:h:A:h}"
n="${1:-5}"
ROUNDS="${2:-2}"
RES=/tmp/m3s8-res/coexist
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
AB="$REPO_ROOT/tools/m1-ab/target/release/m1-ab"
mkdir -p "$RES" || exit 1

TOKEN="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
[[ -n "$TOKEN" ]] || { echo "!! token 取不到（出口 #$n 未在跑？）" >&2; exit 1; }
print -r -- "$TOKEN" > "$RES/token.txt"
WORK="$RES/m1ab-work"; rm -rf "$WORK"; mkdir -p "$WORK"

run_ab() { # $1=tag(输出前缀) $2=workdir
  "$AB" run --token "$TOKEN" --transport quic --secs 8 --rate 0 --window 64 \
    --workdir "$2" > "$RES/$1.log" 2>&1
  grep -E "^\[ab\]" "$RES/$1.log" | tail -3 || tail -5 "$RES/$1.log"
}

for r in $(seq 1 "$ROUNDS"); do
  echo "==> 轮 $r：基线臂（空载）"
  run_ab "base-r$r" "$WORK/base-$r"

  echo "==> 轮 $r：共存臂（后台 files STREAM 下载）"
  HOMEWAY_PERF_TOKEN="$TOKEN" HOMEWAY_PERF_TRANSPORT=quic HOMEWAY_PERF_FILE=m3s8-perf.bin \
  HOMEWAY_PERF_BYTES=67108864 \
    cargo test -p homeway-core --release --test quic_stream_perf \
      stream_files_download_throughput_by_bearer -- --ignored --nocapture --test-threads=1 \
    > "$RES/stream-r$r.log" 2>&1 &
  STREAM=$!
  sleep 1
  run_ab "coex-r$r" "$WORK/coex-$r"
  wait $STREAM 2>/dev/null || true
  grep -hE "\[perf\]" "$RES/stream-r$r.log" || echo "!! 轮 $r 共存臂的服务流无读数"
done

{
  echo "# M3 S8 bulk(L3) × 服务流(STREAM) 共存 A/B（$(date '+%F %T')；出口实例 #$n）"
  echo "## 基线臂（空载）"
  grep -hE "m1ab\[run\]" "$RES"/base-*.log 2>/dev/null || true
  echo "## 共存臂（同刻 files STREAM 下载在跑）"
  grep -hE "m1ab\[run\]" "$RES"/coex-*.log 2>/dev/null || true
  echo "## 共存臂的服务流读数"
  grep -hE "\[perf\]" "$RES"/stream-*.log 2>/dev/null || true
} > "$RES/SUMMARY.txt"
cat "$RES/SUMMARY.txt"
