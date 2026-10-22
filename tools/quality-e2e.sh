#!/bin/zsh
# 私有出口全链观测验收；默认实例94，退出自动停，不碰正式实例。
set -euo pipefail
ROOT="${0:h:A:h}"
INSTANCE="${1:-94}"
if [[ "$INSTANCE" != <-> ]] || (( INSTANCE < 1 || INSTANCE > 1000 )); then
  echo '实例号需要1–1000的整数' >&2
  exit 2
fi
cd "$ROOT"
cargo build -p homeway-cli
export HOMEWAY_BIN="$ROOT/target/debug/homeway"
"$ROOT/tools/local-rust-exit.sh" stop "$INSTANCE" >/dev/null 2>&1 || true
trap '"$ROOT/tools/local-rust-exit.sh" stop "$INSTANCE" >/dev/null 2>&1 || true' EXIT
"$ROOT/tools/local-rust-exit.sh" start "$INSTANCE" >/dev/null
export HOMEWAY_QUALITY_E2E_STATE="/tmp/homeway-rs-rustexit-$INSTANCE"
export HOMEWAY_QUALITY_E2E_TOKEN="$("$ROOT/tools/local-rust-exit.sh" token "$INSTANCE" 2>/dev/null | rg -o 'hmw2[A-Za-z0-9_=+/-]+' | head -1)"
[[ -n "$HOMEWAY_QUALITY_E2E_TOKEN" ]]
cargo test -p homeway-core --test quality_history_e2e -- --ignored --nocapture --test-threads=1
"$HOMEWAY_BIN" serve quality --state "$HOMEWAY_QUALITY_E2E_STATE" --since 2m
"$HOMEWAY_BIN" serve quality --state "$HOMEWAY_QUALITY_E2E_STATE" --since 2m --json > "$HOMEWAY_QUALITY_E2E_STATE/quality-report.json"
echo "报告：$HOMEWAY_QUALITY_E2E_STATE/quality-report.json"
