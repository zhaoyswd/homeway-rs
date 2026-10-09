#!/bin/zsh
# m2-s5-e2e.sh — M2 S5-2 / S5-3 / S5-5 的**产品路径**端到端读数驱动
# （本地私有出口 + 本地私有中继；**绝不碰现役实例**）。
#
# 做什么（逐段对应 `docs/reviews/M2-design.md` §10 的 S5 表 + §3.3 判据 1–6）：
#   A) **S5-2**：直连档 `HOMEWAY_TRANSPORT=wg` vs `=quic` 同刻交替吞吐 A/B
#      （产品路径 = ClientCore 世代 + TUN socketpair + 合成 UDP 流；判据 = 无数量级回退）
#      + A2 容量档（rate=0/window=64）。
#   B) **S5-3-1（判据 1，单源有界）**：`quic-probe flood --mode pin-fail`——
#      B1 = 独立出口 #n2（`HOMEWAY_QUIC_ADMIT_RETRY=never`）⇒ **干净 K−F 算术**（判据面）；
#      B2 = 主出口 #n（缺省 pressure 档）⇒ 「最悲观形态」参考读数（M2.md §1.1 残余点名）。
#   C) **S5-3-1（判据 2 前半，全局有界·在途）**：`flood --mode stall`（黑洞 socket）
#      ⇒ 并发握手闸（`handshake_cap=64`）+ `handshake_deadline` 回收。
#   D) **S5-3-2（判据 2 后半）**：`flood --mode no-hello`（握手完成但不发 Hello）
#      ⇒ 连接总数闸（`conn_cap=2×max_devices=64`）+ `ADMIT_DEADLINE` 内全部被关。
#   E) **S5-3-3（判据 3，不伤既有连接）**：30s 产品路径流 × 中段持续洪泛（异源）
#      ⇒ 逐秒下行吞吐的洪泛窗/净窗对比（判据 ≤10%）。
#   F) **S5-3 预算**：300ms RTT 延迟代理（`tools/udp-delay-proxy.py`）下探针 3 轮 +
#      岛（产品路径 `--quic-ep` 指到代理）3 轮。
#   G) **S5-5 窄路径注入**：`--mtu-cap 1200`（缝，区间外）⇒ 「窄路径不可用」行 +
#      `超限` 计数；`--mtu-cap 1320`（生产区间下限）= 正对照（mds 1282 > 1280，无该行）。
#
# 用法：tools/m2-s5-e2e.sh [实例号=2] [轮数=3]
# 读数：${M2S5_RES:-/tmp/m2s5-res/e2e}/（SUMMARY.txt / *.log / exit-lines.txt / …）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-2}"
ROUNDS="${2:-3}"
RES="${M2S5_RES:-/tmp/m2s5-res/e2e}"
BIN="$REPO_ROOT/target/release/homeway-cli"
AB="$REPO_ROOT/tools/m1-ab/target/release/m1-ab"
PROBE="$REPO_ROOT/tools/quic-probe/target/release/quic-probe"
RELAY_STATE="/tmp/homeway-rs-rustrelay-$n"
RELAY_LOG="$RELAY_STATE/cache/relay.log"
RELAY_PORT=$((42780 + n))
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
EXIT_QUIC_PORT_FILE="$EXIT_STATE/cache/quic_listen_port.txt"
# 独立出口 #n2：只给 B1（`retry_policy=never`），不挂中继（少一个变量）
n2=$((n + 2))  # 端口不撞：QUIC 端口 = listen+1 ⇒ 隔一个实例号
EXIT2_STATE="/tmp/homeway-rs-rustexit-$n2"
EXIT2_LOG="$EXIT2_STATE/stdout.log"
EXIT2_QUIC_PORT_FILE="$EXIT2_STATE/cache/quic_listen_port.txt"
PROXY_PORT=$((42700 + n))
# 第三个出口 #n3（只给 C2：把每源闸预算抬到 1000 以**隔离**出并发握手闸 handshake_cap=64）
n3=$((n + 4))
EXIT3_STATE="/tmp/homeway-rs-rustexit-$n3"
EXIT3_LOG="$EXIT3_STATE/stdout.log"
EXIT3_QUIC_PORT_FILE="$EXIT3_STATE/cache/quic_listen_port.txt"
mkdir -p "$RES" || exit 1

log() { echo "[$(date '+%H:%M:%S')] $*"; }
die() { echo "!! $*" >&2; exit 1; }

LAN_IP="$(ipconfig getifaddr en0 2>/dev/null || echo 192.168.3.12)"
PROXY_PID=""

cleanup() {
  [[ -n "$PROXY_PID" ]] && kill "$PROXY_PID" 2>/dev/null
  return 0
}
trap cleanup EXIT INT TERM

[[ -x "$BIN" ]] || die "缺 $BIN（先 cargo build --release -p homeway-cli）"
[[ -x "$AB" ]] || die "缺 $AB（先 cd tools/m1-ab && cargo build --release）"
[[ -x "$PROBE" ]] || die "缺 $PROBE（先 cd tools/quic-probe && cargo build --release）"

# ---------- 起本地私有中继 + 出口（主出口挂中继）+ 独立出口 #n2 ----------
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
QUIC_PORT="$(cat "$EXIT_QUIC_PORT_FILE" 2>/dev/null)"
[[ -n "$QUIC_PORT" ]] || die "缺 $EXIT_QUIC_PORT_FILE（QUIC 面未起？）"
echo "    主出口 #$n：wg=$((42650 + n)) quic=$QUIC_PORT（token 长 ${#TOKEN}）"
"$BIN" serve token --state "$EXIT_STATE" | grep -E "端点|endpoint" | head -3 >> "$RES/token-endpoints.txt" 2>/dev/null || true

echo "==> 起独立出口 #$n2（HOMEWAY_QUIC_ADMIT_RETRY=never；不挂中继）"
HOMEWAY_QUIC_ADMIT_RETRY=never "$REPO_ROOT/tools/local-rust-exit.sh" start "$n2" > "$RES/exit2-start.txt" 2>&1 || {
  echo "!! 出口 #$n2 起不来（见 $RES/exit2-start.txt）" >&2; exit 1; }
QUIC2_PORT="$(cat "$EXIT2_QUIC_PORT_FILE" 2>/dev/null)"
[[ -n "$QUIC2_PORT" ]] || die "缺 $EXIT2_QUIC_PORT_FILE"
TOKEN2="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n2" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
echo "    独立出口 #$n2：quic=$QUIC2_PORT"

echo "==> 起独立出口 #$n3（config: serve.quic_admit.per_src_fails=1000；不挂中继）"
mkdir -p "$EXIT3_STATE" 2>/dev/null
cat > "$EXIT3_STATE/config.toml" <<'TOML'
# M2 S5-3 C2 臂：把每源闸预算抬到 1000 **且关掉 Retry**，以**隔离**出
# 「连接总数 = 2×max_devices = 64」这条上界。
# 为什么必须同时关 Retry（本切片实测）：pressure 档下 Retry 触发条件②（同源未完成 ≥5）
# 会让后续每条尝试收到 Retry 而**不建握手**（黑洞客户端收不到 Retry ⇒ 永不销账）⇒
# in-flight 顶多停在 5 条；缺省配置下 64 这条线是**双重不可达**（每源闸 16/10s/源 +
# Retry 均先把单源束住）。本臂是「上界本身的触达测试」，非缺省形态（登记在案）。
[serve]
[serve.quic_admit]
per_src_fails = 1000
retry_policy = "never"
TOML
"$REPO_ROOT/tools/local-rust-exit.sh" start "$n3" > "$RES/exit3-start.txt" 2>&1 || {
  echo "!! 出口 #$n3 起不来（见 $RES/exit3-start.txt）" >&2; exit 1; }
QUIC3_PORT="$(cat "$EXIT3_QUIC_PORT_FILE" 2>/dev/null)"
[[ -n "$QUIC3_PORT" ]] || die "缺 $EXIT3_QUIC_PORT_FILE"
TOKEN3="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n3" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
grep -E "抗放大面" "$EXIT3_LOG" | tail -1
echo "    独立出口 #$n3：quic=$QUIC3_PORT"

# WG 档经中继：Direct（WG）端点改死端口（既有缝隙；A/B 的同路径保证）
TOKEN_WG_RELAY="$("$BIN" token "$TOKEN" --dead-direct 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"

LOG0=$( [[ -f "$EXIT_LOG" ]] && wc -l < "$EXIT_LOG" | tr -d ' ' || print 0 )
LOG2_0=$( [[ -f "$EXIT2_LOG" ]] && wc -l < "$EXIT2_LOG" | tr -d ' ' || print 0 )
LOG3_0=$( [[ -f "$EXIT3_LOG" ]] && wc -l < "$EXIT3_LOG" | tr -d ' ' || print 0 )
RELAY0=$( [[ -f "$RELAY_LOG" ]] && wc -l < "$RELAY_LOG" | tr -d ' ' || print 0 )
ABDIR=/tmp/m2s5-res/m1ab-work
rm -rf "$ABDIR"; mkdir -p "$ABDIR"
rc=0

# 出口 footprint 采样（vmmap physical footprint；M0 口径——`ps rss` 不作判据）
sample_fp() {  # sample_fp <pid> <次数>
  # **单位归一（K）**：`vmmap` 在 ≥10MB 时打 `10.3M`（K 档打 `1234K`）——只 strip 非数字
  # 会把 10.3M 读成 103（M1 的 `m1-ab-e2e.sh` 与 `quic-ab.sh` 的采样器同款，探针档恒 <10MB
  # 故未暴露；产品出口 10MB+ 会踩，本切片实测踩过一次 ⇒ 此处归一）。
  local pid="$1" cnt="${2:-6}" out=() f
  for _ in $(seq 1 $cnt); do
    f=$(vmmap -summary "$pid" 2>/dev/null | awk '/Physical footprint:/{v=$3; if (v ~ /M$/) { gsub(/M$/,"",v); printf "%.0f", v*1024 } else { gsub(/K$/,"",v); printf "%.0f", v }; exit}')
    [[ -n "$f" ]] && out+=("$f")
    sleep 0.4
  done
  printf '%s\n' "${out[@]}"
}
lower_med() { printf '%s\n' "$@" | grep -v '^$' | sort -n | awk '{a[NR]=$1} END{ if (NR==0) {print "NA"; exit} print a[int((NR+1)/2)] }'; }
exit_pid() { cat "/tmp/homeway-rs-rustexit-$1/pid" 2>/dev/null; }

# ---------- A) S5-2：直连 A/B ----------
echo "==> A) S5-2 直连 A/B（$ROUNDS 轮交替）"
for r in $(seq 1 $ROUNDS); do
  if (( r % 2 == 1 )); then order=(quic wg); else order=(wg quic); fi
  for tr in $order; do
    log "A r$r $tr"
    HOMEWAY_TRANSPORT=$tr "$AB" run --token "$TOKEN" --transport "$tr" --force-direct \
      --tag "direct-$tr-r$r" --workdir "$ABDIR" --secs 8 --rate 3000 --window 32 \
      --req-size 60 --reply-size 1252 > "$RES/ab-direct-$tr-r$r.log" 2>&1 || rc=1
    grep -E "^m1ab\[" "$RES/ab-direct-$tr-r$r.log" | grep -E "up\.pkt|down\.pkt=|link=" | head -3
  done
done

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

# ---------- A3) 判据 6：常态赛跑 ≥5 轮 ⇒ retry_sent=0 且 flood_refused=0 ----------
# 不带 `--force-direct` ⇒ 岛按 token 的全候选跑**正常赛跑**（直连 + 中继；输家由岛收掉）。
# 判据来源 = 设计 §3.3-6（措辞按 §14-1③ 改写：正常赛跑在阈值内 ⇒ 不触发闸）。
echo "==> A3) 判据 6：常态赛跑 6 轮（全候选；不带 --force-direct）"
A3_LINES0=$(wc -l < "$EXIT_LOG" | tr -d ' ')
for r in 1 2 3 4 5 6; do
  HOMEWAY_TRANSPORT=quic "$AB" run --token "$TOKEN" --transport quic \
    --tag "race-r$r" --workdir "$ABDIR" --secs 6 --rate 500 --window 8 \
    --req-size 60 --reply-size 1252 > "$RES/ab-race-r$r.log" 2>&1 || rc=1
  grep -E "^m1ab\[" "$RES/ab-race-r$r.log" | grep -E "down\.pkt=|link=" | head -2
done
{
  echo "A3 常态赛跑 6 轮（全候选）后出口新增行："
  echo "  地址校验挑战=$(tail -n +$((A3_LINES0 + 1)) "$EXIT_LOG" | grep -c '地址校验挑战')"
  echo "  握手洪泛拒绝=$(tail -n +$((A3_LINES0 + 1)) "$EXIT_LOG" | grep -c '握手洪泛拒绝')"
  echo "  认证超时=$(tail -n +$((A3_LINES0 + 1)) "$EXIT_LOG" | grep -c '认证超时')"
} | tee -a "$RES/SUMMARY-flood.txt"

# ---------- B) 判据 1：单源有界（pin-fail） ----------
echo "==> B1) 判据 1 干净臂：独立出口 #$n2（retry=never）K=24 顺序未完成尝试"
"$PROBE" --token "$TOKEN2" --quic "127.0.0.1:$QUIC2_PORT" --bind "127.0.0.1:0" \
  flood --mode pin-fail --k 24 > "$RES/flood-pinfail-never.log" 2>&1 || rc=1
grep -E "^flood:" "$RES/flood-pinfail-never.log" | tee -a "$RES/SUMMARY-flood.txt"

echo "==> B2) 判据 1 缺省档（pressure，最悲观形态，参考读数）K=24"
"$PROBE" --token "$TOKEN" --quic "127.0.0.1:$QUIC_PORT" --bind "127.0.0.1:0" \
  flood --mode pin-fail --k 24 > "$RES/flood-pinfail-pressure.log" 2>&1 || rc=1
grep -E "^flood:" "$RES/flood-pinfail-pressure.log" | tee -a "$RES/SUMMARY-flood.txt"

# ---------- B3) 判据 2 的「多源」面：异源 /32 的**桶分离**证伪（干净窗） ----------
# 期望（设计 §3.2-④）：键 = v4 /32 ⇒ 两个不同源地址各有独立预算。
# 实测（本切片 S5 发现，见 §3 的 D1）：出口 QUIC socket 是双栈 `[::]` ⇒ IPv4 对端以
# v4-mapped IPv6 出现 ⇒ `SrcKey::of` 落 `::/64` 桶 ⇒ **所有 IPv4 源共用一个预算**。
# 本臂是**可证伪形态**：等窗清（11s）后，源 A 打 k=10（用掉 10/16），紧接着源 B（不同 /32）
# 打 k=8 —— 若两源独立，B 的 8 次应全是握手级失败（transport）；若同桶，B 只放行 6 次、
# 第 7..8 次被拒。
echo "==> B3) 多源桶分离证伪（干净窗：$LAN_IP 打 10 → 127.0.0.1 打 8）"
sleep 11
"$PROBE" --token "$TOKEN" --quic "$LAN_IP:$QUIC_PORT" --bind "$LAN_IP:0" \
  flood --mode pin-fail --k 10 > "$RES/flood-msrc-a.log" 2>&1 || rc=1
"$PROBE" --token "$TOKEN" --quic "127.0.0.1:$QUIC_PORT" --bind "127.0.0.1:0" \
  flood --mode pin-fail --k 8 > "$RES/flood-msrc-b.log" 2>&1 || rc=1
{ echo "-- 源 A $LAN_IP（k=10）"; grep -E "^flood: k=" "$RES/flood-msrc-a.log"
  echo "-- 源 B 127.0.0.1（k=8，紧接着打，不同 /32）"; grep -E "^flood: k=" "$RES/flood-msrc-b.log"; } \
  | tee -a "$RES/SUMMARY-flood.txt"

# ---------- C) 判据 2 前半：并发握手在途（stall 黑洞） ----------
echo "==> C) 判据 2 前半：stall 20 并发（黑洞；干净窗；观察 10s 期限回收）"
sleep 11
"$PROBE" --token "$TOKEN" --quic "$LAN_IP:$QUIC_PORT" --bind "$LAN_IP:0" \
  flood --mode stall --k 20 --hold-secs 14 > "$RES/flood-stall.log" 2>&1 || rc=1
grep -E "^flood:" "$RES/flood-stall.log" | tail -4 | tee -a "$RES/SUMMARY-flood.txt"

echo "==> C2) 判据 2 前半（上界触达）：独立出口 #$n3（每源闸抬到 1000）stall 80 ⇒ handshake_cap=64"
sleep 11
C2_LINES0=$(wc -l < "$EXIT3_LOG" | tr -d ' ')
"$PROBE" --token "$TOKEN3" --quic "$LAN_IP:$QUIC3_PORT" --bind "$LAN_IP:0" \
  flood --mode stall --k 80 --hold-secs 14 > "$RES/flood-stall-cap.log" 2>&1 || rc=1
grep -E "^flood:" "$RES/flood-stall-cap.log" | tail -3 | tee -a "$RES/SUMMARY-flood.txt"
{
  echo "C2 出口#$n3 新增行（抬闸后）："
  echo "  拒新连接（并发握手）=$(tail -n +$((C2_LINES0 + 1)) "$EXIT3_LOG" | grep -c '并发握手')"
  echo "  拒新连接（连接总数）=$(tail -n +$((C2_LINES0 + 1)) "$EXIT3_LOG" | grep -c '连接总数')"
  echo "  握手期限=$(tail -n +$((C2_LINES0 + 1)) "$EXIT3_LOG" | grep -c '握手期限')"
  echo "  前 4 行样本："
  tail -n +$((C2_LINES0 + 1)) "$EXIT3_LOG" | grep -E "拒新连接|握手期限" | head -4 | sed 's/^/    /'
} | tee -a "$RES/SUMMARY-flood.txt"

# ---------- D) 判据 2 后半：握手完成但不发 Hello ----------
echo "==> D) 判据 2 后半：no-hello 70 条（异源 $LAN_IP；等 ADMIT_DEADLINE=10s 回收）"
sleep 11
PID_MED_BEFORE=($(sample_fp "$(exit_pid $n)" 4))
printf 'fp 出口#%s 洪泛前=%sK\n' "$n" "$(lower_med "${PID_MED_BEFORE[@]}")" >> "$RES/fp-exit.txt"
"$PROBE" --token "$TOKEN" --quic "$LAN_IP:$QUIC_PORT" --bind "$LAN_IP:0" \
  flood --mode no-hello --k 70 --hold-secs 16 > "$RES/flood-nohello.log" 2>&1 || rc=1
grep -E "^flood:" "$RES/flood-nohello.log" | tail -5 | tee -a "$RES/SUMMARY-flood.txt"
PID_MED_AFTER=($(sample_fp "$(exit_pid $n)" 4))
printf 'fp 出口#%s 洪泛后=%sK\n' "$n" "$(lower_med "${PID_MED_AFTER[@]}")" >> "$RES/fp-exit.txt"

# ---------- E) 判据 3：不伤既有连接（30s 流 × 中段异源持续洪泛） ----------
echo "==> E) 判据 3：不伤既有连接（30s 流；t=10s 起持续洪泛 ~10s；$ROUNDS 轮）"
for r in $(seq 1 $ROUNDS); do
  log "E r$r"
  T0=$(date +%s)
  HOMEWAY_TRANSPORT=quic "$AB" run --token "$TOKEN" --transport quic --force-direct \
    --tag "flood-impact-r$r" --workdir "$ABDIR" --secs 30 --rate 3000 --window 32 \
    --req-size 60 --reply-size 1252 > "$RES/ab-flood-impact-r$r.log" 2>&1 &
  ABS_PID=$!
  sleep 10
  local_fs=$(date +%s)
  "$PROBE" --token "$TOKEN" --quic "$LAN_IP:$QUIC_PORT" --bind "$LAN_IP:0" \
    flood --mode pin-fail --k 600 --interval-ms 15 > "$RES/flood-impact-r$r.log" 2>&1 || rc=1
  local_fe=$(date +%s)
  wait $ABS_PID 2>/dev/null || rc=1
  printf 'flood-impact r%s: ab_start=%s flood_start=%s flood_end=%s（秒索引 = 相对 ab_start）\n' \
    "$r" "$T0" "$local_fs" "$local_fe" >> "$RES/flood-impact-windows.txt"
  grep -E "^m1ab\[" "$RES/ab-flood-impact-r$r.log" | grep -E "down\.pkt=" | head -1
done

# ---------- E2) 附带损伤：洪泛刚结束时**常态重连**能否立刻成功 ----------
echo "==> E2) 洪泛后常态重连（同一桶未出窗时应当被拒；等窗清后应当成功）"
"$PROBE" --token "$TOKEN" --quic "$LAN_IP:$QUIC_PORT" --bind "$LAN_IP:0" \
  flood --mode pin-fail --k 20 > "$RES/flood-e2-saturate.log" 2>&1 || rc=1
grep -E "^flood: k=" "$RES/flood-e2-saturate.log" | sed 's/^/E2 饱和：/' | tee -a "$RES/SUMMARY-flood.txt"
"$PROBE" --token "$TOKEN" --quic "$LAN_IP:$QUIC_PORT" --bind "$LAN_IP:0" conn \
  > "$RES/e2-reconnect-immediate.log" 2>&1 || true   # 预期被拒（观测面；不计 rc）
grep -E "^握手完成|^准入: A4|^probe: 握手失败|错误" "$RES/e2-reconnect-immediate.log" | head -3 \
  | sed 's/^/E2 立即重连（窗内）：/' | tee -a "$RES/SUMMARY-flood.txt"
sleep 11
"$PROBE" --token "$TOKEN" --quic "$LAN_IP:$QUIC_PORT" --bind "$LAN_IP:0" conn \
  > "$RES/e2-reconnect-afterwindow.log" 2>&1 || rc=1
grep -E "^握手完成|^准入: A4" "$RES/e2-reconnect-afterwindow.log" | head -3 \
  | sed 's/^/E2 等窗清后重连：/' | tee -a "$RES/SUMMARY-flood.txt"

# ---------- F) 预算：300ms RTT（延迟代理） ----------
echo "==> F) 300ms RTT 预算（udp-delay-proxy 单向 150ms；探针 3 轮 + 岛 3 轮）"
python3 "$REPO_ROOT/tools/udp-delay-proxy.py" "127.0.0.1:$PROXY_PORT" "127.0.0.1:$QUIC_PORT" 150 \
  > "$RES/proxy.log" 2>&1 &
PROXY_PID=$!
sleep 0.5
TOKEN_PROXY="$("$BIN" token "$TOKEN" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
for r in 1 2 3; do
  t0=$(python3 -c 'import time;print(int(time.time()*1000))')
  "$PROBE" --token "$TOKEN" --quic "127.0.0.1:$PROXY_PORT" --bind "127.0.0.1:0" conn \
    > "$RES/budget-probe-r$r.log" 2>&1 || rc=1
  t1=$(python3 -c 'import time;print(int(time.time()*1000))')
  printf 'budget-probe r%s: 全程=%sms（外部墙钟）\n' "$r" "$((t1 - t0))" >> "$RES/budget.txt"
  grep -E "^握手完成|^准入: A4|^max_datagram_size" "$RES/budget-probe-r$r.log" | sed "s/^/budget-probe r$r: /" >> "$RES/budget.txt"
done
for r in 1 2 3; do
  HOMEWAY_TRANSPORT=quic "$AB" run --token "$TOKEN" --transport quic --force-direct \
    --quic-ep "127.0.0.1:$PROXY_PORT" \
    --tag "budget-island-r$r" --workdir "$ABDIR" --secs 6 --rate 500 --window 8 \
    --req-size 60 --reply-size 1252 > "$RES/budget-island-r$r.log" 2>&1 || rc=1
  grep -E "^m1ab\[" "$RES/budget-island-r$r.log" | grep -E "ready_ms|down\.pkt=|link=" | sed "s/^/budget-island r$r: /" >> "$RES/budget.txt"
done
grep -E "^budget" "$RES/budget.txt" | tail -20
kill "$PROXY_PID" 2>/dev/null; PROXY_PID=""

# ---------- G) S5-5 窄路径注入 ----------
echo "==> G) S5-5 窄路径：生产 env 旋钮（1320 正对照 / 1200 越界回落）+ 岛缝（migrate --mtu-cap 1200）"
# (a) 生产路径（facade）：`HOMEWAY_QUIC_MTU=1320` = 合法区间下限 ⇒ 客户端 mds 1282（>1280，无窄路径行）
HOMEWAY_QUIC_MTU=1320 HOMEWAY_TRANSPORT=quic "$AB" run --token "$TOKEN" --transport quic --force-direct \
  --tag "mtu-1320" --workdir "$ABDIR" --secs 5 --rate 500 --window 8 \
  --req-size 60 --reply-size 1252 > "$RES/narrow-env1320.log" 2>&1 || rc=1
# (b) 生产路径：`HOMEWAY_QUIC_MTU=1200` = 区间外 ⇒ **记行 + 按缺省 1400 走**（不夹取）
HOMEWAY_QUIC_MTU=1200 HOMEWAY_TRANSPORT=quic "$AB" run --token "$TOKEN" --transport quic --force-direct \
  --tag "mtu-1200" --workdir "$ABDIR" --secs 5 --rate 500 --window 8 \
  --req-size 60 --reply-size 1252 > "$RES/narrow-env1200.log" 2>&1 || rc=1
# (c) 岛缝（migrate 档直建 IslandConfig）：--mtu-cap 1200 ⇒ mds 1162 < 内层 1280
#     ⇒ 「窄路径不可用」行 + `超限` 计数（设计 S5-5 的断言面；生产区间产不出，见 §3）
"$AB" migrate --token "$TOKEN" --relay "127.0.0.1:$RELAY_PORT" --tag narrow-seam \
  --secs 10 --rate 200 --window 8 --migrate-after 8 --req-size 1200 --alt-bind "$LAN_IP:0" \
  --mtu-cap 1200 > "$RES/narrow-seam.log" 2>&1 || rc=1
{
  echo "-- (a) 生产 env HOMEWAY_QUIC_MTU=1320（合法下限；期望 mtu=1282、无窄路径行）"
  grep -E "窄路径不可用|MTU 上限|quic=\{|quic: 丢弃" "$RES/narrow-env1320.log" | head -4
  echo "-- (b) 生产 env HOMEWAY_QUIC_MTU=1200（越界；期望「非法或越界…按缺省 1400 走」记行 + mtu=1362）"
  grep -E "窄路径不可用|MTU 上限|quic=\{|quic: 丢弃" "$RES/narrow-env1200.log" | head -4
  echo "-- (c) 岛缝 --mtu-cap 1200（期望「窄路径不可用」+ 丢弃 超限=…）"
  grep -E "窄路径不可用|quic: 丢弃|quic=\{|路径变更" "$RES/narrow-seam.log" | head -8
} | tee "$RES/narrow-path.txt"

# ---------- I) 产品形态单连接内存（M1 S5-4/E 段的同口径复测；M2 §8 点名） ----------
# 方法照 M1：同一枚 m1-ab（= ClientCore 世代）跑 wg|quic，运行期 `vmmap` 8 次取下中位；
# wg 档不构造岛（地板），两档之差 = **岛边际**（判据 §9.1-3 修订：≤+320K；M1 未过）。
echo "==> I) 产品形态单连接内存（wg 地板 vs quic 岛在位；$ROUNDS 轮）"
: > "$RES/fp-product.txt"
for r in $(seq 1 $ROUNDS); do
  for tr in wg quic; do
    HOMEWAY_TRANSPORT=$tr "$AB" run --token "$TOKEN" --transport "$tr" --tag "fp-$tr-r$r" \
      --workdir "$ABDIR" --secs 26 --rate 20 --window 2 --req-size 60 --reply-size 1252 \
      > "$RES/ab-fp-$tr-r$r.log" 2>&1 &
    fp_pid=$!
    sleep 7
    vals=($(sample_fp "$fp_pid" 8))
    med=$(lower_med "${vals[@]}")
    wait $fp_pid 2>/dev/null || true
    printf 'fp r%s %-5s 产品形态 footprint = %sK（8 次下中位；逐次 %s）\n' \
      "$r" "$tr" "$med" "${(j:, :)vals}" >> "$RES/fp-product.txt"
    if [[ "$tr" == "wg" ]]; then FP_WG="$med"; else FP_QUIC="$med"; fi
  done
  if [[ -n "$FP_WG" && -n "$FP_QUIC" ]]; then
    printf 'fp r%s 岛边际（quic − wg，含 runtime/Endpoint 固定成本）= %sK（判据 ≤320K）\n' \
      "$r" "$(( FP_QUIC - FP_WG ))" >> "$RES/fp-product.txt"
  fi
done
cat "$RES/fp-product.txt"

# ---------- 收束：证据行 + SUMMARY ----------
tail -n +"$((LOG0 + 1))" "$EXIT_LOG" > "$RES/exit-lines.txt" 2>/dev/null || true
tail -n +"$((LOG2_0 + 1))" "$EXIT2_LOG" > "$RES/exit2-lines.txt" 2>/dev/null || true
tail -n +"$((LOG3_0 + 1))" "$EXIT3_LOG" > "$RES/exit3-lines.txt" 2>/dev/null || true
tail -n +"$((RELAY0 + 1))" "$RELAY_LOG" > "$RES/relay-lines.txt" 2>/dev/null || true

{
  echo "# M2 S5-2/S5-3/S5-5 产品路径读数（$(date '+%F %T')；rc=$rc）"
  echo "# 主出口 = $EXIT_STATE（wg=$((42650 + n)) quic=$QUIC_PORT）；独立出口 = $EXIT2_STATE（quic=$QUIC2_PORT，HOMEWAY_QUIC_ADMIT_RETRY=never）"
  echo "# 中继 = $RELAY_STATE（数据口 $RELAY_PORT）；延迟代理 = 127.0.0.1:$PROXY_PORT（单向 150ms）"
  echo
  echo "## A) S5-2 直连 A/B（同刻交替）"
  for r in $(seq 1 $ROUNDS); do
    for tr in quic wg; do
      echo "-- r$r $tr"
      grep -E "^m1ab\[" "$RES/ab-direct-$tr-r$r.log" 2>/dev/null | grep -E "up\.pkt|down\.pkt=|link=" || true
    done
  done
  echo "## A2) 容量档"
  for r in $(seq 1 $ROUNDS); do
    for tr in quic wg; do
      echo "-- r$r $tr"
      grep -E "^m1ab\[" "$RES/ab-cap-$tr-r$r.log" 2>/dev/null | grep -E "down\.pkt=|link=" || true
    done
  done
  echo
  echo "## B/C/D) 洪泛注入（判据 1/2）"
  cat "$RES/SUMMARY-flood.txt" 2>/dev/null || true
  echo "-- 出口#n 洪泛行（挑战/拒绝/超时/期限）"
  grep -E "握手洪泛拒绝|地址校验挑战|认证超时|拒新连接|握手期限" "$RES/exit-lines.txt" 2>/dev/null | head -30 || true
  echo "-- 独立出口#n2（never）洪泛行"
  grep -E "握手洪泛拒绝|地址校验挑战|认证超时|拒新连接|握手期限" "$RES/exit2-lines.txt" 2>/dev/null | head -12 || true
  echo "-- 独立出口#n3（每源闸抬到 1000）洪泛行"
  grep -E "拒新连接|握手期限|握手洪泛拒绝" "$RES/exit3-lines.txt" 2>/dev/null | head -12 || true
  echo
  echo "## E) 判据 3 不伤既有连接"
  cat "$RES/flood-impact-windows.txt" 2>/dev/null || true
  for r in $(seq 1 $ROUNDS); do
    echo "-- r$r 逐秒（sent=上行 acked=下行）"
    grep -E "sec=" "$RES/ab-flood-impact-r$r.log" 2>/dev/null || true
    grep -E "^m1ab\[" "$RES/ab-flood-impact-r$r.log" 2>/dev/null | grep -E "down\.pkt=|loss\.pct" || true
  done
  echo
  echo "## F) 300ms RTT 预算"
  cat "$RES/budget.txt" 2>/dev/null || true
  echo
  echo "## G) S5-5 窄路径"
  cat "$RES/narrow-path.txt" 2>/dev/null || true
  echo
  echo "## H) 出口 footprint（vmmap physical footprint）"
  cat "$RES/fp-exit.txt" 2>/dev/null || true
  echo "## I) 产品形态单连接内存（岛边际）"
  cat "$RES/fp-product.txt" 2>/dev/null || true
  echo "-- 常态零误伤核查（全轮出口行计数）"
  printf '地址校验挑战=%s 握手洪泛拒绝=%s 认证超时=%s 拒新连接=%s\n' \
    "$(grep -c '地址校验挑战' "$RES/exit-lines.txt" 2>/dev/null || echo 0)" \
    "$(grep -c '握手洪泛拒绝' "$RES/exit-lines.txt" 2>/dev/null || echo 0)" \
    "$(grep -c '认证超时' "$RES/exit-lines.txt" 2>/dev/null || echo 0)" \
    "$(grep -c '拒新连接' "$RES/exit-lines.txt" 2>/dev/null || echo 0)"
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt / ab-*.log / flood-*.log）rc=$rc"
echo "==> 停本地实例 #$n / #$n2（隔离纪律）"
"$REPO_ROOT/tools/local-rust-exit.sh" stop "$n" >/dev/null 2>&1
"$REPO_ROOT/tools/local-rust-exit.sh" stop "$n2" >/dev/null 2>&1
"$REPO_ROOT/tools/local-rust-exit.sh" stop "$n3" >/dev/null 2>&1
"$REPO_ROOT/tools/local-rust-relay.sh" stop "$n" >/dev/null 2>&1
exit $rc
