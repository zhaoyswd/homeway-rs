#!/bin/zsh
# quic-ab.sh — WG vs QUIC 承载对比实验台（M0 转正自 /tmp/quic-lab）。
#
# 真源与口径：docs/reviews/M0-design.md §4 + tools/quic-ab/README.md（逐项映射表）。
# 纪律（与 qi-ab.sh 的差别）：**完全不碰 homeway-cli / 任何生产实例**——四臂都是独立探针，
# 只绑 127.0.0.1:0（内核分配端口后读回），无 state 目录/出口/token；收工**只按记录的 PID** kill。
#
# 用法：
#   tools/quic-ab.sh cpu      [--arms raw,wg-shim,quic] [--payload 1280] [--n 60000] [--rounds 3] [--mtu 1400] [--profile lab|product]
#   tools/quic-ab.sh overhead [--n 60000] [--mtu 1400] [--payload 1280] [--profile lab|product]
#   tools/quic-ab.sh size     [--profile lab,product]
#   tools/quic-ab.sh mem      [--mode steady|load|conns|conns-load|rss] [--arms ...] [--rounds 3]
#                             [--conns N（奇数序列 1,3,5,…，N=连接数上界）]
#                             [--conns-points "1,2,3,4,5"（显式点集；缺省 = 五点口径）]
#                             [conns-load 档环境旋钮：CONNS_LOAD_N/RATE/SIZE/AFTER/SAMPLES]
#   tools/quic-ab.sh all      （顺序跑 cpu → overhead → size → mem）
#
# 产物：${QUIC_AB_DIR:-/tmp/quic-ab/<时间戳>}/（逐轮原始件 + summary.txt + bins.sha256 + loadavg.tsv）
set -uo pipefail

REPO_ROOT="${0:h:A:h}"
AB_ROOT="$REPO_ROOT/tools/quic-ab"
ARMS_DIR="$AB_ROOT/arms"
SHIM_DIR="$AB_ROOT/wg-shim"
OUT="${QUIC_AB_DIR:-/tmp/quic-ab/$(date +%Y%m%d-%H%M%S)}"
NDK="${OHOS_NDK:-/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native}"
NDK_CC="$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang"
NDK_STRIP="$NDK/llvm/bin/llvm-strip"
OHOS_TARGET="aarch64-unknown-linux-ohos"

# ---------- 参数默认（口径见 README 判据表） ----------
SUB="${1:-}"; shift 2>/dev/null || true
ARMS="raw,wg-shim,quic"      # 默认三臂；wg-ring 是诊断臂（--arms 显式加）
PAYLOAD=1280
N=60000
ROUNDS=3
MTU=1400
PROFILE="lab"
MODE="steady"
CONNS=5
CONNS_POINTS=""    # 空 = 缺省点集（见下方 mem_conns）；显式 `--conns-points` 优先
CONNS_GIVEN=0      # `--conns N` 显式给出 ⇒ 回到「1,3,5,… 到 N」的奇数序列口径
# 缺省点集（M1 S5-1a：**三点 → 五点**，依据 `docs/reviews/M1-design.md` §9.1-1 的裁决
# 「拟合口径改五点（N=1..5）+ 三点仅作对照」——三点拟合的 base 截距与斜率对 16K 页粒度
# 台阶极敏感（M0 同一份数据 96.0K vs 81.6K 的差就来自采样密度）。
# 三点对照的复现命令：`mem --mode conns --conns-points 1,3,5`。
DEFAULT_CONNS_POINTS="1,2,3,4,5"
ARM_LIST=("${(@s:,:)ARMS}")

while (( $# )); do
  case "$1" in
    --arms) ARMS="$2"; ARM_LIST=("${(@s:,:)ARMS}"); shift 2 ;;
    --payload) PAYLOAD="$2"; shift 2 ;;
    --n) N="$2"; shift 2 ;;
    --rounds) ROUNDS="$2"; shift 2 ;;
    --mtu) MTU="$2"; shift 2 ;;
    --profile) PROFILE="$2"; shift 2 ;;
    --mode) MODE="$2"; shift 2 ;;
    --conns) CONNS="$2"; CONNS_GIVEN=1; shift 2 ;;
    --conns-points) CONNS_POINTS="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "!! 未知参数：$1" >&2; exit 2 ;;
  esac
done

die() { echo "!! $*" >&2; cleanup; exit 1; }
log() { echo "[$(date '+%H:%M:%S')] $*"; }

# ---------- 收工：只杀本 harness 记录过的 PID（绝不泛匹配——主检出可能有现役实例） ----------
typeset -a KIDS=()
cleanup() {
  for pid in $KIDS; do
    kill "$pid" 2>/dev/null || true
  done
  [[ -n "${LOADAVG_PID:-}" ]] && kill "$LOADAVG_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# certs：`arms/quic` 是**编译期** include_bytes(certs/*.der) ⇒ 干净 clone 上必须先现场生成
# （DER/PEM 不入库；缺 openssl 即显式红，不静默）
ensure_certs() {
  [[ -f "$AB_ROOT/certs/cert.der" && -f "$AB_ROOT/certs/key.der" ]] && return 0
  command -v openssl >/dev/null 2>&1 || die "缺 certs/*.der 且系统无 openssl——先装 openssl 或手工跑 tools/quic-ab/certs/gen_certs.sh"
  log "现场生成自签证书（tools/quic-ab/certs/gen_certs.sh）…"
  "$AB_ROOT/certs/gen_certs.sh" > /dev/null || die "证书生成失败"
}

mkdir -p "$OUT"
: > "$OUT/loadavg.tsv"
ensure_certs
# loadavg 1Hz 采样（带时间戳；轮首/轮末由 mark() 打标记）
mark() { printf -- '-- %s\n' "$1" >> "$OUT/loadavg.tsv"; }
(
  while :; do
    printf '%s\t%s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$(sysctl -n vm.loadavg 2>/dev/null || cat /proc/loadavg)" >> "$OUT/loadavg.tsv"
    sleep 1
  done
) &
LOADAVG_PID=$!

# ---------- 工具小件 ----------
# 下中位（lab 口径：偶数点取 a[int((NR+1)/2)]——**照搬，勿当 bug 修**）
lower_median() {
  printf '%s\n' "$@" | grep -v '^$' | sort -n | awk '{a[NR]=$1} END{ if (NR==0) {print "NA"; exit} print a[int((NR+1)/2)] }'
}
# JSON 取字段（探针输出是单行扁平 JSON；不引 jq）
jget() { sed -n "s/.*\"$2\":\([^,}]*\).*/\1/p" <<< "$1"; }
runner() {  # runner <档目录> <臂> → 二进制路径
  local p="$1"
  case "$2" in
    raw)     echo "$ARMS_DIR/target/$p/raw" ;;
    wg|wg-ring) echo "$ARMS_DIR/target/$p/wg" ;;
    wg-shim) echo "$SHIM_DIR/target/$p/wg" ;;
    quic)    echo "$ARMS_DIR/target/$p/quic" ;;
    multiconn) echo "$ARMS_DIR/target/$p/multiconn" ;;
    *) die "未知臂：$2" ;;
  esac
}
profile_dir() {  # 显式映射；未知档位即红（防 `--profile lab,product` 之类静默跑成 product）
  case "$1" in
    lab) echo "release" ;;
    product) echo "product" ;;
    *) die "未知 --profile 档位：$1（只认 lab / product）" ;;
  esac
}
# `--profile` 允许逗号列表（例：lab,product）：cpu/overhead/mem 用**第一个**；size 逐个跑。
PROFILES=("${(@s:,:)PROFILE}")

# 等服务端打印 PORT（内核分配端口后读回——**全程无固定端口**），回显端口
wait_port() {  # wait_port <log> [timeout_ds=100]
  local log="$1" i=0
  while (( i < ${2:-100} )); do
    local p=""; p=$(grep -m1 '^PORT ' "$log" 2>/dev/null | awk '{print $2}')
    if [[ -n "$p" ]]; then echo "$p"; return 0; fi
    sleep 0.1; (( i++ ))
  done
  return 1
}

# 起服务端：跑 <bin> server …，等 PORT 行，回显 "pid port"
start_server() {  # start_server <bin> <logfile> [env…]
  local bin="$1" log="$2"; shift 2
  env "$@" "$bin" server > "$log" 2>&1 &
  local pid=$!
  KIDS+=($pid)
  local p=""
  if ! p=$(wait_port "$log"); then
    echo "!! 服务端未在 10s 内报告 PORT：$bin（见 $log）" >&2
    return 1
  fi
  echo "$pid $p"
}

# 起服务端（带 argv：quic 的 oneway 口径要 `server <expect>`）
start_server_expect() {  # start_server_expect <bin> <log> <expect> [env…]
  local bin="$1" log="$2" expect="$3"; shift 3
  env "$@" "$bin" server "$expect" > "$log" 2>&1 &
  local pid=$!
  KIDS+=($pid)
  local p=""
  if ! p=$(wait_port "$log"); then
    echo "!! 服务端未在 10s 内报告 PORT：$bin（见 $log）" >&2
    return 1
  fi
  echo "$pid $p"
}

# ---------- 构建 ----------
build_probe() {  # build_probe <prof-dir 名> → 构建两 workspace 的探针（lab=release / product=profile product）
  local p="$1" args=()
  [[ "$p" == "product" ]] && args=(--profile product) || args=(--release)
  log "构建探针（$p 档）…"
  ( cd "$ARMS_DIR" && cargo build "${args[@]}" --bins > "$OUT/build-arms-$p.log" 2>&1 ) || { tail -20 "$OUT/build-arms-$p.log"; die "arms 构建失败"; }
  ( cd "$SHIM_DIR" && cargo build "${args[@]}" > "$OUT/build-shim-$p.log" 2>&1 ) || { tail -20 "$OUT/build-shim-$p.log"; die "wg-shim 构建失败"; }
  # 自检（M0 设计 §4.4）：非根 [profile] 会被 cargo 忽略（静默失效）⇒ 有该警告即红
  if grep -qi 'profiles for the non root package will be ignored' "$OUT/build-arms-$p.log" "$OUT/build-shim-$p.log"; then
    die "构建日志出现「非根 profile 被忽略」——档位会静默失效"
  fi
  # 执行用二进制指纹（**构建时**快照——收尾重算会与「本次真正跑的那批」不一致，代码门 L4）
  {
    echo "=== 档位 $p / $(date '+%Y-%m-%d %H:%M:%S')（本次执行用二进制快照）==="
    for f in "$ARMS_DIR/target/$p/raw" "$ARMS_DIR/target/$p/wg" "$ARMS_DIR/target/$p/wg_size" \
             "$ARMS_DIR/target/$p/quic" "$ARMS_DIR/target/$p/multiconn" "$SHIM_DIR/target/$p/wg"; do
      [[ -f "$f" ]] && shasum -a 256 "$f"
    done
  } >> "$OUT/bins.sha256"

  # 档位记录（**cargo metadata 不暴露 profile 值** ⇒ 记 manifest 原文 + 两档尺寸差对照，
  # 见 size 子命令；两者合起来即「档位没静默失效」的证据）
  {
    echo "=== profile 记录（$p 档，$(date '+%Y-%m-%d %H:%M:%S')）==="
    echo "-- $ARMS_DIR/Cargo.toml"; sed -n '/\[profile\./,$p' "$ARMS_DIR/Cargo.toml"
    echo "-- $SHIM_DIR/Cargo.toml";  sed -n '/\[profile\./,$p' "$SHIM_DIR/Cargo.toml"
    echo ""
  } >> "$OUT/profile-record.txt"
}

# ---------- cpu ----------
run_arm_cpu() {  # run_arm_cpu <arm> <round> <prof-dir>
  local arm="$1" r="$2" p="$3"
  local bin=""; bin=$(runner "$p" "$arm")
  [[ -x "$bin" ]] || die "探针不在：$bin"
  local log="$OUT/srv-$arm-r$r.out" json="$OUT/cpu-$arm-r$r.json"
  local sp pid port
  case "$arm" in
    quic)  sp=$(start_server "$bin" "$log" "MTU=$MTU") || die "起服务端失败（$arm）" ;;
    *)     sp=$(start_server "$bin" "$log") || die "起服务端失败（$arm）" ;;
  esac
  pid=${sp%% *}; port=${sp##* }
  sleep 0.8
  case "$arm" in
    raw)  PAYLOAD=$PAYLOAD "$bin" "$port" "$N" > "$json" 2> "$log.cli" ;;
    quic) MTU=$MTU "$bin" "$port" "$N" > "$json" 2> "$log.cli" ;;
    *)    ARM=$arm PAYLOAD=$PAYLOAD "$bin" "$port" "$N" > "$json" 2> "$log.cli" ;;
  esac
  kill "$pid" 2>/dev/null || true
  sleep 0.3
  [[ -s "$json" ]] || die "客户端无 JSON 输出（$arm r$r；见 $log.cli）"
  local us=""; us=$(jget "$(cat "$json")" cpu_us_per_pkt)
  printf '  %-8s r%s: cpu_us_per_pkt=%s\n' "$arm" "$r" "$us"
}

cmd_cpu() {
  local p=""; p=$(profile_dir "${PROFILES[1]}")
  build_probe "$p"
  local med=()
  for r in $(seq 1 $ROUNDS); do
    mark "round $r start"
    local order=("${ARM_LIST[@]}")
    (( r % 2 == 0 )) && order=("${(@Oa)ARM_LIST[@]}")   # 轮序平衡（偶数轮倒序）
    for arm in "${order[@]}"; do
      run_arm_cpu "$arm" "$r" "$p"
    done
    mark "round $r end"
  done
  echo "" >> "$OUT/summary.txt"
  echo "=== cpu（$PROFILE 档；N=$N PAYLOAD=$PAYLOAD MTU=$MTU rounds=$ROUNDS）===" >> "$OUT/summary.txt"
  for arm in "${ARM_LIST[@]}"; do
    local vals=()
    for r in $(seq 1 $ROUNDS); do
      vals+=("$(jget "$(cat "$OUT/cpu-$arm-r$r.json" 2>/dev/null)" cpu_us_per_pkt)")
    done
    local m=""; m=$(lower_median "${vals[@]}")
    printf '%-8s 每包 CPU = %s µs（三轮下中位；逐轮 %s）\n' "$arm" "$m" "${(j:, :)vals}" | tee -a "$OUT/summary.txt"
  done
}

# ---------- overhead ----------
cmd_overhead() {
  local p=""; p=$(profile_dir "${PROFILES[1]}")
  build_probe "$p"
  local out="$OUT/overhead.txt"; : > "$out"
  mark "overhead start"
  # ① WG 线上字节（wg_size 探针：会话建立后 encapsulate 一枚数据包的长度）
  local wgsz="$ARMS_DIR/target/$p/wg_size"
  [[ -x "$wgsz" ]] || die "wg_size 探针不在：$wgsz"
  local wgjson=""; wgjson=$(PAYLOAD=$PAYLOAD "$wgsz")
  echo "WG（$PAYLOAD B 载荷）: $wgjson" | tee -a "$out" "$OUT/summary.txt"
  # ② QUIC oneway（服务端 udp_rx 口径 = 线开销）
  local qbin=""; qbin=$(runner "$p" quic)
  local slog="$OUT/ow-srv.out"
  local sp=""; sp=$(start_server_expect "$qbin" "$slog" "$N" "MTU=$MTU") || die "QUIC oneway 服务端起不来"
  local pid=${sp%% *}; local port=${sp##* }
  sleep 1.2
  MTU=$MTU "$qbin" "$port" "$N" --oneway > "$OUT/ow-cli.json" 2> "$OUT/ow-cli.err"
  sleep 2.0   # 等服务端打印（收尾 1500ms ACK 窗）
  local srvline=""; srvline=$(grep -m1 'quic-oneway-srv' "$slog" || true)
  echo "QUIC oneway（MTU=$MTU；服务端 udp_rx）: ${srvline:-（缺）}" | tee -a "$out" "$OUT/summary.txt"
  kill "$pid" 2>/dev/null || true
  # ③ max_datagram_size 精确打印（附录 A 判据：MTU1200→1162 / MTU1400→1362）
  for mtu in 1200 1400; do
    local lg="$OUT/mds-$mtu.out"
    local sp2=""; sp2=$(start_server "$qbin" "$lg" "MTU=$mtu") || die "起服务端失败（MTU=$mtu）"
    local pid2=${sp2%% *}; local port2=${sp2##* }
    sleep 1.0
    MTU=$mtu "$qbin" "$port2" 2000 > "$OUT/mds-$mtu.json" 2> "$OUT/mds-$mtu.err"
    kill "$pid2" 2>/dev/null || true
    local line=""; line=$(grep -m1 'max_datagram_size' "$OUT/mds-$mtu.err" || echo "（缺）")
    echo "MTU=$mtu: $line" | tee -a "$out" "$OUT/summary.txt"
  done
  mark "overhead end"
}

# ---------- size ----------
# lab 档三格 = 同一探针的三次 OHOS cdylib 构建（feature 切换）；第四格 = 现役 .so（只读对照）。
cmd_size() {
  [[ -x "$NDK_CC" ]] || die "size 需要 NDK 包装 clang：$NDK_CC（真链接路径，不可省）"
  [[ "${CFLAGS_aarch64_unknown_linux_ohos:-}" != *nostdlibinc* ]] || die "CFLAGS 含 -nostdlibinc（check 档 flags 不得进真链接——M0 设计 §4.5 前置）"
  export CC_aarch64_unknown_linux_ohos="$NDK_CC"
  local out="$OUT/size-matrix.txt"; : > "$out"
  # lab 档三格（M0 设计 §4.3/§5.1）：①② 来自空壳探针（v1 形态，无 boringtun/smoltcp），
  # ③ 来自 v2 形态探针（boringtun+smoltcp 在场 + 真引用全路径）。第④格 = 现役 .so 对照。
  # `--profile` 列表逐档全跑（lab = target/release；product = target/product）；
  # 注意 `lab_so` 取的是**最后一个 real 档**的值，两档尺寸差只在 lab∈列表时打印。
  local lab_so=""
  local -a cfgs=(
    "shell:quic-ab-size-shell:libclientcore.so:"
    "shell-quic:quic-ab-size-shell:libclientcore.so:--features quic"
    "real:quic-ab-size:libclientcore2.so:--features quic"
  )
  for prof in "${PROFILES[@]}"; do
  local pdir=""; pdir=$(profile_dir "$prof")
  for cfg in "${cfgs[@]}"; do
    local name="${cfg%%:*}"
    local rest="${cfg#*:}"; local pkg="${rest%%:*}"
    rest="${rest#*:}"; local so_base="${rest%%:*}"; local feats="${rest#*:}"
    local args=()
    [[ -n "$feats" ]] && args=("${(@s: :)feats}")
    if [[ "$pdir" == "release" ]]; then
      ( cd "$ARMS_DIR" && cargo build --release --target "$OHOS_TARGET" -p "$pkg" "${args[@]}" > "$OUT/size-$name.log" 2>&1 ) || { tail -20 "$OUT/size-$name.log"; die "size 构建失败（$name）"; }
    else
      ( cd "$ARMS_DIR" && cargo build --profile product --target "$OHOS_TARGET" -p "$pkg" "${args[@]}" > "$OUT/size-$name.log" 2>&1 ) || { tail -20 "$OUT/size-$name.log"; die "size 构建失败（$name，product 档）"; }
    fi
    local rel="target/$OHOS_TARGET/$pdir/${so_base}"
    local bn="${so_base%.so}"
    local stripped="$OUT/${bn}-$name-$prof.so"
    cp "$ARMS_DIR/$rel" "$stripped"
    "$NDK_STRIP" "$stripped"
    local sz=""; sz=$(wc -c < "$stripped" | tr -d ' ')
    printf '%-8s %-11s %12s B（%s %s）\n' "$prof 档" "$name" "$sz" "$pkg" "$feats" | tee -a "$out" "$OUT/summary.txt"
    [[ "$name" == "real" ]] && lab_so="$sz"
  done
  done
  # 第四格：现役 .so 对照（主检出只读；不存在则显式登记缺位）
  local main_so="${HOMEWAY_MAIN_SO:-$REPO_ROOT/../homeway-rs/target/$OHOS_TARGET/release/libclientcore.so}"
  if [[ -f "$main_so" ]]; then
    local sz sha
    sz=$(wc -c < "$main_so" | tr -d ' ')
    sha=$(shasum -a 256 "$main_so" | cut -c1-12)
    printf '现役对照 %-8s %12s B（sha256 %s…；%s）\n' "so" "$sz" "$sha" "$main_so" | tee -a "$out" "$OUT/summary.txt"
  else
    printf '现役对照 缺位（%s 不在——只读对照跳过）\n' "$main_so" | tee -a "$out" "$OUT/summary.txt"
  fi
  # product 档：同一探针换 profile（无 LTO/unwind/不 strip）⇒ 两档尺寸差 = 档位没死的证据
  ( cd "$ARMS_DIR" && cargo build --profile product --target "$OHOS_TARGET" -p quic-ab-size --features quic > "$OUT/size-product.log" 2>&1 ) || { tail -20 "$OUT/size-product.log"; die "size product 档构建失败"; }
  local pso="$ARMS_DIR/target/$OHOS_TARGET/product/libclientcore2.so"
  local psz=""; psz=$(wc -c < "$pso" | tr -d ' ')
  printf 'product 档 real %8s B（探针；只登记）\n' "$psz" | tee -a "$out" "$OUT/summary.txt"
  if (( ${PROFILES[(I)lab]} )) && [[ -n "$lab_so" ]]; then
    printf '两档尺寸差（real 档）：lab=%s B vs product=%s B（差 %s B，档位有效）\n' "$lab_so" "$psz" "$(( psz - lab_so ))" | tee -a "$out" "$OUT/summary.txt"
  else
    printf '两档尺寸差：未同时跑 lab 与 product（--profile=%s）⇒ 跳过对照\n' "$PROFILE" | tee -a "$out" "$OUT/summary.txt"
  fi
  # 真产品面：build-app-core.sh（product 档 + NDK strip）= M0 硬判据的产物 + 增量
  if [[ -x "$REPO_ROOT/tools/build-app-core.sh" ]]; then
    "$REPO_ROOT/tools/build-app-core.sh" > "$OUT/app-core.log" 2>&1 || { tail -20 "$OUT/app-core.log"; die "build-app-core.sh 失败（OHOS 真链接）"; }
    grep -E '^\[(cc|sym|ver|size)\]' "$OUT/app-core.log" | tee -a "$out" "$OUT/summary.txt"
  fi
}

# ---------- mem ----------
sample_footprint() {  # sample_footprint <pid> <n> → 采样 K 值（换行分隔）
  local pid="$1" n="$2" s=()
  for _ in $(seq 1 $n); do
    local f=""
    f=$(vmmap -summary "$pid" 2>/dev/null | awk '/Physical footprint:/{gsub(/[^0-9]/,"",$3); print $3; exit}')
    [[ -n "$f" ]] && s+=("$f")
    sleep 0.5
  done
  printf '%s\n' "${s[@]}"
}
start_idle_client() {  # start_idle_client <arm> <prof-dir> <idle_secs> <srvlog> → 回显 "pid port"
  local arm="$1" p="$2" idle="$3" srvlog="$4"
  local bin=""; bin=$(runner "$p" "$arm")
  local srv_env=()
  [[ "$arm" == "quic" ]] && srv_env=(MTU=$MTU)
  local sp=""; sp=$(start_server "$bin" "$srvlog" "${srv_env[@]}") || die "起服务端失败（$arm）"
  local pid=${sp%% *}; local port=${sp##* }
  sleep 1.2
  local clilog="$OUT/idle-$arm-cli.out"
  case "$arm" in
    quic) MTU=$MTU IDLE_SECS=$idle "$bin" "$port" 2000 > "$clilog" 2>&1 & ;;
    raw)  IDLE_SECS=$idle "$bin" "$port" 2000 > "$clilog" 2>&1 & ;;
    *)    ARM=$arm IDLE_SECS=$idle "$bin" "$port" 2000 > "$clilog" 2>&1 & ;;
  esac
  local cpid=$!
  KIDS+=($cpid)
  local i=0
  while (( i < 100 )); do
    grep -q "IDLE 模式" "$clilog" 2>/dev/null && break
    sleep 0.2; (( i++ ))
  done
  echo "$cpid $port $pid"
}

cmd_mem() {
  local p=""; p=$(profile_dir "${PROFILES[1]}")
  build_probe "$p"
  local out="$OUT/mem-$MODE.txt"; : > "$out"
  mark "mem($MODE) start"
  case "$MODE" in
    steady)  mem_steady "$p" "$out" ;;
    load)    mem_load "$p" "$out" ;;
    conns)   mem_conns "$p" "$out" ;;
    conns-load) mem_conns_load "$p" "$out" ;;
    rss)     mem_rss "$p" "$out" ;;
    *) die "未知 --mode：$MODE" ;;
  esac
  mark "mem($MODE) end"
}

mem_steady() {  # fp_probe.sh 口径：暖机 1.2s → IDLE 9s → 8 次采样取中位 → 三轮再取中位
  local p="$1" out="$2"
  for arm in "${ARM_LIST[@]}"; do
    local rounds=()
    for r in $(seq 1 $ROUNDS); do
      local res=""; res=$(start_idle_client "$arm" "$p" 9 "$OUT/fp-$arm-srv$r.out") || die "起臂失败"
      local cpid=${res%% *}; local rest=${res#* }
      local port=${rest%% *}; local spid=${rest##* }
      sleep 1.5
      local vals=(${(f)"$(sample_footprint "$cpid" 8)"})
      local m=""; m=$(lower_median "${vals[@]}")
      rounds+=("$m")
      printf '  %-8s r%s: footprint=%sK\n' "$arm" "$r" "$m"
      kill "$cpid" "$spid" 2>/dev/null || true
      sleep 0.4
    done
    local med=""; med=$(lower_median "${rounds[@]}")
    echo "$med" >> "$OUT/fp-$arm.med"
    printf '%-8s 稳态 footprint = %sK（三轮下中位；逐轮 %s）\n' "$arm" "$med" "${(j:, :)rounds}" | tee -a "$out" "$OUT/summary.txt"
  done
}

mem_load() {  # peak_probe.sh 口径：N=300000 传输中每 250ms 采样 max + peak（只登记，不设硬判）
  local p="$1" out="$2"
  local n=300000
  for arm in "${ARM_LIST[@]}"; do
    local bin=""; bin=$(runner "$p" "$arm")
    local slog="$OUT/pk-$arm-srv.out"
    local srv_env=()
    [[ "$arm" == "quic" ]] && srv_env=(MTU=$MTU)
    local sp=""; sp=$(start_server "$bin" "$slog" "${srv_env[@]}") || die "起服务端失败"
    local spid=${sp%% *}; local port=${sp##* }
    sleep 1.2
    case "$arm" in
      quic) MTU=$MTU "$bin" "$port" "$n" > "$OUT/pk-$arm-cli.out" 2>&1 & ;;
      *)    ARM=$arm PAYLOAD=$PAYLOAD "$bin" "$port" "$n" > "$OUT/pk-$arm-cli.out" 2>&1 & ;;
    esac
    local cpid=$!; KIDS+=($cpid)
    local max=0 peak=0
    for _ in $(seq 1 120); do
      sleep 0.25
      ps -o rss= -p "$cpid" >/dev/null 2>&1 || break
      local f="" pk=""
      f=$(vmmap -summary "$cpid" 2>/dev/null | awk '/Physical footprint:/{gsub(/[^0-9]/,"",$3); print $3; exit}')
      pk=$(vmmap -summary "$cpid" 2>/dev/null | awk '/Physical footprint \(peak\):/{gsub(/[^0-9]/,"",$4); print $4; exit}')
      [[ -n "$f" ]] && (( f > max )) && max=$f
      [[ -n "$pk" ]] && (( pk > peak )) && peak=$pk
    done
    wait "$cpid" 2>/dev/null || true
    kill "$spid" 2>/dev/null || true
    printf '%-8s 负载中 max footprint=%sK vmmap peak=%sK（N=%s；只登记）\n' "$arm" "$max" "$peak" "$n" | tee -a "$out" "$OUT/summary.txt"
    sleep 0.3
  done
}

mem_conns() {  # multiconn：点集拟合 base + 每连接边际（服务端 footprint）
  # 口径（M1 S5-1a 裁决，`docs/reviews/M1-design.md` §9.1-1）：
  #   缺省 = **五点**（`DEFAULT_CONNS_POINTS`，1,2,3,4,5 —— M1 起生效；三点对照片
  #   用 `--conns-points 1,3,5` 显式指定）；
  #   `--conns N` ⇒ 取 1,3,5,… 到 N（奇数序列，M0 旧口径的**显式**形态，供 32 连接扩展）；
  #   `--conns-points "…"` 显式指定（优先于 --conns）。
  local p="$1" out="$2"
  local bin=""; bin=$(runner "$p" multiconn)
  [[ -x "$bin" ]] || die "multiconn 探针不在：$bin"
  local pts=()
  local fit_pts=()
  local pts_src=""
  if [[ -n "$CONNS_POINTS" ]]; then
    pts=("${(@s:,:)CONNS_POINTS}")
    pts_src="--conns-points（显式）"
  elif (( CONNS_GIVEN )); then
    local n=1
    while (( n <= CONNS )); do pts+=($n); (( n += 2 )); done
    (( ${#pts} >= 1 )) || pts=(1)
    pts_src="--conns=$CONNS（奇数序列）"
  else
    pts=("${(@s:,:)DEFAULT_CONNS_POINTS}")
    pts_src="缺省五点口径（M1 §9.1-1）"
  fi
  echo "点集：${(j:, :)pts}（口径 = $pts_src${CONNS_POINTS:+；--conns-points=$CONNS_POINTS}）" | tee -a "$out"
  for n in "${pts[@]}"; do
    local slog="$OUT/conns-$n-srv.out"
    "$bin" server "$n" > "$slog" 2>&1 &   # multiconn 的 server 读 argv[2]（不用 env）
    local spid=$!; KIDS+=($spid)
    local i=0 port=""
    while (( i < 100 )); do
      port=$(grep -m1 '^PORT ' "$slog" 2>/dev/null | awk '{print $2}')
      [[ -n "$port" ]] && break
      sleep 0.1; (( i++ ))
    done
    [[ -n "$port" ]] || die "multiconn 服务端未报 PORT（n=$n）"
    "$bin" "$port" "$n" > "$OUT/conns-$n-cli.out" 2>&1 &
    local cpid=$!; KIDS+=($cpid)
    sleep 2.5   # 建链 + hold
    local vals=(${(f)"$(sample_footprint "$spid" 6)"})
    local m=""; m=$(lower_median "${vals[@]}")
    pts+=("$n $m")
    printf '  conns=%s: 服务端 footprint=%sK\n' "$n" "$m"
    fit_pts+=("$n $m")
    kill "$cpid" "$spid" 2>/dev/null || true
    sleep 0.5
  done
  # 拟合：**全点最小二乘**（斜率 = Σ(x-x̄)(y-ȳ)/Σ(x-x̄)²）+ 端点斜率（供对照）
  local fit=""; fit=$(printf '%s\n' "${fit_pts[@]}" | awk '
    {x[NR]=$1; y[NR]=$2; sx+=$1; sy+=$2; n=NR}
    END{
      if (n<2) {print "点不足（<2）"; exit}
      mx=sx/n; my=sy/n; sxy=0; sxx=0;
      for(i=1;i<=n;i++){ sxy+=(x[i]-mx)*(y[i]-my); sxx+=(x[i]-mx)*(x[i]-mx) }
      slope = (sxx==0 ? 0 : sxy/sxx); base = my - slope*mx;
      printf "base=%.1fK 每连接边际=%.2fK（全点最小二乘，%d 点）", base, slope, n
      if (n>=2) printf "；端点斜率=%.2fK", (y[n]-y[1])/(x[n]-x[1])
    }')
  echo "multiconn（服务端）：$fit" | tee -a "$out" "$OUT/summary.txt"
  printf '%s\n' "${fit_pts[@]}" | tee -a "$out"
}

mem_rss() {  # **诊断档，不作判据**（lab 已证：同机两臂差 4.3MB 而二进制差 176B）  local p="$1" out="$2"
  for arm in "${ARM_LIST[@]}"; do
    local rounds=()
    for r in $(seq 1 $ROUNDS); do
      local res=""; res=$(start_idle_client "$arm" "$p" 12 "$OUT/rss-$arm-srv$r.out") || die "起臂失败"
      local cpid=${res%% *}; local rest=${res#* }
      local spid=${rest##* }
      sleep 1.0
      local vals=()
      for _ in $(seq 1 16); do
        local v=""; v=$(ps -o rss= -p "$cpid" 2>/dev/null | tr -d ' ')
        [[ -n "$v" ]] && vals+=("$v")
        sleep 0.5
      done
      local m=""; m=$(lower_median "${vals[@]}")
      rounds+=("$m")
      printf '  %-8s r%s: rss=%sK\n' "$arm" "$r" "$m"
      kill "$cpid" "$spid" 2>/dev/null || true
      sleep 0.4
    done
    local med=""; med=$(lower_median "${rounds[@]}")
    echo "$med" >> "$OUT/rss-$arm.med"
    printf '%-8s rss（**诊断档，非判据**）= %sK（三轮下中位）\n' "$arm" "$med" | tee -a "$out" "$OUT/summary.txt"
  done
}

# ---------- 负载态（M1 §9.1-4）：N 连接 + 持续流量的服务端 footprint ----------
# 判据（`docs/reviews/M1-design.md` §9.1-4）：「出口 32 连接 + 持续流量 ⇒ footprint 增量
# ≤ 64 MiB + 自有队列上限」（64 MiB = 32 × (1 MiB send + 1 MiB recv)，§6.3 裁决）。
# 同一轮里先采**空转**（建链后 start-after 窗内）再采**负载态**（流量窗内）⇒ 增量可直接算。
mem_conns_load() {
  local p="$1" out="$2"
  local bin=""; bin=$(runner "$p" multiconn)
  [[ -x "$bin" ]] || die "multiconn 探针不在：$bin"
  local n="${CONNS_LOAD_N:-32}"
  local rate="${CONNS_LOAD_RATE:-200}"   # 每连接 pps（N×rate = 总入流）
  local size="${CONNS_LOAD_SIZE:-1280}"
  local after="${CONNS_LOAD_AFTER:-6}"   # 空转相位长度（秒）
  local samples="${CONNS_LOAD_SAMPLES:-40}"  # 负载相位采样数（×0.5s）
  local dur=$(( after + samples / 2 + 6 ))
  local nread=()   # CONNS_LOAD_NO_READ=1 ⇒ 客户端不读回程（顶满服务端发送缓冲的最坏面）
  [[ "${CONNS_LOAD_NO_READ:-0}" == "1" ]] && nread=(--no-read)
  local slog="$OUT/connsload-$n-srv.out"
  local clog="$OUT/connsload-$n-cli.out"
  echo "负载态档：N=$n 每连接 $rate pps × $size B；空转相位 ${after}s，负载相位 $(( samples / 2 ))s；no_read=${CONNS_LOAD_NO_READ:-0}" | tee -a "$out"
  "$bin" server "$n" --load > "$slog" 2>&1 &
  local spid=$!; KIDS+=($spid)
  local i=0 port=""
  while (( i < 100 )); do
    port=$(grep -m1 '^PORT ' "$slog" 2>/dev/null | awk '{print $2}')
    [[ -n "$port" ]] && break
    sleep 0.1; (( i++ ))
  done
  [[ -n "$port" ]] || die "multiconn 服务端未报 PORT（conns-load）"
  "$bin" "$port" "$n" --load --rate "$rate" --size "$size" --dur "$dur" --start-after "$after" "${nread[@]}" > "$clog" 2>&1 &
  local cpid=$!; KIDS+=($cpid)
  sleep 2.5
  local idle=(${(f)"$(sample_footprint "$spid" 4)"})
  local idle_med=""; idle_med=$(lower_median "${idle[@]}")
  sleep $(( after - 2 ))
  local vals=(${(f)"$(sample_footprint "$spid" $samples)"})
  local load_med=""; load_med=$(lower_median "${vals[@]}")
  local peak=0 v
  for v in $vals; do (( v > peak )) && peak=$v; done
  local delta=$(( load_med - idle_med ))
  local limit=65536   # 64 MiB（K）
  local verdict="过"
  (( delta > limit )) && verdict="不过"
  printf 'multiconn 负载态（N=%s）：空转=%sK；负载态中位=%sK 峰值=%sK；增量=%sK（门槛 ≤%sK=64MiB；判=%s）\n' \
    "$n" "$idle_med" "$load_med" "$peak" "$delta" "$limit" "$verdict" | tee -a "$out" "$OUT/summary.txt"
  echo "（负载相位逐次采样，K）：${(j:, :)vals}" >> "$out"
  kill "$cpid" "$spid" 2>/dev/null || true
}

# ---------- 收束：bins 指纹 + 读数 ----------
finalize() {
  {
    echo ""
    echo "=== bins.sha256（构建时快照；全文见 bins.sha256）==="
    if [[ -f "$OUT/bins.sha256" ]]; then cat "$OUT/bins.sha256"; else echo "（本次未构建探针——size 子命令不含臂二进制）"; fi
    echo ""
    echo "=== 环境 ==="
    echo "host: $(uname -a)"
    echo "loadavg（首/末）: $(head -1 "$OUT/loadavg.tsv") / $(tail -1 "$OUT/loadavg.tsv")"
    echo "args: arms=$ARMS payload=$PAYLOAD n=$N rounds=$ROUNDS mtu=$MTU profile=$PROFILE mode=$MODE conns=$CONNS conns_points=${CONNS_POINTS:-（缺省 1,2,3,4,5）}"
    echo "rustc: $(rustc --version)"
  } >> "$OUT/summary.txt"
  log "完成：产物在 $OUT（summary.txt / loadavg.tsv / 逐轮原始件）"
}

# ---------- 分发 ----------
[[ -n "$SUB" ]] || { sed -n '2,20p' "$0"; exit 2; }

case "$SUB" in
  cpu)      cmd_cpu ;;
  overhead) cmd_overhead ;;
  size)     cmd_size ;;
  mem)      cmd_mem ;;
  all)      cmd_cpu; cmd_overhead; cmd_size; cmd_mem ;;
  *) echo "!! 未知子命令：$SUB" >&2; sed -n '2,20p' "$0"; exit 2 ;;
esac
finalize
