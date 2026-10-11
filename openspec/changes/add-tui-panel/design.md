## Context

出口侧能力已建成：多腿同显（8 腿/会话上限，满员按「最久停滞→最久未活跃」腾位且裸断不发终止通知）、回放环 + 尺寸哨兵、LIST（含 agent/stateV2/title/cwd/lastActiveMs/clients[]）、agent 状态识别（stateV2 ∈ working/blocked/idle/unknown）。客户端侧：daemon HostTable + 事件总线（`snapshot.get` + `events.subscribe`，订阅域冻结为 link/session）；term 通路 `stream.open kind=term` 纯字节透传。**core 的 `SessionVt` 是公开类型**（`term/vt.rs:213`，`pub mod term`），`key_options()` 公开且已填 kitty flags，`encode_key/encode_mouse/encode_paste_part/encode_focus` 全公开——客户端编码口径零新增公开面即可获得。

**LIST `clients[]` 无腿身份标识**（仅 kind/cols/rows/sinceMs/active，`service.rs:3487-3496`），断连后自身腿已被摘除——「被腾位」不可经 clients[] 自证，四分支判定必须用会话存在性 + 链路状态 + 腿满三个外部可判定信号（r2 N-1）。

**控制面词表是跨仓冻结契约**（`daemon/vocab.rs:1-4`；tier 侧订阅域初始集钉死；`daemon/tests.rs:43` 断言冻结夹具恰 43 帧）。技术评审三轮（docs/reviews/TUI-design.md：r1 31 条、r2 14 条、r3 14 条）后定案 **A 案：控制面零新增、core 零行为改动**——全部新逻辑落在 TUI 进程与本机文件。

约束：TUI 为纯客户端进程（不 acquire `<state>/lock`、不撞 identity session.lock）；线上协议（term 帧、QUIC、capi、tier 对齐面）零改动；本仓对空闲唤醒敏感，一切轮询要有账（tasks 5.5 数值门）。

## Goals / Non-Goals

**Goals:**
- daemon **零契约面改动**（无新 op、无新事件域、无响应字段变更）；**core 零行为改动**（模拟器复用公开的 `SessionVt`；4.8 滚动面三方法 = 仅视图层公开，登记 TUI-design.md）；
- 三条数据通路与全局唯一的选型落地；四分支断线判定的**可判定谓词**（r2 N-1 整改主战场）；
- 接入对会话的副作用最小化并显式登记（r2 N-4：HELLO 尺寸对齐）；
- 状态行三项口径（全部既有数据面）。

**Non-Goals:**
- 面板外提醒（系统通知/响铃）、Socket API 反向开放、会话元数据服务端推送；
- 延迟实时化（`IslandSnapshot.rtt_ms` 中继形态量的是客户端↔中继 ≠ 端到端，不能替换既有 `rttMs`，留作后续 additive 变更）；
- surface 腿消费端（dec_snapshot/dec_diff 生产化）；
- App/capi/tier 侧任何改动；判据行变更（不预期）。

## Decisions

### D1. 进程与线程模型：homeway-cli 内新模块 `tui/`，纯同步多线程 + 事件驱动渲染
主线程跑 UI 事件循环 + 渲染；后台线程 = 控制面事件流、LIST 轮询、终端腿读帧、状态行快照轮询。不引入 tokio。
渲染节拍：事件驱动 draw——任一数据源更新或 500ms 心跳超时才合并重绘，无固定帧率空转；数据通道有界但**不静默丢字节**——通道满时读线程阻塞（背压沿隧道自然回传），宁可滞后不可失步；UI 跟不上时丢的是中间渲染拍，不是字节。

### D2. 终端视图：raw 腿（不声明 capsRawTerminal）+ 内嵌 core `SessionVt` + 接入对齐
- **客户端模拟器 = core 的 `SessionVt`**（r2 N-3 定案）：公开类型、出口同款实现，`key_options()` 提供完整编码口径（kitty flags、鼠标/粘贴/焦点编码），裸用 alacritty_terminal 的路线废弃（ATTACHED modes 仅 8 位 legacy 掩码，缺 kitty/DECBKM/1005 等模式，无法与出口编码口径对齐）。**前置编译探针**（tasks 1.2）：确认 homeway-cli 侧可构造 `SessionVt` 并调用 `key_options()/encode_key()`；探针失败才回头补 core 公开面（预期不需要）。
- HELLO 不带 SURFACE、不带 RAW_TERMINAL caps 位 → 服务端代答终端查询不被抑制（让位规则只由 RAW_TERMINAL 位决定）；kind = legacy，选举/腾位无歧视待遇；
- **接入尺寸对齐（r2 N-4 / r3 I）**：HELLO 尺寸一律取**最近一次 LIST 快照**报告的会话 cols/rows（快照与 HELLO 之间他腿改几何的残余竞态登记 Risks）；交互实例在用户首次交互后发送 pane 尺寸的 RESIZE（此时本腿经 activitySeq 选举成为活动腿，尺寸变化是用户意图）；镜像/旁观实例 NEVER 下发 RESIZE。接入的登记副作用：成为活动腿、会话剪贴板读缓存随活动腿重置、首腿接入的 focus-in 注入（仅会话无其它腿时发生，既有语义，r3 H）；
- **稳定 client_id（r2 N-2 / r3 G）**：HELLO 尾随块 SHALL 携带 caps 块（位全 0）——`enc_hello_tail` 的编码约束是「带 ID 必带 caps 块，`caps_present=false` 静默丢 ID」（`frames.rs:350-368`；term_cli.rs:984 恒传 true 为先例）；重附依赖服务端同实例替换路径回收残留腿；被自身残留腿挡住时只做占用复查/退避，不绕过检查硬接；
- **四分支断线谓词（r2 N-1，三种处理结果）**：流断后依序判定——①会话不在 LIST ⇒ 终止（停拨 + 「会话已结束」+ 回落）；②该主机链路离去/形态不可用（本机 = 本地 socket 重连失败）⇒ 链路中断（自动重拨，**间隔自 0.5s 起、指数递增至 10s 上限，链路恢复事件立即重置**——r3 F，防紧循环重拨）；③会话腿满 ⇒ 「会话观察腿已满，等待空位」（退避 10–30s，LIST 见空位恢复，腿满期不重附）；④其余 ⇒ 按链路中断重附。腿满预检查阈值取缺省上限常量（8）；服务端经 `HOMEWAY_TERM_MAX_CLIENTS` 覆盖为已知限制，服务端接入拒绝为权威兜底（r3 B）。**不做心跳保活**（raw 腿刷活跃度的原语只有 INPUT 与 RESIZE，都有害）。
- 输入（r1 D-4）：crossterm 键鼠事件 → `SessionVt`/keyenc 编码 → **DATA 字节帧**（`term attach` 同路）；INPUT(0x11) 是 surface 家族帧（服务端编码），不使用。粘贴按会话括号粘贴位（2004）包裹、焦点事件按会话上报位（1004）注入（r2 N-8）。

### D3. 会话列表：选中主机 LIST 短连接轮询，活动态自适应拍
沿 `term list` 同款路径（不占 8 腿预算）。拍长自适应：**活动态 ≤2s**（满足 3 秒门槛），非活动态 10s（spec 显式豁免并要求恢复活动态后 3 秒内刷新）；活动态判据 = 最近 30 秒内有用户输入或外层终端焦点在面板（经外层 1004 跟踪）。仅轮询当前选中主机。显示排序 lastActiveMs 降序、会话名破平；选中以会话名为键。徽章映射：`agent ∈ {shell, unknown}` → 无 agent；否则 stateV2 四态。占用方信息按 `clients[]` 可得字段呈现（数量、kind 构成、active 标记）——不做腿身份识别（数据面不存在）。

### D4. 状态行：既有数据面如实显示，零 daemon 改动
- 连接状态/形态：`snapshot.get` 初值 + `events.subscribe(link, session)` 增量（复用 `status --watch` 骨架，含 cursor 回放 + generation 门；**不复刻其 `cursor_stale → exit(1)` 行为**，改为重连 + 全量重快照，r2 N-12③）；via 值域含 `none`，呈现归入占位；
- 延迟：`HostState.link.rttMs` 端到端探活口径 + **采样年龄显示**（>120s 标注陈旧/占位，r2 N-10）；不实时化（见 Non-Goals）；
- 流量：`HostState.stats{rxBytes,txBytes}` 由 TUI 状态行拍（1s）差分出速率，口径标注「应用流」；
- 轮询账：量化验收见 tasks 5.5（数值门：daemon CPU 增量、ctx switch 增量、QUIC 新建流上限，基线校准后定稿并记录）。

### D5. 本机条目：TUI 合成，可用性 = socket 可连
路径 = `<state>/serve/term.sock`（TUI 既已 dial `<state>/control.sock` 必然知道 `<state>`；`serve_dir` 恒定）；**不复用 `term_cli` 本地 dial 段**（其 `--state` 指向 serve 子目录）。可用性判据 = 本地终端服务 socket 可连接；serve.status 的非 running 各态（stopping/stopped/failed/absent）统一归入不可用（r2 N-12②）。连接形态标签 `local`。主机列表确定序：本机优先，远端按名称升序、同名按加入先后（r2 N-6②）。

### D6. 全局唯一 = 选中状态文件（A 案定案，r2 N-5/N-6 补齐并发与回落语义）
- 选中状态（revision 单调 + 主机 id + 会话名）落 `<state>/cache/tui-selection.json`；
- **原子写**：临时文件 + `rename` 替换，杜绝半截 JSON；**读方按内容判定变化**（内容指纹/mtime+size 组合，不只 mtime）；**运行期解析失败或文件消失 = 忽略本次读、保持当前选中**，仅启动时走回落链（r2 N-5）；
- **回落不写回**：仅用户发起的切换持久化；派生的回落仅本地生效（r2 N-6①，避免实例间用陈旧 LIST 互相拉扯）；并发写 = last-writer-wins（选中语义可接受）；
- cache/ 是可清理层：daemon 持 `<state>/lock` 期间 reset 被拒，清理只影响下次启动（走回落链，spec 已对齐该语义）；
- 300ms watch 仅 TUI 存活期间存在，本机 stat 无唤醒账；
- **留档备选（B 案）**：跨设备同步或事件驱动镜像需求出现时，走 additive delta（op×2 + 域×1 + kind×1，tier 侧同批登记）。

### D7. 键位与焦点模式（r1 R-1 / r2 N-7/N-8 / r3 L 定案）
双焦点模式（面板导航焦点 / 终端焦点，终端焦点除单一退出组合键外全透传）。具体键位实现期定，退出组合键避开常见 TUI 应用组合空间。外层终端的焦点上报（1004）与括号粘贴（2004）均由面板启用——1004 用于活动态判据与导航焦点期间的会话失焦/复得注入，2004 用于接收 Paste 事件；终端焦点下两者同时按会话对应上报位转发（嗅探与转发不冲突）。导航焦点期间会话按失焦处理（会话启用 1004 时注入失焦序列）。

### D8. 依赖、模块布局与交叉编译
- `crates/homeway-cli/Cargo.toml` 新增 ratatui、crossterm；alacritty_terminal 走 workspace 依赖继承；
- `crates/homeway-cli/src/tui/` 模块：`app.rs`（状态机/选中路由/回落链）、`backend.rs`（控制面 + term 通路 + 选中文件）、`hostlist.rs`/`sesslist.rs`/`termview.rs`/`statusline.rs`、`keys.rs`（焦点模式）；
- 三目标交叉 check（aarch64-ohos + musl×2）前置到任务组 1；编不过则 cfg 门控并记回本 design。

## Risks / Trade-offs

- [TOCTOU 残余竞态] LIST 预检查与 HELLO 之间他人可占满，服务端注册序满员时先腾位 → 面板以「检查为满不接入 + 稳定 client_id 同实例替换 + 被让位退避」把触发概率与影响压到最低；绝对「绝不触发服务端腾位」不可达，spec 按「面板可控行为」收窄（r2 N-2）。
- [瞬断 + 满员的恢复延迟] 腿满分支以 clients[] 计数为可观察代理，瞬断未回收时也会显示满 ⇒ 此场景恢复延迟最长 30s（被让位退避上限）；替代方案（试探接入）会触发服务端腾位，不可取（r3 E）。
- [LIST 时效竞态] 对齐值来自最近一次 LIST 快照，快照后他腿改几何的窗口内接入仍可能触发一次尺寸变化；窗窄（一次往返），登记为已知限制（r3 I）。
- [接入成为活动腿的副作用] 活动腿切换 + 剪贴板读缓存重置是既有服务端语义，已显式登记为登记副作用（spec），HELLO 尺寸对齐消除了几何面副作用。
- [kitty 模式协商] 内嵌 `SessionVt` 与出口同款同版本，`key_options()` 即模式真源；单测覆盖模式位→编码矩阵。
- [选中文件并发] 原子写 + 内容去重 + 回落不写回后，残余风险 = 极端并发下 last-writer-wins 丢一次切换，下次用户操作自愈——选中语义可接受。
- [轮询开销] 活动态 2s LIST 拍 + 1s 快照拍仅前台存在；数值门验收（tasks 5.5）。
- [ratatui 交叉编译] tasks 1.4 前置；失败即 cfg 门控。
- [多实例渲染漂移] 各实例自开腿回放起点略有差异（滚动位置为腿私有态）→ spec 只约束选中一致与会话内容一致。

## Migration Plan

纯增量功能，无迁移。上线顺序：ROADMAP 立线登记 → 编译探针（core 零改动定稿）→ 依赖 + 交叉 check → TUI 模块骨架 → backend 三通路 → UI 四区 → 本地矩阵（local-rust-exit.sh + 双实例 + 腿满构造 + surface 腿断言）验收 → 代码评审门。回退 = 不启用子命令（选中文件残留无害，无契约面残留）。

## Open Questions

（无——两轮评审 45 条意见全部处置；键位具体值、数值门校准为实现期细节，不影响规格。）
