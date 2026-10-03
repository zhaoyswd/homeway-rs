#!/bin/zsh
# perf-ab.sh — 性能 A/B 数据收集（R5-5c；设计 §3/§4，评审 ③-1..③-7 整改口径）
#
# 公平性口径（预登记——PERF-AB.md 方法论节同文）：
#  - 双栈常驻、按轮交替（GGG 与 RRR 同起，G,R,G,R,G,R 交替发轮——消时段漂移；
#    空闲对端巡检在跑，声明计入噪声带）；
#  - 轮 = 同一会话内一次 speedtest::run（Rust=独立 speedtest 动词 --rounds；Go=daemon
#    会话内 speedtest CLI）；每轮自带 2s warmup（两侧参数同构 10s/10s/4 流）；
#  - 尾延迟 = echo RTT（经隧道单连接 200 次回显往返，python 测量器两链路同一脚本）；
#  - RSS = 1Hz 轮询测量期取 max + 就绪稳态两口径；
#  - 体积两口径（原始 / strip）。
# 端口：GGG=42660(exit)/42750(relay)/42800(echo)；RRR=42667/42757/42807（--perf 留口）。
# state：/tmp/homeway-rs-matrix-perf/（与矩阵目录分开；跑前自 wipe）。
# 产物：/tmp/homeway-rs-matrix-perf/{ggg,rrr}.log + rtt-{ggg,rrr}.json + rss.tsv
#      + stdout 汇总行（PERF-AB.md 从此汇总）。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
RUST_BIN="$REPO_ROOT/target/release/homeway-cli"
GO_BIN="${HOMEWAY_GO:-$REPO_ROOT/bin/homeway-go}"
BASE="/tmp/homeway-rs-matrix-perf"

# 本机内网 IP（第二道门 低-6：原硬编码 192.168.3.12——换机/换网卡即错；与
# tools/matrix.sh lan_ip 同探测形态）
lan_ip() {
  local out
  out=$(ipconfig getifaddr en0 2>/dev/null) || out=$(ipconfig getifaddr en1 2>/dev/null) || out="127.0.0.1"
  echo "$out"
}

[[ -x "$RUST_BIN" ]] || { (cd "$REPO_ROOT" && cargo build --release -p homeway-cli) || exit 1; }
# 互斥锁（第二道门 中-5 整改：裸 mkdir 抢锁——`mkdir -p` 对已存在目录恒成功拦不住
# 并发；且锁须在 $BASE **之外**——下面的清场 rm -rf $BASE 会把建在里面的锁一起删掉）
if ! mkdir /tmp/homeway-rs-matrix-perf.lock 2>/dev/null; then
  echo "!! perf-ab 已在跑（/tmp/homeway-rs-matrix-perf.lock 占用）" >&2
  exit 1
fi
trap 'rmdir /tmp/homeway-rs-matrix-perf.lock 2>/dev/null' EXIT

# 清场（复用矩阵的纪律）
pkill -f "homeway-rs-matrix-perf" 2>/dev/null
rm -rf "$BASE"; mkdir -p "$BASE"

start_echo() { # $1=port
  nohup python3 -c "
import socket, threading
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('0.0.0.0', $1)); s.listen(8)
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
    threading.Thread(target=serve, args=(c,), daemon=True).start()
" >> "$BASE/echo-$1.log" 2>&1 &
  echo $! > "$BASE/echo-$1.pid"
  sleep 0.5
}

# ---------- 起两套栈 ----------
setup_stack() { # setup_stack <ggg|rrr> <exitport> <relayport> <echoport>
  local side="$1" ep="$2" rp="$3" xport="$4"
  local st="$BASE/$side"
  mkdir -p "$st/relay/cache" "$st/exit" "$st/client/identity" "$st/files"
  # relay
  if [[ "$side" == ggg ]]; then
    nohup "$GO_BIN" relay --state "$st/relay" --listen "127.0.0.1:$rp" --advertise "127.0.0.1:$rp" >> "$st/relay.log" 2>&1 &
  else
    nohup "$RUST_BIN" relay --state "$st/relay" --listen "127.0.0.1:$rp" --advertise "127.0.0.1:$rp" --no-hints >> "$st/relay.log" 2>&1 &
  fi
  echo $! > "$st/relay.pid"
  sleep 1.5
  local RL1
  RL1=$(grep -o 'rl1[A-Za-z0-9+/=_-]*' "$st/relay.log" "$st/relay/cache/relay.log" 2>/dev/null | head -1 | grep -o 'rl1[A-Za-z0-9+/=_-]*')
  # exit
  if [[ "$side" == ggg ]]; then
    cat > "$st/exit/config.toml" <<EOF
[serve]
files_root = "$st/files"
EOF
    nohup "$GO_BIN" serve --state "$st/exit" --listen "$ep" --bind-interface none \
      --upnp=false --stun= --stun6= --public-endpoint "127.0.0.1:$ep" --relay "$RL1" --verbose \
      >> "$st/exit.log" 2>&1 &
  else
    nohup "$RUST_BIN" serve --state "$st/exit" --listen "$ep" --bind-interface none \
      --upnp=false --stun= --public-endpoint "127.0.0.1:$ep" --relay "$RL1" \
      --files-root "$st/files" --verbose >> "$st/exit.log" 2>&1 &
  fi
  echo $! > "$st/exit.pid"
  # client
  if [[ "$side" == ggg ]]; then
    cat > "$st/client/config.toml" <<EOF
[serve]
enabled = false
[relay]
enabled = false
EOF
    nohup "$GO_BIN" --state "$st/client" --verbose >> "$st/client.log" 2>&1 &
    echo $! > "$st/client.pid"
  fi
  start_echo "$xport"
}

# Rust 客户端的 connect 需要 token——先占位起会被拒；改为拿 token 后再起。
setup_stack_rust_client() { # <side> <ep>
  local side="$1" ep="$2"
  local st="$BASE/$side"
  local TOK
  TOK=$("$RUST_BIN" serve token --state "$st/exit" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
  nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/client/identity" --hold 3600 >> "$st/client.log" 2>&1 &
  echo $! > "$st/client.pid"
  echo "$TOK" > "$st/token"
}

echo "==> 起 GGG（42660/42750/42800）与 RRR（42667/42757/42807）"
setup_stack ggg 42660 42750 42800
setup_stack rrr 42667 42757 42807
sleep 3
setup_stack_rust_client rrr 42667
sleep 2

# Go 客户端 host add
GGG_TOK=$("$GO_BIN" serve token --state "$BASE/ggg/exit" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
"$GO_BIN" host add --state "$BASE/ggg/client" --name perf "$GGG_TOK" || { echo "!! GGG host add 失败" >&2; }
sleep 3

# RSS 轮询（1Hz，测量期全程——后台采集）
(while true; do
  for side in ggg rrr; do
    for role in exit relay client; do
      p=$(cat "$BASE/$side/$role.pid" 2>/dev/null) || continue
      rss=$(ps -o rss= -p "$p" 2>/dev/null | tr -d ' ')
      [[ -n "$rss" ]] && printf '%s\t%s\t%s\t%s\n' "$(date +%s)" "$side-$role" "$rss" "run" >> "$BASE/rss.tsv"
    done
  done
  sleep 1
done) &
RSS_POLLER=$!

# ---------- 交替轮 ----------
echo "==> 交替 3+3 轮（G,R,G,R,G,R；每轮 = 同会话一次 run，自带 warmup）"
for i in 1 2 3; do
  # Go 轮
  G_OUT=$("$GO_BIN" speedtest --state "$BASE/ggg/client" -host perf 2>&1 | grep -E '精确值' | tail -1)
  echo "G$i|$G_OUT" >> "$BASE/rounds.tsv"
  echo "  G$i: $G_OUT"
  # Rust 轮
  RRR_TOK=$(cat "$BASE/rrr/token")
  R_OUT=$(cd "$BASE/rrr" && "$RUST_BIN" speedtest --token "$RRR_TOK" --identity-dir "$BASE/rrr/client/identity" --rounds 1 2>&1 | grep -E 'round 1' | tail -1)
  echo "R$i|$R_OUT" >> "$BASE/rounds.tsv"
  echo "  R$i: $R_OUT"
done

# ---------- echo RTT ----------
echo "==> echo RTT（经隧道 200 次回显往返）"
# Go 侧 forward
"$GO_BIN" forward add --state "$BASE/ggg/client" -host perf --listen 42900 --target "$(lan_ip):42800" >/dev/null 2>&1
sleep 1
python3 "$REPO_ROOT/tools/echo-rtt.py" 127.0.0.1 42900 200 > "$BASE/rtt-ggg.json"
echo "  GGG: $(cat "$BASE/rtt-ggg.json")"
# Rust 侧 portfwd（独立会话——RTT 面单连接，与会话竞速无关）
(cd "$BASE/rrr" && nohup "$RUST_BIN" portfwd --token "$RRR_TOK" --identity-dir "$BASE/rrr/client/identity" --map 42901:$(lan_ip):42807 > "$BASE/rrr/portfwd.log" 2>&1 &)
sleep 4
python3 "$REPO_ROOT/tools/echo-rtt.py" 127.0.0.1 42901 200 > "$BASE/rtt-rrr.json"
echo "  RRR: $(cat "$BASE/rtt-rrr.json")"

# ---------- 稳态 RSS（轮询停止后采 5 拍均值 = 尾态） ----------
kill $RSS_POLLER 2>/dev/null
sleep 2
for side in ggg rrr; do
  for role in exit relay client; do
    p=$(cat "$BASE/$side/$role.pid" 2>/dev/null) || continue
    rss=$(ps -o rss= -p "$p" 2>/dev/null | tr -d ' ')
    [[ -n "$rss" ]] && printf '%s\t%s\t%s\t%s\n' "$(date +%s)" "$side-$role" "$rss" "steady" >> "$BASE/rss.tsv"
  done
done

# ---------- 体积 ----------
GO_SIZE=$(stat -f %z "$GO_BIN")
RS_SIZE=$(stat -f %z "$RUST_BIN")
cp "$RUST_BIN" /tmp/homeway-rs-cli-strip && strip /tmp/homeway-rs-cli-strip
RS_STRIP=$(stat -f %z /tmp/homeway-rs-cli-strip)
cp "$GO_BIN" /tmp/homeway-go-strip && strip /tmp/homeway-go-strip
GO_STRIP=$(stat -f %z /tmp/homeway-go-strip)
rm -f /tmp/homeway-rs-cli-strip /tmp/homeway-go-strip

# ---------- 收工 ----------
echo "==> 收工（保留 state 供复核：$BASE）"
kill $(cat "$BASE/ggg/client.pid" "$BASE/ggg/exit.pid" "$BASE/ggg/relay.pid" \
      "$BASE/rrr/client.pid" "$BASE/rrr/exit.pid" "$BASE/rrr/relay.pid" \
      "$BASE/echo-42800.pid" "$BASE/echo-42807.pid" 2>/dev/null)
pkill -f "matrix-perf" 2>/dev/null

# ---------- 汇总 ----------
echo
echo "===== PERF-AB 原始数据 ====="
echo "-- 轮次（rounds.tsv）--"; cat "$BASE/rounds.tsv"
echo "-- RTT --"; echo "GGG: $(cat "$BASE/rtt-ggg.json")"; echo "RRR: $(cat "$BASE/rtt-rrr.json")"
echo "-- RSS（KB；run=max 采集、steady=尾态）--"
python3 - <<'PY'
import collections
mx = collections.defaultdict(int)
sd = {}
for line in open('/tmp/homeway-rs-matrix-perf/rss.tsv'):
    parts = line.rstrip('\n').split('\t')
    if len(parts) != 4: continue
    _, who, rss, phase = parts
    rss = int(rss)
    mx[who] = max(mx[who], rss)
    if phase == 'steady': sd[who] = rss
for who in sorted(mx):
    print(f"  {who}: max={mx[who]}KB steady={sd.get(who, '?')}KB")
PY
echo "-- 体积 --"
echo "  homeway-go: $GO_SIZE B（strip: $GO_STRIP B）"
echo "  homeway-cli: $RS_SIZE B（strip: $RS_STRIP B）"
echo "-- 环境 --"
echo "  loadavg: $(sysctl -n vm.loadavg 2>/dev/null || uptime)"
echo "  GOGC/GOMAXPROCS: 未设置（默认）"
