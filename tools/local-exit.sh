#!/bin/zsh
# local-exit.sh — 本地 Go 出口烟囱（R0.3；互操作测试一律用本脚本起的私有实例，绝不碰现役出口）
#
# 用法：tools/local-exit.sh <命令> [实例号]
#   实例号 n 缺省 1；出口 state = /tmp/homeway-rs-exit-n，WG UDP 端口 = 42640+n（错开默认 41641
#   ——本机 41641 是现役 launchd 出口，绝不能撞）。客户端 state = /tmp/homeway-rs-client-n。
#
# 命令：
#   start [n]        起出口（serve 前台单角色形态，一次性 flag 不写期望态：
#                    --listen、--upnp=false、--stun/--stun6 关、--public-endpoint 127.0.0.1:P
#                    ⇒ 跳过公网探测立即公布回环端点；--verbose 全量日志落 <state>/stdout.log）
#   stop [n]         停出口（SIGTERM；等退出）
#   token [n]        打印当前 token（`serve token` 纯读命令，进程未跑也能取台账末行）
#   status [n]       pid / 实际监听端口（listen_port.txt）/ 就绪判据行
#   log [n] [行数]   tail 出口 stdout.log（缺省 40 行）
#   wipe [n]         删出口 state（身份密钥一并删 ⇒ 下次 token 变）
#   client-start [n] 起客户端统一进程（config 预置 serve/relay enabled=false——全新 state 默认
#                    serve.enabled=true 会在 41641 起出口撞现役出口，必须先写 config）
#   client-add [n]   host add（自动取本脚本 token；产生出口侧 peer: + 与客户端 link: 判据行）
#   client-stop [n]  停客户端
#   client-log [n]   tail 客户端 stdout.log
#
# 二进制：缺省 ../../bin/homeway-go；不在则从 baseline 克隆构建（GOTOOLCHAIN=go1.24.5）。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
BIN="${HOMEWAY_GO:-$REPO_ROOT/bin/homeway-go}"

if [[ ! -x "$BIN" ]]; then
  echo "==> $BIN 不在，从 baseline 克隆构建" >&2
  (cd "$REPO_ROOT/baseline/homeway" && GOTOOLCHAIN=go1.24.5 go build -o "$BIN" ./cmd/homeway) || exit 1
fi

cmd="${1:-help}"; n="${2:-1}"
EXIT_STATE="/tmp/homeway-rs-exit-$n"
CLIENT_STATE="/tmp/homeway-rs-client-$n"
EXIT_PORT=$((42640 + n))
EXIT_LOG="$EXIT_STATE/stdout.log"
CLIENT_LOG="$CLIENT_STATE/stdout.log"
EXIT_PIDFILE="$EXIT_STATE/pid"
CLIENT_PIDFILE="$CLIENT_STATE/pid"

alive() { [[ -f "$1" ]] && kill -0 "$(cat "$1")" 2>/dev/null; }

wait_line() {  # wait_line <文件> <模式> [秒] —— 等日志里出现判据行
  local file="$1" pat="$2" secs="${3:-20}" i=0
  while (( i < secs )); do
    grep -m1 "$pat" "$file" 2>/dev/null && return 0
    sleep 1; (( i++ ))
  done
  return 1
}

case "$cmd" in
start)
  if alive "$EXIT_PIDFILE"; then echo "出口 #$n 已在跑（pid=$(cat "$EXIT_PIDFILE")）"; exit 0; fi
  mkdir -p "$EXIT_STATE"
  echo "==> 起出口 #$n：state=$EXIT_STATE wg=127.0.0.1:$EXIT_PORT"
  nohup "$BIN" serve --state "$EXIT_STATE" --listen "$EXIT_PORT" \
    --upnp=false --stun= --stun6= --public-endpoint "127.0.0.1:$EXIT_PORT" --verbose \
    >> "$EXIT_LOG" 2>&1 &
  echo $! > "$EXIT_PIDFILE"
  if wait_line "$EXIT_LOG" 'serve 就绪' 25; then
    echo "==> 就绪。token："; "$0" token "$n"
  else
    echo "!! 25s 内未见「serve 就绪」——看日志：$0 log $n" >&2; tail -20 "$EXIT_LOG" >&2; exit 1
  fi
  ;;
stop)
  if alive "$EXIT_PIDFILE"; then
    local_pid=$(cat "$EXIT_PIDFILE"); kill -TERM "$local_pid" 2>/dev/null
    for i in {1..10}; do kill -0 "$local_pid" 2>/dev/null || break; sleep 1; done
    kill -0 "$local_pid" 2>/dev/null && kill -KILL "$local_pid" 2>/dev/null
    echo "==> 出口 #$n 已停（pid=$local_pid）"
  else
    echo "出口 #$n 未在跑"; fi
  rm -f "$EXIT_PIDFILE"
  ;;
token)
  "$BIN" serve token --state "$EXIT_STATE"
  ;;
status)
  if alive "$EXIT_PIDFILE"; then
    echo "pid=$(cat "$EXIT_PIDFILE") 配置端口=$EXIT_PORT 实际=$(cat "$EXIT_STATE/serve/listen_port.txt" 2>/dev/null || echo '?')"
    grep -m1 'serve 就绪' "$EXIT_LOG" 2>/dev/null
    grep -m1 '过境拦截就绪' "$EXIT_LOG" 2>/dev/null
    grep -m1 'peer: +' "$EXIT_LOG" 2>/dev/null || echo '（尚无 peer 注册）'
  else echo "出口 #$n 未在跑"; fi
  ;;
log)  tail -"${3:-40}" "$EXIT_LOG" ;;
wipe) "$0" stop "$n"; rm -rf "$EXIT_STATE"; echo "==> 已删 $EXIT_STATE" ;;
client-start)
  if alive "$CLIENT_PIDFILE"; then echo "客户端 #$n 已在跑（pid=$(cat "$CLIENT_PIDFILE")）"; exit 0; fi
  mkdir -p "$CLIENT_STATE"
  if [[ ! -f "$CLIENT_STATE/config.toml" ]]; then
    cat > "$CLIENT_STATE/config.toml" <<EOF
# homeway-rs 本地测试客户端（local-exit.sh 生成）——双角色全关，只跑 client+控制面
[serve]
enabled = false
[relay]
enabled = false
EOF
  fi
  echo "==> 起客户端统一进程 #$n：state=$CLIENT_STATE"
  nohup "$BIN" --state "$CLIENT_STATE" --verbose >> "$CLIENT_LOG" 2>&1 &
  echo $! > "$CLIENT_PIDFILE"
  sleep 1; kill -0 "$(cat "$CLIENT_PIDFILE")" 2>/dev/null || { echo "!! 客户端启动即退出，看 $CLIENT_LOG" >&2; exit 1; }
  echo "==> 客户端 pid=$(cat "$CLIENT_PIDFILE")"
  ;;
client-add)
  tok=$("$BIN" serve token --state "$EXIT_STATE" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
  [[ -n "$tok" ]] || { echo "!! 取不到出口 #$n 的 token（先 start）" >&2; exit 1; }
  echo "==> host add（token 已掩码取用，长度 ${#tok}）"
  "$BIN" host add --state "$CLIENT_STATE" --name "local$n" "$tok"
  ;;
client-stop)
  if alive "$CLIENT_PIDFILE"; then
    local_pid=$(cat "$CLIENT_PIDFILE"); kill -TERM "$local_pid" 2>/dev/null; sleep 2
    kill -0 "$local_pid" 2>/dev/null && kill -KILL "$local_pid" 2>/dev/null
    echo "==> 客户端 #$n 已停"
  else echo "客户端 #$n 未在跑"; fi
  rm -f "$CLIENT_PIDFILE"
  ;;
client-log) tail -"${3:-40}" "$CLIENT_LOG" ;;
*) sed -n '2,20p' "$0"; exit 1 ;;
esac
