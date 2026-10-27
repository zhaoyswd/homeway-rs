## 结论

- **技术门（门一/方案）：有条件通过。** 用权威 surface 取代本地 VT 重解析、observer 协商、自动主题单事件流、PID 祖先识别，方向是对的；但“自动主题是否回灌共享 VT”和“自会话关闭语义”需要明确边界。
- **代码门（门二）：不通过。** 直接硬失败是 CI 的 `clippy -D warnings` 已红；同时 `docs/reviews/2026-10-27-tui-usability.md:58` 的门二记录仍为“待回填”。单测和私有 smoke 通过不能抵扣这两项。

## 1. Surface / 历史 / 裁剪

| 位置 | 问题 | 建议 | 严重 |
|---|---|---|---|
| `crates/homeway-cli/src/tui/surface.rs:158-160` | `fetch_reply` 对合法的 `count == 0` 直接返回 `Ok(false)`，但不清 `pending`。若服务端因 history trim/预算返回 0 行，客户端要等 `FETCH_WAIT`（2s）后才重试；同一区间持续返回 0 时会反复空白重试。 | 对已匹配请求的 0 行应答清 `pending`，并按当前 `history_base` 重新计算 `from` 或标记 miss 后立即重试；补一个 trim/budget 边界测试。 | 低 |
| `crates/homeway-cli/src/tui/surface.rs:48-176` | 看过 SNAPSHOT/DIFF/FETCH_ROWS 的 `revision / seq / history_epoch / base / cols / rows` 校验、过期应答拒绝、`seq+1` 断档恢复，**没发现问题**。 | — | — |
| `crates/homeway-cli/src/tui/surface.rs:182-346` | 看过绝对锚点、`base`/`offset` clamp、新输出保持锚点、回底恢复跟随、缓存 cell/row 上限；缓存上限与 `MAX_VIEWPORT_CELLS` 相容，**没发现问题**。 | — | — |
| `crates/homeway-cli/src/tui/view.rs:670-729` | 看过权威行按 slice 顺序绘制、右边界宽格只画空格、尾格不落边框、TAB/组合格安全投影；未重新折行或窄 VT 重解析，**没发现问题**。 | — | — |

## 2. OBSERVER 协商、注册与输入接管

| 位置 | 问题 | 建议 | 严重 |
|---|---|---|---|
| `crates/homeway-core/src/term/service.rs:1646-1653`（配合 `1695-1726`） | `passive` 只决定是否 `note_activity`，不决定是否写 PTY。非活动 observer 的纯终端应答 `Text` 仍会被编码写入 PTY；而 DATA 路径在 `1467` 会丢弃非 active 腿的应答，两条路径语义不一致。当前新 TUI 不发这类应答，所以暂不可达，但未来/其它 observer 客户端可能污染 active 程序。 | observer 未 active 且事件为纯应答时丢弃，或明确要求 observer 客户端不得发送自动应答；补 observer `Text` 应答不落 PTY 的测试。 | 低 |
| `crates/homeway-core/src/term/service.rs:1194-1201`、`session.rs:385-407`、`session.rs:556-574` | 看过 observer 仅 surface、禁止 takeover、创建前失败；注册不抢 active；非活动 RESIZE 只记偏好；活动腿退出时不自动选举未输入 observer；真实按键/粘贴/应用鼠标才接管。**没发现问题。** | — | — |
| `crates/homeway-core/src/term/session.rs:402-407`、`service.rs:1444-1453` | 旧 CLI/App 未置 observer 的路径仍走原逻辑；旧服务缺 feature 在 HELLO 前失败；`features::ALL` 的未知位旧客户端忽略。**没发现问题。** | — | — |

## 3. 自动主题

| 位置 | 问题 | 建议 | 严重 |
|---|---|---|---|
| `crates/homeway-cli/src/tui/mod.rs:101-108`；`crates/homeway-cli/src/tui/transport.rs:425-439`；`crates/homeway-core/src/term/service.rs:1767-1770` | 自动检测到的主题只更新 TUI chrome；新 TUI 已移除本地 VT/代答路径，但没有任何 `Op::THEME` 发送入口。共享 VT 的默认色仍来自旧 active 腿或保持默认，因此会话内程序查询 OSC 10/11、或依赖默认背景判断深浅色时，可能得到旧主题/无应答；旧 TUI 的本地 VT 曾用当前主题代答。若本轮验收只要求 TUI chrome，这是未登记的边界；否则是自动主题未闭环。 | 在首个完整 snapshot 后及主题变化时发送 `Op::THEME`（observer 阶段由服务端缓存，激活时 `apply_active_theme_locked` 生效）；补一个真实程序中 OSC 11 查询的亮/暗测试。若不做，应在设计记录中明确“应用主题查询不在本轮范围”。 | 中（边界确认项） |
| `crates/homeway-cli/src/tui/appearance.rs:64-147` | 看过单 Crossterm 事件流、完整合法 OSC 11 才消费、BEL/ST、RGB 深度、分片、超时/失配原序返回、`Paste/Resize` 不误消费、无第二个 stdin reader、探测有界和 fallback。**没发现问题。** | — | — |

## 4. 自 attach / 异步与资源

| 位置 | 问题 | 建议 | 严重 |
|---|---|---|---|
| `crates/homeway-cli/src/tui/view.rs:1177-1185`、`view.rs:760-767`、`mod.rs:919-928` | `select_session_row` 在判断 sel f 之前就改了 `selected_session`。若当前正附着 A，左键/回车选到“自身” S，终端仍显示 A，但侧栏选择和后续 `m`/关闭菜单已指向 S；菜单显示“关闭自身并退出”，而 `Action::CloseSession` 只发 KILL，没有显式退出 TUI，是否退出依赖宿主 PTY/信号。存在“看着 A、操作 S”的状态错位和退出语义不闭环。 | 自会话判断前不要改选中项；若需要右键关闭 self，用独立的菜单目标或 `CloseSelf`，并在关闭确认后显式退出（或改文案不再承诺“并退出”）。补“已附着 A + 选中 S”测试。 | 中 |
| `crates/homeway-cli/src/tui/backend.rs:615-620` | 每次本地 LIST（约 2s）都全量 `read_procs()` 再重算 self 标记；空 session 也采样。若进程表采样瞬时失败/不完整，之前标记的 self 会变回 false，出现可选中承载会话的窗口；同时有大进程表时是无谓开销。 | 空列表跳过采样；按 name+pid 保留上一轮 self 标记，采样失败时不要静默清空（至少提示/保守处理）；或复用已有进程采样缓存。 | 低 |
| `crates/homeway-cli/src/tui/backend.rs:136-157`、`transport.rs:279-448` | 看过本地 PID 祖先匹配、远程同名/同 PID 不标记、真实 smoke 的嵌套 TUI 禁 attach；异步 attach/input/切换 generation、有界队列、取消、退出不 KILL。**没发现问题。** | — | — |

## 5. Rust 质量与门禁

| 位置 | 问题 | 建议 | 严重 |
|---|---|---|---|
| `crates/homeway-cli/src/tui/appearance.rs:239`；`crates/homeway-cli/src/tui/view.rs:1593-1594` | CI 的 `cargo clippy --workspace --all-targets -- -D warnings` 失败：生产代码 `appearance.rs:239` 触发 `question_mark`；新测试 `view.rs:1593` 触发 `field_reassign_with_default`。这不是“建议”，是当前代码门硬红。 | 把 `else if let Some(hex)` 改为 `let ... else`/`?`；测试用 `Model { notice: "READY".into(), ..Default::default() }`。修后重跑 `--all-targets -D warnings`。 | 中（门禁） |
| `docs/reviews/2026-10-27-tui-usability.md:58` | 门二仍是“待源码冻结后按影响回填”，没有测试/clippy/真实验证/边界结论记录。 | 回填 targeted tests、clippy、私有 smoke、debug SHA256，以及自动主题/自关闭的明确边界。 | 中（流程门） |
| `crates/homeway-cli/src/tui/{surface,appearance,input}.rs`、core observer 改动 | 看过 thiserror 类型、`pub(super)`/`pub(crate)` 可见性、资源 Drop/取消、测试可读性；未发现逐行移植式抽象或字符串错误。**没发现问题。** | — | — |

## 验证状态（只作证据，不代替门禁）

- `cargo test -p homeway-cli tui::`：62 passed。
- `cargo test -p homeway-core --lib observer`：4 passed。
- `cargo test -p homeway-core --lib term::frames::tests`：11 passed。
- `tools/tui-observer-smoke.py --bin target/debug/homeway`：通过（observer 窄窗、真实输入接管、raw 腿回夺、承载会话自嵌套拦截、创建/分离）。
- `tools/tui-smoke.py --bin target/debug/homeway`：通过（单行布局、自动主题、resize、真实 ls、分离重进、SIGTERM 恢复）。
- `cargo clippy -p homeway-cli -p homeway-core --all-targets -- -D warnings`：**失败**，`pipeline=101`；`homeway-core` 单独 clippy 干净。
- `fixtures/term/frames.v1.jsonl`、`fixtures/MANIFEST.md` 的 SHA256 与 `fixtures/SHA256SUMS` 一致；`git diff --check` 干净。
- 未跑全仓 `cargo test --workspace` 和 fuzz seed 展开门，不能据此声称全仓门通过。

**建议收口顺序：** 先修 clippy（硬门）→ 明确/修复自动主题回灌 → 修 self 选中/关闭语义 → 回填门二记录 → 再复跑 clippy + targeted tests + 两条 smoke。
