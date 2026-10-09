#!/bin/zsh
# quic-pf-e2e.sh — M4 S5：**portfwd（STREAM[dial]）端到端 + 四形态归因 + spec 本机核验**驱动。
#
# 做什么（**本地私有出口**，绝不碰现役实例；形态照 `tools/quic-island-e2e.sh` 先例）：
#   ①`wipe` + 起本地 Rust 出口 #n；
#   ②跑 `crates/homeway-core/tests/quic_pf_e2e.rs` 的 **5 条 `#[ignore]` 用例**（串行）：
#     · port_forward_rides_stream_dial_against_local_exit（R1-S①：真监听 + 字节往返 + 负判据）
#     · stream_dial_failures_do_not_leak_island_stream_slots（S2 泄漏判据 N=80）
#     · recover_downpush_on_island_satisfies_falsify_metrics（**S4**：NAPI 分档 falsify 三指标）
#     · port_forward_target_forms_and_failure_attribution（**S5**：四形态 × 归因 + R1①②行为断言）
#     · port_forward_dial_timeout_reports_0x26_with_exit_line（**S5**：`0x26` 真 socket 面）
#   ③**spec 不回退核验的本机面**（R1–R5）：结构性 grep 断言 + 值域单测（见下 §spec）；
#   ④读数与证据行落 `/tmp/m4s5-res/`（**仓外**）。
#
# 用法：tools/quic-pf-e2e.sh [实例号]（缺省 1）
# 读数：/tmp/m4s5-res/{SUMMARY.txt, pf-*.log, spec-*.txt, exit-lines.txt}
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
n="${1:-1}"
RES=/tmp/m4s5-res
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
EXIT_LOG="$EXIT_STATE/stdout.log"
SRC="$REPO_ROOT/crates/homeway-core/src"

mkdir -p "$RES" || exit 1

echo "==> 环境体检（可选）"
echo "    仓根 = $REPO_ROOT；出口实例 = $EXIT_STATE；读数落 $RES"

echo "==> 本地私有出口 #$n：wipe + start（绝不碰现役）"
"$REPO_ROOT/tools/local-rust-exit.sh" wipe "$n" > "$RES/exit-wipe.txt" 2>&1 || true
"$REPO_ROOT/tools/local-rust-exit.sh" start "$n" > "$RES/exit-start.txt" 2>&1 || {
  echo "!! 出口起不来（见 $RES/exit-start.txt）" >&2; exit 1; }

echo "==> 取 token（serve token 输出里抽 hmw1…）"
TOKEN="$("$REPO_ROOT/tools/local-rust-exit.sh" token "$n" 2>/dev/null | grep -oE 'hmw1[A-Za-z0-9_=+/-]+' | head -1)"
if [[ -z "$TOKEN" ]]; then
  echo "!! token 取不到" >&2
  "$REPO_ROOT/tools/local-rust-exit.sh" token "$n" | head -5 >&2
  exit 1
fi
print -r -- "$TOKEN" > "$RES/token.txt"
echo "    token 长度 = ${#TOKEN}（内容落 $RES/token.txt）"

LOG0=$(wc -l < "$EXIT_LOG" | tr -d ' ')

# ---------------------------------------------------------------------------
# ①–② e2e 五条用例（串行）
# ---------------------------------------------------------------------------
rc=0
run_one() {
  local name="$1" out="$2"
  echo "==> 跑用例 $name"
  ( cd "$REPO_ROOT" && HOMEWAY_ISLAND_E2E_TOKEN="$TOKEN" HOMEWAY_ISLAND_E2E_EXIT_LOG="$EXIT_LOG" \
      cargo test -p homeway-core --test quic_pf_e2e "$name" -- --ignored --nocapture --test-threads=1 ) \
    > "$out" 2>&1
  local r=$?
  grep -E "^\[pf-e2e|^test result" "$out" || true
  return $r
}

# **顺序是判据的一部分**（出口行的节流 = 既有语义 `log_due(n) = n≤3 ∨ n%100=0`，M4 零改动；
# 代码门 r21 F11 订正：**两个独立计数窗**——受理/结束行共用 `streams_open` 计数、拒行共用
# `stream_refused` 计数，各自「首 3 + 每 100」，且**跨 tag 共享**（dial 与 tag1–3/probe 同窗））
# ⇒ 断言出口行的用例必须落在**新起出口**的头三次额度内 ⇒ 顺序 =
# ride（受理行 #≤3）→ 0x26（拒绝 #1）→ forms（拒绝 #2/#3）→ leak（只断言客户端行）→ s4。
# 单独重跑某条断言出口行的用例时须先 `local-rust-exit.sh wipe n && start n`（本脚本已保证）。
run_one port_forward_rides_stream_dial_against_local_exit              "$RES/pf-ride.log"      || rc=1
run_one port_forward_dial_timeout_reports_0x26_with_exit_line          "$RES/pf-0x26.log"      || rc=1
run_one port_forward_target_forms_and_failure_attribution              "$RES/pf-forms.log"     || rc=1
run_one stream_dial_failures_do_not_leak_island_stream_slots           "$RES/pf-leak.log"      || rc=1
run_one recover_downpush_on_island_satisfies_falsify_metrics           "$RES/pf-s4-recover.log" || rc=1

# 出口侧本轮新增行
tail -n +"$((LOG0 + 1))" "$EXIT_LOG" > "$RES/exit-lines.txt" 2>/dev/null || true

# ---------------------------------------------------------------------------
# ③ spec 不回退核验（本机面）：R1–R5 的结构性 grep 断言 + 值域单测
#
# 口径（设计 §6）：①正文 SHALL 条款 ②scenario ③结构性/零触碰声明。本段只做**能钉住的**：
# R1①「仅回环」/ R1②「不依赖 TUN 路由」/ R1③「绕过名单」= 代码面 grep（消费点为零）；
# R5 = 全仓 `forward-via-proxy` 零命中（vacuous 达标）；R2 = 核侧无持久化面（grep 零命中）。
# R3/R4 的**行为**面由 §①–② 的用例与单测承担（本段只跑单测并落读数）。
# ---------------------------------------------------------------------------
spec_out="$RES/spec-checks.txt"
: > "$spec_out"
spec() {  # spec <id> <结论描述> <命令...>
  local id="$1" desc="$2"; shift 2
  local out; out=$("$@" 2>&1)
  local r=$?
  if (( r == 0 )); then
    print -r -- "[spec $id] PASS  $desc" >> "$spec_out"
  else
    print -r -- "[spec $id] FAIL  $desc（rc=$r）" >> "$spec_out"
    rc=1
  fi
  print -r -- "          命令：$*" >> "$spec_out"
  [[ -n "$out" ]] && print -r -- "          读数：$(print -r -- "$out" | head -3 | tr '\n' ' ')" >> "$spec_out"
  return 0
}

echo "==> spec 本机核验：结构性 grep + 值域单测"
# R1①「在手机上监听 127.0.0.1:<listen>（仅回环，不暴露局域网）」= 结构性（pf_bind 硬编码回环）。
# 断言面 = **函数体**（不误伤注释/其它函数里的字面量）：体内须有 LOCALHOST 且无 ANY。
spec "R1-①" "pf_bind 函数体硬编码回环（含 LOCALHOST、无 INADDR_ANY/0.0.0.0）" \
  zsh -c "a=\$(awk '/fn pf_bind/,/^}/' '$SRC/facade/portfwd.rs'); print -r -- \"\$a\" | grep -q 'Ipv4Addr::LOCALHOST' && ! print -r -- \"\$a\" | grep -qE 'INADDR_ANY|0\.0\.0\.0'"
# R1②「不依赖 TUN 路由」= 结构性（pf 拨号走 QUIC 流面/裸拨，不查路由表；QUIC 档连 stackb 都不经）
spec "R1-②" "pf 拨号缝零 WG 面引用（隔离门第 ⑪ 条同款断言）" \
  "$REPO_ROOT/tools/check-quic-isolation.sh"
spec "R1-②'" "客户端拨号缝只经 quic_stream::dial_target / session_connect_target 两腿" \
  zsh -c "grep -q 'fn pf_dial_via_run' '$SRC/facade/tun_exec.rs' && grep -q 'dial_target' '$SRC/facade/tun_exec.rs'"
# R1③「不受应用绕过名单影响」= 结构性：**全核非测试代码里 `bypass` 零命中**（没有该配置面 ⇒
# pf 路径无从消费；测试名里的 bypass 由 awk 在 `#[cfg(test)]` 处截断排除）
# 注：desc 串**不得用反引号**（双引号内会触发命令替换，落纸描述残缺——代码门 r21 F8）。
spec "R1-③" "全核（core+quic）非测试代码零 bypass（无该配置面）" \
  zsh -c "for f in \$(grep -rl bypass '$REPO_ROOT/crates/homeway-core/src' '$REPO_ROOT/crates/homeway-quic/src' --include='*.rs' 2>/dev/null); do if awk '/^#\[cfg\(test\)\]/{exit} {print}' \"\$f\" | grep -qi bypass; then exit 1; fi; done; exit 0"
# R2「持久化/级联删除/重连生效在 tier」= 核侧无持久化面（HostStore 在 tier；核侧只有装表接口 + rc）
spec "R2" "核侧**代码面**零 HostStore/hosts.json（注释引用不算）" \
  zsh -c "! grep -rnE 'HostStore|hosts\.json' '$SRC/facade/portfwd.rs' '$SRC/facade/mod.rs' | grep -vE ':[0-9]+:[[:space:]]*(//|/\*|\*)'"
# R5「出口侧回环目标不经代理」= vacuous 达标（前提「出口配了转发代理」在 Rust 出口不存在）
spec "R5" "实现面（crates/）零 forward-via-proxy（vacuous 达标；旧栈已退役）" \
  zsh -c "! grep -rq 'forward-via-proxy' '$REPO_ROOT/crates' 2>/dev/null"
# R3/R4 的**行为面**：值域单测（本机可验的那半）
spec "R3/R4" "portfwd 值域/状态面单测全绿（validate_table + 逐条状态 + code 真值）" \
  zsh -c "cd '$REPO_ROOT' && cargo test -q -p homeway-core --lib facade::portfwd 2>&1 | tail -3 | grep -q 'test result: ok'"
spec "R3/code" "词表门 PASS（portfwd/err 单元的 code 值域零改动）" \
  "$REPO_ROOT/tools/check-vocab.sh"

{
  echo "# M4 S5 portfwd 端到端读数（$(date '+%F %T')）"
  echo "# 出口实例 = $EXIT_STATE（日志：$EXIT_LOG）；拓扑 = 本机 ← pf(127.0.0.1:L) ← 岛 ← QUIC 流 ← Rust 出口 → 真 OS 拨号"
  echo
  echo "## 用例结论（run_one 汇总 exit code = $rc；0 = 五条都过）"
  grep -hE "^\[pf-e2e|^test .*(ok|FAILED)|^test result" \
    "$RES/pf-ride.log" "$RES/pf-leak.log" "$RES/pf-s4-recover.log" "$RES/pf-forms.log" "$RES/pf-0x26.log" 2>/dev/null || true
  echo
  echo "## S4 falsify 三指标（预登记：误判率 ≤1/5、零 RECOVER 行、耗时 ≤8s）"
  grep -hE "s4" "$RES/pf-s4-recover.log" 2>/dev/null || true
  echo
  echo "## 四形态 × 归因（S5：行文 + rc 逐格）"
  grep -hE "forms" "$RES/pf-forms.log" 2>/dev/null || true
  echo
  echo "## 0x26 真面（出口 10s 预算到点）"
  grep -hE "0x26" "$RES/pf-0x26.log" 2>/dev/null || true
  echo
  echo "## 泄漏判据（N=80；第 80 次拒因须为「目标拒绝」而非「入口队列满」）"
  grep -hE "leak" "$RES/pf-leak.log" 2>/dev/null || true
  echo
  echo "## spec 本机核验（R1–R5 结构性/值域面）"
  cat "$spec_out"
  echo
  echo "## R5 前提面的透明读数（`forward-via-proxy` 全仓命中处；实现面 = 0）"
  grep -rn "forward-via-proxy" "$REPO_ROOT/crates" "$REPO_ROOT/tools" "$REPO_ROOT/docs" 2>/dev/null | sed 's/:.*//' | sort | uniq -c || true
  echo
  echo "## 出口侧本轮新增行（tag=dial 族 / 归因行）"
  grep -E "tag=dial|目标拨号|目标地址类不可拨|目标帧未读出|服务流拒|目标侧中断" "$RES/exit-lines.txt" 2>/dev/null | head -40 || true
} > "$RES/SUMMARY.txt"

echo "==> 读数落 $RES/（SUMMARY.txt / pf-*.log / spec-checks.txt / exit-lines.txt）"
cat "$RES/SUMMARY.txt"
exit $rc
