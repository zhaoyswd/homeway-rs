## 结论先说

- **门一：不能签通过（有条件）。** 高1–高4 的技术定案已基本落到代码，**高5 的刷新隔离仍不成立**，且 ATTACHED 模式重建、复评/验收记录未闭合。
- **门二：不通过。** 当前快照有 **2 高 / 4 中 / 1 低**；高项分别影响输入安全与“角色/创建不被刷新阻塞”的既定承诺。
- **r1 处置：高4 足；高1/高2/高3 基本足但各有余项；高5 不足。** 中3、中9、中11 仍有缺口；其余中/低项基本足。
- **基准：** 当前工作区 16:48 快照（只读源码；未运行 cargo/ci-local/真实 PTY，避免与正在跑的 ci-local 抢锁；未启动生产、未读 token）。评审期间源码仍有更新，若之后继续改动需增量复核。

---

## 高

### 1. ATTACHED 的终端模式被丢弃，长会话回放后 bracketed paste/鼠标/焦点/kitty 模式可能全错

- **位置**：`crates/homeway-cli/src/tui/transport.rs:306-311`；`crates/homeway-cli/src/tui/mod.rs:450-453, 700-717`；`crates/homeway-core/src/term/service.rs:1304-1311, 2931-2935`
- **问题**：`transport.rs:311` 调用 `dec_attached` 后把 `(prev_size, modes, agent, state, name)` 全部丢弃；TUI 的 `Op::ATTACHED` 只改底栏提示。服务端在 ATTACHED 里专门发了 `rt.scan.modes()`，但回放前序只写 `?47h/l + 3J + 2J + H`，**不包含** `?2004h`、`?1004h`、`?1000/1002/1003/1006h`、kitty flags、DECCKM 等。会话运行足够久、这些序列已滚出 replay ring 时，新建的 `SessionVt` 停在默认模式：`Action::Paste` 看到的 `bracketed_paste=false`，多行粘贴会被直接写进 shell，可能执行本应只是“粘贴文本”的命令；鼠标/焦点/kitty 键也会按错误模式编码。
- **建议**：把 ATTACHED 的 modes 随 `AttachEvent` 传入，在 `REPLAY_DONE` 后应用到 `SessionVt`（增加最小的 mode-apply API，或写规范模式序列）；至少覆盖 bracketed paste、鼠标 1000/1002/1003/1006、focus 1004、kitty flags、DECCKM、alt screen，并补“模式序列在 replay 窗口之前”的测试。
- **严重程度**：**高**

### 2. Refresh 仍与修改操作共用一个 worker，六个串行 3s 请求可把 AddHost/SetRole/KILL/CREATE 堵到 18s

- **位置**：`crates/homeway-cli/src/tui/backend.rs:16, 207, 256-285, 383-408`；`docs/INTEROP-CRITERIA.md:606`
- **问题**：查询 latest-only lane 只隔离了 `Command::Sessions`；`Command::Refresh` 仍在与 `AddHost`/`SetRole`/`SetProxy`/`NewSession`/`KillSession` 共用的 16 槽命令 worker 上执行。`snapshot()` 串行发 HostList→SnapshotGet→subscribe→SocksStatus→ServeStatus→RelayStatus 六个请求，每个 `REQUEST_BUDGET=3s`，控制面慢或半死时最坏占住 worker 约 18s；期间用户操作只能排队，队列满还会返回 `Busy`。这与 r1 高5“查询 lane 不阻塞角色/创建操作”及判据行“刷新/列表查询3s”不一致——被隔离的只有会话列表，刷新没有。
- **建议**：刷新独立成 single-flight/latest-only lane（独立 ControlClient 和 epoch），或缩成 `daemon.status + socks.status` 两次请求；修改命令队列不得排在刷新之后。若保留六请求，给整次 snapshot 一个总 deadline 并可取消。
- **严重程度**：**高**

---

## 中

### 3. 多步操作仍会把“已成功”报成失败，成功通知还会被刷新失败覆盖

- **位置**：`crates/homeway-cli/src/tui/backend.rs:443-451, 524-545, 563-580`；`crates/homeway-cli/src/tui/mod.rs:411-426`
- **问题**：
  - `SetProxy` 在 `socks.on` 已成功后，`listen` 缺失或为 0 时用 `?` 直接返回 `Error::Unexpected(0)`；代理实际已启动，但不会发成功 Notice，也不会刷新。
  - `KillSession` 在 KILL 返回 OK 后，`self.sessions(&target)` 失败会整体返回 Err；“会话已关闭”的 Notice 被丢弃，用户重试只会得到 no_session。
  - `with_snapshot` 在成功 Notice 后追加 `Event::Failed(error)`；`process_backend` 先处理 Notice 再处理 Failed，最终底栏被刷新错误覆盖。出口启动但取 token 失败、host.add 成功但刷新失败等场景，用户只看到“控制请求失败”，看不到已成功的事实（token 剪贴板任务仍会启动，但提示丢了）。这直接违反了 `with_snapshot` 注释和 r1 高5“刷新失败不能抹掉成功”。
- **建议**：把结果拆成“提交成功 Notice + 刷新结果/警告”两条独立通道；提交成功后先发不可覆盖的 Notice；`listen` 缺失时提示“代理已启动，未取到监听地址”并继续刷新；KILL 成功后即使 LIST 失败也提示“会话已关闭，列表刷新失败”并安排重试。
- **严重程度**：**中**

### 4. `local-term.sock` 仍按“存在”而非“存活”选择，死 socket 会阻断 `serve/term.sock` 回退

- **位置**：`crates/homeway-cli/src/tui/backend.rs:587-598`；`crates/homeway-cli/src/tui/transport.rs:56-61`；`crates/homeway-cli/src/term_cli.rs:568-579, 708-710`；`crates/homeway-cli/src/local_term.rs:22-36`
- **问题**：两处 `local_term_sock` 都是 `if local.exists()` 直接返回，没有 `probe_alive`；`transport` 也只检查 `path.exists()` 后直接 connect。统一进程被 SIGKILL、或 `LocalTerm::start` 已 bind 但线程 spawn 失败时，`local-term.sock` 会留下；此时前台 `homeway serve` 仍可正常监听 `serve/term.sock`，但 `homeway term` 和 TUI 都优先拨死的 local socket，连接失败且**不回退**旧扁平/serve socket。`LocalTerm::start` 的 spawn 失败分支只 `service.close()`，也没有删除刚 bind 的 socket。这正是 r1 中3 指出的“存在但已死”缺口。
- **建议**：选择前用 `ipc::probe_alive`/connect 探测；连接失败时按 local→flat→serve 依次回退；`LocalTerm::start` 失败时删除本次 bind 的 socket；补 stale socket 的 term/TUI 回归。
- **严重程度**：**中**

### 5. attach 后的 live `Op::ERROR` 被静默丢弃，bad_input/bad_resize 无提示且 resize 不会重试

- **位置**：`crates/homeway-cli/src/tui/mod.rs:450-502`（`_ => {}`）；`crates/homeway-core/src/term/service.rs:1424, 1494-1499`
- **问题**：服务端在 ATTACHED 后会通过同一条腿发 `ERROR bad_input`（例如另一客户端正在粘贴、粘贴分片顺序错误）和 `ERROR bad_resize`。`process_attach` 只处理 ATTACHED/REPLAY_DONE/DATA/ENDED/STATE，其余全部 `_ => {}`；`check_error` 只在握手/round_trip 使用。结果是：用户粘贴被拒但界面无提示、`connected` 仍为 true；resize 被拒时 `sent_size` 已经被置为新几何，后续也不会再成功重试，PTY 几何与本地 VT 可能长期不一致。
- **建议**：处理 `Op::ERROR`，`dec_error` 后写入 notice；`bad_resize` 时重置 `sent_size`、按服务端实际/上一次确认几何重试并明示；`bad_input` 保留会话但提示用户。
- **严重程度**：**中**

### 6. CreateSession 无单飞，重复按键/切换主机会留下“创建成功但无人导航”的孤儿会话

- **位置**：`crates/homeway-cli/src/tui/view.rs:944, 983-988, 997-1003`；`crates/homeway-cli/src/tui/mod.rs:654-658, 287, 379-410`；`crates/homeway-cli/src/tui/backend.rs:547-561`
- **问题**：`Action::CreateSession` 每次都会发 `NewSession { target, epoch: navigation_epoch }`，没有 pending/single-flight。连续按两次 `n`/`+`/Enter 会创建两个会话，携带同一 epoch；第一个 `Created` 进入 `select_session` 并递增 `navigation_epoch`，第二个 `Created` 因 epoch 过期被丢弃——第二个 shell 已经在远端跑起来，但 TUI 没有导航也没有提示。创建在途时切换主机/session 同样会让 `Created` 因 target/epoch 不匹配被丢弃，旧主机上留下孤儿会话。会话上限 16 时这会实际消耗资源。
- **建议**：创建按键做 pending 禁用或 single-flight；为每次创建携带独立 epoch；收到非当前上下文的 `Created` 时仍提示“会话已在主机 X 创建”并刷新原 target，而不是静默丢弃；补双击创建、创建中切换主机测试。
- **严重程度**：**中**

---

## 低

### 7. Ctrl+B 前缀状态不会被鼠标/粘贴/切换清掉，可能导致下一次按键被吞或直接退出

- **位置**：`crates/homeway-cli/src/tui/view.rs:922-1016`（仅在 Key 分支清 `prefix`），`:963-965`；`crates/homeway-cli/src/tui/mod.rs:265-276`（`detach` 不重置）
- **问题**：Ctrl+B 后 `model.prefix=true`；如果下一步是鼠标点击、粘贴、Focus 事件或主机/session 切换，`handle_event` 不会清 prefix，`detach` 也不清。用户按 Ctrl+B 后点击一个 session，再按 `q`，会被当成 `Ctrl+B q` 直接退出；按 Ctrl+B 后点击侧栏再输入命令，第一个键会被当作管理命令吞掉。它不会 KILL 会话，但会造成非预期退出/丢键。
- **建议**：任何非 Key 事件、主机/session 切换、`detach`、打开 modal、焦点丢失时清 prefix；可加短超时；补“Ctrl+B 后点击再按键”测试。
- **严重程度**：**低**

---

## r1 逐项处置复核

| r1 项 | 复核结论 |
|---|---|
| 高1 本地所有权 | **基本足**：共享 `TermService`、`serve stop/restart` 不 close、统一退出才 close、0600 UDS、真 PTY 测试均在；余死 socket 客户端回退（发现4）。 |
| 高2 raw 回放/应答 | **基本足**：`REPLAY_DONE` 前不回历史查询、live 应答、服务端 active raw 过滤均对；余 ATTACHED 模式重建（发现1）。 |
| 高3 generation/epoch | **基本足**：Sessions target+epoch、Created navigation epoch、attach generation、切换清输入均有；余创建单飞/孤儿（发现6）。 |
| 高4 破坏性操作确认 | **足（按主会话裁定）**：不加额外确认/force UI 不列为缺陷。源码 `core/daemon/mod.rs:354-364` 的 `remove_host` 只删 host 并级联 forward/socks/speedtest，**没有 term KILL**；`tui-remote-smoke.py` 也断言删除主机后远端会话仍在。 |
| 高5 预算/取消/部分成功 | **不足**：会话查询 lane 已独立，但 Refresh 仍与修改共用 worker（发现2）；多步成功结果仍会被 `Failed` 覆盖（发现3）。 |
| 中1/2/4/5/6/7/8/10 | **基本足**：TUI 适配器、AltEscPrefix 统一、剪贴板 0600 unlink、主题、sync flush、quiet spawn（`dial_control_spawn_quiet` 已实现）、本地伪主机、agentLabel 均已落实。中2 的“当前 VT 模式”仍受发现1影响。 |
| 中3 断链/本地回退 | **部分不足**：输入门控/重选恢复在；死 local socket 回退见发现4。 |
| 中9 多步部分成功 | **不足**：同发现3。 |
| 中11 验证/门记录 | **未闭合**：`docs/reviews/2026-10-27-tui.md:95,99,101` 仍是“待回填”；新 `tools/tui-remote-smoke.py` 已出现，但本次未运行，文档也未记录证据。 |
| 低1 CLI/自动拉起 | **足**：`--help`、非 TTY、Windows、F10 dialog、静默自动拉起均对；等号旗/`--no-spawn` 按本轮取舍未加。 |

---

## 门一 / 门二

- **门一：不能签通过。** 技术方向和高1–高4 的定案可接受，但高5 的刷新隔离未按定案落地，ATTACHED 模式重建仍是设计/代码共同缺口，复评结论与十项验收矩阵仍未回填。建议先修发现1/2，再回填 `docs/reviews/2026-10-27-tui.md` 后复评。
- **门二：不通过。** 先处理 2 个高项和 4 个中项，再重跑 `tui` 模块测试、`local_term` 共享 registry 真 PTY、`term/service` pi_label、`tui-smoke.py` 与新增的 `tui-remote-smoke.py`，并把结果写入评审文档。当前不建议把本批记为门二通过。

---

## 已看、未发现问题的方面

- **raw 回放/应答归属本身**：`transport.rs` 的 replay 测试覆盖历史 DA/DSR 不应答、live DSR 才应答；`service.rs` 的 `response_owner`/`pure_terminal_reply` 对非 active raw 应答做丢弃，方向正确。模式重建问题单列在发现1。
- **本地终端生命周期**：`UnifiedRoles.terminal` 共享给 `ServeEngine`，`term_srv` 只在 owned 时 close；`serve stop/restart` 保留会话；`LocalTerm` 在 state lock 释放前 drop；独立 `homeway serve` 仍独占。死 socket 回退问题单列在发现4。
- **agentLabel**：服务端每拍从当前前台进程重取，退出后回退固定枚举；`agent`/STATE/帧字节不变；pi/旧枚举测试在。TUI 优先 label、缺失回退 agent。
- **剪贴板**：0600 创建即 unlink 的临时文件作 stdin，避开 SIGPIPE/pipe join；native/OSC52 三态分开；OSC52 只提示“已请求”；helper 2s kill+wait。
- **resize/输入预算**：`Busy` 重试、`sent_size` 成功入队才确认、切换/离线清队列、2 MiB 输入上限、16 帧/拍调度；对应测试在。
- **终端恢复**：阶段 guard、panic hook、SIGINT/SIGTERM/SIGHUP 只置 atomic；raw/备用屏/鼠标/bracketed paste/focus/kitty push-pop 的 restore 都在；`dial_control_spawn_quiet` 已避免后台 stderr 污染备用屏。
- **宽字/焦点**：按 `cell.width/skip` 绘制，宽字测试覆盖；F6/F10 在侧栏/终端的不同语义、Release/Repeat、focus 编码按 VT 模式门控，方向正确。
- **平台隔离**：ratatui/crossterm 只在非 Windows CLI 目标；Windows 明确报不支持；core/capi 未引入 TUI 依赖；`dial_with_timeout` 已把远程 3s 列表预算带进 control 握手。
- **Rust 形态**：thiserror、enum、有界 channel、`Arc`/`Mutex` 用在共享服务/取消面；未发现明显的逐行移植或字符串错误回潮。
- **host.remove**：源码级确认不 KILL 远端 term 会话；`tui-remote-smoke.py` 的删除主机后 LIST 断言方向正确。

**验证说明**：本轮未运行 cargo/test/ci-local（避免抢锁），未启动生产、未读真实 token；以上代码结论均为静态证据，测试/脚本结果需以你后续 ci-local 与真实 PTY 运行回填为准。
