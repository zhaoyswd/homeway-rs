#!/bin/zsh
# local-rust-relay.sh — 本地 **Rust** 中继（R4-4d；互操作测试用本脚本起的私有实例，
# 绝不碰现役中继/出口——同 local-relay.sh 隔离条款，state 全在 /tmp、端口 4278x 段错开）
#
# 用法：tools/local-rust-relay.sh <命令> [实例号]
#   实例号 n 缺省 1；中继 state = /tmp/homeway-rs-rustrelay-n，UDP 监听 = 42780+n。
#
# 命令：
#   start [n]        起 Rust 中继（release 构建；--advertise 127.0.0.1:PORT）
#   stop [n]         停中继
#   token [n]        打印 rl1 token（grep 终端输出——与 Go 中继同纪律：启动时打一轮）
#   status [n]       pid / 端口 / 注册腿与控制面判据行采样
#   log [n] [行数]   tail relay.log（运行日志；终端只有 token 与端点公告）
#   wipe [n]         停 + 删中继 state
#
# 二进制：target/release/homeway-cli（先 build）。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
BIN="$REPO_ROOT/target/release/homeway-cli"

cmd="${1:-help}"; n="${2:-1}"
RELAY_STATE="/tmp/homeway-rs-rustrelay-$n"
RELAY_PORT=$((42780 + n))
RELAY_LOG="$RELAY_STATE/cache/relay.log"
RELAY_PIDFILE="$RELAY_STATE/pid"

build_bin() {
  echo "==> cargo build --release -p homeway-cli" >&2
  (cd "$REPO_ROOT" && cargo build --release -p homeway-cli) || exit 1
}

our_pid() {
  local pf="$1" p
  [[ -f "$pf" ]] || return 1
  p=$(cat "$pf" 2>/dev/null) || return 1
  kill -0 "$p" 2>/dev/null || return 1
  [[ "$(ps -p "$p" -o comm= 2>/dev/null)" == *homeway-cli* ]] || return 1
  REPLY_PID=$p
}

wait_line() {
  local file="$1" pat="$2" secs="${3:-20}" i=0
  while (( i < secs )); do
    grep -m1 "$pat" "$file" 2>/dev/null && return 0
    sleep 1; (( i++ ))
  done
  return 1
}

case "$cmd" in
start)
  if our_pid "$RELAY_PIDFILE"; then echo "Rust 中继 #$n 已在跑（pid=$REPLY_PID）"; exit 0; fi
  [[ -x "$BIN" ]] || build_bin
  mkdir -p "$RELAY_STATE/cache" || exit 1
  echo "==> 起 Rust 中继 #$n：state=$RELAY_STATE udp=127.0.0.1:$RELAY_PORT"
  nohup "$BIN" relay --state "$RELAY_STATE" \
    --listen "127.0.0.1:$RELAY_PORT" --advertise "127.0.0.1:$RELAY_PORT" \
    >> "$RELAY_STATE/stdout.log" 2>&1 &
  echo $! > "$RELAY_PIDFILE"
  sleep 1
  our_pid "$RELAY_PIDFILE" || { echo "!! 中继启动即退出，看 $RELAY_STATE/stdout.log" 2>&1; tail -5 "$RELAY_STATE/stdout.log" >&2; exit 1; }
  if wait_line "$RELAY_LOG" '中继就绪' 10; then :; fi
  echo "==> 中继 pid=$REPLY_PID。rl1 token："
  "$0" token "$n"
  ;;
stop)
  if our_pid "$RELAY_PIDFILE"; then
    local_pid=$REPLY_PID
    kill -TERM "$local_pid" 2>/dev/null
    for i in {1..8}; do kill -0 "$local_pid" 2>/dev/null || break; sleep 1; done
    kill -0 "$local_pid" 2>/dev/null && kill -KILL "$local_pid" 2>/dev/null
    echo "==> Rust 中继 #$n 已停（pid=$local_pid）"
  else echo "Rust 中继 #$n 未在跑"; fi
  rm -f "$RELAY_PIDFILE"
  ;;
token)
  # rl1 只在启动时打一轮；stdout.log 与 relay.log 都可能承载
  tok=$(grep -o 'rl1[A-Za-z0-9+/=_-]*' "$RELAY_STATE/stdout.log" "$RELAY_LOG" 2>/dev/null | head -1 | grep -o 'rl1[A-Za-z0-9+/=_-]*')
  [[ -n "$tok" ]] || { echo "!! 取不到 rl1 token（先 start）" 2>&1; exit 1; }
  print -r -- "$tok"
  ;;
status)
  if our_pid "$RELAY_PIDFILE"; then
    echo "pid=$REPLY_PID udp=127.0.0.1:$RELAY_PORT"
    grep -E '注册|控制面|会话|回收' "$RELAY_LOG" 2>/dev/null | tail -6
  else echo "Rust 中继 #$n 未在跑"; fi
  ;;
log) tail -"${3:-40}" "$RELAY_LOG" 2>/dev/null || tail -"${3:-40}" "$RELAY_STATE/stdout.log" 2>/dev/null ;;
wipe) "$0" stop "$n"; rm -rf "$RELAY_STATE"; echo "==> 已删 $RELAY_STATE" ;;
*) sed -n '2,20p' "$0"; exit 1 ;;
esac
