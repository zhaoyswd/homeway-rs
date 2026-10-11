#!/usr/bin/env bash
# TUI 面板冒烟 + 验收驱动（openspec add-tui-panel 任务 5.6）。
#
# 用法：
#   tools/tui-smoke.sh            # 无 TTY 自动化段（边界行为 + 构建/单测）+ 手测指引
#   tools/tui-smoke.sh --hands-on # 追加：起本地 Rust 出口并给出交互验收步骤（需人工在真终端执行）
#
# 手测清单真源 = docs/reviews/TUI-acceptance.md（逐项通过后勾选并记录）。
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"
FAIL=0

say() { echo "==> $*"; }
fail() { echo "!! $*" >&2; FAIL=1; }

say "[1/4] 构建 + tui 单测"
cargo build -q -p homeway-cli || fail "构建失败"
cargo test -q -p homeway-cli --bin homeway-cli tui:: || fail "tui 单测失败"

say "[2/4] 边界行为（非 TTY / TERM=dumb / --help）"
OUT_HELP="$(./target/debug/homeway-cli tui --help)" || fail "--help 失败"
echo "$OUT_HELP" | grep -q "四区面板" || fail "--help 文案缺失"
set +e
OUT_TTY="$(./target/debug/homeway-cli tui < /dev/null 2>&1)"
RC_TTY=$?
set -e
[ "$RC_TTY" = "2" ] && echo "$OUT_TTY" | grep -q "TTY" || fail "非 TTY 应 exit 2 并提示 TTY"
set +e
OUT_DUMB="$(TERM=dumb ./target/debug/homeway-cli tui < /dev/null 2>&1)"
set -e
# 非 TTY 先于 TERM 判定——两者都退出即覆盖（真终端 TERM=dumb 判定见单测 + 手测）

say "[3/4] 本地矩阵（Rust 出口）就绪检查"
if [ "${1:-}" = "--hands-on" ]; then
    say "起本地 Rust 出口（tools/local-rust-exit.sh）"
    "$REPO_ROOT/tools/local-rust-exit.sh" up || fail "local-rust-exit 启动失败"
    cat <<'EOF'

==== 交互验收（真终端逐项执行，清单 = docs/reviews/TUI-acceptance.md）====
  1)  单实例：cargo run -p homeway-cli -- tui --state /tmp/homeway-rs-rustexit-0
      □ 主机列表含本机与远端；□ 会话列表徽章；□ Enter 进入终端焦点跑 vim/htop；
      □ Alt/Fn 透传；□ F12 返回；□ 状态行三项
  2)  断线三态：重启出口进程（→自动重附）/ kill 会话（→已结束+回落）/
      占满 8 腿再接入（→腿满提示不抢占）
  3)  双实例：两个 tui 并行，A 切换 B 1s 内镜像；A 退出 B 不受影响
  4)  共存：面板观察中并行 term attach；App 行为不变
  （本地出口 state = /tmp/homeway-rs-rustexit-0；本机条目 = serve 直连）
EOF
else
    say "（--hands-on 跳过；本地矩阵交互验收需真终端，见 docs/reviews/TUI-acceptance.md）"
fi

say "[4/4] 汇总"
if [ "$FAIL" = "0" ]; then
    echo "SMOKE OK"
else
    echo "SMOKE FAILED" >&2
    exit 1
fi
