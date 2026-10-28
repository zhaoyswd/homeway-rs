# TUI blocking 状态通知（2026-10-28）

用户要求会话 blocking 时主动通知。实现限 CLI 前端，不改 core、服务端、RPC、wire，
不扩展为所有主机后台订阅，也不改已有行内状态、剪贴板和操作提示。

## 行为与边界

- 仍按原有周期轮询当前选中主机。只有通过现有 Target/epoch 校验的成功完整会话列表
  才进入比较；首次成功（包括空列表）建立该主机基线，不补历史 blocking。
- 后续从其它状态进入 Blocked、新出现且已 Blocked 的会话提醒一次，持续 Blocked 不重复，
  恢复后重入可再提醒。同名且正 PID 改变视作新会话。状态按稳定 Target/会话名保存，
  切换主机不会重建基线；失败不清状态，成功缺失会话和成功快照移除主机时及时回收。
- 通知标题为“会话需要操作”，正文在转场时捕获原主机名与会话标题（空标题回退名称）。
  它报告已观察到的转场，不按输出时的新选择重构正文，不覆盖行内消息或操作归属。
- macOS 用固定 AppleScript 的 `display notification`，内容只经 argv 传递；Linux 用
  `notify-send --`。SSH 或 `TERM_SESSION_ID=tailcat-…` 优先单个 OSC9 请求承载终端，
  复用剪贴板原有远程识别。工具失败也单次回退 OSC9，不叠加 BEL 或其它通知协议。
  只保证提交请求，实际显示取决于系统或终端支持及设置，不宣称 OS 确认送达。
- 单 worker、请求/结果队列各16，helper 2秒超时，20ms取消检查。退出设置必要的
  AtomicBool 后 join，不排空积压请求；正在运行的 child 会 kill/wait，结果队列满也能取消。
  UI 只输出受控返回序列。提交拒绝时用有界终端回退，最多16条加一条超额数量摘要，
  避免丢弃该次边沿或无限重试。文本按 UTF-8 边界限长，C0/C1过滤，不能注入 OSC。

官方接口依据：[Apple 通知命令](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/DisplayNotifications.html)、
[iTerm2 OSC9](https://iterm2.com/documentation-escape-codes.html)。未安装依赖、修改终端偏好或通知设置。

## 分工与独立两道门

主实现者负责 App 基线/转场、UI 回退与回归；notification_sender 负责 notifications.rs 和
clipboard 最小环境检测提取。第三个非作者 notification_review 先审设计后实读两模块，
技术门/代码门通过；sender 作者另对 App 独立只读通过，作者没有自签自己的模块。

审查覆盖首次空基线、错误/陈旧回包、跨主机、删除回收、PID重建、队列压力不吞边沿、
原操作消息隔离、固定 argv、控制过滤、helper超时/退出与结果背压。Arc只用于跨线程
取消，没有新增锁或逐行移植式结构。所有自动测试的 App fixture 统一使用录制 sender，
不调用真实系统通知工具；测试不会写真实桌面剪贴板或访问正式 state。

## 验证

实现者已运行 rustfmt、`git diff --check`，均 exit0；未运行 cargo、真实通知或部署。
沿用现有测试，新增 App 真实 Sessions 事件归属/边沿回归、受控录制、队列拒绝回退，
sender 使用模拟提交和私有 child 验证 argv、OSC9、非阻塞背压、超时与取消回收。
以下实际编译、TUI回归、build/clippy及私有交互均由主会话执行。

主会话首轮 `cargo check -p homeway-cli --offline` 实际 exit0（3.08s）。四处纯测试
fixture改为 `std::slice::from_ref` 避免无谓clone后，非作者另行实读终签，生产代码未变。

主会话全TUI实际 exit0：**114 passed、0 failed、1 ignored**，日志
`/tmp/homeway-tui-blocking-notifications-tests.log`。自动测试仅使用模拟通知和私有 helper，
没有发送真实系统通知。新增 App 3项及sender 6项回归均在此轮通过。

`cargo build -p homeway-cli --offline` 实际exit0（0.29s）；
`cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` 实际exit0（2.21s）。
原本机私有交互烟囱实际exit0、**23 PASS**，日志
`/tmp/homeway-tui-blocking-notifications-smoke.log`，空闲CPU 0.75%，所有脚本自有进程收回。
该烟囱验证原会话/键鼠/布局交互，没有驱动真实blocking或观察系统通知弹窗；新通知路径由
上述真实App事件至mock sender、受控OSC和私有helper回归覆盖，不扩大声称桌面接收已验证。

为验证真实AppleScript接口，主会话将生产常量原文提取到
`/tmp/homeway-tui-blocking-notification.applescript`（完整SHA256
`b048d11952abeae6ca00fffe63b87c6d1cfcf10f9efea0bb39a2a46db2829985`）。
默认沙箱中 `osacompile` 实际exit1，提示第2行 Expected end of line but found identifier。
随后同一原文在授权的沙箱外用系统 `/usr/bin/osacompile` 实际exit0，确认是系统脚本接口
访问限制而非产品语法错误；没有修改脚本或把失败静默记为通过。此步骤只编译、不执行，
不把编译成功冒充真实桌面通知已显示。

## 本机 debug 换件（完成）

最小适配器 `/tmp/tui-blocking-notifications-install.py`，SHA256
`baa05d9f90c6be5c54870d437c8ff3398d96c0164a8c2c9066c128aec3187ee5`。
相对已审 command-shortcut 适配器只改 OLD_SHA 为完整
`3e0e1cbc590d7cebb6526ff07c3dc7d00df9394467649294cff2f75b7fd7f071` 和备份前缀。
锁、私有备份、签名/fsync、原子换退与原零参守护 PID 不变要求沿用，非作者独立增量门通过。

最终工件完整SHA256为
`1390e0ae3a93fb100f58237cfef1febea29f08da817e93b49a28cae6fc4dd59b`，烟囱运行前后一致。
主会话实际执行适配器，exit0、回执 **SUCCESS**，PATH同该工件SHA，PID **55632→55632**，
没有重启、回滚或访问正式state。旧件与回执保留于
`/Users/zhaozhe/homeway-rollback/tui-blocking-notifications-20261028-ae00ba040f78/`。
独立核验PATH与target/debug完整SHA相等、严格codesign exit0；ps确认为PPID1、UID501、
零参`/opt/homebrew/bin/homeway`。重新进入TUI生效，通知实际可见仍依赖OS或终端设置。
LAN71 本轮未尝试或部署，不推送或发布。
