# TUI 终端标题栏关闭按钮（2026-10-28）

> 首版空态显示灰色X；用户随后要求空态完全隐藏，已按文末追加调整实现。首版验证/
> 部署事实保留，当前行为以追加调整为准。

右侧终端标题栏增加 `[ X ]`，关闭当前展示的所选会话，不退出 TUI。仅修改 CLI 前端
mod/view/backend模块与验收工具；core/wire/服务端不变，后台无需重启。

## 技术与代码双门

**backend 独立技术交叉门、UI 独立代码门均通过，无阻塞项。** 主实现者负责mod/view/
工具，backend作者负责定向关闭结果，再反向审mod/view/工具；主实现者另行回读后端
成功/失败边界，UI对三文件作独立只读代码评审。

- 按钮与左侧 `[ + ]` 共用五列尺寸和主题accent，禁用时muted。位置由共享Geometry给出，
  在顶边框且不进入PTY内容区域。标题用独立矩形按物理宽度裁剪，给按钮/边界留位；
  40列窗口、超长中文标题都不覆盖按钮或右边框。
- 只有左键press产生动作，release/right/move不发关闭也不透传PTY。无展示目标、所选与
  展示目标不一致、自身会话、创建/关闭在途时灰显；连接中或缓存断线但有明确目标时
  仍可关闭，不要求connected或surface.ready。
- header动作携带名字与navigation epoch，App执行前重新核对当前上下文，再复用已有
  CloseSession/KillSession。切主机/会话后的旧名字或旧epoch动作不能重定向到新目标。
  原右键关闭未展示条目的功能保留，不把header额外eligibility强套到菜单。
- 关闭单飞记录原target/name，只由精确匹配的定向结果完成；其它错误/刷新/旧目标
  反馈不能解除它。匹配关闭失败后可重试；切主机后的原请求完成会解除pending，却不
  改新选择或attach。已排队KILL持有原target/name，不随之后导航变化。
- 后端前端模块新增 `SessionClosed { target, name, result }`，成功及execute前后所有
  失败均携原目标，包括UDS连接/准入早失败；原连接清理与失败不增mutation epoch保持。
  仅将原KILL的泛Notice/Failed细化为定向结果，仍沿原通知/刷新流程，无新协议或通知框架。

实现使用既有类型/Result与上下文，未新增字符串分支判定、共享锁或无谓复制，也无
逐行移植式接口。自身禁选/禁关及其它主机/会话菜单保持原有保护。

## 冻结源码验证

主会话分别实际执行：

- `cargo test -p homeway-cli tui:: --offline`：**81通过、0失败、1 ignored，exit0**。
- `cargo build -p homeway-cli --offline`：**exit0**。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings`：**exit0**。
- 新debug SHA256：`6d3afe88aadf430f95a73430f8b974fc45da30288ef61734555c357f66ecb94c`。

新增回归覆盖暗/亮与40/100列下按钮尺寸/颜色、标题裁边、单press/释放隔离、禁用/自身；
App中创建/关闭单飞、无关失败/不匹配结果、跨主机匹配失败后的新选择与重试、旧epoch
拒绝、菜单仍能关闭未展示条目；后端真实私有UDS的成功KILL、服务ERROR、无UDS建连前
失败都返回定向原目标。没有以core全仓/跨平台重跑替代本轮前端交互验证。

## 真实鼠标与预览

复用 `tools/tui-smoke.py`：私有state显式关闭角色，创建A/B后，SGR按按钮X字符实际
坐标（1基列width-3、行1）发送press+release，CLI确认仅B消失、A存活、TUI仍活，空X
灰显。等待当前屏确认A重新接入后输入新marker，避免累计“已接入”旧文本掩盖在途；
另建C走原右键关闭回归，不因新按钮删掉原菜单覆盖。原真实ls/TAB/宽字/主题/分离/
恢复验证保留。Python AST通过，子任务不执行提权测试或连接生产state。

命令由主会话执行，截图目录可选：

```sh
python3 tools/tui-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-close-previews
```

预览包含 `empty-dark`（灰X）、`shell-dark`（可关X）、`terminal-close-ready-light` 与
`terminal-close-empty-light`；由真实PTY还原，不是伪造会话画面。

主会话首轮对上述同SHA实际执行烟囱，**20项PASS、exit0**：X press/release只删B留A，
TUI继续运行、A重新接入执行新marker、另建C走原右键关闭均通过，十个ls等原回归保留，
私有资源已清理。该轮实际产物位于 `/tmp/homeway-tui-terminal-close-previews/`。

随后UI独立审查提出工具等待应更精确：灰X也可能是关闭pending，不能单凭灰色称为空态。
仅新增一句等待**当前屏幕**中央创建引导，确认旧selection已清，再判断empty灰X并重接A；
没有改Rust或二进制，首轮成功保留，同SHA增强断言复跑由主会话执行，结果另行记录。
增强断言后同SHA复跑也 **20项PASS、exit0**，当前空态确认、灰X、A新输入及C菜单路径
均完成，私有资源清理。两轮是同场景先后核验，不累加宣称40种场景。实际截图仍在
`/tmp/homeway-tui-terminal-close-previews/`；主会话已查看ready-light、empty-light与
empty-dark，确认启用/灰显、主题与尺寸。子任务也查看empty-light，确认关闭B后A留在
列表且右侧回创建引导。

## 仅换前端准备

延续本机debug调试授权，主会话最终仅原子更新PATH文件，后台PID55632保持。短安装器
`/tmp/tui-terminal-close-install.py`，SHA256：
`1bbd27bf35bfb662080bbb3a849551510c61305bf9dc5eb9238442a68b199974`。
相对已审4b6版本仅将OLD_SHA更新为现役完整fbc、回退目录前缀改terminal-close；新SHA
仍为必传参数。保留目标锁、受限备份/回执、同目录暂存/fsync/签名/owner501:80/mode755、
原子rename与异常原子退文件、前后PID一致；不调用daemon/launchctl/kill，不访问state。
backend技术与UI代码增量双门均通过：逐字节核对相对4b6仅上述两处差异、AST与新件
哈希匹配；UI并终签增强后的空态等待，未引入新的产品改动或测试执行面。

精确命令由主会话在验收后执行：

```sh
/opt/homebrew/opt/python@3.13/bin/python3.13 /tmp/tui-terminal-close-install.py --expected-sha 6d3afe88aadf430f95a73430f8b974fc45da30288ef61734555c357f66ecb94c
```

已运行TUI需退出重进以使用新按钮。无发布/推送/远端或服务端换装。

## 前端更新完成

主会话唯一执行上述安装命令，**exit0、receipt SUCCESS**。子任务只读复核
`~/homeway-rollback/tui-terminal-close-20261028-0347eac30d14/receipt.json`，完整old为fbc，
完整new/installed为上述6d3a；正式PATH与target/debug SHA256一致。前后守护PID均为
**55632**，`frontend_only=true`、`service_restart_requested=false`、
`rollback_performed=false`。回退目录0700、旧件`old`0700、receipt0600，旧fbc保留。

本轮仅原子更新前端文件，没有重启后台、访问state或终止生产会话；实际点击/关闭
证明来自上述私有实例，不宣称生产交互验收。最后仅补报告，未再改源码/工具/脚本或
重复测试。主会话负责ROADMAP及本地中文提交。

## 用户追加：终端空态完全隐藏按钮

无右侧展示目标时不画X、不保留标题专用空隙，也没有关闭热区；这与“有会话但暂时
不可关闭”的灰显独立。Model仅增加一个显示派生字段，由App每次绘制前从
`active_session.is_some()`取得；不拿connected/surface.ready判断是否展示。空态关闭
Rect保持零尺寸，绘制/命中均无效，标题宽度从panel-8收回panel-2；即使残留旧payload
也不能重新建立热区。有目标但创建/关闭在途或自身不可关仍保留灰显，原目标/epoch/
单飞及后端关闭流程一字不改。仅mod/view与烟囱变化，无core/wire/后台改动。

**backend技术、UI代码简短增量双门均通过，无阻塞。** 扩展已有按钮测试，覆盖暗/亮、
40/100列下无X字符/空热区/旧坐标点击无动作、长标题回收空间，同时保留有目标灰显与
原有效按钮测试；没有新增测试框架或测试数量。

主会话实际分别执行：TUI专项**81通过、0失败、1 ignored，exit0**；debug build与CLI
all-targets clippy `-D warnings`均**exit0**。新SHA256：
`31b3a879b857635ca299116df86c3583eaefa37dc02f31023a2059b894b106e3`。

原真实烟囱同步将empty灰色断言改为hidden：初始空态当前header无X，点击原按钮坐标
后仍零会话/TUI活；有A/B时正常X关闭B，等待当前空态后无X，再点旧坐标A仍唯一存活，
随后仍可重新接入A输入marker，另建C保留菜单回归。预览沿原工具生成，子任务未执行
测试或连接生产state，实际命令由主会话执行：

```sh
python3 tools/tui-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-close-hidden-previews
```

主会话实际执行上述同SHA烟囱，**21项PASS、exit0**：初始空header隐藏及旧坐标无动作，
关B后当前空态隐藏及旧坐标保A、A重新接入输入、C菜单及原ls/TTY恢复均通过，私有
资源清理。实际预览在命令所列目录；与首版两轮20PASS分列，不合并累加计数。

仅换前端安装器 `/tmp/tui-terminal-close-hidden-install.py`，SHA256
`17d90ff0b4bbd807a3f651f378132d21aec5cde6c0b47579660b96eae104d1a9`，相对已审1bb版
严格只有OLD_SHA→完整6d3a、backup前缀→terminal-close-hidden两处参数变更。backend与
UI均只读核定最终SHA/diff/新工件哈希通过，原受限备份/锁/fsync/原子换退/PID一致与
无state无重启门继承。

精确命令由主会话在验收后执行：

```sh
/opt/homebrew/opt/python@3.13/bin/python3.13 /tmp/tui-terminal-close-hidden-install.py --expected-sha 31b3a879b857635ca299116df86c3583eaefa37dc02f31023a2059b894b106e3
```

主会话已唯一执行micro前端更新，**exit0、receipt SUCCESS**。子任务只读核验
`~/homeway-rollback/tui-terminal-close-hidden-20261028-396876cec358/receipt.json`：old完整
6d3a、新件/installed完整31b3，正式PATH与target/debug完整SHA一致；前后守护PID均为
**55632**，frontend_only=true、service_restart_requested=false、rollback_performed=false。
目录0700、旧件`old`0700、回执0600，旧件保留。主会话已查看实际empty-dark与
terminal-close-empty-light均无X，ready-light仍有X，预览来自本次私有21PASS烟囱。

本次micro完成证据为81/0（1 ignored）、build/clippy、同件21PASS及上述仅文件更新，
不与首版累计计数。不重启后台、不访问state、不终止生产会话；已运行TUI退出重进
即可使用空态隐藏行为。最后只补文档，未改源码/工具或重测。
