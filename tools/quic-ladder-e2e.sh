#!/bin/zsh
# quic-ladder-e2e.sh — M3 S4 **快探阶梯的故障注入 e2e**（设计 §3.1/§3.2/§3.3）。
#
# 做什么（**本地私有出口实例**，绝不碰现役；实例号缺省 2 —— 与 island/wg 脚本的 #1 错开）：
#   ①`cargo build --release -p homeway-cli`（工具坑：改核后不重建会跑到旧二进制）；
#   ②`local-rust-exit.sh wipe/start` 起一枚干净出口 ⇒ 取 token；
#   ③逐条跑 `crates/homeway-core/tests/quic_ladder_e2e.rs` 的四条 `#[ignore]` 用例：
#     · 相位 A：kill -9 出口 + **立刻**重启 ⇒ `T_recv ≤ 3.5s`（起点 = E1 `serve 就绪` 行）
#     · 相位 B：kill -9 + **停机 5s** ⇒ 同上（客户端必然先走完一次失败链动作）
#     · 负向①：瞬时黑洞 1.5s（穿楔子代理）⇒ 只记抖动、**不动作**
#     · 负向②：持续黑障（回显永不返回、连接不关）⇒ **必有动作** + 撤障后自愈
#   **每条用例前重新取 token**（kill 重启后出口可能铸新 token；读旧 token 会假红）。
#
# 用法：tools/quic-ladder-e2e.sh [实例号]（缺省 2）
# 读数：/tmp/m3s4-res/（SUMMARY.txt / 逐条 *.log / 出口日志切片）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-2}"
RES=/tmp/m3s4-res
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
EXIT_PIDFILE="$EXIT_STATE/pid"
WEDGE_CTRL_GLOB="/tmp/hw-wedge-*"

mkdir -p "$RES" || exit 1
echo "==> 清上一轮读数与楔子控制文件"
rm -f "$RES"/*.log "$RES"/SUMMARY.txt 2>/dev/null || true
rm -f ${~WEDGE_CTRL_GLOB} 2>/dev/null || true

echo "==> 构建 release 二进制（工具坑：改核后必须先 build）"
(cd "$REPO_ROOT" && cargo build --release -p homeway-cli) > "$RES/build.log" 2>&1 || {
  echo "!! release 构建失败（见 $RES/build.log）" >&2; exit 1; }
tail -2 "$RES/build.log"

echo "==> 起干净出口 #$n（wipe + start）"
"$REPO_ROOT/tools/local-rust-exit.sh" wipe "$n" > "$RES/exit-wipe.txt" 2>&1 || true
"$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
  echo "!! 出口起不来（见 $RES/exit-start.txt）" >&2; exit 1; }

rc=0
run_one() {
  local name="$1"
  # 每条前面重取 token（kill 重启后可能铸新 token）
  local tok
  tok="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw2[A-Za-z0-9_=+/-]+' | head -1)"
  if [[ -z "$tok" ]]; then
    echo "!! token 取不到" >&2; rc=1; return 1
  fi
  # 出口必须在跑（前一条用例可能刚重启过它）
  if ! "$REPO_ROOT/tools/local-rust-exit.sh" status "$n" 2>/dev/null | grep -q '^pid='; then
    echo "==> 出口不在跑 ⇒ 重新 start"
    "$REPO_ROOT/tools/local-rust-exit.sh" start "$n" >> "$RES/exit-start.txt" 2>&1 || {
      echo "!! 出口起重失败" >&2; rc=1; return 1; }
  fi
  echo "==> 跑用例 $name"
  ( cd "$REPO_ROOT" && \
    HOMEWAY_LADDER_TOKEN="$tok" \
    HOMEWAY_LADDER_EXIT_LOG="$EXIT_LOG" \
    HOMEWAY_LADDER_PIDFILE="$EXIT_PIDFILE" \
    HOMEWAY_LADDER_REPO="$REPO_ROOT" \
    HOMEWAY_LADDER_RESTART="/bin/zsh $REPO_ROOT/tools/local-rust-exit.sh start $n" \
    cargo test -p homeway-core --test quic_ladder_e2e "$name" -- --ignored --nocapture --test-threads=1 ) \
    > "$RES/$name.log" 2>&1
  local r=$?
  grep -E "^\[e2e-ladder\]|^test result" "$RES/$name.log" || true
  [[ $r -eq 0 ]] || { echo "!! 用例 $name 红（见 $RES/$name.log）" >&2; rc=1; }
  return $r
}

run_one kill9_then_immediate_restart_recovers_within_t_recv
run_one kill9_then_restart_after_five_seconds_recovers_within_t_recv
run_one transient_blackhole_window_only_jitters_and_takes_no_action
run_one wedge_without_close_must_escalate_to_an_action

{
  echo "# M3 S4 快探阶梯故障注入读数（$(date '+%F %T')；出口实例 = $EXIT_STATE）"
  echo "# 判据：T_recv ≤ 3500ms（起点 = E1 \`serve 就绪\` 行时刻 → 客户端首个回显成功）"
  echo "## 逐条关键读数"
  grep -hE "^\[e2e-ladder\]" "$RES"/*.log 2>/dev/null || true
  echo "## 岛侧判据行（全量，带本地时戳）"
  grep -hE "^\[ *[0-9]+\.[0-9]+s\] \[island\]" "$RES"/*.log 2>/dev/null || true
  echo "## 出口侧本轮相关行（末 60 条）"
  tail -60 "$EXIT_LOG" 2>/dev/null || true
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt + 逐条 *.log；rc=$rc）"
exit $rc
