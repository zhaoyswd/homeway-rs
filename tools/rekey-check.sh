#!/bin/zsh
# rekey-check.sh — 单会话跨 WG rekey 窗口的长上传判据（R6 前置批 ① P0 的正面判据）
#
# 背景（2026-10-03 根因收口）：R5 轮 4/复跑 1 的 L3-F-100MB「mid-transfer rekey stall」
# 根因 = **测试形态的双会话互踢**，不是 boringtun/wireguard-go 的 rekey 缺陷：
#   矩阵主客户端（connect --hold 常驻）与独立 files CLI 进程用**同一 identity**
#   （同 pubkey/devTag）并发——wireguard-go 出口单 peer 只有一条 keypair 链
#   （current/previous/next），后到握手经 ReceivedWithKeypair 顶掉 current ⇒
#   被踢方「出口→客户端」方向黑洞（客户端解不开新 keypair 的包），其 boringtun
#   15s（KEEPALIVE+REKEY_TIMEOUT）自愈 rekey 又反向踢死前者 ⇒「写通道长时间无
#   进展」。Go 客户端无此形态（daemon 单会话），产品形态（tier 手机核单进程
#   常驻会话）同样无此问题。
# 本脚本钉死**产品形态**的健康度：单会话 400MB @2MiB/s ≈205s，必然横跨
# REKEY_AFTER_TIME=120s 的 rekey 窗口（实验 C 实测：出口在会话 120.0s 整收到
# rekey initiation，上传全程无断流、sha256 对账偏差 0）。
#
# 用法：tools/rekey-check.sh [实例号] [大小MB] [限速B/s]
#   实例号缺省 9（local-exit.sh 端口 42649，避开矩阵 4266x/常用 1-7）；
#   大小缺省 400（400MB@2MiB/s≈205s 跨 120s 一次 rekey）；限速缺省 2000000。
# 判据（全过 = exit 0）：
#   ① 上传完成（无「写通道长时间无进展」看门狗中止）；
#   ② 出口日志在本轮会话内 ≥2 次「Received handshake initiation」（1 次初始 +
#     ≥1 次 rekey——证明确实跨了 rekey 窗口，防「跑太短没跨窗」的假绿）；
#   ③ 下载 sha256 对账一致（偏差 0）。
# 隔离：local-exit.sh 私有实例（/tmp state + 错开端口 + UPnP/STUN 关），绝不碰现役出口。
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
RUST_BIN="$REPO_ROOT/target/release/homeway-cli"
N="${1:-9}"
SIZE_MB="${2:-400}"
RATE="${3:-2000000}"

tmo() { # tmo <secs> cmd…（同 matrix.sh 形态）
  local secs=$1; shift
  "$@" &
  local p=$!
  ( sleep "$secs"; kill -KILL $p 2>/dev/null ) &
  local wd=$!
  wait $p 2>/dev/null; local rc=$?
  kill -KILL $wd 2>/dev/null
  return $rc
}

[[ -x "$RUST_BIN" ]] || { echo "!! 先 cargo build --release -p homeway-cli" >&2; exit 2; }

WORK="/tmp/homeway-rs-rekey-check"
rm -rf "$WORK"; mkdir -p "$WORK/identity"

# 出口（未跑则起；已在跑则复用——判据②以行号起点区分本轮）
if ! "$REPO_ROOT/tools/local-exit.sh" status "$N" >/dev/null 2>&1; then
  "$REPO_ROOT/tools/local-exit.sh" wipe "$N" >/dev/null 2>&1
  "$REPO_ROOT/tools/local-exit.sh" start "$N" >/dev/null 2>&1 || { echo "!! 出口起不来" >&2; exit 2; }
fi
TOK=$("$REPO_ROOT/tools/local-exit.sh" token "$N" | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
[[ -n "$TOK" ]] || { echo "!! 取不到 token" >&2; exit 2; }

EXIT_LOG="/tmp/homeway-rs-exit-$N/stdout.log"
LOG0=$(wc -l < "$EXIT_LOG" | tr -d ' ')

echo "==> 造 ${SIZE_MB}MB 测试文件并上传（@${RATE}B/s，跨 120s rekey 窗口）"
dd if=/dev/urandom of="$WORK/up.bin" bs=1048576 count="$SIZE_MB" 2>/dev/null
UP_SHA=$(shasum -a 256 "$WORK/up.bin" | cut -d' ' -f1)
T0=$SECONDS
# 上传预算 = 大小/限速 × 3 + 60s 握手/预热余量
BUDGET=$(( SIZE_MB * 1048576 / RATE * 3 + 60 ))
UP_ALL=$(tmo $BUDGET "$RUST_BIN" files upload --token "$TOK" --identity-dir "$WORK/identity" \
  --rate-limit "$RATE" /rekey-check-$N.bin "$WORK/up.bin" 2>&1)
UP_RC=$?
UP=$(echo "$UP_ALL" | tail -1)
DT=$(( SECONDS - T0 ))
echo "$UP"

# 判据①：上传完成（无看门狗中止）
if [[ "$UP" == *"无进展"* || "$UP_RC" -ne 0 ]]; then
  echo "rekey-check: FAIL 判据① 上传中断（${DT}s）：$UP"
  exit 1
fi
# 判据②：本轮会话内 ≥2 次握手 initiation（初始 + rekey）
HS=$(tail -n +"$((LOG0 + 1))" "$EXIT_LOG" | grep -c 'Received handshake initiation')
if (( HS < 2 )); then
  echo "rekey-check: FAIL 判据② 本轮仅 ${HS} 次 initiation（未跨 rekey 窗口——检查限速/大小参数）"
  exit 1
fi
# 判据③：下载对账
DN=$(tmo $BUDGET "$RUST_BIN" files download --token "$TOK" --identity-dir "$WORK/identity" \
  --rate-limit 4000000 /rekey-check-$N.bin "$WORK/dn.bin" 2>&1 | tail -1)
DN_SHA=$(shasum -a 256 "$WORK/dn.bin" 2>/dev/null | cut -d' ' -f1)
if [[ "$UP_SHA" != "$DN_SHA" || -z "$DN_SHA" ]]; then
  echo "rekey-check: FAIL 判据③ 对账不符 up=${UP_SHA:0:12} dn=${DN_SHA:0:12}"
  exit 1
fi

# 清理远端测试文件（files 根 = 出口 $HOME）
tmo 60 "$RUST_BIN" files rm --token "$TOK" --identity-dir "$WORK/identity" /rekey-check-$N.bin >/dev/null 2>&1
rm -rf "$WORK"
echo "rekey-check: PASS ${SIZE_MB}MB/${DT}s 跨 rekey（initiation ×${HS}）全程无断流，对账偏差 0（sha256 ${UP_SHA:0:16}…）"
