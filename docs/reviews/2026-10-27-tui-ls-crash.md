# TUI 执行 ls 崩溃整改（2026-10-27）

用户在首版 `a58a977` 的debug二进制中新建本地shell、执行ls即退出；用户给出的panic为
`ratatui-core-0.1.2/src/buffer/cell_width.rs:36: control character passed to cell_width without filtering`。
旧件SHA256=`bdd4d4773146c57da7e6b9b1204e6de320fc10c02ac9115f65b7ccbeefaeb458`，
只读备份`/tmp/homeway-tui-bdd4-ls-control`。

## 技术门

UI与backend子代理独立回读源代码，结论一致：Alacritty 0.26 `put_tab`在空cell保留
`cell.c='\t'`后推进到tabstop；`SessionVt::cell_of`有意保留该符号用于文本/复制语义。
TUI把该格直接传入ratatui低层`Cell::set_symbol`，而此API要求调用方过滤控制字符。
因此真实`ls -C`多列TAB可触发debug断言；release虽无断言，原TAB还会错误移动外层
整窗光标，不能以release、吞panic或截断输出解决。

最小设计：仅在VT→ratatui的显示投影将不可打印控制符变为空格/去除组合格内控制符，
每个格仍按既有绝对x、width、style绘制。VT、PTY数据、历史/复制、帧与检测机制均保留。
正常Unicode、组合附标、ZWJ/变体选择符、宽字、颜色/attr不得被过滤。

技术评审（实施前）：UI代理确认HT所在格为一个可见空白，tabstop跨度已由VT后继格坐标
体现，禁止二次展开TAB；backend代理确认修改仅属TUI适配边界，不改core/wire。
技术门通过，按上述边界实现。

## 红→绿与验收计划

- 旧debug真实PTY：私有state显式serve/relay off，RUST_BACKTRACE=1，创建shell后执行
  HOME目录实际`ls -C`，捕获TUI/daemon退出码和LIST存活情况；留原始回溯与state路径。
- 自动回归：`SessionVt`喂`a\tb`及带色TAB，ratatui `Terminal::draw`真实差分阶段不得panic，
  后继字符固定在第9列、style不变；源VT格仍为TAB。补C0/DEL/C1显示边界及中文/组合字不变。
- 真实PTY脚本扩展实际ls：HOME、仓库、私有目录的多列/颜色/中文/长名/滚动输出，断言
  命令确实执行、内容/列位置正确、输入仍可用。旧件应红，新debug件应绿，避免marker误判。
- 实现后独立代码门、TUI专项与clippy；root最后提交。生产与默认state不用于测试。

## 代码门与最终证据

修复只改`view.rs`的`terminal_symbol`显示投影：空/纯控制格借用一个空格；打印字符原样
借用；控制符基格带组合附标时保留空白基格与附标，混合内容只去控制码。绝对x/width/
skip、色彩和attr链未变，原VT和输出字节未变，没有catch panic/截断输出/换release绕断言。

**代码门通过**：backend独立回读投影、两轮真实draw/diff与Unicode边界测试，确认无
逐行移植痕迹、无剩高/中项；UI独立复核并执行旧件/新件真实ls对照，确认列位/颜色/
宽字/长名和滚动正确。技术门与代码门均在本子任务内完成，不重新做整批大范围评审。

| 证据 | 真实结果 |
|---|---|
| 单测红→绿 | 新增`terminal_tabs_keep_grid_columns_and_style_across_real_draw_diffs`在未修复代码下**exit101**，与现场相同cell_width控制码panic（`/tmp/homeway-tui-ls-unit-red.log`）；修复后包含控制符/Unicode等定向**5/0**（`/tmp/homeway-tui-ls-unit-green.log`）。最初额外颜色断言误假设TAB继承SGR，已按Alacritty真实“只写c、保留原格style”纠正：原蓝底保留、A/B红色、C/D绿色，未放宽空间/内容判据。 |
| 独立真实旧件红 | 私有PTY160×40，HOME实际`ls -C`使旧bdd4 TUI **exit101**，完整回溯`/tmp/homeway-tui-ls-ui-backtrace.log`。100×30 pane仅66列、最长名33列恰成单列，故初测未红；不是不稳定修复。 |
| 进程归因 | `/tmp/homeway-tui-ls-repro-result.log`记录旧TUI101/panic=true，同时daemon未退出、前后同shell仍在；崩的是TUI绘制，不是统一进程或shell。 |
| 原会话原历史恢复 | 单runner `/tmp/homeway-tui-ls-replay.py`连续旧101→确认daemon/同shell活→新TUI attach同一个session（不新建）→原含TAB历史回放正常→再次真实ls与唯一marker完成→正常exit0/termios恢复，CLI LIST同session仍在，finally收尾。**exit0**，结果`/tmp/homeway-tui-ls-replay-result.log`，原始新旧输出`/tmp/homeway-tui-ls-paired-b8xwhkfq/{old,new}-tui.raw.log`。早前分离runner因工具回收测试守护而未开始重进的失败已甄别，不当产品故障或成功证据。 |
| 扩充smoke旧件红 | `/tmp/homeway-tui-ls-smoke-old.log`：脚本**exit1**，100_HOME单列通过后100_PROJECT真实ls多列导致TUI101/同断言，验证新测试确能抓住此漏测。 |
| 扩充smoke新件绿 | `python3 tools/tui-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-ls-previews` **exit0**，100×30和160×40下HOME/PROJECT/SHORT_TAB/COLOR_UNICODE_LONG/SCROLL共**10个真实ls场景全部通过**。短名目录预检真实ls必须含TAB，核对48个短名；颜色、中文/🦀/NFD组合名/137字符长名、180行滚动均核实实际文件名/位置与session+attach存活。原OSC7、resize、主题、菜单关会话、分离重进、SIGTERM还原继续通过。日志`/tmp/homeway-tui-ls-smoke-new.log`。 |
| 专项/静态门 | 全TUI **43/0，exit0**（`/tmp/homeway-tui-ls-all-tests.log`）；workspace all-targets clippy `-D warnings` **exit0**（`/tmp/homeway-tui-ls-clippy.log`）；`git diff --check`通过。仅显示适配改动，不重跑无关wire/core/matrix门。 |

**修复后的实际调试件已构建到**`target/debug/homeway`，SHA256=
`6660982d9a54b8fcf6a9ea30570de80d7c1438c346864e3068cf7787c6f9cdf8`，真实smoke前后
相同，构建日志`/tmp/homeway-tui-ls-build.log`。预览`/tmp/homeway-tui-ls-previews/ls-wide-{color,scroll}.png`
及txt，UI代理实际看图确认。私有测试进程已finally回收，不要求用户删除或重建旧会话。
未推送/发版/部署，未重启用户守护，生产state未动。
