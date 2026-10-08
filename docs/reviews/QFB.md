# Q-F-B 批记录：portfwd 实装监听器（真监听 + 状态面/计数/生命周期接线）

> 批次：`docs/REVIEW-ROADMAP.md`「Q-F-B」行（**紧接 Q-F 之后的独立批**：Q-F 只把 portfwd 落成
> 「如实报失败」的诚实语义〔方案 A〕，**真监听器（方案 B）**留到本批——用户已明确授权补齐）。
> 本文 = 第 2 棒（实现）产出：实现清单（F1–F9 逐条）+ 测试/判据证据 + **代码门 dsh `r27.nYwPfi`**
> 意见与逐条处置 + 设计文档实现注记（三条硬约定之③的落账）+ 残余登记 + 用户触点 E2E 待办。
> 设计真源 = `docs/reviews/QFB-design.md`（v3，两轮设计门 `r25.snY8bb`/`r26.vcqR9Q` 均 exit=0）。
> 交接真源 = `QF.md` §7；需求真源（只读）= `tier:openspec/specs/port-forwarding/spec.md`。

---

## 1. 实现清单（F1–F9 逐条）

| # | 条目 | 落点 | 一行说明 |
|---|---|---|---|
| **F1** | 真监听器运行时（`PfRuntime` 全注入 + 两阶段原子替换） | `facade/portfwd.rs`（新增 `PfRuntime`/`PfContext`/`PfListener`/`PfInner`/`StagedLns`/`FlowGuard`/`ListenerExit`） | 运行时**不依赖 `GenRun`**（拨号/停止位/上限/ack 等待全注入 ⇒ 纯回环可测）；install 两阶段：短锁只 take 旧 `lns`（`states` 原地保留至**单次换入**）+ 旧监听器**退出 ack**（共享 400ms；`Disconnected` = 线程已退 = fd 已关）+ 未 ack 端口 3×50ms 重试 bind + 双记行；accept 线程 `poll(2)`≤50ms、**fd 单属线程**、退出守卫 `ListenerExit` 保证「先关 fd 再投 ack」（panic 展开亦置迟到失败）；**阀与准入回 accept 线程**（Go 形）+ RAII `FlowGuard`（conns/flows 回退 + accepted 计数） |
| **F2** | 裸拨 + 目标映射 + 泵接线 | `facade/tun_exec.rs`（`session_connect_target`/`pf_dial_via_run`/`conn_err_to_io`）、`facade/portfwd.rs`（`dial_target` 扩面/`PfDialTarget::resolve`）、`facade/bridge_host.rs`（`pump` 参数化 + `WriteHalf for TcpStream`）、`sysfd.rs`（`rst_close_tcp` 上移） | pf 用**裸拨** `connect_deadline`（**不复用** `healing_dial`——常态拒绝不触发 R2 阶梯；桥路径保持 `healing_dial` 原样）；环回/`0.0.0.0` 目标映射为 `ExitPort`（⇒ 出口隧道 IP:port）；`pump(r,w,logf,label,dir,eof_log)`：桥传 `("桥泵",true)`（行文逐字不变）、pf 传 `("port-forward",false)`（EOF 零日志）；`rst_close_tcp` 单源上移 `sysfd`（daemon 侧 `pub(super) use` 保住三处调用点零改动） |
| **F3** | 状态面接线 + 失义残基清理 | `facade/tun_exec.rs`（`runner_of`/`stats_line`）、`facade/portfwd.rs`（`snapshot_states`/`PfCounters`） | `pf_accepted/pf_fails` 取真计数；`port_forwards` = `pf.snapshot_states()`（`snapshot()` 仍是唯一组装点）；抽 `stats_line(rd,wr,a,f)` 纯函数（行文逐字不变）；**删残基**：`PfStateKind::Unavailable`/`PfState::unavailable`/`unavailable_err_text`/`pf_states(rules)`/`tun_exec::portfwd_states(rules)` + 三个既有单测改写（`grep pf_states/unavailable` = 0 命中） |
| **F4** | 生命周期挂点 | `facade/tun_exec.rs`（`Arc::new_cyclic` 建 `pf` + attach 装表 + stale 复查 + 收工顺序 + `Finish::drop` 兜底 + rc） | `GenRun.pf: PfRuntime`（拨号闭包持 `Weak<GenRun>`；`stop` 与 pf 的 `gen_stop` **同一枚 Arc**）；attach：`stage=Attached` 后、桥构造前 `install`（Go 同序：`tunmode.go:828` 在 `:832` 之前）+ stale 复查（gen/stop）⇒ 撤回；收工：`pf.stop_all()` 在 `bridge.stop()` **之前**；`Finish::drop` 头部幂等兜底；热替换 rc = `0`（活世代真装表）/`-1`（无世代 / 换代 / 收口）/`-2`（JSON/校验，含条数上限） |
| **F5** | 安全守卫 | `facade/portfwd.rs` | `MAX_PF_RULES=8`、`MAX_PF_FLOWS=256`（偏离 Go 4096，口径 = 手机内存预算）、`validate_table` 增 `TableErr::TooMany`；拨号预算 = `cfg.dial_ms`（缺省 15s）；线程显式栈 **256 KiB**（见 §4 实现注记）；阀**拒绝型**（Go 同形，不复用桥的「挤最老」） |
| **F6** | bind 语义对齐 Go | `facade/portfwd.rs::pf_bind` | `sysfd::socket_cloexec` + `SO_REUSEADDR` 显式 + `SO_LINGER` 面（RST 走 `sysfd`）+ backlog 128 + 非阻塞 + **单次尝试**；地址**硬编码 `127.0.0.1`**（无配置面、无 `0.0.0.0` 退路）+ accepted socket 显式转阻塞（BSD 继承 `O_NONBLOCK`） |
| **F7** | `code` 词表收口 | `facade/portfwd.rs` | `PfState::failed` 的 `code=Some(BindFailed)` = **真值**；成功态空码；`dial_failed`/`invalid_target` 不产出（`ALLOWED_ABSENT` 保持）⇒ 词表门/夹具/NAPI 符号**零改动**；Q-F 的「有意偏离」→ **达标**（限定语见登记） |
| **F8** | 旁路防御面 | `facade/portfwd.rs::defensive_err` | `listen` 非法 / 超条数 / `target_ip` 非法 ⇒ **不 bind** + `failed_with(err, None)`（空码 + 精确 err）+ 行；逐条、不影响其余；`target_port=0` 走折叠语义（登记） |
| **F9** | 测试缝与环境一致性 | `facade/portfwd.rs`（`inject_wait_ack`/`inject_swap_seam`/`lns_len`/`conn_thread_spawns`）、`facade/tun_exec.rs::synthetic_for_test` | 小阀值注入、ack 等待注入（强制走超时路径）、**确定性换入缝**（直喂「读侧只见全旧/全新」）、空闲端口 helper、`tun_shared.gen` 与 `run.gen` 对齐（且 `pf.stop` 与 `run.stop` 同源）；「spawn 失败」按 Q-F 口径用纯函数缝 + 如实标注 |

**规模**：`git diff --stat` = 6 文件 / +2003 −280；新增单测 22 条（portfwd 21 + tun_exec/门测试改写）。

---

## 2. 测试 / 判据证据

### 2.1 门（本棒实跑）

| 门 | 命令 | 结果 |
|---|---|---|
| 单元测试 | `cargo test --workspace --no-fail-fast` | **全绿**：lib **659 passed / 0 failed / 4 ignored**（173.3s）+ 其余 11 个测试目标全绿（4+39+5+1+3+3+4+2+3+1+0），**exit 0**（最终整改后一轮；见 §2.2 计数与 flake 甄别） |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | **exit 0，零告警** |
| 词表门 | `zsh tools/check-vocab.sh` | **PASS**（Rust 声明 5 单元 / 26 值；缺席表 4 项在册——**零改动**） |
| 交叉 check | `cargo check -p homeway-core --target aarch64-unknown-linux-ohos` | **通过**（唯一告警 = 既有 `go_fmt.rs` 的 `libc::time_t` deprecated，非本批） |

### 2.2 计数与 flake 甄别

- 首次全量：`656 passed / 0 failed / 4 ignored`（lib）+ 其余目标全绿。
- 中间一轮：`658 passed / 1 failed / 4 ignored`——失败 = **`term::service::tests::attach_size_applies_to_pty`**
  （60s 硬期限超时）。按三条口径判**在册 flake**：① 隔离复跑 **3/3 绿（0.87–0.88s）**；
  ② 与改动面**零交集**（`term/service.rs` 不在本批 diff 内）；③ **已在册**
  （`REVIEW-ROADMAP.md`「已知 flake」表，2026-10-08 Q-J 批复跑观察：隔离恒绿、并发/高负载下超时红——
  本次即全量并发跑 + `loadavg ≈1.5–2`）。
- **最终一轮（整改后）= 全绿 exit 0**（`659 passed / 0 failed`，见上表）。
- 另一条在册 flake `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect` 在本棒也复现过：
  隔离 3 跑 = 1 绿 2 红（同二进制红绿翻转）+ **基线 `git stash` 复跑 4 次 = 2 红 2 绿**（基线可复现）
  ⇒ 判 flake（三证齐）。
- 未观察到 portfwd 面任何 flake（本批新增 22 条测试用**探测空闲端口** helper，不钉固定端口）。

### 2.3 真 socket 用例（本批核心判据）

| 用例 | 断言（真 socket） |
|---|---|
| `install_reports_per_entry_states` | 成功条 `listening` 且 `127.0.0.1:<port>` **真可连**；占用条 `failed` + `code=bind_failed` + errno 原文 |
| `inbound_roundtrip_through_pump` | 客户端 ↔（真 TCP 监听口）↔ 泵 ↔（UnixStream 对作「目标」）**双向真往返** + 半关闭双向 EOF 传播 + `conns/flows` 归零 + pf 侧 EOF 零日志 |
| `hot_replace_frees_reused_port` / `hot_replace_keeps_established_conns` | 同端口热替换后**真可连**；替换**不打断**已建立连接（双向继续收发） |
| `dial_failure_rst_and_counters` | 桩拨号失败 ⇒ 对端 `read` = **`ConnectionReset`**（非 EOF）+ `accepted=1/fails=1` + `conns/flows` 归零 |
| `flow_valve_rejects_over_limit` | 第 3 连接被拒（对端读到 EOF/错）+ `flow_rejected=1` + **`conn_thread_spawns=2`**（阀在 spawn 之前的直接证据）+ 放行后 `flows` 归零 |
| `stop_all_clears_states_and_frees_ports` / `generation_stop_flag_releases_ports_and_teardown_clears_states` | 收工后 states 空、端口**真可重绑**、幂等；世代停止位（`gen_stop`）让 accept 线程自退 ⇒ 端口立即释放 |
| `install_ack_timeout_retries_then_marks` | 注入「永不 ack」+ 真占用端口 ⇒ 3×50ms 重试 + **双记行**（Go 行文 + 「未在预算内释放」标注行） |
| `bind_uses_reuseaddr_and_single_attempt` | `getsockopt(SO_REUSEADDR)` 已设（darwin 回读 = 位值 4 / linux = 1）+ 占用期二次 bind 真 `EADDRINUSE` + 释放后可重绑 |
| `bypass_config_entries_are_failed_not_bound` | 13 条破损/超限表：破损条 `failed` + **空码** + 精确 err、**只有 4 条真 bind**（`lns_len==4`） |
| `accept_fatal_marks_entry_failed` / `accept_transient_backs_off_and_continues` / `listener_exit_marks_late_fail_on_panic` | 致命 ⇒ 状态转 `failed`（空码）+ 行；瞬态 ⇒ 退避不退出；accept 线程 panic ⇒ 迟到失败置位 |
| `install_atomic_single_swap` | 换入缝阻塞期读侧 = **全旧**（非空、非半表），换入后 = 全新 |
| `poisoned_lock_still_stops_all` | 毒锁后 `stop_all` 不 panic、端口真释放 |
| `pump_bridge_label_and_eof_line` / `pump_pf_label_eof_silent_errors_logged` | 桥侧 EOF 行文逐字不变；pf 侧 EOF 零日志、读错误**无条件**记行（零字节也留痕） |

### 2.4 未覆盖（如实）

- `gen_loop` 的 attach 装表 → 桥构造 与 收工段顺序（`pf.stop_all()` 在 `bridge.stop()` 前）**没有端到端用例**
  ——真世代 attach 需要真出口（本机单测不可达）；以「两处调用点 + 顺序注释 + `stop_all` 单测 + 世代停止位用例」覆盖，
  顺序本身留给真机 E2E（§6）与代码门复核。
- 线程 spawn 失败（accept/conn/泵）：不可注入 ⇒ 按 Q-F 口径用**纯函数缝**（`defensive_state_constructors`）断言状态构造，**如实标注**。

---

## 3. 判据 / 登记（与代码同批 commit）

落 `docs/INTEROP-CRITERIA.md`（本批 commit 同批）：

1. **「判据变更记录」+1 行**（2026-10-08 Q-F-B 批落地；**接续 Q-F 同族行**）：`portForwards[]` 状态文案
   与 `ClientCoreTunSetPortForwards` 返回码的**从 → 到**六要素（state / err / code / conns /
   未装表期与收工后空表 / rc）。
2. **「计数输入集 / 数值语义变化」+4 行**：`stats.pfAccepted`/`stats.pfFails`（恒 0 → 真计数）、
   `portForwards[].conns`（恒 0 → 真连接数）、`stats:` 行 pf 两位（恒 `0/0` → 真值，**行文不变**）、
   **新增观测行（additive）10 类**。
3. **「已知口径注记」**：Q-F portfwd 注记追加**「已收口」指针**（保留历史原文）+
   **新增注记 A**（真监听器 + 偏离 Go 四处 + 热替换/收工语义）+
   **新增注记 B**（非常态面 + 残余 **10 条**）。
4. 收口面（**主会话**执行，见 §7）：`REVIEW-ROADMAP.md` Q-F-B 行状态、Q-F 行「已由 Q-F-B 收口」、
   `ROADMAP.md:88` 注销、`AUDIT-2026-10-07.md` Q-F 条目尾注收口指针。
5. **零改动面（本棒核实）**：编号判据行（E/C/R 族）零改动；`fixtures/` 零改动；词表门四处
   （声明集/缺席表/manifest/tier 码表）零改动；NAPI 符号面零改动（仅返回值值域变化）。
6. **「已知不达标」注销记录**（Q-F 挂账 → 本批达标）：
   - `tier:openspec/specs/port-forwarding`「**端口映射的建立与访问**」SHALL：**已知不达标 → 达标**
     （真监听 + 经隧道转发 + 与分流模式无关）。
   - 同 spec「**映射状态可见**」失败码 MUST：**有意偏离 → 达标**（`bind_failed` 取真值；
     限定语 = 非常态空码面走 App 空码兜底路径）。
   - `INTEROP-CRITERIA.md` §5.1 第 3 行 / §5.3 portfwd 注记的「已知不达标」表述：**由本批的收口指针取代**。
   - tier 两处失义文案（`PortForwardsPage.ets` 的 dirty 兜底提示、`TierVpnExtensionAbility.ets` 的 rc 日志）：
     **随本批自愈**，不产生 tier 需求（设计 §0.4 订正①）。

---

## 4. 设计文档实现注记（三条硬约定之③：设计与代码矛盾/不可实现不得静默降级）

设计门放行条件三条硬约定：① 不带 v1/v2 旧写法进代码；② 判据变更同批 commit；③ 发现设计
与代码矛盾不得静默降级，要上报。本棒触发的记录如下（**均已落账**，未静默）：

| # | 设计 v3 原文 | 实现实况 | 处置 |
|---|---|---|---|
| 1 | §1-F1-3「万一仍 panic（如分配失败）…由世代 `Finish::drop → stop_all` 清 states 收口」 | **该句对「阶段 3 已 spawn 但未换入 `inner`」的监听器不成立**——它们不在 `inner.lns` 里，`stop_all` 看不到（且 `Finish::drop` 的头部位调用早于 `finish_generation → signal_stop`）。真收口机制 = **暂存守卫 `StagedLns`（RAII，drop 时置停止位）** + 世代停止位（`gen_stop` 让 accept 线程 ≤1 拍自退） | **实现补 `StagedLns`**（孤儿窗口收敛为 0）**+ 登记订正**：`INTEROP-CRITERIA.md` 注记 B ⑩ 明写该句的订正指针（代码门 r27 P3 同源） |
| 2 | §1-F5-5「泵线程与 conn 线程均 `stack_size(128 KiB)`」 | 实现取 **256 KiB**（与本仓全部会话面线程同档；Rust 栈溢出 = 进程 abort，「省虚拟内存」收益可忽略） | **登记**：`INTEROP-CRITERIA.md` 注记 A 内写明「设计 v3 写 128 KiB，实现取 256 KiB」+ 理由（代码门 r27 P6） |
| 3 | §1-F1-2 结构草图含 `PfInner.rules: Vec<PortForwardRule>` | 实现**不保留** `rules` 字段——`states` 已是状态面唯一真源，保留一份原始表 = 双份状态（设计同节自称「单源」） | 记于本表（无判据影响；`install` 逐条产 `PfState` 即用即弃） |
| 4 | §5.1「修前红证据：状态面真值（`listening`/`bind_failed`）、rc（`-1 → 0`）、Fatal 迟到失败」 | 本批为**新实装**（无法「修前跑真监听器」）⇒ 以「HEAD 语义 → 新语义」的对照断言替代（既有单测改写为真值断言 + rc 正例/负例 + Fatal 注入） | 记于本表 |
| 5 | §1-F1-3 步骤 2b「对这些端口做有界重试 bind（3 次 × 50ms）」 | 采纳；**附注**：旧监听器线程若正处 accept 的 Backoff 睡眠（最长 1s）则该窗口可能不足 ⇒ 仍失败时**双记行**（Go 行文 + 「未在预算内释放」标注行），不会是假「端口被占用」 | 代码门 r27 §3 表已认同；登记见注记 B ④ |
| 6 | §6.2 ⑨ 观测行枚举 | 实现另有两条 additive 行未在枚举内（**转发流拆半失败 / 本地 fd 复制失败**） | 已按代码门 r27 P1 **补进登记枚举**（代码不动——多一条可观测行是正向） |

---

## 5. 代码门（dsh 外部评审）

### 5.1 轮次档案

| 轮 | 目录 | exit | 产物 | 性质 |
|---|---|---|---|---|
| 代码门 | **`/tmp/dsh-review/r27.nYwPfi`** | **0**（前台捕获：`dsh … ; echo "exit=$?"`） | `output.md` **113 行（已 Read 全文）**；`stderr.log` 为推理流（不参与结论） | 未提交改动全量评审（实现 F1–F9 + 登记） |

- 评审**未修改仓库任何文件**（`git status` 复核：仅本批 6 个已改文件 + 未跟踪的 `QFB-design.md`）。
- 评审独立做的事：复跑 `cargo test -p homeway-core --lib`（656 passed / 1 failed，自行判为在册 flake 并给三证）、
  `clippy --workspace --all-targets -D warnings`（exit 0）、OHOS 交叉 check、`check-vocab.sh`（PASS）；
  `grep` 复核残基清理（`pf_states`/`unavailable`/`pf_rules` 全 0 命中）；`git show HEAD:` 复核登记的
  「from」侧行文逐字；逐条比对 Go oracle 五条行文、阀位置、`SetLinger`、rc 前置门；并**核探**了一条
  自认的「看似高危」（macOS accepted socket 无 `SO_NOSIGPIPE` → SIGPIPE）后判定不成立（宿主 ABI = linux，
  std 走 `MSG_NOSIGNAL`）。

### 5.2 门结论原文（摘要）

> 「**未发现高危**（无数据竞争、无端口/fd/线程泄漏、无谎报、无安全面缺口）。实现与 `QFB-design.md` v3 的
> F1–F9 / D1–D15 逐条对得上……**判「可通过（附整改建议）」**：1 条登记遗漏（政策面）+ 10 条低危
> （多为防御纵深/测试证据强度/地道 Rust），**没有一条需要重做设计**。」

### 5.3 逐条处置表

| 编号 | 严重度 | 意见（摘要） | 处置 | 落到 |
|---|---|---|---|---|
| **P1** | 中（政策面） | 两条新增观测行（转发流拆半失败 / 本地 fd 复制失败）未进登记枚举 | **认同**：登记枚举补写 + 计数订正（代码不动） | `INTEROP-CRITERIA.md` §计数输入集 ⑨ |
| **P2** | 低 | accept 线程 panic 路径不置 `late_fail`（`listening` 谎报最后一条缝） | **认同并改码**：`ListenerExit` 上移模块级 + 持 `Arc<PfState>`，`thread::panicking()` 时置迟到失败；补单测 `listener_exit_marks_late_fail_on_panic` | `portfwd.rs`（守卫 + 测试） |
| **P3** | 低 | `install` 中途 panic ⇒ 未换入监听器成孤儿；设计自证句与实现不符 | **认同并改码**：新增 `StagedLns` 暂存守卫（drop 时对未提交条目置停止位）；**并登记设计句订正**（本文 §4-1 + 注记 B ⑩） | `portfwd.rs` + 登记 |
| **P4** | 低 | 阀「未 spawn 线程」证据弱于设计（代理证据） | **认同**：加 `#[cfg(test)] conn_spawns` 计数缝 + 断言 `== 2` | `portfwd.rs`（缝 + 测试） |
| **P5** | 低 | 设计 §5.2 #15（世代收工）无用例 | **认同**：补 `generation_stop_flag_releases_ports_and_teardown_clears_states`（世代停止位 ⇒ 端口释放；`stop_all` ⇒ 清表）。**残余**：`gen_loop` 的调用顺序无端到端用例（需真出口）——如实记 §2.4 | `tun_exec.rs`（测试） |
| **P6** | 低 | 128 KiB 栈是全仓最小值、缺实测依据（Rust 栈溢出 = abort） | **认同并改码**：`PF_THREAD_STACK` → **256 KiB**（与会话面同档）+ 登记实现注记（设计写 128 KiB） | `portfwd.rs` + 注记 A |
| **P7** | 低 | `late_fail: Mutex<Option<String>>` 可用 `OnceLock` | **认同并改码**：`LateFail = OnceLock<String>`（`set` 幂等、读无锁） | `portfwd.rs` |
| **P8** | 低 | `install` 145 行 + `io::Error::other("bind 失败")` 字符串兜底 + `BTreeSet` 全限定 | **认同并改码**：抽 `install_one(&self, r, idx, &unfreed) -> (Arc<PfState>, Option<PfListener>)`；bind 结果改 `io::Result<TcpListener>`（**消除字符串错误**）；`std::collections` 全限定清理 | `portfwd.rs` |
| **P9** | 低 | `swap_seam` 生产结构体常驻 | **认同并改码**：字段与注入方法同门 `#[cfg(test)]` | `portfwd.rs` |
| **P10** | 低 | `synthetic_for_test` 的 `pf.stop` 与 `run.stop` 非同一 Arc ⇒ `gen_stop` 通路零覆盖 | **认同并改码**：同一枚 `Arc` + 新测试覆盖停止位通路（同 P5） | `tun_exec.rs` |
| **P11** | 低 | pf 泵零字节读错误不记行（Go netpipe 口径差） | **认同并改码**：pf 侧读错误**无条件记行**（桥侧旧门槛逐条不变）+ 双向单测 | `bridge_host.rs` + 登记 ⑨ |
| §3 表 | — | 「§5.2 #3b 3×50ms 重试 vs accept Backoff 最长 1s 睡眠」 | **认同**：已在注记 B ④ 覆盖（双记行保证不伪装成「端口被占用」）；本文 §4-5 附注 | 登记 |

**整改后复跑**：`cargo clippy --workspace --all-targets -D warnings` exit 0；`cargo test -p homeway-core --lib facade::`
**101 passed / 0 failed**；全量见 §2.2。**无高危必改项、无豁免项**。

---

## 6. 不做与残余登记（防「静默漏做」）

| # | 项 | 处置与理由 |
|---|---|---|
| 1 | **真机浏览器 E2E 四场景**（用户触点，见 §6.1） | **用户触点**：本棒不可执行，交包后由用户执行 |
| 2 | tier 两处文案 | **不改 tier**：本批落地即自愈 |
| 3 | CLI `cmd_portfwd` 切核心实现 | **不做**（D8）：CLI 走 `Session` 面、是测试动词；残留第三份泵拷贝**另行登记**（本批只做 RST 单源） |
| 4 | 已建立连接的带宽/时长占满 | **登记**（Go 同形）：无空闲回收、无速率整形 |
| 5 | 跨世代换代的同端口瞬时 `EADDRINUSE` | **登记**（Go 同形窄窗） |
| 6 | 收工 join/ack 到点 detach | **登记**（+ additive 行）；无泄漏路径（无 Arc 环） |
| 7 | `TunExecutor` 默认 `request_port_forwards` 仍 `-1` | **保持**（无承载 = 真话；真承载 = `TunnelExec` ⇒ `0`） |
| 8 | 破损配置的空码路径（F8） | **登记**（注记 B ①）；可达性 = 手改/损坏的持久化记录 |
| 9 | 引擎侧连接表无独立上限 | **登记**（注记 B ⑧） |
| 10 | 引擎缓冲按连接固定 2×1 MiB（不可缩放） | **登记**；阀值 256 已按此定 |
| 11 | `stats_loop` 的 tick 下限 5s | 不变量；`stats_line` 纯函数解决可测性 |
| 12 | accept 致命错误「转 `failed`」偏离 Go | **登记**（注记 A②）；Go 的「留 `listening`、不释放 fd」在 Rust 结构下不可照抄 |
| 13 | 「macOS accepted socket 无 `SO_NOSIGPIPE`」 | **代码门自核后判不成立**（宿主 ABI = linux，std 走 `MSG_NOSIGNAL`）——记录以免重复排查 |
| 14 | `install` 的按需语义（旧表在途连接不计入新表 `conns`） | **登记**（计数输入集行）；Go 同形（连接与快照同属一次装表） |

### 6.1 真机四场景 E2E 待办（用户触点）

| 场景（tier spec） | 判据 | 形态 |
|---|---|---|
| ① 浏览器访问出口主机自己的 8080 | `http://127.0.0.1:8080` 正常加载 | 真机 + 真出口 |
| ② 目标是出口可达的其它 IP | `5000 → 192.168.3.5:5000` 浏览器可访问 | 真机 |
| ③ 与分流模式无关 | IP 分流开启且目标网段不在隧道网段 ⇒ 仍可转发 | 真机 |
| ④ 端口被占用单条失败、其余正常 | 页面「失败 · 端口被占用」（`bind_failed` 分派），其余「监听中」，隧道正常 | 真机 |
| 状态可见（不新增轮询路径） | 既有状态通道（`pfSet` 后 App 主动重采样回推 + 5s 事件泵） | 真机 |

---

## 7. 收口面（**主会话**执行，本棒不越界）

- `docs/REVIEW-ROADMAP.md`：Q-F-B 行状态回填（设计门/代码门/记录三列）+ Q-F 行补「**已由 Q-F-B 收口（达标）**」。
- `docs/ROADMAP.md`（「Q 批之后的未开批」第 3 条现列 Q-F-B）→ 注销/记完成。
- `docs/reviews/AUDIT-2026-10-07.md` Q-F 条目尾注 → 补收口指针（不改原文）。
- `QF.md` §7 / `QF-design.md` §2.4 = 历史交接，**不改写**；收口记录 = 本文。

---

## 8. 提交

| commit | 内容 |
|---|---|
| `129f1c4` | 实现 F1–F9 + 代码门 r27 整改（6 文件：`facade/portfwd.rs`/`tun_exec.rs`/`bridge_host.rs`/`mod.rs`、`sysfd.rs`、`daemon/carriers/mod.rs`） |
| `08a0f62` | 判据登记（`INTEROP-CRITERIA.md`：判据变更记录 +1 / 计数输入集 +4 / 注记 A·B + Q-F 收口指针） |
| `11800a7` | 批记录（本文）+ 设计文档 v3 入库 |
| `（本行 commit）` | 批记录补：§8 填 commit hash（三件套入库后回填） |
