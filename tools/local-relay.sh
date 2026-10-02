#!/bin/zsh
# local-relay.sh — 本地 Go 中继（R2.2d；隔离条款执行面：state 全在 /tmp、端口 4274x
# 错开现役阿里云 41741、绝不碰现役中继/出口）。
#
# 用法：tools/local-relay.sh <命令> [实例号]
#   实例号 n 缺省 1；中继 state = /tmp/homeway-rs-relay-n，UDP 监听 = 42740+n。
#
# 命令：
#   start [n]        起中继（homeway relay --listen 127.0.0.1:PORT --advertise 127.0.0.1:PORT）
#   stop [n]         停中继
#   token [n]        打印 rl1 token（grep 日志——relay 无 token 子命令，启动日志里打）
#   status [n]       pid / 端口 / 注册腿行
#   log [n] [行数]   tail 中继日志
#   wipe [n]         删中继 state
#
# 二进制：与 local-exit.sh 共用 bin/homeway-go（同一基线门）。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
BIN="${HOMEWAY_GO:-$REPO_ROOT/bin/homeway-go}"
CLONE="$REPO_ROOT/baseline/homeway"

[[ -x "$BIN" ]] || { echo "!! $BIN 不在（先跑 tools/local-exit.sh start 触发构建）" >&2; exit 1; }

cmd="${1:-help}"; n="${2:-1}"
RELAY_STATE="/tmp/homeway-rs-relay-$n"
RELAY_PORT=$((42740 + n))
RELAY_LOG="$RELAY_STATE/cache/relay.log"
RELAY_PIDFILE="$RELAY_STATE/pid"

our_pid() {
  local pf="$1" p
  [[ -f "$pf" ]] || return 1
  p=$(cat "$pf" 2>/dev/null) || return 1
  kill -0 "$p" 2>/dev/null || return 1
  [[ "$(ps -p "$p" -o comm= 2>/dev/null)" == *homeway* ]] || return 1
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
  if our_pid "$RELAY_PIDFILE"; then echo "中继 #$n 已在跑（pid=$REPLY_PID）"; exit 0; fi
  mkdir -p "$RELAY_STATE/cache" || exit 1
  echo "==> 起中继 #$n：state=$RELAY_STATE udp=127.0.0.1:$RELAY_PORT"
  nohup "$BIN" relay --state "$RELAY_STATE" \
    --listen "127.0.0.1:$RELAY_PORT" --advertise "127.0.0.1:$RELAY_PORT" \
    >> "$RELAY_STATE/stdout.log" 2>&1 &
  echo $! > "$RELAY_PIDFILE"
  sleep 1
  our_pid "$RELAY_PIDFILE" || { echo "!! 中继启动即退出，看 $RELAY_STATE/stdout.log" >&2; tail -5 "$RELAY_STATE/stdout.log" >&2; exit 1; }
  if wait_line "$RELAY_LOG" '中继.*监听|中继就绪|relay' 10; then :; fi
  echo "==> 中继 pid=$REPLY_PID。rl1 token："
  "$0" token "$n"
  ;;
stop)
  if our_pid "$RELAY_PIDFILE"; then
    local_pid=$REPLY_PID
    kill -TERM "$local_pid" 2>/dev/null
    for i in {1..8}; do kill -0 "$local_pid" 2>/dev/null || break; sleep 1; done
    kill -0 "$local_pid" 2>/dev/null && kill -KILL "$local_pid" 2>/dev/null
    echo "==> 中继 #$n 已停（pid=$local_pid）"
  else echo "中继 #$n 未在跑"; fi
  rm -f "$RELAY_PIDFILE"
  ;;
token)
  # rl1 只在启动日志打一轮；stdout.log 与 relay.log 都可能承载
  tok=$(grep -o 'rl1[A-Za-z0-9+/=_-]*' "$RELAY_STATE/stdout.log" "$RELAY_LOG" 2>/dev/null | head -1 | grep -o 'rl1[A-Za-z0-9+/=_-]*')
  [[ -n "$tok" ]] || { echo "!! 取不到 rl1 token（先 start）" >&2; exit 1; }
  print -r -- "$tok"
  ;;
status)
  if our_pid "$RELAY_PIDFILE"; then
    echo "pid=$REPLY_PID udp=127.0.0.1:$RELAY_PORT"
    grep -E '注册|后端' "$RELAY_LOG" 2>/dev/null | tail -3
  else echo "中继 #$n 未在跑"; fi
  ;;
log) tail -"${3:-40}" "$RELAY_LOG" 2>/dev/null || tail -"${3:-40}" "$RELAY_STATE/stdout.log" 2>/dev/null ;;
wipe) "$0" stop "$n"; rm -rf "$RELAY_STATE"; echo "==> 已删 $RELAY_STATE" ;;
*) sed -n '2,20p' "$0"; exit 1 ;;
esac
