# TUI 立线技术评审（add-tui-panel 设计门）

- 日期：2026-10-11
- 评审对象：openspec change `add-tui-panel` 四工件（proposal / specs/tui-panel / design / tasks），规划工件，无代码
- 评审者：dsh --profile headless（外部独立评审，无会话语境），目录 /tmp/dsh-review/r1.LHEtPQ（output.md 全文存档）
- 结论：31 条意见（高 4 / 中 19 / 低 8）+ 12 组代码事实抽查（9 组成立、3 组部分成立，无虚构）+ 12 项「看过没问题」明确声明
- 处置：**31/31 全部采纳**（1 条部分采纳，见 R-2 处置注）。本记录为技术评审门（ROADMAP 评审协议）的 r1 存档。

## 定案决策（本评审最大产出）

**D-1 → A 案：控制面零新增。** 评审证实 control.sock 词表是跨仓冻结契约（`daemon/vocab.rs:1-4`「只增不改，新增走后续 spec delta」；tier daemon-control-plane 钉死订阅域初始集；`daemon/tests.rs:43` 断言冻结夹具恰 43 帧），新增 `tui.state.get/set` op + `tui` 事件域是对只读跨仓 spec 的 delta，需 tier 侧登记（用户触点）。定案：

- 全局唯一改走 **`<state>/cache/tui-selection.json` 选中文件 + 实例文件 watch**（revision 单调、last-writer-wins、损坏回落）；落 cache/ 免 export/import 白名单联动（D-9 连带消解）。
- **放弃改动①（rtt 实时化）**：`IslandSnapshot.rtt_ms`（quinn 平滑 RTT）在中继形态下量的是客户端↔中继，直接替换既有 `rttMs`（端到端探活、App 面 tun_status 同源）会静默改变值语义（D-3）；状态行如实显示既有口径并标注新鲜度。
- **放弃改动②（serveDir 字段）**：`serve_dir = state_dir.join("serve")` 恒定，TUI 既已 dial `<state>/control.sock` 必然知道 `<state>`（D-8）。

结果：daemon 侧零改动，core 仅一处可见性公开（D-5）。B 案（走 additive delta）留档于 design D6 备选。

## 意见与处置对照

### ① 需求（R-1～R-10）

| 条 | 摘要 | 严重度 | 处置 |
|---|---|---|---|
| R-1 | 焦点模型未定义且不可测；Alt/Fn 与常见 TUI 应用冲突；鼠标/粘贴归属空白 | 高 | **采纳**。spec 重写为双焦点模式需求（面板导航焦点/终端焦点，终端焦点除单一退出组合键外全透传），补 3 场景含鼠标归属（spec「双焦点模式与输入归属」）；design D8 定案 |
| R-2 | 8 腿满腾位对被动观察腿不利（note_activity 仅 DATA/RESIZE/INPUT 触发）；「不挤占」不可单方保证；心跳保活有害 | 高 | **采纳（措辞部分调整）**。spec 新增「面板腿占用与腾位边界」需求：接入前 LIST clients 检查、腿满不接入（不触发服务端腾位）、被腾位呈长退避提示不抢占；design D2 删心跳保活。注：面板控制不了被别人腾位，能承诺的是不抢占与明确呈现——spec 按「面板可控行为」收窄 |
| R-3 | `done` 徽章真源不存在（stateV2 四态；done 仅 OSC 21337 输入词映射 IDLE）；无 agent 判定未定义 | 中 | **采纳**。spec 钉 agent∈{shell,unknown}→无 agent，否则 stateV2 四态 |
| R-4 | LIST 顺序为 HashMap 迭代无定义，排序契约不可测，索引选中会跳 | 中 | **采纳**。spec 钉显示排序 lastActiveMs 降序+名破平，选中以会话名为键 |
| R-5 | 断线三态（终止/腾位/链路）混一，kill 会话→无限重拨 | 中 | **采纳**。spec 新增三态恢复需求；design D2 三态判定 |
| R-6 | daemon 重启（generation 失配）无需求；status --watch 的 exit(1) 不适用于面板 | 中 | **采纳**。spec 新增控制面连接恢复需求（自动重连+全量重快照）；tasks 3.1 |
| R-7 | 空态/选中失效回落缺失 | 中 | **采纳**。spec 新增「选中状态与回落」需求（回落链：会话→同主机最近活跃→第一可用主机→空态） |
| R-8 | 本机可用性判据（HOMEWAY_TERM=off 反例）、serve.status 真状态域、local 形态标签缺失 | 中 | **采纳**。spec 改「socket 可连」判据 + local 标签 + 占位规则；tasks 3.2 修 |
| R-9 | 非 TTY 拒绝、任何退出路径还原终端状态缺失 | 中 | **采纳**。spec 新增「终端环境边界」需求；tasks 4.7 |
| R-10 | spec 夹带实现措辞/不可测措辞 | 低 | **采纳**。重写时全部改为可观察契约 |

### ② 方案（D-1～D-14）

| 条 | 摘要 | 严重度 | 处置 |
|---|---|---|---|
| D-1 | 控制面词表跨仓冻结，新增 op/域是对只读 spec 的 delta，proposal 却声明「零协议改动」 | 高 | **采纳 → A 案定案**（见上）；Modified Capabilities 维持「无」的前提改为「零契约面改动」 |
| D-2 | 腾位误述 + 心跳保活有害 + 重拨自激 | 高 | **采纳**。design D2 重写（三态判定、腿满检查、删心跳） |
| D-3 | rttMs 语义替换（中继形态量的是客户端↔中继）静默改既有字段含义 | 中偏高 | **采纳**。放弃改动①，状态行用既有端到端口径（design D4） |
| D-4 | 输入通路矛盾：raw 腿输入是 DATA 裸字节（客户端编码），INPUT 是 surface 家族服务端编码帧 | 中 | **采纳**（已复核 service.rs:1501 起 / term_cli.rs:1724）。design D2/tasks 3.5 定为 keyenc→DATA |
| D-5 | KF_*/kitty_flags_of 私有，客户端编码无公开取法 | 中 | **采纳**。design D7：core 仅可见性公开；tasks 2.1 |
| D-6 | 渲染节拍/通道背压缺失（固定帧率违空闲唤醒纪律；丢字节=屏幕损坏） | 中 | **采纳**。design D1 补事件驱动 draw + 有界不丢字节 + 合帧；tasks 3.7 |
| D-7 | events.subscribe 域校验整体拒绝（老 daemon 代际偏斜） | 中 | **采纳（随 A 案消解）**。不新增订阅域；design D4 注明 |
| D-8 | serveDir 非最小面且理由不成立（客户端必已知 state 目录） | 中 | **采纳**。删改动②；design D5/tasks 3.2 改直算路径 |
| D-9 | tui-state.json 不在 CLIENT_PIECES 白名单（导入直接拒） | 中 | **采纳（随 A 案消解）**。选中文件落 cache/ 免白名单；design D6 记理由 |
| D-10 | LIST 轮询成本账缺量化 | 中 | **采纳**。自适应拍（前台 2s/空闲 10s）+ tasks 5.5 量化验收 |
| D-11 | homeway-cli 被 CI/本地门交叉 check 三目标，ratatui/crossterm 面未论证 | 中 | **采纳**。tasks 1.3 前置三目标 check；失败即 cfg 门控 |
| D-12 | 引用行号/措辞小偏差（让位规则只由 RAW_TERMINAL 决定；哨兵位置；LIST JSON 形状位置；stats 漏 speedtest；回退说法过强） | 低 | **采纳**。design 重写时勘误 |
| D-13 | 多实例尺寸互踩 | 低 | **采纳**。design D2：仅交互实例 RESIZE，镜像实例 NEVER；tasks 3.5 断言 |
| D-14 | term_cli 本地 dial 路径约定陷阱（其 --state 指 serve 子目录） | 低 | **采纳**。design D5/tasks 3.2 注记不复用 |

### ③ 任务（T-1～T-7）

| 条 | 摘要 | 严重度 | 处置 |
|---|---|---|---|
| T-1 | 缺评审两道门（硬规则 5） | 高 | **采纳**。tasks 5.8 代码评审门；本记录即技术评审门 r1 |
| T-2 | `serve status --json` 验证命令不存在 | 中 | **采纳**。随 D-8 删 serveDir，任务消失 |
| T-3 | LIST 夹具不存在（冻结夹具有恰数断言） | 中 | **采纳**。tasks 3.3 改内联 JSON 字样 |
| T-4 | 漏项：白名单/交叉 check/腾位验证/成本量化/终端恢复等 | 中 | **采纳**。分别落 tasks 5.5/5.7/5.2/4.7 等 |
| T-5 | 「App 等价 surface 客户端」仓内不存在 | 中 | **采纳**。tasks 5.4 改为 term attach + 8 腿满构造 |
| T-6 | 手测不可复现 | 中 | **采纳**。tasks 5.6 smoke 脚本 + 清单落盘 |
| T-7 | workspace 依赖继承；3.4 拆 a/b | 低 | **采纳**。tasks 1.2/3.4a/3.4b |

## 遗留与下一步

- 工件已按上述处置整体修订（spec 13 需求 / design 9 决策 / tasks 5 组 24 条）。
- 建议工件修订后、开工前走第 2 轮评审复核三项高危（R-1 焦点模型、R-2/D-2 腿边界、D-1 A 案）是否消解。
- 实施完工后走代码评审门（含 Go 直译痕迹 checklist），记录续入本文件。
