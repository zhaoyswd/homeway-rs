# TUI 历史、布局与多端接入整改（2026-10-27）

本轮承接 `d1286ea`，用户要求历史回滚、随终端背景自动主题、去掉顶部品牌和主题入口、
底栏压为一行、多端宽网格裁剪，以及远程 shell 内运行 TUI 时避免接入自身承载会话。
实现、技术门与代码门均在子任务内完成；主会话只核对证据、维护 ROADMAP 和本地提交。
所有真实验证使用显式 `/tmp` state；正式守护与生产 state 不作测试对端。

## 技术设计与门一

### 已核实的根因与边界

- 原 TUI 没有回滚锚点，只画本地 raw VT 当前屏；鼠标滚轮总交编码器，shell 未开启
  mouse reporting 时自然无动作。原本地 VT 随 pane resize，会把其它活跃腿的宽输出重新
  按窄列解释；原 ATTACH 注册和后续 RESIZE 又都会抢占 active，因此仅改裁剪不足。
- 服务端现有 surface v5 已包含权威网格、尺寸、seq/revision、historyEpoch/base，
  FETCH_ROWS 也有相同身份边界。采用这些现有材料，不从 LIST 轮询尺寸后猜 raw 字节布局。
- LIST 已包含真实 PTY pid。原 guard 只比较 `TERM_SESSION_ID` 与会话名，提示发生在接入后，
  还会误拒另一远端的同名会话。当前 TUI 位于承载 shell 的子进程链，可以复用现有进程
  采样器将本地 LIST pid 与实际祖先链匹配；远端名字与 PID 不提供主机身份，不作推断。
- Crossterm 0.29 不提供颜色事件。其 Unix parser 将 OSC 开头解析为 Alt+]，随后为 payload
  按键及 BEL / Alt+\\；只用这一条事件流识别 OSC 11，应答不会发给附着会话，也不开第二
  个 stdin 读者。来源为 xterm [OSC 控制序列](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
  与 crossterm 0.29 本地源码。未支持 OSC 11 时用 COLORFGBG，最终回退深色。

### 实施约束

1. TUI 展示使用 surface SNAPSHOT/DIFF，历史按绝对物理行 FETCH；不重折行，宽格被 pane
   右边界切开时不把半格画到边框。缓存及请求有界，身份/几何失效后拒绝旧 FETCH，滚动
   锚点在新输出时保持，历史裁剪时 clamp；回到底部恢复跟随。
2. 普通滚轮在终端未报告鼠标时浏览历史；应用报告鼠标时保留给应用。Shift+滚轮与
   Shift+PageUp/PageDown/Home/End 明确用于本地回滚，普通终端键仍交应用。
3. 仅 TUI 采用明确协商的 surface observer：接入/被动查看/窗口变化不抢其它活跃腿；
   实际输入才沿既有 active 策略接管并同步 pane 尺寸。旧 CLI/App 默认行为保持。
   若旧服务未声明能力，TUI 清楚报需升级，不以忽略 flag 的方式冒险抢宽。
4. 移除顶部整行和手动主题入口/参数/持久化；单行底栏有界裁剪，错误仍可见。OSC 11
   探测启动、焦点回归和低频进行，绝不阻塞 UI；过滤器识别 BEL/ST、常见 RGB 深度、
   分片和迟到应答，不把 Paste 中的字面 OSC 当终端应答，不匹配按键原序返回。
5. 本地承载会话提前标记，禁止自 attach 并提示选择其它/新建；远端同名/PID不误拒。
   退出和切换仍仅 detach，不 KILL；已有 `ls` 控制格显示修复保留。

**门一通过**：backend 独立调查 registry/PTY/surface 并提出 observer 能力协商；UI 独立
审查布局/历史输入归属与主题事件流；主实现者交叉审查这两项方案，主会话核准必要的
协议能力扩展。处置包括：注册、被动 RESIZE、活动腿退出三个选举点都排除未输入观察腿；
自动 focus、hover、keyup 与终端应答不接管，真实应用滚轮/按键/粘贴接管；禁止
observer+raw / observer+显式 takeover 在创建前失败；旧服务缺能力明确失败。
旧 Rust `frames::dec_greeting` 仅检查结构，新增 unknown-feature 回归；只读 tier
`surface_codec.cpp:1104–1112` 按 required mask 判定，因此旧 CLI/App 忽略新增能力位。
原默认接入逻辑与旧 golden 用例保留。新能力位/HELLO flag 及受影响 golden/MANIFEST/
SHA/派生 fuzz 种子摘要同批登记，未改 surface 网格格式或版本。

主题方案明确限定：**只有完整、合法且终止的 OSC 11 应答被消费**。异常或不完整候选
与用户按键在 crossterm 事件层无法完全判源；缓冲有界，超时/失配原序返回并停止后续
探测，优先保护真实按键，绝不在超时后吞疑似十六进制尾巴。迟到的完整新帧仍能识别。
标准查询不支持时可用 COLORFGBG，最终深色；不宣称所有终端支持实时系统主题通知。

## 门二与验收记录

按仓库协议调用 reviewer SKILL 做本轮聚焦外评，完整原文保存在
[`2026-10-27-tui-usability-code-review.md`](2026-10-27-tui-usability-code-review.md)。
运行目录 `/tmp/dsh-review/tui-usability-r1.YnreNa`，`dsh --profile headless` **exit0**；
原结论为“门一有条件通过、门二不通过”，不把进程成功改写成代码门通过。

| 外评项 | 处置与边界 |
|---|---|
| 低1：零行 FETCH pending | 匹配的零行应答完成 pending，按新 history_base clamp，独立 2s 退避避免每拍空重试；补 trim/预算 miss 回归。 |
| 低2：observer Text 纯应答 | 非活动 observer 的非 paste 纯应答直接丢弃，与 DATA 归属规则一致；真实 paste 保留。由 backend 实现、主实现者独立复核。 |
| 中3：主题只改 chrome | 首个完整 snapshot 后及有效深浅色变化时发送已有 per-leg THEME，Busy 下重试。非活动只缓存本腿色，接管时沿既有 active theme 应用；补程序 OSC11 亮暗查询。 |
| 中4：自身选中/关闭错位 | 拦截 self 先于改选中项，继续显示/操作原会话；承载会话禁止关闭，说明退出 TUI 后再关闭，不新增自杀/承诺显式退出流程。 |
| 低5：采样失败丢 self 标记 | 空本地列表免采样，已确证 name+pid 保留，消失或 pid 变更时清理；远端不借用本机 PID/缓存。 |
| 中6：clippy 硬红 | 按惯用 `?` 与结构体初始化修正，最终树重跑 all-targets `-D warnings`。 |
| 中7：记录未闭合 | 本节保留外评原结论、逐项处置和候选/最终树区分，最后冻结后统一补最终 SHA、命令 exit 与独立签门。 |

独立交叉复核：backend 回读主实现者的 surface/FETCH、appearance/事件保真、主循环的
generation/背压/退出、鼠标 press-release 跨快照配对；UI 回读其非作者部分的自动主题与
真实历史行为；主实现者独立回读 backend 的 registry/observer、握手协商与本机祖先标记。
整改后的独立交叉复核与最终针对性补验现已完成。**门一、门二通过**：这是原外评
“有条件通过/不通过”之后逐项整改、由独立子代理交叉复核后的签门，不改写为 dsh 直接
通过。backend 明确签过 surface/FETCH/主题、统一输入几何 ledger、observer/真实嵌套；
UI 仅只读复核主实现者接手后的 self 禁选/禁关/导航修复，未把本人原作者范围算独立。
主实现者独立复核 backend 的纯应答早退、稳定 name+pid 记忆和注册表在途摘腿守卫。
没有剩余高/中项；Rust thiserror、可见性、复用编码器/注册表与无逐行移植痕迹均确认。

候选证据：SHA256 `1c7d445e33ec4871abaa5011f79d47705c5b7b4133535708aa83079174c63637`
的旧 ls 烟囱完整 exit0（10 个真实 ls 场景、OSC7、66×27/81×33、自动主题、关闭第二会话/
保留首个、分离重进/SIGTERM）；观察腿/真实嵌套烟囱 6 PASS、exit0。候选预览
`/tmp/homeway-tui-usability-previews/{empty-dark,shell-dark,shell-light,ls-wide-color,ls-wide-scroll}.png`
由主会话实际看图核对无品牌首行、单行 footer、深浅/宽字/TAB可读。

第一轮新交互脚本在主题阶段失败，严格采样证明 reader 已运行；测试器将 bracketed Paste
插入 OSC11 色值中间，构造了畸形应答。`/tmp/homeway-tui-usability-diagnostics.json` 显示
字节回退与深色保持，符合有限匹配器设计。测试改为合法完整应答与字面 OSC Paste 相邻，
不删除字节等值断言；用单行 base64 与 ready 文件消除命令时序歧义，原失败日志保留。

core term 首轮为 fixture 新 case 的 clientId 断言遗漏（215/1），修正 fixture 消费后第二轮
`clear_screen_skips_backlog` 超过整体截止被 SIGTERM，原文保留；隔离同例 **1/0，3.62s**，
第三轮完整 **216/0，13.83s**（`/tmp/homeway-tui-usability-term-green.log`）。未定位到挂轮
根因，不登记为已证实既有 flake。后续局部 core 整改另列针对性结果。

## 最终冻结与五项验收

实际交付调试件 `target/debug/homeway`，SHA256：
`e19c014d5c986ed9c653631e0e9e3764d2c393e338d5c1ffa4111bb44fa259ea`。
最终源码冻结后构建，三组真实脚本前后哈希保持相同；以下均为该件结果，不累加候选重跑。

| 用户项 | 最终证据 |
|---|---|
| 1 历史回滚 | 真实 PTY 180 行：普通滚轮、Shift+PageUp/Home 触发服务端 FETCH 到 HIST_000；由外部 release 文件让已运行程序产生新输出，原 27 行画面逐行不变；Shift+End 恢复 APPENDED_LIVE。应用 mouse reporting 普通滚轮实收 SGR，Shift+滚轮不入 PTY；备用屏不混主屏历史，ED3/epoch 后不复用旧页。键入实际输入按终端惯例恢复实时。 |
| 2 自动主题/去首行 | panel 从 y=0 开始，无 homeway 品牌或手动按钮。合法 OSC11 BEL/ST、8/16bit、相邻普通键、字面 OSC Paste、非匹配 Alt+] 实际输入逐字节等值；真实会话内 Python 程序查询 OSC11，分别读到当前浅色/深色默认背景。旧 theme 文件不参与选择。 |
| 3 单行底栏 | 40×10/48×12/100×30 自动布局检查与实际 PNG 均仅末行 footer；状态只保留一份退出提示。TUI 窗口100×30对应PTY66×27，120×36对应81×33。 |
| 4 多端权威宽裁 | raw 活动140×40，窄 TUI observer 接入/缩窗/focus 不抢尺寸；完整宽行只裁左段，下一原行 WIDE_READY 不重折行。真实键/应用鼠标才切 pane，原 raw 输入可夺回140×40。连续 SGR press/release 在新快照未到前都按新 pane 同坐标，真实 od 字节对账；Key/Paste/wheel 的统一 ledger 由单测钉住，未配对被动事件在匹配 baseline 前等待，不猜超时 owner。 |
| 5 承载会话 | 真实 `homeway term`→承载 shell→`homeway tui` 的祖先 PID 匹配为“·自身”，不递归 attach；self 点选不改变已有 A 目标，右键只说明“退出后再关闭”且禁止 KILL，箭头跳过自身。仍能创建/控制另一 shell，退出回原承载 shell，两会话保留。远端同名/同 PID 单测不误拒。 |

| 最终检查 | 命令/日志与真实退出 |
|---|---|
| TUI 专项 | `cargo test -p homeway-cli tui:: --offline` **66/0，exit0**；`/tmp/homeway-tui-usability-final-tui.log`。覆盖 FETCH 空页退避、seq/epoch/宽字/TAB、self 保原目标、Key/Paste/wheel 与 press-release 几何窗口、隐藏 pane 旧 keyup。 |
| core 增量 | `cargo test -p homeway-core observer --lib --offline` **4/0，exit0**；`/tmp/homeway-tui-usability-final-observer-core.log`，实际验证非活动 THEME 不改 active 查询色、纯 Text 应答丢弃、真实输入接管后应用缓存色。注册表摘腿活动守卫此前专项 **10/0**（`/tmp/homeway-tui-usability-session-final.log`）；完整 term216/0与挂轮按上节分列。 |
| clippy/build | workspace `--all-targets --offline -- -D warnings` **exit0**，`/tmp/homeway-tui-usability-final-clippy.log`；`cargo build -p homeway-cli --offline` **exit0**。 |
| usability 真脚本 | `python3 tools/tui-usability-smoke.py --phase all --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-usability-final-previews` **8 PASS、exit0**；`/tmp/homeway-tui-usability-final-all.log`。合法主题与历史可用 `--phase` 独立执行，避免一个场景阻塞其它验收。 |
| observer/嵌套真脚本 | `python3 tools/tui-observer-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-observer-previews` **7 PASS、exit0**；`/tmp/homeway-tui-usability-final-observer-smoke.log`。首轮同7PASS工具封装丢失exit字段单独保留，最终轮有明确 TOOL_EXIT_CODE:0，不混算。 |
| 原真实 ls/生命周期回归 | `python3 tools/tui-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-usability-final-ls-previews` **exit0**；`/tmp/homeway-tui-usability-final-ls.log`。100/160列 HOME/项目/多列TAB/彩色中文组合字长名/滚动10场景，OSC7、resize、关闭第二保首个、分离重进与SIGTERM终端还原均过；空闲CPU **0.50%/4s**（单逻辑CPU=100%），不是性能判据。 |
| 夹具与平台 | `fixtures/SHA256SUMS` **64项全过、exit0**，`/tmp/homeway-tui-usability-final-fixtures.log`；frames14案 parity在term整轮，fuzz展开173种子摘要`d4aab7fe739260cf1461464e56d6102798c33f8d7e47928059aac2b1e9ef78a8`。注册/几何核心冻结后 Windows、双musl、OHOS真NDK check均exit0（`/tmp/homeway-tui-usability-{windows,musl,musl-arm,ohos}-final.log`）；最后纯应答早退/客户端增量以上述core4/0、TUI66/0、clippy补验，不宣称这些平台真实运行。 |

预览（`/tmp` 构建/测试产物不入 git）：最终历史
`/tmp/homeway-tui-usability-final-previews/{history-oldest,history-live,wide-authority-cropped}.png`，
观察/承载 `/tmp/homeway-tui-observer-previews/{raw-wide-140x40,observer-cropped-pane,nested-self-session-blocked}.png`，
最终深浅/ls `/tmp/homeway-tui-usability-final-ls-previews/{shell-dark,shell-light,ls-wide-color}.png`，
均有同名 txt。主实现者实际看过历史图，backend 实际看过三套观察图，主会话也核对了最终
宽行裁剪/下一原行/自身提示与单 footer。

所有脚本 finally 回收自有 PTY/daemon；最终只读 ps 检查本轮 `/tmp/homeway-tui-{usability,observer,smoke}-`
与私有出口前缀无遗留进程，exit0。无 pycache/pyc 入库，`git diff --check` 通过。
未重启生产守护、未使用默认 state、未触系统剪贴板；旧服务缺 observer 能力须升级后接入，
本轮没有自动换装/发版/部署。源码与必要验收完结，交主会话维护 ROADMAP 并本地中文提交。
