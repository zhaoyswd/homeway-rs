## 结论先说

- **门一：技术设计可通过（有条件）。** r2 的两个高项（ATTACHED 输入模式、Refresh 隔离）方向都已落地；7 项整改基本闭合。但正式签字仍缺“当前工作树同 SHA 的验证”，见下方证据限制。
- **门二：不通过。** 当前源码比最终 `a6dd` 二进制和 17:13 的最终测试更新，最终二进制/日志不覆盖当前树；此外还有 1 个中项、3 个低项。若按你给的 `a6dd` 快照评审，旧快照的中3（KILL 成功后 LIST 失败覆盖成功文案）也仍未修，因此两种口径下门二都不能签过。

> 本评审未改代码、未运行 cargo/ci-local/真实 PTY/生产进程；以下为静态代码审查加已有日志核对。

---

## 发现

### 1. [中] “整轮 3s 绝对预算”仍未覆盖 `connect(2)`

- **位置**：`crates/homeway-core/src/daemon/client.rs:270-272`；`crates/homeway-core/src/daemon/ipc.rs:148-151`；调用侧 `crates/homeway-cli/src/tui/backend.rs:497-515`。
- **问题**：`dial_with_timeout` 先在本地算出 deadline，随后调用 `IpcStream::connect`；Unix 分支实际是阻塞的 `UnixStream::connect`，没有把 deadline 传进去。监听端 backlog 满或挂死时，连接阶段可超过 3s/10s，所以 `snapshot` 的“整轮 3s 绝对预算”只在 **connect 成功之后**成立。现有测试从 listener `accept()` 之后才开始计时，没有覆盖慢 connect。
- **建议**：改成非阻塞 connect + poll 到 deadline（或等价的 bounded connect），并把“写锁等待/帧写入/应答/连接”统一到同一个绝对截止；若平台无法保证，至少在 API 注释和判据中明确“预算从连接建立后开始”，不要把 connect 算进绝对预算。
- **严重程度**：**中**。正常本机 UDS 很少触发，但这是本轮新增 API 的承诺边界。

### 2. [低] `operation_fact` 是全局单值，会串台或过期

- **位置**：`crates/homeway-cli/src/tui/mod.rs:165-176, 210`；`mod.rs:382-384, 423-467, 595-598`；测试 `mod.rs:930-953`。
- **问题**：当前源码用 `operation_fact` 保留成功事实，已能在 `Sessions Err`/`RefreshFailed` 时附在错误后，这一点比 `a6dd` 旧逻辑好。但它是全局单值，只在 `Failed` 和 `SelectHost` 清除；成功 `Sessions`/`Snapshot`、`SelectSession`、`CloseSession`、`CreateSession`、`ThemeChanged` 都不清。结果：
  - 后续无关的列表/状态刷新失败，可能拼上很早以前的操作成功事实；
  - `process_clipboard` 使用“最新 fact”，角色复制结果可能被拼到后来 KILL/创建/主题操作后面，或覆盖刚到的刷新警告。
  现有测试只覆盖“Notice→RefreshFailed→Sessions Err”的顺序，没有覆盖 stale fact 或剪贴板并发。
- **建议**：给操作/剪贴板任务带 operation id，或在成功对账/上下文切换时清除 fact；不要让异步剪贴板结果读取全局最新 fact。补“成功事实已过期后再失败”和“剪贴板结果晚于新 Notice”的测试。
- **严重程度**：**低**（只影响通知归属，不影响会话状态；但会误导）。

### 3. [低] 前缀/弹窗打断时，kitty 应用仍可能收不到 keyup

- **位置**：`crates/homeway-cli/src/tui/view.rs:910-945, 959-972`。
- **问题**：终端 press 被转发后，`forwarded_keys` 记录该键；但 release 分支仍要求 `!model.prefix && !modal_open() && focus == Terminal`。例如按住 `a`（已发给 kitty 应用）→ 按 Ctrl+B 置 prefix → 松开 `a`，release 被直接丢弃；打开 modal 同理。`forwarded_keys` 明明知道这是已发 press，却没有用来强制发送对应 release。r2 低7的“UI消费press不漏keyup”因此仍未完全闭合；现有测试只覆盖 UI 自己消费掉的 press/release。
- **建议**：release 时如果该 KeyCode 在 `forwarded_keys` 中，按已发送事实转发 keyup（至少 prefix 场景应转发）；补“终端 press → Ctrl+B → release”和“终端 press → 打开 modal → release”用例。
- **严重程度**：**低**。

### 4. [低] ENDED / attach 错误路径不重置前缀和输入归属

- **位置**：`crates/homeway-cli/src/tui/mod.rs:484-491`（attach frame 错误）、`mod.rs:540-548`（ENDED）；对照 `mod.rs:271-283` 的 `detach()`。
- **问题**：`detach()` 会调用 `model.reset_terminal_input()` 并清队列，但 attach 错误和 ENDED 只清 `attach`/`connected`/部分输入，不重置 prefix、`forwarded_keys`、`suppressed_releases`。如果 Ctrl+B 后会话结束或 attach 失败，prefix 可能保留，下一次按 `q` 会直接退出 TUI；输入归属也不完整。
- **建议**：这两条路径调用 `detach()`，至少显式调用 `model.reset_terminal_input()`；补“prefix 中途 ENDED / attach 错误”的测试。
- **严重程度**：**低**。

---

## r2 七项复核

| r2项 | 结论 |
|---|---|
| 高1 ATTACHED模式 | **闭合**：`transport`/`mod.rs` 消费 legacy 位；`service.rs:1319-1326, 2978-2988` 同锁捕获并在 REPLAY_DONE 前补 `SessionVt::input_mode_checkpoint`；`vt.rs:570-596` 不切屏/清屏/动光标/历史/size；raw-only 为空检查点。顺序安全，未发现 checkpoint 越过 REPLAY_DONE 的路径。 |
| 高2 Refresh隔离 | **基本闭合**：`backend.rs:206-244` 独立 refresh 线程/lane/control client；`480-565` 一条绝对 deadline 贯穿请求；`391-419` mutation epoch 过滤已排队旧 snapshot；mutation 线程不再 inline 刷新。残余见发现1。 |
| 中3 部分成功 | **a6dd 快照未闭合**：旧 `mod.rs` 的 `Sessions Err` 直接覆盖成功文案，KILL 成功仍会被列表错误抹掉。当前工作树已新增 `operation_fact` 修这个点，但该改动晚于最终测试/二进制；且引入发现2的 stale 归属风险。 |
| 中4 stale UDS | **闭合**：`local_term.rs:43-48, 63-105` 有 probe_alive、local→flat→serve 实际 connect 回退、spawn 失败 unlink；CLI/TUI 共用 `connect_local_terminal`。 |
| 中5 live ERROR | **闭合**：`mod.rs:556-568` 显示 `bad_input`/`bad_resize`；`bad_resize` 清 `sent_size` 和 pending 输入，下一轮 resize 重试。 |
| 中6 Create单飞 | **闭合**：`mod.rs:393-421, 726-735` create_pending 单飞、CreateFailed 清位、过期 Created 提示原主机且不导航、Created 后强制新 LIST epoch。未发现新的创建错序。 |
| 低7 prefix | **部分闭合**：非 Key/modal/选择清 prefix、detach reset 已有；但发现3/4仍有 keyup 和 ENDED 路径残留。 |
| 新 API | **基本闭合**：`dial` 缺省行为保持，`request_deadline` 覆盖写锁/写入/应答并清理 pending；但 connect 阶段见发现1。 |

**未发现新问题**：mode checkpoint 的屏态安全与旧 raw 兼容、local 回退、live `bad_resize` 重试、Create 单飞/旧 LIST 过滤、刷新与 mutation 的 epoch 顺序，方向上均成立；未发现明显逐行移植或新增字符串错误。

---

## 证据限制（这是门二的主要阻断）

1. **当前树与最终证据不一致**：我读到当前工作树时：
   - `crates/homeway-cli/src/tui/mod.rs` mtime = **17:19:43**；
   - 最终测试 `/tmp/homeway-tui-final-tests.log` = **17:13:40**；
   - `target/debug/homeway` 已在 **17:21:41** 重建，当前 SHA256 = `db9dd39bb779c15ba986cea11bbf7fa5015eaf11a5921e9e0daa990778a97e32`，**不是** prompt 中的 `a6dd...`。

   因此 `a6dd` 的本地/远端 smoke、31/0 最终测试、clippy 与平台 check 都不覆盖当前 `operation_fact` 改动。当前二进制没有对应的最终测试日志。若这是并行补丁，需要按当前树重新构建并复跑；否则应先把树冻结到 `a6dd` 对应状态再签字。

2. **文档仍不是最终闭合态**：`docs/reviews/2026-10-27-tui.md:95,101` 仍是“后续复评/整改进行中”；`:142-149` 把 17:13 的 31/0 写成最终证据，但早于 17:19 的源码；`:158` 也说明 r3 结论待真实 exit 更新。文档更新了大部分验证表，但没有覆盖当前树。

3. **全量门不是单轮全绿**：完整 `ci-local.sh quick` 的 16:49 日志对应**初版树**；终端 batch 是 `210/0 + 1 失败`，失败项 `attach_size_applies_to_pty` 触发 60s 硬期限，隔离复跑 1/0、0.87s。这如实说明了环境性挂死，但不能写成“最终全量一次全绿”。crosscheck 仍待回填。

4. **构建档别混淆**：`target/release/homeway` 是 16:44，早于最终 debug；16:57 的 release smoke 不能作为最终 `a6dd` 的证据。最终 local 8 PASS / remote 7 PASS 的日志确实存在，但对应 17:08/17:10，早于 17:19 的当前源码。

5. 我未独立运行 cargo/真实 PTY/生产；以上结论是静态审查 + 已有日志/哈希核对，不能替代当前树的最终复跑。
