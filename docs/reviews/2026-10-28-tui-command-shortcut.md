# TUI Command+B 快捷入口与标注（2026-10-28）

按用户字面增加实际 Command+B 入口，macOS 主提示为 `⌘+B`，其它系统为 `Ctrl+B`，
Ctrl+B 继续兼容。页脚、全屏指令标题、小窗口提示与 help 同步标注；窄栏保留完整键名。
新入口仅接受 `b + SUPER`，不吞 Alt/Super 杂组合，原 Control 分支保持兼容。
弹窗内再次按快捷键仍投递原 KeyEvent，不把 SUPER 伪装成 CONTROL；原释放配对、
输入隔离和终端 Geometry 不变。明确保证的连按透传是 Ctrl+B；Command 需要承载终端
上报，向会话应用透传还取决于应用的键盘协议，未改终端偏好或新增全局键监听。

用户同时询问 blocking：现有 TUI 每2秒刷新当前选中主机的 LIST/stateV2，仅更新黄色点
和 `blocking`，没有状态转场声音或系统提醒；附着 STATE 只更新标题，NOTIFY 未处理。
本轮仅核实并答复现状，不实现新提醒、不监控所有主机，也不修改 core/wire/通知机制。

## 独立两道门

主会话确认 Command 与 Control 的差别及兼容边界；非作者 shortcut_review 独立实读最终
源码、测试、烟囱和安装适配器，技术门/代码门均通过，无剩余阻塞。覆盖 exact SUPER、
旧 Ctrl、原键与释放配对、最小窗口、Linux 窄栏、真实 CSI-u 入口及文案承诺边界；
没有新增锁/Arc 或逐行移植式结构。审查指出的全屏主标缺口已修为带实际快捷键的标题。

## 验证与本机换件

实现者实际完成 rustfmt、diffcheck 和 Python AST 检查，均 exit0；未运行 cargo 或部署。
沿用现有回归，新增一次原 KeyEvent/释放配对回归，并将 popup 回归扩到 SUPER/Ctrl。
既有私有 smoke 增加 `CSI 98;9u` 的 Command+B 开关，保留 Ctrl+B 兼容和后续真实输入断言。
主会话实际全TUI **105 passed、0 failed、1 ignored，exit0**，日志
`/tmp/homeway-tui-command-shortcut-tests.log`；build exit0（0.08s），
clippy `--all-targets -- -D warnings` exit0（1.92s）。本轮没有失败，不借用上一轮失败链。
既有 ignored 预览入口实际 exit0、1 passed，renderer exit0、生成8张PNG；主会话查看
one-dark 与 commands-small-light，确认新标注及窄屏完整。私有本机 smoke 实际 exit0、
**23 PASS**，日志 `/tmp/homeway-tui-command-shortcut-smoke.log`，包含 Ctrl+B 和
Command+B CSI-u 打开/关闭且不泄露PTY输入，原会话交互流程均保留；模拟增强事件不代表
所有物理终端都已配置 Command+B。最终工件运行前后SHA相同：
`3e0e1cbc590d7cebb6526ff07c3dc7d00df9394467649294cff2f75b7fd7f071`。

最小本机 file-only 适配器 `/tmp/tui-command-shortcut-install.py`，SHA256
`325301e4d966ece3e9621a66857ae026c670eee713b20dbbc1da376d70b8cb81`。
非作者实核相对已审 `tui-compact-status-install.py` 仅旧 SHA 改为
`e1c9dac9bf0ea6b7f110e9ec870f6cf15ced73e092cd66da50aec78224e6ed5c` 与备份前缀
`tui-command-shortcut-20261028-` 两处；锁、备份、签名/fsync、原子换退及 PID 不变要求沿用。
主会话实际执行安装器 exit0、回执 **SUCCESS**，PATH与上述最终工件同SHA；PID
**55632→55632**，未重启、未回滚、未访问正式state。旧件和回执保留于
`/Users/zhaozhe/homeway-rollback/tui-command-shortcut-20261028-39baef936405/`。
独立核验target/debug与PATH完整SHA一致、严格codesign exit0，ps为PPID1/UID501/
零参 `/opt/homebrew/bin/homeway`。重新进入TUI生效；blocking未新增主动提醒，
LAN71本轮未尝试或部署。
