#!/bin/zsh
# quic-island-e2e.sh — M1 S2a 的岛侧端到端烟囱（**本地私有出口**，绝不碰现役出口）。
#
# 做什么：①起本地 Rust 出口（`local-rust-exit.sh start`，隔离条款同源）；②取它的 token；
# ③跑 `crates/homeway-core/tests/quic_island_e2e.rs` 的 `#[ignore]` 用例（岛连上 → 登记 →
# 出口 `peer: +` → rebind 迁移后仍通）；④把读数与证据行留到 /tmp（**仓外**，不污染工作树）。
#
# 用法：tools/quic-island-e2e.sh [实例号]（缺省 1）
# 读数：/tmp/m1s2a-res/（island-e2e.log / exit-lines.txt / SUMMARY.txt）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-1}"
RES=/tmp/m1s2a-res
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"

mkdir -p "$RES" || exit 1

echo "==> 起本地 Rust 出口 #$n（tools/local-rust-exit.sh start）"
"$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
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

echo "==> 跑岛侧端到端用例（#[ignore]，--nocapture）"
( cd "$REPO_ROOT" && HOMEWAY_ISLAND_E2E_TOKEN="$TOKEN" HOMEWAY_ISLAND_E2E_EXIT_LOG="$EXIT_LOG" \
    cargo test -p homeway-core --test quic_island_e2e -- --ignored --nocapture ) \
  > "$RES/island-e2e.log" 2>&1
rc=$?

# 出口侧本轮新增行（含 `peer: +` / `quic: 连接采纳` / `quic: 路径变更`）
tail -n +"$((LOG0 + 1))" "$EXIT_LOG" > "$RES/exit-lines.txt"

{
  echo "# M1 S2a 岛侧端到端读数（$(date '+%F %T')）"
  echo "# 出口实例 = $EXIT_STATE（日志：$EXIT_LOG）"
  echo "## 用例结论（cargo test exit code = $rc）"
  grep -E "^\[e2e\]|^test |^test result" "$RES/island-e2e.log" || true
  echo "## 出口侧证据行（本轮新增）"
  grep -E "peer: \+|quic: 连接采纳|quic: 路径变更|quic: 端点就绪" "$RES/exit-lines.txt" || true
  echo "## 岛侧判据行"
  grep -E "quic: " "$RES/island-e2e.log" | grep -v "^\[e2e\]" | head -40 || true
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt / island-e2e.log / exit-lines.txt）"
cat "$RES/SUMMARY.txt"
exit $rc
