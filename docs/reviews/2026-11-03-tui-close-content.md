# TUI 关闭会话后清除右侧旧画面（2026-11-03）

## 根因与技术评审

用户反馈点击右上角 X 关闭会话，左侧回到主机，右侧仍显示原会话。基线为
`5bd91454ea10516d14a61446d3f7ed5c1d5872c6`。当前恢复机制会在后台 LIST 中缺失正在
展示的会话时保留连接与缓存画面，避免早于 attach 的旧 LIST 误拆健康连接；但主动
关闭成功的 `SessionClosed` 也只发通知、刷新列表，没有清理当前展示与恢复状态。
因此列表先到而 ENDED 未到或连接处于恢复时，左右两侧状态会分离。

实现前由非实现者 `close_review` 子代理独立只读技术评审，通过。按 ROADMAP fallback
使用结构化双角色评审；reviewer skill 自身仅在用户明确请求 review 时触发，本次是
修 bug，未调用外部 dsh。方案只在精确匹配 close_pending 的成功结果中，额外核对当前
主机和 active_session 名字，再复用 detach 清除画面、输入、attach 与 recovery，清空
会话选择并把焦点交回主机。其它列表条目保留，不用 reset_sessions；被动 LIST 的
防误拆门保留。关闭失败、关闭未展示的条目、用户已切换其它主机或会话均不清当前终端。

产品只改 `crates/homeway-cli/src/tui/mod.rs`，无 core、wire、行为常量、协议判据或
夹具变化。detach 已提升 generation 并丢弃 attach，旧腿消息不会进入新的展示；无新
共享状态、锁或复制。沿用原有按 target/name 匹配关闭结果的契约；未扩大为 PID 感知
的 KILL 协议，不宣称解决关闭过程中同名会话重建的独立身份问题。

## 验证与失败记录

所有日志和实际退出码均分别保存在 `/tmp/homeway-tui-close-content-*.log` 与对应
`.exit` 文件中，未将启动成功当成验证通过。

- 修前新增回归单跑 **0 passed / 1 failed，exit101**，断言旧 surface/active 应已清除
  失败，实证复现根因。日志 `before.log`。
- 首轮完整 TUI **145 passed / 1 failed / 1 ignored，exit101**，产品清理断言已通过，
  失败在测试渲染文本提取：未跳过中文宽字符的尾格，不能找到连续中文。修正测试为按
  cell_width 前进，未改产品或降低空态断言。日志 `tests.log`。
- 修正后专项 **146 passed / 0 failed / 1 ignored，exit0**，日志 `tests-final.log`。
  随后在同一回归补迟到 SNAPSHOT 不复活断言，最终专项再次 **146/0、1 ignored，
  exit0**，日志 `tests-verified.log`。命令为
  `cargo test -p homeway-cli tui:: --offline`。
- `cargo build -p homeway-cli --offline` **exit0**，日志 `build.log`。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` **exit0**，日志
  `clippy.log`，包含最终迟到 SNAPSHOT 测试增量。
- `rustfmt --edition 2021 --config skip_children=true` 与 `git diff --check` 通过。
  首次误用 edition2024 且递归子模块引入的格式噪声已全部撤回；最终只有目标文件必要
  改动，未保留无关格式变化。

两项新增回归使用带真实历史画面的 history_app 与 Recovery，覆盖先 LIST 消失后关闭
确认、先确认后 LIST 两序；关闭后同 epoch 旧 LIST 及迟到 SNAPSHOT 不复活终端；确认
清除 recovery/attach、提升 generation、隐藏 X、清空标题、焦点回主机，实际 TestBackend
渲染显示中央创建引导且没有旧文字；关闭失败、其它名字或跨主机的完成保留当前画面与
恢复状态。既有关闭单飞、导航 epoch 和旧 LIST 保护回归继续通过。

## 私有真实交互

不改既有工具，执行：

```sh
python3 tools/tui-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-close-content-previews
```

实际 **exit0、23 PASS**，日志 `smoke.log`。工具只使用 `/tmp` 私有 state，serve/relay
均关闭，运行真实 PTY 并以实际坐标发送鼠标 X press/release。确认只关闭当前会话，
原会话仍在；当前屏中央回到创建引导、X 隐藏、旧热区不会关闭剩余会话；原会话重新
接入并执行新输入，原菜单关闭路径、终端恢复及自有进程回收均通过。

同件 debug SHA256：
`94006ff21162eb3b31d5a3415fd00036a852d32fd25ed9f9099cd67905c24829`。
构建后仅修改测试断言，产品源码未再变化。已实看
`/tmp/homeway-tui-close-content-previews/terminal-close-empty-light.png`：原会话保留在左侧，
主机高亮、右侧无旧终端内容而显示创建引导、标题栏无 X。

## 代码评审与边界

非实现者 `close_review` 独立只读代码门初审通过：成功与当前展示上下文守卫正确，
清理复用现有 detach，失败与导航分支保持，未发现资源/竞态阻断或 Rust 直译痕迹。
最终日志、截图与迟到 SNAPSHOT 增量由同一评审者复核终签：**技术与代码两门通过，
无阻断遗留**。评审者逐段核最终 diff 与 surface_frame 无 surface 早退路径，独立核
四个实际 `.exit` 均为 0、TUI 146/0/1 ignored 及烟囱 23 PASS，并实看关闭后空态图。

未运行完整 workspace/ci-local/RRR；本轮仅涉及前端关闭状态，专项与真实私有交互验证
覆盖此改动。未访问默认 state 或生产实例，未安装、推送、发版或部署。主会话负责
ROADMAP 更新与中文单元本地提交；现役二进制需后续发布换装才包含此修复。
