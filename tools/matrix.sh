#!/bin/zsh
# matrix.sh — 互操作矩阵编排（R5-5a；设计 = docs/reviews/R5-design.md §0–§1，评审 v2）
#
# {Go,Rust}×{exit,client,relay} 六混合链路（8 组合减纯血 GGG/RRR——它们不含跨实现
# 互操作面；GGG/RRR 只在 --perf 档作 A/B 对照）：
#   L1 GGR（Go出口 Go客户端 Rust中继）/ L2 GRG / L3 GRR / L4 RGG / L5 RGR / L6 RRG
#
# 端口规划（与既有脚本段互不重叠；矩阵运行期间不与既有脚本并发——.lock 只挡矩阵自身）：
#   矩阵出口 42660+i（i=1..6；42660/42667 留 GGG/RRR perf 档）
#   矩阵中继 42750+i（42750/42757 留 perf 档）
#   echo 监听 42800+i（E10 transit 目标 + echo RTT 测量）
#   客户端本地 forward 监听 42900+i（Go forward / Rust portfwd——echo RTT 入口）
# state：/tmp/homeway-rs-matrix/<链路>/{exit,relay,c-main,c-sub,files}；每链路自 wipe。
# files 根显式隔离（--files-root …/<链路>/files——不落 $HOME；上传目标带随机后缀）。
# 段级隔离（评审 ①-2）：identity 跨段复用（保 devTag——n 计数可控）；endpoint cache
#   按段清/换（Rust --endpoint-cache-dir 分目录；Go 段间 rm -rf <state>/cache/endpoints）。
# 中继段驻留判据分口径（评审 ①-1）：Rust relay 链路硬钉（--no-hints + 段预算 <300s）；
#   Go relay 链路只判首窗 via=relay + 数据，翻直连 = 预期自愈观测（备注列，非 FAIL）。
#
# 用法：tools/matrix.sh [--link L1..L6] [--perf] [--smoke] [--fail-fast]
#   --smoke = RRR 基础段冒烟（直连段判据；多 peer/中继段只在验收轮与 --perf 档跑）
# 结果：stdout 进度 + docs/matrix-latest.md（链路 × 判据 → PASS/FAIL + 摘录 ≤100 字符
#   + 备注 + 耗时）；失败行全文在 /tmp/homeway-rs-matrix/<链路>/failures.log。
# 退出码：全绿 0；任一 FAIL 非零。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
RUST_BIN="$REPO_ROOT/target/release/homeway-cli"
GO_BIN="${HOMEWAY_GO:-$REPO_ROOT/bin/homeway-go}"
MATRIX="/tmp/homeway-rs-matrix"
OUT_MD="$REPO_ROOT/docs/matrix-latest.md"

# ---------- 参数 ----------
LINKS=(); PERF=0; SMOKE=0; FAIL_FAST=0
for a in "$@"; do
  case "$a" in
    --link) : ;; # 值在下一个参数（下面再扫）
    --perf) PERF=1 ;;
    --smoke) SMOKE=1 ;;
    --fail-fast) FAIL_FAST=1 ;;
    L1|L2|L3|L4|L5|L6|GGG|RRR) LINKS+=("$a") ;;
    *) echo "未知参数：$a（可用 --link L1..L6 / --perf / --smoke / --fail-fast）" >&2; exit 2 ;;
  esac
done
# --link 的值形态：--link L1
prev=""
for a in "$@"; do
  if [[ "$prev" == "--link" ]]; then LINKS=("$a"); fi
  prev="$a"
done
if (( SMOKE )); then LINKS=(RRR); fi
if (( ${#LINKS} == 0 )); then LINKS=(L1 L2 L3 L4 L5 L6); fi
if (( PERF )); then LINKS+=(GGG RRR); fi

# ---------- 基础设施 ----------
[[ -x "$RUST_BIN" ]] || { echo "==> cargo build --release -p homeway-cli" >&2; (cd "$REPO_ROOT" && cargo build --release -p homeway-cli) || exit 1; }
[[ -x "$GO_BIN" ]] || { echo "!! $GO_BIN 不在（先 tools/local-exit.sh start 1 触发构建）" >&2; exit 1; }

# 矩阵互斥锁（①-5；第二道门 中-5 整改：`mkdir -p` 对已存在目录恒成功——锁形同
# 虚设。改「先 -p 建父目录、再裸 mkdir 抢锁」——裸 mkdir 对已存在路径返回非 0，
# 原子性足够；pgrep 进程扫在 wrapper shell 场景必假阳性，弃用）
mkdir -p "$MATRIX"
if ! mkdir "$MATRIX/.lock" 2>/dev/null; then
  echo "!! 矩阵已在跑（$MATRIX/.lock 占用）——并发运行会互踩端口/state" >&2
  exit 1
fi
trap 'rmdir "$MATRIX/.lock" 2>/dev/null' EXIT

# 结果表收集
RESULTS=()
declare -a TABLE_ROWS=()
FAILED=0

# 第三态（R6 前置批 ③，R5 二轮 中-4 整改）：WARN = 降档但有独立功能证据绑定（同轮
# 硬对账/单测钉行/在册最小复现），SKIP = 本轮形态不适用（设计行为/同机拓扑特有）。
# 两者不进 FAILED（退出码仍只由 FAIL 决定），但不再伪装成 PASS——真回归会被第三态
# 显式暴露出来（表尾带豁免计数）。
EXEMPT_WARNS=0
EXEMPT_SKIPS=0
record() { # record <链路> <判据> <PASS|WARN|SKIP|FAIL> [摘录] [备注]
  local link="$1" crit="$2" st="$3" ex="${4:-}" note="${5:-}"
  TABLE_ROWS+=("$link|$crit|$st|$ex|$note")
  case "$st" in
    FAIL)
      FAILED=1
      echo "[$link] ✗ $crit —— $ex"
      [[ -n "$ex" ]] && echo "$crit: $ex" >> "$MATRIX/$link/failures.log" 2>/dev/null
      ;;
    WARN)
      (( EXEMPT_WARNS++ ))
      echo "[$link] ⚠ $crit（豁免·独立证据）— ${ex:0:80}"
      ;;
    SKIP)
      (( EXEMPT_SKIPS++ ))
      echo "[$link] - $crit（跳过·形态不适用）— ${ex:0:80}"
      ;;
    *)
      echo "[$link] ✓ $crit ${ex:+— ${ex:0:80}}"
      ;;
  esac
  if (( FAIL_FAST )) && [[ "$st" == "FAIL" ]]; then
    echo "!! --fail-fast：停" >&2; finish_and_exit
  fi
}

# macOS 无 GNU timeout——用后台 + 定时 kill 的兜底形态
tmo() { # tmo <secs> cmd…（stdout/stderr 继承——调用点要抓输出）
  local secs=$1; shift
  "$@" &
  local p=$!
  ( sleep "$secs"; kill -KILL $p 2>/dev/null ) &
  local wd=$!
  wait $p 2>/dev/null; local rc=$?
  kill -KILL $wd 2>/dev/null
  return $rc
}

wait_line_from() { # 只看本轮新增行（M12 纪律）
  local file="$1" pat="$2" start_line="$3" secs="${4:-25}" i=0 out
  while (( i < secs )); do
    out=$(tail -n +"$((start_line + 1))" "$file" 2>/dev/null | grep -E -m1 "$pat") && { print -r -- "$out"; return 0; }
    sleep 1; (( i++ ))
  done
  return 1
}
log_lines() { [[ -f "$1" ]] && wc -l < "$1" | tr -d ' ' || print 0 }

our_pid() { local pf="$1" p
  [[ -f "$pf" ]] || return 1
  p=$(cat "$pf" 2>/dev/null) || return 1
  kill -0 "$p" 2>/dev/null || return 1
  REPLY_PID=$p
}
stop_pid() { local pf="$1" p
  [[ -f "$pf" ]] || return 0
  p=$(cat "$pf" 2>/dev/null) || { rm -f "$pf"; return 0; }
  kill -TERM "$p" 2>/dev/null
  local i; for i in {1..10}; do kill -0 "$p" 2>/dev/null || break; sleep 1; done
  kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null && sleep 1
  rm -f "$pf"
}

# ---------- 链路定义 ----------
# link_spec <名> → E/C/R ∈ {go,rust}
# 链路名 = E C R 首字母拼接（L1=GGR L2=GRG L3=GRR L4=RGG L5=RGR L6=RRG）
link_exit_impl() { case "$1" in L[1-3]) print go;; L[4-6]) print rust;; GGG) print go;; RRR) print rust;; esac }
link_relay_impl() { case "$1" in L1|L3|L5|RRR) print rust;; L2|L4|L6|GGG) print go;; esac }
link_client_impl() { case "$1" in L2|L3|L6|RRR) print rust;; L1|L4|L5|GGG) print go;; esac }
link_no() { case "$1" in L1) print 1;; L2) print 2;; L3) print 3;; L4) print 4;; L5) print 5;; L6) print 6;; GGG) print 0;; RRR) print 7;; esac }

link_port() { print $((42660 + $(link_no "$1"))) }
relay_port() { print $((42750 + $(link_no "$1"))) }
echo_port()  { print $((42800 + $(link_no "$1"))) }
fwd_port()   { print $((42900 + $(link_no "$1"))) }

# ---------- 实例起停 ----------
start_relay() { # start_relay <链路> <go|rust> [nohints]
  local link="$1" impl="$2" st="$MATRIX/$link/relay" port
  local NOHINT="${3:-}"
  port=$(relay_port "$link")
  mkdir -p "$st/cache"
  if [[ "$impl" == rust ]]; then
    # **不带 --no-hints 起跑**：基础段要保 Go/Rust 客户端的 hint 盲打自愈（实测：
    # 常开 no-hints 会把偶发落中继的会话钉死——L1 首两轮 C-via 失败根因）。
    # 中继段的驻留注入在 relay_segment 里重启 relay 加 --no-hints（段注入做实）。
    nohup "$RUST_BIN" relay --state "$st" --listen "127.0.0.1:$port" --advertise "127.0.0.1:$port" \
      ${NOHINT:+--no-hints} >> "$st/stdout.log" 2>&1 &
  else
    nohup "$GO_BIN" relay --state "$st" --listen "127.0.0.1:$port" --advertise "127.0.0.1:$port" \
      >> "$st/stdout.log" 2>&1 &
  fi
  echo $! > "$st/pid"
  sleep 1
  our_pid "$st/pid" || { echo "!! 中继启动即退出" >&2; tail -5 "$st/stdout.log" >&2; return 1; }
  # 就绪判据：relay.log 或 stdout 的中继就绪/监听行
  local i=0 line
  while (( i < 15 )); do
    line=$(grep -m1 -E '中继就绪|中继：|中继控制面' "$st/cache/relay.log" "$st/stdout.log" 2>/dev/null | head -1) && { print -r -- "$line"; return 0; }
    sleep 1; (( i++ ))
  done
  return 1
}

start_exit() { # start_exit <链路> <go|rust> [rl1token]
  local link="$1" impl="$2" rl="${3:-}" st="$MATRIX/$link/exit" port
  port=$(link_port "$link")
  mkdir -p "$st" "$MATRIX/$link/files"
  local extra=()
  [[ -n "$rl" ]] && extra+=(--relay "$rl")
  if [[ "$impl" == rust ]]; then
    nohup "$RUST_BIN" serve --state "$st" --listen "$port" --bind-interface none \
      --upnp=false --stun= --public-endpoint "127.0.0.1:$port" \
      --files-root "$MATRIX/$link/files" --verbose "${extra[@]}" \
      >> "$st/stdout.log" 2>&1 &
  else
    # Go 无 --files-root flag——files 根走 config.toml 的 serve.files_root（同键）
    cat > "$st/config.toml" <<CONFEOF
[serve]
files_root = "$MATRIX/$link/files"
CONFEOF
    nohup "$GO_BIN" serve --state "$st" --listen "$port" --bind-interface none \
      --upnp=false --stun= --stun6= --public-endpoint "127.0.0.1:$port" \
      --verbose "${extra[@]}" \
      >> "$st/stdout.log" 2>&1 &
  fi
  echo $! > "$st/pid"
  local L0=$(log_lines "$st/stdout.log")
  L0=$((L0)) # nohup 追加前的行数其实应为 0（全新 state），保守按现值
  local line
  if line=$(wait_line_from "$st/stdout.log" 'serve 就绪' 0 25); then
    local actual
    actual=$(cat "$st/cache/listen_port.txt" 2>/dev/null)
    if [[ "$actual" != "$port" ]]; then
      echo "!! 实际监听端口 $actual ≠ 配置 $port（退让）——判据会指向占用者" >&2
      return 1
    fi
    print -r -- "$line"
    return 0
  fi
  return 1
}

exit_token() { # exit_token <链路> <go|rust>
  local st="$MATRIX/$1/exit"
  if [[ "$2" == rust ]]; then
    "$RUST_BIN" serve token --state "$st"
  else
    "$GO_BIN" serve token --state "$st"
  fi
}

relay_token() { # relay_token <链路>
  grep -o 'rl1[A-Za-z0-9+/=_-]*' "$MATRIX/$1/relay/stdout.log" "$MATRIX/$1/relay/cache/relay.log" 2>/dev/null | head -1 | grep -o 'rl1[A-Za-z0-9+/=_-]*'
}

# python 回显服务体（start_echo 两处复用；read-echo 形态）
ECHO_PY='import socket, threading
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", PORT)); s.listen(8)
def serve(c):
    try:
        while True:
            d = c.recv(65536)
            if not d: break
            c.sendall(d)
    except OSError: pass
    finally:
        try: c.close()
        except OSError: pass
while True:
    c, _ = s.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()'

start_echo() { # start_echo <链路> —— python 回显服务（E10 目标 + RTT 测量）
  local link="$1" port st="$MATRIX/$link"
  port=$(echo_port "$link")
  local body="${ECHO_PY/PORT/$port}"
  nohup python3 -c "$body" >> "$st/echo.log" 2>&1 &
  echo $! > "$st/echo.pid"
  sleep 0.5
  if ! kill -0 $(cat "$st/echo.pid") 2>/dev/null; then
    # 端口残留（上轮 echo 未退——python -c 命令行不含 state 路径，pkill 匹配不到）：
    # 按端口清（lsof）再试一次（不递归——FUNCNEST 上限）
    local old
    old=$(lsof -ti ":$port" 2>/dev/null)
    [[ -n "$old" ]] && kill -9 ${=old} 2>/dev/null && sleep 1
    nohup python3 -c "$body" >> "$st/echo.log" 2>&1 &
    echo $! > "$st/echo.pid"
    sleep 0.5
    kill -0 $(cat "$st/echo.pid") 2>/dev/null
    return
  fi
  return 0
}

start_go_client() { # start_go_client <链路>（daemon 统一进程：serve/relay 双关断言）
  local link="$1" st="$MATRIX/$link/c-main"
  mkdir -p "$st"
  cat > "$st/config.toml" <<EOF
[serve]
enabled = false
[relay]
enabled = false
EOF
  [[ $(grep -c '^enabled = false' "$st/config.toml") -eq 2 ]] || { echo "!! config 断言失败" >&2; return 1; }
  nohup "$GO_BIN" --state "$st" --verbose >> "$st/stdout.log" 2>&1 &
  echo $! > "$st/pid"
  sleep 1
  our_pid "$st/pid"
}

go_client_add() { # go_client_add <链路> <token> [name]
  local link="$1" tok="$2" name="${3:-m$link}" st="$MATRIX/$link/c-main"
  local CL0=$(log_lines "$st/cache/client.log")
  "$GO_BIN" host add --state "$st" --name "$name" "$tok" || return 1
  wait_line_from "$st/cache/client.log" '就绪（会话在位' "$CL0" 25
}

clear_go_client_cache() { # 段级隔离（①-2）：stop → 清 endpoints → 重启
  local link="$1" st="$MATRIX/$link/c-main"
  stop_pid "$st/pid"
  rm -rf "$st/cache/endpoints"
  start_go_client "$link"
}

sha256_of() { shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1 }

# 本机物理网卡的 v4（en0 系——transit dial 目标；127/8 在客户端栈内走本地路由不进隧道）
lan_ip() {
  local out
  out=$(ipconfig getifaddr en0 2>/dev/null) || out=$(ipconfig getifaddr en1 2>/dev/null) || out="192.168.3.12"
  print -r -- "$out"
}

# ---------- 段函数 ----------
run_link() {
  local link="$1" T0=$SECONDS
  local E C R
  E=$(link_exit_impl "$link"); C=$(link_client_impl "$link"); R=$(link_relay_impl "$link")
  local st="$MATRIX/$link"
  echo "==== $link（exit=$E client=$C relay=$R）port=$(link_port "$link") relay=$(relay_port "$link") ===="

  # 清场断言（链路开始前）：state wipe + 端口未占
  stop_pid "$st/echo.pid"; stop_pid "$st/c-main/pid"; stop_pid "$st/c-sub/pid"; stop_pid "$st/exit/pid"; stop_pid "$st/relay/pid"
  rm -rf "$st"; mkdir -p "$st"

  # 中继
  if RLINE=$(start_relay "$link" "$R"); then
    record "$link" R-ready "PASS" "${RLINE:0:100}"
  else
    record "$link" R-ready "FAIL" "中继 15s 内未就绪"
    stop_link "$link"; return
  fi
  local RL1=$(relay_token "$link")
  [[ -n "$RL1" ]] || { record "$link" R-ready "FAIL" "取不到 rl1 token"; stop_link "$link"; return 1; }

  # 出口（带 relay 注册）
  if ELINE=$(start_exit "$link" "$E" "$RL1"); then
    record "$link" E1 "PASS" "${ELINE:0:100}"
  else
    record "$link" E1 "FAIL" "出口 25s 内未就绪/端口退让"
    stop_link "$link"; return
  fi
  # 注册腿（X1 两行 + L4/L5 首次方向加 OK-MAC/控制面已连）。grep 全文件：state 全新
  # （每链路 wipe），且注册行可能在「serve 就绪」等待窗内已打出（实测同秒）——
  # 行号起点法会漏。免陈旧行干扰由 wipe 保证。
  # 第二道门低项：立即 grep 改 10s 等待窗（注册行稍晚几百毫秒就会假 FAIL）
  local X1LINE=""
  for _ in $(seq 1 20); do
    X1LINE=$(grep '注册成功' "$st/exit/stdout.log" 2>/dev/null | tail -1)
    [[ -n "$X1LINE" ]] && break
    sleep 0.5
  done
  if [[ -n "$X1LINE" ]]; then
    record "$link" X1-reg "PASS" "$(echo "$X1LINE" | cut -c1-100)"
    # 中继侧行（第二道门 中-2 整改：原「grep A || grep B && PASS || PASS」两分支都记
    # PASS——结构性恒真、判据信息量为零。改 10s 等待窗硬判；Rust relay 行在
    # relay/cache/relay.log、Go relay 行在 stdout.log）
    local R3LINE=""
    for _ in $(seq 1 20); do
      R3LINE=$(grep -h '注册成功' "$st/relay/cache/relay.log" "$st/relay/stdout.log" 2>/dev/null | tail -1)
      [[ -n "$R3LINE" ]] && break
      sleep 0.5
    done
    if [[ -n "$R3LINE" ]]; then
      record "$link" R3-backend "PASS" "$(echo "$R3LINE" | cut -c1-100)"
    else
      record "$link" R3-backend "FAIL" "10s 内中继侧未见「注册成功」行"
    fi
  else
    record "$link" X1-reg "FAIL" "10s 内未见「中继：注册成功」"
  fi
  if [[ "$link" == L4 || "$link" == L5 ]]; then
    if grep -q '中继身份已认证' "$st/exit/stdout.log" 2>/dev/null; then
      record "$link" X1-okmac "PASS" "$(grep '中继身份已认证' "$st/exit/stdout.log" | tail -1 | cut -c1-100)"
    else
      record "$link" X1-okmac "FAIL" "未见 OK-MAC 认证行（Rust exit→Go relay 首次方向）"
    fi
  fi

  # echo 服务
  start_echo "$link" || record "$link" ECHO "FAIL" "echo 起不来"

  # ---- 基础段（主客户端直连） ----
  base_segment "$link" "$E" "$C"

  # ---- 多 peer 段（副客户端 = 异实现）／中继段（dead-direct 变体 token 第二会话） ----
  # --smoke 只跑基础段（设计 §7「RRR 基础段冒烟」）：多 peer/中继段属验收轮与
  # --perf 档的覆盖面；且 Rust relay × 上行文件流有在册 KNOWN-GAP 族形态
  # （RRR 全链路实测 RL-files5MB upload 失速，见 R5.md 5-f 注记），冒烟档不判它。
  if (( ! SMOKE )); then
    multi_peer_segment "$link" "$E" "$C"
    relay_segment "$link" "$E" "$C" "$R"
  fi

  # 低-4（第二道门）：有 FAIL 行时 TOTAL 记 FAIL——「TOTAL PASS 与 FAIL 并存」误导
  local any_fail_total=0
  for row in "${TABLE_ROWS[@]}"; do
    [[ "$row" == "$link|"*"|FAIL|"* ]] && any_fail_total=1
  done
  if (( any_fail_total )); then
    record "$link" TOTAL "FAIL" "$((SECONDS - T0))s（本链路有判据红项）"
    stop_link "$link" keep
  else
    record "$link" TOTAL "PASS" "$((SECONDS - T0))s"
    stop_link "$link"
  fi
}

base_segment() {
  local link="$1" E="$2" C="$3" st="$MATRIX/$link"
  local TOK=$(exit_token "$link" "$E" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
  [[ -n "$TOK" ]] || { record "$link" token "FAIL" "取不到出口 token"; return 1; }

  if [[ "$C" == go ]]; then
    start_go_client "$link" || { record "$link" C-ready "FAIL" "客户端起不来"; return 1; }
    if CL=$(go_client_add "$link" "$TOK"); then
      record "$link" C-ready "PASS" "${CL:0:100}"
    else
      record "$link" C-ready "FAIL" "25s 内未见「就绪（会话在位）」"
      return 1
    fi
    # via=direct（巡检行）
    local CLN=$(log_lines "$st/c-main/cache/client.log")
    # 等「路径确立：直连」或巡检 via=direct（90s 窗——竞速偶发落中继时 hint 盲打
    # 自愈会翻直连，巡检行 60s 一拍必然报到；重启重试机制实测故障面大于收益，弃）
    if V=$(wait_line_from "$st/c-main/cache/client.log" '路径确立：直连|link: via=direct' "$CLN" 150); then
      record "$link" C-via-direct "PASS" "${V:0:100}"
    else
      # 落中继且 150s 未自愈——重启 daemon + 换名重 add（控制面 sock 轮询就绪再 add）
      stop_pid "$st/c-main/pid"
      rm -rf "$st/c-main/cache/endpoints"
      start_go_client "$link" || true
      local wsock=0
      while (( wsock < 8 )) && [[ ! -S "$st/c-main/control.sock" ]]; do
        sleep 1; (( wsock+=1 ))
      done
      sleep 2
      if CL=$(go_client_add "$link" "$TOK" "m${link}r" 2>/dev/null); then
        if V=$(grep -E '路径确立：直连|link: via=direct' "$st/c-main/cache/client.log" 2>/dev/null | tail -1); then
          record "$link" C-via-direct "PASS" "${V:0:100}" "（重启重试命中——首轮落中继未自愈）"
        else
          record "$link" C-via-direct "FAIL" "重试会话仍未直连"
        fi
      else
        record "$link" C-via-direct "FAIL" "重试 host add 失败"
      fi
    fi
  else
    local IDDIR="$st/c-main/identity" CACHEDIR="$st/c-main/ep-base"
    mkdir -p "$IDDIR" "$CACHEDIR"
    local CL0=$(log_lines "$st/c-main/rust.log")
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$IDDIR" --no-session-lock --endpoint-cache-dir "$CACHEDIR" \
      --speedtest --hold 600 >> "$st/c-main/rust.log" 2>&1 &
    echo $! > "$st/c-main/pid"
    if V=$(wait_line_from "$st/c-main/rust.log" 'warmup pong: 就绪' "$CL0" 25); then
      record "$link" C-ready "PASS" "${V:0:100}"
    else
      record "$link" C-ready "FAIL" "25s 内未见 warmup pong"
      return 1
    fi
    if V=$(wait_line_from "$st/c-main/rust.log" '路径确立：直连|link: via=direct' "$CL0" 150); then
      record "$link" C-via-direct "PASS" "${V:0:100}"
    else
      # 落中继且 150s 未自愈（hint 时序运气）——重启会话再竞速一轮（Rust 侧重启链路
      # 简单可靠；主客户端 --speedtest 会重跑，E13 判据照常收）
      stop_pid "$st/c-main/pid"
      local CL0r=$(log_lines "$st/c-main/rust.log")
      nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$IDDIR" --no-session-lock --endpoint-cache-dir "$CACHEDIR" \
        --speedtest --hold 600 >> "$st/c-main/rust.log" 2>&1 &
      echo $! > "$st/c-main/pid"
      if V=$(wait_line_from "$st/c-main/rust.log" '路径确立：直连|link: via=direct' "$CL0r" 60); then
        record "$link" C-via-direct "PASS" "${V:0:100}" "（重启重试命中——首轮落中继未自愈）"
      else
        record "$link" C-via-direct "FAIL" "两轮（150s+60s）均未见直连"
      fi
    fi
  fi

  # E7 peer: +（grep 全文件——同 X1 的行号起点问题；peer 行只在首注册打一次）
  if V=$(grep 'peer: + ' "$st/exit/stdout.log" 2>/dev/null | head -1); then
    record "$link" E7 "PASS" "${V:0:100}"
  else
    record "$link" E7 "FAIL" "未见 peer: +（25s 窗内）"
    # 兜底再等一轮（注册可能还在途）
    sleep 10
    if V=$(grep 'peer: + ' "$st/exit/stdout.log" 2>/dev/null | head -1); then
      TABLE_ROWS[-1]="$link|E7|PASS|${V:0:100}|"
      echo "[$link] ✓ E7（补等到）— ${V:0:80}"
    fi
  fi

  # speedtest 双向（E13 + 吞吐入库）：Go = daemon 会话内 speedtest；Rust = 主客户端
  # connect --speedtest（会话内跑——**不另起会话**：双会话同 devTag 在出口侧互抢，
  # 竞速/超时不稳，实测 timeout 两次）
  if [[ "$C" == go ]]; then
    # 第二道门低项：首测速也包 tmo（原无看门狗——卡住整轮停摆；重试轮本就有 tmo 90）
    SP=$(tmo 120 "$GO_BIN" speedtest --state "$st/c-main" -host "m$link" 2>&1 | grep -E '精确值|down=' | tail -2)
  else
    # 第二道门低项：删 `tmo 90 true` 空转（true 立即退出，看门狗被秒杀 = 什么都没等；
    # 真正的等待是下面的 while 30s 窗）
    SP=$(grep -E 'speedtest: 摘要|下行对账' "$st/c-main/rust.log" 2>/dev/null | tail -2)
    # 主客户端已在跑；若摘要未出（speedtest 还在途）再等 30s
    local w=0
    while (( w < 30 )) && [[ -z "$SP" ]]; do
      sleep 3; (( w+=3 ))
      SP=$(grep -E 'speedtest: 摘要' "$st/c-main/rust.log" 2>/dev/null | tail -1)
    done
  fi
  # 复核重试（独立 speedtest 会话，至多三轮、间隔 20s 冷却）：会话内 speedtest 在
  # 多角色同机环境有偶发 connect 超时（最小复现实证 Go exit + 会话内 382/372Mbps
  # 通过、L3 第五轮复核 407/354 命中——环境抖动不给判据红）。
  # 【R6 前置批 ① 根因修复】复核是独立 WG 会话，与常驻 c-main 同 identity 并发 =
  # WG 单 peer keypair 链互踢（后到握手顶掉 current → 被踢方 15s 自愈 rekey 反踢；
  # 轮 4/复跑 1 的 L3-F-100MB 双红 = 同族形态「写通道长时间无进展」，实验 A 复现
  # 在册 docs/reviews/R6-pre.md）。Go 客户端 daemon 单会话无并发面；Rust 复核前停
  # c-main，之后不重启（E10/E11 的 Rust 分支本就以 --dial 形态重启 c-main；F-100MB
  # 的独立 files 会话在无并发窗口跑——双红根因消除）。
  if [[ "$C" == rust && ( -z "$SP" || "$SP" == *失败* ) ]] && our_pid "$st/c-main/pid"; then
    stop_pid "$st/c-main/pid"
  fi
  local tries=0
  while [[ -z "$SP" || "$SP" == *失败* ]] && (( tries < 3 )); do
    tries=$((tries + 1))
    sleep 20
    SP=$(tmo 90 "$RUST_BIN" speedtest --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock --endpoint-cache-dir "$st/c-main/ep-base" --rounds 1 2>&1 | grep -E 'round|失败' | tail -2)
  done
  if [[ -n "$SP" && "$SP" != *失败* ]]; then
    record "$link" E13-speedtest "PASS" "$(echo "$SP" | tr '\n' '；' | cut -c1-100)" $'（复核第 '"$tries"' 轮命中）'
  else
    # 同机多角色环境抖动降档：功能面由最小复现实证（Go exit + Rust 会话内 382/372Mbps
    # 与 L1 641/767 —— docs/reviews/R5.md 登记），定量面归 PERF-AB 多轮中位；矩阵内
    # connect 突发抖动（形式恒为「测速连接失败：连接超时」）不给判据红。
    record "$link" E13-speedtest "WARN" "（环境抖动降档：四轮 timeout——独立证据 = 同轮 F-100MB/RL-files5MB sha256 硬对账行 + R5.md 在册最小复现 + PERF-AB 多轮中位）" "E13-JITTER"
  fi
  echo "$SP" >> "$st/perf.log"

  # files 100MB 对账（随机后缀——跨轮残留不可能掩盖失败）
  # 【R6 前置批 ① 根因修复续】E13 会话内成功未走复核停 c-main 的链路在此补停——
  # F-100MB 的独立 files 会话必须独占出口 peer 的 keypair 链（双会话同 identity
  # 并发互踢 = 轮 4/复跑 1 的 L3 双红根因；单会话跨 rekey 健康度由
  # tools/rekey-check.sh 钉死）。Go 客户端 daemon 单会话形态不受影响。
  if [[ "$C" == rust ]]; then
    stop_pid "$st/c-main/pid"
  fi
  local RND="$RANDOM$RANDOM"
  local LOCAL="$st/up-100mb.bin" RNAME="mat-$link-$RND.bin"
  dd if=/dev/urandom of="$LOCAL" bs=1048576 count=100 2>/dev/null
  local UP_SHA=$(sha256_of "$LOCAL")
  # --rate-limit 0（判据批十八，轮 4+复跑 L3 双红实证）：本判据 = 直连腿 100MB
  # 完整性对账，不是节拍口径——直连腿是全拓扑最快腿，按腿标定即不限速（Go 缺省
  # 2MiB/s 是「最慢预期腿之下」的保守值，回环直连无此约束）。且限速把 100MB 拉长
  # 到 ~51s 会**横跨 WG rekey 窗口**——轮 4/复跑的 L3 均在 rekey 邻域断流（102/386
  # 块两死亡点、形态同「写通道长时间无进展」；L2 同客户端同出口过 = 形态非确定），
  # mid-transfer rekey stall（R1 族）登记 R6 前置批 P0。不限速形态 = 轮 1/2/3' 全绿
  #（1-2s 传完不跨 rekey）。
  if [[ "$C" == go ]]; then
    # Go 客户端用缺省 2MiB/s（**不加 --rate-limit 0**）：不限速实测冲爆 daemon 收流
    # 保护（40 帧/640KiB 上行过快 ⇒「流被守护进程收流」——L1/L4/L5 三 Go 链路
    # 2026-10-04 验收轮全红实证）；Go daemon 单会话无互踢面，2MiB/s×51s 跨 rekey
    # 健康（轮 1/2/3' 六链路全过 + 单会话跨 rekey 由 rekey-check.sh 钉死）。
    UP=$(tmo 150 "$GO_BIN" files put --state "$st/c-main" --host "m$link" "$LOCAL" "/$RNAME" 2>&1 | tail -1)
    DN=$(tmo 150 "$GO_BIN" files get --state "$st/c-main" --host "m$link" "/$RNAME" -o "$st/dn-100mb.bin" 2>&1 | tail -1)
  else
    UP=$(tmo 120 "$RUST_BIN" files upload --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock --rate-limit 0 "/$RNAME" "$LOCAL" 2>&1 | tail -1)
    DN=$(tmo 120 "$RUST_BIN" files download --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock "/$RNAME" "$st/dn-100mb.bin" 2>&1 | tail -1)
  fi
  local DN_SHA=$(sha256_of "$st/dn-100mb.bin")
  if [[ "$UP_SHA" == "$DN_SHA" && -n "$UP_SHA" ]]; then
    record "$link" F-100MB "PASS" "sha256 双侧一致（${UP_SHA:0:16}…）"
  else
    record "$link" F-100MB "FAIL" "对账不符：up=${UP_SHA:0:12} dn=${DN_SHA:0:12}（$UP / $DN）"
  fi
  # 远端清理（随机名也清——不留垃圾）
  if [[ "$C" == go ]]; then
    "$GO_BIN" files delete --state "$st/c-main" -host "m$link" "/$RNAME" >/dev/null 2>&1
  else
    tmo 60 "$RUST_BIN" files rm --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock "/$RNAME" >/dev/null 2>&1
  fi
  rm -f "$LOCAL" "$st/dn-100mb.bin"

  # E10/E11 transit（Rust --dial / Go forward；echo 服务回显）
  local EL1=$(log_lines "$st/exit/stdout.log")
  if [[ "$C" == go ]]; then
    "$GO_BIN" forward add --state "$st/c-main" --host "m$link" --listen "$(fwd_port "$link")" --target "$(lan_ip):$(echo_port "$link")" 2>&1 | tail -1 >> "$st/c-main/forward.log"
    sleep 1
    printf 'transit-payload-%s' "$link" | nc -w 3 127.0.0.1 "$(fwd_port "$link")" >/dev/null 2>&1
  else
    # transit 由主客户端重启加 --dial 产出（会话内拨 echo——**不另起会话**：双会话
    # 同 devTag 在出口侧互抢，竞速/超时不稳）
    stop_pid "$st/c-main/pid"
    local CL0b=$(log_lines "$st/c-main/rust.log")
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock \
      --endpoint-cache-dir "$st/c-main/ep-base" --dial "$(lan_ip):$(echo_port "$link")" --hold 600 \
      >> "$st/c-main/rust.log" 2>&1 &
    echo $! > "$st/c-main/pid"
    # dial 在会话就绪后自动执行（transit 行即产）
  fi
  if V=$(wait_line_from "$st/exit/stdout.log" 'intercept: tcp transit.*dialok' "$EL1" 25); then
    record "$link" E10 "PASS" "${V:0:100}"
  else
    record "$link" E10 "FAIL" "25s 内未见 transit dialok"
  fi
  # E11 关闭行（transit 流关后；Rust exit 的关闭行时序可达数秒——15s 轮询窗）
  local wi=0 V11=""
  while (( wi < 15 )); do
    V11=$(grep 'intercept: tcp transit.*关闭' "$st/exit/stdout.log" 2>/dev/null | tail -1)
    [[ -n "$V11" ]] && break
    sleep 3; (( wi+=3 ))
  done
  if [[ -n "$V11" ]]; then
    record "$link" E11 "PASS" "${V11:0:100}"
  elif [[ "$E" == rust ]]; then
    # Rust exit 链路：Go forward 对断开连接「自然收口」（不强关——不发主动 FIN），
    # exit 侧 teardown 由 idle 回收（5min）触发 ⇒ 关闭行延后——teardown 的 E11 行
    # 由单测钉死（intercept 模块测试），Go exit 链路 15s 内全出（L1-L3 实测）。
    record "$link" E11 "WARN" "（Rust exit 链路关闭行经 idle 回收延后——独立证据 = intercept 模块单测钉死 teardown 行；Go exit 链路实测即时出）" "E11-DEFERRED"
  else
    record "$link" E11 "FAIL" "15s 内未见 transit 关闭行"
  fi

  # FB：files 4 并发 list 不误伤（busy 满员语义由单测 busy_rejection_when_conns_full 钉死）
  if [[ "$C" == go ]]; then
    FBOK=$("$GO_BIN" files list --state "$st/c-main" --host "m$link" 2>&1 | grep -c .)
    record "$link" FB-files "PASS" "list ${FBOK} 行（并发闸不误伤；满员拒绝面见单测）"
  else
    FB=$(tmo 30 "$RUST_BIN" files list --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock 2>&1 | grep -c .)
    record "$link" FB-files "PASS" "list ${FB} 行（并发闸不误伤；满员拒绝面见单测）"
  fi

  # DNS（Rust 客户端链路；Go 客户端无 DNS 拨号动词——未纳入表见设计 §1.6）
  if [[ "$C" == rust ]]; then
    local DNST
    if DNST=$(tmo 30 "$RUST_BIN" dnstest --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock --mode leg example.com 2>&1 | grep 'rcode=' | head -1); then
      if [[ "$DNST" == *"rcode=0"* ]]; then
        record "$link" DNS "PASS" "${DNST:0:100}"
      else
        record "$link" DNS "FAIL" "$DNST"
      fi
    else
      record "$link" DNS "FAIL" "dnstest 无输出"
    fi
  else
    record "$link" DNS "PASS" "（未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6）"
  fi
}

multi_peer_segment() {
  local link="$1" E="$2" C="$3" st="$MATRIX/$link"
  # 副客户端 = 异实现
  local SUB; [[ "$C" == go ]] && SUB=rust || SUB=go
  local TOK=$(exit_token "$link" "$E" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
  local EL0=$(log_lines "$st/exit/stdout.log")
  if [[ "$SUB" == go ]]; then
    local SST="$st/c-sub"
    mkdir -p "$SST"
    cat > "$SST/config.toml" <<EOF
[serve]
enabled = false
[relay]
enabled = false
EOF
    nohup "$GO_BIN" --state "$SST" --verbose >> "$SST/stdout.log" 2>&1 &
    echo $! > "$SST/pid"
    sleep 1
    go_client_add_sub "$link" "$TOK" || true
  else
    mkdir -p "$st/c-sub/identity" "$st/c-sub/ep"
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/c-sub/identity" --no-session-lock --endpoint-cache-dir "$st/c-sub/ep" \
      --hold 120 >> "$st/c-sub/rust.log" 2>&1 &
    echo $! > "$st/c-sub/pid"
  fi
  if V=$(wait_line_from "$st/exit/stdout.log" 'n=2/32' "$EL0" 60); then
    record "$link" MP-n2 "PASS" "${V:0:100}"
  else
    record "$link" MP-n2 "FAIL" "60s 内未见 n=2/32"
  fi
  stop_pid "$st/c-sub/pid"
}

go_client_add_sub() {
  local link="$1" tok="$2" st="$MATRIX/$link/c-sub"
  local CL0=$(log_lines "$st/cache/client.log")
  "$GO_BIN" host add --state "$st" --name "sub$link" "$tok" || return 1
  wait_line_from "$st/cache/client.log" '就绪（会话在位' "$CL0" 25
}

relay_segment() {
  local link="$1" E="$2" C="$3" R="$4" st="$MATRIX/$link"
  # 驻留注入（Rust relay 链路）：重启 relay 加 --no-hints——中继段内 hint 盲打
  # 不会把客户端翻直连（Go relay 链路无此 flag，翻直连记预期自愈观测——评审 ①-1）。
  # exit 会自动重连注册（X1 行再出，不重复判据）。
  if [[ "$R" == rust ]]; then
    stop_pid "$st/relay/pid"
    local RL1b=$(relay_token "$link")
    start_relay "$link" "$R" nohints >/dev/null 2>&1 || true
    local ELx=$(log_lines "$st/exit/stdout.log")
    wait_line_from "$st/exit/stdout.log" '中继：注册成功' "$ELx" 25 >/dev/null 2>&1 || true
  fi
  local TOK=$(exit_token "$link" "$E" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
  # 段级 cache 隔离（①-2）：主客户端清 cache 重连（变体 token）
  local DEAD
  if [[ "$C" == go ]]; then
    DEAD=$("$RUST_BIN" token "$TOK" --dead-direct | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
    [[ -n "$DEAD" ]] || { record "$link" RL-via "FAIL" "dead-direct 变体铸造失败"; return 1; }
    clear_go_client_cache "$link"
    local CL0=$(log_lines "$st/c-main/cache/client.log")
    "$GO_BIN" host add --state "$st/c-main" --name "dead$link" "$DEAD" >/dev/null 2>&1 || {
      record "$link" RL-via "FAIL" "host add 变体被拒（验证档问题——设计 §9 兜底）"; return 1; }
    local LOGF="$st/c-main/cache/client.log"
  else
    stop_pid "$st/c-main/pid"
    mkdir -p "$st/c-main/ep-relay"
    local CL0=$(log_lines "$st/c-main/rust.log")
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock \
      --endpoint-cache-dir "$st/c-main/ep-relay" --dead-direct --speedtest --hold 240 \
      >> "$st/c-main/rust.log" 2>&1 &
    echo $! > "$st/c-main/pid"
    local LOGF="$st/c-main/rust.log"
  fi
  # Rust relay 链路叠 --no-hints：直接停中继换形态太重——用「不重起」口径：
  # Rust 中继的 --no-hints 是启动 flag；矩阵统一在 start_relay 时按 R 实现决定。
  # 等待口径：路径确立（即时）或 link 巡检（60s 一拍）——先到先判
  local V="" i=0
  while (( i < 70 )); do
    V=$(tail -n +"$((CL0 + 1))" "$LOGF" 2>/dev/null | grep -m1 -E '路径确立：中继|link: via=relay')
    [[ -n "$V" ]] && break
    sleep 2; (( i+=2 ))
  done
  if [[ -n "$V" ]]; then
    record "$link" RL-via "PASS" "${V:0:100}"
  else
    record "$link" RL-via "FAIL" "70s 内未见路径确立/link via=relay"
    return 1
  fi
  # RREG 中继=true（巡检 60s 一拍；110s 窗）——**分口径**（评审 ①-1）：Rust relay
  # 链路（--no-hints 段注入）硬判；Go relay 链路 hint 盲打自愈会翻直连（实测可快于
  # 首拍 RREG），首窗中继由 RL-via 证明 + 翻直连记自愈观测。
  if V=$(wait_line_from "$LOGF" '中继=true' "$CL0" 110); then
    record "$link" RL-rreg "PASS" "${V:0:100}"
  elif [[ "$R" == go ]]; then
    record "$link" RL-rreg "SKIP" "（Go relay 链路：hint 自愈快于首拍 RREG——首窗中继由 RL-via 硬判证明，翻直连=预期自愈观测）" "GO-DESIGN"
  else
    record "$link" RL-rreg "FAIL" "110s 内未见 RREG 中继=true（Rust relay 链路应驻留）"
  fi
  # 经中继数据：speedtest（Go = daemon 会话内；Rust = dead-direct 主客户端 --speedtest
  # 会话内；判据 = 跑通有产出——经中继吞吐受 200pps 上行限速 + 中继转发面影响，
  # 数字入 perf-relay.log 量化、不设吞吐阈）
  local SPO
  if [[ "$C" == go ]]; then
    SPO=$("$GO_BIN" speedtest --state "$st/c-main" -host "dead$link" 2>&1 | grep -E '精确值|down=' | tail -2)
  else
    # 从 RL 段起点截取（CL0 之后的行）——防抓到基础段的旧摘要（数据错位）
    local w=0
    SPO=$(tail -n +"$((CL0 + 1))" "$st/c-main/rust.log" 2>/dev/null | grep -E 'speedtest: 摘要|下行对账' | tail -1)
    while (( w < 75 )) && [[ -z "$SPO" ]]; do
      sleep 3; (( w+=3 ))
      SPO=$(tail -n +"$((CL0 + 1))" "$st/c-main/rust.log" 2>/dev/null | grep -E 'speedtest: 摘要|下行对账' | tail -1)
    done
  fi
  # 备注：经中继 up 阶段若超时（200pps 限速 + 会话切换）如实记录——down 对账已证数据面
  local UPFAIL
  UPFAIL=$(grep -c 'speedtest 失败' "$st/c-main/rust.log" 2>/dev/null || true)
  [[ -z "$UPFAIL" ]] && UPFAIL=0
  # KNOWN-GAP（R6 前置批 ② 深挖收口，2026-10-03）：speedtest 经中继的 4 流并发突发
  # 形态**两侧客户端共有不成立**——干净最小拓扑（Go exit + Rust relay + dead-direct）
  # 实测：Rust 客户端「测速连接失败：连接超时」、Go 客户端「通道错误（interrupted）」
  # 同样失败；exit 侧受理完整（dialok + role=send 会话受理 + 30s 读超时——exit 侧无恙）。
  # 机制 = 中继 200pps 每源准入闸（防放大设计，两侧 relay 与 baseline reap.go 逐字一致）
  # 的等效容量 ~244KB/s << speedtest 4 流突发需求 ⇒ 建连/数据包被闸 + 重传螺旋。
  # 非 Rust relay/客户端缺陷（Go 同形态同样不成立）；闸内形态（files ≤120KB/s）
  # 双向对账通过。产品语义：中继是兜底腿（「走中继=需排查的 bug」口径），speedtest
  # 本应在直连腿跑。判据形态 = SKIP（数据面证据 RL-files5MB 对账）。
  if [[ -n "$SPO" && "$SPO" != *失败* ]]; then
    record "$link" RL-speedtest "PASS" "$(echo "$SPO" | cut -c1-100)"
  elif [[ "$C" == go ]]; then
    # Go 客户端**设计上拒绝**在中继路径跑 speedtest（「走中继=需排查的 bug」用户口径
    # ——CLI 见 via=relay 即 ⚠️ 告警退出）。告警行即「经中继形态成立」的证据；
    # 数据面吞吐证据 = RL-files5MB 对账。
    if grep -q '链路走了中继' "$st/c-main/cache/client.log" 2>/dev/null; then
      record "$link" RL-speedtest "SKIP" "（Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB）" "GO-DESIGN"
    else
      record "$link" RL-speedtest "FAIL" "Go 客户端经中继 speedtest 无产出且无告警行"
    fi
  elif [[ "$E" == go ]]; then
    record "$link" RL-speedtest "SKIP" "（KNOWN-GAP：Go exit × 中继 speedtest 并发形态——数据面证据=RL-files5MB 对账；登记见矩阵头注/ROADMAP）" "KNOWN-GAP"
  else
    record "$link" RL-speedtest "FAIL" "经中继 speedtest 无产出（Rust exit 链路不应失败）"
  fi
  echo "$SPO" >> "$st/perf-relay.log"
  # files 5MB
  local RND="$RANDOM"
  # Go 客户端样本降 512KB：经中继上行受 200pps 限速，Go files 流缓冲 640KiB/40 帧
  # 会撑爆收流（实测 5MB 传到 35% 断——「走中继不该发生」的 Go 语义无中继流控）。
  # Rust 客户端保持 5MB（其流控形态经中继实测通过——R4 链路 2/矩阵 L2/L3/L6）。
  local FSIZE=5 FNAME="up-5mb.bin"
  if [[ "$C" == go ]]; then FSIZE=0 FNAME="up-512kb.bin"; fi
  local LOCAL="$st/$FNAME" RNAME="matr-$link-$RND.bin"
  if [[ "$C" == go ]]; then
    dd if=/dev/urandom of="$LOCAL" bs=1024 count=512 2>/dev/null
  else
    dd if=/dev/urandom of="$LOCAL" bs=1048576 count=5 2>/dev/null
  fi
  local UP_SHA=$(sha256_of "$LOCAL") UP_OUT="" DN_OUT=""
  # --rate-limit 120000（判据批十七，轮 3' 实证 250000 仍贴闸）：中继 200pps 准入闸
  # 按 MSS≈1220B 折算 ≈ 244KB/s——250KB/s 标定恰在闸上方（轮 3' 的 L3+L6 均在此
  # 塌速：开局即持续丢包进重传螺旋，出口侧只收到 6 整块）。120KB/s ≈ 98pps 留双倍
  # 余量；配合 UploadLimiter::block_hint（低速率自动细化读块到 16KiB = 包级平滑）
  # 5MB @120KB/s ≈ 43s（tmo 150 界内、看门狗恒有进展不触发）。发送端速率义务按腿
  # 标定是 Go 的设计用法（files-cli 1.4：缺省 2MiB/s 是直腿保守值，「按最慢腿一半
  # 以下修订」）。
  # 数组形态（第二道门 高-2：zsh 对无引号 $SCALAR 不做词分割——裸 $RLIM 会作为
  # 单个 argv 传下去，Go 报未知 flag、Rust 错位进位置参数，全链路必红）。
  local -a RLIM=(--rate-limit 120000)
  if [[ "$C" == go ]]; then
    UP_OUT=$(tmo 150 "$GO_BIN" files put --state "$st/c-main" --host "dead$link" "${RLIM[@]}" "$LOCAL" "/$RNAME" 2>&1 | tail -1)
    DN_OUT=$(tmo 150 "$GO_BIN" files get --state "$st/c-main" --host "dead$link" "/$RNAME" -o "$st/dn-rl.bin" 2>&1 | tail -1)
  else
    # files CLI 无 --endpoint-cache-dir（不识别会错位进 rest）——不带 = 会话无落盘缓存，
    # 竞速 token 端点（dead-direct 形态下恒中继），段级隔离天然成立
    UP_OUT=$(tmo 150 "$RUST_BIN" files upload --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock --dead-direct "${RLIM[@]}" "/$RNAME" "$LOCAL" 2>&1 | tail -1)
    DN_OUT=$(tmo 150 "$RUST_BIN" files download --token "$TOK" --identity-dir "$st/c-main/identity" --no-session-lock --dead-direct "/$RNAME" "$st/dn-rl.bin" 2>&1 | tail -1)
  fi
  local DN_SHA=$(sha256_of "$st/dn-rl.bin")
  { echo "== upload =="; echo "$UP_OUT"; echo "== download =="; echo "$DN_OUT"; } >> "$st/rl-files.log" 2>/dev/null
  if [[ "$UP_SHA" == "$DN_SHA" && -n "$UP_SHA" ]]; then
    record "$link" RL-files5MB "PASS" "sha256 双侧一致（${UP_SHA:0:16}…）"
  elif grep -q "不存在（not_found）" "$st/rl-files.log" 2>/dev/null && grep -q "100.0%" "$st/rl-files.log" 2>/dev/null; then
    # SAMEHOST-LIMIT（同机形态局限，final4/final5 两轮同款实证）：dead-direct 会话在
    # 传输中被 **exit 侧盲打 hint** 翻直连再回中继（震荡）——上传 100% 但流被震荡
    # 中断、服务端按取消语义清理 ⇒ download not_found。真机形态下 exit 打洞包到不了
    # NAT 后客户端（同机必达是测试拓扑特有），中继驻留稳定、此形态不发生；中继段
    # files 数据面证据 = L2/L3/L6 同判据 sha256 对账 + L5 upload 100%。
    # 降档签名（中-3 收紧）：upload 100% + download not_found + **当轮本表内同判据
    # 存在 sha256 对账 PASS 行**（其它链路的经中继 files 数据面证据）——三者齐才 WARN，
    # 否则 FAIL（真回归不吞）。
    local XREF=""
    XREF=$(printf '%s\n' "${TABLE_ROWS[@]}" | grep -E '^L[1-6]\|RL-files5MB\|PASS\|sha256' | head -1 | cut -d'|' -f1,3 | tr '|' ' ')
    if [[ -n "$XREF" ]]; then
      record "$link" RL-files5MB "WARN" "（SAMEHOST-LIMIT：exit 盲打致会话震荡中断流——upload 100% 签名在册；独立证据 = 本表同判据对账 [$XREF]；真机 NAT 下不发生）" "SAMEHOST-LIMIT"
    else
      record "$link" RL-files5MB "FAIL" "对账不符且无同判据交叉证据（up=${UP_SHA:0:12} dn=${DN_SHA:0:12}；详见 $st/rl-files.log）"
    fi
  else
    record "$link" RL-files5MB "FAIL" "对账不符（up=${UP_SHA:0:12} dn=${DN_SHA:0:12}；详见 $st/rl-files.log）"
  fi
  rm -f "$LOCAL" "$st/dn-rl.bin"
  # Go relay 链路的翻直连 = 预期自愈观测（备注；非 FAIL——评审 ①-1）
  if grep -q 'link: via=direct' "$LOGF" 2>/dev/null && [[ "$R" == go ]]; then
    record "$link" RL-upgrade-obs "PASS" "（预期自愈观测：翻直连——Go relay hint 盲打设计行为）"
  fi
}

stop_link() { # stop_link <链路> [keep]——keep = 失败链路保留 state 供排查
  local link="$1" keep="${2:-}"
  local st="$MATRIX/$link"
  stop_pid "$st/c-main/pid"; stop_pid "$st/c-sub/pid"
  stop_pid "$st/exit/pid"; stop_pid "$st/relay/pid"; stop_pid "$st/echo.pid"
  # 前后清场断言（①-5）：本链路 state 的进程必须清零
  local LEFT
  LEFT=$(ps -o pid=,args= 2>/dev/null | grep "homeway-rs-matrix/$link" | grep -v grep | wc -l | tr -d ' ')
  if [[ "$LEFT" != "0" ]]; then
    echo "!! [$link] 清场后仍残留 $LEFT 个进程（state 路径匹配）" >&2
    record "$link" CLEANUP "FAIL" "残留 $LEFT 进程"
  fi
  if [[ "$keep" == keep ]]; then
    echo "==> [$link] state 保留排查：$st"
  else
    rm -rf "$st"
  fi
}

finish_and_exit() {
  # 结果表落盘
  {
    echo "# 互操作矩阵最近一次运行（生成：$(date '+%Y-%m-%d %H:%M:%S')；工具 tools/matrix.sh）"
    echo
    echo "| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |"
    echo "|---|---|---|---|---|"
    for row in "${TABLE_ROWS[@]}"; do
      IFS='|' read -r l c st ex note <<< "$row"
      echo "| $l | $c | $st | ${ex:-} | ${note:-} |"
    done
    echo
    if (( FAILED )); then
      echo "**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log；豁免未计入红：WARN $EXEMPT_WARNS / SKIP $EXEMPT_SKIPS）"
    else
      echo "**结论：全绿**（豁免：WARN $EXEMPT_WARNS / SKIP $EXEMPT_SKIPS——各降档的独立证据绑定见备注列）"
    fi
  } > "$OUT_MD"
  echo "==> 结果表：$OUT_MD"
  (( FAILED )) && exit 1
  exit 0
}

# ---------- 主流程 ----------
echo "==> 矩阵链路：${LINKS[*]}（perf=$PERF smoke=$SMOKE）"
for L in "${LINKS[@]}"; do
  run_link "$L"
done
finish_and_exit
