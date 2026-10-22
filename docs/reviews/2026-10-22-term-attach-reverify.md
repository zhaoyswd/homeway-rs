# surface attach 真实 TUI 与真机复验（2026-10-22）

结论：清洁修复 `f883d7a0fdc9c3220509221eb983a17e16179785` 的真实 TUI/主力真机复验通过，
没有发现需要恢复 surface 人工尺寸哨兵的回归。raw 催重画哨兵继续保留；真实 resize 继续生效。

实现评审见 [原整改记录](2026-10-22-term-surface-attach.md)。App 总报告为
`tier:docs/reviews/terminal-attach-reverify-2026-10-22.md`。

## 第一道门：技术结构化自评（执行前）

| 检查 | 实现者方案 / 评审者结论 |
|---|---|
| 范围与授权 | 复验同尺寸 surface 不再人工减一列后的真实 TUI、异尺寸与退出 shell 历史；只用私有出口，不发布、推送或换装生产。 |
| 对齐完整性 | Codex 0.163.0 默认备用屏、`--no-alt-screen` 与 opencode 2.1.2 `--standalone` 实际启动；只空闲、分离、重进、resize、Ctrl-C 退出，不发送任务或对话。 |
| 内容判据 | 解码 surface v5/cell v2 的完整快照、镜像与 FETCH_ROWS；比较字素、width/skip、前景/背景、属性、wraps、epoch/base。不能以帧数量、出现任意输出或截图预览代替内容完整性。 |
| 行为边界 | 默认备用屏内容不得混入主屏；inline 合法输出保留。区分程序响应真实 SIGWINCH 的清屏行为与服务端人工尺寸污染。 |
| 并发与错误面 | 独立会话、会话名隔离；检查 gzip 分片、体字节精确耗尽、行列宽与宽字尾格。双腿另设接收器真实应用 snapshot/diff，不能主动补全量掩盖漏差分。 |
| 与基线漂移 | 新版私有 73 与官方旧版二进制私有 74 对照；两者均显式临时 state/独立端口。旧生产进程与其 state 不作为测试对象。 |

技术门：通过。宿主、真机与原图像素证据分别采集，不把某一层通过冒充所有层验收。

## 第二道门：结果与代码结构化自评（执行后）

| 检查 | 证据与结果 |
|---|---|
| 宿主同尺寸 | 三种 TUI 各覆盖 80 列运行中、38 列运行中、退出 shell 后 80 列重进，合计 **9 组**；完整 grid、mirror、historyEpoch、historyBase 逐项相等。 |
| 宿主跨尺寸 | 默认 Codex/opencode 在 80×24→38×24→80×24 后欢迎与输入 UI 完整，返回 80 列的完整 grid 与初始逐格相等。另测 80×24→49×33→49×34→38×34→49×34→80×24。 |
| 宿主 shell 历史 | 默认 Codex/opencode 退出后各 **35 条**唯一 shell 种子全在，备用屏帮助/输入内容未进入主屏。三种模式 shell 80→38→80 的完整 FETCH_ROWS 按 wraps 还原后，逻辑行逐字相等。 |
| inline 输出 | inline 退出后合法 Codex 欢迎、指令、退出说明与 shell 提示符保留。运行中真实 resize 会清除此前 shell 回滚；独立 pty.fork 原始捕获显示 38/80 两轮各一次 ESC[3J 与 ESC[2J，属于 Codex 自身行为，不能据此按进程名删历史。 |
| 焦点/主题与旧版对照 | 同长 cwd、绝对 Codex 命令，在 49×33 空闲 40 秒后发送 INPUT focus out→断开→49×34 HELLO→THEME→focus in；新版 73 与旧版 74 最终 grid 逐格相等。 |
| 双 surface 隔离 | B 同尺寸 HELLO→FETCH_SNAPSHOT→close 连续 **20 轮**，A 仅收到最初 1 条 snapshot、后续 0 条 diff，完整 grid 仍等于权威 VT。动态输出期间 B 请求 **80 次**快照，A 收到 85 条更新、seq 连续，最终 grid 正确；跨尺寸/退出 shell 同样对拍通过。 |
| C++ 实际解码到绘制输入 | 真实 wire 喂本仓 surface_codec/scroll/selection/render_bridge；80×24、49×33、49×34、38×34 均 0 拒收。模型非空/桥后 textLen 分别 421/421、408/408、408/408、392/392；顶部 20 行始终 314/314，hidden=0、offset=0。 |
| Rust 只读复核 | 每会话单 surface_loop；flush 先复制共享 dirty，再逐腿生成快照/差分，snapshot commit 不会独占消费其他腿更新；FETCH_SNAPSHOT 只标请求腿。未发现此次复验要求修改源码的缺陷。 |
| Rust / 直译痕迹 | 本轮无 Rust 源码变化，没有新增 Arc/Mutex、字符串错误、接口仿写、无谓复制或包映射；Python/C++ 探针仅在临时目录。 |
| 证据纠错 | 图片预览一度被读成“顶部漏画”；原始 JPEG 的独立像素复核证伪。撤回 probe/GL 漏画推断，不据此扩大产品修改。详见下节。 |

代码/结果门：通过。已有源码验证结果见原整改记录；本次文档收口未新增源码，不重跑 cargo 全门。

## 主力真机与私有对照验收

主会话在 `FMR0224116011480` 安装清洁 `f883d7a` 核与正常 debug 三包，使用新版私有实例 73，
并以官方旧版二进制的私有实例 74 做 A/B；生产出口没有换装。

- Codex 默认/opencode 运行中同尺寸重进通过，捏合 49→45 列与再次重进通过，退出到 shell 后历史保持。
- 默认 Codex 退出后 **45 条长逻辑行全部逐字保留**；退出→重进的 FETCH **100 个物理行逐格相等**。
- App opencode 49×34 运行中同尺寸重进，完整 grid、mirror、epoch 相等。
- 临时 RVPROOF 计数日志已全部从源码撤销，正常 debug 三包已重新构建并安装；未将诊断插桩留在交付包。
- 临时 App 主机已删除，原主机与 VPN 直连恢复；73/74 两个私有实例已停止，生产出口未重启或换装。

## 原始图片像素纠错

主会话先发现、子任务独立用 PIL/NumPy 确认：以下四组原始 JPEG 均为 1260×2720，
终端顶部区域 `x=0..1260,y=305..1650` 逐 RGB 通道完全相等，**差异像素 0、最大通道差 0**：

- `phone-codex-running.jpeg` / `phone-codex-reopened.jpeg`
- `reference-settled-reopen.jpeg` / `reference-after-probe.jpeg`
- `final-no-probe-first.jpeg` / `final-no-probe-reopen.jpeg`
- `proof-first.jpeg` / `proof-reopen.jpeg`

八张该区域原始 RGB 字节 SHA256 相同：
`b0087303e6e3633ab40d307e0916b651604fb5b52d790d63f799ccc64e746495`。
截图预览中的“顶部空白”印象不符合文件像素；未发现原图顶部缺字差异，不归因为 App 或 GL 缺陷。

## 证据位置与限制

临时宿主证据目录 `/private/tmp/homeway-tui-surface-evidence/`：
`host-summary.json`、三模式同尺寸 before/after JSON、三模式 shell wide/narrow/restored-history JSON、
`host-phone-geometry-*`、`host-phone-delay-*`、`host-reference-*`、`dual-*`、`cpp-wire-*`、
`direct-pty-inline-{narrow,wide}.bin`、`independent-image-pixel-check.json`。
主会话真机证据目录 `/tmp/tier-term-reverify-evidence/`，跨仓总报告见文首 tier 路径。

临时探针位于 `/private/tmp/homeway-tui-surface-probe.py`、`homeway-codex-*-probe.py`、
`homeway-dual-{surface,close}-probe.py`、`homeway-cpp-wire-capture.py`、`homeway-surface-cpp-read.cpp`。
最初交互探针在 KILL 后将正常 EOF 打成异常，断言与证据已先落盘；随后修正收尾，不把该退出码当通过判据。
清洁私有二进制 SHA256 为 `e6df6e698fedbc0ef42947162061d00f04bb2ffbe9406c83f80e6219e6e47870`。

未执行真实模型任务，故不新增复杂长任务 transcript 的保证；已损坏的旧历史无法从此次复验推断会自动还原。
生产修复仍待明确部署后经 Release 链路换装；本次验收没有发布或推送。
