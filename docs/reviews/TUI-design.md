# TUI 立线技术评审（add-tui-panel 设计门）

- 日期：2026-10-11
- 评审对象：openspec change `add-tui-panel` 四工件（proposal / specs/tui-panel / design / tasks），规划工件，无代码
- 评审者：dsh --profile headless（外部独立评审，无会话语境），目录 /tmp/dsh-review/r1.LHEtPQ（output.md 全文存档）
- 结论：31 条意见（高 5 / 中 21 / 低 5）+ 12 组代码事实抽查（9 组成立、3 组部分成立，无虚构）+ 12 项「看过没问题」明确声明
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
| D-5 | KF_*/kitty_flags_of 私有，客户端编码无公开取法 | 中 | **采纳 → r2 N-3 推翻本处置前提**：`SessionVt` 本就是公开类型且 `key_options()` 提供完整编码口径，正解 = 客户端模拟器复用 core 公开的 `SessionVt`，**core 零改动**（见 r2 节；tasks 1.2 编译探针前置确认） |
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

- 工件已按上述处置整体修订（spec 13 需求 / design 9 决策 / tasks 5 组 29 条）。
- 建议工件修订后、开工前走第 2 轮评审复核三项高危（R-1 焦点模型、R-2/D-2 腿边界、D-1 A 案）是否消解。
- 实施完工后走代码评审门（含 Go 直译痕迹 checklist），记录续入本文件。

---

# 第 2 轮评审（r2，2026-10-11）

- 评审者：dsh --profile headless，目录 /tmp/dsh-review/r2.sv1QEi（output.md 全文存档）
- 范围：4cd461e..ebd53a7（r1 修订后的四工件 + 本记录）；带 r1→r2 增量上下文
- 方法：对照 r1 原文逐条复核 + `openspec validate --strict`（valid）+ 对修订依赖的代码事实逐条重验（clients[] 字段、SessionVt 公开面、注册/腾位序、artifact 白名单、hosts 顺序等）
- 结论：**14 条新发现/未消解**（高 2〔N-1/N-4〕、中 7、低 5）+ 三项高危消解判定（R-1 主要消解有残余、R-2/D-2 行为侧消解判定侧未消解、D-1 定案消解传播不彻底）+ r1 处置复核（**已消解 23 / 部分消解 6 / 未消解 1**〔D-5 → N-3〕）+ r1「没问题」12 项**无一被修订破坏**
- 处置：**14/14 全部采纳**，工件再次整体修订（下表）；其中 N-3 为修正机会——D-5 的 r1 处置前提有误，正解 = 客户端模拟器直接用 core 公开的 `SessionVt`（出口同款类型，`key_options()` 即模式真源），**core 恢复真零改动**（编译探针前置确认，tasks 1.2）

| 条 | 摘要 | 严重度 | 处置 |
|---|---|---|---|
| N-1 | 「被腾位」判定依据不存在（clients[] 无腿标识；断连后自身腿恒不在；裸断与链路断流层同形）→ 需求 5/6 不可验收 | 高 | **采纳**。spec 断线语义重写为可判定谓词四分支（会话不在 LIST ⇒ 终止；链路离去/形态不可用 ⇒ 链路中断；腿满 ⇒ 让位边界；其余 ⇒ 按链路中断），占用方信息按可得字段；tasks 3.4b 改 mock link/session 驱动 |
| N-2 | 「不触发服务端腾位」TOCTOU 过承诺；残留腿自我阻塞 | 中 | **采纳**。spec 收窄「检查为满 MUST NOT 接入」+ 稳定 client_id 同实例替换 + 残留腿不硬接；TOCTOU 残余竞态登记 design Risks |
| N-3 | D-5 前提错误：SessionVt/key_options/encode_* 全公开，零 core 改动即可；裸 alacritty 路线缺模式面 | 中 | **采纳**。design D2 改「模拟器 = core SessionVt」；tasks 1.2 编译探针前置、2.1 改条件任务（预期跳过）；proposal Impact 改 core 零改动 |
| N-4 | attach 即尺寸/活动腿事件，HELLO 尺寸对齐缺失 → 与「既有客户端行为不变」冲突；「是否活动腿」无从实现 | 高 | **采纳**。spec 既有客户端需求重写（接入以当前会话尺寸对齐、SHALL NOT 改变会话几何；登记副作用 = 活动腿切换 + 剪贴板读缓存重置）；design D2：HELLO 尺寸取 LIST cols/rows，交互实例首次交互后才 RESIZE；tasks 3.4a/5.4 加对齐断言与 surface 腿断言 |
| N-5 | 选中文件并发/异常语义（revision 撞号、半截 JSON、cache 可弃、mtime 漏读） | 中 | **采纳**。design D6：临时文件 + rename 原子写、内容指纹去重、运行期坏读忽略仅启动回落、reset cache 语义注记 |
| N-6 | 回落写回互相拉扯；「第一个可用主机」无确定序 | 中 | **采纳**。spec：回落不写回（仅用户切换持久化）；主机确定序（本机优先 + 名称升序同名按加入先后） |
| N-7 | 3 秒门槛与 10s 空闲拍不相容；「失焦」无定义 | 中 | **采纳**。spec：3 秒限定活动态（最近 30s 有输入或外层焦点在面板），非活动态豁免 + 恢复后 3s 刷新；design D3 拍长对齐 |
| N-8 | 粘贴（2004）与焦点事件（1004）归属空白 | 中 | **采纳**。spec 需求 4 补两条 scenario；design D7 注外层 1004 启用归属；tasks 3.5/4.1/4.4 单测 |
| N-9 | proposal 三处漏改（done 徽章 / INPUT 帧 / Modified Capabilities 括注与 A 案冲突） | 中 | **采纳**。三处已改 |
| N-10 | 新鲜度/长退避不可测 | 低 | **采纳**。spec：rtt 采样年龄 >120s 标陈旧；让位退避 10–30s 且腿满期不重附 |
| N-11 | 交叉引用与计数错位（design 1.4→1.3、tasks 5.7→5.6、本记录高/中/低与 tasks 条数） | 低 | **采纳**。已订正（本节同批） |
| N-12 | via 值域含 none；serve.status 五态；design 未明示不复刻 watch exit(1) | 低 | **采纳**。spec 状态行补占位规则；tasks 3.2 五态归不可用；design D4/tasks 3.1 明示重连替代 |
| N-13 | 5.5 无阈值；5.4 间接验证看不到几何/快照面 | 低 | **采纳**。tasks 5.5 数值门（CPU/ctx/流上限，基线校准定稿）；5.4 加 core 面 surface 腿断言 |
| N-14 | SIGKILL 不可拦截；镜像作用域 | 低 | **采纳**。spec：可捕获退出路径枚举 + 强制终止排除；多实例限定同一状态目录 |

- r2 后状态：spec 13 需求（新增粘贴/焦点、残留腿不硬接、流断未满、采样陈旧、回落不写回、接入不触发几何变化等场景）；design 8 决策（D2 重写为 SessionVt + 接入对齐 + 三态谓词）；tasks 5 组 30 条（r3 J 订正：1.x5 + 2.x1 + 3.x8 + 4.x7 + 5.x9）。
- 下一步：r3 复核 14 条是否消解、修订有无引入新矛盾；通过则进入 `/opsx:apply`。

---

# 第 3 轮评审（r3，2026-10-11）

- 评审者：dsh --profile headless，目录 /tmp/dsh-review/r3.p8GdQv（output.md 全文存档）
- 范围：ebd53a7..bdef0ab；对照 r1/r2 原文逐条复核 + `openspec validate --strict`（valid）+ 代码事实重验（HELLO 尾随块编码、注册/腾位序、SessionVt 公开面、`HostLink.at` 节拍、stream.open 的 QUIC 流语义、首腿 focus 注入）
- 结论：**r2 十四条 = 11 条完全消解、3 条消解有残余（N-1/N-2/N-3）**；两条高经代码级复核实质消除（N-4 几何面：HELLO 尺寸=reg.size ⇒ note_activity 返 None ⇒ 无 setsize/哨兵/全量快照，session.rs:570-583 + service.rs:1203）；r1 的 23 条已消解项与 r2 的 12 项「没问题」**无一回退**；唯一未闭合子项 = r1 R-2/D-2 的「重拨自激」节拍侧（→ 本轮 F）。
- **开工就绪度：可进入 /opsx:apply，无高危阻断**；建议开工前 10 分钟级小修订（A/B/N/D 四条 + C 口径）。
- 新发现 14 条（中 6 / 低 8），**全部采纳**：

| 条 | 摘要 | 严重度 | 处置 |
|---|---|---|---|
| A | proposal:14 core 口径第 4 处漏改（与 :29/design/r2 定案互斥） | 中 | 已改「core 零改动」；本记录 r1 D-5 行加勘误指针 |
| B | 「腿满」判据不可观察（上限是 serve 进程内配置，不在任何 JSON/RPC 面；满员注册先腾位无法试探） | 中 | spec 需求 5 钉死：缺省上限 8 为预检查口径，服务端覆盖为已知限制，服务端接入拒绝为权威兜底；tasks 3.4a 断言写死来源 |
| C | 5.5 数值门算错（6/min 是 10s 拍的数；快照拍走 control.sock 不产生 QUIC 流；LIST 每拍一条新流 ⇒ 2s 拍应为 30/min） | 中 | tasks 5.5 重写：活动态 ≤35/min、非活动 ≤8/min、快照拍移出 QUIC 预算 |
| D | 需求 3（非活动态豁免）与需求 9（1s 镜像含列表/视图）互斥 | 中 | spec 需求 9 补「选中变化（含镜像）计为活动态触发：立即重取 LIST/重附，不受非活动态拍长限制」 |
| E | 「腿满⇒被让位」误判语义（瞬断未回收也显示满）+ 恢复代价 10–30s | 中低 | 文案改「会话观察腿已满，等待空位」（态而非因）；design Risks 登记瞬断+满员恢复延迟 |
| F | 分支②/④自动重拨无节拍 ⇒ r1「重拨自激」节拍侧未闭合 | 中 | spec：重拨间隔 0.5s 起指数至 10s 上限、链路恢复重置 + 新 scenario；tasks 3.4b 加间隔下界断言 |
| G | `enc_hello_tail` caps_present=false 静默丢 client_id（frames.rs:350-368「带 ID 必带 caps 块」） | 低 | design D2 补「尾随块 SHALL 携带全零 caps 块」；tasks 3.4a 断言改「服务端观察到同 client_id 走同实例替换」 |
| H | 「副作用仅限两项」漏首腿 focus-in 注入（service.rs:1233/2618） | 低 | spec 补第三项（仅会话无其它腿时发生，既有语义） |
| I | 「对齐恒为 no-op」有 LIST 时效竞态 | 低 | 措辞收窄「以最近一次 LIST 快照对齐」；design Risks 登记 |
| J | 记录任务数 stale（30 非 29） | 低 | 已订正 |
| K | 术语笔误（三态→四分支三结果；重拨；非 running 四态） | 低 | 已订正 |
| L | 外层 2004 启用归属；导航焦点期间会话焦点归属未定义 | 低 | design D7 补外层 2004/1004 归属；spec 补「导航焦点期间会话按失焦处理」scenario |
| M | 「不受支持的终端」无判据 | 低 | spec 钉判据（TERM 缺失/dumb/初始化失败） |
| N | 「主机不可用⇒回落」与「链路中断⇒留守重拨」触发面冲突 | 中 | spec 需求 10：主机回落仅在主机从表消失或不可恢复终止态时发生；可恢复异常留守 + 新 scenario；tasks 4.6 分支断言 |

- r3 后状态：spec 13 需求 18 场景组（新增重拨紧循环、导航焦点失焦、链路中断不回落回落三场景）；design 8 决策 + Risks 三条新增；tasks 5 组 30 条（断言全数对齐）。
- 下一步：进入实施（/opsx:apply）；代码评审门仍留待实施完工后（tasks 5.8）。

---

# 4.8 登记：core 滚动面最小公开（2026-10-11，实施中拆出）

- **缘起**：spec 需求 4 承诺「会话未启用鼠标上报时滚轮用于面板内回看」；实施发现 `SessionVt`
  无滚动偏移面（`vt.rs:49`「本层 display_offset 恒 0」实证，网格探针复核）——回看需要
  core 增量。按 design D7 编译探针先例的同一路径处理：**先登记、同批变更、最小面**。
- **变更面**（`term/vt.rs`，仅公开既有 alacritty 能力，无 wire/行为变化）：
  `display_offset()`（只读偏移）、`scroll_display(delta)`（包装 `Term::scroll_display`，
  贴顶/贴底截断）、`rows_at_offset()`（按当前偏移取整屏行——只读，不推进脏标/指纹；
  `read_row` 泛化为带偏移变体，offset=0 恒等旧 `rows()`）。
- **TUI 消费**：终端焦点 + 会话未启用鼠标上报 ⇒ 滚轮 → `scroll_display(±3)`；渲染按
  offset=0 走 `rows_and_commit`（提交脏标）、offset>0 走 `rows_at_offset`（瞬态回看视图）。
- **边界**：core 判据面（term 帧/编码器/会话注册表）不受影响——纯视图层；三目标交叉
  check 与 clippy 随同批验证。
