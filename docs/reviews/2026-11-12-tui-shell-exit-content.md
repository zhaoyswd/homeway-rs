# TUI shell 自然退出后清除右侧画面（2026-11-12）

## 根因与实现前技术门

非实现者 `exit_review` 子代理独立只读技术评审通过，按 ROADMAP 结构化双角色门执行。
当前非 REPLACED 的 ENDED 清 recovery 后调用 disconnect；disconnect 为网络恢复保留
surface 和 active_session。如果订阅先报告会话消失，后台列表的 recovery 防误拆门
会保留旧屏；ENDED 随后也保留旧屏，同 revision 心跳不再次处理列表，导致左右分离。

方案复用 detach，统一清除终端、输入、选择与恢复状态并将焦点交回主机。只用于原本
已经认定终结的证据：非 REPLACED ENDED、同腿探活会话缺失/非正 PID/已知 PID 变化、
attach 的 not_found，以及已成功的主动关闭。保留原会话通知上下文，不直接删列表条目；
订阅与 LIST 仍是列表权威。REPLACED、普通 EOF、超时和其它错误继续 disconnect 恢复。
后台 LIST 缺失的保护门、wire、判据、常量和生产实例均不改。

技术门要求覆盖实际 process_attach 接线、列表/ENDED 两序、世代失效与迟到帧保护、
旧 LIST 不复活、同腿/not_found 终结清屏和 REPLACED/EOF 保留恢复，并用真实私有 PTY
输入 exit 验证空态。技术评审未发现阻断，实施按上述方案完成。

## 实际验证与失败留证

- 新增实际 process_attach 接线回归修前单跑 **exit101、0 passed / 1 failed**，在
  surface/active_session 应清空断言失败。日志 `/tmp/homeway-tui-shell-exit-before.log`，
  退出码同前缀 `.exit`，是修前确定性复现。
- `cargo test -p homeway-cli tui:: --offline` **exit0、155 passed / 0 failed /
  1 ignored**，日志 `/tmp/homeway-tui-shell-exit-tests.log`。新增回归覆盖退出码0/7、
  KILLED/SERVICE_STOPPED 与 LIST 前后两序；原会话通知 target 保留，世代提升、选择/
  标题/输入/恢复/画面清除；旧 LIST/迟到 SNAPSHOT/后续恢复轮询不复活，实际 TestBackend
  宽格渲染为空态且无旧文字；旧 generation ENDED 不清新终端，not_found 清屏而普通
  断线与 REPLACED 保留恢复。既有同腿缺失/PID 变化回归补验清屏，旧 LIST 防误拆继续绿。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` **exit0**，
  日志 `/tmp/homeway-tui-shell-exit-clippy.log`。
- `cargo build -p homeway-cli --offline` 最终 **exit0**，日志
  `/tmp/homeway-tui-shell-exit-build-final.log`。git diff --check 与 Python AST 通过。
  rustfmt 曾格式化既有未整形的顶层 run，已恢复其全部无关格式，仅保留本批改动。

所有上列命令均实取完成状态及退出码，不以启动或日志存在作通过证据。未跑完整
workspace、ci-local、跨平台运行或远端链路；本批只改 TUI 明确终结状态，无承载变更。

首轮真实烟囱 `/tmp/homeway-tui-shell-exit-smoke-before.log` exit1，当时工具按每一物理
格拼接中文，错误加入宽字符尾格空白，无法正确检测空态；**不作为充分修前实证**。
改工具按 wcwidth 跳过尾格，不改产品或放宽空态断言。之后名为
`smoke-before-verified.log` 的4 PASS实际使用了 cargo test 集成编译所更新的产品件
df9c29…，**是修后验证，文件名不代表修前**。以下显式最终构建再验为交付依据。

## 私有真实 PTY

新工具 `tools/tui-shell-exit-smoke.py` 复用既有 Tui/pyte helper，使用显式 `/tmp` state、
serve/relay 关闭的自有 daemon；ExitStack 回收自有 TUI 与 daemon，不访问默认 state。

```sh
python3 tools/tui-shell-exit-smoke.py --snapshot-dir /tmp/homeway-tui-shell-exit-previews
```

最终实际 **exit0、4 PASS**，日志 `/tmp/homeway-tui-shell-exit-smoke-final.log`，同件
SHA256 前后均 `a9ffc73fcadea0c1a31c92c7d585c95e6a67480ed875b77911a488a0139939da`。
从真实 TUI 新建 shell、显示唯一旧屏标记后输入 exit，确认服务列表已空，当前物理右屏
显示创建引导、无旧标记且 X 隐藏；立即再新建并以 exit 7 重验，两轮均通过。最后 TUI
正常退出并恢复 TTY，daemon 仍活且无工作会话，随后回收自有资源。3秒是本地工具
观测预算，不是远端端到端保证。

已实看 `/tmp/homeway-tui-shell-exit-previews/shell-exit-0-empty.png`：左侧暂无会话、
主机高亮并显示会话已结束，右侧中央创建 shell 引导，无旧终端或关闭按钮。exit7 同目录
亦保存文本/PNG。未操作剪贴板、生产会话或现役二进制，未安装、推送、发布或部署。

## 代码门

同一非实现者 `exit_review` 独立只读初审通过，无阻断：统一清理只在既有终结判定中
调用，普通网络恢复与订阅/LIST 防误拆保持；世代过滤和旧腿丢弃正确；无新增生产共享
状态、锁、字符串错误或逐行移植。transport 的 scripted 工厂仅 cfg(test)，使实际
process_attach 从有界队列消费测试事件，不注入生产接口。初审已核专项155/0/1ignored、
原始exit101与烟囱4 PASS实际日志。最终 build/烟囱实际退出码、完整SHA、最终diff与
git diff --check，及 shell-exit-0-empty.png 均由该独立评审者补核，**两门最终通过，
无阻断遗留**；无需追加产品或测试改动。主会话更新 ROADMAP 后做中文单元本地提交。
