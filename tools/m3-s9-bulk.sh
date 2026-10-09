#!/bin/zsh
# m3-s9-bulk.sh — M3 **S9** 多流并行 bulk 复测/出口存活探针（真机 speedtest 的同规格形态）。
#
# 做什么：起干净本地私有出口 → 同一 QUIC 连接上 N 条 `STREAM[tag=files]` 并行下载同一大文件
# （缺省 4 流 × ~91 MiB，照 S8 真机 speedtest 的规格）→ 逐轮打印聚合/逐流 MiB/s，
# 并**全程监测出口进程**（存活、RSS、收工行、崩溃报告）。
# 判据：① 每流完成字节 = 源字节（下载完整性）；② 出口进程逐轮存活；③ 聚合吞吐读数登记。
#
# 用法：tools/m3-s9-bulk.sh [实例号=5] [轮数=3] [流数=4] [每流字节=95420416]
# 读数：/tmp/m3s9-res/bulk/（SUMMARY.txt / par-r<N>.log / exit-watch.tsv）
set -uo pipefail
REPO_ROOT="${0:h:A:h}"
n="${1:-5}"; ROUNDS="${2:-3}"; STREAMS="${3:-4}"; BYTES="${4:-95420416}"
RES=/tmp/m3s9-res/bulk
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
SRC="$HOME/m3s9-bulk.bin"; FNAME="m3s9-bulk.bin"
mkdir -p "$RES" || exit 1

echo "==> 源文件：$SRC（$BYTES B）"
if [[ ! -f "$SRC" ]] || [[ "$(wc -c < "$SRC" | tr -d ' ')" != "$BYTES" ]]; then
  head -c "$BYTES" /dev/urandom > "$SRC" || exit 1
fi
shasum -a 256 "$SRC" | tee "$RES/src.sha256"

echo "==> 起干净出口 #$n"
"$REPO_ROOT/tools/local-rust-exit.sh" wipe "$n" > "$RES/exit-wipe.txt" 2>&1 || true
"$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
  echo "!! 出口起不来（见 $RES/exit-start.txt）" >&2; exit 1; }
TOKEN="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw2[A-Za-z0-9_=+/-]+' | head -1)"
[[ -n "$TOKEN" ]] || { echo "!! token 取不到" >&2; exit 1; }
print -r -- "$TOKEN" > "$RES/token.txt"
PID=$(cat "$EXIT_STATE/pid")

echo "==> 构建测试二进制（release）"
(cd "$REPO_ROOT" && cargo build --release -p homeway-core --tests) > "$RES/build.log" 2>&1 || {
  echo "!! 构建失败（见 $RES/build.log）" >&2; tail -20 "$RES/build.log"; exit 1; }

echo "==> 出口监测（pid=$PID；RSS/存活，逐 0.5s 一行）"
: > "$RES/exit-watch.tsv"
( while kill -0 "$PID" 2>/dev/null; do
    printf "%s\t%s\t%s\t%s\t%s\n" "$(date '+%T')" "$(ps -o rss= -p "$PID" | tr -d ' ')" \
      "$(ps -M -p "$PID" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')" \
      "$(lsof -p "$PID" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')" "alive" >> "$RES/exit-watch.tsv"
    sleep 0.5
  done
  printf "%s\t-\t-\t-\tDEAD\n" "$(date '+%T')" >> "$RES/exit-watch.tsv" ) &
WATCH=$!

rc=0
for r in $(seq 1 "$ROUNDS"); do
  out="$RES/par-r$r.log"
  HOMEWAY_PERF_TOKEN="$TOKEN" HOMEWAY_PERF_TRANSPORT=quic HOMEWAY_PERF_FILE="$FNAME" \
  HOMEWAY_PERF_BYTES="$BYTES" HOMEWAY_PERF_PARALLEL="$STREAMS" HOMEWAY_PERF_ROUNDS=1 \
    cargo test -p homeway-core --release --test quic_stream_perf \
      stream_files_parallel_download -- --ignored --nocapture --test-threads=1 \
    > "$out" 2>&1
  lrc=$?; [[ $lrc -ne 0 ]] && rc=1
  grep -hE "\[perf-par\]|panicked" "$out" || echo "!! 轮 $r 无读数（rc=$lrc，见 $out）"
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "!! 轮 $r 之后出口进程已不在（pid=$PID）——见 $RES/exit-watch.tsv 与 $EXIT_LOG"
    rc=1
  fi
done
kill "$WATCH" 2>/dev/null; wait "$WATCH" 2>/dev/null

# 退出形态判定（S8 §4-1 的「出口进程死亡」随访面）：无收工行 = 不干净退出
if kill -0 "$PID" 2>/dev/null; then
  echo "==> 出口退出形态：全程存活（pid=$PID，$ROUNDS 轮）"
else
  if grep -q "收到停止信号" "$EXIT_LOG" 2>/dev/null; then
    echo "==> 出口退出形态：clean（日志有「收到停止信号」收工行）"
  else
    echo "==> 出口退出形态：**UNCLEAN**（无收工行、无 serve 前台收工——疑似 SIGKILL/外部击杀/静默终止）"
    echo "    末 40 行出口日志 → $RES/exit-tail-unclean.txt"
    tail -40 "$EXIT_LOG" > "$RES/exit-tail-unclean.txt" 2>&1
    ls -t "$HOME"/Library/Logs/DiagnosticReports/*homeway* 2>/dev/null | head -5 \
      | tee "$RES/crash-reports.txt" || echo "    （无 homeway 相关 DiagnosticReports）"
  fi
fi

{
  echo "# M3 S9 多流并行 bulk（$(date '+%F %T')；出口实例 #$n pid=$PID）"
  echo "# 源文件 $FNAME（$BYTES B）；流数=$STREAMS；轮数=$ROUNDS"
  echo "## 逐轮读数"
  grep -hE "\[perf-par\]" "$RES"/par-r*.log 2>/dev/null || true
  echo "## 出口存活/RSS/线程/fd（首行/末行/峰值）"
  echo -e "时刻\tRSS(KB)\t线程\tfd\t状态"; head -1 "$RES/exit-watch.tsv"; tail -1 "$RES/exit-watch.tsv"
  awk -F'\t' 'BEGIN{m=0;t=0;f=0} $2+0>m{m=$2+0} $3+0>t{t=$3+0} $4+0>f{f=$4+0} END{printf "peak_rss_kb=%d peak_threads=%d peak_fds=%d\n",m,t,f}' "$RES/exit-watch.tsv"
  echo "## 出口侧服务流行（节流面：首 3 + 每 100）"
  grep -aE "服务流|收工|panic" "$EXIT_LOG" 2>/dev/null | tail -10 || true
} > "$RES/SUMMARY.txt"
cat "$RES/SUMMARY.txt"
exit $rc
