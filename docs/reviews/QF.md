# Q-F 批记录：客户端核与桥（session / facade / wgcore Client 面）

> 批次：Q 批整改 `docs/REVIEW-ROADMAP.md` §Q-F（顺序 … → Q-E → **Q-F** → Q-G → Q-H → Q-J）。
> 本文 = 第 2 棒（实现）产出：实现清单 + 测试/判据证据 + **两轮外部评审（设计门 r12 + r13 / 代码门 r14 + 复审 r15）**
> 的逐条处置 + 不做项与残余登记 + **portfwd-B 交接块**。
> 设计规格 = `docs/reviews/QF-design.md`（v3，设计门已过；实现注记见其 §8）；基线 = `git HEAD 905928c`（Q-E 收口）。
> 范围 = 客户端核与桥的**加固**（诚实语义、状态机、预算/期限、锁纪律、spawn 可观测性）；新功能（portfwd 实装监听器）
> 与 tier 触点（`tun_attach/tun_stop` 异步壳）**不在本批**（见 §6）。

---

## 0. 门命令与证据（本树实测）

| 门 | 命令 | 结果 |
|---|---|---|
| 测试 | `cargo test --workspace` | **全绿**：homeway-core lib **588 passed / 0 failed / 4 ignored**（基线 554/1〔已知 flake〕→ 净增 34 条用例）+ 集成/向量轨（1+3+3+4+2+3+1）与 cli（4+14）全绿 |
| 静态 | `cargo clippy --workspace --all-targets -- -D warnings` | **clean**（exit 0） |
| 词表 | `zsh tools/check-vocab.sh` | **PASS**（Rust 声明 5 单元 / 26 值；ledger sha256 一致；缺席表 4 项在册——`bind_failed` 仍声明，未新增词值） |
| 已知 flake 甄别 | `wgcore::stackb` / `daemon::server_bad_frame` / `speedtest_server::serve_send_end_to_end` / `term::service::attach_size_applies_to_pty` | 本批全程 6 次全量跑：**仅基线那次**出现 `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect` 红（在册 flake，隔离复跑绿、与改动面无交集）；其余全绿 |

**评审轮次留档**（`~/.agents/skills/reviewer/SKILL.md` 固定姿势；前台跑、成败只认 exit code）：

- 设计门（第 1 棒）：`/tmp/dsh-review/r12.kyMDLA`（exit=0；34 条〔3 高〕→ 全认同整改为 v2）+
  `/tmp/dsh-review/r13.4ux4qT`（exit=0；4 条必改 + 4 条并入 → v3 过门）——摘要见 `QF-design.md` §6。
- 代码门：**`/tmp/dsh-review/r14.pE8g8w`**（`exit=0`；输出 156 行，**已用 Read 全文读完**）——
  结论 = **无高危**；1 中（F8e 额度归属）+ 5 低（`stop()` 读点 / svc 线程 panic 面 / 段④注释 / detach 测试缺口 / 登记错配）
  + 4 条小渍与 2 处顺手改记账；三门独立复跑全绿。
- 处置后复审：**`/tmp/dsh-review/r15.76jP4t`**（`exit=0`；结论见 §4.4）。

---

## 1. 实现清单（F1–F8 + N1–N7）

> 逐条给「改了什么（文件/函数）」。设计规格 = `QF-design.md` §1；实现期偏离集中记在 `QF-design.md` §8。

| # | 要点 | 主要落点（文件::函数） | 关键测试 |
|---|---|---|---|
| **F1**（P0） | **portfwd 诚实语义**：状态恒 `failed` + 明确 err（空 `code`）、`target` 走 `pf_target_text`、热替换 rc 改 `-1`（表照存）、数值面标注真值、组装收敛为纯函数 | `facade/portfwd.rs`（`PfStateKind` enum + `PfState::unavailable`/`failed`/`listening` + `unavailable_err_text` + `pf_states` 纯函数）、`facade/tun_exec.rs`（`request_port_forwards` → 恒 -1；`portfwd_states(rules)` 薄转发；`runner_of` 注释 pf 计数真值恒 0）、`homeway-cli/src/main.rs`（`cmd_portfwd` 文案收敛，N7） | `portfwd::unavailable_state_snapshot` / `pf_states_reports_unavailable_with_target_text`（四形态）/ `tun_exec::portfwd_states_matches_pure_source` / `runner_of_reports_honest_port_forwards` / `request_port_forwards_stores_table_and_reports_minus_one` |
| **F2**（P1） | **暖机硬失败不发布就绪 + 失败可见 + 可重建**：三分支（stopping 逐字 / Ready 发布 / 硬失败保留失败实例 + `fully_stopped` 置位 + 域态身份守卫）；运行槽门改「已收尾 ⇒ 可替换」；`status(domain)` 槽空 + 域 Failed 报 failed+原因 | `facade/service_exec.rs`（`session_thread_body` 三分支 + `publish_ready` + `slot_is_same` + `ServiceRun::{mark_fully_stopped,is_settled}` + `with_session_factory` 测试缝 + `status(&self, domain)`）、`session/mod.rs`（`synthetic_for_test` 会话缝 + `stop()` 的 failed 终态保留）、`facade/mod.rs`（`service_status` 调用点） | `publish_ready_truth_table` / `warm_hard_failure_keeps_failed_and_replaceable`（三分支③ + 可重建 + stop 无 2s 空等）/ `warm_stopping_race_clears_slot_and_goes_idle`（分支①回归）/ `settle_inline_never_leaves_stopping` / `status_reports_failed_when_slot_empty` |
| **F3**（P1） | **阶梯与闸等待纳入调用方预算**：`LadderDeps.deadline` + 每档/每动作前检查 + 探测按剩余夹取；`merge_until` 返回「是否等到」；`Deadline` 映射 **-1**、不计耗尽、等待方到点跳过重建；两域 `dial_with_recover`/`healing_dial` 传期限；**F3b** 无界 RPC 有界化 | `session/recover.rs`（`LadderRc::Deadline` + `deadline_hit`/`clamp_probe` + `merge_until`/`Round::wait_until` + `exhausted_delta` 单源）、`session/mod.rs`（`recover_until`/`recover_stale_until`/`healing_dial`）、`facade/tun_exec.rs`（`GenRun::recover_until` + `dial_with_recover`）、`wgcore/mod.rs`（`rearm_soft_bounded`/`refresh_reg_result_bounded`/`shutdown_bounded`/`close_bounded`） | `ladder_deadline_refuses_at_level_start` / `deadline_not_counted_as_exhausted` / `merge_until_three_forms` / `dial_with_recover_bounded_by_caller_budget`（桩被调用 + 预算内 TimedOut）/ `ladder_deadline_maps_to_minus_one` / `recover_deadline_not_counted_and_no_rebuild` / `recover_wait_timeout_skips_rebuild` |
| **F4**（P1） | **`dial_port` 锁出拨号**：`DialFn = Arc<dyn Fn…>`（单 Arc，无 `Arc<Mutex>` 退路）；`handle_conn` 克隆 Arc 出锁调用 | `facade/bridge_host.rs`（`DialFn`/`handle_conn`/`set_dial`）、`facade/service_exec.rs`（`BridgeDialFn` 别名 + `bridge_dial_closure`）、`facade/tun_exec.rs`（桥构造） | `bridge_host::dial_port_lock_not_held_during_dial`（A 阻塞时 B 已进入 + `set_dial` 不阻塞） |
| **F5** | `tun_attach`/`tun_stop`/`service_stop` 同步阻塞 | **零 core 改动**（移出本批，§6.2） | — |
| **F6**（P1） | **锁纪律单源 + Drop 零 panic + 收工五段共享 6s 预算**：`syncutil::lock_unpoison`（+ `read/write_unpoison`）；session 31 处 / recover 5 处 / `tun_exec` 3 处 / `wgcore::Client` 面全部收敛；段④ `try_lock` 快跳；段⑤ 新增 `Client::stop_within`（到点 detach ⇒ JoinHandle + wake fd 交 `hw-engine-reap`）；`punch_to`/`tunnel_punch_to` 停机不新发探测；删死函数 `dial_unix_direct` | `syncutil.rs`（新）、`session/mod.rs`（`stop()` 五段 + `join_bounded`）、`session/recover.rs`（gate 锁）、`facade/tun_exec.rs`、`facade/tun_shared.rs`（重导出）、`wgcore/mod.rs`（`stop_within` + Client 面锁）、`facade/service_exec.rs` | `syncutil::lock_unpoison_survives_poisoned_mutex` / `join_bounded_both_forms` / `session::drop_session_never_panics`（毒锁 + Drop）/ `stop_skips_cache_final_write_when_lock_busy`（段④快跳 + 记行）/ `wgcore::stop_within_detaches_and_reaper_closes_wake_fd`（detach + 收割线程关 fd）/ `session::stop_within_normal_path_and_reentrant` |
| **F7**（P2） | **spawn 失败全记行（三类）+ 桥状态诚实性**：`Builder::spawn` 10 个站点全部「失败 ⇒ 记行 + 降级动作」；`std::thread::spawn` 三处处置（svc 收尾线程改 Builder + `settle_inline`；服务域旁路探测改 Builder + 记行；隧道域三处保留 + 登记为 panic 归因）；服务域巡检线程 `catch_unwind`（记行 + 不改域态）；**新增**：`homeway-svc` 会话线程体 `catch_unwind`（代码门 ①-1）；桥 `unavailable`（spawn 点 + listen 结论点）⇒ `status()` 空路径 | `syncutil.rs`（`log_spawn_failed`）、`session/mod.rs`、`facade/tun_exec.rs`、`facade/bridge_host.rs`、`facade/service_exec.rs` | `syncutil::spawn_failed_line_shape` / `bridge_host::unavailable_bridge_reports_empty_path` / `session_thread_panic_is_caught_and_domain_failed` |
| **F8a**（P2） | 写退避分级（前 50 拍 2ms ⇒ 10ms ⇒ 100 拍后 20ms 封顶；10s 无进展界与空写短路保留） | `facade/tun_exec.rs`（`SessionWriteHalf::write` + `write_retry_backoff`） | `write_backoff_schedule` |
| **F8b**（P2） | `stats_loop` 死分支删除（`base_done`/`last_diag`/`elapsed` 三账面量）；`diag_fd_secs` 保留为配置面并登记「无消费」 | `facade/tun_exec.rs`（`stats_loop`） | 既有 stats 相关测试不变 |
| **F8c**（P2） | 巡回归因打**真因**（`probe` 保留 `ConnErr`，失败行 `巡检失败（连续 {n}）：{err}`——与隧道域同族行同串形态） | `session/mod.rs`（`patrol_loop`） | 判据变更记录第 1 行（§3） |
| **F8d**（P2） | `Secret` **拆宏**去 `Copy` + Drop 擦除（`wipe_bytes`：volatile 写 + fence，零新依赖）；`PeerId` 保持 `Copy`；`GenRun.secret`/`Shared.secret` 改 `Secret` | `token.rs`（`byte_array_newtype_common!` + `byte_array_newtype!` + `secret_newtype!` + `wipe_bytes`）、`wgcore/mod.rs`、`session/mod.rs`、`facade/tun_exec.rs` | `wipe_bytes_zeroes_all` / `secret_debug_is_redacted`（不变）/ 既有 token 向量全绿 |
| **F8e**（P2） | `lookup_host` 在飞上限 = **分档令牌池**（critical 4 / background 4；额度随 **worker** 生命周期，调用方超时不归还；获取耗时计进调用方预算） | `wtransport/domain_eps.rs`（`ResolveLane`/`ResolverLimiter`/`Permit`/`process_limiter`/`lookup_host(…, lane)` + `debug_reset_process_limiter`〔test-seams〕）、`daemon/hosts.rs`（reach = Critical 档，一行） | `resolver_limiter_lanes_and_budget` / `permit_is_held_by_worker_not_caller` / `resolver_acquire_wait_converges_within_budget` / `lookup_localhost`（不变） |
| **N1** | 无界 RPC 收敛（`rearm_soft`/`refresh_reg_result`/`shutdown`/`close`） | 见 F3b + `SessionWriteHalf::close_write`/`SharedConn::drop`（`EXIT_RPC_BUDGET`=2s） | 设计清单穷举核对（`rearm_soft`/`refresh_reg_result` 在必须退出的线程上已无无界调用点） |
| **N2** | 桥「假状态」 | 见 F7b（`unavailable` ⇒ 空路径） | `unavailable_bridge_reports_empty_path` |
| **N3** | `service_stop` 同步面（落点在 tier） | 只登记（§6.2） | — |
| **N4** | 隧道域 pf 计数/`pf=0/0` 语义标注 | 见 F1（注释 + §5.2 登记） | `runner_of_reports_honest_port_forwards` |
| **N5** | `tun_set_port_forwards` 门（probe_running + attached）本身正确 | **非缺陷**（复核结论；门保持不动） | 既有 `facade::port_forwards_gate` 不变 |
| **N6** | `std::thread::spawn` 失败 = panic 面 | 见 F7-2（三处处置 + 登记） | `settle_inline_never_leaves_stopping` / `session_thread_panic_is_caught_and_domain_failed` |
| **N7** | CLI `cmd_portfwd` 目标文案第三份拷贝 | 见 F1-5（复用 `pf_target_text`）+ **实际拨号端口 `0 ⇒ listen`**（登记） | 词表门 + CLI 测试不变 |

---

## 2. 测试证据

**总账**：`cargo test --workspace` = homeway-core lib **588 passed / 0 failed / 4 ignored**（基线 554 passed/1 failed；
本批新增/改写用例 **34** 条）；`cargo clippy --workspace --all-targets -- -D warnings` exit 0；`zsh tools/check-vocab.sh` PASS。

**本批新增用例（按面）**：

- `portfwd`：`unavailable_state_snapshot`、`pf_states_reports_unavailable_with_target_text`（四形态）、`state_codes`（改）。
- `tun_exec`：`portfwd_states_matches_pure_source`、`runner_of_reports_honest_port_forwards`、
  `request_port_forwards_stores_table_and_reports_minus_one`、`dial_with_recover_bounded_by_caller_budget`、
  `ladder_deadline_maps_to_minus_one`、`write_backoff_schedule`。
- `recover`：`ladder_deadline_refuses_at_level_start`、`ladder_deadline_midway_still_allows_recovery`、
  `deadline_not_counted_as_exhausted`、`merge_until_three_forms`（①等待方到点②NAPI 并发 rc=-1③None 同旧行为）。
- `session`：`recover_deadline_not_counted_and_no_rebuild`、`recover_wait_timeout_skips_rebuild`、
  `stop_skips_cache_final_write_when_lock_busy`（段④）、`stop_within_normal_path_and_reentrant`、`drop_session_never_panics`（毒锁 + Drop）。
- `service_exec`：`publish_ready_truth_table`、`warm_hard_failure_keeps_failed_and_replaceable`、
  `warm_stopping_race_clears_slot_and_goes_idle`、`settle_inline_never_leaves_stopping`、
  `status_reports_failed_when_slot_empty`、`session_thread_panic_is_caught_and_domain_failed`。
- `bridge_host`：`dial_port_lock_not_held_during_dial`、`unavailable_bridge_reports_empty_path`。
- `syncutil`：`lock_unpoison_survives_poisoned_mutex`、`join_bounded_both_forms`、`spawn_failed_line_shape`。
- `wgcore`：`stop_within_detaches_and_reaper_closes_wake_fd`（detach + 收割线程关 fd，F_GETFD 探测）。
- `token`：`wipe_bytes_zeroes_all`。
- `domain_eps`：`resolver_limiter_lanes_and_budget`、`permit_is_held_by_worker_not_caller`（走可注入主体
  `lookup_host_with` + 阻塞解析器：调用方 TimedOut 后 `inflight == 1`、worker 结束后归 0）、
  `resolver_acquire_wait_converges_within_budget`。

**墙钟断言纪律**：`dial_with_recover_bounded_by_caller_budget`（预算 600ms，断言 <3s，桩被调用才判绿——避免假绿）；
`stop_skips_cache_final_write_when_lock_busy`（持锁 1.2s，断言 stop <600ms——阻塞获取会等满 1.2s）；
`resolver_*`（等待按预算收敛，阈值 ≥2×）；`stop_within` detach 用 `F_GETFD` 事件断言而非墙钟。

**已知 flake 甄别**：`daemon::tests::server_bad_frame_gets_goodbye_and_disconnect` 在基线跑红（在册 flake；隔离复跑绿、
与改动面无交集）⇒ 判 flake 不算回归。**新增登记**（代码门观察，非本批代码问题）：同一工作树并发跑两个
`cargo test` 时 `daemon::carriers::forward::*` 固定端口（19990/20001）互撞 ⇒ `EADDRINUSE` 红——已写入
`REVIEW-ROADMAP.md`「已知 flake 登记」表。

---

## 3. 判据行登记（`docs/INTEROP-CRITERIA.md`，与代码同批 commit）

- **§5.1「判据变更记录」+3 行**（`git diff` 已核）：
  1. **服务会话巡检失败行**：`巡检失败（连续 %d）：探测超时` → `巡检失败（连续 %d）：<真因>`（超时实渲染 `连接超时`——与隧道域同族行同串形态）。
  2. **服务会话收工等待行**：`收工等待巡检线程超时（STOP_WAIT）——放行自退` → per-thread 五段形态（+ 段④跳过终写行）。
  3. **`portForwards[]` 状态文案 + rc 契约面**：`listening` → `failed`；err 明确文案；`code` 保持空（**有意偏离 spec MUST**）；
     `target` 四形态；rc `0` → `-1`。
- **§5.2「计数输入集 / 数值语义变化」+6 行**：`pfAccepted/pfFails` 语义订正；additive 新行（含桥 accept/conn/pump 起不来、
  各派生线程 spawn 失败、暖机硬失败、RECOVER 预算耗尽、域名解析并发上限、svc 收尾线程就地收尾、两处 panic 兜底）；
  `unhealthyReason` 取值集不变；`LadderRc::Deadline` 记账；`healing_dial_*` 调用方最长等待缩短；域名解析并发上限。
- **§5.3「已知口径注记」+6 条**：portfwd 诚实态（含 CLI 拨号端口语义）；桥状态诚实性（偏离 Go）；服务会话暖机硬失败
  （含 `Session::stop` 先读后写 + **F2-3 的 tier 后果**）；桥拨号期限（偏离 Go）；收工等待（五段 + 段④ I/O 残余 + 范围声明）；
  域名解析并发上限。
- **编号判据行（E/C/R 族）**：**行文零改动**（本批不触出口/拦截/终端判据行；`fixtures/` 无 `portForwards` 夹具 ⇒ 无夹具变更）。

---

## 4. 代码门（r14）意见与逐条处置

> 轮次目录 **`/tmp/dsh-review/r14.pE8g8w`**（`exit=0`；`output.md` 156 行，**已用 Read 全文读完**）。
> 评审独立做的事：三门复跑（`cargo test --workspace` / `clippy -D warnings` / `check-vocab.sh`）、全量读 `git diff`
> （19 改 + 2 新）、回读 `QF-design.md` 全 565 行 + Go 基线 `app_portfwd.go` + tier 只读消费面四处。

### 4.1 结论

**无高危**；1 条中危（F8e 额度归属）+ 5 条低危 + 4 条小渍 + 2 处「顺手改」记账建议。**全部处置**（下表）；
无「不认同」项（0 条），1 条**部分认同**（`pf_states` 直产 `PfStateIn`——按设计 F1-2 的「唯一组装点」要求豁免并登记）。

### 4.2 逐条处置表

| # | 严重度 | 意见 | 处置 | 证据 |
|---|---|---|---|---|
| **②-1** | **中** | F8e 令牌池没有给 worker 设界：`Permit` 是调用方局部量 ⇒ 调用方超时即归还额度，"卡死的 detach 解析线程"不受池约束（登记文字与设计残余都不成立；审计条目 14 根因未收口） | **认同并改码**：`Permit` **move 进 worker 闭包**（额度 = 在飞解析；调用方超时不归还；spawn 失败 ⇒ 闭包析构归还；`remaining==0` 早退 ⇒ 归还） | 新单测 `permit_is_held_by_worker_not_caller`；登记文字精确化为「**在飞解析**分档上限」（§5.2/§5.3 两处） |
| **②-2** | 低 | `stop()` 注释"不再有任何无界段"过强：`try_lock` 只保证不等锁，段④拿到锁后的 I/O 仍无期限 | **认同**：注释降级 + §5.3 补「段④落盘 I/O 无期限（残余，不宣称不再有无界段）」 | `session/mod.rs` 注释两处 + `INTEROP-CRITERIA.md` §5.3 F6 段 |
| **③-1** | 低 | `was_failed` 读点前移 ⇒ 丢掉"收工窗口内新产生的 Failed"（HEAD 是 join 后才读） | **认同并改码**：入口那次读只决定是否写 `Stopping`；**收工末尾再读一次**，仅当 `state != Failed` 才写 Idle | `session/mod.rs::stop` 末尾 `failed_now` |
| **①-1** | 低 | `homeway-svc` 会话线程体无 `catch_unwind`（panic ⇒ 域态永久 `Starting`、`service_start` 恒 0） | **认同并改码**（F7 同族补充）：线程体抽 `session_thread_body` + `catch_unwind(AssertUnwindSafe)`；落空 = 记行 + 桥停 + 清槽 + 域 Failed | 新单测 `session_thread_panic_is_caught_and_domain_failed`；additive 行 ⑧ |
| **⑤-1** | 中 | `INTEROP-CRITERIA.md` 两处指向 `docs/reviews/QF.md`（悬空）+ `REVIEW-ROADMAP.md` 未增 portfwd-B 挂账行 | **认同并补**：`QF.md`（本文，含「portfwd-B 交接」）+ ROADMAP Q-F 行挂账（Q-F-B，含「已知不达标」声明） | 本文 §7 + `REVIEW-ROADMAP.md` Q-F 行 |
| **⑤-2** | 低 | CLI `cmd_portfwd` 的 `--map L:<ip>:0` 实际拨号端口由 0 改 L（未登记；方向正确——与 Go 桌面 facade 同义） | **认同并登记**：§5.3 portfwd 注记补该行为变更（含 Go 两处不一致的取证：`clientcore::pfDial` 原样拨 `TargetPort`、桌面 facade `forward.go` 才 `0 ⇒ listen`；CLI 是 Rust 独有测试动词） | `INTEROP-CRITERIA.md` §5.3 |
| **⑤-3** | 低 | 登记正文两处行文错配：C17 措辞（failed 形态另有文字）；设计 §4.2 草稿 `预算 %v 已用尽` 与实现/登记不一致 | **认同**：§5.1 第 2 行改写（C17 idle 形态逐字不变 + failed 行是另一条行）；`QF-design.md` §8-4 记实现文案 | 两文档 diff |
| **⑤-4** | 低 | `INTEROP-CRITERIA.md` 表格被空行截断 + 文件无结尾换行 | **认同并改** | 该文件尾部 |
| **⑥-1** | 低 | 四处小渍：`unavailable: AtomicBool` 应为 `bool`；bridge spawn 失败分支两次取锁；`pf_states` 现造带 `AtomicI64` 的 `PfState`；`syncutil` 的"一律走本件"措辞过强 | **认同 3/4 并改**（bool 化 + 单临界区 + 措辞降级）；**第 3 项部分认同**：按设计 F1-2「`PfState::snapshot()` 为唯一组装点」保留现形（每 250ms ≤8 条映射的代价可忽略），登记豁免 | `bridge_host.rs`、`syncutil.rs` doc |
| **⑦-顺手改 1** | 记账 | facade 四文件 `.lup()` 收敛改了 ~40 处（设计写"facade 既有调用点零改动"） | **记账**：`QF-design.md` §8-9① + 本记录 | 本文 §5 |
| **⑦-顺手改 2** | 记账 | CLI 拨号端口（=⑤-2） | 见 ⑤-2 | — |
| **新增①** | 中 | F2-3（槽空 + 域 Failed ⇒ status 报 failed）缺契约面登记；tier 侧 `SERVICE+FAILED ⇒ BRIDGE_ACTION_HEAL_HOST`（会自动再拉起一次服务会话） | **认同并登记**：§5.3 F2 段补 F2-3 子项 + tier 后果（明示"有意为之"：状态面与 rc 门一致；如需改变表达另开批） | `INTEROP-CRITERIA.md` §5.3 |
| **新增②** | 低 | `stop_within` detach/收割分支零测试覆盖（测试 doc 承诺超实际） | **认同并补**：`wgcore::tests::stop_within_detaches_and_reaper_closes_wake_fd`（detach + `F_GETFD` 探测）；`session` 侧测试改名 + doc 对齐 | 两处测试 |
| **新增③** | 低 | 文件集口径（19 改 + 2 新，非"14+1"） | **记账**：`QF-design.md` §8-10 + 本文 §5 | — |
| **新增④** | 观察 | 同树并发跑两个 `cargo test` 会撞固定端口 | **认同并登记**：`REVIEW-ROADMAP.md`「已知 flake 登记」表 +1 行 | 该表 |

### 4.3 部分认同 / 不认同项

- **`pf_states` 直产 `PfStateIn`（⑥-1 第 3 项）**：**部分认同**——评审的动机（省一次 `AtomicI64` 构造）成立，但设计
  F1-2 明确要求「接线后 `PfState::snapshot()` 成为 `portForwards[]` 元素的**唯一组装点**」（设计门 C7b 的整改项）；
  改为直产会把组装点重新分裂成两处。**处置 = 保留现形 + 登记豁免**（代价：每 250ms 快照 ≤8 条映射各一枚
  `AtomicI64`，可忽略；Q-F-B 实装监听器时该结构本来就要接真计数）。
- **其余 0 条不认同**：评审的 14 条（含新增 3 条）全部成立并处置。

### 4.4 复审（r15）结论

> 轮次目录 **`/tmp/dsh-review/r15.76jP4t`**（`exit=0`；`output.md` 111 行，**已用 Read 全文读完**）。
> 复审对象 = 上述整改后的工作树 diff（8 项逐条复核 + 新增问题扫描）。评审独立复跑三门：`cargo test --workspace`
> **588 passed / 0 failed / 4 ignored**（三条新测与改名后的测试实跑通过）、`clippy -D warnings` 零告警、词表门 PASS；
> 并按 mtime 核对整改轮改动面恰好落在所述十个文件上（无额外可疑改动）。

- **8 项整改逐条复核 → 全部到位**：① F8e 额度随 worker 生命周期（`Permit` move 进闭包；`remaining==0` 早退、
  spawn 失败、worker 提前结束三条边界路径逐条查过——均无泄漏/无 double-close；单锁 + 条件变量无死锁）；
  ② `stop()` 两读语义自洽（状态写点全集 5 处，收工窗口内可能新写 Failed 的只有 `rebuild_session` 一处，末尾读覆盖它）；
  ③ `session_thread_body` 与设计伪码逐条对齐、抽取无夹带语义变化、兜底链自身不再 panic（无 `panic=abort`）；
  ④ 段④注释/登记两处口径一致；⑤ detach 分支真覆盖（`F_GETFD` EBADF）；⑥ 四处登记错配 + `QF-design.md` §8（10 条）
  + ROADMAP 挂账/flake 行全部落实；⑦ `bool` 化后 `status()` 过滤语义不变、`pf_states` 豁免**有设计依据（判成立）**；
  ⑧ `QF.md` 已在树里、两处指针不再悬空。
- **新增问题 6 条（1 中 / 5 低，无高危）**——处置见 §4.5。
- **评审结论原文要点**：「上一轮 8 条意见（含 r14 自增的 3 条）整改全部到位……三条门在当前工作树上独立复跑全绿……
  我判**本轮整改足以过代码门**」。提交前建议处理项 = 新增 1（本文预写复审结论，必须回填）+ 新增 3（panic 分支顺序）——
  **两项均已办**（见 §4.5）。

### 4.5 r15 新增问题与处置（逐条）

| # | 严重度 | 意见 | 处置 | 证据 |
|---|---|---|---|---|
| 新增 1 | **中** | 本文 §4.4 的复审结论是**预写**的（落笔时 r15 尚未产出）——评审记录先于评审存在，正是本批协议要防的记录失真 | **认同并已办**：本节即 r15 原文回填（轮次目录 / exit 码 / 三门复跑 / 逐条结论均为实测字段） | 本节 + §4.5 |
| 新增 2 | 低 | `permit_is_held_by_worker_not_caller` 钉不住整改点（自建池手工 acquire，全程不调 `lookup_host`——把 `Permit` 放回调用方该测照样绿） | **认同并改码**：`lookup_host` 拆出可注入主体 `lookup_host_with(limiter, host, budget, lane, resolve)`（池与解析器都可替换）；`ResolverLimiter` 内部改 `Arc<LimiterInner>`（`Permit` 变 `'static`，可 move 进 worker）；测试改走真 `lookup_host_with` + 阻塞解析器——**断言「调用方 TimedOut 后 `inflight == 1`、worker 结束后归 0」**（旧形态此处必 0 ⇒ 该测必红） | `domain_eps.rs`（`lookup_host_with`/`resolve_system`/`LimiterInner`）+ 该单测 |
| 新增 3 | 低 | panic 落空分支与 `Err(e)` 分支不同形（顺序倒置 ⇒ 「清槽」与「写域态」之间留抢占窗口） | **认同并改码**：顺序对齐为「写域态 → 停桥 → 清槽」（逐字同形） | `service_exec.rs` panic 分支 |
| 新增 4 | 低 | `stop()` 的两处「读—写」非原子（`set_state` 另取锁），Failed 落在读之后仍会被覆盖 | **认同并改码**：抽 `Shared::set_state_unless_failed`（同一临界区判定 + 写），入口（Stopping）与末尾（Idle）两处都换——窗口彻底闭合，且比两读更简单 | `session/mod.rs`（`set_state_unless_failed` + `stop()`） |
| 新增 5 | 低 | 新 detach 测试两点健壮性：裸 fd 号 `F_GETFD` 有 ABA 假活风险；断言隐含「引擎此刻仍在跑」 | **认同并改**：加前置断言（`handle` 未 finished ⇒ 必走 detach 分支）+ ABA 限制备案（轮询自 detach 起、窗口极小，真出现按 flake 记） | `wgcore/mod.rs` 该测 |
| 新增 6 | 低 | `remaining == 0` 的归因在「零预算进入」形态下不成立（daemon `saturating_sub` 可能给 0——并无等待） | **认同并改**：归因区分两态（零预算进入 ⇒「域名解析预算为零（调用方未给预算）」；等待耗尽 ⇒ 原文案） | `domain_eps.rs` |

---

## 5. 实现口径与记账（供后续批核对）

- **文件集**：19 改 + 2 新（`syncutil.rs`、`docs/reviews/QF.md`）；其中 `lib.rs`（模块注册）、
  `facade/{tun_shared,service_op,demand,events,files_op}.rs`（`.lup()` 三份私有 trait 收敛，~40 处）、
  `daemon/hosts.rs`（F8e lane 一行）、`homeway-cli/src/main.rs`（N7 文案 + 拨号端口）为本批的**外围必要面**。
- **测试缝口径**：`Session::synthetic_for_test`/`GenRun::synthetic_for_test`/`ServiceExec::with_session_factory`
  全部 `#[cfg(test)]`（crate 内可见，**生产 API 零改动**）；`ResolverLimiter` 的进程单例复位面走
  `feature = "test-seams"`（`debug_reset_process_limiter`——集成测试看不到 `cfg(test)`，设计门 D4 口径）。
- **未采纳的设计项**：F1-2 的 `state/code` enum 化**已做**（`PfStateKind` + `Option<PortfwdErr>`）；F2-3 可选小修**已做**；
  F8e 的「超限快失败」形态**未采**（设计门 3.3 否决，改用分档池）；`punch_to` 的 path_probe 5s 预算**未改**（设计 F6-5 同款）；
  `pf_states` 直产 `PfStateIn` **未采**（设计 F1-2「唯一组装点」要求；代码门 ⑥-1 判豁免成立）。
- **代码门整改轮追加**（r14 → r15）：F8e 额度归属改「在飞解析」+ 可注入主体 `lookup_host_with`；`stop()` 改
  `set_state_unless_failed`（单临界区）；panic 落空分支顺序对齐 `Err(e)`；detach 测试加前置断言；零预算归因区分。
  另：**实现期发现的一处设计与代码矛盾**（`Session::stop()` 的 failed 终态保留分支恒不可达）已按设计意图修正并双处登记
  （`QF-design.md` §8-2 + `INTEROP-CRITERIA.md` §5.3），非静默降级——见 §8-2。

---

## 6. 不做项、移出项与残余登记（防静默漏做）

### 6.1 设计 §7 逐条核对（14 条）

| # | 项 | 本批状态 |
|---|---|---|
| 1 | **实装端口转发监听器（B 方案）** | **未做**（挂账为独立批 **Q-F-B**，见 §7 交接块）；tier `port-forwarding` spec 的 SHALL 处于**已知不达标**（已登记） |
| 2 | `tun_attach`/`tun_stop`/`service_stop` 的 async 壳 | **未做**（移出 → tier 触点；`Index.d.ts` 三处同步符号 + `tailcat_napi.cpp` 的 `napi_create_async_work` 先例；本批只以 F2/F6 收窄**实际**耗时） |
| 3 | 状态快照分层缓存 | **未做**（Q-I 前段已裁决整条不做；只登记） |
| 4 | 服务域 `rebuild_session` 无期限 | **未做**（残余；挂 Q-G） |
| 5 | `tun_stop` 世代收尾 4s 串行上界 / 隧道域三处 `c.stop()` 不在五段预算内 | **未做**（残余；F6-4 已写范围声明；挂 Q-G） |
| 6 | `Client::stop_within` 到点 detach 的窗口 | **登记**（引擎线程可能存活到自行退出，由 `hw-engine-reap` 收口；`fully_stopped` 弱于 Go `isDone()` 的窄窗 + 缓解论证） |
| 7 | `Client::read` 的无界阻塞（桥泵数据面） | **未做**（数据面阻塞语义；登记） |
| 8 | `Secret` zeroize 残余（`Psk`、boringtun 内部副本、`CoreConfig` 移交引擎后的副本） | **登记**（引擎生命周期内必然存在；擦除需 boringtun 侧支持） |
| 9 | `diag_fd_secs` 无消费 | **登记**（字段保留——删字段会动 `TunConfigJson` serde 面） |
| 10 | session/facade 以外同族 `expect("…中毒")` + `EndpointCache::save` 的 2 处逻辑不可达 `expect` | **按域登记**（Q-G/Q-H 面优先）；本批做 session + recover + `tun_exec` + `wgcore::Client` 面；**等价内联 `into_inner` 副本 ~39 处仍在**（措辞已如实降级） |
| 11 | `stats_loop` 的 fd 快照基线行 | **不补**（OHOS 沙箱 fd 快照受限） |
| 12 | 无自愈巡检残余（派生线程 spawn 失败只记行） | **登记**（不引入 `unhealthy` 以免触发无谓整套重建） |
| 13 | CLI `cmd_portfwd` 目标文案第三份拷贝 | **已做**（复用 `pf_target_text`）+ 拨号端口语义登记 |
| 14 | F2-3 的可选 status 一致性小修 | **已做**（`status(&self, domain)`；契约面变化 + tier 后果已登记） |

### 6.2 移出项（落点不在本仓）

- **`tun_attach`/`tun_stop`/`service_stop` 的异步壳**（N3/F5）：核心结论 = 5s/3s 是与 Go 对齐的**合同值**，异步壳须在
  tier 的 napi 层新增导出（`clientCoreTunAttachAsync`/`clientCoreTunStopAsync`/`clientCoreServiceStopAsync`，照
  `clientCoreTunRecoverAsync` 先例）；core 侧零改动即可支持。**tier 建议**（含两条随 F1 永久失义的文案）见 §7。

### 6.3 本批残余（如实登记）

1. 段④（缓存终写）拿到锁后的落盘 I/O 无期限（`EndpointCache::save`：读盘/建目录/写/rename，无 fsync、raw 未变时早退）。
2. `stop_within` detach 后引擎线程可能存活到自行退出（持 UDP fd/缓冲）；收割线程起不来时 fd 泄漏一枚（保持打开，
   避免驱动 `poll` POLLHUP 忙转）。
3. `fully_stopped` 弱于 Go `isDone()`（替换窗口内旧引擎线程可能仍在）。
4. 域名解析：黑洞下 critical 档 4 枚卡死 worker 可被占满 ⇒ **有界地失败**（不是「不会失效」）；`resolve_domains`
   对每个域名条目各用整份预算（N×budget）不变。
5. 隧道域 `Finish::drop`/`request_stop`/`rebuild_session→old.stop()` 三条 `c.stop()` 仍无界（挂 Q-G）。
6. `homeway-svc` 线程 panic 兜底后域态 Failed（可再受理）；若 panic 发生在 `Session::start` 内部已建部分资源处，
   由 `Session` 自身的 `Drop` 收口（与本批 F6 的 Drop 零 panic 链同源）。
7. 同树并发跑两个 `cargo test` 会撞固定端口（已入 flake 表）。

---

## 7. portfwd-B 交接（主会话据此立批；本文为唯一交接真源）

> 背景：主会话裁定本批 portfwd 落 **A（诚实语义）**，**B（实装监听器）立为独立批「Q-F-B」紧接本批之后**。
> 本节 = 给 Q-F-B 的交接块（①②③ 三项按主会话要求逐条给出）。

### ① A 落地后 tier `openspec/specs/port-forwarding` 处于**已知不达标**状态

- 需求真源（只读）`tier:openspec/specs/port-forwarding/spec.md`：「端口映射的建立与访问」要求 App 在手机上
  监听 `127.0.0.1:<监听端口>`（仅回环）并把入站连接经隧道转发到目标——**SHALL 未满足**（本核无监听器）。
- 已登记（两处）：`docs/INTEROP-CRITERIA.md` §5.1 第 3 行（影响面列明「tier `port-forwarding` spec **已知不达标**」）
  + §5.3 portfwd 注记；`REVIEW-ROADMAP.md` Q-F 行（本批挂账）。
- 用户可见形态：每条映射恒显示「失败 · 手机核未提供端口转发监听（127.0.0.1:<listen> 未监听）——该映射在当前版本
  不可用，不影响隧道」；浏览器连不上 = **如实**（此前恒「监听中」是谎报）。

### ② 冻结词表下 A 的表达局限（**B 批必须解决的第一件事**）

- `portForwards[].code` 现为**空串**：spec「映射状态可见」的 MUST 要求失败映射带**稳定枚举 code**
  （`bind_failed`/`dial_failed`/`invalid_target`）——A 有意偏离该 MUST（已显式登记）。
  理由：现有唯一可产出的 `bind_failed` 在 tier 侧渲染为「**端口被占用**」（`PortForwardRules.ets:207-218`），
  对"未实装监听器"是**具体化假归因**；空码走 App 登记的空码兜底路径（原样展示 `err`、不误归因）。
- **B 批第一件事**：实装监听器后 `code` 才能取真值（`bind_failed` = 真 bind 失败；`dial_failed`/`invalid_target`
  按需接线），此时 §5.1 第 3 行与 §5.3 注记需**同批登记**（把"有意偏离"改回"达标"，并说明 `code` 取值集变化）。
- 词表门（`tools/check-vocab.sh`）不受影响（`bind_failed` 仍声明；`dial_failed`/`invalid_target` 在缺席表在册）。

### ③ 实现轮廓 + 可复用先例 + 安全面 + 增量估算（起点 = `QF-design.md` §2.4）

- **轮廓**（`QF-design.md` §2.4，7 条）：`GenRun` 增 `pf: Mutex<PfRuntime>`（`lns`/`states`/`accepted`/`fails`）；
  整表替换（锁内停旧监听器 + 清状态 → 逐条 `TcpListener::bind(("127.0.0.1", listen))`：成功 ⇒ `PfState::listening`
  + spawn accept 线程；失败 ⇒ `PfState::failed(err)` = `bind_failed`）；返回 0（**真有承载面**）；accept 线程
  （并发阀 → `conns += 1` → `session_connect(&run, target_port, 15s)` → `into_halves()` → 复用
  `bridge_host::pump`；目标语义走现成 `portfwd::dial_target()`）；世代收工与 `bridge.stop()` 同序停全部监听器；
  状态面 `portfwd_states` 读真 `PfState`；`pf_accepted/pf_fails` 与 `stats` 行接真计数（**行文不变**）。
- **可复用先例（已真跑）**：CLI `cmd_portfwd`（`crates/homeway-cli/src/main.rs`，本地监听 → `healing_dial_addr` →
  双向泵整条实现；PERF-AB 的 RRR 臂 `--map 42901:<lan_ip>:42807` 真跑过）；出口侧同路径可达性已由
  `server/intercept/mod.rs` + PERF-AB 验证（任意目标 IP:port）。
- **安全面**：仅 `127.0.0.1`（不暴露局域网）；`listen ≥1024` 且同表唯一（`validate_table` 已有）；**新增**规则条数上限
  （建议 8）、并发流阀（Go `maxTCPFlows=4096`）、拨号 15s 期限、世代收工全关。
- **增量估算**：≈150–250 行 + 守卫三件套 ≈+80 行 + 测试；**真机浏览器验证**（用户触点）是真代价。
- **本批已铺好的地基**（B 批直接受益）：`PfState`/`PfStateKind`/`snapshot()` 单源、`pf_target_text`、`dial_target`、
  `pf_states` 纯函数、`request_port_forwards` 的存储面（现返回 -1，B 批改成真受理后需**同批改登记**）、
  `runner_of` 的 `pf_accepted/pf_fails` 接线位（现注释为真值 0）。
- **tier 侧连带失义文案**（随 F1 永久失义，B 批或 tier 批需同步）：`PortForwardsPage.ets:484` 的「点『立即重连』兜底」
  提示；`TierVpnExtensionAbility.ets:1010` 的 rc 日志文案（rc 恒 -1 ⇒ 该文案在已连接时也会打出）。

---

## 8. 需上报项 / 提请主会话裁决

1. **F2-3 的 tier 后果（已采 + 已登记，提请确认）**：槽空 + 域 `Failed` 时 `service_status` 报
   `{"state":"failed","reason":…}`（此前 idle）⇒ tier `BridgeRules.ets` 对 `SERVICE+FAILED` 返回
   `BRIDGE_ACTION_HEAL_HOST`（**自动再拉起一次服务会话**，`bridgeWaitBudgetMs=0`）。本批判断 = 有意为之
   （状态面与 rc 门一致、再拉起受 App 既有预算约束）；如主会话希望改变表达（例如暖机 Err 路径留 Idle），
   另开小批即可（改动 ≈2 行 + 登记）。
2. **`Session::stop()` 的 failed 终态保留**：设计假定成立但 HEAD 分支恒不可达（`set_state(Stopping)` 覆盖）
   ⇒ 本批改为「入口先读 + 末尾再读」。**这是设计与代码现状的一处矛盾，已按设计意图修正并双处登记**
   （`QF-design.md` §8-2 + `INTEROP-CRITERIA.md` §5.3），不是静默降级。
3. **Q-F 行状态回填**：`REVIEW-ROADMAP.md` 的 Q-F 行已写「实现棒已交付 + Q-F-B 挂账」；**完成态与 AUDIT 勾选**
   按协议由主会话核对「评审记录在 + 测试证据在」后回填。
