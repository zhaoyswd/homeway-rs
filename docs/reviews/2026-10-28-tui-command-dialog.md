# TUI 左侧对齐、Ctrl+B 提示与居中指令弹窗

日期：2026-10-28。基线：`f48ce0b`（main，v0.3.22 发布与部署记录已收口）。

## 范围与设计

- 会话空态说明、左下 footer 的首字符与「会话」标题首字符同列；保留现有列表与终端几何。
- 所有主提示统一 `Ctrl+B`，保留既有可收到的 `SUPER+B` 兼容入口与 Ctrl+B 双按透传；不修改宿主终端设置，不承诺 Command 键能由宿主上报。
- 快捷指令采用与添加主机相同的居中局部窗口风格，最大宽 64 列、最大高 16 行，宽高分别在 viewport 两侧留 2 列/上下留 1 行后取上限；40×10 时为 36×8。小窗口宽高使用饱和运算，不改最小常规尺寸。
- 维持原遮罩事件隔离、执行/未知键/取消后关闭、详情滚动、隐藏终端光标；绘制 overlay 不改变终端 geometry。
- 仅更改 TUI 显示、既有布局/交互测试与私有 smoke 断言；不改 core/RPC/wire、判据、行为时间常量或生产实例。

## 技术评审（实现前）

先读取仓库指定的 `/Users/zhaozhe/.agents/skills/reviewer/SKILL.md`，实际调用 `dsh --profile headless` 技术门 exit1：沙箱拒绝写 `/Users/zhaozhe/.dsh/profiles/headless/cordis.yml`（EPERM），未重试、未升级、未更改 profile。原始 prompt/output/stderr 保存在 `/tmp/dsh-review/r1.uzAiaG/`。按 ROADMAP fallback 由非实现者 `independent_review` 静态独立实读设计、源码与原回归，未跑测试/服务。原意见完整记录如下：

1. `view.rs:437/697/804`：对齐后 40×10 的 footer 宽度约 11 列，应保留可完整显示的 `Ctrl+B 指令`；既有布局测试应比较标题与文本首字实际列。
2. `view.rs:869/1088`：命令弹窗可复用添加主机的视觉样式；内容宽高使用饱和减法和有界 `inner`，检查低于 40×10 及极小窗口。
3. `view.rs:2922`、`tui-smoke.py:529`：更新全屏断言，验证居中边框与背景保留。关闭检测应搜索唯一标题 `快捷指令 · Ctrl+B`，避免误匹配 footer。
4. `view.rs:920`：局部弹窗减少详情区域，既有历史回归应验证详情仍可见，并覆盖上下滚动。
5. 输入隔离、执行关闭、键释放配对均独立于显示几何，保持现有事件路径即可；无需新增状态、`Arc` 或 `Mutex`，未见 Go 直译痕迹风险。

技术门结论：可开工，无高/中阻断。实现者接受全部建议，使用物理 cell 坐标核对空态/footer 与标题、保留 11 列短提示；更新既有 modal 与小窗口回归，复用历史回归覆盖详情滚动；不修改事件路由。

检查点：对齐和短 footer 可读性；正常窗口保留背景、小窗口有界；CJK 宽度；详情可读/滚动；modal 输入/鼠标/粘贴隔离；光标与终端 geometry；地道 Rust 与逐行移植痕迹；不触达默认 state、通知、剪贴板或生产。

## 实现与验证

源码仅调整显示常量、左边距与命令弹窗 Rect；沿用原 `inner`、渲染/事件隔离和历史段落滚动。同步 help/README 当前使用说明，旧历史记录不改。复用既有布局、modal、历史、小窗口测试并修正私有 smoke 的全屏假设；预览加入正常浅色弹窗与浅色空态。

实现子任务实际执行：

- `rustfmt --edition 2021 crates/homeway-cli/src/tui/view.rs crates/homeway-cli/src/tui/mod.rs`、`git diff --check`、Python AST parse：均 exit0。
- `cargo test -p homeway-cli tui:: --offline`：exit101，**105 passed / 9 failed / 1 ignored**。9 项均在未改 backend/transport 测试的私有 Unix socket bind 处返回 EPERM，原日志 `/tmp/homeway-tui-command-dialog-tests.log`。不把该轮记作全 TUI 通过，未升级/重复尝试，已交主任务实际复验。
- `cargo test -p homeway-cli tui::view:: --offline`：exit0，**30 passed / 0 failed / 1 ignored**，日志 `/tmp/homeway-tui-command-dialog-view-tests.log`。覆盖实际标题列对齐、窄 footer 完整提示、正常/40×10 弹窗完整快捷项/详情、背景逐 cell 保留、光标/geometry/模态输入、历史↑↓可见变化、低于最小常规尺寸和 1×1 等极小窗口不 panic。
- `cargo test -p homeway-cli sidebar_layout_preview_artifacts --offline -- --ignored`：exit0，**1 passed**；`python3 tools/tui-sidebar-previews.py` exit0，生成10张 PNG。实现者查看 `commands-dark`、`commands-small-light`、`empty-light`：正常局部居中、40×10 四周保留且四行指令和一行详情可见、空态/footer 与标题同列。目录 `/tmp/homeway-tui-sidebar-previews/`。

本子任务未运行真实私有 PTY smoke、未构建安装二进制、未运行实际桌面通知/复制，不声称这些路径已验证；最终 build/clippy、全 TUI 与私有 PTY 冒烟由主任务实际执行后补证。无 push/tag/部署/生产访问。

## 代码评审（实现后）

非实现者 `independent_review` 复读最终 diff、事件路径、既有测试更新、README/help 和 40×10 预览 PNG。代码门通过，未发现高、中或低严重度问题，无待处置项。逐项结论：

- 空态/footer 与标题同列；窄 footer 可完整显示 `Ctrl+B 指令`。
- 弹窗有界居中；40×10 保留四行快捷项、取消、滚动提示和一行详情；正常尺寸保留周围内容。
- 外侧可见会话区域的鼠标/粘贴/新按键继续经过原 modal 路径；执行/未知键后关闭、详情滚动、Ctrl/SUPER 双按与 release 配对、终端 geometry 和光标语义保留。
- 改动仅局部 Rect/常量；无额外 `Arc`/`Mutex`、字符串错误、接口仿写、无谓拷贝、包结构映射，未发现 Go 直译痕迹。

评审者明确本门为静态审查，未自行运行测试或服务；已核对全 TUI 原日志的 105 通过/9 IPC 权限失败/1 ignored，没有把该日志记作全绿。真实私有 PTY 结果以主任务后续实际运行记录为准。

稳定实现文件 SHA256（不含主任务另改的 ROADMAP 与本记录）：

| 文件 | SHA256 |
|---|---|
| `crates/homeway-cli/src/tui/view.rs` | `883e0529be1cd6eee03cefe9779761927651b67a76ea6297852c650e22bec525` |
| `crates/homeway-cli/src/tui/mod.rs` | `dc68f015430fa34f67fb14c0188b739ef6091abad565d7e3da95a3fb27060db5` |
| `tools/tui-smoke.py` | `d185240239f2bd11dccf0ce8c04a94cb3089c5f75cf679c467dd4cdcb8b38bd7` |
| `README.md` | `f7e925a0361b41385f8d93329411fa4f559694d656917187cf6c3741de1a43c3` |

## 主会话实际复验与工件

实现文件稳定后，root在具备本地IPC权限的环境实际复验完整TUI：
`cargo test -p homeway-cli tui:: --offline` **exit0、114 passed / 0 failed / 1 ignored**，
日志 `/tmp/homeway-tui-command-dialog-root-tests.log`。没有覆盖此前沙箱105/9的失败日志。
root实际 `cargo build -p homeway-cli --offline` **exit0**，以及
`cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` **exit0**（7.63秒），
日志分别为 `/tmp/homeway-tui-command-dialog-build.log`、
`/tmp/homeway-tui-command-dialog-clippy.log`。

同一调试件实际运行 `tools/tui-smoke.py --bin target/debug/homeway`，私有PTY/daemon和
显式/tmp状态下 **exit0、23 PASS**，原日志
`/tmp/homeway-tui-command-dialog-smoke.log`。涵盖Ctrl+B和合成Command+B CSI-u打开
居中面板、外侧鼠标/粘贴仍隔离、命令关闭不泄漏PTY输入，原ls/缩放/主题/关闭/再接入
回归均通过，自有进程全部收尾。合成SUPER事件可达不代表用户宿主会传递Command，主提示
因此只标Ctrl+B。root检查显示层暗色正常、浅色40×10、浅色空态及真实私有命令弹窗PNG，
确认左对齐与四周背景可见。真实截图在 `/tmp/homeway-tui-command-dialog-private-previews/`。

最终本地可执行文件 `target/debug/homeway` SHA256
`2280d20e4f19113f4c7fb0abe60d31af23d8da1964b7bb14754df874ba48b610`，源码SHA与代码门
表一致。本轮仅本地实现、验证与提交，不推送/发版/安装，不访问正式state或重启两端生产；
系统PATH仍为此前已发布的官方v0.3.22。用户可直接运行`./target/debug/homeway tui`查看本轮。
