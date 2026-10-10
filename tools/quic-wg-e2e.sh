#!/bin/zsh
# quic-wg-e2e.sh — **单承载（QUIC-only）端点面**的本地全链证据（**本地私有出口，绝不碰现役实例**）。
#
# **M5 C4 改写**：原「`_wg` 档全链」/「`--quic=false` 缺失路径」两面随 WG 面与承载开关删除
# 而退役（`serve.quic` 键已不存在，出口再也产不出「无 QUIC 端点」的 token）。现脚本走两半：
#   ①**正向**：真出口铸出的 token = 单承载 QUIC（带 `rpk`、端点全为 `Quic` 类、`Direct` 零命中）
#     + 出口侧 QUIC 面确实起了（`cache/quic_listen_port.txt` 在 + E-q1/E-q4 行在场）；
#   ②**负向**（设计 §2.6-G9）：**存量 WG-only token**（无 `rpk`、只带 `Direct` 端点）⇒
#     世代**可见失败**（`岛未就用（…）` + `state=failed` + 零回落话术 + 放锁）——
#     该 token 由用例内铸造（出口已无法产出该形态），故这一半是 `cargo test` 常跑用例。
#   ②b**负向（M7 S1）**：**上一代 `hmw1` token** ⇒ 解析面**点名归因**（哨兵原文
#     `homeway/token: 不支持的 token 版本: hmw1` + 换代归因「存量 token 已失效…重新粘贴」）
#     ——核面（status reason）与 CLI 面（`host add`）两处都断言；两条都无需出口。
#
# 做什么：
#   ①起本地 Rust 出口（正常形态）；
#   ②取出口 token，断言 QUIC 面**起了**（`cache/quic_listen_port.txt` 存在 + E-q1 `quic: 端点就绪`）；
#   ③跑 `crates/homeway-core/tests/quic_wg_e2e.rs`：
#     · `exit_token_is_single_bearer_quic_only`（`--ignored`：需出口 token）；
#     · `wg_only_token_generation_fails_visibly_without_fallback`（常跑；G9 负例，无需出口）；
#     · `legacy_hmw1_token_surfaces_actionable_changeover_attribution`（常跑；**M7 S1** 负例）。
#   ③b CLI 面断言（M7 S1）：`host add hmw1…` ⇒ rc=1 + stderr 带换代归因（本地语法校验前置，
#     不连 daemon、不入表 ⇒ 零副作用）。
#   ④读数与证据行留到 /tmp（**仓外**，不污染工作树）。
#
# 用法：tools/quic-wg-e2e.sh [实例号]（缺省 1）
# 读数：/tmp/m5c4-token-res/（SUMMARY.txt / token-e2e.log / g9-e2e.log / s1-e2e.log /
#      s1-cli-assert.txt / exit-lines.txt / quic-on-assert.txt）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-1}"
RES=/tmp/m5c4-token-res
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
LISTEN_PORT_FILE="$EXIT_STATE/cache/listen_port.txt"
QUIC_PORT_FILE="$EXIT_STATE/cache/quic_listen_port.txt"

mkdir -p "$RES" || exit 1

# **先停**（上一个 e2e 可能还在跑同一实例）：否则下面的端口缓存清理 + E-q1 断言都指不到
# 本轮日志/本轮文件（C4 实测的假红形态）
"$REPO_ROOT/tools/local-rust-exit.sh" stop "$n" >/dev/null 2>&1 || true

# 断言前置：清掉上一轮的端口缓存——否则「文件存在」是假阳性（M5：两个文件同值 = 公共端口）
rm -f "$QUIC_PORT_FILE" "$LISTEN_PORT_FILE"
# **起服之前**记日志基线：启动期的判据行（`quic: 端点就绪`）只在这一段里
LOG0=$( [[ -f "$EXIT_LOG" ]] && wc -l < "$EXIT_LOG" | tr -d ' ' || print 0 )

echo "==> 起本地 Rust 出口 #$n（单承载形态）"
"$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
  echo "!! 出口起不来（见 $RES/exit-start.txt）" >&2; exit 1; }

echo "==> 取 token（serve token 输出里抽 hmw2…）"
TOKEN="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw[0-9][A-Za-z0-9_=+/-]+' | head -1)"
if [[ -z "$TOKEN" ]]; then
  echo "!! token 取不到（serve token 输出：）" >&2
  "$REPO_ROOT/tools/local-rust-exit.sh" token "$n" | head -5 >&2
  exit 1
fi
print -r -- "$TOKEN" > "$RES/token.txt"
echo "    token 长度 = ${#TOKEN}（内容落 $RES/token.txt）"

# ---- ② 出口侧：公共端口（QUIC）确实起了 ----
RC_ON=0
{
  echo "# 单承载出口形态断言（$(date '+%F %T')）"
  if [[ -f "$QUIC_PORT_FILE" && -f "$LISTEN_PORT_FILE" ]]; then
    echo "OK  cache/quic_listen_port.txt = $(cat "$QUIC_PORT_FILE")；listen_port.txt = $(cat "$LISTEN_PORT_FILE")（M5 同值 = 唯一公共端口）"
  else
    echo "!! 失败：公共端口落盘文件缺失（QUIC 面未起？）"
    RC_ON=1
  fi
  if tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -q "端点就绪"; then
    tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -m1 "端点就绪"
  else
    echo "!! 失败：本轮出口日志无 E-q1（端点就绪）"
    RC_ON=1
  fi
  # M5：E1 的端口字段 = quic（`wg=` 字段退役）
  tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -m1 "serve 就绪" || true
} > "$RES/quic-on-assert.txt"
cat "$RES/quic-on-assert.txt"

rc=0
run_one() {
  local name="$1" out="$2" ignored="$3"
  echo "==> 跑用例 $name"
  local extra=()
  [[ "$ignored" == "ignored" ]] && extra=(--ignored)
  ( cd "$REPO_ROOT" && HOMEWAY_WG_E2E_TOKEN="$TOKEN" HOMEWAY_WG_E2E_EXIT_LOG="$EXIT_LOG" \
      cargo test -p homeway-core --test quic_wg_e2e "$name" -- "${extra[@]}" --nocapture --test-threads=1 ) \
    > "$out" 2>&1
  local r=$?
  grep -E "^\[g9|^\[s1|^\[token|^test result" "$out" || true
  return $r
}

run_one exit_token_is_single_bearer_quic_only "$RES/token-e2e.log" ignored || rc=1
run_one wg_only_token_generation_fails_visibly_without_fallback "$RES/g9-e2e.log" "" || rc=1
# M7 S1：上一代 `hmw1` 串 ⇒ 点名归因（核面 status reason；无需出口）
run_one legacy_hmw1_token_surfaces_actionable_changeover_attribution "$RES/s1-e2e.log" "" || rc=1

# ---- ③b M7 S1 的 CLI 面（`host add` 本地语法校验前置：不连 daemon、不入表）----
S1_CLI_RC=0
{
  echo "# M7 S1 CLI 面断言（$(date '+%F %T')）：存量 hmw1 串必须点名归因"
  CLI_BIN="${HOMEWAY_BIN:-$REPO_ROOT/target/release/homeway-cli}"
  legacy="hmw1iu50C3IgUoXEPLRuz7_lQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
  S1_CLI_OUT=$("$CLI_BIN" host add "$legacy" 2>&1)
  S1_CLI_CMD_RC=$?
  echo "rc=$S1_CLI_CMD_RC（期望 1）"
  echo "输出：$S1_CLI_OUT"
  if [[ $S1_CLI_CMD_RC -ne 1 ]]; then
    echo "!! 失败：期望 rc=1（本地语法校验前置）"; S1_CLI_RC=1
  fi
  for needle in "不支持的 token 版本: hmw1" "存量 token 已失效" "serve token" "重新粘贴"; do
    if [[ "$S1_CLI_OUT" == *"$needle"* ]]; then
      echo "OK  含 $needle"
    else
      echo "!! 失败：缺 $needle"; S1_CLI_RC=1
    fi
  done
} > "$RES/s1-cli-assert.txt" 2>&1
cat "$RES/s1-cli-assert.txt"
[[ $S1_CLI_RC -eq 0 ]] || rc=1

# 出口侧本轮新增行（含 `peer: +` / `quic:` 面）
tail -n +"$((LOG0 + 1))" "$EXIT_LOG" > "$RES/exit-lines.txt" 2>/dev/null || true

{
  echo "# M5 C4 单承载端点面读数（$(date '+%F %T')）"
  echo "# 出口实例 = $EXIT_STATE（日志：$EXIT_LOG）"
  echo "# 拓扑：①真出口 token = QUIC-only（正向）；②存量 WG-only token ⇒ 可见失败（G9 负向）；"
  echo "#       ③存量 hmw1 token ⇒ 点名归因（M7 S1；核面 + CLI 面）"
  echo "## 出口侧断言（公共端口在 + E-q1 + E1 行）"
  cat "$RES/quic-on-assert.txt"
  echo "## 用例结论（汇总 exit code = $rc；0 = 三条都过 + S1 CLI 面过；出口侧断言 rc=$RC_ON）"
  grep -E "^\[g9|^\[s1|^\[token|^test |^test result" "$RES/token-e2e.log" "$RES/g9-e2e.log" "$RES/s1-e2e.log" 2>/dev/null || true
  echo "## M7 S1 CLI 面"
  cat "$RES/s1-cli-assert.txt" 2>/dev/null || true
  echo "## 出口侧证据行（本轮新增）"
  grep -E "peer: \+|端点就绪|流面参数|serve 就绪|中继" "$RES/exit-lines.txt" 2>/dev/null | head -20 || true
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt / token-e2e.log / g9-e2e.log / exit-lines.txt / quic-on-assert.txt）"
cat "$RES/SUMMARY.txt"
[[ $RC_ON -eq 0 ]] || exit 1
exit $rc
