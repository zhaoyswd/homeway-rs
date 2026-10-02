#!/bin/zsh
# local-exit.sh — 本地 Go 出口烟囱（R0.3；互操作测试一律用本脚本起的私有实例，绝不碰现役出口）
#
# ⚠️ 本脚本是隔离条款（ROADMAP）第 2/3 条的**唯一执行面**：客户端/出口 state 全在 /tmp、
# 端口错开现役 41641、UPnP/STUN 全关、绝不触碰 launchd 与生产 state。改本脚本须复核这四点。
#
# 用法：tools/local-exit.sh <命令> [实例号]
#   实例号 n 缺省 1；出口 state = /tmp/homeway-rs-exit-n，WG UDP 端口 = 42640+n。
#
# 命令：
#   start [n]        起出口（serve 前台单角色形态，一次性 flag 不写期望态：
#                    --listen、--bind-interface none（不钉物理卡，只回环可达）、
#                    --upnp=false、--stun/--stun6 关、--public-endpoint 127.0.0.1:P。
#                    就绪判定含**端口退让检查**：listen_port.txt ≠ 配置端口即失败退出）
#   stop [n]         停出口（SIGTERM，收尾确认后才清 pidfile）
#   token [n]        打印当前 token（`serve token` 纯读命令，进程未跑也能取台账末行）
#   status [n]       pid / 实际监听端口 / 本轮就绪判据行
#   log [n] [行数]   tail 出口 stdout.log
#   wipe [n]         删出口 state
#   client-start [n] 起客户端统一进程（**无条件重写** config.toml 双角色全关——全新 state
#                    的默认 serve.enabled=true 会在 41641 起出口撞现役出口，写后断言再启动）
#   client-add [n]   host add（等客户端「就绪（会话在位）」判据行）
#   client-stop [n]  停客户端
#   client-wipe [n]  删客户端 state
#   client-log [n]   tail 客户端 stdout.log
#
# 二进制：缺省 ../../bin/homeway-go；不在或克隆 HEAD 与 bin/homeway-go.baseline 记录的
# 构建基线不一致时，自动从 baseline 克隆重建（GOTOOLCHAIN=go1.24.5，防旧二进制污染判据）。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
BIN="${HOMEWAY_GO:-$REPO_ROOT/bin/homeway-go}"
BIN_BASELINE_MARK="$REPO_ROOT/bin/homeway-go.baseline"
CLONE="$REPO_ROOT/baseline/homeway"

build_bin() {  # 从克隆重建并记录基线 hash（M6：防升级基线后旧二进制继续被当判据真源）
  (cd "$CLONE" && GOTOOLCHAIN=go1.24.5 go build -o "$BIN" ./cmd/homeway) || return 1
  git -C "$CLONE" rev-parse HEAD > "$BIN_BASELINE_MARK"
}

ensure_bin() {
  [[ -x "$BIN" ]] || { echo "==> $BIN 不在，从 baseline 克隆构建" >&2; build_bin || exit 1; return; }
  if [[ -d "$CLONE" ]]; then
    local want have
    want=$(git -C "$CLONE" rev-parse HEAD 2>/dev/null) || return 0
    have=$(cat "$BIN_BASELINE_MARK" 2>/dev/null)
    if [[ "$want" != "$have" ]]; then
      echo "==> 克隆 HEAD（$want）≠ 二进制构建基线（$have）——重建" >&2
      build_bin || exit 1
    fi
  fi
}
ensure_bin

cmd="${1:-help}"; n="${2:-1}"
EXIT_STATE="/tmp/homeway-rs-exit-$n"
CLIENT_STATE="/tmp/homeway-rs-client-$n"
EXIT_PORT=$((42640 + n))
EXIT_LOG="$EXIT_STATE/stdout.log"
CLIENT_LOG="$CLIENT_STATE/stdout.log"
EXIT_PIDFILE="$EXIT_STATE/pid"
CLIENT_PIDFILE="$CLIENT_STATE/pid"

our_pid() {  # pid 存活**且**命令名含 homeway（防 pid 复用误判，L9）
  local pf="$1" p
  [[ -f "$pf" ]] || return 1
  p=$(cat "$pf" 2>/dev/null) || return 1
  kill -0 "$p" 2>/dev/null || return 1
  [[ "$(ps -p "$p" -o comm= 2>/dev/null)" == *homeway* ]] || return 1
  REPLY_PID=$p
}

wait_line_from() {  # 只看本轮新增行（M12：日志跨轮追加，整文件 grep 会拿上一轮的行假就绪）
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
  if our_pid "$EXIT_PIDFILE"; then echo "出口 #$n 已在跑（pid=$REPLY_PID）"; exit 0; fi
  mkdir -p "$EXIT_STATE" || exit 1
  LOG0=$(log_lines "$EXIT_LOG")
  echo "==> 起出口 #$n：state=$EXIT_STATE wg=127.0.0.1:$EXIT_PORT（本轮日志从第 $((LOG0 + 1)) 行起）"
  nohup "$BIN" serve --state "$EXIT_STATE" --listen "$EXIT_PORT" --bind-interface none \
    --upnp=false --stun= --stun6= --public-endpoint "127.0.0.1:$EXIT_PORT" --verbose \
    >> "$EXIT_LOG" 2>&1 &
  echo $! > "$EXIT_PIDFILE"
  if ready=$(wait_line_from "$EXIT_LOG" 'serve 就绪' "$LOG0" 25); then
    # 端口退让检查（M13）：退让后 token 公布的仍是配置端口 ⇒ 判据指向无关进程，必须硬失败
    actual=$(cat "$EXIT_STATE/cache/listen_port.txt" 2>/dev/null)
    if [[ "$actual" != "$EXIT_PORT" ]]; then
      echo "!! 实际监听端口 $actual ≠ 配置 $EXIT_PORT（被占用退让）——判据会指向占用者，拒绝继续。换实例号或清理占用。" >&2
      "$0" stop "$n" >/dev/null 2>&1
      exit 1
    fi
    echo "==> 就绪（端口无退让）。token："; "$0" token "$n"
  else
    echo "!! 25s 内未见「serve 就绪」——看日志：$0 log $n" >&2; tail -5 "$EXIT_LOG" >&2; exit 1
  fi
  ;;
stop)
  if our_pid "$EXIT_PIDFILE"; then
    local_pid=$REPLY_PID
    kill -TERM "$local_pid" 2>/dev/null
    for i in {1..10}; do kill -0 "$local_pid" 2>/dev/null || break; sleep 1; done
    if kill -0 "$local_pid" 2>/dev/null; then
      kill -KILL "$local_pid" 2>/dev/null; sleep 1
    fi
    kill -0 "$local_pid" 2>/dev/null && { echo "!! pid=$local_pid 停不干净，保留 pidfile 供排查" >&2; exit 1; }
    echo "==> 出口 #$n 已停（pid=$local_pid）"
  else
    echo "出口 #$n 未在跑"; fi
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
  else echo "出口 #$n 未在跑"; fi
  ;;
log)  tail -"${3:-40}" "$EXIT_LOG" ;;
wipe) "$0" stop "$n"; rm -rf "$EXIT_STATE"; echo "==> 已删 $EXIT_STATE" ;;
client-start)
  if our_pid "$CLIENT_PIDFILE"; then echo "客户端 #$n 已在跑（pid=$REPLY_PID）"; exit 0; fi
  mkdir -p "$CLIENT_STATE" || exit 1
  # H1：无条件重写（幂等覆盖）+ 写后断言。全新 state 的默认 serve.enabled=true 会在
  # 41641 起真出口撞现役 launchd 实例（并改写其 UPnP 映射）——这是本脚本的最高风险面。
  cat > "$CLIENT_STATE/config.toml" <<EOF
# homeway-rs 本地测试客户端（local-exit.sh 生成）——双角色全关，只跑 client+控制面
[serve]
enabled = false
[relay]
enabled = false
EOF
  grep -q '^enabled = false' "$CLIENT_STATE/config.toml" || { echo "!! config.toml 写入失败" >&2; exit 1; }
  [[ $(grep -c '^enabled = false' "$CLIENT_STATE/config.toml") -eq 2 ]] || { echo "!! config.toml 断言失败（serve/relay 应双关）" >&2; exit 1; }
  echo "==> 起客户端统一进程 #$n：state=$CLIENT_STATE（serve/relay 已断言全关）"
  nohup "$BIN" --state "$CLIENT_STATE" --verbose >> "$CLIENT_LOG" 2>&1 &
  echo $! > "$CLIENT_PIDFILE"
  sleep 1
  our_pid "$CLIENT_PIDFILE" || { echo "!! 客户端启动即退出，看 $CLIENT_LOG" >&2; exit 1; }
  echo "==> 客户端 pid=$REPLY_PID"
  ;;
client-add)
  tok=$("$BIN" serve token --state "$EXIT_STATE" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
  [[ -n "$tok" ]] || { echo "!! 取不到出口 #$n 的 token（先 start）" >&2; exit 1; }
  echo "==> host add（token 已掩码取用，长度 ${#tok}）"
  CL0=$(log_lines "$CLIENT_STATE/cache/client.log")
  "$BIN" host add --state "$CLIENT_STATE" --name "local$n" "$tok" || exit 1
  # 服务会话就绪判据（M1：服务会话形态的核心「就绪」行，此前漏等）
  if ready=$(wait_line_from "$CLIENT_STATE/cache/client.log" '就绪（会话在位' "$CL0" 20); then
    echo "==> $ready"
  else
    echo "!! 20s 内未见客户端「就绪（会话在位）」——看：$0 client-log $n" >&2; exit 1
  fi
  ;;
client-stop)
  if our_pid "$CLIENT_PIDFILE"; then
    local_pid=$REPLY_PID
    kill -TERM "$local_pid" 2>/dev/null
    for i in {1..10}; do kill -0 "$local_pid" 2>/dev/null || break; sleep 1; done
    kill -0 "$local_pid" 2>/dev/null && kill -KILL "$local_pid" 2>/dev/null
    echo "==> 客户端 #$n 已停"
  else echo "客户端 #$n 未在跑"; fi
  rm -f "$CLIENT_PIDFILE"
  ;;
client-wipe) "$0" client-stop "$n"; rm -rf "$CLIENT_STATE"; echo "==> 已删 $CLIENT_STATE" ;;
client-log) tail -"${3:-40}" "$CLIENT_LOG" ;;
*) sed -n '2,26p' "$0"; exit 1 ;;
esac
