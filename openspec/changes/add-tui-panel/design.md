## Context

出口侧能力已建成：多腿同显（8 腿/会话上限，满员按「最久停滞→最久未活跃」腾位且裸断不发终止通知）、回放环 + 尺寸哨兵、LIST（含 agent/stateV2/title/cwd/clients[]）、agent 状态识别（stateV2 ∈ working/blocked/idle/unknown）。客户端侧：daemon HostTable + 事件总线（`snapshot.get` + `events.subscribe`，订阅域冻结为 link/session）；term 通路 `stream.open kind=term` 纯字节透传。依赖树内已有 alacritty_terminal 0.26，ratatui/crossterm 为零。

**控制面词表是跨仓冻结契约**（`daemon/vocab.rs:1-4`：spec daemon-control-plane 冻结的受控枚举，只增不改，新增走后续 spec delta；tier 侧订阅域初始集钉死 link/session；`daemon/tests.rs:43` 断言冻结夹具恰 43 帧）。技术评审（docs/reviews/TUI-design.md，2026-10-11，31 条意见）后本设计定案 **A 案：控制面零新增**——全部新逻辑落在 TUI 进程与本机文件，daemon 侧仅一处可见性公开。

约束：TUI 为纯客户端进程（不 acquire `<state>/lock`、不撞 identity session.lock）；线上协议（term 帧、QUIC、capi、tier 对齐面）零改动；本仓对空闲唤醒敏感，一切轮询要有账。

## Goals / Non-Goals

**Goals:**
- daemon **零契约面改动**（无新 op、无新事件域、无响应字段变更）；core 仅一处可见性公开；
- 三条数据通路（主机状态 / 会话列表 / 终端视图）与全局唯一的选型落地；
- 状态行三项的口径定义（全部用既有数据面）；
- 腿占用边界与三态断线恢复的机制设计（评审 R-2/R-5/D-2 的整改主战场）。

**Non-Goals:**
- 面板外提醒（系统通知/响铃）、Socket API 反向开放、会话元数据服务端推送；
- surface 腿消费端（dec_snapshot/dec_diff 生产化）、延迟实时化（`IslandSnapshot.rtt_ms` 因中继形态语义不同〔客户端↔中继 ≠ 端到端〕不能直接替换既有 `rttMs`，实时化留作后续 additive 变更）；
- App/capi/tier 侧任何改动；判据行变更（不预期）。

## Decisions

### D1. 进程与线程模型：homeway-cli 内新模块 `tui/`，纯同步多线程 + 事件驱动渲染
主线程跑 UI 事件循环 + 渲染；后台线程 = 控制面事件流、LIST 轮询、终端腿读帧、状态行快照轮询。不引入 tokio。
渲染节拍（评审 D-6）：**事件驱动 draw**——任一数据源更新或 500ms 心跳超时才触发一次重绘合并，无固定帧率空转；数据通道有界但**不静默丢字节**——终端腿读帧线程与 UI 之间的字节/帧通道满时读线程阻塞（背压沿隧道自然回传），宁可滞后不可失步；屏幕内容按合帧策略合并（UI 跟不上时丢的是中间渲染拍，不是字节）。

### D2. 终端视图：raw 腿（不声明 capsRawTerminal）+ 内嵌 alacritty_terminal 0.26 + 腿占用边界
- HELLO 不带 SURFACE、不带 RAW_TERMINAL caps 位 → 服务端代答终端查询不被抑制（让位规则只由 RAW_TERMINAL 位决定，`service.rs:2130-2133`）；kind = legacy，选举/腾位无歧视待遇；
- 内嵌模拟器与出口同版本同实现；DATA 回放喂入 → 网格 → 渲染进 pane；scrollback 用模拟器自带历史；重附自动回放 + 尺寸哨兵（PTY 侧 SIGWINCH nudge，`service.rs:1296-1298/1877-1900`）保证屏幕恢复；
- **接入前腿满检查**（评审 R-2）：LIST 响应含 `clients[]`，腿已满 SHALL NOT 接入（不触发服务端腾位），呈现提示；接入后每会话仅本面板 1 腿；
- **三态断线判定**（评审 R-5/D-2）：收到会话终止通知，或重附后 LIST 确认会话不存在 → 停拨 + 「会话已结束」；裸断且 LIST 显示腿满/自己不在 clients → 判定被腾位 → 长退避 + 提示，**不抢占重连**；其余流断 → 链路中断 → 自动重拨 + 回放重附。**不做心跳保活**——raw 腿刷活跃度的原语只有 INPUT（注入 shell 字节）与 RESIZE（改会话全局 PTY 尺寸），都有害；
- 尺寸策略（评审 D-13）：仅用户交互的本实例（终端焦点下按键/自身窗口变化且为活动腿时）发送 pane 尺寸 RESIZE；纯镜像实例与旁观状态 NEVER 下发 RESIZE；
- 输入（评审 D-4）：crossterm 键鼠事件 → **客户端 keyenc 编码 → DATA 字节帧**（与 `term attach` 同路，term_cli.rs:1724 先例）；INPUT(0x11) 是 surface 家族结构化帧（服务端编码），本方案不使用。

### D3. 会话列表：选中主机 LIST 短连接轮询，自适应拍
沿 `term list` 同款路径（`stream.open kind=term` → LIST 往返 → 断开，**不占 8 腿预算**，`service.rs:973-984`）。拍长自适应（评审 D-10）：面板前台可见 2s、失焦/空闲 10s；仅轮询当前选中主机。显示排序 lastActiveMs 降序（LIST JSON 字段齐备）、会话名破平；选中以会话名为键。徽章映射：`agent ∈ {shell, unknown}` → 无 agent；否则 stateV2 → working/blocked/idle/unknown（评审 R-3：真源无 done 态）。
**备选**：常驻观察腿收 STATE 推送——白占腿预算且有腾位风险；不取。

### D4. 状态行：既有数据面如实显示，零 daemon 改动
- 连接状态/形态：`snapshot.get` 初值 + `events.subscribe(link, session)` 增量（复用 `status --watch` 骨架，含 cursor 回放 + generation 门；本方案不新增订阅域，无老 daemon 代际问题）；
- 延迟：`HostState.link.rttMs` 端到端探活口径如实显示（60s 巡检拍，新鲜度在 UI 标注）；**评审 D-3 后放弃改动①**——quinn 平滑 RTT 在中继形态下量的是客户端↔中继，直接替换会静默改变既有字段（含 App 面 `tun_status`）的值语义；
- 流量：`HostState.stats{rxBytes,txBytes}`（岛 STREAM 流累计，事件不含）由 TUI 状态行拍（1s `snapshot.get`）差分出速率，口径标注「应用流」；
- 轮询账：1s 快照拍 + 2s LIST 拍仅在前台运行期存在，量级与 `status --watch` 的 500ms 心跳同级；**量化验收任务见 tasks 5.5**（空转面板 vs 基线的 CPU/上下文切换/流计数对照）。

### D5. 本机条目：TUI 合成，可用性 = socket 可连
daemon 主机表只有远端后端（键 = token peer_id），本机条目由 TUI 合成。路径 = `<state>/serve/term.sock`（TUI 既已 dial `<state>/control.sock` 必然知道 `<state>`；`serve_dir = state_dir.join("serve")` 恒定）——**评审 D-8 后删除 serveDir 字段改动**。可用性判据 = 本地终端服务 socket 可连接（`HOMEWAY_TERM=off` 时 serve 在跑也无 term.sock，评审 R-8）；**不复用 `term_cli` 本地 dial 段**——其 `--state` 约定指向 serve 子目录，照抄会连错路径（评审 D-14）。连接形态标签 `local`。

### D6. 全局唯一 = 选中状态文件（评审 D-1 定案 A 案）
- 选中状态（revision 单调递增 + 主机 id + 会话名）落 `<state>/cache/tui-selection.json`；实例切换选中即写文件；各实例 300ms `stat`/mtime watch + 变更读入，镜像 ≤1s（本机文件 stat，不唤醒 daemon、不经 control.sock）；
- **为什么不走 daemon 词表**：`tui.state.get/set` op + `tui` 事件域是对跨仓冻结契约（daemon-control-plane）的 delta，需 tier 侧 spec delta（用户触点）+ 冻结夹具面 + 白名单面联动，成本与风险不成比例；文件方案零契约面；
- **为什么落 cache/ 而非 client/**：`artifact.rs` 的 CLIENT_PIECES 白名单覆盖 client/ 下文件（未登记文件导入直接拒）——选中是设备本地偏好，不随 state export/import 迁移本就合理，落 cache/ 免白名单联动（评审 D-9 随之消解）；
- 并发写 = last-writer-wins（选中语义可接受）；文件损坏/缺失 = 视为无持久化选中，走回落链；
- **留档备选（B 案）**：若未来需要跨设备同步选中或事件驱动镜像，再走 additive delta（op×2 + 域×1 + kind×1，tier 侧同批登记）。

### D7. core 最小公开面（评审 D-5）
kitty 键盘位常量（`KF_*`）与模式位换算目前私有，客户端编码无公开取法。core 侧新增**仅可见性**的最小公开访问器（`term/keyenc.rs`/`vt.rs` 暴露模式位 → kitty flags 换算），无行为变化、无 wire 变化。

### D8. 键位与焦点模式（评审 R-1 定案）
双焦点模式钉进契约（见 spec）：面板导航焦点 / 终端焦点；终端焦点下除一个固定的「退出终端焦点」组合键外全部原样转发。具体键位实现期定，约束：退出组合键避开常见 TUI 应用的组合空间（建议功能键或罕见 Alt 组合），面板导航键仅在导航焦点下生效。

### D9. 依赖、模块布局与交叉编译
- `crates/homeway-cli/Cargo.toml` 新增 ratatui、crossterm；alacritty_terminal 走 **workspace 依赖继承**（顶层已锁 0.26），防两处版本漂移；
- `crates/homeway-cli/src/tui/` 模块：`app.rs`（状态机/选中路由/回落链）、`backend.rs`（控制面 + term 通路 + 选中文件）、`hostlist.rs`/`sesslist.rs`/`termview.rs`/`statusline.rs`、`keys.rs`（焦点模式）；
- **三目标交叉 check（评审 D-11）**：CI 与本地门把 homeway-cli check 到 aarch64-ohos + musl×2，ratatui/crossterm（mio/libc/signal 面）必须三目标可编译；编不过则 `cfg` 门控 tui 模块并在 design 补记。

## Risks / Trade-offs

- [腿满/腾位场景的体验] 接入被拒与被让位都是一等状态（有提示、有退避），不是静默失败 → spec 钉死；腿占用在接入前经 LIST 检查，正常情况下不会触发服务端腾位。
- [kitty 键盘模式协商] 内嵌模拟器模式位与 keyenc 编码一致性 → 单测覆盖模式位→编码矩阵（D7 公开访问器后可测）。
- [文件方案的事件延迟] 300ms stat 轮询 + 文件读入，镜像 ≤1s 有裕量；本机 stat 无唤醒账；仅 TUI 存活期间存在。
- [轮询开销] LIST 自适应拍 + 快照 1s 拍仅前台存在；量化验收（tasks 5.5）兜底，超标则降拍。
- [ratatui 交叉编译风险] 三目标 check 前置到任务组 1（tasks 1.4），失败即 cfg 门控，不阻塞其余设计。
- [多实例渲染漂移] 各实例自开腿回放起点略有差异（滚动位置为腿私有态）→ spec 只约束选中一致与会话内容一致。
- [镜像实例抢尺寸] 镜像实例不 RESIZE（D2），活动腿选举天然把尺寸权交给最近交互的腿。

## Migration Plan

纯增量功能，无迁移。上线顺序：ROADMAP 立线登记 → core 可见性公开（唯一 core 改动）→ TUI 模块骨架 → 三目标交叉 check 验证 → backend 三通路 → UI 四区 → 本地矩阵（local-rust-exit.sh + 双实例 + 腿满构造）验收 → 代码评审门。回退 = 不启用子命令（选中文件与 core 可见性改动残留无害，无契约面残留）。

## Open Questions

（无——评审 31 条意见已全部处置；键位具体值、自适应拍参数为实现期细节，不影响规格。）
