#!/bin/zsh
# local-rust-exit.sh — 本地 **Rust** 出口烟囱（R3-3f；互操作测试用本脚本起的私有实例，
# 绝不碰现役出口——同 local-exit.sh 的隔离条款，端口再错开一段：Rust 出口 4265x）
#
# 用法：tools/local-rust-exit.sh <命令> [实例号]
#   实例号 n 缺省 1；出口 state = /tmp/homeway-rs-rustexit-n，WG UDP 端口 = 42650+n。
#
# 命令：
#   start [n]     起 Rust 出口（release 构建；--bind-interface none --upnp=false
#                 --stun= --public-endpoint 127.0.0.1:P；EXIT_EXTRA_FLAGS 可追加：
#                 --peer-ttl 15s 注入等。就绪判定含端口退让检查）
#   stop [n]      停出口（SIGTERM；D5 收工序后清 pidfile）
#   token [n]     打印当前 token（serve token 纯读——台账末行）
#   status [n]    pid / 实际端口 / 判据行采样
#   log [n] [行数] tail stdout
#   wipe [n]      停 + 删 state
#   go-client-add [n]  Go 客户端（baseline 克隆构建的统一进程）host add 到本 Rust
#                      出口——客户端 state 复用 local-exit.sh 的 client-start 形态
#                      （serve/relay 双关断言）；等「就绪（会话在位）」判据行
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
BIN="$REPO_ROOT/target/release/homeway-cli"
GO_BIN="${HOMEWAY_GO:-$REPO_ROOT/bin/homeway-go}"

cmd="${1:-help}"; n="${2:-1}"
EXIT_STATE="/tmp/homeway-rs-rustexit-$n"
CLIENT_STATE="/tmp/homeway-rs-rustgo-client-$n"
EXIT_PORT=$((42650 + n))
EXIT_LOG="$EXIT_STATE/stdout.log"
CLIENT_LOG="$CLIENT_STATE/stdout.log"
EXIT_PIDFILE="$EXIT_STATE/pid"
CLIENT_PIDFILE="$CLIENT_STATE/pid"

build_bin() {
  echo "==> cargo build --release -p homeway-cli" >&2
  (cd "$REPO_ROOT" && cargo build --release -p homeway-cli) || exit 1
}

our_pid() {
  # pattern 缺省 homeway-cli（Rust 出口）；go 客户端侧传 homeway-go 形态
  local pf="$1" p pat="${2:-homeway-cli}"
  [[ -f "$pf" ]] || return 1
  p=$(cat "$pf" 2>/dev/null) || return 1
  kill -0 "$p" 2>/dev/null || return 1
  [[ "$(ps -p "$p" -o comm= 2>/dev/null)" == *"${pat}"* ]] || return 1
  REPLY_PID=$p
}

wait_line_from() {
  local file="$1" pat="$2" start_line="$3" secs="${4:-25}" i=0
  while (( i < secs )); do
    out=$(tail -n +"$((start_line + 1))" "$file" 2>/dev/null | grep -m1 "$pat") && { print -r -- "$out"; return 0; }
    sleep 1; (( i++ ))
  done
  return 1
}

log_lines() { [[ -f "$1" ]] && wc -l < "$1" | tr -d ' ' || print 0; }

case "$cmd" in
start)
  if our_pid "$EXIT_PIDFILE"; then echo "Rust 出口 #$n 已在跑（pid=$REPLY_PID）"; exit 0; fi
  [[ -x "$BIN" ]] || build_bin
  mkdir -p "$EXIT_STATE" || exit 1
  LOG0=$(log_lines "$EXIT_LOG")
  echo "==> 起 Rust 出口 #$n：state=$EXIT_STATE wg=127.0.0.1:$EXIT_PORT（本轮日志从第 $((LOG0 + 1)) 行起）"
  nohup "$BIN" serve --state "$EXIT_STATE" --listen "$EXIT_PORT" --bind-interface none \
    --upnp=false --stun= --public-endpoint "127.0.0.1:$EXIT_PORT" --verbose \
    ${=EXIT_EXTRA_FLAGS:-} \
    >> "$EXIT_LOG" 2>&1 &
  echo $! > "$EXIT_PIDFILE"
  if ready=$(wait_line_from "$EXIT_LOG" 'serve 就绪' "$LOG0" 25); then
    actual=$(cat "$EXIT_STATE/cache/listen_port.txt" 2>/dev/null)
    if [[ "$actual" != "$EXIT_PORT" ]]; then
      echo "!! 实际监听端口 $actual ≠ 配置 $EXIT_PORT（被占用退让）——判据会指向占用者，拒绝继续。" >&2
      "$0" stop "$n" >/dev/null 2>&1; exit 1
    fi
    echo "==> 就绪（端口无退让）。token："; "$0" token "$n"
  else
    echo "!! 25s 内未见「serve 就绪」——看日志：$0 log $n" >&2; tail -8 "$EXIT_LOG" >&2; exit 1
  fi
  ;;
stop)
  if our_pid "$EXIT_PIDFILE"; then
    local_pid=$REPLY_PID
    kill -TERM "$local_pid" 2>/dev/null
    for i in {1..15}; do kill -0 "$local_pid" 2>/dev/null || break; sleep 1; done
    if kill -0 "$local_pid" 2>/dev/null; then
      kill -KILL "$local_pid" 2>/dev/null; sleep 1
    fi
    echo "==> Rust 出口 #$n 已停（pid=$local_pid）"
  else echo "Rust 出口 #$n 未在跑"; fi
  rm -f "$EXIT_PIDFILE"
  ;;
token)
  "$BIN" serve token --state "$EXIT_STATE"
  ;;
status)
  if our_pid "$EXIT_PIDFILE"; then
    echo "pid=$REPLY_PID 配置端口=$EXIT_PORT 实际=$(cat "$EXIT_STATE/cache/listen_port.txt" 2>/dev/null || echo '?')"
    grep 'serve 就绪' "$EXIT_LOG" 2>/dev/null | tail -1
    grep '过境拦截就绪' "$EXIT_LOG" 2>/dev/null | tail -1
    grep 'peer: +' "$EXIT_LOG" 2>/dev/null | tail -1 || echo '（尚无 peer 注册）'
  else echo "Rust 出口 #$n 未在跑"; fi
  ;;
log)  tail -"${3:-40}" "$EXIT_LOG" ;;
wipe) "$0" stop "$n"; rm -rf "$EXIT_STATE"; echo "==> 已删 $EXIT_STATE" ;;
go-client-add)
  # Go 客户端统一进程（serve/relay 双关——同 local-exit.sh client-start 的断言纪律）
  [[ -x "$GO_BIN" ]] || { echo "!! $GO_BIN 不在（先 tools/local-exit.sh start 1 触发构建或手工构建）" >&2; exit 1; }
  if ! our_pid "$CLIENT_PIDFILE" homeway; then
    mkdir -p "$CLIENT_STATE" || exit 1
    cat > "$CLIENT_STATE/config.toml" <<EOF
[serve]
enabled = false
[relay]
enabled = false
EOF
    grep -q '^enabled = false' "$CLIENT_STATE/config.toml" || { echo "!! config.toml 写入失败" >&2; exit 1; }
    [[ $(grep -c '^enabled = false' "$CLIENT_STATE/config.toml") -eq 2 ]] || { echo "!! config.toml 断言失败" >&2; exit 1; }
    echo "==> 起 Go 客户端统一进程 #$n（对 Rust 出口）：state=$CLIENT_STATE"
    nohup "$GO_BIN" --state "$CLIENT_STATE" --verbose >> "$CLIENT_LOG" 2>&1 &
    echo $! > "$CLIENT_PIDFILE"
    sleep 1
    our_pid "$CLIENT_PIDFILE" || { echo "!! 客户端启动即退出，看 $CLIENT_LOG" >&2; exit 1; }
    echo "==> 客户端 pid=$REPLY_PID"
  fi
  tok=$("$0" token "$n" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
  [[ -n "$tok" ]] || { echo "!! 取不到 Rust 出口 #$n 的 token（先 start）" >&2; exit 1; }
  echo "==> host add（token 已掩码取用，长度 ${#tok}）"
  CL0=$(log_lines "$CLIENT_STATE/cache/client.log")
  "$GO_BIN" host add --state "$CLIENT_STATE" --name "rustexit$n" "$tok" || exit 1
  if ready=$(wait_line_from "$CLIENT_STATE/cache/client.log" '就绪（会话在位' "$CL0" 25); then
    echo "==> $ready"
  else
    echo "!! 25s 内未见客户端「就绪（会话在位）」——看：tail -40 $CLIENT_LOG" >&2; exit 1
  fi
  ;;
go-client-stop)
  if our_pid "$CLIENT_PIDFILE" homeway; then
    local_pid=$REPLY_PID
    kill -TERM "$local_pid" 2>/dev/null
    for i in {1..10}; do kill -0 "$local_pid" 2>/dev/null || break; sleep 1; done
    kill -0 "$local_pid" 2>/dev/null && kill -KILL "$local_pid" 2>/dev/null
    echo "==> Go 客户端 #$n 已停"
  else echo "Go 客户端 #$n 未在跑"; fi
  rm -f "$CLIENT_PIDFILE"
  ;;
go-client-log) tail -"${3:-40}" "$CLIENT_LOG" ;;
go-client-wipe) "$0" go-client-stop "$n"; rm -rf "$CLIENT_STATE"; echo "==> 已删 $CLIENT_STATE" ;;
*) sed -n '2,28p' "$0"; exit 1 ;;
esac
