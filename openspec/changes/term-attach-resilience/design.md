# term-attach-resilience — 设计

（r1 方案评审 + r2 完工代码评审整改版：r2 高/中意见（H1 GREETING 档 close/M1 拨号预算/
M2 矩阵收编/M3 证据链）与低档项已并入——锁外兜底措辞如实化、surface 腿保持背靠背
（快照真源是 vt 不需要逼重绘）、重拨成功后排空断链期积压、退避成功即复位、哨兵线程
panic 防护、env 放行 0；同拍「读腿断 vs stdin EOF」以等待循环兜底（EOF 在重连等待
窗 poll 立即命中 → 干净退出）注记于 D2。r1 意见原序——哨兵启用面收窄、真变路径保持现状、new 重拨降级、拨号错误类型化、重拨失败路径收连接、本地面分档、等待期 stdin 归属、标题/client_id 进程级、Ctrl-C 承诺落地、排空简化为 tcflush、分类器迁移取消、判据登记补齐、两道评审门入任务。）

## Context

三处现状（诊断与实验数据见 proposal；代码锚点经 r1 评审逐条核对属实）：

- 尺寸哨兵 `sentinel_repaint_locked`（service.rs:1957）在锁内背靠背 `resize(C-1)`→`resize(C)`，两次 SIGWINCH 被内核合并；opencode 类 App 比较尺寸后跳过重绘（vim 无条件重绘故幸免）。哨兵有 **4 个触发点**：1314（attach）、1395（输入触发几何切换）、1448（RESIZE 帧）、2022（end_leg 重选举）。attach 几何未变时不走 `apply_size_locked`（同尺寸早退，service.rs:3389），哨兵是唯一重绘机制；几何真变时 apply 的真实 SIGWINCH 本身已足以逼重绘（用户手动 resize 有效的机理）。focus-in 注入对 opencode 双重失效：它不开 `?1004`（判据门不满足），实测注入也无重绘输出。
- CLI `attach_run`（term_cli.rs:1483）对一切流终结走 `finish` → 还原终端 → 退出，无重连。服务端 self_reconnect 替换旧腿的注册语义已备（session.rs:365），同 client_id 重拨即触发；`first = leg_count()==0` 在替换之后求值（session.rs:394），旧腿已被摘的 重连腿重新拿到 first=true。`new` 置 `CREATE|ONLY_IF_ABSENT`（term_cli.rs:1014-1019），重拨会撞 `already_exists`（service.rs attach_or_create）——创建型会话的重连必须降级。
- 应答让位（service.rs:2200）：任一 raw-capable 腿在场服务端不代答查询，查询直达真终端；断链退出竞态里孤儿应答漏给 shell。`ControlClient` 无 Drop、reader 线程持 Arc 克隆（term_cli.rs:58 注释自认「不关则 fd+reader 线程滞留」）——失败路径不显式 close 则泄漏，被无上限重试放大成 EMFILE。
- raw 模式 cfmakeraw 关 ISIG（term_cli.rs:1075-1084）：**Ctrl-C 是普通字节**，不是信号——重连等待期的「Ctrl-C 退出」必须显式实现。

约束：wire 帧契约零变更；服务端会话注册表/词表语义不变；surface（App）腿不受影响；行为常量改动须登记 INTEROP-CRITERIA.md。

## Goals / Non-Goals

**Goals:**

- attach 后差分渲染 TUI 画面完整（不依赖用户手动 resize），且几何真变路径零行为变化。
- 远程面链路级断链 → attach 进程驻留重连，网络恢复后会话自动恢复（含 `new` 创建型会话）；会话级终结、本地 I/O 失效、用户退出不被重连劫持。
- 任何退出路径不向 shell 漏查询应答乱码；重连等待与长重试不泄漏 fd/线程。

**Non-Goals:**

- VT 态合成重绘（tmux 式服务端全屏序列化）——长期正解，留作后续独立变更。
- daemon 侧透明流续接（断链不断流的流级续接协议）——CLI 重拨复用整条既有握手、服务端零改。
- surface 腿 / App 面任何改动。
- 重连的输入续传保证（断链瞬间击键允许丢弃，与既有 send 失败路径一致）。
- 本地面写超时 30s 的既有边界（term_cli.rs:254-266，死 socket 上主循环最久阻塞 30s）——登记不修。
- 应答分类器 `pure_terminal_reply` 的迁移/可见性提升——随排空简化（D3）取消，保持 service.rs 私有。

## Decisions

### D1 哨兵间隔：启用面收窄到「attach 几何未变」+ 延迟第二跳

- **分档规则**：`sentinel_repaint` 增加「间隔形态」参数，**仅 attach 触发点（1314）且本次 `reg.size_applied == None`（几何未变）时启用**；其余情况（attach 带几何应用、1395/1448/2022 三处真变点）保持现状背靠背形态。理由：真变路径 apply（真 SIGWINCH）+ 第一跳（C'-1）若改为间隔，两次信号合并后 App 读到**中间值 C'-1** 并停留一个间隔期——把现状无害的合并变成可见的窄列回归；而真 SIGWINCH 本身已足以逼重绘，无需哨兵。此分档同时保住 `sentinel_count` 低 7 位判据口径（一次 attach 恰 +1，与几何是否应用无关——分档只改两跳的时序形态）。
- **常量与配置**：`SENTINEL_GAP` 默认 300ms（实验锚点：300ms 间隔 → opencode 14732B 全屏重绘、背靠背 0B）；以 `TermConfig` 新字段 `sentinel_gap: Duration` 落地，`from_env` 读 `HOMEWAY_TERM_SENTINEL_GAP`（仿 replay 的既有范式；不在调用点直读 env——测试并行污染）。真源注释钉在字段处。
- **两跳形态**：第一跳原位（锁内：计数 +1、`resize(sentinel)`）；第二跳 spawn 延迟线程（KILL_GRACE 先例，service.rs:2064——每 attach 一个 300ms 短命线程，成本可忽略；**不建周期设施**：term 服务无通用定时轮，出口守护的驱动拍属另一进程，为单一延迟建定时轮是过度设计）。fire 时锁内**五重验**，全过才恢复：
  1. registry 无此会话或 `done` → 放弃；
  2. `rt.gen != sess_gen` → 放弃（同名重建）；
  3. `rt.stopped` → 放弃；
  4. `rt.size() != base`（发起哨兵时的会话尺寸）→ 放弃（间隔期间真实 resize 已让位，不覆盖）；
  5. 否则 `pty.resize(base)`（幂等：同值 TIOCSWINSZ 不产生信号）。
- **不变量（注释钉死 + 反证单测）**：第一跳只动 PTY，**不写 ring epoch、不改 `rt.size()`**——这是重验④有效性的前提；若实现顺手改用 `apply_size_locked` 做第一跳，④会恒不通过 ⇒ 恢复跳永远让位 ⇒ PTY 停在 sentinel。单测：哨兵第一跳后 `rt.size()` 仍 == base。
- **spawn 失败兜底**：`Builder::spawn` 返回 `Err` 时在锁内立即 `pty.resize(base)` + 一行日志——恢复跳失效绝不把会话留在 sentinel 列宽上跑（后果比 KILL_GRACE 忽略 Err 严重）。
- **签名涟漪**：延迟线程持 `Arc<TermService>` 克隆（300ms 生命周期无害）做重锁；`sentinel_repaint_locked` 及调用链相应升 `self: &Arc<Self>`（调用点均已在 Arc 上下文，机械改动）。
- **既有vt 语义不动**：间隔窗内 vt 按 base 宽解析 sentinel 宽输出（≤300ms 的解析偏差，随恢复跳的全屏重绘自愈）；不为此改 vt 几何——登记 Risks。
- 连续 attach 叠加：最坏交错 `C-1→C→C-1→C`，各跳独立触发重绘、终值收敛 base；叠加可把「一帧」瞬态拉长（仅竞态重连场景可触发）——登记 Risks，**不设哨兵令牌**（场景极窄，机制不成比例）。
- pty.rs:286 的第二个背靠背哨兵实现仅测试用（服务端生产语义在 service.rs）：注释标注「仅测试夹具，勿在生产语义上引用」，不删（测试仍用）。
- 否决项（r1 评审确认合理）：CLI 侧假 RESIZE（几何会真写进会话/vt/环且 surface 不受益）；rows 差（等价无收益）；resp_tx 注入队列（PTY 输入字节面 ≠ ioctl）。

### D2 重连循环：远程面外层循环 + 连接段/终局段拆分

- **总体结构**：`attach_cmd` = `loop { attach_dial(reconnect) → attach_run_once → outcome }`；`outcome ∈ {Finished, LinkLost}`。现 `attach_run`（~280 行）拆两段：
  - **连接段 `attach_run_once`**：连接期资源（reader 线程、poke 管道、conn 写半、dm 的查询窗遗留处理），收尾 = 停 reader → `close()` → join（**保持既有「先 close 再 join」序**——close 是解阻塞读腿的唯一逃生口，term_cli.rs:1580-1583 注释）；本地 `data_rx` 断开后不再参与任何 poll。
  - **终局段 `finish`**：进程级唯一出口——信号面拆除、标题还原、**stdin 排空（D3）**、termios 还原、文案打印。
  - tty raw、信号管道、dm 状态机、**标题查询与 `old_title`、`client_id`** 均进程级一次性（见下），跨重连保持。
- **拨号错误类型化（矩阵前置）**：`attach_dial`/`dial_term` 错误从 `String` 改为 `enum DialErr { Session(code, msg), Transport(..), Proto(..) }`——ERROR 帧的 code 面不再被 `proto_error_text` 提前抹掉；`proto_error_text` 只在终局打印时调用。`already_exists` 在 Session(code) 上仍可判。
- **触发矩阵（独立纯函数 + 真值表单测）**：
  - → `Finished`：ENDED 帧；ERROR 帧/会话级 DialErr（no_session、already_exists 降级后不应再出现但保留分支）；**DATA 写本地 stdout 失败**（本地终端没了，重拨无意义）；**stdin EOF**（n==0，干净收尾——须在读腿 Disconnected 判 LinkLost 之前不被抢判）；poll 失败；用户信号（SIGTERM/SIGHUP/SIGQUIT/kill -INT）；分离键；**本地面（TermConn::Local/UDS）的一切链路级终结**——对端即出口进程，其死亡意味着会话已不存在，重拨永无成功，走既有可行动文案。
  - → `LinkLost`（仅远程面）：读腿 Io / 通道 Disconnected / `StreamEnded` 任意 reason / `send_data` 写失败 / Realign 的 `send_resize` 失败（补返回值检查，归入写失败同类）。
  - 重拨失败二分：Session 级 → `Finished`；Transport 级 → 继续 `LinkLost`。
- **`new` 重拨降级**：`attach_dial` 增加 `reconnect: bool`——首连按 `o.create/o.reuse` 置位，**首连成功（收到 ATTACHED）后本进程内后续重拨一律清 `CREATE|ONLY_IF_ABSENT`**（纯 attach 语义），创建型会话不再被 already_exists 打死。
- **等待期交互（重拨间隔内）**：`poll([stdin, signal_pipe])`（`data_rx` 已退场）——stdin 喂 dm：`DetachAction::Detach` → `Finished`；**孤立 0x03（Ctrl-C 字节）→ `Finished`**（等待态无远端可投递，当退出处理安全；raw 下 Ctrl-C 本不是信号，banner 的「Ctrl-C 退出」由这条兑现）；其余字节丢弃（含孤儿应答——顺带消费了 D3 要排的应答）。信号面照旧（SIGWINCH 记尺寸无须缓存，HELLO 每次带当前 `tty.size()`）。
- **横幅**：断链即打一行（`\r\n` + 暗色「homeway: 与主机的连接断开，重连中…（Ctrl-C 退出）」）；重连成功后服务端握手自带 `3J/2J/H` 清屏，横幅自然消失。altscreen 下覆盖底行可接受（断链期画面本已陈旧）。
- **退避**：命名常量（0.5s 起、×2、封顶 5s；与既有三套退避——unified_cli 的 ROLE_BACKOFF/host 重建/files_server——的关系与差异注记在常量注释：本套服务交互重拨、恢复最多延后一个封顶值是显式取舍）；退避起点 = 上次尝试结束（拨号自带 `--timeout` 预算，实际间隔 = 退避 + 拨号时长）。无重试上限：网络恢复时间不可知，用户退出与 Session 级错误是自然终点。
- **重拨失败路径收连接（防 EMFILE）**：`ControlClient` 无 Drop——每次失败尝试（open_stream 失败、GREETING 失败等一切 Err 面）显式 `client.close()`；验证任务「长重试下 fd/线程数稳定」（lsof 计数或失败路径单测）。既有隐患，被无上限循环放大，本批收口。
- **进程级一次性**：OSC 21 标题查询与 `old_title` 只在进程启动做一次（若每段重连重查，第二次拿到的「旧标题」已是 homeway 自己设的会话标题，终局会把用户原标题还原错）；`client_id()` 用 `OnceLock` 缓存一次（防拿不到 tty 路径时每拨换 id ⇒ 旧腿不被替换、腿数堆积）。
- 否决项：**daemon 透明重开流**——需流级续接协议（REPLAY 起点协商、输入去重），本批不立。

### D3 退出排空：poll + tcflush（不区分形态）

- `drain_terminal_input(timeout_ms)`：`poll(stdin, POLLIN, timeout)`（等在途应答落队）→ `tcflush(0, TCIFLUSH)` 丢弃全部排队输入。不读字节、无 8KiB 上限、无形态判定、无截断误判面——「全吞」语义下分类器无行为效果，故 `pure_terminal_reply` **不迁移**（r1 意见 4.1/4.4：spec 原「仅纯应答才丢弃」与 design「一并吞弃」自相矛盾，简化后统一为不区分形态）。
- **统一超时 50ms，适用一切退出路径**（r1 意见 4.2：detach 等正常退出时远端 TUI 仍在跑、可能刚发查询，在途应答与断链同形——0ms 档不成立）：应答已在队列时 poll 立即命中（零额外延迟）；无排队输入时付满 50ms——退出路径一次、不可感知。`TCSANOW` 还原语义不变（否决 TCSAFLUSH：会改变一切退出路径的输入语义，面过大）。
- 位置：连接拆除后、`tty.restore()` 之前——**raw 态之下、开回显前**（此时尚未开回显，排队字节不会被回显；终局段独占 stdin 无并发面）。
- 非 tty stdin（理论不可达——attach 有 TTY 前置）：`tcflush` 失败静默忽略。

### D4 判据登记

INTEROP-CRITERIA.md「判据变更记录」同批登记 **5 行**（五列格式与既有表一致）：

1. 哨兵间隔常量：attach 几何未变路径从背靠背（0ms）→ 300ms 可调（`HOMEWAY_TERM_SENTINEL_GAP`）；真变路径不变。
2. 重连退避常量：新增 0.5s×2 封顶 5s（远程面交互重拨）。
3. 退出排空：新增统一 50ms poll + tcflush(TCIFLUSH)。
4. 重连横幅：新行为面立册（新串，无 Go 对应——先例：term 清屏修复的「无既有判据行——登记留痕 + 新行为面立册」）。
5. 断链三态文案（`TermReadErr::message`，Go remoteEndMessage 同串）产出面收窄：链路级终结从「文案退出」收窄为「横幅+重连」，三态文案只在终局面（本地面/Session 级/ENDED 后）产出。

## Risks / Trade-offs

- [300ms 对信号处理极快的 TUI 仍可能合并] → config 字段可调 + 真机验证清单含 opencode/vim 双例；下限受瞬态约束不再放大。
- [间隔窗内 vt（base 宽）与 PTY（sentinel 宽）几何不一致——surface 快照若恰在窗内取到会错位] → 窗 ≤300ms 且随恢复跳全屏重绘自愈；登记不为此改 vt。
- [连续 attach 叠加可延长窄列瞬态] → 仅竞态重连可触发，终值收敛；登记不设令牌机制。
- [多腿期间一帧窄屏瞬态可见] → 与真实 resize 瞬态同形；真机不适再立 VT 合成重绘批。
- [重连横幅覆盖 altscreen 底行] → 恢复后服务端清屏自愈；断链期画面陈旧是既有事实。
- [每次退出付 ≤50ms 排空窗] → 一次且有界，退出路径不可感知；换「一切退出路径不漏应答」的完备性。
- [本地面写超时 30s：死 socket 上 send_data 最久阻塞 30s，等待期交互在该窗内不响应] → 既有边界，登记不修（r1 意见 3.11）。
- [排空吞用户竞态击键] → 窗 ≤50ms 且仅退出路径，与既有 send 失败丢弃语义一致。
- [`attach_run` 重构面大] → 连接段/终局段拆分 + 决策矩阵独立函数化 + DialErr 类型化前置，真值表单测先行；实现批分 commit（哨兵 / 重连+排空 / 判据登记各一）。

## Migration Plan

- 全部改动同仓同版次；哨兵（服务端）与排空/重连（CLI）随下一次发版一并生效，回滚 = revert 对应 commit。
- 无状态迁移、无 wire 变更、无兼容矩阵（旧 CLI 对新服务端、新 CLI 对旧服务端均退化为现状行为：哨兵照旧背靠背、断链照旧退出、退出照旧可能漏应答——不劣化）。

## Open Questions

- 哨兵间隔是否需按平台分档（Darwin/Linux 信号投递延迟差异）——当前单值 300ms + config 覆盖已可用，真机数据足够后再议，不改变行为契约。
