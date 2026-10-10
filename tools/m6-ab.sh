#!/bin/bash
# m6-ab.sh — M6 真机终验编排（设计 §7.1② 的入库 harness：`tools/m6-ab.sh`）。
#
# 定位：**harness-only**（不改产品面）。两臂的命令逐字对称，臂 = 唯一变量（OLD=4841b20 重构建 /
# NEW=HEAD 树）。臂命名用 OLD/NEW（不得用 *_wg 形态：check-wg-removed.sh 的 `_wg` 是子串匹配）。
# 旧臂断言串只在本脚本/设备日志里出现，绝不进 crates/**。
#
# 子命令：
#   loadavg <secs> <tsv>       1 Hz loadavg 采样（轮内作废口径：峰值 >6；轮首口径：>4）
#   cpu-time <pid>             设备侧累计 CPU（ps -o time= -p <pid>；getrusage 类）
#   net <pid>                  设备侧累计字节（hidumper --net <pid>：Received/Sent Bytes）
#   mem <pid>                  设备侧内存（hidumper --mem <pid>：VmRSS/VmHWM 等，登记面）
#   vpnpid                     打印 VPN 扩展进程 pid（ps -ef 全串匹配 me.zhaozhe.tier:vpn）
#   dev-reset <TOK> [n]        wakeup + force-stop + `aa start --ps host_token <TOK>`
#   dev-tap <x> <y>            uitest 模拟点击
#   dev-uidump <out.json>      uitest dumpLayout → recv 到本地
#   dev-open <url>             `aa start -U <url>`（浏览器；-U 面 S1 待验）
#   pull <tag>                 拉设备核日志 → /tmp/m6-dev/tailcat-tun-<tag>.log；hilog 切片
#   round <arm> <cond> <idx> <url> <bytes> <srvlog> [timeout]
#                              一轮完整下载计时（见下）
#   clock-offset <n>           设备/Mac 时钟偏移（各 n 次 date +%s.%N 取中位）
#
# round 语义：起 1 Hz loadavg 采样 → 记 vpn pid / cpu / net 基线 → 浏览器开 <url> →
#   轮询 <srvlog> 出现 `?r=<arm><cond><idx>` 且字节数 = <bytes> 的行 → 记轮末读数 →
#   落 /tmp/m6-ab/rounds.tsv（列：时刻/臂/态/标签/墙钟/字节/CPU秒/占空/设备字节/loadavg 峰值）。
#   作废判定（设计 §2.4）：轮首 loadavg >4 或峰值 >6 ⇒ 标 void；服务端未收满 ⇒ void。
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-/Users/zhaozhe/Documents/projects/homeway-rs}"
HDC="${HDC:-/Users/zhaozhe/.local/bin/hdc}"
DEV="${DEV:-FMR0224116011480}"
BUNDLE="${BUNDLE:-me.zhaozhe.tier}"
DEV_LOG="/data/app/el2/100/base/${BUNDLE}/haps/entry/files/tailcat-tun.log"
AB=/tmp/m6-ab
DEVOUT=/tmp/m6-dev
mkdir -p "$AB" "$DEVOUT"

hdc() { "$HDC" -t "$DEV" "$@"; }

case "${1:-help}" in
loadavg)
  secs="${2:-60}"; out="${3:-$AB/loadavg.tsv}"
  : > "$out"
  for _ in $(seq 1 "$secs"); do
    v=$(uptime | sed -E 's/.*load averages?: *([0-9.]+).*/\1/')
    printf '%s\t%s\n' "$(date '+%T')" "$v" >> "$out"
    sleep 1
  done
  awk -F'\t' 'BEGIN{m=0;s=0;n=0} {v=$2+0; if(v>m)m=v; s+=v; n++} END{printf "max=%s mean=%.2f n=%d\n", m, (n?s/n:0), n}' "$out"
  ;;

cpu-time)
  hdc shell "ps -o time= -p ${2:?pid}" 2>/dev/null | tr -d ' \r'
  ;;

net)
  hdc shell "hidumper --net ${2:?pid}" 2>/dev/null \
    | grep -E 'Received Bytes|Sent Bytes' | tr -d ' ' | paste -sd' ' -
  ;;

mem)
  hdc shell "hidumper --mem ${2:?pid}" 2>/dev/null | head -20
  ;;

vpnpid)
  hdc shell "ps -ef | grep 'me.zhaozhe.tier:vpn' | grep -v grep" 2>/dev/null | awk '{print $2}' | head -1
  ;;

dev-reset)
  tok="${2:?token}"; tries="${3:-25}"
  hdc shell "power-shell wakeup" >/dev/null 2>&1
  hdc shell "aa force-stop $BUNDLE" >/dev/null 2>&1
  sleep 1
  hdc shell "aa start -a EntryAbility -b $BUNDLE --ps host_token '$tok'" >/dev/null 2>&1
  # 等 App 起来（进程在场）
  for _ in $(seq 1 "$tries"); do
    pid=$(hdc shell "ps -ef | grep -v grep | grep -c '$BUNDLE$'" 2>/dev/null | tr -d ' \r')
    [[ "${pid:-0}" -ge 1 ]] && break
    sleep 1
  done
  echo "app_up tries=$((tries))"
  ;;

dev-tap)
  hdc shell "uitest uiInput click ${2:?x} ${3:?y}" >/dev/null 2>&1
  echo "tap ${2} ${3}"
  ;;

dev-uidump)
  out="${2:-$DEVOUT/ui.json}"
  hdc shell "uitest dumpLayout -p /data/local/tmp/ui.json" >/dev/null 2>&1
  hdc file recv /data/local/tmp/ui.json "$out" >/dev/null 2>&1
  echo "dump=$out"
  ;;

dev-open)
  # S1 实测：不带 -b/-a 的 `aa start -U` 会弹系统应用选择器 ⇒ 显式指定浏览器
  hdc shell "power-shell wakeup" >/dev/null 2>&1
  hdc shell "aa start -b com.huawei.hmos.browser -a MainAbility -U '${2:?url}'" 2>&1 | tail -2
  ;;

pull)
  tag="${2:?tag}"
  hdc file recv "$DEV_LOG" "$DEVOUT/tailcat-tun-$tag.log" >/dev/null 2>&1 || echo "!! 核日志拉取失败"
  hdc shell "hilog -x -T TierVPN" > "$DEVOUT/hilog-$tag.txt" 2>/dev/null || true
  hdc shell "hilog -x" > "$DEVOUT/hilog-all-$tag.txt" 2>/dev/null || true
  echo "pulled tag=${tag}（$(grep -ac . "$DEVOUT/tailcat-tun-$tag.log" 2>/dev/null || echo 0) 行核日志）"
  ;;

clock-offset)
  n="${2:-10}"; out="$DEVOUT/clock-offset.txt"
  : > "$out"
  for _ in $(seq 1 "$n"); do
    d=$(hdc shell "date +%s.%N" 2>/dev/null | tr -d '\r')
    m=$(date +%s.%N)
    printf '%s\t%s\n' "$d" "$m" >> "$out"
  done
  python3 - "$out" <<'PY'
import sys, statistics
pairs=[l.split() for l in open(sys.argv[1]) if l.strip()]
d=[(float(a)-float(b)) for a,b in pairs]
print(f"n={len(d)} median_offset_s={statistics.median(d):+.4f} min={min(d):+.4f} max={max(d):+.4f}")
PY
  ;;

round)
  arm="${2:?arm}"; cond="${3:?cond}"; idx="${4:?idx}"; url="${5:?url}"; bytes="${6:?bytes}"; srv="${7:?srvlog}"
  tmo="${8:-600}"
  label="${arm}${cond}${idx}"
  ts=$(date '+%T')
  la_before=$(uptime | sed -E 's/.*load averages?: *([0-9.]+).*/\1/')
  pid=$(hdc shell "ps -ef | grep 'me.zhaozhe.tier:vpn' | grep -v grep" 2>/dev/null | awk '{print $2}' | head -1)
  cpu0=$( [[ -n "$pid" ]] && hdc shell "ps -o time= -p $pid" 2>/dev/null | tr -d ' \r\n')
  net0=$(hdc shell "hidumper --net ${pid:-0}" 2>/dev/null | grep -E 'Received Bytes|Sent Bytes' | tr -d ' ' | paste -sd' ' -)
  "$0" loadavg "$tmo" "$AB/loadavg-$label.tsv" > "$AB/loadavg-$label.sum" 2>&1 &
  lapid=$!
  "$0" dev-open "$url" >/dev/null 2>&1
  # 浏览器对 octet-stream 会弹「下载」确认框（S1 实测：download_check_dialog_confirm_button）；
  # 先等框出现（dumpLayout 取坐标，缺省回退 S1 实测坐标），再点「立即下载」。
  tapx=""; tapy=""
  for _ in $(seq 1 8); do
    sleep 2
    "$0" dev-uidump "$DEVOUT/ui-round.json" >/dev/null 2>&1
    coords=$(python3 - "$DEVOUT/ui-round.json" <<'PY'
import json,sys,re
try: d=json.load(open(sys.argv[1]))
except Exception: raise SystemExit
def w(n):
    a=n.get('attributes',{}); i=a.get('id','') or ''
    if 'download_check_dialog_confirm_button' in i:
        m=re.match(r'\[(\d+),(\d+)\]\[(\d+),(\d+)\]', a.get('bounds','') or '')
        if m:
            x1,y1,x2,y2=map(int,m.groups()); print((x1+x2)//2,(y1+y2)//2); raise SystemExit
    for c in n.get('children',[]) or []: w(c)
w(d)
PY
)
    if [[ -n "$coords" ]]; then tapx=${coords% *}; tapy=${coords#* }; break; fi
  done
  [[ -z "$tapx" ]] && { tapx=906; tapy=1595; }
  "$0" dev-tap "$tapx" "$tapy" >/dev/null 2>&1
  t0=$(python3 -c 'import time;print(time.time())')
  got=""
  while :; do
    now=$(python3 -c 'import time;print(time.time())')
    el=$(python3 -c "print(f'{$now-$t0:.1f}')")
    if python3 -c "print(1 if $el > $tmo else 0)" | grep -q 1; then break; fi
    row=$(grep -a "r=${label}	" "$srv" 2>/dev/null | tail -1)
    if [[ -z "$row" ]]; then row=$(grep -a "r=${label}" "$srv" 2>/dev/null | grep -av 'ping' | tail -1); fi
    if [[ -n "$row" ]]; then
      got=$(echo "$row" | awk -F'\t' '{print $4"\t"$5"\t"$6}')
      b=$(echo "$row" | awk -F'\t' '{print $6}')
      [[ "$b" == "$bytes" ]] && break
    fi
    sleep 1
  done
  wall=$(python3 -c "import time;print(f'{time.time()-$t0:.3f}')")
  kill "$lapid" 2>/dev/null; wait "$lapid" 2>/dev/null
  la=$(awk -F'\t' 'BEGIN{m=0;s=0;n=0} {v=$2+0; if(v>m)m=v; s+=v; n++} END{printf "max=%s mean=%.2f n=%d", m, (n?s/n:0), n}' "$AB/loadavg-$label.tsv" 2>/dev/null)
  cpu1=$( [[ -n "$pid" ]] && hdc shell "ps -o time= -p $pid" 2>/dev/null | tr -d ' \r\n')
  net1=$(hdc shell "hidumper --net ${pid:-0}" 2>/dev/null | grep -E 'Received Bytes|Sent Bytes' | tr -d ' ' | paste -sd' ' -)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$ts" "$arm" "$cond" "$label" "$wall" "${got:-MISS}" "$pid" "$cpu0" "$cpu1" "$net0" "$net1" "$la" \
    >> "$AB/rounds.tsv"
  echo "round label=$label wall=${wall}s got=${got:-MISS} pid=$pid cpu=${cpu0}→${cpu1} la=[$la]"
  ;;

help|*)
  sed -n '2,30p' "$0"
  ;;
esac
