#!/bin/zsh
# quic-island-e2e.sh — M1 岛侧端到端烟囱（**本地私有出口 + 本地私有中继**，绝不碰现役实例）。
#
# 做什么：
#   ①起本地 Rust 中继（`local-rust-relay.sh start`）并取它的 rl1 token；
#   ②起本地 Rust 出口并**挂在该中继上**（`EXIT_EXTRA_FLAGS=--relay <rl1…>`）；
#   ③取出口 token（含 wg/quic/relay 三类端点）；
#   ④跑 `crates/homeway-core/tests/quic_island_e2e.rs` 的两条 `#[ignore]` 用例（**串行**）：
#     · S2a：岛连上 → 登记 → 出口 `peer: +` → rebind 迁移后仍通；
#     · S2b：**只给中继候选** ⇒ 信封路径全链 + 数据面双向（TUN fd ⇄ DATAGRAM，经中继）；
#   ⑤读数与证据行留到 /tmp（**仓外**，不污染工作树）。
#
# 用法：tools/quic-island-e2e.sh [实例号]（缺省 1）
# 读数：/tmp/m1s2b-res/（SUMMARY.txt / island-e2e.log / relay-e2e.log / exit-lines.txt / …）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-1}"
RES=/tmp/m1s2b-res
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
RELAY_STATE="/tmp/homeway-rs-rustrelay-$n"
RELAY_LOG="$RELAY_STATE/cache/relay.log"

mkdir -p "$RES" || exit 1

echo "==> 起本地 Rust 中继 #$n（tools/local-rust-relay.sh start）"
"$REPO_ROOT/tools/local-rust-relay.sh" start "$n" > "$RES/relay-start.txt" 2>&1 || {
  echo "!! 中继起不来（见 $RES/relay-start.txt）" >&2; exit 1; }
RL1="$("$REPO_ROOT/tools/local-rust-relay.sh" token "$n" 2>/dev/null | grep -oE 'rl1[A-Za-z0-9_=+/-]+' | head -1)"
if [[ -z "$RL1" ]]; then
  echo "!! rl1 token 取不到" >&2; exit 1
fi
print -r -- "$RL1" > "$RES/rl1.txt"

echo "==> 起本地 Rust 出口 #$n（挂中继：--relay rl1…）"
EXIT_EXTRA_FLAGS="--relay $RL1" "$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
  echo "!! 出口起不来（见 $RES/exit-start.txt）" >&2; exit 1; }

echo "==> 取 token（serve token 输出里抽 hmw1…）"
TOKEN="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
if [[ -z "$TOKEN" ]]; then
  echo "!! token 取不到（serve token 输出：）" >&2
  "$REPO_ROOT/tools/local-rust-exit.sh" token "$n" | head -5 >&2
  exit 1
fi
print -r -- "$TOKEN" > "$RES/token.txt"
echo "    token 长度 = ${#TOKEN}（内容落 $RES/token.txt）"

LOG0=$(wc -l < "$EXIT_LOG" | tr -d ' ')
RELAY_LOG0=0
[[ -f "$RELAY_LOG" ]] && RELAY_LOG0=$(wc -l < "$RELAY_LOG" | tr -d ' ')

rc=0
run_one() {
  local name="$1" out="$2"
  echo "==> 跑用例 $name"
  ( cd "$REPO_ROOT" && HOMEWAY_ISLAND_E2E_TOKEN="$TOKEN" HOMEWAY_ISLAND_E2E_EXIT_LOG="$EXIT_LOG" \
      cargo test -p homeway-core --test quic_island_e2e "$name" -- --ignored --nocapture --test-threads=1 ) \
    > "$out" 2>&1
  local r=$?
  grep -E "^\[e2e|^test result" "$out" || true
  return $r
}

run_one island_connects_registers_and_survives_rebind_against_local_exit "$RES/island-e2e.log" || rc=1
run_one island_uses_relay_and_pushes_traffic_through_tun "$RES/relay-e2e.log" || rc=1
# M1 S3-1：**世代级**（真产品路径）——ClientCore prepare/attach + TUN 流量经 QUIC DATAGRAM
run_one generation_l3_rides_quic_datagram_against_local_exit "$RES/generation-e2e.log" || rc=1

# 出口侧本轮新增行（含 `peer: +` / `quic: 连接采纳` / `quic: 路径变更`）
tail -n +"$((LOG0 + 1))" "$EXIT_LOG" > "$RES/exit-lines.txt" 2>/dev/null || true
if [[ -f "$RELAY_LOG" ]]; then
  tail -n +"$((RELAY_LOG0 + 1))" "$RELAY_LOG" > "$RES/relay-lines.txt" 2>/dev/null || true
fi

{
  echo "# M1 S2b/S3-1 岛侧端到端读数（$(date '+%F %T')）"
  echo "# 出口实例 = $EXIT_STATE（日志：$EXIT_LOG）；中继实例 = $RELAY_STATE（日志：$RELAY_LOG）"
  echo "# 拓扑：岛(quic 客户端) → Rust 中继 127.0.0.1:$((42780 + n)) → Rust 出口（挂中继腿）"
  echo "## 用例结论（run_one 汇总 exit code = $rc；0 = 三条都过）"
  grep -E "^\[e2e|^test |^test result" "$RES/island-e2e.log" "$RES/relay-e2e.log" 2>/dev/null || true
  echo "## 出口侧证据行（本轮新增）"
  grep -E "peer: \+|quic: 连接采纳|quic: 路径变更|quic: 端点就绪|中继控制面|transit" "$RES/exit-lines.txt" 2>/dev/null || true
  echo "## 中继侧行（本轮新增；腿/会话/丢弃）"
  grep -E "中继|会话|腿|丢弃|转发" "$RES/relay-lines.txt" 2>/dev/null | tail -20 || true
  echo "## 岛侧判据行（S2a + S2b）"
  grep -hE "quic: |island\]" "$RES/island-e2e.log" "$RES/relay-e2e.log" "$RES/generation-e2e.log" 2>/dev/null | grep -v "^\[e2e" | head -60 || true
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt / island-e2e.log / relay-e2e.log / exit-lines.txt / relay-lines.txt）"
cat "$RES/SUMMARY.txt"
exit $rc
