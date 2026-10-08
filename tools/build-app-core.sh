#!/usr/bin/env bash
# build-app-core.sh — 构建 Rust 版 libclientcore.so（aarch64-unknown-linux-ohos cdylib）
# 并做三道门：
#   1. 符号对齐：20 个 ClientCore* 导出面逐一在产物符号表里（真源 = tier 的
#      tailcat_napi.cpp extern 块——消费侧契约；同名符号原位替换，缺一即红）；
#   2. 版本注入：HOMEWAY_CORE_VERSION（核源 SHA）+ HOMEWAY_RUSTC_VERSION——
#      产物 rodata 里必须能查到注入串（对齐 Go 版 -X main.buildVersion 校验门）；
#   3. 体积记录：对比 Go 版（9.2MB 量级；PoC 估 1.3-1.5MB）。
# 产物：target/aarch64-unknown-linux-ohos/release/libclientcore.so（strip 后）
# 供 tier build-core.sh 的 CORE_IMPL=rust 档消费（双落盘由 tier 侧完成）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="aarch64-unknown-linux-ohos"
NDK="${OHOS_NDK:-/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native}"
STRIP="${NDK}/llvm/bin/llvm-strip"
NM="${NDK}/llvm/bin/llvm-nm"

# 20 个导出符号（真源 = tier:tailcat/src/main/cpp/tailcat_napi.cpp 的 extern "C" 块；
# Go 版 //export 同名——同名符号原位替换的契约面）
SYMBOLS=(
  ClientCoreVersion
  ClientCoreTunPrepare
  ClientCoreTunAttach
  ClientCoreTunStatus
  ClientCoreTunStop
  ClientCoreTunRecover
  ClientCoreTunSetPortForwards
  ClientCoreTunRunning
  ClientCoreTunSetForeground
  ClientCoreTunSetActivity
  ClientCoreProbeAddr
  ClientCoreProbeReach
  ClientCoreServiceStart
  ClientCoreServiceStop
  ClientCoreServiceStatus
  ClientCoreFilesCall
  ClientCoreTermCall
  ClientCoreSpeedTestStart
  ClientCoreSpeedTestStatus
  ClientCoreSpeedTestCancel
)

if [ ! -x "${NM}" ]; then
  echo "error: 找不到 OHOS NDK llvm 工具（${NM}——设 OHOS_NDK 或安装 DevEco Studio）" >&2
  exit 1
fi

# ---- C 侧前置（M0 必修，设计 §2.5/§8.2 R-A）：ring 0.17 的 build script 会编 C/汇编，
# 而 cc-rs **不读** .cargo/config.toml 的 linker ⇒ 必须显式给 CC，否则用系统 cc，
# ring 的 C 代码找不到 assert.h 当场断（实测）。**真构建走真 sysroot：不带 -nostdlibinc**
# （check-only 垫片只属于 ci.yml / ci-local 的 check 档，见 tools/cc-check-shim/stdlib.h）。----
export CC_aarch64_unknown_linux_ohos="${NDK}/llvm/bin/aarch64-unknown-linux-ohos-clang"
# fail-closed：check-only 档的 flags **绝不允许**进真构建（M0 设计 §2.3/§8.2 R-D 的口子）
if [ -n "${CFLAGS_aarch64_unknown_linux_ohos:-}" ] && [[ "${CFLAGS_aarch64_unknown_linux_ohos}" == *nostdlibinc* ]]; then
  echo "error: CFLAGS_aarch64_unknown_linux_ohos 含 -nostdlibinc（check-only 垫片档）——真构建必须用真 sysroot；请 unset 后重跑" >&2
  exit 1
fi
if [ ! -x "${CC_aarch64_unknown_linux_ohos}" ]; then
  echo "error: 找不到 NDK 包装 clang（${CC_aarch64_unknown_linux_ohos}）——真构建必须有真 sysroot" >&2
  exit 1
fi
echo "[cc] CC_aarch64_unknown_linux_ohos=${CC_aarch64_unknown_linux_ohos}（真 sysroot；不带 -nostdlibinc）"

# 版本注入串：核源 SHA（+dirty 标记）——与 tier build-core.sh 的注入口径同形。
# dirty 判定路径集与 tier 侧闸门一致（复核 r3-F9：crates/tools/fixtures/Cargo.*/
# .cargo/rust-toolchain.toml——两侧不一致会让 dev 模式的 +dirty 标记错位）
SHA="$(git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)"
DIRTY=""
if [ -n "$(git -C "$ROOT" status --porcelain -- crates tools fixtures Cargo.toml Cargo.lock .cargo rust-toolchain.toml 2>/dev/null)" ]; then
  DIRTY="+dirty"
fi
INJECT_VER="${SHA}${DIRTY}-rust"
RUSTC_VER="$(rustc --version | awk '{print $2}')"

echo "[ver] 注入 HOMEWAY_CORE_VERSION=${INJECT_VER} HOMEWAY_RUSTC_VERSION=${RUSTC_VER}"

cd "$ROOT"
HOMEWAY_CORE_VERSION="${INJECT_VER}" \
HOMEWAY_RUSTC_VERSION="${RUSTC_VER}" \
cargo build --release --target "$TARGET" -p homeway-capi

SO="$ROOT/target/$TARGET/release/libclientcore.so"
if [ ! -f "$SO" ]; then
  echo "error: 产物未生成：${SO}" >&2
  exit 1
fi

# strip（release cdylib 默认 strip = none——显式过一遍 NDK strip）
"${STRIP}" "$SO"

# ---- 门 1：符号对齐（缺一即红） ----
MISS=0
for sym in "${SYMBOLS[@]}"; do
  if ! "${NM}" -D --defined-only "$SO" | awk '{print $3}' | grep -qx "${sym}"; then
    echo "error: 产物缺导出符号 ${sym}" >&2
    MISS=1
  fi
done
if [ "$MISS" -ne 0 ]; then
  echo "error: 符号对齐失败（上列缺失）——不得交付" >&2
  exit 1
fi
EXPORTED="$("${NM}" -D --defined-only "$SO" | awk '{print $3}' | grep -c '^ClientCore' || true)"
echo "[sym] 20/20 导出面齐（产物 ClientCore* 动态符号共 ${EXPORTED} 个）"

# ---- 门 2：版本注入校验（产物 rodata 必含注入串） ----
# 注意不能写 `strings | grep -q`：本脚本 set -o pipefail，grep -q 命中即早退会让
# strings 吃 SIGPIPE（141）把整条管线判红（tier build-core.sh 同款坑——首跑实测误红）。
# grep -c 读完全程，无早退。
HIT="$(strings "$SO" | grep -cF "${INJECT_VER}" || true)"
if [ "$HIT" -eq 0 ] 2>/dev/null; then
  echo "error: 版本注入校验失败：产物内找不到 ${INJECT_VER}" >&2
  exit 1
fi
echo "[ver] 版本注入校验通过：${INJECT_VER}"

# ---- 门 3：体积记录 ----
SIZE="$(wc -c < "$SO" | tr -d ' ')"
echo "[size] ${SO} = ${SIZE} bytes（$(( SIZE / 1024 / 1024 )).$(( (SIZE / 1024 % 1024) * 10 / 1024 ))MB；Go 版对照 9.2MB）"

echo "built: ${SO}"
