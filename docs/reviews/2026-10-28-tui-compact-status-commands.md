# TUI 精简行内状态与全屏快捷指令（2026-10-28）

用户要求取消全屏底栏，把极简提示放到左下会话区末行；操作/错误通知放对应条目第二行，
正常接入不提示。Ctrl+B 打开全屏指令，执行一项即关闭。

## 实现边界

- 右侧终端使用全高，左下列表内部扣出一行，仅显示 `^B 快捷指令` 或真实历史距离与
  `⇧End` 回底提示；窄屏保留真实 End 键名，自适应去空格/缩计数，完整距离与快捷键在弹窗可读。
- Ctrl+B 全屏覆盖原布局，使用同一 Geometry，避免弹窗引发尺寸变化。真实指令为
  h/s/Enter/n/a/m/q/Ctrl+B；Esc/未知键消费并关闭，鼠标与粘贴不穿透，方向键滚动详情。
  已发按键的释放和已发鼠标 Down 的 Up 仍成对投递，弹窗不转发新按键/Drag/滚轮或焦点输入。
- 展示使用 typed `NoticeTarget::Host(Target)` 或 `Session { target, name }`；按稳定主机ID/
  会话名定位，消失或当前不可见会话仅回退自己的主机行，不贴到新选中项。CLI后台事件
  Notice/Warning/Failed 传原始 Target；未修改 core、服务端、RPC、wire 或后台线程调度。
- 内部完整 notice 保留原异步请求归属。行内摘要精简，完整 fact/error 在全屏弹窗可读；
  摘要与详情经既有 clean 过滤控制字符。正常 state/type/cwd 与状态点颜色沿用，临时通知
  使用提示色，不改变状态点含义；成功约3秒后恢复 metadata，沿既有循环到期，无新增任务。
  全屏阅读时暂停清理，关闭后按原期限处理。Progress 同目标完成后清理，错误留待后续操作。
- Brief 与 warning 恢复均绑定完整 typed notice，旧期限/成功 poll 不清新错误或 view 说明。
  剪贴板队列失败是错误，不悬挂 progress；Selection 保留 ticket/context，Operation 另绑定
  operation/typed notice，emit OSC52 前先核归属，旧结果不能清掉新 pending owner。
  同操作刷新警告可与复制结果合并；等待复制时刷新恢复仍保持 progress，不提前套成功期限。
  系统成功摘要“已复制”，终端请求摘要“已发复制请求”，后者不宣称终端确认。

## 分工与独立评审

实现者负责 App/CLI后台事件、通知生命周期与归属回归；view_commands 子任务负责 view、输入、
预览与三份私有 smoke 适配。技术边界在两作者间交叉确认，并由第三个未写代码的 review
子任务独立核查。主会话执行实际验证，子任务不运行 cargo、安装或生产服务。

独立技术门认可：typed 原目标、有界单通知、现有 tick 生命周期、全屏 overlay 不改 geometry、
最小40×10命令与详情可达。没有新增共享锁/Arc/后台框架，不存在逐行移植式接口。
代码门整改包括：复制入队失败误作 progress、旧 Operation callback 先 take 误清新请求、
异主机 Sessions 错误混入旧 fact、warning 恢复覆盖新说明、待复制状态过早到期、关闭成功
retarget 后 Brief owner 失配。对应修正与必要回归均已落；独立评审者最终实读后整体技术/代码门通过，无剩余阻塞。

## 实际验证记录

主会话首轮 `cargo check -p homeway-cli --offline` 实际 exit101，E0502：poll_surface 的 attach
借用跨过 show_error。仅将 FETCH 错误收集到局部变量，在本轮 attach 使用结束后应用通知，
不复制连接或增加 Arc。随后 cargo check 实际 exit0（0.92s）。

首轮全 TUI 实际 exit101：99 passed、2 failed、1 ignored。App 历史测试仍断言旧箭头，
已改按权威距离与真实 `⇧End` 断言。view 的奇偶高度测试使用旧全屏底栏尺寸：更新既有
回归同时覆盖31行/13内容行时 spare 不命中、32行/14内容行的完整末项第二行可选择，
并核真实 footer 点击不改变选择，不机械放宽旧数字。另由实际 TestBackend 验证最小/标准
窗口打开 prefix 后光标隐藏、terminal rect 不变。该轮失败结果不冒充最终树全绿。

后一轮全TUI实际 exit101，测试尚未运行：新增cursor回归把 TerminalView 交给了测试中的
SessionVt draw wrapper，两处E0308。仅修测试调用真实 `super::draw` 入口，保留光标与几何断言，
不改产品接口；此轮编译失败也不记为测试通过。

再次全TUI实际 exit101：103 passed、1 failed、1 ignored。唯一剩余失败来自 App 测试
screen helper 逐物理格拼中文尾格空格，得到“^B 快 捷 指 令”；产品显示没有增加这些空格。
仅让该 helper 与 view 既有 buffer_line 一样按 cell.cell_width 步进，保留真实空格与精确
`^B 快捷指令` 断言，没有压缩全部空白或修改产品文案。

该 helper 首次修正后编译又实际 exit101、未运行测试：E0599 提示测试模块缺少
`ratatui::buffer::CellWidth` trait 导入，已仅补测试 import；不把静态 diffcheck 当编译通过。

随后全TUI仍实际103 passed、1 failed、1 ignored：同一历史回归后半第二次Start还残留旧
箭头断言。已通读整个测试，两个Start均核权威距离且非零、精确“{distance}行 ⇧End回底”；
首次正常、End归零、清会话后的三处均精确“^B 快捷指令”，该函数不再留旧箭头检查。
该修正仍仅涉及测试，主会话先运行单例再完整回归，实际结果在最终证据中记录。

主会话一次中间 build 实际 exit0（1.08s）；随后独立门又发现并修正上述 warning/复制期限/
关闭 Brief 归属边界，该中间工件不能作为最终部署件。最终验证与前述失败分列如下。

主会话对阶段工件 SHA256
`37a356af12da0f9c1515ed4a18335cbc9f0380e9b470893a0147f815e014aed1`
实际执行三套私有交互，均 exit0：本机 **22 PASS**、远端 **8 PASS**、选区 **7 PASS**，
日志分别为 `/tmp/homeway-tui-compact-local.log`、`/tmp/homeway-tui-compact-remote.log`、
`/tmp/homeway-tui-compact-selection.log`。这些运行保持原始严格输入、观察接入和精确复制判据，
并覆盖全屏指令打开、未知键不透传及继续shell输入。没有访问生产实例或真实桌面剪贴板。
后续即便只改测试，Cargo仍可能重建regular binary且DWARF/source checksum导致SHA改变；
因此上述结果只归属37a356阶段件，最终产物哈希、同件必要复验与换装证据另录，不能混称。

预览沿用既有 ignored `sidebar_layout_preview_artifacts` 与 `tools/tui-sidebar-previews.py`，
新增 commands-dark、commands-small-light、status-light，总共8幅；不创建新渲染框架。
主会话单独运行该ignored预览入口，实际exit0、1 passed，生成8份JSON；PNG renderer实际exit0。
已实际查看commands-dark、commands-small-light、status-light，以及37a356件真实shell-dark：
全屏遮罩、40×10完整命令、详情、行内短请求、左下footer与右侧全高符合预期。
三份 smoke 保留真实终端 ready/元数据、输入、观察者尺寸、裁剪、选区精确字节与OSC52判据，
适配新布局与行内摘要；不再依赖已取消的“已接入”全屏提示。

最终历史单例 exact 实际 **1 passed、0 failed、exit0**；随后同树全TUI实际 **104 passed、
0 failed、1 ignored、exit0**，日志 `/tmp/homeway-tui-compact-tests-final.log`。
最终 `cargo build -p homeway-cli --offline` 实际 exit0（0.08s）；
`cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` 实际 exit0（1.93s）。

最终 debug 工件完整 SHA256 为
`e1c9dac9bf0ea6b7f110e9ec870f6cf15ced73e092cd66da50aec78224e6ed5c`。
主会话对该同件重新执行三套私有交互，全部实际 exit0：本机 **22 PASS**、远端 **8 PASS**、
选区 **7 PASS**，分别记录在 `/tmp/homeway-tui-compact-local-final.log`、
`/tmp/homeway-tui-compact-remote-final.log`、`/tmp/homeway-tui-compact-selection-final.log`。
选区日志另核完整最终SHA前后一致；复制使用mock helper或捕获OSC52请求，不冒称真实桌面
剪贴板已获终端允许。此前预览与最终件采用同一渲染实现，之后只有测试接线/断言修正。
主会话另查看最终同件真实 commands-dark 与 selection-light-history-reverse 两张PNG：
全屏覆盖、完整快捷列表、左下“15行 ⇧End回底”及会话第二行拖选提示均正常。

## 本机 debug 文件更新（完成）

最小适配器 `/tmp/tui-compact-status-install.py`，SHA256
`bd6d6c343b72972e7b7cb93a6b29f893f647fe9f656e8465919fde16031561a5`。
非作者独立静态技术/代码门通过：对已审 `/tmp/tui-sidebar-width-install.py` 仅旧SHA改为
`53fb8678c93918c8ff7cee675567f7d533e8adca1e8781d3406b6880efc15d8b` 与备份前缀
`tui-compact-status-20261028-` 两处。保留必填新SHA、锁/私有备份/签名/fsync、原子换退、
前后唯一零参守护PID不变，不读正式state、不重启服务。静态门不代替真实换装证据。

主会话实际执行安装器，exit0、回执 **SUCCESS**，新完整SHA与最终已测工件一致；
后台PID **55632→55632**，未重启、未回滚、未访问正式state。旧件及回执保留于
`/Users/zhaozhe/homeway-rollback/tui-compact-status-20261028-14238ea6c41c/`。
另行独立核验PATH文件与target/debug完整SHA相等，严格codesign实际exit0；
ps确认PID55632仍为PPID1、UID501、零参`/opt/homebrew/bin/homeway`。
已运行的TUI需退出后重新进入，才能使用本轮新前端。

LAN71此前不可达，本轮不重试/上传/换装；不推送、不发布，阿里云不动。
