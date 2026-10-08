#!/bin/zsh
# m1-ab-e2e.sh — M1 S5-2 / S5-3①–④ 的**产品路径**端到端读数驱动
# （本地私有出口 + 本地私有中继；**绝不碰现役实例**）。
#
# 做什么（逐段对应 `docs/reviews/M1-design.md` §10 的 S5 表）：
#   A) **S5-2**：直连（LAN）档的 `HOMEWAY_TRANSPORT=wg` vs `=quic` 同刻交替吞吐 A/B
#      （`tools/m1-ab` 的产品路径驱动：ClientCore 世代装配 + TUN socketpair + 合成 UDP 流，
#      回显面放大以便读**下行**；判据 = 「无数量级回退」）；
#   B) **S5-3③**：**经中继**下行吞吐 QUIC/WG 同刻 A/B（QUIC 档 = token 的 QUIC 端点改死端口
#      `--force-relay`；WG 档 = `homeway-cli token … --dead-direct`）——**相对判据**，绝对数字
#      只登记并标注不可比（§12-④）；
#   C) **S5-3④**：**经中继迁移**用例（岛级 `Cmd::Rebind`）：连接保持 + 中继腿/assoc 峰值
#      （中继日志「起会话」行 + 「中继统计」行）+ pend 窗丢包量（逐秒 sent/acked 表）；
#   D) **S5-3①②**：包尺寸 **1411/1402B**（读探针 socket 的**字节计数**，不用 lo0 分片断言）
#      + 上行 200pps 闸（超速灌包 ⇒ 中继「转发 上」增量 + 探针的 `congestion_events/lost_packets`）。
#
# 用法：tools/m1-ab-e2e.sh [实例号=2] [轮数=3]
# 读数：/tmp/m1s5-res/e2e/（SUMMARY.txt / ab-*.log / probe-*.log / exit-lines.txt / relay-lines.txt）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-2}"
ROUNDS="${2:-3}"
RES=/tmp/m1s5-res/e2e
BIN="$REPO_ROOT/target/release/homeway-cli"
AB="$REPO_ROOT/tools/m1-ab/target/release/m1-ab"
PROBE="$REPO_ROOT/tools/quic-probe/target/release/quic-probe"
RELAY_STATE="/tmp/homeway-rs-rustrelay-$n"
RELAY_LOG="$RELAY_STATE/cache/relay.log"
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
RELAY_PORT=$((42780 + n))
mkdir -p "$RES" || exit 1

log() { echo "[$(date '+%H:%M:%S')] $*" }
die() { echo "!! $*" >&2; exit 1; }

[[ -x "$BIN" ]] || die "缺 $BIN（先 cargo build --release -p homeway-cli）"
[[ -x "$AB" ]] || die "缺 $AB（先 cd tools/m1-ab && cargo build --release）"
[[ -x "$PROBE" ]] || die "缺 $PROBE（先 cd tools/quic-probe && cargo build --release）"

# ---------- 起本地私有中继 + 出口（出口挂在该中继上） ----------
echo "==> 起本地 Rust 中继 #$n"
"$REPO_ROOT/tools/local-rust-relay.sh" start "$n" > "$RES/relay-start.txt" 2>&1 || {
  echo "!! 中继起不来（见 $RES/relay-start.txt）" >&2; exit 1; }
RL1="$("$REPO_ROOT/tools/local-rust-relay.sh" token "$n" 2>/dev/null | grep -oE 'rl1[A-Za-z0-9_=+/-]+' | head -1)"
[[ -n "$RL1" ]] || die "rl1 token 取不到"
print -r -- "$RL1" > "$RES/rl1.txt"

echo "==> 起本地 Rust 出口 #$n（挂中继：--relay rl1…）"
EXIT_EXTRA_FLAGS="--relay $RL1" "$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
  echo "!! 出口起不来（见 $RES/exit-start.txt）" >&2; exit 1; }
TOKEN="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
[[ -n "$TOKEN" ]] || die "token 取不到"
print -r -- "$TOKEN" > "$RES/token.txt"
echo "    token 长度 = ${#TOKEN}（落 $RES/token.txt）"

# WG 档经中继：Direct（WG）端点改死端口（产品 CLI 的既有缝隙）
TOKEN_WG_RELAY="$("$BIN" token "$TOKEN" --dead-direct 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
[[ -n "$TOKEN_WG_RELAY" ]] || die "--dead-direct 重编码失败"
print -r -- "$TOKEN_WG_RELAY" > "$RES/token-wg-relay.txt"

LOG0=$( [[ -f "$EXIT_LOG" ]] && wc -l < "$EXIT_LOG" | tr -d ' ' || print 0 )
RELAY0=$( [[ -f "$RELAY_LOG" ]] && wc -l < "$RELAY_LOG" | tr -d ' ' || print 0 )
ABDIR=/tmp/m1s5-res/m1ab-work
rm -rf "$ABDIR"; mkdir -p "$ABDIR"

rc=0

# ---------- A) S5-2：直连 A/B ----------
echo "==> A) S5-2 直连 A/B（$ROUNDS 轮交替）"
for r in $(seq 1 $ROUNDS); do
  if (( r % 2 == 1 )); then order=(quic wg); else order=(wg quic); fi
  for tr in $order; do
    log "A r$r $tr"
    # 直连臂：两档都**钉在直连路径**（QUIC 档 --force-direct 杀掉中继候选——否则本地回环下
    # 赛跑可能被中继抢先，两臂就不可比了；WG 档本来就走「直连优先」）。
    # 负载：3000pps 报头 + window 32（下行满尺寸回包 ⇒ 目标 ~30Mbps 下行）。
    if [[ "$tr" == "quic" ]]; then
      HOMEWAY_TRANSPORT=quic "$AB" run --token "$TOKEN" --transport quic --force-direct \
        --tag "direct-$tr-r$r" --workdir "$ABDIR" --secs 8 --rate 3000 --window 32 \
        --req-size 60 --reply-size 1252 > "$RES/ab-direct-$tr-r$r.log" 2>&1 || rc=1
    else
      HOMEWAY_TRANSPORT=wg "$AB" run --token "$TOKEN" --transport wg --force-direct \
        --tag "direct-$tr-r$r" --workdir "$ABDIR" --secs 8 --rate 3000 --window 32 \
        --req-size 60 --reply-size 1252 > "$RES/ab-direct-$tr-r$r.log" 2>&1 || rc=1
      # WG 档没有 QUIC 端点改写面（--force-direct 只动 QUIC 中继端点）——它的直连
      # 优先由 C4/C5 的「直连优先」语义保证；读 link.via 复核（报告里按 via 判同路径）。
    fi
    grep -E "^m1ab\[" "$RES/ab-direct-$tr-r$r.log" | grep -E "up\.pkt|down\.pkt=|通过性|quic\.|link=" | head -4
  done
done

# ---------- A2) S5-2 追加：**容量档**（rate 0 + window 64 = 只受在飞窗约束） ----------
echo "==> A2) 直连容量档（rate=0 window=64；$ROUNDS 轮交替）"
for r in $(seq 1 $ROUNDS); do
  if (( r % 2 == 1 )); then order=(quic wg); else order=(wg quic); fi
  for tr in $order; do
    log "A2 r$r $tr"
    HOMEWAY_TRANSPORT=$tr "$AB" run --token "$TOKEN" --transport "$tr" --force-direct \
      --tag "cap-$tr-r$r" --workdir "$ABDIR" --secs 8 --rate 0 --window 64 \
      --req-size 60 --reply-size 1252 > "$RES/ab-cap-$tr-r$r.log" 2>&1 || rc=1
    grep -E "^m1ab\[" "$RES/ab-cap-$tr-r$r.log" | grep -E "up\.pkt|down\.pkt=" | head -2
  done
done

# ---------- B) S5-3③：经中继 A/B ----------
echo "==> B) S5-3③ 经中继 A/B（$ROUNDS 轮交替；rate=150pps 低于 200pps 闸）"
for r in $(seq 1 $ROUNDS); do
  if (( r % 2 == 1 )); then order=(quic wg); else order=(wg quic); fi
  for tr in $order; do
    log "B r$r $tr（经中继）"
    if [[ "$tr" == "quic" ]]; then
      # 上行预算刻意压在 100pps（<200pps 闸）+ 每请求 8 个回包 ⇒ 下行 ~8Mbps：
      # 这条正是 §7.2 B3 的结构（上行预算 × 放大倍数 = 下行），两档同参数 ⇒ 可比。
      HOMEWAY_TRANSPORT=quic "$AB" run --token "$TOKEN" --transport quic --force-relay \
        --tag "relay-quic-r$r" --workdir "$ABDIR" --secs 10 --rate 100 --window 8 \
        --reply-count 8 --req-size 60 --reply-size 1252 > "$RES/ab-relay-$tr-r$r.log" 2>&1 || rc=1
    else
      HOMEWAY_TRANSPORT=wg "$AB" run --token "$TOKEN_WG_RELAY" --transport wg \
        --tag "relay-wg-r$r" --workdir "$ABDIR" --secs 10 --rate 100 --window 8 \
        --reply-count 8 --req-size 60 --reply-size 1252 > "$RES/ab-relay-$tr-r$r.log" 2>&1 || rc=1
    fi
    grep -E "^m1ab\[" "$RES/ab-relay-$tr-r$r.log" | grep -E "up\.pkt|down\.pkt=|通过性|quic\.|link=" | head -4
  done
done

# ---------- C) S5-3④：经中继迁移 ----------
echo "==> C) S5-3④ 经中继迁移（岛级 Rebind：127.0.0.1 → 127.0.0.2）"
RELAY_C0=$(wc -l < "$RELAY_LOG" | tr -d ' ')
log "C migrate"
"$AB" migrate --token "$TOKEN" --relay "127.0.0.1:$RELAY_PORT" --tag migrate \
  --secs 45 --rate 120 --window 8 --migrate-after 20 --alt-bind 127.0.0.2:0 --echo-bind 127.0.0.1:39105 \
  > "$RES/ab-migrate.log" 2>&1 || rc=1
grep -E "^m1ab\[migrate\]" "$RES/ab-migrate.log" | grep -E "connect\.winner|rebind|before|after|echo\.rx" || true
# 等到中继下一拍「中继统计」行（迁移后 assoc 峰值就写在那里；最多等 70s）
C_DEADLINE=$(( $(date +%s) + 70 ))
while (( $(date +%s) < C_DEADLINE )); do
  if tail -n +"$((RELAY_C0 + 1))" "$RELAY_LOG" | grep -q "中继统计"; then break; fi
  sleep 2
done
tail -n +"$((RELAY_C0 + 1))" "$RELAY_LOG" | grep -E "中继统计|起会话|分配腿|回收" | tail -12 > "$RES/relay-migrate-lines.txt"
cat "$RES/relay-migrate-lines.txt"
# 迁移后逐秒丢包（pend 窗）已在 ab-migrate.log 的逐秒表里
grep -E "sec=" "$RES/ab-migrate.log" | sed -n '18,32p' > "$RES/migrate-sec-table.txt"

# ---------- D) S5-3①②：包尺寸 + 上行 200pps 闸 ----------
echo "==> D) S5-3①② 包尺寸 / pps 闸（探针经中继）"
# D1 回显面（下行满尺寸回包的来源：内层 1362B ⇒ 出口 QUIC 包 1400B ⇒ 下行线上 1402B）
python3 -c '
import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 39001))
reply = b"\x5a" * 1334          # 内层载荷 ⇒ 内层包 20+8+1334 = 1362
while True:
    try:
        data, addr = s.recvfrom(2048)
    except Exception:
        continue
    if len(data) >= 28:
        reply = data[28:36].ljust(8, b"\x5a") + b"\x5a" * (1334 - 8)
    s.sendto(reply, addr)
' > /dev/null 2>&1 &
ECHO_PID=$!
sleep 0.5

# D1a 上行满尺寸（--size 1362 = mds ⇒ QUIC 包 1400 ⇒ 线上 1411）
log "D1 包尺寸（经中继，size=1362）"
"$PROBE" --token "$TOKEN" --relay "127.0.0.1:$RELAY_PORT" --bind 127.0.0.2:0 push \
  --n 600 --size 1362 --dst 127.0.0.1:39001 --wait-ms 3000 > "$RES/probe-size.log" 2>&1 || rc=1
grep -E "^sock:|^push:|^quinn:|^max_datagram" "$RES/probe-size.log" | tee "$RES/probe-size-readings.txt"

# D2 上行 200pps 闸（超速灌包：400pps×75s；分离桶 = 探针 127.0.0.2）
log "D2 pps 闸（分离桶：探针 127.0.0.2，400pps×~75s）"
RELAY_D2=$(wc -l < "$RELAY_LOG" | tr -d ' ')
"$PROBE" --token "$TOKEN" --relay "127.0.0.1:$RELAY_PORT" --bind 127.0.0.2:0 push \
  --n 30000 --size 1280 --rate 400 --dst 127.0.0.1:39001 --wait-ms 3000 > "$RES/probe-rate-sep.log" 2>&1 || rc=1
grep -E "^push:|^quinn:|^sock:" "$RES/probe-rate-sep.log" | tee "$RES/probe-rate-sep-readings.txt"
tail -n +"$((RELAY_D2 + 1))" "$RELAY_LOG" | grep "中继统计" | tail -3 > "$RES/relay-rate-sep-lines.txt"

# D3 同桶形态（探针 127.0.0.1 = 与出口同 IP ⇒ 共 200pps 桶；登记形态差异）
log "D3 pps 闸（同桶形态：探针 127.0.0.1，400pps×~65s）"
RELAY_D3=$(wc -l < "$RELAY_LOG" | tr -d ' ')
"$PROBE" --token "$TOKEN" --relay "127.0.0.1:$RELAY_PORT" push \
  --n 26000 --size 1280 --rate 400 --dst 127.0.0.1:39001 --wait-ms 3000 > "$RES/probe-rate-same.log" 2>&1 || rc=1
grep -E "^push:|^quinn:|^sock:" "$RES/probe-rate-same.log" | tee "$RES/probe-rate-same-readings.txt"
tail -n +"$((RELAY_D3 + 1))" "$RELAY_LOG" | grep "中继统计" | tail -3 > "$RES/relay-rate-same-lines.txt"
kill "$ECHO_PID" 2>/dev/null || true

# ---------- E) S5-4：产品形态 footprint（岛 + 客户端同进程；wg 档 = 地板） ----------
# 判据（§9.1-3）：「单连接 ≤+320K（相对地板的全量——含 runtime/Endpoint 固定成本）；
# M1 收口用产品形态（岛 + 客户端）复测」⇒ 本段：同进程（m1-ab = ClientCore 世代）
# 两档各采一次 vmmap physical footprint（**不是 ps RSS**，M0 口径），增量 = 岛边际。
sample_fp() {
  local pid="$1" n="$2"; local -a s=(); local f
  for _ in $(seq 1 $n); do
    f=$(vmmap -summary "$pid" 2>/dev/null | awk '/Physical footprint:/{gsub(/[^0-9]/,"",$3); print $3; exit}')
    [[ -n "$f" ]] && s+=("$f")
    sleep 0.5
  done
  printf '%s\n' "${s[@]}"
}
lower_med() { printf '%s\n' "$@" | grep -v '^$' | sort -n | awk '{a[NR]=$1} END{ if (NR==0) {print "NA"; exit} print a[int((NR+1)/2)] }'
}
echo "==> E) S5-4 产品形态 footprint（wg 档 = 地板 / quic 档 = 岛在位的全量）"
FP_WG=""; FP_QUIC=""
for tr in wg quic; do
  log "E fp-$tr"
  HOMEWAY_TRANSPORT=$tr "$AB" run --token "$TOKEN" --transport "$tr" --tag "fp-$tr" \
    --workdir "$ABDIR" --secs 26 --rate 20 --window 2 --req-size 60 --reply-size 1252 \
    > "$RES/ab-fp-$tr.log" 2>&1 &
  local_pid=$!
  sleep 7
  vals=($(sample_fp "$local_pid" 8))
  med=$(lower_med "${vals[@]}")
  wait "$local_pid" 2>/dev/null || true
  printf 'fp %-5s 产品形态 footprint = %sK（8 次下中位；逐次 %s）\n' "$tr" "$med" "${(j:, :)vals}" >> "$RES/fp-product.txt"
  [[ "$tr" == "wg" ]] && FP_WG="$med" || FP_QUIC="$med"
done
cat "$RES/fp-product.txt"
if [[ -n "$FP_WG" && -n "$FP_QUIC" ]]; then
  printf 'fp 岛边际（quic − wg，产品形态含 runtime/Endpoint 固定成本）= %sK（判据 ≤320K）\n' "$(( FP_QUIC - FP_WG ))" | tee -a "$RES/fp-product.txt"
fi

# ---------- 收束：证据行 + SUMMARY ----------
tail -n +"$((LOG0 + 1))" "$EXIT_LOG" > "$RES/exit-lines.txt" 2>/dev/null || true
tail -n +"$((RELAY0 + 1))" "$RELAY_LOG" > "$RES/relay-lines.txt" 2>/dev/null || true

{
  echo "# M1 S5-2/S5-3 产品路径读数（$(date '+%F %T')；rc=$rc）"
  echo "# 出口实例 = $EXIT_STATE（$EXIT_LOG）；中继实例 = $RELAY_STATE（relay.log，数据口 $RELAY_PORT）"
  echo "# 拓扑：m1-ab(ClientCore 世代，tun=socketpair) → [直连 | 中继 127.0.0.1:$RELAY_PORT → 出口腿] → 出口 intercept → 127.0.0.1 回显"
  echo
  echo "## A) S5-2 直连 A/B（product path；同刻交替）"
  for r in $(seq 1 $ROUNDS); do
    for tr in quic wg; do
      echo "-- r$r $tr"
      grep -E "^m1ab\[" "$RES/ab-direct-$tr-r$r.log" 2>/dev/null | grep -E "up\.pkt|down\.pkt=|通过性自检|link=|quic=\{" || true
    done
  done
  echo
  echo "## A2) S5-2 容量档（直连；rate=0 window=64）"
  for r in $(seq 1 $ROUNDS); do
    for tr in quic wg; do
      echo "-- r$r $tr（容量档）"
      grep -E "^m1ab\[" "$RES/ab-cap-$tr-r$r.log" 2>/dev/null | grep -E "up\.pkt|down\.pkt=|link=" || true
    done
  done
  echo
  echo "## B) S5-3③ 经中继 A/B"
  for r in $(seq 1 $ROUNDS); do
    for tr in quic wg; do
      echo "-- r$r $tr（经中继）"
      grep -E "^m1ab\[" "$RES/ab-relay-$tr-r$r.log" 2>/dev/null | grep -E "up\.pkt|down\.pkt=|通过性自检|link=|quic=\{" || true
    done
  done
  echo
  echo "## C) S5-3④ 经中继迁移"
  grep -E "^m1ab\[migrate\]" "$RES/ab-migrate.log" 2>/dev/null | grep -E "connect\.winner|rebind\.|before|after|echo\.rx|stopped" || true
  echo "-- 中继侧（本轮新增：起会话/统计/回收）"
  cat "$RES/relay-migrate-lines.txt" 2>/dev/null || true
  echo "-- 迁移窗逐秒 sent/acked（pend 窗丢包）"
  cat "$RES/migrate-sec-table.txt" 2>/dev/null || true
  echo
  echo "## D) S5-3①② 包尺寸与 pps 闸（探针经中继）"
  echo "-- D1 包尺寸（size=1362；tx_max 期望 1411 = 1400+11；rx_max 期望 1402 = 1400+2）"
  cat "$RES/probe-size-readings.txt" 2>/dev/null || true
  echo "-- D2 闸（分离桶）"
  cat "$RES/probe-rate-sep-readings.txt" 2>/dev/null || true
  cat "$RES/relay-rate-sep-lines.txt" 2>/dev/null || true
  echo "-- D3 闸（同桶形态）"
  cat "$RES/probe-rate-same-readings.txt" 2>/dev/null || true
  cat "$RES/relay-rate-same-lines.txt" 2>/dev/null || true
  echo
  echo "## 出口侧证据行（本轮新增）"
  grep -E "peer: \+|quic: 连接采纳|quic: 路径变更|quic: 端点就绪|邻居|腿" "$RES/exit-lines.txt" 2>/dev/null | head -20 || true
  echo "## 中继侧（本轮新增，尾部）"
  tail -20 "$RES/relay-lines.txt" 2>/dev/null || true
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt / ab-*.log / probe-*.log）rc=$rc"
exit $rc
