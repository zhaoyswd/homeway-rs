# homeway TUI 技术设计开工评审（门一）

**结论先说：方向通过，门一未闭合（有条件通过整改）。** 复用既有 token/daemon/term 的方向正确，没有另起 SSH 或会话服务；但设计稿没有把本地终端生命周期、raw 应答归属、输入/请求 generation、破坏性操作、控制面预算这些一等决策写清。当前不宜按现稿签门一，但可以按下面高项整改后继续实现。

核验范围：`docs/reviews/2026-10-27-tui.md`、`AGENTS.md`、`ROADMAP.md` 评审协议、`docs/INTEROP-CRITERIA.md` 新增 TUI 条目，以及 `term_cli`、term service、`frames`、`keyenc`、`vt`、daemon/client、`local_term`/`engine` 的协议锚点。未修改代码或文档，未启动生产进程；只做了一次 OHOS CLI 交叉 check（exit 0），不把它当运行时验收。以下代码锚点只用于判断方案与既有协议是否相容，不评价未完成代码的完成度。

## 高

### 高-1 本地终端已是统一进程的一等架构变更，但设计稿没有这项决策

- **位置**：`docs/reviews/2026-10-27-tui.md:7,21`；`crates/homeway-cli/src/local_term.rs`；`unified_cli.rs::run_unified_state`；`server/engine.rs::start_with_term_service/shutdown`；`docs/INTEROP-CRITERIA.md:603-604`
- **问题**：设计稿只说“采用原有 term 服务”，但当前骨架新增了进程级 `TermService` + `<state>/local-term.sock`，并让 `ServeEngine` 借用同一注册表。这带来一串设计决策：是否所有非 Windows 统一进程都要启动本地终端；`local-term.sock` bind 失败是否应让整个统一进程退出；`serve stop/restart` 后本地会话是否保留；旧 `serve/term.sock` 与新的 `local-term.sock` 的优先级和残留 socket 回退；`HOMEWAY_TERM=off` 时 TUI 本地主机如何呈现。当前骨架把 `LocalTerm::start` 失败做成 `eprintln + exit(1)`，而既有 term 监听器失败是 fail-soft；这会影响生产进程启动可靠性。`homeway term` 已接入 `local-term.sock`，这一点是对的，但设计稿需要同步。
- **建议**：在设计稿中单列“本地终端所有权与生命周期”一节，明确：启动条件（恒开/按需/可配置）、失败策略（fail-soft 还是显式致命）、socket 优先级与 stale 清理、`serve stop/restart` 的会话保留语义、`HOMEWAY_TERM=off` 的界面提示、旧守护无 `local-term` 时的降级路径。UI 的“关闭出口”文案要改成能表达“撤网络出口，本地会话可能继续”的含义，并补 `serve on/off/restart`、stale socket、只读 state、短路径、双实例测试。
- **严重程度**：高

### 高-2 raw attach 的终端应答归属与 replay 未定义，当前骨架会回答历史查询

- **位置**：`docs/reviews/2026-10-27-tui.md:18-20`；`term/service.rs::response_owner`、`pump_loop` 的 `suppress`、`run_raw_writer`；`tui/mod.rs::process_attach`；`term/vt.rs::write_collecting`
- **问题**：既有协议在 active raw 腿存在时，**服务端故意不代答**，把查询应答责任让给 raw 客户端。TUI 的本地 `SessionVt` 因此必须成为唯一应答者；设计稿只写了“输出交给 SessionVt 解析”，没有写应答归属。更具体地：raw 回放中的 `DATA` 含历史查询，当前骨架对所有 `DATA` 都调用 `write_collecting` 并回发应答，且忽略 `REPLAY_DONE`，会把历史 DA/DSR/颜色查询的过期应答注入 PTY。多客户端同时 attach 时，active 腿会变化，TUI 也无法知道何时该停止代答，可能出现双答或无人答。
- **建议**：在 attach 状态机中加入明确的 replay 阶段：`ATTACHED` 后只解析不回发，`REPLAY_DONE` 后才允许 live 应答；`REPLAY_DONE` 的 `TRUNCATED/SIZE_CHANGE` 也要呈现。明确首版只支持“单 active raw TUI”或补充 active 归属协议；不要把多腿应答竞态留成隐含行为。补“回放中含 DA/DSR，TUI 不应答历史查询、live 查询应正确应答”的测试；同时按 `SessionVt::sync_deadline/flush_sync_timeout` 处理同步输出超时。
- **严重程度**：高

### 高-3 generation 只过滤 attach 数据，未覆盖控制面结果和输入归属

- **位置**：`docs/reviews/2026-10-27-tui.md:21`；`tui/backend.rs::Event`；`tui/mod.rs::process_backend/select_session/send_input`；`tui/view.rs::handle_event`
- **问题**：当前 generation 只加在 `AttachEvent` 上；`Snapshot/Sessions/Created/Failed/Notice` 没有请求 epoch。用户从主机 A 切到 B 再切回 A 时，A 的旧 `Sessions` 结果仍满足 `target == self.target()`，可能覆盖新结果；同一目标的 `Created`、失败通知也可能串到新上下文。会话刷新用名字保选是好的，但 `SelectSession(index)` 等动作仍以索引取目标，列表变化时有选错风险。输入侧只检查 `connected`，没有 attach epoch、没有 host-link 门控，也没有明确“切换时清空待发队列/前缀状态”，这正是输入串台的边界。
- **建议**：给所有异步控制结果加单调 request epoch，UI 只接受当前 epoch；attach 输入帧携带 attach epoch，旧 attach 的 writer 不再可写。主机/会话选择统一按稳定 id/name 解析，不从列表索引直接取目标。切换、离线、`ENDED`、attach 错误时清空待发输入与 `Ctrl+B` 前缀状态。补“慢响应 + 快速切主机/会话”“旧 Create/List 结果晚到”“离线前已排队输入”测试。
- **严重程度**：高

### 高-4 右键菜单的破坏性操作没有确认和影响说明

- **位置**：`docs/reviews/2026-10-27-tui.md:24`；`tui/view.rs::menu_labels/activate_menu`；`tui/mod.rs` 的 `ToggleExit/ToggleRelay/DeleteHost/CloseSession`
- **问题**：右键 + Enter 直接“关闭出口/关闭中继/删除主机/关闭会话”。`KILL` 会结束远端进程；`host.remove` 会摘除主机及其会话；`serve stop` 会撤掉网络出口并影响远端客户端，即使新的本地 term 架构让本地会话继续，也不能按“只是关一个开关”呈现。误触或对离线主机的操作都没有二次确认，数据/会话损失风险高。
- **建议**：加入确认对话框，明确写出后果（“将结束该主机上的会话”“将撤掉公网出口，远端客户端会断开；本地会话保留”）。删除主机/关闭会话建议要求名称确认或至少默认取消；离线主机禁用危险项；操作进行中显示 pending，成功后以刷新结果为准。补菜单误触、Esc 取消、离线禁用测试。
- **严重程度**：高

### 高-5 控制面统一 3s 预算与串行 refresh，取消/超时/部分成功语义不足

- **位置**：`docs/reviews/2026-10-27-tui.md:13-16`；`tui/backend.rs::REQUEST_BUDGET`、`snapshot/execute`；`daemon_cli.rs` 的 `TIMEOUT=10s`、`HOST_ADD_TIMEOUT=20s`；`tui/transport.rs::IO_BUDGET=3s`；`INTEROP-CRITERIA.md:606`
- **问题**：所有控制请求都用 3s，但 `host.add` 的既有预算是 20s（reach 探测本身 3.5s），角色启动/停止、`stream.open` 也可能超过 3s，attach 的 greeting/ATTACHED 同样用了 3s。一次 `snapshot` 串行发 `host.list + snapshot.get + subscribe + socks.status + serve.status + relay.status`，最坏会长时间占住单 worker；请求无法取消，队列 16 槽满了用 `try_send` 直接丢动作。更严重的是多步操作（host.add→snapshot、start→token→snapshot、socks.on→取 listen→snapshot）如果后一步失败，UI 会把已经成功的变更整体报成失败，重试又可能撞 `host_exists/already`。
- **建议**：按操作定义预算（host add 20s、角色操作与现有 CLI 对齐、stream open 用 10s/30s，refresh 才用短预算），并允许单飞/取消/合并；用 `daemon.status` 拿到 hosts/roles/generation，再加一次 `socks.status`，替代串行六请求；删除未消费的 `subscribe(view)`。多步操作返回结构化阶段结果，失败后先 refresh 对账，不要把“已变更但确认失败”伪装成整体失败。补 timeout 后宿主已添加、start 后 token 获取失败、队列满等测试。
- **严重程度**：高

## 中

### 中-1 term 客户端能力重复实现，且没有沿用既有重连语义

- **位置**：`docs/reviews/2026-10-27-tui.md:7,18-19`；`term_cli.rs::dial_term/attach_dial/attach_session`；`tui/transport.rs`
- **问题**：TUI 重新实现了 greeting、HELLO、ATTACHED、LIST/CREATE/KILL、client id、写线程和输入队列；`term_cli` 里已有的连接归因、重连阶梯、退出排空、稳定 client id、self-reconnect 没有被复用。短期能跑，长期协议一改就会出现两套行为漂移；而且 TUI 目前没有重连策略，断链只能重新选择会话，和现有 `homeway term attach` 的韧性不一致。
- **建议**：抽出一个与 UI 无关的 term client（dial/handshake/list/create/kill/attach），CLI 与 TUI 共用；或至少对 `frames`/`fixtures/term` 做双客户端 conformance。设计稿明确 TUI 断链是自动重连、退避重试还是显式失败重选，并说明不沿用 `term_cli` 的哪部分。
- **严重程度**：中

### 中-2 键、鼠标、焦点、IME/kitty 的输入模型未闭合

- **位置**：`docs/reviews/2026-10-27-tui.md:20,23-25`；`tui/input.rs`；`tui/view.rs::handle_event`；`term/keyenc.rs`
- **问题**：raw 腿不能发抽象 `INPUT` 帧，键必须由本地 `keyenc` 编码成 DATA；设计稿没有给出 crossterm `KeyEvent` 到 `keyenc::KeyEvent` 的映射表。当前骨架硬编码 `KeyFlavor::AltEscPrefix`，与 macOS 的 HELLO 平台口径可能不一致；没有启用外层 kitty keyboard enhancement，却在远端 VT 打开 kitty 模式时按该模式编码；`KeyEventKind::Release` 被 `handle_event` 直接丢弃，也没有 `composing`/IME 语义。焦点方面只在窗口级 FocusGained/Lost 时发 `encode_focus`，从终端聚焦到侧栏时没有 focus-out，从侧栏回终端时没有 focus-in。
- **建议**：明确“本地终端能力如何映射到远端 VT 模式”：平台 flavor 与 HELLO 一致；如需 kitty/mok2，先向外层终端启用并在 RAII 中恢复；Release/Repeat、IME composing、粘贴边界写清。焦点切换时按远端 `modes().focus_events` 发焦点事件。`Ctrl+B` 前缀后的未知键建议按 tmux/screen 语义透传“前缀 + 键”，不要静默吞掉；`F6/F10` 若保留全局语义，应给终端内应用一个逃生口。
- **严重程度**：中

### 中-3 离线/断链检测与输入门控不完整

- **位置**：`docs/reviews/2026-10-27-tui.md:13,21`；`tui/mod.rs::process_attach/send_input`；`tui/backend.rs`
- **问题**：`send_input` 只检查 `connected`，不检查主机 link 是否 ready；主机掉线但 attach 尚未报错时，按键仍会被写入队列。主机状态靠 2s 轮询，未消费既有事件；断链后没有重连或明确的“必须重选”状态。`local-term.sock` 存在但已死时，当前优先级会直接失败，不回退 `serve/term.sock`。
- **建议**：输入路径以“当前 attach 可写 + 主机在线”双门控；断链时清空待发输入并显示可重试状态。决定自动重连/退避还是显式重选；若消费 daemon events，就真正消费，否则不要保留空 `subscribe`。本地 socket 先探测 alive，再决定回退。
- **严重程度**：中

### 中-4 剪贴板成功语义、SSH/tmux 场景和远端 OSC52 未定义

- **位置**：`docs/reviews/2026-10-27-tui.md:15-16`；`tui/clipboard.rs`；`tui/mod.rs::process_clipboard`；`term/vt.rs::ClipEvt`
- **问题**：“复制完成才提示成功”对 OSC52 不成立：写出 OSC52 只是请求，终端可能忽略；`pbcopy/xclip/wl-copy` 退出 0 也不是可验证的“已进入剪贴板”。在 SSH 下这些命令操作的是远端机器，UI 却会提示系统剪贴板成功。当前 token 的唯一交付路径是剪贴板，若复制失败，角色/代理已启动但 token 既不显示也不持久化，用户无法从 TUI 恢复。远端程序通过 raw 流发来的 OSC52 Store/Load 也没有被 TUI 处理。
- **建议**：把结果定义为三态：“系统命令返回成功”“OSC52 已请求（终端可能不生效）”“失败”；不要对请求型路径写“已复制成功”。在 SSH/tmux 下优先提示 OSC52 或明确本地/远端剪贴板。复制失败时提供重试并保留 token 于内存直到退出，仍不落日志/界面。明确是否支持远端 OSC52 Store/Load；若支持，Load 必须有安全策略。
- **严重程度**：中

### 中-5 主题没有覆盖终端 ANSI 调色板，浅色主题可读性无保证

- **位置**：`docs/reviews/2026-10-27-tui.md:10-11`；`tui/view.rs::terminal_color/apply_theme`；`term/vt.rs::set_default_colors`
- **问题**：当前主题只改侧栏/chrome 和 VT 的默认 fg/bg；`Color::Palette(index)` 仍直接交给外层终端，浅色主题下程序输出的默认 16 色仍可能是深色终端配色，出现低对比。光标、选中、反色等也没有主题策略。设计稿没有写主题文件结构（当前是 `tui-theme`）和升级/缺省行为。
- **建议**：定义主题的 ANSI 0–15（必要时 256）映射，或明确“终端调色板由外层终端负责”并给出对比度兜底；补光标/选中色；主题文件命名、原子写、损坏回退、只读 state 行为写入设计。
- **严重程度**：中

### 中-6 raw VT 重绘的范围和定时器未定义

- **位置**：`docs/reviews/2026-10-27-tui.md:19-20,25`；`term/vt.rs::write_collecting/sync_deadline/rows`；`tui/view.rs::draw_terminal`
- **问题**：本地 VT 有 `sync_deadline/flush_sync_timeout`，但 TUI 主循环没有周期调用；程序发同步输出后若无后续写入，可能一直不出画。当前每 25ms 全量 `rows()` 重绘，未利用 `update/dirty_rows`，大窗口下是持续分配和逐 cell 转换。回放截断/尺寸变化标志、滚动历史、光标形状都没有决策；用户要求的“原终端内容”是否包括 scrollback 也未说明。
- **建议**：在事件循环定时调用 `flush_sync_timeout`；明确 viewport/scrollback 范围和滚动行为；能用 `dirty_rows`/合并帧就减少全量转换；把 `REPLAY_DONE` 的 truncation/size-change 呈现出来；光标形状至少跟随 `CursorShape`。
- **严重程度**：中

### 中-7 RAII/panic/信号与后台日志的收尾细节不足

- **位置**：`docs/reviews/2026-10-27-tui.md:26`；`tui/terminal.rs::TerminalGuard::enter/restore`；`daemon_cli.rs::dial_control_spawn`；`tui/backend.rs`/`transport.rs`
- **问题**：`enter()` 在安装部分信号、启用 raw 之后若 `execute!` 失败，guard 尚未构造，可能留下 raw 模式；全局 panic hook 对后台线程 panic 也会恢复备用屏，而主线程可能继续绘制；`dial_control_spawn` 在 TUI 已进入备用屏后仍会 `eprintln!` 启动提示，可能污染画面；backend/attach/clipboard 线程在退出时没有 join 或 panic 观测。
- **建议**：把初始化做成阶段化 guard（任一步失败都回滚 raw/备用屏/信号）；panic hook 区分主线程与后台线程（或后台 panic 直接终止进程并恢复）；后台日志重定向到 state 日志或 UI notice，禁止直接写 stderr；退出时 join/取消 attach、backend、clipboard，补 SIGINT/SIGTERM/SIGHUP/panic/初始化失败测试。
- **严重程度**：中

### 中-8 本地伪主机、实时状态和关闭会话的顺序未定

- **位置**：`docs/reviews/2026-10-27-tui.md:18-21`；`tui/mod.rs::local_host/select_session`；`tui/view.rs` 的 Session 状态
- **问题**：本地“我的电脑”是 UI 合成项，设计稿没有说明它何时出现、出口关闭时是否仍显示、`HOMEWAY_TERM=off` 时如何提示。侧栏状态只靠 LIST 轮询；attach 收到的 `STATE` 只更新标题，不更新选中会话的 agent/state，用户看到的“运行中/待操作”可能滞后。`CloseSession` 在发 KILL 前就 `detach`，KILL 失败时会话还在但界面已断开；`DeleteHost` 同样先 detach。
- **建议**：把本地伪主机和 target 规则写入设计；`STATE` 按会话名更新选中项；`ENDED` 后保留会话直到刷新再移除；KILL/删除主机先等待成功或至少保留错误后的可操作状态，不要在结果未知时清空 UI。
- **严重程度**：中

### 中-9 host/role/proxy 多步操作的部分成功与幂等语义

- **位置**：`docs/reviews/2026-10-27-tui.md:13-16`；`tui/backend.rs::execute` 的 AddHost/SetRole/SetProxy；`daemon_cli.rs` 的 host add 预算
- **问题**：`host.add`、`serve.start→serve.token`、`socks.on→取 listen` 都是多步操作；后一步失败或超时，前一步可能已经生效。UI 把整体报成失败，重试会撞 `host_exists`、`already`；角色 `stop` 是异步收尾，界面却立即显示“已关闭”。`host add` 的 20s 预算被 TUI 的 3s 截断，也属于同一问题。TUI 还没有暴露 `force`/名称可选等既有 host add 语义。
- **建议**：把每个操作拆成“提交结果 + 对账刷新”，部分成功要明确提示“已启动但 token 获取失败”；重试前先 refresh/解析 already；stop 显示 stopping 或等待状态；host add 暴露 force/可选名字，并校验 token/名称格式。补超时后宿主已变更、token 失败、already 等测试。
- **严重程度**：中

### 中-10 新增的 LIST `agentLabel` 未进入设计，也未被 TUI 消费

- **位置**：`docs/reviews/2026-10-27-tui.md:18`；`term/service.rs` 的 `agent_label`；`tui/backend.rs::Session`；`docs/INTEROP-CRITERIA.md:605`
- **问题**：实现给 LIST JSON 增加了可选 `agentLabel`，用于保留 pi 等固定枚举之外的身份；这是 additive 兼容，但设计稿仍写“消费既有 title/agent/state/cwd”，TUI 的 `Session` 也没有该字段。结果是把 `agent` 固定枚举显示成 `other`，新增识别能力没有被消费；同时新增 JSON 字段缺少旧消费者测试和设计记录。
- **建议**：设计稿明确 `agentLabel` 的来源、回退和展示规则；TUI 优先显示 label、缺失时回退 `agent`；加旧 LIST 消费者/空字段/退出进程不沿用缓存的测试。若首版不需要，就不要增加这个字段。
- **严重程度**：中

### 中-11 验证/十项验收和门一记录仍是占位，无法作为评审证据

- **位置**：`docs/reviews/2026-10-27-tui.md:28-38`；`ROADMAP.md` 评审协议；`docs/INTEROP-CRITERIA.md:606`
- **问题**：设计稿没有 ROADMAP 要求的设计要点（对齐表/风险/拆步），也没有列出所谓的“十项验收”判据。`INTEROP-CRITERIA` 说“验证与两道门见 TUI review”，但该文档的对应节仍是“待记录/待实现”。这样无法记录门一处置，也无法证明 `/tmp` 私有实例、离线/超时、输入隔离、raw 应答、宽字、剪贴板、信号收尾等关键面。
- **建议**：在开工前补一张验收矩阵：私有 state/端口、host add/remove、serve/relay stop 与会话保留、离线/超时/取消、快速切主机/会话、raw replay 应答、宽字/焦点、剪贴板四态、panic/信号/初始化失败、Windows/OHOS 编译面。每条写判据、证据路径和“不代表发布/部署”。门一记录必须包含本清单的处置。
- **严重程度**：中

## 低

### 低-1 CLI/UX 契约没有回填设计稿，且有自动拉起副作用

- **位置**：`docs/reviews/2026-10-27-tui.md:9-10`；`tui/mod.rs::options`；`tui/backend.rs::Worker::client`
- **问题**：设计稿只写 `homeway tui`，没有记录当前已实现的 `--state/--theme/--help`、非 TTY 拒绝、Windows 明确报错；`--state=DIR` 等号形不支持，也没有 `--no-spawn`。TUI 首次刷新会经 `dial_control_spawn` 自动拉起统一进程，可能同时按 config 启动角色；这对管理界面可以接受，但应作为显式行为写入设计。`F10` 在添加主机对话框中也直接退出，容易丢输入。
- **建议**：把 CLI 契约和自动拉起策略写入设计；需要时补 `--no-spawn` 和等号形解析；对话框内至少确认/屏蔽 F10；帮助文案与 Windows/非 TTY 行为加测试。
- **严重程度**：低

## 可精简处

1. **控制面刷新**：用 `daemon.status`（hosts/roles/generation）+ `socks.status` 替代 `host.list + snapshot.get + subscribe + serve.status + relay.status`；删除未消费的 `subscribe(view)`，它当前既不能保活也不能提供状态。
2. **term 客户端**：抽共享 client，删除 TUI 与 `term_cli` 的 greeting/HELLO/LIST/CREATE/KILL 双实现；至少用 `fixtures/term` 做两客户端对拍。
3. **刷新策略**：host 静态信息缓存，会话只在主机切换/显式动作/STATE 变化时刷新；2s 全量 refresh 与 6 次串行请求没有必要。
4. **绘制**：用 `dirty_rows`/合并帧或“有数据才画”替代每 25ms 全量 `rows()`；大窗口下减少逐 cell 分配。
5. **本地 term 启动**：如果并非所有统一进程都需要本地终端，改为按需/显式启用；若确认恒开，至少把失败降级和资源成本写清。
6. **`agentLabel`**：要么在 TUI 中消费并回退，要么首版不要新增该字段；不要留下“服务端加了、客户端不读”的半截能力。

## 看过无问题的方面

- **复用方向**：没有引入 SSH 或另一套会话服务；host/role/proxy 分别落在 `host.*`、`serve.*/relay.*`、`socks.*`，会话落在既有 term LIST/CREATE/KILL/raw attach 上。方向正确。
- **协议帧兼容**：没有新增 term op 或协议版本；使用既有 HELLO/ATTACHED/LIST/CREATE/KILL/DATA/STATE/ENDED。LIST 的 `agentLabel` 是 additive JSON 字段，旧消费者可忽略（但需按中-10 消费/记录）。
- **自动识别**：LIST 的 title/agent/stateV2/cwd 已被消费；`stateV2` 未知值有回退；`agentLabel` 的缺口见中-10。
- **raw 终端隔离**：输出不直接透传外层终端、交给 `SessionVt` 按 cell 绘制，能避免 ANSI 覆盖侧栏；宽字按 `width/skip` 处理的方向正确。应答归属仍需按高-2 补齐。
- **平台/依赖隔离**：ratatui/crossterm 只在非 Windows CLI 目标；core/capi 未引入 TUI 依赖；`cfg` 门和 Windows 明确报错存在。OHOS CLI 交叉 check 通过（仅编译面，不代表 OHOS 运行时）。
- **布局与鼠标**：左上主机、左下会话、右终端的基本几何；鼠标命中与绘制共用 geometry；滚动 offset、窄屏提示、菜单/弹窗屏内裁切；方向正确。
- **主题与状态文字**：主题单独存 state（`tui-theme`），颜色之外有中文状态，不是只靠颜色；文件结构和 ANSI 调色板仍需按中-5 补。
- **资源收尾意图**：RAII 还原 raw/备用屏/鼠标、panic hook、SIGINT/SIGTERM/SIGHUP 处理和“退出只分离不 KILL”的方向正确；初始化失败、后台 panic、日志和 join 细节见中-7。
- **测试纪律**：设计明确只用 `/tmp` state 与独立端口，不碰现役进程/state；方向正确，但验收矩阵尚未落成。
- **Rust 形态**：`thiserror` 错误类型、enum 状态、target 特定依赖、有界 mpsc/取消位，未看到逐行移植痕迹；不以未完成代码判完成度。

## 门一结论

**技术方向通过，门一材料不通过，建议“有条件通过整改”后复评。** 开工前至少要闭环高-1 到高-5；高-1 的本地终端生命周期、高-2 的 raw 应答归属、高-3 的 request/input epoch、高-4 的破坏性确认、高-5 的预算/取消/部分成功，都是会直接影响数据安全或会话正确性的决策。中-10/中-11 需要回填设计稿并形成可核查的验收证据；其余中项可以边实现边处置，但必须在代码评审前登记。

当前实现可以继续，但在上述高项定案前，不建议把本批记为“门一通过”，也不建议直接进入门二。评审处置应回填 `docs/reviews/2026-10-27-tui.md`，由同一实现子任务复评；本输出未修改任何代码或文档，也未启动生产进程。
