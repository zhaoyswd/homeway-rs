# TUI 稳定会话排序与通知点击归属（2026-10-29）

## 排序根因与设计

TUI backend每次成功LIST后按Reverse(lastActiveMs)重排，core在每次PTY输出时更新
last_active_ms，因此无键鼠操作也会反复改变列表。现有LIST已经提供固定createdMs；
未找到用户层最近使用排序约定。本轮TUI改createdMs升序，再以稳定name补并列，新的
会话自然放后；旧端缺createdMs默认为0，按name确定。标题/blocked/输出/lastActiveMs
不参与排序。选择仍按稳定name回收，不改变通知基线或恢复PID身份，不改core或wire。

## 通知根因与方案调查

当前macOS使用osascript固定display notification，仅标题/副标题/正文，没有点击目标。
Apple官方说明点击归属发送脚本/app，故会打开Script Editor/Finder而非承载窗口。
独立调查已核iTerm2 OSC9官方协议与上游点击源session reveal；Ghostty官方支持OSC9，
但不能据此承诺每版本精准点击；Apple Terminal未查到官方OSC9保证。

用户最新硬约束优先：各种终端通用；无法可靠回原会话时无动作，不打开不确定app。
因此不做Terminal/iTerm特定激活、不装helper/依赖、不发真实桌面通知。macOS统一向
承载终端发OSC9，由宿主原生绑定通知/会话；移除osascript来源。已有SSH/tailcat路线
同样保持OSC9。Linux沿既有notify-send无点击action（失败回OSC9），不加launch行为。
不支持OSC9的终端可能不显示系统通知，README明确此限制，不伪称所有终端支持或
保证精准跳转；行内blocking状态仍保留。

请求原主机/会话文本捕获、16槽有界队列、退出取消、控制字符过滤及单次回退保持。
自动测试仅mock并验证Mac从不调用helper、OSC序列唯一安全，不真实发送通知/激活窗口。

## 技术评审

非实现者 independent_review 分两段技术门、最终统一代码门；两段都在对应实现前明确通过。

排序技术门通过，无阻断，原始建议：

1. 用 sort_by 比较创建字段再比较name，避免sort key clone；旧端0与同毫秒并列须稳定，
   输出/标题/blocked/lastActive变化不影响次序。**接受**：created_ms: i64反序列化createdMs，
   直接字段cmp，移除TUI不再使用的last_active_ms字段，忽略wire中额外lastActiveMs。
2. 复用LIST回归反转活动值与返回顺序，确认App按name找选择、PID/blocked不依赖索引。
   **接受**：新增私有FrameIo真实LIST两轮回归；App现有name/PID/blocked路径不变且完整TUI通过。

通知技术门通过，无阻断，原始建议：

1. Mac必须在任何native helper调用前短路，彻底移除osascript/APPLESCRIPT/Mac参数分支；
   mock验证Mac本地及远端只出一条安全OSC9且不调用native，取消无输出。**接受**：显式
   Platform路由先Mac/remote，再Unix native；新增无副作用mock与取消断言。
2. Linux保持notify-send无action/激活/desktop-entry参数，失败仅一次OSC9；README明确
   不保证所有宿主通知或精准跳转，不宣称真实点击已验证。**接受**：原Unix参数/失败处理
   保持，文档按最新用户边界说明。

## 官方依据与能力边界

[Apple通知文档](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/DisplayNotifications.html)
明确点击会打开发送脚本/app；[AppleScript命令参考](https://developer.apple.com/library/archive/documentation/AppleScript/Conceptual/AppleScriptLangGuide/reference/ASLR_cmds.html)
提供正文/title/subtitle/sound参数，没有原终端会话click callback。用户报告点击进Finder/
Script Editor，本轮没有实发通知复现，代码与官方归属机制吻合，不能靠修改title修复归属。

[iTerm2官方OSC9](https://iterm2.com/documentation-escape-codes.html)支持由承载终端发通知；
[上游点击处理源码](https://github.com/gnachman/iTerm2/blob/master/sources/Notifications/iTermNotificationController.m#L419)
将通知userInfo对应到源session并reveal。[Ghostty官方OSC9](https://ghostty.org/docs/vt/osc/9)
也确认协议支持。以上不能证明所有终端/所有版本都支持显示与精确focus；尤其未找到
Apple Terminal的官方OSC9支持保证。因此这里只请求实际宿主自行处理，不增加任何特定app
激活或猜窗口逻辑；宿主未支持/禁用时可能没有系统通知，行内blocking状态仍保留。

这些官方材料由独立调查子任务读取，本轮不安装系统工具、Rust依赖或helper，不改通知设置，
不发送真实桌面通知、不激活窗口。未采用特定app/TTY脚本或持久通知app方案。

## 实现与实际验证

修改仅README、TUI backend与notifications，不改core/wire/现有判据或行为时间常量。
排序固定创建时间/稳定name；Mac通知仅走已存在的Request::terminal单OSC9安全序列，
由UI线程输出。Linux原有notify-send参数没有action、activate、desktop-entry跳转，
SSH/tailcat仍先走终端，发送时不重构原主机/会话正文，队列/取消/过滤保持。

实际执行：

- rustfmt与`git diff --check`：exit0。
- `cargo test -p homeway-cli tui::backend::tests::session_order_survives_output_activity_and_unordered_list_replies --offline`：
  **exit0，1 passed**，日志`/tmp/homeway-tui-session-order-test.log`。只绑/tmp私有Unix
  socket的FrameIo服务，两轮真实GREETING/LIST应答反转lastActive与原始数组顺序，涵盖
  同毫秒createdMs、旧端缺创建字段0、blocked/working值，TUI顺序始终
  legacy-a/legacy-z/a/b/new，创建值0/0/100/100/200。
- 排序先完成时全TUI实际 **exit0，127 passed / 0 failed / 1 ignored**，日志
  `/tmp/homeway-tui-session-order-notification-tests.log`。
- 两部分最终`cargo test -p homeway-cli tui:: --offline`实际 **exit0，128 passed / 0 failed /
  1 ignored**，日志`/tmp/homeway-tui-session-order-notification-tests-final.log`。新增Mac
  route mock在本地/远端均以panic sentinel证明不会调用native，准确单OSC9及取消无输出；
  既有原主机转场/排队归属、过滤、非阻塞背压、helper超时与退出kill/wait测试继续通过。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings`实际 **exit0**，日志
  `/tmp/homeway-tui-session-order-notification-clippy.log`。

所有通知测试使用mock或私有child；App fixture仍用recording sender，没有真实桌面通知/
点击副作用。本轮没有构建安装binary、跑外部网络/生产或全workspace烟囱；私有LIST回归
已直接覆盖本次排序根因，无需额外制造真实应用输出。未修改默认state或用户通知设置。

## 统一代码评审

非实现者 independent_review 最终统一代码门通过，**无高/中/低待修项**。评审者实读
README/backend/notifications diff与最终128/0/1 ignored、clippy完成日志，结论：

- createdMs typed字段、升序/name补齐没有clone key；真实FrameIo反转活动与原始次序
  测试有效。App仍按name回收选择，PID map/恢复与blocked基线没有改变或改成依赖索引。
- Mac在native前短路OSC9，osascript/APPLESCRIPT分支已删除；Linux无action/激活，
  SSH/tailcat仍终端；原16队列、UTF-8限长/C0C1过滤、取消、超时与单次回退保持。
- 新mock无真实通知副作用；README说明不支持协议可能无通知，没有不确定app启动路径，
  不冒称实际桌面点击或宿主精准focus已经验证。无多余共享锁/Arc、接口仿写、无谓拷贝或
  逐行移植痕迹。

该门为独立静态/日志审查，评审者没有自行运行服务或发通知；无待办项。主会话负责
ROADMAP与本地中文单元提交，本子任务未提交、推送、安装、发布或部署。
