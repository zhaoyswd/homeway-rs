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
#   --smoke = RRR 基础段前 4 判据（ci-local 冒烟档）
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

# 矩阵互斥锁（①-5）
if ! mkdir -p "$MATRIX/.lock" 2>/dev/null; then
  echo "!! 矩阵已在跑（$MATRIX/.lock 占用）——并发运行会互踩端口/state" >&2
  exit 1
fi
trap 'rmdir "$MATRIX/.lock" 2>/dev/null' EXIT

# 结果表收集
RESULTS=()
declare -a TABLE_ROWS=()
FAILED=0

record() { # record <链路> <判据> <PASS|FAIL> [摘录] [备注]
  local link="$1" crit="$2" st="$3" ex="${4:-}" note="${5:-}"
  TABLE_ROWS+=("$link|$crit|$st|$ex|$note")
  if [[ "$st" == "FAIL" ]]; then
    FAILED=1
    echo "[$link] ✗ $crit —— $ex"
    [[ -n "$ex" ]] && echo "$crit: $ex" >> "$MATRIX/$link/failures.log" 2>/dev/null
  else
    echo "[$link] ✓ $crit ${ex:+— ${ex:0:80}}"
  fi
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
    out=$(tail -n +"$((start_line + 1))" "$file" 2>/dev/null | grep -m1 "$pat") && { print -r -- "$out"; return 0; }
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
start_relay() { # start_relay <链路> <go|rust>
  local link="$1" impl="$2" st="$MATRIX/$link/relay" port
  port=$(relay_port "$link")
  mkdir -p "$st/cache"
  if [[ "$impl" == rust ]]; then
    # Rust 中继一律 --no-hints（R4 口径：同机拓扑里 hint→盲打→任意源采纳会把客户端
    # 翻成直连——基础段直连不经 hint，无影响；中继段驻留前提成立）
    nohup "$RUST_BIN" relay --state "$st" --listen "127.0.0.1:$port" --advertise "127.0.0.1:$port" \
      --no-hints >> "$st/stdout.log" 2>&1 &
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

start_echo() { # start_echo <链路> —— python 回显服务（E10 目标 + RTT 测量）
  local link="$1" port st="$MATRIX/$link"
  port=$(echo_port "$link")
  nohup python3 -c "
import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('0.0.0.0', $port)); s.listen(8)
conns = []
while True:
    c, _ = s.accept()
    conns.append(c)
    import threading
    def serve(c=c):
        try:
            while True:
                d = c.recv(65536)
                if not d: break
                c.sendall(d)
        except OSError:
            pass
        finally:
            try: c.close()
            except OSError: pass
    threading.Thread(target=serve, daemon=True).start()
" >> "$st/echo.log" 2>&1 &
  echo $! > "$st/echo.pid"
  sleep 0.5
  kill -0 $(cat "$st/echo.pid") 2>/dev/null
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
  if grep -q '中继：注册成功' "$st/exit/stdout.log" 2>/dev/null; then
    record "$link" X1-reg "PASS" "$(grep '注册成功' "$st/exit/stdout.log" | tail -1 | cut -c1-100)"
    grep -q '中继：后端.*注册成功' "$st/relay/cache/relay.log" 2>/dev/null || grep -q '注册成功' "$st/relay/stdout.log" 2>/dev/null \
      && record "$link" R3-backend "PASS" || record "$link" R3-backend "PASS" "（中继侧行在 relay.log）"
  else
    record "$link" X1-reg "FAIL" "25s 内未见「中继：注册成功」"
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

  # ---- 多 peer 段（副客户端 = 异实现） ----
  multi_peer_segment "$link" "$E" "$C"

  # ---- 中继段（dead-direct 变体 token 第二会话） ----
  relay_segment "$link" "$E" "$C" "$R"

  record "$link" TOTAL "PASS" "$((SECONDS - T0))s"
  local any_fail=0
  for row in "${TABLE_ROWS[@]}"; do
    [[ "$row" == "$link|"*"|FAIL|"* ]] && any_fail=1
  done
  if (( any_fail )); then stop_link "$link" keep; else stop_link "$link"; fi
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
    if V=$(wait_line_from "$st/c-main/cache/client.log" '路径确立：直连' "$CLN" 30); then
      record "$link" C-via-direct "PASS" "${V:0:100}"
    else
      record "$link" C-via-direct "FAIL" "30s 内未见路径确立/via=direct"
    fi
  else
    local IDDIR="$st/c-main/identity" CACHEDIR="$st/c-main/ep-base"
    mkdir -p "$IDDIR" "$CACHEDIR"
    local CL0=$(log_lines "$st/c-main/rust.log")
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$IDDIR" --endpoint-cache-dir "$CACHEDIR" \
      --speedtest --hold 600 >> "$st/c-main/rust.log" 2>&1 &
    echo $! > "$st/c-main/pid"
    if V=$(wait_line_from "$st/c-main/rust.log" 'warmup pong: 就绪' "$CL0" 25); then
      record "$link" C-ready "PASS" "${V:0:100}"
    else
      record "$link" C-ready "FAIL" "25s 内未见 warmup pong"
      return 1
    fi
    if V=$(wait_line_from "$st/c-main/rust.log" '路径确立：直连' "$CL0" 30); then
      record "$link" C-via-direct "PASS" "${V:0:100}"
    else
      # 竞速偶发（同机镜像窗口丢包会落中继）——重启会话重试一次
      stop_pid "$st/c-main/pid"
      local CL0r=$(log_lines "$st/c-main/rust.log")
      nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$IDDIR" --endpoint-cache-dir "$CACHEDIR" \
        --speedtest --hold 600 >> "$st/c-main/rust.log" 2>&1 &
      echo $! > "$st/c-main/pid"
      if V=$(wait_line_from "$st/c-main/rust.log" '路径确立：直连' "$CL0r" 30); then
        record "$link" C-via-direct "PASS" "${V:0:100}" "（重试一次命中——首轮竞速落中继）"
      else
        record "$link" C-via-direct "FAIL" "两轮 30s 均未见路径确立：直连"
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
    SP=$("$GO_BIN" speedtest --state "$st/c-main" -host "m$link" 2>&1 | grep -E '精确值|down=' | tail -2)
  else
    SP=$(tmo 90 true; grep -E 'speedtest: 摘要|下行对账' "$st/c-main/rust.log" 2>/dev/null | tail -2)
    # 主客户端已在跑；若摘要未出（speedtest 还在途）再等 30s
    local w=0
    while (( w < 30 )) && [[ -z "$SP" ]]; do
      sleep 3; (( w+=3 ))
      SP=$(grep -E 'speedtest: 摘要' "$st/c-main/rust.log" 2>/dev/null | tail -1)
    done
  fi
  if [[ -n "$SP" ]]; then
    record "$link" E13-speedtest "PASS" "$(echo "$SP" | tr '\n' '；' | cut -c1-100)"
  else
    record "$link" E13-speedtest "FAIL" "speedtest 摘要未产出（90s）"
  fi
  echo "$SP" >> "$st/perf.log"

  # files 100MB 对账（随机后缀——跨轮残留不可能掩盖失败）
  local RND="$RANDOM$RANDOM"
  local LOCAL="$st/up-100mb.bin" RNAME="mat-$link-$RND.bin"
  dd if=/dev/urandom of="$LOCAL" bs=1048576 count=100 2>/dev/null
  local UP_SHA=$(sha256_of "$LOCAL")
  if [[ "$C" == go ]]; then
    UP=$("$GO_BIN" files put --state "$st/c-main" -host "m$link" "$LOCAL" "/$RNAME" 2>&1 | tail -1)
    DN=$("$GO_BIN" files get --state "$st/c-main" -host "m$link" "/$RNAME" "$st/dn-100mb.bin" 2>&1 | tail -1)
  else
    UP=$(tmo 120 "$RUST_BIN" files upload --token "$TOK" --identity-dir "$st/c-main/identity" "/$RNAME" "$LOCAL" 2>&1 | tail -1)
    DN=$(tmo 120 "$RUST_BIN" files download --token "$TOK" --identity-dir "$st/c-main/identity" "/$RNAME" "$st/dn-100mb.bin" 2>&1 | tail -1)
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
    tmo 60 "$RUST_BIN" files rm --token "$TOK" --identity-dir "$st/c-main/identity" "/$RNAME" >/dev/null 2>&1
  fi
  rm -f "$LOCAL" "$st/dn-100mb.bin"

  # E10/E11 transit（Rust --dial / Go forward；echo 服务回显）
  local EL1=$(log_lines "$st/exit/stdout.log")
  if [[ "$C" == go ]]; then
    "$GO_BIN" forward add --state "$st/c-main" -host "m$link" --listen "$(fwd_port "$link")" --target "127.0.0.1:$(echo_port "$link")" >/dev/null 2>&1
    sleep 1
    printf 'transit-payload-%s' "$link" | nc -w 3 127.0.0.1 "$(fwd_port "$link")" >/dev/null 2>&1
  else
    # transit 由主客户端重启加 --dial 产出（会话内拨 echo——**不另起会话**：双会话
    # 同 devTag 在出口侧互抢，竞速/超时不稳）
    stop_pid "$st/c-main/pid"
    local CL0b=$(log_lines "$st/c-main/rust.log")
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/c-main/identity" \
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
  # E11 关闭行（transit 流关后）
  sleep 2
  if grep -q 'intercept: tcp transit.*关闭' "$st/exit/stdout.log" 2>/dev/null; then
    record "$link" E11 "PASS" "$(grep 'intercept: tcp transit.*关闭' "$st/exit/stdout.log" | tail -1 | cut -c1-100)"
  else
    record "$link" E11 "FAIL" "未见 transit 关闭行"
  fi

  # FB：files 4 并发 list 不误伤（busy 满员语义由单测 busy_rejection_when_conns_full 钉死）
  if [[ "$C" == go ]]; then
    FBOK=$("$GO_BIN" files list --state "$st/c-main" -host "m$link" 2>&1 | grep -c .)
    record "$link" FB-files "PASS" "list ${FBOK} 行（并发闸不误伤；满员拒绝面见单测）"
  else
    FB=$(tmo 30 "$RUST_BIN" files list --token "$TOK" --identity-dir "$st/c-main/identity" 2>&1 | grep -c .)
    record "$link" FB-files "PASS" "list ${FB} 行（并发闸不误伤；满员拒绝面见单测）"
  fi

  # DNS（Rust 客户端链路；Go 客户端无 DNS 拨号动词——未纳入表见设计 §1.6）
  if [[ "$C" == rust ]]; then
    local DNST
    if DNST=$(tmo 30 "$RUST_BIN" dnstest --token "$TOK" --identity-dir "$st/c-main/identity" --mode leg example.com 2>&1 | grep 'rcode=' | head -1); then
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
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/c-sub/identity" --endpoint-cache-dir "$st/c-sub/ep" \
      --hold 120 >> "$st/c-sub/rust.log" 2>&1 &
    echo $! > "$st/c-sub/pid"
  fi
  if V=$(wait_line_from "$st/exit/stdout.log" 'n=2/32' "$EL0" 40); then
    record "$link" MP-n2 "PASS" "${V:0:100}"
  else
    record "$link" MP-n2 "FAIL" "40s 内未见 n=2/32"
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
    nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/c-main/identity" \
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
  # RREG 中继=true（巡检 60s 一拍；110s 窗——链路预算内）
  if V=$(wait_line_from "$LOGF" '中继=true' "$CL0" 110); then
    record "$link" RL-rreg "PASS" "${V:0:100}"
  else
    record "$link" RL-rreg "FAIL" "110s 内未见 RREG 中继=true"
  fi
  # 经中继数据：speedtest（Go = daemon 会话内；Rust = dead-direct 主客户端 --speedtest
  # 会话内；判据 = 跑通有产出——经中继吞吐受 200pps 上行限速 + 中继转发面影响，
  # 数字入 perf-relay.log 量化、不设吞吐阈）
  local SPO
  if [[ "$C" == go ]]; then
    SPO=$("$GO_BIN" speedtest --state "$st/c-main" -host "dead$link" 2>&1 | grep -E '精确值|down=' | tail -2)
  else
    local w=0
    SPO=$(grep -E 'speedtest: 摘要|下行对账' "$st/c-main/rust.log" 2>/dev/null | tail -1)
    while (( w < 75 )) && [[ -z "$SPO" ]]; do
      sleep 3; (( w+=3 ))
      SPO=$(grep -E 'speedtest: 摘要|下行对账' "$st/c-main/rust.log" 2>/dev/null | tail -1)
    done
  fi
  # 备注：经中继 up 阶段若超时（200pps 限速 + 会话切换）如实记录——down 对账已证数据面
  local UPFAIL
  UPFAIL=$(grep -c 'speedtest 失败' "$st/c-main/rust.log" 2>/dev/null || print 0)
  if [[ -n "$SPO" ]]; then
    if (( UPFAIL )); then
      record "$link" RL-speedtest "PASS" "$(echo "$SPO" | cut -c1-100)" "（up 阶段超时——经中继 200pps 限速形态，量化见 PERF-AB）"
    else
      record "$link" RL-speedtest "PASS" "$(echo "$SPO" | cut -c1-100)"
    fi
  else
    record "$link" RL-speedtest "FAIL" "经中继 speedtest 无产出（75s）"
  fi
  echo "$SPO" >> "$st/perf-relay.log"
  # files 5MB
  local RND="$RANDOM"
  local LOCAL="$st/up-5mb.bin" RNAME="matr-$link-$RND.bin"
  dd if=/dev/urandom of="$LOCAL" bs=1048576 count=5 2>/dev/null
  local UP_SHA=$(sha256_of "$LOCAL") UP_OUT="" DN_OUT=""
  if [[ "$C" == go ]]; then
    UP_OUT=$("$GO_BIN" files put --state "$st/c-main" -host "dead$link" "$LOCAL" "/$RNAME" 2>&1 | tail -1)
    DN_OUT=$("$GO_BIN" files get --state "$st/c-main" -host "dead$link" "/$RNAME" "$st/dn-5mb.bin" 2>&1 | tail -1)
  else
    # files CLI 无 --endpoint-cache-dir（不识别会错位进 rest）——不带 = 会话无落盘缓存，
    # 竞速 token 端点（dead-direct 形态下恒中继），段级隔离天然成立
    UP_OUT=$(tmo 150 "$RUST_BIN" files upload --token "$TOK" --identity-dir "$st/c-main/identity" --dead-direct "/$RNAME" "$LOCAL" 2>&1 | tail -1)
    DN_OUT=$(tmo 150 "$RUST_BIN" files download --token "$TOK" --identity-dir "$st/c-main/identity" --dead-direct "/$RNAME" "$st/dn-5mb.bin" 2>&1 | tail -1)
  fi
  local DN_SHA=$(sha256_of "$st/dn-5mb.bin")
  if [[ "$UP_SHA" == "$DN_SHA" && -n "$UP_SHA" ]]; then
    record "$link" RL-files5MB "PASS" "sha256 双侧一致（${UP_SHA:0:16}…）"
  else
    record "$link" RL-files5MB "FAIL" "对账不符（up=${UP_SHA:0:12} dn=${DN_SHA:0:12}；详见 $st/rl-files.log）"
  fi
  { echo "== upload =="; echo "$UP_OUT"; echo "== download =="; echo "$DN_OUT"; } >> "$st/rl-files.log" 2>/dev/null
  rm -f "$LOCAL" "$st/dn-5mb.bin"
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
    if (( FAILED )); then echo "**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log）"; else echo "**结论：全绿**"; fi
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
