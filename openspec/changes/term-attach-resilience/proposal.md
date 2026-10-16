# term-attach-resilience — 提案

## Why

2026-10-16 真机实测发现 term attach 劣化路径上的三个连环缺陷：

1. **TUI attach 后画面不完整**：opencode 这类按行差分渲染的 TUI，attach 后只有持续重写的动画行（如 loading 条）可见，静态内容全空，手动 resize 窗口才恢复。根因 = 尺寸哨兵的两次 SIGWINCH 在同一把锁内背靠背发出，被内核合并成一个；App 处理信号时读到的最终尺寸与原尺寸相同，直接跳过重绘（真机对照实验：vim 背靠背仍重绘 2780B，opencode **0 字节**；间隔 300ms 则重绘 14732B）。
2. **断网即退出、恢复不自愈**：网络中断时 QUIC 连接死 → term 流死 → `homeway term attach` 直接退出进程。服务端会话与 PTY 完好保留、self_reconnect 替换旧腿的注册语义也已备好——只差客户端重连这一口气。
3. **退出后 shell 留数字乱码**：应答让位设计下（raw 腿在场服务端不代答），TUI 的能力查询直达本地真终端；断链退出竞态里终端排好的孤儿应答（如 DA1 的 `62;1;2;…c`）在进程退出后漏给 shell 被回显。

三者在同一条「attach 劣化路径」上且体验上互为因果（重连后的完整画面依赖①、重连等待掩盖不了③的最终退出），一个需求内同批修。

## What Changes

- **哨兵重绘加间隔（启用面收窄到「attach 几何未变」）**：尺寸哨兵现有 4 个触发点中，仅 attach 且本次未发生几何应用的那一处改为间隔形态——第一跳原位（sentinel 计数与现状同口径），第二跳（恢复真实尺寸）由延迟线程隔约 300ms 执行，触发时锁内五重验（在册未 done / gen 一致 / 未 stopped / 会话尺寸仍为发起值 / 恢复），期间真实 resize 落入则让位。**真变路径（RESIZE / 输入几何切换 / end_leg 重选举 / 带几何应用的 attach）保持现状背靠背形态**——真实 SIGWINCH 本身已足够逼重绘（用户手动 resize 有效的机理），且避免「apply+第一跳合并落中间值」的新窄列瞬态。
- **attach 断链重连循环（仅远程面）**：CLI attach 主体改为连接生命周期循环。链路级失败（读腿 Io/EOF/StreamEnded、写失败）在远程目标下不再退出进程——保持本地终端 raw、显示一行重连横幅、按退避重拨（同 client_id ⇒ 服务端按 self_reconnect 替换旧腿；`new` 创建的会话重拨自动降级创建位，不再被 already_exists 打死）；ENDED 帧、ERROR/会话级错误、用户退出照常退出。本地面（UDS 直连）断链即终局——出口进程已死、会话随之消失，重拨永无成功，走既有可行动文案。等待期持续读 stdin 喂分离键状态机、孤立 Ctrl-C 字节视为退出、其余丢弃（不注入恢复后的会话）。
- **退出排空**：`finish()` 在还原 termios 之前做一次有界排空——`poll(约 50ms)` 等在途应答落队后 `tcflush(TCIFLUSH)` 丢弃全部排队输入（不区分形态；竞态击键与既有发送失败丢弃语义一致）。不迁移应答分类器。
- **拨号错误类型化**：`attach_dial`/`dial_term` 错误从裸 `String` 改为类型化（会话级/传输级/协议级），支撑重连判定矩阵；重拨失败路径显式关闭控制连接（ControlClient 无 Drop，无限重试下会 fd/线程泄漏直至 EMFILE——既有隐患，本批收口）。
- **判据登记**：哨兵间隔、重连退避、退出排空等行为常量与用户可见面（重连横幅新串、断链三态文案产出面收窄）按硬规则在 `docs/INTEROP-CRITERIA.md`「判据变更记录」登记（日期/条目/从→到/原因/影响面，同批 commit）。

不改 wire 协议字节（frames 契约零变更）；服务端会话注册表/词表语义不变；surface（App）腿路径不受影响。

## Capabilities

### New Capabilities

- `term-attach-resilience`: term attach 劣化路径的韧性行为——attach 后的 TUI 全屏重绘保障（间隔哨兵）、raw 腿断链自动重连、退出前终端查询应答排空。

### Modified Capabilities

（无——`openspec/specs/` 尚为空白，本变更全部以新能力立目。）

## Impact

- `crates/homeway-core/src/term/service.rs`：attach 路径哨兵分档 + 延迟恢复线程（`sentinel_repaint_locked` 相关签名升 `Arc<Self>` 的涟漪）；`crates/homeway-core/src/term/`（config 面：`sentinel_gap` 字段）。
- `crates/homeway-cli/src/term_cli.rs`：attach 主体重构——连接循环化（每连接一段运行体）、连接段/终局段拆分、拨号错误类型化、重连横幅与退避常量、等待期交互面、退出排空；`tty.rs` 相关（标题查询与 client_id 进程级一次）。
- `docs/INTEROP-CRITERIA.md`：判据变更记录登记 5 行。
- `docs/reviews/`：两道评审门记录（技术评审随本方案评审落档、代码评审随完工落档）。
- 测试面：service 单测（哨兵延迟恢复的让位/幂等/不变量反证）、CLI 决策矩阵真值表单测、openpty 排空单测；CI 面用 vim + 自造差分渲染探针，opencode 归真机/用户触点档。
- 风险面：间隔期 vt 与 PTY 几何不一致窗（≤300ms，surface 快照可能错位、随后自愈）；多哨兵叠加可延长瞬态（竞态重连场景，登记不设机制）；每次退出付 ≤50ms 排空窗；本地面写超时 30s 的既有边界不在本批修。
