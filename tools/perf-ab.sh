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
    # M5 C4：Rust 出口的公共端口 = QUIC 端口（= --listen + 1）
    nohup "$RUST_BIN" serve --state "$st/exit" --listen "$ep" --bind-interface none \
      --upnp=false --stun= --public-endpoint "127.0.0.1:$((ep + 1))" --relay "$RL1" \
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
# 【R6 前置批 ⑤ 整改】token 一律铸 --loopback-only 变体：同机部署的出口按网卡自报
# 端点（token 含 LAN IP），客户端赛跑随机采纳 LAN IP 时每包 UDP sendto 走 en0 环回
# 路径（实测 18.3µs/包 vs lo0 5.5µs/包 = 3.3 倍——R5 轮的 down 0.42× 主因即此 artifact，
# 不是实现栈差距）。A/B 公平口径 = 两侧同走 lo0（真机形态无此路径，跨网络物理直达）。
setup_stack_rust_client() { # <side> <ep>
  local side="$1" ep="$2"
  local st="$BASE/$side"
  local TOK
  TOK=$("$RUST_BIN" serve token --state "$st/exit" | grep -o 'hmw[0-9][A-Za-z0-9+/=_-]*' | head -1)
  TOK=$("$RUST_BIN" token "$TOK" --loopback-only)
  nohup "$RUST_BIN" connect --token "$TOK" --identity-dir "$st/client/identity" --no-session-lock --hold 3600 >> "$st/client.log" 2>&1 &
  echo $! > "$st/client.pid"
  echo "$TOK" > "$st/token"
}

# 收口保障（dsh 评审整改）：任何退出路径清 RSS 轮询进程——此前 v1 中止轮的 poller
# 存活混入下一轮（rss.tsv 行率翻倍污染口径）
PERF_AB_CLEANED=0
perf_ab_cleanup() {
  if (( PERF_AB_CLEANED )); then return; fi
  PERF_AB_CLEANED=1
  [[ -n "${RSS_POLLER:-}" ]] && kill $RSS_POLLER 2>/dev/null
}
trap perf_ab_cleanup EXIT INT TERM

echo "==> 起 GGG（42660/42750/42800）与 RRR（42667/42757/42807）"
setup_stack ggg 42660 42750 42800
setup_stack rrr 42667 42757 42807
sleep 3
setup_stack_rust_client rrr 42667
sleep 2

# Go 客户端 host add（token 同铸 loopback-only——两侧同走 lo0 的公平对照，见上）
GGG_TOK=$("$GO_BIN" serve token --state "$BASE/ggg/exit" | grep -o 'hmw[0-9][A-Za-z0-9+/=_-]*' | head -1)
GGG_TOK=$("$RUST_BIN" token "$GGG_TOK" --loopback-only)
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
# 中-7 整改（R6 前置批 ④）：每轮跑完断言直连路径（非直连轮作废重跑，至多一次重试；
# 两次仍中继即硬失败——200pps 中继限速混进 A/B 会让对照失真）。Go 侧判 daemon 会话
# 日志的最新路径行；Rust 侧判 speedtest 会话自己的「路径确立」行。
# 中-6 整改（R6 前置批 ④）：Rust 轮的 speedtest 是独立 CLI 进程——跑动期 1Hz 采其
# RSS 以 rrr-client-cli 行入 rss.tsv（峰值口径，非常驻——报告列名区分）。
assert_direct_gg() { # Go：speedtest 轮输出的「via=…（开跑时冻结）」行 = 该轮路径的权威口径
  # （实测首拍赛跑可落中继、随后自愈回直连——daemon 日志的残留路径行会误判，弃用）
  grep -m1 -E 'via=(direct|relay)' "$1" 2>/dev/null | grep -q 'via=direct'
}
assert_direct_rr() { # Rust：本轮输出文件里的会话路径行
  grep -E '路径确立：|link: via=' "$1" 2>/dev/null | grep -m1 -qE '直连|via=direct'
}
echo "==> 交替 3+3 轮（G,R,G,R,G,R；每轮 = 同会话一次 run，自带 warmup；直连断言）"
for i in 1 2 3; do
  # Go 轮（≤2 次尝试）
  G_OUT="" G_TRY=0
  while (( G_TRY < 2 )); do
    G_TRY=$(( G_TRY + 1 ))
    "$GO_BIN" speedtest --state "$BASE/ggg/client" -host perf > "$BASE/ggg/round-$i.log" 2>&1
    if assert_direct_gg "$BASE/ggg/round-$i.log"; then
      G_OUT=$(grep -E '精确值' "$BASE/ggg/round-$i.log" | tail -1)
      break
    fi
    echo "  G$i 第${G_TRY}次落中继——作废重跑" >&2
  done
  if [[ -z "$G_OUT" ]]; then
    echo "!! G$i 两轮均非直连——A/B 失真，中止（先排查 hint/端点）" >&2; exit 1
  fi
  echo "G$i|$G_OUT" >> "$BASE/rounds.tsv"
  echo "  G$i: $G_OUT"
  # Rust 轮（≤2 次尝试；CLI 进程 RSS 并入采样）
  RRR_TOK=$(cat "$BASE/rrr/token")
  R_OUT="" R_TRY=0
  while (( R_TRY < 2 )); do
    R_TRY=$(( R_TRY + 1 ))
    (cd "$BASE/rrr" && "$RUST_BIN" speedtest --token "$RRR_TOK" --identity-dir "$BASE/rrr/client/identity" --no-session-lock --rounds 1 > "$BASE/rrr/round-$i.log" 2>&1) &
    local_cli=$!
    (while kill -0 $local_cli 2>/dev/null; do
      rss=$(ps -o rss= -p $local_cli 2>/dev/null | tr -d ' ')
      [[ -n "$rss" ]] && printf '%s\t%s\t%s\t%s\n' "$(date +%s)" "rrr-client-cli" "$rss" "run" >> "$BASE/rss.tsv"
      sleep 1
    done) &
    local_poll=$!
    wait $local_cli
    kill $local_poll 2>/dev/null
    if assert_direct_rr "$BASE/rrr/round-$i.log"; then
      R_OUT=$(grep -E 'round 1' "$BASE/rrr/round-$i.log" | tail -1)
      break
    fi
    echo "  R$i 第${R_TRY}次落中继——作废重跑" >&2
  done
  if [[ -z "$R_OUT" ]]; then
    echo "!! R$i 两轮均非直连——A/B 失真，中止" >&2; exit 1
  fi
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
# Rust 侧 portfwd（独立会话——RTT 面单连接，与会话竞速无关；直连断言同中-7：
# 中继腿 RTT 会混入 200pps 排队，A/B 失真）
(cd "$BASE/rrr" && nohup "$RUST_BIN" portfwd --token "$RRR_TOK" --identity-dir "$BASE/rrr/client/identity" --no-session-lock --map 42901:$(lan_ip):42807 > "$BASE/rrr/portfwd.log" 2>&1 &)
sleep 4
if ! grep -E '路径确立：|link: via=' "$BASE/rrr/portfwd.log" 2>/dev/null | grep -m1 -qE '直连|via=direct'; then
  echo "!! RTT 腿 portfwd 会话非直连——中止（RTT 对照失真）" >&2; exit 1
fi
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
