# TUI 鼠标拖选与自动复制（2026-10-28）

本轮仅修改 CLI 的显示、输入手势与剪贴板适配，不改 core、wire、服务端或会话尺寸策略。
生产实例/凭证未用于实现验证；本机已由主会话仅换前端文件，不重启后台；LAN71状态见部署记录。

## 技术门与实现边界

主会话核准最小设计：左键Down捕获一次当前可见authority快照，拖动时冻结该画面并高亮线性选区；
底层Surface继续收取数据，Up从同一份快照生成文本。无自动滚屏或跨页选择框架。
backend独立调查/技术交叉门、UI独立代码交叉门在子任务内进行，最终签门与实际验证结果记在下文。

- Shell中左键拖选；mouse-reporting应用保留普通鼠标输入，Shift+Down才开始本地选择。
  手势路由锁定到Up，中途modifier改变不转交；已发应用press的drag/release继续原配对。
  本地选择取消后仍吞对应Up，拖出面板按边界收尾。标题、X、侧栏、空画面均不开始选择。
- 本地Down只改变UI焦点，不发Focus、INPUT或RESIZE；完整拖选不会接管observer活动腿。
  单击没有文本、不发剪贴板请求。选择中footer来自gesture状态，取消不写持久notice。
- 快照仅含pane可见物理列。复制按行流式排列，完整权威宽度的softwrap连行，硬换行保留LF，
  行末显示填充空格去除。宽字头尾一同选择，组合符保留，TAB/C0沿既有显示投影为空格，
  隐藏字仅复制可见空格；右边裁断宽字归为可见空格，不带出隐藏右半或pane外尾巴。
- 主机/会话/generation、ready、revision、history epoch/base、几何或ALT变化使选区失效。
  滚动、键入、粘贴、焦点离开、窗口调整主动取消；修饰键本身与release不打断手势。
  普通seq输出允许继续冻结；历史尚未FETCH的行不能复制，不猜造内容或返回部分文本。
- 沿用剪贴板现有32 KiB上限（clipboard::MAX_BYTES）；UTF-8文本超限明确失败，不部分复制。
  System只在系统工具成功后提示已复制；SSH或homeway term承载优先提交终端请求，只提示
  “已提交终端剪贴板请求”，不宣称终端确认。环境路由纯函数有代表性测试。
- 选择复制使用独立purpose/ticket及目标身份，不递增/夺取角色token或代理操作的operation。
  新错误/操作、导航、取消、新手势主动丢弃旧复制归属，同文字新notice也不能复用。
  周期成功poll不清有效owner；OSC序列在owner/上下文复核后才写stdout。Native工具已经发生
  的系统复制不能撤销，但其过期结果不会覆盖当前提示；测试不使用真实系统剪贴板。

## 分工与独立评审

backend作者：selection.rs、Surface只读getter；UI作者：view高亮、clipboard用途/远程路由；
主实现者：App手势、异步归属、App回归与整合。各模块由未写该模块的人交叉审查，作者自查不算独立门。

- UI对selection/Surface独立只读通过：右裁不改源网格；宽字/组合/TAB/隐藏字、软硬换行、
  缺行/超限拒绝、identity与后台seq边界成立。未自行执行cargo。
- backend对view/clipboard与初版App只读通过；指出新gesture/取消必须主动废弃pending复制，
  已纳入App失效点。最终App及direct失败/全MouseDown失效增量已独立通过。
- 主实现者回读selection与view/clipboard：复用VT投影、不注入PTY、不创建锁/共享状态，
  错误使用thiserror与Result；无Go逐行移植式接口、多余Arc/Mutex或全仓架构改动。

## 验证记录

主会话先实际执行 `cargo check -p homeway-cli --offline`，exit0，3.38s。
第一轮TUI测试编译exit101：App测试用Geometry结构更新语法涉及view私有字段（E0451），
未运行测试；已改成Default后仅赋可见terminal，不扩大字段可见性。该失败保留，不写成通过。

第二轮全TUI编译通过，实际92通过、1失败、1 ignored，exit101。唯一失败是view宽字尾格
TestBackend断言：Ratatui diff跳过width2头的尾格，测试误把TestBackend的Reset尾格当完整Frame。
已仅修测试位置：overlay后FrameBuffer仍强断言head/tail RGB、HIDDEN/去REVERSED；真实flush后
保留宽头、隐字、普通字与源VT未变断言。未修改产品高亮/隐藏语义或期望颜色。

backend最终技术与独立App代码门通过；UI独立selection/Surface与App门通过，direct失败源/
全MouseDown的后续增量由backend复核通过。UI终检发现command分支替换漏行后，主实现者精确
补1行并由UI实读终签通过，最终无剩余阻塞。四个直接错误源（command队列、FETCH、INPUT满、
resize）均主动废弃旧copy；新MouseDown（含自身说明这种ActionNone）同样废弃，补充同文字
Failed与pane外Down回归。测试用recording剪贴板仅cfg(test)，不运行系统工具。

主会话在clippy修正前的阶段源码上执行全TUI回归，实际exit0：**94 passed、0 failed、1 ignored**，
日志 `/tmp/homeway-tui-selection-final-tests.log`。随后独立执行
`cargo build -p homeway-cli --offline`，实际exit0（0.08s）。
随后clippy实际exit101：`unnecessary_unwrap`指出App的name先判断is_some后unwrap。
已将该分支改为`if let Some(name)`配合原active/mouse条件的filter，直接把已绑定name移入context，
不加lint豁免、不改变None/不匹配/Shift/鼠标路由。该增量另做非作者只读评审；修改后的最终
TUI/build/clippy重跑结果见下文，前一轮运行结果不冒称覆盖后续源码。

续接子任务的非作者对最终if-let/filter增量实读，技术与代码门通过：None、activeNone、
不同name均拒绝，name匹配后沿原条件判断mouse-reporting与Shift，focus/local_left/快照/
return位置不变。烟囱作者补tailcat路由后，由续接主实现者独立只读复核其准确载荷、native
零新增、上下文及分实例baseline/资源回收，无新增阻塞；该脚本AST与diffcheck实际exit0。

主会话对该小修正前的debug件 `38f393c148c07b35a9c4b1733acaa51e3560d316ffce31a25f3748f1b6feba64`
运行真实私有选择烟囱，实际exit0、**6 PASS**，日志
`/tmp/homeway-tui-selection-smoke.log`。包含click无复制、精确CJK/组合/TAB、深浅高亮、裁尾、
历史反向两行、raw活动腿140×40不变、TTY还原与资源清理。主会话实际查看暗色CJK/TAB、暗色
裁尾、浅色历史反向三张PNG，高亮对齐且可辨。该验证使用mock helper，不触真实桌面剪贴板。
带tailcat标记的真实PTY仅提交OSC52、native mock无新增调用的追加验证，见下文最终7 PASS。

## 真实私有交互入口

```sh
python3 tools/tui-selection-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-selection-previews
```

独立私有state显式关闭serve/relay。仅TUI child PATH注入私有pbcopy/wl-copy/xclip/xsel，清除其
SSH/tailcat路由标记；mock将stdin原字节与调用次数写私有文件，绝不触真实系统剪贴板。
原raw CLI为140×40活动腿，TUI窄observer用可见cell坐标发送SGR；检查click无写、CJK/组合符/
TAB拖选精确文本、裁尾不泄漏、浅色历史反向跨两行。每步LIST确认rawactive、appinactive与
140×40保持；finally只回收脚本自有PTY/统一进程，核产物哈希未变。应用mouse/Shift配对由
App输入队列回归覆盖，不把模拟终端验收等同所有终端软件鼠标/真实系统clipboard实测。

追加的独立tailcat TUI仍用同私有shell和mock PATH，SSH变量清空，仅对子进程设置
`TERM_SESSION_ID=tailcat-selection-smoke`与`TERM_PROGRAM=Tailcat`。捕获释放后的PTY原串，
必须仅一条OSC52、base64载荷逐字节对应所选`TERM_COPY:中é`，native mock计数和内容均不变，
屏幕仅提示“已提交终端剪贴板请求”。观察腿用本次独立baseline比较，shell PID沿用；
两个TUI各自验证TTY还原，finally各关闭一次。该路径不接入真实桌面终端或系统剪贴板。

最终if-let源码树由主会话重新执行全TUI回归，实际exit0：**94 passed、0 failed、1 ignored**；
独立build实际exit0，最终clippy实际exit0（1.50s）。最终debug工件SHA256为
`6d8b05b3f50699963a524e164bc0f308e0776ca7a2b43ea32fc35e3f33aaf647`。
主会话对同一最终工件执行包含tailcat路径的新版真实私有烟囱，实际exit0、**7 PASS**，日志
`/tmp/homeway-tui-selection-final-smoke.log`。新增真实PTY验证精确OSC52请求、native mock零新增
及被动observer所有权；其余六项与前轮一致。未验证真实桌面clipboard写入确认，不把提交OSC52
请求说成终端已允许或用户剪贴板已更新。主会话另行实际查看最终新SHA的暗色CJK截图，
高亮、边框与底栏正常；前轮三张同渲染实现的深浅/历史截图记录仍保留。

## 本机已更新；LAN71待连接恢复

backend技术与UI代码增量门均独立通过。两个脚本均不调用homeway/daemon/launchctl/kill，
不访问state；要求新完整SHA、旧完整c373、codesign、锁/备份、同目录暂存/fsync/原子rename，
异常条件退文件，持久receipt且前后唯一零参守护PID不变。未改变任一端后台或身份。

- 本机 `/tmp/tui-selection-copy-install.py`，SHA256
  `4c3786c548a759c49d02ee9ecf7d30115380f777ecf01b3818ea7fa569d1eccc`。
  相对已审439330仅OLD_SHA→c373与selection-copy备份前缀。
- LAN71 `/tmp/tui-selection-copy-lan71-install.py`，SHA256
  `6a52cc2da483912f2fb6b33cb552cdf78bf04b2f867fde62a81c845f0707cabf`。
  对local仅target=/usr/local/bin/homeway、source=/tmp/homeway-lan71-selection-20261028/homeway.debug、
  /usr/bin/python3、兼容3.9的chunk SHA、501:0条件fchown与远端备份前缀。目录继承gid0已匹配时
  不无谓chown；不匹配且权限不足则在rename前拒绝。AST按3.9语法通过，未远端执行。

本机安装器由主会话实际执行，exit0、receipt **SUCCESS**。PATH新件完整SHA与最终工件相同，
`/opt/homebrew/bin/homeway`与`target/debug/homeway`另行shasum一致；严格codesign实际exit0。
后台零参守护PID **55632→55632**，`rollback_performed=false`，无服务重启或state访问。
旧件及receipt保留于
`/Users/zhaozhe/homeway-rollback/tui-selection-copy-20261028-8930929df71a/`。
已运行的旧TUI进程需退出后重新进入，才能使用新前端。脚本静态门与这些实际运行证据分列。

主会话本次尝试LAN71只读SSH预检，连接超时5秒、实际exit255（port22 timeout）；命令已在
网络沙箱外执行，属于目标不可达。该次未创建stage、未上传、未运行安装器；LAN71仍待换装，
不能把历史c373部署或本机验证写成本轮远端完成。第二次只读SSH重试同样实际exit255，
port22 `Operation timed out`，亦未创建stage/上传/换装，此轮不再重试。LAN71最后已验工件仍为
c373；本轮因连接不可达未重新实核远端现状，须待连接恢复后再核旧件与PID后执行换装。
