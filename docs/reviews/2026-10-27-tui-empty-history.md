# TUI 空会话历史误提示修复（2026-10-27）

用户反馈刚进入 TUI、尚无会话时，底栏出现“正在浏览历史”。本轮仅修 TUI 前端，不改
core、wire、服务端或 state；不复用上一轮 launchd 部署任务。

## 技术门

根因在 `App::action(Action::TerminalMouse)`：`surface.mouse_position(...) == None` 同时
涵盖无 surface、未 ready、浏览历史和越界位置，原通用 else 却将所有情况写成持久的
“正在浏览历史；Shift+End返回实时画面”。因此空右侧的 MouseMoved/点击就能触发误提示，
与用户是否滚过历史无关。已有 footer 本身已从真实 `scrollbar.offset - top` 派生 ↑N/回底
提示，无需另存一条可能残留的 notice。

**技术门通过**（backend 独立评审）：删除该通用错误赋值；Scroll 只作用于已选中且名称
匹配当前 active_session 的 ready surface；reset_sessions/reset_terminal_input 清除
mouse_reporting/history_behind 派生字段。断线后刻意保留的 ready 缓存仍可本地回看，
不增加 connected 门，避免把这个提示修复扩大成禁止离线缓存浏览。

## 代码门与验证

**代码门通过**（UI 独立只读复核，主实现者交叉审安装器）：仅 mod.rs/view.rs，未引入
新错误类型、core/wire变化或逐行移植式抽象。历史事实继续来自权威 surface；空态输入
只被忽略，不覆盖已有状态/错误通知；回底或清空会话后派生提示消失。

新增回归通过实际 `view::handle_event → App::action → ratatui TestBackend` 链验证：

- 空终端 MouseMoved、点击、滚轮及 Shift+PageUp/PageDown/Home/End 保留原 notice，无
  ↑N/回底提示，不创建 surface 或 active 会话。
- 真实80行历史可到最早页，历史中鼠标不写持久 notice；End 后提示消失；会话列表清空
  触发 detach 后再次消失。该例也保留断线 ready 缓存的既有本地回看行为。
- 缺选中项或 surface 未 ready 时历史快捷键不改变 viewport；扩展原 footer 渲染测试，
  确认 reset 在下一次 render 之前就已清除历史/鼠标派生状态。

执行记录如实分列：子任务最初提交的合并 `rustfmt + cargo test` 命令因整体提权审批等待，
1401.4秒后中止，未获得 session/输出，**测试没有启动**，不能写成测试超时或失败。
随后主会话改用单独命令实际执行：

- `cargo test -p homeway-cli tui:: --offline`：**69通过、0失败，exit0**。
- `cargo build -p homeway-cli --offline`：**exit0**；仅生成新的 debug 工件。
- 新调试件 SHA256：`b3c104f5f045a58bdb202b452ad37e59c24fcc65f89bc217e8a6ff18fe1a8d4f`。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings`：主会话实际执行
  **exit0，3.57秒**。不重复跑无关全仓矩阵/跨平台门。

本轮 TestBackend 是交互路由与渲染证据，不宣称新增正式 state/生产终端交互实测。

## 仅换前端文件的可逆方案

用户在本机 debug 调试的上下文中继续反馈，主会话负责决定并执行前端更新；子任务不
自行安装或重启。固定源 `target/debug/homeway`，固定目标 `/opt/homebrew/bin/homeway`。
短安装器 `/tmp/homeway-tui-empty-history-install.py`，SHA256：
`48c30dffca61fc497d9d40505eb9744b3b23c9cd4d2ccc88bf5c8d65d4b87564`。

安装器经 backend 技术门、UI代码门及主实现者独立回读，AST通过；硬校验现役旧 e19完整
SHA和调用方指定的新SHA，目标级flock串行，0700唯一回退目录、旧件0700/回执0600。
同目录O_EXCL暂存，copy/fsync、codesign strict校验、owner501:80/mode755、原子rename及
父目录fsync，屏蔽rename与提交标记之间的信号窗口。异常仅原子退文件，不重启进程。
只用ps核对正式零参进程，成功要求前后PID一致；脚本没有homeway/daemon/launchctl/kill
调用，也不访问state。不需要独立launchd任务或重新备份state。

根代理的精确更新命令为：

```sh
/opt/homebrew/opt/python@3.13/bin/python3.13 /tmp/homeway-tui-empty-history-install.py --expected-sha b3c104f5f045a58bdb202b452ad37e59c24fcc65f89bc217e8a6ff18fe1a8d4f
```

更新只改变后续启动的PATH前端文件，当前后台和已运行的TUI仍保持原映像；用户应重新
进入TUI读取修复。

**主会话已唯一执行上述前端更新，exit0，receipt=SUCCESS。** 子任务随后只读复核
`~/homeway-rollback/tui-empty-history-20261027-3b5cd87d9e7d/receipt.json` 及正式路径SHA，
新件与完整 b3c104f5…1a8d4f 一致；`pid_before=pid_after=17589`、`frontend_only=true`、
`service_restart_requested=false`、`rollback_performed=false`。旧e19保存在该目录的
`old`，目录0700、旧件0700、receipt0600。

本次仅原子替换PATH文件；后台没有重启或杀会话，安装器不访问state或迁移/恢复配置身份，不做额外生产
attach/resize/TUI交互验收。交互证据为上述69项私有状态/渲染测试；用户重新进入TUI后
使用新前端。未发布、推送或操作远端。
