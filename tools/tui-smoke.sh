#!/usr/bin/env bash
# TUI 面板冒烟 + 验收驱动（openspec add-tui-panel 任务 5.6）。
#
# 用法：
#   tools/tui-smoke.sh            # 无 TTY 自动化段（边界行为 + 构建/单测）+ 手测指引
#   tools/tui-smoke.sh --hands-on # 追加：起本地 Rust 出口（实例 1）并给出交互验收步骤
#                                 # （面板本体需在真终端人工启动——脚本不接管 TTY）
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

say "[2/4] 边界行为（非 TTY / --help）"
OUT_HELP="$(./target/debug/homeway-cli tui --help)" || fail "--help 失败"
echo "$OUT_HELP" | grep -q "四区面板" || fail "--help 文案缺失"
set +e
OUT_TTY="$(./target/debug/homeway-cli tui < /dev/null 2>&1)"
RC_TTY=$?
set -e
[ "$RC_TTY" = "2" ] && echo "$OUT_TTY" | grep -q "TTY" || fail "非 TTY 应 exit 2 并提示 TTY"

say "[3/4] 验收拓扑（统一进程：control + serve 一体——TUI 的主机/状态面走 control.sock）"
TUI_STATE=/tmp/homeway-tui-demo
BIN=./target/release/homeway-cli
if [ "${1:-}" = "--hands-on" ]; then
    if [ -S "$TUI_STATE/control.sock" ]; then
        say "统一进程已在跑（${TUI_STATE}）——复用"
    else
        say "起统一进程（control+client+serve 按期望态）+ 打开 serve"
        nohup "$BIN" --state "$TUI_STATE" > /tmp/homeway-tui-demo.log 2>&1 &
        sleep 3
        "$BIN" serve start --state "$TUI_STATE" | tail -1 || fail "serve start"
        sleep 1
    fi
    "$BIN" term new demo -d --state "$TUI_STATE/serve" | tail -1 || true
    "$BIN" term new work -d --state "$TUI_STATE/serve" | tail -1 || true
    [ -S "$TUI_STATE/control.sock" ] && [ -S "$TUI_STATE/serve/term.sock" ] \
        || fail "验收拓扑不就绪（control.sock / serve/term.sock）"
    cat <<'RUNBOOK'

==== 拓扑就绪。启动面板（真终端执行这一条）====
    target/release/homeway-cli tui --state /tmp/homeway-tui-demo

  面板内：↑↓/点击选中；Enter 进终端焦点（跑 vim/htop）；F12 返回；q 退出；
  滚轮回看（状态行「回看中(N)」）；粘贴多行；状态行 = 形态/延迟(采样年龄)/流量

  【2.1 远端链路（可选）】再起一个出口实例当远端主机：
    tools/local-rust-exit.sh start 2
    TOKEN2=$(tools/local-rust-exit.sh token 2)
    target/release/homeway-cli host add "$TOKEN2" --state /tmp/homeway-tui-demo
    target/release/homeway-cli term new remote-demo --host <主机名> --state /tmp/homeway-tui-demo
    断线测试：tools/local-rust-exit.sh stop 2 → start 2（面板应自动重附）
  【2.2/2.3】会话内 exit / term delete（→已结束+回落）；8 个 term attach 占满（→腿满提示）
  【5.3 双实例】再开一个同 --state 的 tui：A 切换 B 1s 内镜像
  【5.4 共存】面板观察中：term attach demo --state /tmp/homeway-tui-demo/serve
RUNBOOK
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
