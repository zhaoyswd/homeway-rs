#!/bin/zsh
# qi-ab.sh — Q-I 尾段（性能细节批·尾段）A/B 测量 harness（设计 §2 F7 / §4）。
#
# 用法：
#   tools/qi-ab.sh speedtest <binA> <binB> [--rounds 3] [--streams 4] [--en0] [--only A|B]
#   tools/qi-ab.sh files     <binA> <binB> [--rounds 2] [--size 256M] [--only A|B]
#   tools/qi-ab.sh rss       <binA> <binB> [--flows 16] [--secs 30] [--only A|B]
#
# 纪律（设计 §4.1；只起本地私有实例，绝不碰两台生产出口 / tier / homeway / baseline）：
#  - 出口 state=/tmp/qit-exit9、WG 端口 42659；客户端统一进程 state=/tmp/qit-client。
#  - 出口启动形态 = `serve --state … --listen 42659 --bind-interface none --upnp=false
#    --public-endpoint 127.0.0.1:42659 --verbose`——**不带任何 stun flag**：该形态对
#    F0 修复前后的二进制都能起（public_endpoint 非空 ⇒ 公网端点推断短路，QIt-design §2 F0）。
#    客户端恒定用 binA 起（单变量 = 出口臂；客户端二进制不随臂切换）。
#  - 臂切换 = 原子替换 target/release/homeway-cli（cp+mv；trap 恢复原物 + 清进程）。
#  - 轮序平衡：--rounds 3 ⇒ A,B,B,A,A,B；--rounds 2 ⇒ A,B,B,A（消时段漂移）。
#  - loadavg 1Hz 带时间戳落盘 + 轮首/轮末标记（loadavg.tsv）；1min >4 的轮在汇总标 [!]
#    （判决纪律：只在 1min ≤4 作数）。
#  - 逐轮产物：speedtest-<arm>-r<N>.json / sample-<arm>-r<N>.txt（末轮）/ cpu-<arm>.tsv /
#    rss-<arm>.tsv（1Hz）/ endpoint-<arm>-r<N>.txt（lsof+host list 快照）/ exit-<arm>.log
#    （出口 stdout 全量归档，reactor 剂量行可复核）/ bins.sha256 / summary.txt。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
BIN_PATH="$REPO_ROOT/target/release/homeway-cli"
EXIT_STATE="/tmp/qit-exit9"
CLIENT_STATE="/tmp/qit-client"
EXIT_PORT=42659
SOCKS_PORT=42779
FIREHOSE_PORT=42769
FILES_ROOT="/tmp/qit-files-root9"

usage() { sed -n '2,28p' "$0"; exit 1; }

mode="${1:-}"; shift || usage
[[ -n "$mode" ]] || usage
binA="${1:-}"; binB="${2:-}"; shift 2 || usage
[[ -f "$binA" && -f "$binB" ]] || { echo "!! 需要两个存在的二进制：binA binB" >&2; usage; }

rounds=3; streams=4; en0=0; only=""; size="256M"; flows=16; secs=30
while (( $# )); do
  case "$1" in
    --rounds) rounds="$2"; shift 2 ;;
    --streams) streams="$2"; shift 2 ;;
    --en0) en0=1; shift ;;
    --only) only="$2"; shift 2 ;;
    --size) size="$2"; shift 2 ;;
    --flows) flows="$2"; shift 2 ;;
    --secs) secs="$2"; shift 2 ;;
    *) echo "!! 未知参数：$1" >&2; usage ;;
  esac
done

DIR="${QI_AB_DIR:-/tmp/qi-ab/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$DIR" || exit 1

echo "==> 产物目录：$DIR（mode=$mode rounds=$rounds streams=$streams en0=$en0 only=${only:-both}）"
shasum -a 256 "$binA" "$binB" > "$DIR/bins.sha256"
cat "$DIR/bins.sha256"

# ---------- 通用工具 ----------
up() { sysctl -n vm.loadavg 2>/dev/null | awk '{print $2, $3, $4}'; }
mark() { # mark <event>；loadavg 表头 + 时刻
  printf '%s\t%s\t%s\n' "$(date +%H:%M:%S)" "$1" "$(up | tr ' ' '\t')" >> "$DIR/loadavg.tsv"
}
LOADAVG_PID=""
start_loadavg_poll() { # 1Hz 全轮落盘（设计 §4.1「轮首 / 全轮（带时间戳）/ 轮末」）
  ( while true; do
      printf '%s\ttick\t%s\n' "$(date +%H:%M:%S)" "$(up | tr ' ' '\t')" >> "$DIR/loadavg.tsv"
      sleep 1
    done ) &
  LOADAVG_PID=$!
}
stop_loadavg_poll() { [[ -n "${LOADAVG_PID:-}" ]] && kill "$LOADAVG_PID" 2>/dev/null; LOADAVG_PID=""; }
time_to_secs() { # "0:41.62" / "1:02:03" => 秒
  echo "$1" | tr -d ' ' | awk -F: '{ if (NF==3) printf "%.2f", $1*3600+$2*60+$3; else printf "%.2f", $1*60+$2 }'
}

ORIG_BIN_BAK="$DIR/homeway-cli.orig"
if [[ -f "$BIN_PATH" ]]; then cp "$BIN_PATH" "$ORIG_BIN_BAK"; fi
EXIT_PID=""; CLIENT_PID=""; FIREHOSE_PID=""; RSS_PID=""; SAMPLE_PID=""
CLEANED=0
cleanup() {
  (( CLEANED )) && return
  CLEANED=1
  [[ -n "${EXIT_PID:-}" ]] && kill "$EXIT_PID" 2>/dev/null
  [[ -n "${CLIENT_PID:-}" ]] && kill "$CLIENT_PID" 2>/dev/null
  [[ -n "${FIREHOSE_PID:-}" ]] && kill "$FIREHOSE_PID" 2>/dev/null
  [[ -n "${RSS_PID:-}" ]] && kill "$RSS_PID" 2>/dev/null
  [[ -n "${SAMPLE_PID:-}" ]] && kill "$SAMPLE_PID" 2>/dev/null
  stop_loadavg_poll
  if [[ -f "$ORIG_BIN_BAK" ]]; then cp "$ORIG_BIN_BAK" "$BIN_PATH.tmp.$$" && mv -f "$BIN_PATH.tmp.$$" "$BIN_PATH"; fi
  echo "==> 收工（二进制已恢复；残留核对：）"
  pgrep -fl "qit-exit9|qit-client" || echo "  （无 qit 残留进程）"
}
trap cleanup EXIT INT TERM

arm_bin() { # arm_bin <A|B>：原子替换
  local src="$binA"; [[ "$1" == "B" ]] && src="$binB"
  cp "$src" "$BIN_PATH.tmp.$$" && mv -f "$BIN_PATH.tmp.$$" "$BIN_PATH" || { echo "!! 臂切换失败" >&2; exit 1; }
}

start_exit() { # start_exit <arm> [--files]
  local arm="$1"; shift
  local ex="$DIR/exit-$arm.log"
  [[ -f "$ex" ]] || : > "$ex"
  if [[ "${1:-}" == "--files" ]]; then
    nohup "$BIN_PATH" serve --state "$EXIT_STATE" --listen "$EXIT_PORT" --bind-interface none \
      --upnp=false --public-endpoint "127.0.0.1:$((EXIT_PORT + 1))" --files-root "$FILES_ROOT" --verbose \
      >> "$ex" 2>&1 &
  else
    nohup "$BIN_PATH" serve --state "$EXIT_STATE" --listen "$EXIT_PORT" --bind-interface none \
      --upnp=false --public-endpoint "127.0.0.1:$((EXIT_PORT + 1))" --verbose \
      >> "$ex" 2>&1 &
  fi
  EXIT_PID=$!
  local i=0
  while (( i < 25 )); do
    kill -0 "$EXIT_PID" 2>/dev/null || break
    tail -60 "$ex" | grep -q 'serve 就绪' && break
    sleep 1; (( i++ ))
  done
  kill -0 "$EXIT_PID" 2>/dev/null || { echo "!! 出口（$arm）起不来——看 $ex" >&2; tail -12 "$ex" >&2; exit 1; }
  local actual
  actual=$(cat "$EXIT_STATE/cache/listen_port.txt" 2>/dev/null)
  # M5 C4：公共端口 = QUIC 端口（= --listen + 1；出口唯一公共口）
  [[ "$actual" == "$((EXIT_PORT + 1))" ]] || { echo "!! 实际公共端口 $actual ≠ $((EXIT_PORT + 1))（= listen+1；退让）——中止" >&2; exit 1; }
}
stop_exit() {
  [[ -n "${EXIT_PID:-}" ]] || return
  kill -TERM "$EXIT_PID" 2>/dev/null
  local i=0
  while (( i < 15 )); do kill -0 "$EXIT_PID" 2>/dev/null || break; sleep 1; (( i++ )); done
  kill -KILL "$EXIT_PID" 2>/dev/null
  EXIT_PID=""; sleep 0.5
}

setup_client() { # 客户端统一进程（serve/relay 双关；恒定 binA）
  pkill -f "homeway-cli --state $CLIENT_STATE" 2>/dev/null
  rm -rf "$CLIENT_STATE"; mkdir -p "$CLIENT_STATE"
  printf '[serve]\nenabled = false\n[relay]\nenabled = false\n' > "$CLIENT_STATE/config.toml"
  nohup "$binA" --state "$CLIENT_STATE" --verbose > "$DIR/client.log" 2>&1 &
  CLIENT_PID=$!
  sleep 1.5
  kill -0 "$CLIENT_PID" 2>/dev/null || { echo "!! 客户端起不来——看 $DIR/client.log" >&2; exit 1; }
}

mint_and_add() { # 出口 token →（--en0 原样 / 缺省 loopback-only）→ host add
  local raw tok
  raw=$("$binA" serve token --state "$EXIT_STATE" | grep -o 'hmw2[A-Za-z0-9+/=_-]*' | head -1)
  [[ -n "$raw" ]] || { echo "!! 取不到出口 token（出口先起一轮）" >&2; exit 1; }
  if (( en0 )); then tok="$raw"; else tok=$("$binA" token "$raw" --loopback-only); fi
  "$binA" host add --state "$CLIENT_STATE" --name qi --force "$tok" >/dev/null 2>&1 \
    || { echo "!! host add 失败" >&2; exit 1; }
  print -r -- "$tok" > "$DIR/token.txt"
}

start_rss_poll() { # start_rss_poll <arm> <round>
  local arm="$1" r="$2"
  ( while kill -0 "${EXIT_PID}" 2>/dev/null; do
      rss=$(ps -o rss= -p "$EXIT_PID" 2>/dev/null | tr -d ' ')
      [[ -n "$rss" ]] && printf '%s\t%s\tr%s\t%s\n' "$(date +%s)" "$arm" "$r" "$rss" >> "$DIR/rss-$arm.tsv"
      sleep 1
    done ) &
  RSS_PID=$!
}
stop_rss_poll() { [[ -n "${RSS_PID:-}" ]] && kill "$RSS_PID" 2>/dev/null; RSS_PID=""; }

endpoint_capture() { # endpoint_capture <arm> <round>
  local arm="$1" r="$2" f="$DIR/endpoint-$arm-r$r.txt"
  {
    echo "== $(date +%H:%M:%S) 臂=$arm 轮=$r =="
    echo "-- lsof -nP -a -p $CLIENT_PID -i UDP（客户端 daemon 的 socket；注：macOS lsof 对已 connect 的 UDP 常不显对端，端点权威口径见下行 host list 的 ep）--"
    lsof -nP -a -p "$CLIENT_PID" -i UDP 2>/dev/null | grep -v COMMAND
    echo "-- host list --json（含采纳端点 ep）--"
    HL=$("$binA" host list --state "$CLIENT_STATE" --json 2>/dev/null)
    print -r -- "$HL" | head -c 2000
    echo ""
    echo "-- 本轮采纳端点（ep；--en0 形态须为内网地址，回环 ⇒ 该轮建议作废）--"
    print -r -- "$HL" | tr ',' '\n' | grep -o '"[a-z]*[Ee]p[^,{]*' | head -3
    echo ""
  } > "$f" 2>&1
}

balanced_order() { # 平衡轮序（设计 §4.1）：[A,B,B,A] 循环取前 2n —— n=3 ⇒ A,B,B,A,A,B
  local n="$1" i
  local pat=(A B B A)
  for (( i = 0; i < n * 2; i++ )); do
    echo "${pat[$((i % 4 + 1))]}"
  done
}

# ---------- 会话前置（两模式共用）----------
wipe_session() {
  pkill -f "homeway-cli --state $CLIENT_STATE" 2>/dev/null
  rm -rf "$EXIT_STATE" "$CLIENT_STATE"; mkdir -p "$EXIT_STATE"
}

case "$mode" in
speedtest)
  wipe_session; setup_client
  arm_bin A; start_exit A     # 首轮只为铸 token
  mint_and_add
  stop_exit
  order=(${(f)"$(balanced_order $rounds)"})
  echo "==> 轮序：${order[@]}（每轮重启出口；sample 只跑每臂末轮）"
  mark "session-start"
  start_loadavg_poll
  rA=0; rB=0; lastA="" ; lastB=""
  for arm in "${order[@]}"; do
    [[ -n "$only" && "$arm" != "$only" ]] && continue
    if [[ "$arm" == "A" ]]; then rA=$((rA+1)); r=$rA; else rB=$((rB+1)); r=$rB; fi
    lastA="$rA"; lastB="$rB"
    arm_bin "$arm"
    start_exit "$arm"
    mark "round-start $arm-r$r"
    cpu0=$(ps -o time= -p "$EXIT_PID" | tr -d ' '); t0=$(date +%s)
    start_rss_poll "$arm" "$r"
    if [[ "$arm" == A && "$r" == "$rounds" || "$arm" == B && "$r" == "$rounds" ]]; then
      ( sleep 3; sample "$EXIT_PID" 20 -file "$DIR/sample-$arm-r$r.txt" >/dev/null 2>&1 ) &
      SAMPLE_PID=$!
    fi
    "$binA" speedtest --host qi --state "$CLIENT_STATE" --json \
      --down 15s --up 15s --streams "$streams" > "$DIR/speedtest-$arm-r$r.json" 2>&1
    sp_rc=$?
    t1=$(date +%s); cpu1=$(ps -o time= -p "$EXIT_PID" | tr -d ' ')
    [[ -n "$SAMPLE_PID" ]] && { wait "$SAMPLE_PID" 2>/dev/null; SAMPLE_PID=""; }
    stop_rss_poll
    endpoint_capture "$arm" "$r"
    # --en0 形态：采纳到回环 ⇒ 该轮按设计 §4.2 预登记标「作废」（自动补跑未实装——登记在批记录）
    if (( en0 )); then
      adopted=$("$binA" host list --state "$CLIENT_STATE" --json 2>/dev/null | tr ',' '\n' | grep -o '127\.0\.0\.1:[0-9]*' | head -1)
      [[ -n "$adopted" ]] && mark "round-void $arm-r$r（en0 采纳到回环 $adopted）"
    fi
    printf '%s\t%s\tr%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$(date +%H:%M:%S)" "$arm" "$r" "$(time_to_secs "$cpu0")" "$(time_to_secs "$cpu1")" \
      "$((t1-t0))" "$sp_rc" "$(up | tr ' ' '/')" >> "$DIR/cpu-$arm.tsv"
    mark "round-end $arm-r$r"
    echo "  轮 $arm-r$r：rc=$sp_rc $(head -c 220 "$DIR/speedtest-$arm-r$r.json" | tr '\n' ' ')"
    stop_exit
    sleep 2
  done
  mark "session-end"
  ;;

files)
  wipe_session; setup_client
  mkdir -p "$FILES_ROOT"
  SRC="$DIR/src-$size.bin"
  if [[ ! -f "$SRC" ]]; then
    echo "==> 生成 $size 随机源文件（$SRC）"
    dd if=/dev/urandom of="$SRC" bs=1m count="${size%M}" 2>/dev/null
  fi
  echo "==> 预热 page cache（读一遍源文件）"
  cat "$SRC" > /dev/null
  # IO/CPU 定性对照（设计 §4.2）：/tmp 内纯拷贝耗时
  ( /usr/bin/time -p cp "$SRC" "$DIR/io-control.bin" ) 2> "$DIR/io-control.time"
  echo "  io-control（cp 同尺寸）：$(grep real "$DIR/io-control.time" | tr -d '\n')"
  rm -f "$DIR/io-control.bin"
  arm_bin A; start_exit A --files
  mint_and_add
  stop_exit
  order=(${(f)"$(balanced_order $rounds)"})
  echo "==> 轮序：${order[@]}"
  mark "session-start"
  start_loadavg_poll
  rA=0; rB=0
  for arm in "${order[@]}"; do
    [[ -n "$only" && "$arm" != "$only" ]] && continue
    if [[ "$arm" == "A" ]]; then rA=$((rA+1)); r=$rA; else rB=$((rB+1)); r=$rB; fi
    arm_bin "$arm"; start_exit "$arm" --files
    local_remote="/qit-$arm-r$r.bin"; dst="$DIR/dst-$arm-r$r.bin"
    mark "round-start $arm-r$r"
    cpu0=$(ps -o time= -p "$EXIT_PID" | tr -d ' '); t0=$(date +%s)
    start_rss_poll "$arm" "$r"
    # 形态说明：daemon 托管远程形态（`--host`）在本机实测「流已终结（gone）」且与臂无关
    # （A 出口 + A 客户端同样失败）⇒ 本臂改用 **CLI 进程直连形态**（`--token`；设计
    # §4.2「files 臂唯一主侧 = CLI 进程侧」同义）。
    ( /usr/bin/time -p "$binA" files put --token "$(cat "$DIR/token.txt")" \
        --identity-dir "$DIR/fid" --no-session-lock --rate-limit 0 \
        "$SRC" "$local_remote" ) > "$DIR/files-put-$arm-r$r.log" 2>&1
    put_rc=$?
    ( /usr/bin/time -p "$binA" files get --token "$(cat "$DIR/token.txt")" \
        --identity-dir "$DIR/fid" --no-session-lock "$local_remote" -o "$dst" ) \
      > "$DIR/files-get-$arm-r$r.log" 2>&1
    get_rc=$?
    t1=$(date +%s); cpu1=$(ps -o time= -p "$EXIT_PID" | tr -d ' ')
    stop_rss_poll
    src_hash=$(shasum -a 256 "$SRC" | awk '{print $1}')
    dst_hash=$(shasum -a 256 "$dst" 2>/dev/null | awk '{print $1}')
    put_real=$(grep '^real' "$DIR/files-put-$arm-r$r.log" | awk '{print $2}')
    get_real=$(grep '^real' "$DIR/files-get-$arm-r$r.log" | awk '{print $2}')
    put_cpu=$(awk '/^user|^sys/{s+=$2} END{printf "%.2f", s}' "$DIR/files-put-$arm-r$r.log")
    get_cpu=$(awk '/^user|^sys/{s+=$2} END{printf "%.2f", s}' "$DIR/files-get-$arm-r$r.log")
    eq="NO"; [[ "$src_hash" == "$dst_hash" && -n "$dst_hash" ]] && eq="YES"
    printf '%s\t%s\tr%s\tput_rc=%s\tget_rc=%s\tput_real=%s\tget_real=%s\tput_cpu=%s\tget_cpu=%s\tsha_eq=%s\tcpu0=%s\tcpu1=%s\twall=%s\n' \
      "$(date +%H:%M:%S)" "$arm" "$r" "$put_rc" "$get_rc" "$put_real" "$get_real" "$put_cpu" "$get_cpu" "$eq" \
      "$(time_to_secs "$cpu0")" "$(time_to_secs "$cpu1")" "$((t1-t0))" >> "$DIR/files-$arm.tsv"
    mark "round-end $arm-r$r"
    echo "  轮 $arm-r$r：put=${put_real}s(get=${get_real}s) rc=$put_rc/$get_rc sha_eq=$eq"
    rm -f "$dst"
    stop_exit; sleep 2
  done
  mark "session-end"
  ;;

rss)
  wipe_session; setup_client
  arm_bin A; start_exit A
  mint_and_add
  stop_exit   # 铸 token 的临时出口必停（否则下面每轮 start 撞实例锁——已实测）
  # 出口侧 firehose（持续发数据）；客户端侧慢读（1KiB 停 50ms）经 SOCKS → 出口 tx_backlog 堆积
  nohup python3 -c "
import socket, threading
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', $FIREHOSE_PORT)); s.listen(64)
def serve(c):
    try:
        blob = b'x' * 65536
        while True:
            c.sendall(blob)
    except OSError:
        pass
    finally:
        try: c.close()
        except OSError: pass
while True:
    c, _ = s.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
" >> "$DIR/firehose.log" 2>&1 &
  FIREHOSE_PID=$!
  sleep 0.5
  "$binA" socks on --host qi --state "$CLIENT_STATE" --listen "$SOCKS_PORT" > "$DIR/socks-on.log" 2>&1 \
    || { echo "!! socks on 失败——看 $DIR/socks-on.log" >&2; }
  sleep 1
  reader_script="$DIR/slow-reader.py"
  cat > "$reader_script" <<PYEOF
import socket, struct, sys, time
n = int(sys.argv[1]); socks = ('127.0.0.1', $SOCKS_PORT); target = ('127.0.0.1', $FIREHOSE_PORT)
def one():
    s = socket.socket(); s.connect(socks)
    s.sendall(b'\x05\x01\x00'); s.recv(2)
    host = socket.inet_aton(target[0])
    s.sendall(b'\x05\x01\x00\x01' + host + struct.pack('!H', target[1])); s.recv(10)
    end = time.time() + $secs
    while time.time() < end:
        d = s.recv(1024)          # 慢读：1KiB / 50ms
        if not d: break
        time.sleep(0.05)
    s.close()
import threading
ts = [threading.Thread(target=one, daemon=True) for _ in range(n)]
[t.start() for t in ts]
[t.join() for t in ts]
PYEOF
  order=(${(f)"$(balanced_order $rounds)"})
  mark "session-start"
  rA=0; rB=0
  for arm in "${order[@]}"; do
    [[ -n "$only" && "$arm" != "$only" ]] && continue
    if [[ "$arm" == "A" ]]; then rA=$((rA+1)); r=$rA; else rB=$((rB+1)); r=$rB; fi
    arm_bin "$arm"; start_exit "$arm"
    mark "round-start $arm-r$r"
    sleep 1
    ( while kill -0 "$EXIT_PID" 2>/dev/null; do
        rss=$(ps -o rss= -p "$EXIT_PID" 2>/dev/null | tr -d ' ')
        [[ -n "$rss" ]] && printf '%s\t%s\tr%s\t%s\n' "$(date +%s)" "$arm" "$r" "$rss" >> "$DIR/rss-$arm.tsv"
        sleep 1
      done ) &
    RSS_PID=$!
    python3 "$reader_script" "$flows" > "$DIR/rss-$arm-r$r.log" 2>&1
    stop_rss_poll
    mark "round-end $arm-r$r"
    mx=$(awk -v a="$arm" -v r="r$r" -F'\t' '$2==a && $3==r {if ($4+0>m) m=$4+0} END{print m+0}' "$DIR/rss-$arm.tsv")
    echo "  轮 $arm-r$r：出口 RSS max = ${mx} KB（$flows 流 × $secs s 慢读）"
    stop_exit; sleep 2
  done
  mark "session-end"
  stop_loadavg_poll
  "$binA" socks off --host qi --state "$CLIENT_STATE" >/dev/null 2>&1
  ;;

*) usage ;;
esac

# ---------- 汇总 ----------
cat > "$DIR/sample-analyze.py" <<'PYEOF'
# sample 调用树叶帧计数（列位编码深度：count 起始列 / 2 = 深度）
import re, sys, glob, os
CL = re.compile(r'^([\s+!:|]*)(\d+) (.*)$')
def blocks_of(path):
    lines = open(path, errors='replace').read().splitlines()
    out = []; cur = None
    for ln in lines:
        m = re.match(r'^\s*(\d+)\s+(Thread_\S+)(.*)$', ln)
        if m:
            cur = [m.group(0).strip(), []]; out.append(cur); continue
        if cur is not None: cur[1].append(ln)
    return out
def analyze(path, thread_sub='homeway-serve-drv'):
    for name, body in blocks_of(path):
        if thread_sub not in name: continue
        total = int(name.split()[0]); stack = []; pairs = {}; symt = {}
        for ln in body:
            m = CL.match(ln)
            if not m: continue
            cnt = int(m.group(2)); sym = m.group(3).strip()
            if cnt == 0: continue
            depth = len(m.group(1)) // 2
            while stack and stack[-1][0] >= depth: stack.pop()
            parent = stack[-1][1] if stack else '(thread)'
            stack.append((depth, sym))
            pairs[(parent, sym)] = pairs.get((parent, sym), 0) + cnt
            symt[sym] = symt.get(sym, 0) + cnt
        ch = lambda pp, sp: sum(c for (p, s), c in pairs.items() if pp in p and sp in s)
        tot = lambda sp: sum(c for s, c in symt.items() if sp in s)
        return dict(total=total, poll_reac=ch('reactor_turn', 'poll  (in libsystem_kernel'),
                    poll_eng=ch('engine', 'poll  (in libsystem_kernel'),
                    reac=tot('reactor_turn'), bzero_dns=ch('DnsFaces', '__bzero'),
                    bzero_all=tot('__bzero'), ss=tot('service_sockets'), pump=tot('Interceptor4pump'))
    return None
files = sorted(glob.glob(os.path.join(sys.argv[1], 'sample-*.txt')))
if files:
    print("\n-- sample 叶帧（驱动线程 homeway-serve-drv）--")
    for f in files:
        a = analyze(f)
        if not a:
            print(f"  {os.path.basename(f)}: 未找到驱动线程"); continue
        t = a['total']
        print(f"  {os.path.basename(f)} 总样本={t}")
        for k, lab in [('poll_reac', 'reactor_turn→poll（每拍零超时 poll，F2 目标）'),
                       ('poll_eng', 'engine→poll（阻塞 poll，F2 后为唯一 poll）'),
                       ('bzero_dns', 'DnsFaces::service→__bzero（F1 目标）'),
                       ('bzero_all', '__bzero 全线程'),
                       ('reac', 'reactor_turn 子树'), ('ss', 'service_sockets 子树'),
                       ('pump', 'pump 子树')]:
            print(f"    {a[k]:6d}  {100*a[k]/t:5.2f}%  {lab}")
        print(f"    poll 两项合计（粗判据分母）= {a['poll_reac'] + a['poll_eng']}")
PYEOF
echo
echo "===== 汇总（产物：$DIR）====="
python3 - "$DIR" <<'PY'
import json, os, sys, glob, statistics
d = sys.argv[1]

def loadavg_rows():
    rows = []
    for line in open(os.path.join(d, 'loadavg.tsv')):
        p = line.rstrip('\n').split('\t')
        if len(p) >= 5:
            rows.append((p[0], p[1], float(p[2]), float(p[3]), float(p[4])))
    return rows

la = loadavg_rows()
print("-- loadavg（轮首/轮末；1min >4 标 [!]）--")
for t, ev, l1, l5, l15 in la:
    if ev.startswith(('round-start', 'session-')):
        flag = ' [!]' if l1 > 4 else ''
        print(f"  {t} {ev:<22} {l1:.2f}/{l5:.2f}/{l15:.2f}{flag}")
# 逐轮 1min 峰值（1Hz tick 取区间内 max；>6 = 该轮作废，设计 §4.1）
rounds = {}
cur = None
for t, ev, l1, l5, l15 in la:
    if ev.startswith('round-start'):
        cur = ev.split()[1]
        rounds[cur] = [l1]
    elif ev.startswith('round-end') and cur is not None:
        rounds[cur].append(l1)
        cur = None
    elif ev == 'tick' and cur is not None:
        rounds[cur].append(l1)
if rounds:
    print("-- 逐轮 1min loadavg 峰值（>6 ⇒ 该轮作废）--")
    for r, vals in rounds.items():
        mx = max(vals)
        print(f"  {r:<8} peak={mx:.2f}{'  [作废]' if mx > 6 else ''}")

def med(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None

# speedtest
st = {}
for f in sorted(glob.glob(os.path.join(d, 'speedtest-*.json'))):
    base = os.path.basename(f)[len('speedtest-'):-len('.json')]
    arm, r = base.split('-r')
    try:
        raw = open(f).read().strip().splitlines()
        j = json.loads([ln for ln in raw if ln.lstrip().startswith(('[', '{'))][-1])
    except Exception:
        print(f"  !! {base}: JSON 解析失败")
        continue
    h = j[0] if isinstance(j, list) and j else j
    if not isinstance(h, dict):
        continue
    st.setdefault(arm, []).append((int(r), h))
if st:
    print("\n-- speedtest（down/up MB/s；CPUδ=出口累计 ps time= 差；s/GB=CPUδ÷传输GB）--")
    hdr = f"{'arm':<4}{'r':<3}{'down':>9}{'up':>9}{'rtt':>6}{'via':>8}{'CPUδ(s)':>9}{'wall':>6}{'CPU%':>7}{'s/GB':>8}"
    print(hdr)
    per = {}
    for arm, rows in sorted(st.items()):
        for r, h in sorted(rows):
            down = h.get('downBps', 0) / 1e6
            up = h.get('upBps', 0) / 1e6
            wall = h.get('wallMs', 0) / 1000
            cpu_file = os.path.join(d, f'cpu-{arm}.tsv')
            cpu_delta = None
            if os.path.exists(cpu_file):
                for line in open(cpu_file):
                    p = line.rstrip('\n').split('\t')
                    if len(p) >= 5 and p[1] == arm and p[2] == f'r{r}':
                        cpu_delta = float(p[4]) - float(p[3])
            gb = ((h.get('usageDown', 0) + h.get('usageUp', 0)) / 1e9) if (h.get('usageDown') or h.get('usageUp')) else None
            sgb = (cpu_delta / gb) if (cpu_delta and gb) else None
            cpu_pct = (100 * cpu_delta / wall) if (cpu_delta and wall) else None
            per.setdefault(arm, []).append((down, up, cpu_delta, sgb, cpu_pct))
            print(f"{arm:<4}{r:<3}{down:9.2f}{up:9.2f}{h.get('rttMs', 0):6}{str(h.get('via',''))[:7]:>8}"
                  f"{(f'{cpu_delta:.2f}' if cpu_delta else '-'):>9}{wall:6.1f}"
                  f"{(f'{cpu_pct:.1f}' if cpu_pct else '-'):>7}{(f'{sgb:.2f}' if sgb else '-'):>8}")
    print("\n-- speedtest 中位 / B÷A --")
    for arm, rows in sorted(per.items()):
        print(f"  {arm}: down={med([x[0] for x in rows]):.2f} up={med([x[1] for x in rows]):.2f} "
              f"CPUδ={med([x[2] for x in rows]):.2f} s/GB={med([x[3] for x in rows]) and round(med([x[3] for x in rows]),2) or '-'}")
    if 'A' in per and 'B' in per:
        f = lambda i: (med([x[i] for x in per['B']]) / med([x[i] for x in per['A']]) - 1) * 100
        print(f"  B/A: down {f(0):+.1f}%  up {f(1):+.1f}%  CPUδ {f(2):+.1f}%  s/GB {f(3):+.1f}%")
        rssA = rssB = None
# files
if glob.glob(os.path.join(d, 'files-*.tsv')):
    print("\n-- files 臂（put/get 256MiB；CLI 进程 CPU=user+sys）--")
    for f in sorted(glob.glob(os.path.join(d, 'files-*.tsv'))):
        for line in open(f):
            p = line.rstrip('\n').split('\t')
            print("  " + "  ".join(p))
# RSS
rssmax = {}
for f in sorted(glob.glob(os.path.join(d, 'rss-*.tsv'))):
    arm = os.path.basename(f)[4:-4].split('-')[0]
    rows = {}
    for line in open(f):
        p = line.rstrip('\n').split('\t')
        if len(p) >= 4:
            rows[p[2]] = max(rows.get(p[2], 0), int(p[3]))
    for r, v in rows.items():
        rssmax.setdefault(arm, []).append(v)
        print(f"\n  RSS-{arm}-{r} max={v} KB")
if 'A' in rssmax and 'B' in rssmax:
    a = max(rssmax['A']); b = max(rssmax['B'])
    print(f"  RSS：A max={a}KB B max={b}KB ⇒ B/A = {b/a:.3f}（判据 ≤1.1）")
PY
echo "==> 完（$DIR）"
python3 "$DIR/sample-analyze.py" "$DIR"
