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

say "[3/4] 本地矩阵（Rust 出口）就绪检查"
if [ "${1:-}" = "--hands-on" ]; then
    say "起本地 Rust 出口（tools/local-rust-exit.sh start 1：release 构建 + 就绪判定）"
    "$REPO_ROOT/tools/local-rust-exit.sh" start 1 || fail "local-rust-exit 启动失败"
    "$REPO_ROOT/tools/local-rust-exit.sh" status 1 || true
    cat <<'RUNBOOK'

==== 交互验收（真终端逐项执行，清单 = docs/reviews/TUI-acceptance.md）====
  本地出口 state = /tmp/homeway-rs-rustexit-1（实例 1；QUIC 端口 42652）

  【会话准备】本地面 --state 指 serve 子目录（term_cli 语义，r1 D-14）：
    target/release/homeway-cli term new demo --state /tmp/homeway-rs-rustexit-1/serve
    target/release/homeway-cli term new work --state /tmp/homeway-rs-rustexit-1/serve
    target/release/homeway-cli term list --state /tmp/homeway-rs-rustexit-1/serve

  【面板】另开一个真终端：
    target/release/homeway-cli tui --state /tmp/homeway-rs-rustexit-1
    □ 1.2 主机列表：本机条目（local，可用；远端主机需按 2.1 host add）
    □ 1.3 会话列表：demo/work + agent 徽章；↑↓/点击选中
    □ 1.4 Enter 进终端焦点：vim/htop 正常渲染；1.5 Alt 组合/F1/Ctrl+C 透传
    □ 1.6 F12 返回面板；1.7 状态行（形态/延迟含采样年龄/流量）；1.8 粘贴
    □ 4.8 滚轮回看：会话未启用鼠标上报时滚轮上翻（状态行出现「回看中(N)」）

  【2.1 远端链路（可选，测断线自愈需远端隧道）】：
    tools/local-rust-exit.sh start 2
    TOKEN2=$(tools/local-rust-exit.sh token 2)
    target/release/homeway-cli host add "$TOKEN2" --state /tmp/homeway-rs-rustexit-1
    target/release/homeway-cli term new remote-demo --host <主机名> --state /tmp/homeway-rs-rustexit-1
    面板切到远端主机接入后：tools/local-rust-exit.sh stop 2（→断线）
    → tools/local-rust-exit.sh start 2（→自动重附，屏幕经回放恢复）

  【2.2/2.3 断线三态】会话内 exit 或 term delete（→已结束+回落）；
    8 个 term attach 占满腿再让面板接入（→腿满提示不抢占）
  【5.3 双实例】再开一个同 --state 的 tui：A 切换选中 B 1s 内镜像；A 退出 B 不受影响
  【5.4 共存】面板观察中：term attach demo --state /tmp/homeway-rs-rustexit-1/serve
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
