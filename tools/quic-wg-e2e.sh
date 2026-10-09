#!/bin/zsh
# quic-wg-e2e.sh — **单承载失败路径**的本地全链证据（**本地私有出口，绝不碰现役实例**）。
#
# **M5 C3 改写**：原「`_wg` 档全链」（A/B 开关的价值面）随 WG 面删除而退役；本脚本改为
# 走**设计 §2.6-G9 的负例实测**——`--quic=false` 的出口发不出 QUIC 端点/RPK ⇒ 该 token
# 在单承载下**必然失败**，且失败必须**可见**（不得静默、不得回落）。
#
# 做什么：
#   ①起本地 Rust 出口（`EXIT_EXTRA_FLAGS=--quic=false`：QUIC 面**关闭**）；
#   ②取出口 token，断言 **QUIC 面确实没起**（无 `quic: 端点就绪`/`UPnP：QUIC` 行、
#     `cache/quic_listen_port.txt` 不存在）；
#   ③跑 `crates/homeway-core/tests/quic_wg_e2e.rs` 的 `#[ignore]` 用例：
#     · `wg_only_token_generation_fails_visibly_without_fallback`：世代日志必须出现
#       `quic: 岛未就用（…候选为空）`、状态面 `state=failed`、**零**回落/兜底话术、
#       **零** A/B 开关行；failed 后 `tun_stop` 即收 0（放锁）。
#   ④读数与证据行留到 /tmp（**仓外**，不污染工作树）。
#
# 用法：tools/quic-wg-e2e.sh [实例号]（缺省 1）
# 读数：/tmp/m5c3-g9-res/（SUMMARY.txt / g9-e2e.log / exit-lines.txt / quic-off-assert.txt）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-1}"
RES=/tmp/m5c3-g9-res
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
QUIC_PORT_FILE="$EXIT_STATE/cache/quic_listen_port.txt"

mkdir -p "$RES" || exit 1

# 断言前置：清掉上一轮（QUIC 面开启形态）留下的端口缓存——否则「文件不存在」是假阴性
rm -f "$QUIC_PORT_FILE"
# **起服之前**记日志基线：启动期的判据行（`quic: 端点就绪`/`quic: 面未启用`）只在这一段里
LOG0=$( [[ -f "$EXIT_LOG" ]] && wc -l < "$EXIT_LOG" | tr -d ' ' || print 0 )

echo "==> 起本地 Rust 出口 #$n（QUIC 面关闭：--quic=false）"
EXIT_EXTRA_FLAGS="--quic=false" "$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
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

# ---- ② QUIC 面关闭的出口侧证据（S3-4）----
RC_OFF=0
{
  echo "# serve.quic=false 形态断言（$(date '+%F %T')）"
  if [[ -f "$QUIC_PORT_FILE" ]]; then
    echo "!! 失败：cache/quic_listen_port.txt 存在（= QUIC 面起了）"
    RC_OFF=1
  else
    echo "OK  cache/quic_listen_port.txt 不存在（未监听 QUIC 端口）"
  fi
  # 只看**本轮**新增行（日志跨轮追加——全文件 grep 会把上一轮的 QUIC 面行算进来）
  if tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -q "quic: 端点就绪"; then
    echo "!! 失败：本轮出口日志出现 E-q1（quic: 端点就绪）"
    RC_OFF=1
  else
    echo "OK  本轮出口日志无 E-q1（quic: 端点就绪）"
  fi
  if tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -q "UPnP：QUIC"; then
    echo "!! 失败：本轮出口日志出现 E-q4（UPnP：QUIC …）"
    RC_OFF=1
  else
    echo "OK  本轮出口日志无 E-q4（UPnP：QUIC …）"
  fi
  if tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -q "quic: 面未启用"; then
    tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -m1 "quic: 面未启用"
  else
    echo "（无「quic: 面未启用」行——出口版本未带 S3-4？）"
  fi
} > "$RES/quic-off-assert.txt"
cat "$RES/quic-off-assert.txt"

rc=0
run_one() {
  local name="$1" out="$2"
  echo "==> 跑用例 $name"
  ( cd "$REPO_ROOT" && HOMEWAY_WG_E2E_TOKEN="$TOKEN" HOMEWAY_WG_E2E_EXIT_LOG="$EXIT_LOG" \
      cargo test -p homeway-core --test quic_wg_e2e "$name" -- --ignored --nocapture --test-threads=1 ) \
    > "$out" 2>&1
  local r=$?
  grep -E "^\[g9|^test result" "$out" || true
  return $r
}

run_one wg_only_token_generation_fails_visibly_without_fallback "$RES/g9-e2e.log" || rc=1

# 出口侧本轮新增行（含 `peer: +` / `intercept: tcp exempt …`）
tail -n +"$((LOG0 + 1))" "$EXIT_LOG" > "$RES/exit-lines.txt" 2>/dev/null || true

{
  echo "# M5 C3 G9 负例实测（$(date '+%F %T')）"
  echo "# 出口实例 = $EXIT_STATE（日志：$EXIT_LOG；**--quic=false** 形态）"
  echo "# 拓扑：ClientCore(tun) → 岛装配（候选=空）⇒ 可见失败（无回落承载）"
  echo "## serve.quic=false 断言（出口侧：QUIC 面确实没起）"
  cat "$RES/quic-off-assert.txt"
  echo "## 用例结论（run_one 汇总 exit code = $rc；0 = 过；QUIC 面关闭断言 rc=$RC_OFF）"
  grep -E "^\[g9|^test |^test result" "$RES/g9-e2e.log" 2>/dev/null || true
  echo "## 出口侧证据行（本轮新增）"
  grep -E "peer: \+|intercept: tcp|udp intercept|serve 就绪|quic: 面未启用" "$RES/exit-lines.txt" 2>/dev/null | head -20 || true
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt / g9-e2e.log / exit-lines.txt / quic-off-assert.txt）"
cat "$RES/SUMMARY.txt"
[[ $RC_OFF -eq 0 ]] || exit 1
exit $rc
