## Why

多台主机（家里出口 + 本机）上的终端会话与 AI agent 正在成为日常工作面，目前只有两个触点：手机 App（单主机视角）和逐会话 `term attach`。缺一个桌面端全局工作台：不切窗口就知道哪台主机、哪些会话、哪个 agent 在等输入。出口侧的多腿同显、LIST、agent 状态识别（`term/agent.rs` + manifest）已建成，本变更是把这套能力接给桌面终端前端，属补拼图而非开新机制。

## What Changes

- 新增 `homeway tui` 子命令：ratatui 全屏面板，四区布局——
  - 左上：主机列表，**含本机条目**（远端主机来自 daemon HostTable，本机为 serve 直连特殊条目）；
  - 左下：当前选中主机的终端会话列表，每条带 agent 状态徽章（working/blocked/done/idle，语义与 App 一致，同一 `Op::LIST` 真源）；
  - 右侧：当前选中会话的终端视图，**可交互**（crossterm 键鼠 → keyenc → INPUT 帧）；
  - 底部状态行：当前主机的连接状态、延迟、流量。
- **全局唯一**：选中状态本机共享（落 `<state>/cache/` 选中文件，实例间文件 watch 镜像）；多个 `homeway tui` 实例同开显示相同内容（选中与高亮镜像一致，右侧面板各实例自开观察腿渲染同一会话）。
- 右侧终端采用 raw 腿（**不声明** capsRawTerminal 位）+ 内嵌 alacritty_terminal 0.26 渲染管线；输入经客户端 keyenc 编码走 DATA 字节帧（与 `term attach` 同路）；断线恢复走既有回放环 + 尺寸哨兵路径，并按「链路中断/会话终止/被腾位」三态分别处理。
- 零协议与契约面改动：term 帧协议、QUIC 承载、capi/App 面、tier 对齐面、**daemon 控制面词表（跨仓冻结契约）**全部不动；core 仅一处可见性公开（keyenc kitty 模式位访问器）。
- 本期非目标：面板外提醒（系统通知/响铃）、herdr 式 Socket API 反向开放、会话元数据服务端推送（LIST 轮询差分替代）、延迟实时化（既有端到端探活口径 60s 拍如实显示）。

## Capabilities

### New Capabilities

- `tui-panel`: `homeway tui` 面板的完整外部行为——四区布局与导航、主机列表（含本机条目语义）、会话列表与 agent 徽章、终端视图的交互与断线恢复表现、状态行内容、多实例同显语义。

### Modified Capabilities

（无——不改任何既有能力的需求；daemon 新增的本地控制面 op 作为 `tui-panel` 的支撑面在 design 中描述，其外部可观察行为即「第二实例看到同一选中」，归入 `tui-panel` 需求。）

## Impact

- **代码**：`crates/homeway-cli`（`tui` 子命令 + 面板模块，新依赖 ratatui + crossterm）；`crates/homeway-core` 仅一处可见性公开（keyenc kitty 模式位访问器，无行为/wire 变化）。**daemon 控制面零改动**（技术评审 D-1 定案 A 案：跨仓冻结词表不走 delta，全局唯一改走本机选中文件）。
- **依赖**：新增 ratatui + crossterm；alacritty_terminal 0.26 走 workspace 继承；homeway-cli 须过 OHOS/musl×2 三目标交叉 check（CI 既有面）。
- **不动**：term 帧协议、QUIC 线协议、`homeway-capi`、tier 消费面、daemon 词表/事件域/响应字段。
- **流程义务**：新功能线，ROADMAP.md 登记立线；技术评审记录 = `docs/reviews/TUI-design.md`（2026-10-11，31 条意见已闭环处置）；代码评审门在实施完工后执行。若实施中发现触碰判据行（不预期），按 `docs/INTEROP-CRITERIA.md` 登记并停下评审。
