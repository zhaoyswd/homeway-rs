# TUI：macOS 自带终端的 blocking 提醒（2026-11-06）

## 根因与范围

用户在 Terminal.app 执行 `homeway tui`，会话转为 blocking 时没有提示。现有 macOS
通知路线无条件输出 OSC9；2026-10-29 为遵守“无法可靠回原会话则不动作、不打开不确定
应用”的约束，已移除会打开 Script Editor/Finder 的 osascript 来源。本轮保留该约束。
历史记录已指出，不能保证 Apple Terminal 的 OSC9 支持，现有路径缺少该宿主的原生提醒。

[Apple 官方高级设置说明](https://support.apple.com/en-md/guide/terminal/trmladvn/mac)
明确 Terminal 收到 BEL（Control-G / `\a`）后，可按描述文件设置声音、闪烁、后台 Dock
图标徽标及跳动。已实际浏览此页面；通用入口首次返回 429，地区入口成功，不将请求失败
冒充成功。该接口提供终端提示，不能据此承诺通知中心横幅或精准点击回 homeway 内部会话。

修复限 CLI 前端：本机 macOS、`TERM_PROGRAM=Apple_Terminal` 精确匹配、且不属于已有
SSH/tailcat 远端识别时，改为单个 BEL；其它 macOS、SSH/tailcat 仍用 OSC9。Linux 本机
原 notify-send 及失败回退保持。未知、空或非 UTF-8 程序名不猜宿主，继续原路线。嵌套
终端可能继承宿主环境，实际效果取决于承载终端，不宣称自动探测所有复用器。

路由用 enum 表达，在 Notifications 启动时捕获；worker、UI 请求拒绝回退、队列溢出
摘要共用该值，不因不同线程重新读取环境而漂移。不叠加 BEL 与 OSC9，不新增 helper、
依赖、点击动作或设置修改。原边沿去重、首次基线、跨主机归属、16 槽队列和取消预算不变。
无 core、wire、既有判据行或行为时间常量变更，未编辑 INTEROP-CRITERIA。

## 独立技术门

非实现者子代理 notification_review 在实现前独立阅读 notifications、clipboard 的远端
识别、App 回退及 10-29 历史约束，并独立浏览 Apple 官方依据。技术门明确通过，无高/中
阻断。建议及处置：

1. 类型表达路线、纯函数选择、启动捕获后 worker/UI 复用：接受，Route 与纯 route 函数
   加 Notifications.route，测试不修改全局环境。
2. remote 优先，Apple_Terminal 精确匹配，未知保持旧路线：接受。
3. BEL 是独立 0x07，不作 OSC 终止符、不叠加协议，取消仍无输出：接受。
4. 文档说明终端描述文件可关闭所有提示，非系统横幅、不保证嵌套/精准点击：接受。

本轮采用 ROADMAP 允许的结构化独立子评审，未调用 dsh/reviewer，不声称外部 dsh 门。

## 实际验证

- 修前回归真实失败：`apple_terminal_receives_one_native_bell_instead_of_osc9` 期望单 BEL，
  旧实现实际返回 OSC9；日志 `/tmp/homeway-tui-mac-notifications-red.log`。
- 最终全 TUI **149 passed、0 failed、1 ignored**，命令实际 exit0；日志
  `/tmp/homeway-tui-mac-notifications-tests.log`。新增三个测试覆盖根因 BEL、宿主/远端
  路由矩阵、真实私有 worker 的输出与回退一致及取消；扩展原 App 队列压力回归，OSC9
  与 BEL 两路均验证 16 条加一条摘要，持续 blocked 不重复。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` 实际 exit0；日志
  `/tmp/homeway-tui-mac-notifications-clippy.log`。
- `cargo build -p homeway-cli --offline` 实际 exit0；日志
  `/tmp/homeway-tui-mac-notifications-build.log`。
- `cargo test -p homeway-cli --offline` 实际 exit0：CLI **231 passed、0 failed、2 ignored**，
  集成 **5 passed、0 failed**；日志 `/tmp/homeway-tui-mac-notifications-cli-tests.log`。
  最后一项私有配置失败回归运行 63.81 秒，保留其超过 60 秒提示和最终通过，不将中间
  四项完成冒充全量结束。

测试仅模拟 sender、内存输出与私有 worker/child，没有实际发送桌面通知、激活窗口或
修改 Terminal 偏好；不能声称 GUI 声音、Dock 或点击已实测。只读核本机 Terminal 默认/
启动描述文件均 Basic，未显式保存 bell/bounce 键，故不能据缺省键断言有效开关。实际
提示需按 README 的“终端 → 设置 → 描述文件 → 高级 → 铃声”选择。

实现中误用 `cargo fmt --all` 造成 158 个无关 Rust 文件格式变化，已仅撤销本轮造成的
这些格式变化；最终 diff 仅 README、TUI notifications/mod 与本记录。没有留下格式批。

## 独立最终代码门

非实现者 notification_review 最终实读 README、notifications、mod 的完整 diff，
核修前回归红、TUI 149/0/1、CLI 231/0/2、集成 5/0、clippy 结束日志及 diff 检查，
代码门明确通过，无高/中/低待修项。

唯一低优先反馈是 README 原首句统一宣称通知包含主机/标题，BEL 无文本；已改成“支持
文本的通知显示…”并保留列表详情说明，评审者复核后终签。路由精确本机 Apple 且远端
优先，未知/非 UTF-8 回原 OSC9；Mac 无 helper，Linux 参数/超时/取消不变。App 两路
队列回归验证 17 个 BEL 或 17 条 OSC，持续 blocked 不重发。未新增多余 Arc/Mutex、
无谓拷贝、字符串错误或逐行移植痕迹。

该门是独立静态及日志审查，评审者未自行运行测试或实际 GUI，不扩大为桌面显示/点击
已验证。

未安装二进制、未触碰生产实例或正式 state、未推送/发版/部署。主会话负责 ROADMAP
与本地中文工作单元提交。
