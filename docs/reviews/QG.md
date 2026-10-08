# Q-G 批记录：进程卫生与 fd（CLOEXEC / poll revents / relay 停管道 / 权限与 sun_path）

> 批次真源 = `docs/REVIEW-ROADMAP.md`「Q-G 进程卫生与 fd」+ `docs/reviews/AUDIT-2026-10-07.md`「Q-G」节
> + **Q-F 移交关联项**（`docs/reviews/QF.md` §6.3 第 5 条 / `QF-design.md` §7-4·§7-5）。
> 实现基准 = `docs/reviews/QG-design.md`（**v3**，设计门两轮：dsh `r16.W8cBAM` exit=0 → `r17.lQ201f` exit=0）。
> 本棒 = **实现 + 测试 + 判据登记 + 代码门（dsh r18）+ 批记录**；基线 HEAD = `8b96bb6`（Q-F 收口，工作树干净）。

---

## 0. 门命令与证据（本树实测）

| 门 | 命令 | 结果 |
|---|---|---|
| 测试 | `cargo test --workspace` | **639 passed / 0 failed**（提交前复跑 + 代码门整改后复跑 + 最终复跑，共 4 轮；其中 1 轮命中登记 flake `term::service::tests::attach_size_applies_to_pty`，甄别见下；基线 Q-F 收口 = 623 passed） |
| 静态 | `cargo clippy --workspace --all-targets -- -D warnings` | **clean**（0 告警；整改后复跑 clean） |
| 交叉 check | `cargo check --target {aarch64-unknown-linux-ohos, x86_64-unknown-linux-musl, aarch64-unknown-linux-musl} -p homeway-core -p homeway-cli -p homeway-capi` | **三目标全过**（仅 `go_fmt.rs` 的既有 `libc::time_t` deprecation 警告，非本批；F1 linux/OHOS 原子位分支由此覆盖编译面） |
| 手工回归（F3） | 真二进制 + 临时 state + 端口 43982：`relay start → stop → start → status` | `enabled=true **state=running**`；`role relay: 失败（relay run 线程退出（异常终结））` 行计数 **0**（修前必现，设计 §0.5 已实测） |
| fd 残留（F3） | 同进程 `lsof -p` PIPE 计数：running / stopped | **6 / 2**——停用后 relay 的 stop 管道两端 + wake 管道两端全部回收（旧形态 `16 PIPE` 读端残留消失） |
| 前台信号面（F3） | 前台 `relay` + `kill -INT` | 退出码 **0**（SIGNAL_HIT → `shutdown` 路径正常） |
| 出口回归（F4 落点在出口启动路径） | `tools/local-rust-exit.sh start 1` + `status` | `serve 就绪：…`/`intercept: 过境拦截就绪…` 判据行同串；`serve/key.bin` = `-rw-------`、`serve/` = `drwx------` |
| 互操作（F4 身份面） | `tools/local-rust-exit.sh go-client-add 1`（Go 基线客户端 × Rust 出口） | `host add` rc=0 → `服务会话: 就绪（会话在位，无桥直通）`（C3/C6/C16 同串） |
| 全仓 fd 扫描 | `grep -rnE "libc::(socket\|socketpair\|pipe\|pipe2\|open\|openat\|dup\|dup2\|dup3\|accept\|accept4\|eventfd\|memfd_create\|creat)\(" crates/` | 命中**只剩 `sysfd.rs` 自身**（+ `wgcore` 一处测试 `dup2` 造 `nval`）⇒ 11 生产点 + 2 测试点全部收口 |
| 词表 | `zsh tools/check-vocab.sh` | PASS（本批不动词表；登记面为判据变更表） |

**flake 甄别（一次命中，按登记口径放过）**：代码门整改后的第 1 次全量复跑（load ≈2.2）中
`term::service::tests::attach_size_applies_to_pty` 红（`service.rs:3948` 的**硬期限 60s 超时**：
「环境性挂死复现」）。甄别三件套：① **隔离复跑 3/3 绿**（0.86/0.86/0.87s，与登记表「PTY 时序 0.87s 隔离」
逐值吻合）；② **与改动面无交集**（本批未触 `term/service.rs`；`term_cli.rs` 改动仅在 CLI 的信号/poke 管道创建，
与该 PTY 用例零共享路径）；③ **基线可复现** = 该测试**已在「已知 flake 登记」表内**（2026-10-08 Q-E 复跑观察，
形态 = 「PTY 时序；隔离恒绿、并发/高负载下超时红」——登记时即为基线证据，本次命中症状逐字相同）⇒ 判 flake，
**不算回归**。其后全量复跑（load ≈2.17）**639 passed / 0 failed 全绿**。其余三个登记 flake
（`wgcore::stackb` / `daemon::server_bad_frame` / `speedtest_server::serve_send_end_to_end`）本批三轮全绿；
未出现「同树并发跑两个 cargo test」形态（本批全程串行）。

---

## 1. 实现清单（F1–F5）

### F1 自建 fd 全域 CLOEXEC（唯一收口 + 11 生产点 + 2 测试点 + N7）

- **新增 `crates/homeway-core/src/sysfd.rs`**（`pub`，CLI 复用；`lib.rs` 挂载）：`set_cloexec(BorrowedFd)`、
  `pipe_cloexec()`、`socket_cloexec()`、`socketpair_cloexec()`、`pub const SUN_PATH_MAX`。平台分派 =
  `#[cfg(target_os = "linux")]`（含 OHOS）走 `SOCK_CLOEXEC` / `pipe2(O_CLOEXEC)` **原子位**；darwin 建后立即 fcntl。
- **11 生产点**：`server/intercept/mod.rs`（`dial_nonblocking` 的 socket + `set_fd_nonblocking`→**`set_fd_flags`**
  一次收口 F_SETFL+F_SETFD）、`udpbatch.rs`（`bind_dual_stack`/`bind_v6_only`）、`server/bind.rs`（tx 线程
  socketpair）、`wgcore/mod.rs`（wake 管道）、`relay/mod.rs`（wake 管道）、`relay_cli.rs`（stop 管道 → `StopPipe`）、
  `serve_cli.rs`（serve stop 管道）、`daemon_cli.rs`（status --watch 管道）、`term_cli.rs`（attach 信号管道 + poke 管道）、
  `facade/bridge_host.rs`（`connect_budget` 改调 helper，单一实现）。
- **2 测试点**：`relay/mod.rs` 的 `TestRelay`（改 `StopPipe`）、`server/intercept/mod.rs` 的写侧重试用例（改 helper，断言不变）。
- **N7**：`term_cli.rs` poke 建立失败早退补 `drop_signal_pipes(pipes)`（SIG_W 留活 fd + 两 fd 泄漏）。
- 生命周期归属按设计 U9：CLI 三根既有 i32 结构体的管道「helper 建 → `into_raw_fd()` 交既有字段」；
  **新代码一律 `OwnedFd`**。App 传入的 tun fd 不动 flags。

### F2 `poll_fd` 返回 revents 派生 `Ready`（掐热自旋 + 补死亡上报 + 防误报）

- `wgcore/mod.rs`：`poll_fd(fd, events, deadline) -> io::Result<Ready>`（`Ready{readable,writable,hup,err,nval}`；
  片到无事件 = 全 false；EINTR 内部重试；只认未知位 ⇒ 保守 `err=true`）。
- 读侧（`tun_read_loop`）：EAGAIN 分支**可读优先**（HUP/ERR 不在此判死，落回 `read` 定性）；`n==0` 分支
  **判死只看 `hup||err||nval`** + **确认一拍（真睡眠 50ms 后复 poll）**；超时一律 continue；删掉不可达的
  `Interrupted` 外抛分支。**代码门③ 补强**：`n==0` 非判死路径在「poll 立返可读但空」形态加**地板睡眠**。
- 写侧（`write_fd_all`）：**期限检查写死循环顶**（U1）+ `hup/err/nval` 立即出线（不烧 5s）+ 注释订正
  （理由 = 立即出线，非 SIGPIPE）。
- 常量落位：`POLL_SLICE`/`DEAD_CONFIRM_DELAY`（`pub(crate)`）。

### F3 relay 停/唤醒管道：去单例、可重建、裸 fd 写收口、消泄漏、双关

- `relay/mod.rs`：新增 `pub struct StopPipe{r,w}`（每实例新建、`read_fd()`/`write_fd()`/`signal()`）；
  新增私有 `WakeHandle{Mutex<Option<OwnedFd>>}`（`wake()` 锁内写、`close()` 锁内 take）；
  wake 读端 = 局部 `OwnedFd`（RAII）；写端非阻塞（N3）；`PollSource::Stop` 判定补 `POLLNVAL`（防御一行）；
  `run` 文档写明 stop_fd **借用、不 close** 契约；收工改 `wake.close()`（先断写端、读端随作用域关）。
- `relay_cli.rs`：`RelayProc{stop: Option<StopPipe>, join, exited}` + **幂等 `shutdown`**（disarm → 条件恢复
  SIG_DFL → `signal()` → `join` → drop 两端）；`stop()`/`Drop` 共用；`arm_signal(&self)`（armed 状态由
  `STOP_FD >= 0` 单源，无 `armed` 字段）；前台等待改**单通道**（`SIGNAL_HIT` + `exited()`，删双读者竞态）；
  删 `STOP_PIPE` OnceLock/`new_stop_pipe`/`stop_pipe`。
- **代码门①/⑤ 相关**：`shutdown` 的 SIG_DFL 恢复**只在曾 arm 时**执行（见 §4 处置表与 §5.1 偏离登记）。

### F4 权限原子化（含高危私钥面）+ `sun_path` 平台上限单源

- `server/state.rs`（**高危**）：`key.bin` 改「创建即 `.mode(0o600)` + fchmod 归一 + 失败告警」；
  **读路径**（存量 key.bin）best-effort 归一（代码门① 补强）；`append_file`（tokens/revoked 台账）同款去双层静默；
  `State::open` 目录 `DirBuilder::mode(0o700)` + create 后无条件 chmod + 告警。
- `nodestate.rs`（A4/A5/A6）、`daemon/carriers/mod.rs`（A7）、`daemon_cli.rs`（A8）、`wtransport/endpoint_cache.rs`
  （B 组新增）、`session_lock.rs`（B 组新增）：`.mode(0o600)`/`DirBuilder::mode(0o700)` + fchmod 归一，
  **失败一律告警不阻断**（代码门② 统一）。
- UDS：`daemon/listen.rs` **目录 0700 前置到 bind 之前** + `PathTooLong` 文案/判据；`files_server.rs::listen_local_service`
  加「bind 前目录 0700（失败告警不阻断）」+ `chmod 0600` 失败**告警**（Go `chmodTighten` 同串形态）+ 量法统一
  `as_os_str().len()` + 契约写入函数文档。
- `sun_path` 单源：`SUN_PATH_MAX`（darwin 103 / linux·OHOS 107），判据统一 **`len > SUN_PATH_MAX`**；
  五处使用点改走单源；两处「已同义」点只注明同源（不字面改调）。

### F5 五处无界 `c.stop()` 落回预算

- `wgcore`：`pub(crate) const CLIENT_CLOSE_BUDGET = 2s`（与 Q-F `EXIT_RPC_BUDGET` 同值）。
- 五处改 `stop_within(Instant::now() + CLIENT_CLOSE_BUDGET)` + 记行：`facade/tun_exec.rs`（`request_stop` 的
  Preparing 分支 / `Finish::drop` / `gen_loop` 装配窗口）+ `session/mod.rs`（`rebuild_session` 的 new·old）。
- `Drop for Client` **保持无界**（设计 §3-D3）。

---

## 2. 测试证据（新增/修改单测）

| 测试 | 位置 | 钉什么 |
|---|---|---|
| `created_fds_carry_cloexec_and_set_is_idempotent` | `sysfd.rs` | 三 helper 产物带 `FD_CLOEXEC` + 幂等（修前红） |
| `sun_path_max_matches_platform` | `sysfd.rs` | 平台值（darwin 103 / linux 107）+ 结构自洽 |
| `created_fd_not_inherited_across_exec` | `sysfd.rs` | **真继承探针**：`/bin/sh -c '[ -e /dev/fd/N ]'`（内建 `[` ⇒ 无子进程 fd 复用面）；本机另证裸 `pipe` 同探针红（exit=3）⇒ 判别力成立。**偏离设计 U6 的 dup2 形态并说明理由**（`dup2` 按 POSIX 清 CLOEXEC ⇒ 探针恒绿） |
| `poll_fd_ready_masks` | `wgcore/mod.rs` | 四态掩码：超时全 false / **`writable` 正例** / **`readable` 正例** / 关写端 `hup`（只断言 hup）/ 满管道不得报异常位 / `nval`（高位号 900 构造） |
| `ready_masks_dead_predicate_only_uses_hup_err_nval` | `wgcore/mod.rs` | 判死谓词只看 hup/err/nval（U8 的 poll 层替代断言） |
| `write_all_deadline_returns_timedout` | `wgcore/mod.rs` | 预算上界（不可写且无 HUP ⇒ 5s 内 TimedOut）——**定性 = 回归守卫，非 U1 判别用例**（见 §4 处置 ⑧） |
| `write_all_dead_fd_returns_promptly` | `wgcore/mod.rs` | 死 fd 立即出线（<1s；EPIPE 先行 ⇒ `hup` 分支不可达，**非判别用例**，已登记） |
| `tun_read_loop_n0_with_hup_reports_dead` | `wgcore/mod.rs` | **darwin 实测**：`n==0` + `POLLHUP` ⇒ ≤1s 上报 `TunFdDead` 且线程退出（修前：热自旋不退出） |
| `read_side_prefers_readable_on_hup_with_data` | `wgcore/mod.rs` | 可读优先：先收最后一包、后收 `TunFdDead` |
| `stop_pipe_is_per_instance_and_wake_handle_never_writes_after_close` | `relay/mod.rs` | `StopPipe` 两次新建 fd 对不同（修前 OnceLock 恒同 ⇒ 红）+ `signal()` 可读 + `WakeHandle.close()` 后 `wake()` 不写（含号复用对抗） |
| `relay_run_exits_immediately_when_stop_pipe_write_end_already_closed` | `relay/mod.rs` | 单例语义复刻（只关写端 ⇒ run 立返；机制复刻 + 文档化） |
| `relay_proc_shutdown_is_idempotent_and_disarms_stop_fd` | `relay_cli.rs` | arm→`STOP_FD`=w / shutdown→-1 / 结构断言 `stop.is_none()` / 再 shutdown + drop 不 panic |
| `permissions_are_atomic_0600_and_dir_0700` | `server/state.rs` | 全新 state：目录 0700 + key.bin/tokens.jsonl/revoked.jsonl 均 0600（**回归守卫**：正常 umask 下修前最终也是 0600，设计 §4.2 已降级） |
| `save_artifact_is_0600` | `wtransport/endpoint_cache.rs` | 端点缓存产物 0600 |
| `listen_control_tightens_dir_to_0700` | `daemon/listen.rs` | 0755 目录上调 ⇒ 返回后 0700（目录前置） |
| `listen_control_path_limit_boundary` | `daemon/listen.rs` | 深路径正例/负例：`dir + "/control.sock"` 长度 == `SUN_PATH_MAX` 可 bind、== +1 必 `PathTooLong`（off-by-one 订正的判据面） |
| `uds_listen_and_occupancy`（扩展） | `files_server.rs` | 0600 + **bind 前目录 0700**；chmod 失败告警路径不可构造（登记为代码面复核） |
| `flush_out_eagain_returns_with_backlog`（改） | `server/intercept/mod.rs` | 测试态 socketpair 走 helper（断言不变） |

**覆盖有限项（如实登记）**：F5 五处调用点无单测缝（`Client` 非 trait）⇒ 代码面 + 预算表核对 + 手工观察；
F1 的 11 处**落点**覆盖为代码面（探针只证 helper 语义）；`files_server` chmod 失败告警路径不可确定构造。

---

## 3. 判据行登记（`docs/INTEROP-CRITERIA.md`，与代码同批 commit）

### §5.1 变更记录（2 行，已落「登记表」）

1. **`sun_path` 上限**：`len >= 100` → **`len > SUN_PATH_MAX`**（放行 103/107；文案加〔平台值〕；量法统一
   `as_os_str().len()`）；影响面 = `daemon/listen.rs`、`files_server.rs`、`facade/bridge_host.rs` + 两处同义点注明
   + 深路径单测；**行为放宽**（Go 仍拒 100–107）；已核无脚本/文档消费方。
2. **`relay stop` 后 `relay start` 的 broken 路径行消失**（`role relay: 失败（relay run 线程退出（异常终结））…`）；
   修前无限刷（设计 §0.5 实测）→ 修后不出现（本批手工回归复现）；**Q-H 条目「relay start 不可恢复」由本批 F3 修根因**。

### §5.2 数值语义（**4 行**；比设计草稿多 1 行 = 代码门⑨ 的写侧预算收紧）

- `unhealthyReason=fd` 触发集与时机（新增 HUP/ERR/NVAL 型 + 确认一拍；**残余：持续型 HUP 会被确认拍放行误判**）。
- **TUN 写侧预算语义**（新增）：期限检查写死循环顶 ⇒ 「恰好到点变可写」的那一次写入被放弃（代码门⑨ 登记）。
- 隧道域世代收尾上界：无界 → ≤2s/处（五处；`Drop for Client` 仍无界）。
- **新增 additive 观测**：`files` UDS chmod 告警 + `state.rs` key/台账/目录/**存量私钥读路径**告警 +
  `nodestate` config 模板 / `carriers::save_json_atomic` / `daemon_cli` spawn 日志 / `endpoint_cache` 收紧失败告警
  （代码门② 统一为告警不阻断）+ F5 detach 记行。

### §5.3 已知口径注记（3 条）

- 【Q-G】TUN fd 失效判据（判死只看 hup/err/nval、超时不判死、可读优先、确认一拍、**地板睡眠**、偏离 Go 的加固、
  残余：OHOS 语义不可取证 + 持续 HUP）。
- 【Q-G】fd 继承纪律（全仓自建 fd 一律 CLOEXEC；App tun fd 不改 flags；真实继承面 = 3 处 std `Command` exec；
  PTY shell 由 portable-pty 净化但**不作豁免理由**；残余：darwin 创建→fcntl 窗口 + linux 原子路径无本机运行期验证）。
- 【Q-G】权限口径（普通文件「创建即 0600 + fchmod 归一」；目录「`.mode` + 无条件 chmod」；UDS「目录先 0700 +
  bind→chmod 0600 + 告警」；**权限面定性 = 修缺陷（对齐 Go）⇒ 不登记为判据变更**）。

---

## 4. 代码门（dsh r18）意见与逐条处置

- **轮次目录**：`/tmp/dsh-review/r18.5G1xcc/`（`prompt.txt` / `output.md` 135 行 / `stderr.log`）。
- **命令**：仓根 `dsh --profile headless "$(cat prompt.txt)" > output.md 2> stderr.log`，**前台捕获 `exit=0`**。
- **评审对象**：**工作树 diff**（21 个改动文件 + 新文件 `sysfd.rs`，未 commit）。
- **结论**：**10 条问题（0 高 / 3 中 / 7 低）+ 4 条设计与代码矛盾（D1–D4）+ 7 组「看过没问题」**；
  评审独立取证（全仓 raw-fd 扫描、`baseline/homeway` 逐点核对、13 个新单测实跑、3 轮 stop/start + lsof fd 计数、
  两目标交叉 check、F4 高危断言）并复现了 F3 手工回归与泄漏面。
- **一句话**（原文）：本批实现与设计 v3 高度吻合，F1/F2/F3 的机制面与判据登记经其实跑/复现均成立，
  fd 生命周期与 revents 语义未发现真缺陷；必须处理 = 问题 1（存量私钥不归一）与问题 2（F4 失败策略倒挂）。

### 4.1 逐条处置表

| # | 严重度 | 意见（摘要） | 处置 | 落点 |
|---|---|---|---|---|
| 1 | 中 | **存量 `key.bin` 权限永不归一**（读路径早退，命中旧 chmod 失败的私钥永远 0644） | **认同改**：读路径补 best-effort 归一（`OpenOptions::read` + `set_permissions(0600)` + 失败告警，读语义不变） | `server/state.rs::private_key` + §5.2 additive 行 |
| 2 | 中 | **F4 失败策略倒挂**：A1–A3（高危）告警放行，A6/A7/A8（非凭据）硬失败 ⇒ 新增未登记行为变化（config 模板 fchmod 失败 = 统一进程起不来；`save_json_atomic` 硬失败留 `.tmp`） | **认同改**：A6/A7/A8 + `endpoint_cache` 统一为**告警不阻断**（与 A1–A5 一致；Go `chmodTighten` 亦为告警） | `nodestate.rs`/`carriers/mod.rs`/`daemon_cli.rs`/`endpoint_cache.rs` + §5.2 additive 行 |
| 3 | 中 | **`n==0` 非判死路径无最低睡眠** ⇒ 「read 返 0 且 poll 立返 POLLIN（可读但空）」形态仍可满核自旋（修前同形，属审计条目残余） | **认同改（加固）**：非判死且首拍 `readable` 时加**地板睡眠**（`DEAD_CONFIRM_DELAY`）；超时/HUP 形态不受影响 | `wgcore/mod.rs` + §5.3 注记 |
| 4 | 低 | 确认拍对**持续型** HUP 无效；影响面登记只写「更早/更准」 | **认同改（登记）**：§5.2 该行补「持续 HUP 的健康 fd（若存在）会被误判——确认拍只滤瞬时形态」+ §5.3 | `docs/INTEROP-CRITERIA.md` |
| 5 | 低 | F3 的 `SIG_DFL` 收窄是**未登记偏离**；且 N9 的窗口归因不成立（in-flight handler 由「swap(-1) 先于 close + join 先于 close」兜住） | **认同改（记账 + 注释）**：代码注释订正归因；偏离登记在本文 §5.1 与 `relay_cli.rs` 注释（双处） | `relay_cli.rs` + 本文 §5.1 |
| 6 | 低 | 三处注释与实现矛盾（`StopPipe` drop 序写反 / `Client::stop` 文档说「隧道域三条」已过时 / `unified_cli` 说「RelayProc 无 Drop」） | **认同改**：三处一并订正 | `relay_cli.rs`/`wgcore/mod.rs`/`unified_cli.rs` |
| 7 | 低 | 判据登记与代码行文不一致（`listen.rs` 缺〔平台值〕）+ 悬空引用 `docs/reviews/QG.md` | **认同改**：`listen.rs` 补齐〔平台值〕（三处逐字一致）；`QG.md` 本文件即落地（引用消解） | `daemon/listen.rs` + 本文件 |
| 8 | 低 | 测试有效性：① `write_all_hup_returns_promptly` 走 EPIPE 恒绿；② `write_all_deadline_returns_timedout` 修前也绿；③ `writable` 无正向断言；④ F1 探针只覆盖 helper | **认同改（③ 改码；①②④ 登记）**：③ 补 `readable`/`writable` 正向断言；① 改名 `write_all_dead_fd_returns_promptly` + docstring 标明「非判别用例」；② docstring 标明「预算上界回归守卫」；④ 在本文 §2「覆盖有限项」登记 | `wgcore/mod.rs` + 本文 §2 |
| 9 | 低 | 写侧期限检查的语义收紧未登记（到点不再重试最后一次写入） | **认同改（登记）**：§5.2 新增一行 | `docs/INTEROP-CRITERIA.md` |
| 10 | 低 | 越界外观察：`daemon_cli` 的 launchd 探测与 `--state` 不匹配（临时 state 会去等**生产出口**的 launchd 代理；若 KeepAlive 触发会把生产出口拉起） | **认同（不改）**：Q-H 面（`--state` 形态族/N1·L7 同族）；**提请主会话在 Q-H 立条**（本文 §6 移交项） | 登记入 §6 |

### 4.2 设计与代码现状矛盾（D1–D4）

| # | 设计条款 | 代码现状 | 处置 |
|---|---|---|---|
| **D1** | §2-F3.2/§7.6-N9：`shutdown` **无条件**恢复 SIGINT/SIGTERM 为 SIG_DFL | 代码**只在曾 arm 时**恢复（统一进程从未 arm ⇒ 不覆盖宿主 handler） | **代码更正确**：无条件恢复会打掉统一进程宿主的 handler（Ctrl-C 从优雅收工退化为默认处置）。**登记为本批对设计的显式收窄**（双处：`relay_cli.rs` 注释 + 本文 §5.1）；设计意图（防后续信号落 handler）保留 |
| **D2** | §2-F4.1：A2/A6/A7/A8「`.mode` + fchmod + **失败告警**」 | 实现初版把 A6/A7/A8 写成硬失败 | 已按 D2 原文改回**告警不阻断**（见处置 2）——设计条款恢复一致 |
| **D3** | §4.2 F2 测试计划「写满管道对写端 poll ⇒ `writable = true`」 | 实现初版退化为「不得报异常位」；且注释引用了不存在的「§4.2 降级登记」 | **设计该句本身不成立**（满管道写端 poll 恒返 0 ⇒ 不可写）；改为**空管道写端 ⇒ `writable = true`** 的正例（正确形态）+ 保留「满管道不得报异常位」；不存在的登记引用已消解 |
| **D4** | §2-F2.3「其余 ⇒ continue」/U8「构造不出」 | `continue` 前无睡眠 ⇒ 热自旋口子 | 代码忠实设计；**设计本身留口子** ⇒ 本批按代码门③ 加地板睡眠（加固，登记 §5.3） |

### 4.3 评审「看过没问题」项（照录，不改动）

① fd 生命周期：CLOEXEC 覆盖无漏点（独立扫描只剩 `sysfd.rs`）；App tun fd 未动；exec 面恰 3 处；pipe 重建竞态
（双 stop/号复用/double-close）结构性消除；3 轮 lsof 无逐轮增长。② revents 语义：四态处置正确、写侧三重封顶、
可读优先实跑通过、darwin 热自旋修正落地。③ 权限与平台：普通文件原子 0600 成立、Go oracle 逐点核对成立、
`sun_path` 边界实跑通过、UDS 目录 0700 无越权（三生产调用点都是 `serve_dir`）、`files` 告警与 Go `chmodTighten` 一致。
④ Go 对齐：修缺陷/偏离加固/新差异三分类逐点成立；判据行数值语义与代码逐字一致（文案小节见处置 7）。
⑤ Go 直译痕迹：未发现新的直译痕迹（全 RAII、无字符串错误、无无谓拷贝；`StopPipe` 落 core 比设计稿更好）。
⑥ 越界：21 个文件全在设计清单内；未触 Q-I 尾段/Q-H 面（含 `serve_cli::wait_stop_pipe` 判据缺陷保持原样）。
⑦ 其他：新增测试构造纪律到位（高位号 `nval`、结构断言替代 EBADF、真睡眠确认拍、`dir+13` 量纲）；
F5 五处调用点与登记一致（grep 计数 5）。

---

## 5. 实现口径与记账（供后续批核对）

### 5.1 对设计文档的显式偏离（2 处，均为「实现期发现设计条款与代码现状矛盾」的处置）

1. **SIG_DFL 恢复条件化**（D1；设计 §2-F3.2 明文「无条件」）：改为「仅本 proc 曾 arm」。
   理由：统一进程的 SIGINT/SIGTERM handler 由宿主安装，无条件恢复会**打掉宿主 handler**（回归）；
   relay 自己的 handler 只在前台单角色安装（`install_relay_stop_signal` 只在 `cmd_relay` 调用）。
   两处登记 = `relay_cli.rs::shutdown` 文档注释 + 本表。代码门（r18）第 5 条已复核并认同收窄技术正确。
2. **F1 端到端探针形态**（设计 U6 建议「自重入 test harness + `dup2` 高位号」）：改为
   `/bin/sh -c '[ -e /dev/fd/N ]'`（内建 `[`，子进程不新开 fd）。
   理由：`dup2` 按 POSIX **清除** `FD_CLOEXEC` ⇒ 该形态探针恒绿（连修前也绿），与设计 §4.2 的
   「修前同探针红」自相矛盾；`/dev/fd` 探测 + 内建 `[` 同时避开「harness 复用号」与「dup 清标志」两个坑。
   判别力已实测（裸 `pipe` 同探针 exit=3 / CLOEXEC exit=0）。登记于 `sysfd.rs` 测试文档注释 + 本表。

### 5.2 记账纪律

- 判据变更**同批 commit**（`docs/INTEROP-CRITERIA.md`）；未登记措辞改动为零（本批唯一文案改动 = `sun_path` 三处，
  已登记且与代码逐字一致——代码门第 7 条复核项已闭合）。
- 设计文档（`QG-design.md`）与设计门记录随本批入库（设计棒产物，未 commit 的部分由本批一并提交）。

---

## 6. 不做项、移交项与残余登记（防静默漏做）

### 6.1 设计 §6 逐条核对（8 条）

| # | 设计条目 | 本批处置 |
|---|---|---|
| 1 | Linux 原子路径（`pipe2`/`SOCK_CLOEXEC`）的运行期验证 | **未做**（本机 darwin）——以三目标编译探针 + 交叉 check 为准（本批已跑，全过）；如实登记 |
| 2 | `Drop for Client` 改有界 | **不做**（§3-D3 裁定；`Drop` 是最后兜底） |
| 3 | 3 处 std `Command` exec 的主动净化 | **不做**（CLOEXEC 补齐后继承面已闭合；std 无该 API） |
| 4 | `serve_cli`/`term_cli`/`daemon_cli` 的 stop 管道结构改造 | **不做**（单次生命周期、无 restart 面；只补 CLOEXEC + N7 早退） |
| 5 | daemon SOCKS/hosts 控制面项（Q-H）、`dnsface.rs` 64KB（Q-I 尾段）、DNS TTL/files 拷贝（Q-I 尾段） | **不越界**（代码门⑥ 已独立核对） |
| 6 | `rebuild_session` 的 `Client::start` 与缓存落盘无期限、`stop_within` detach 后引擎线程在飞 | **不做**（Q-F 在册残余 `QF.md` §6.3-1/-2） |
| 7 | **跨批消费登记**：Q-H 条目「`relay stop` 后 `relay start` 不可恢复」的根因面由本批 F3 修掉 | **已修（F3）**；**Q-H 收口时须勾选该条并复核**（防重开同一修）——见 §7 |
| 8 | `serve_cli::wait_stop_pipe` 的 `>= 0` 判据缺陷 | **不做**（Q-H 面；代码门⑥ 已核未顺手改） |

### 6.2 本批残余（如实登记）

1. **darwin 创建→fcntl 窗口**（无原子位可用；窗口 ≈ 数十纳秒 + exec 面 3 处）。
2. **linux 原子路径无本机运行期验证**（只有编译探针 + 交叉 check）。
3. **OHOS VPN fd 语义不可取证**（capi 只收 App 传入的裸 fd）——`unhealthyReason=fd` 的误报面残余；
   **持续型 HUP** 会被确认拍放行误判（代码门④）。
4. **UDS 同用户权限窗口**（`bind()` 无 mode 参数；跨用户暴露面由目录 0700 关闭）。
5. **F5 无修前红、调用点覆盖有限**（无桩缝）；`tun_stop` 的 4s 串行上界**只减不消**。
6. **F4 的「0644 窗口 / chmod 失败」在单测不可确定观测**（回归守卫 + 代码面复核）；
   `files_server` 的 chmod 失败告警路径不可构造（代码面复核）。
7. **测试有效性降级登记**（代码门⑧）：`write_all_dead_fd_returns_promptly`（EPIPE 先行，`hup` 分支不可达）、
   `write_all_deadline_returns_timedout`（预算上界守卫，修前亦绿）——均为**非判别用例**，保留作回归网。
8. **F1 落点覆盖为代码面**（探针只证 helper 语义）。
9. **`n==0` 地板睡眠**属加固（设计未要求）：只覆盖「poll 立返可读但空」形态；正常 EOF/超时形态零影响。

### 6.3 移交项（提请主会话）

- **Q-H 立条**（代码门第 10 条，本批不修）：`daemon_cli::detect_launchd_agent` 的 launchd 代理探测**与
  `--state` 无关**——对非默认 state 操作时，CLI 会去等**生产出口**的 launchd 代理（本批手工回归实测：
  `relay start --state /tmp/…` 打印「守护进程未运行（launchd 代理 me.zhaozhe.homeway-exit 在册）——等 KeepAlive 重拉…」
  并白等 4s；若 KeepAlive 真被触发会把生产出口拉起）。与 Q-H 的「N1/L7 前台默认 state 与统一进程对齐」同族。
- **`tools/local-rust-exit.sh go-client-add` 的既有脚本小瑕**（本批实测发现，未改）：该分支的
  `our_pid "$CLIENT_PIDFILE"` 少了 `homeway` 形态参数 ⇒ Go 客户端（`homeway-go`）实际已起也会被判「启动即退出」。
  本批以手工等价命令完成互操作回归（`host add` + 就绪行）；脚本修正属工具面，留给后续批。

---

## 7. Q-H 跨批勾选义务（主会话收口时复核）

- **`AUDIT-2026-10-07.md` Q-H 节条目「`relay stop` 后 `relay start` 不可恢复」的根因面由本批 F3 修掉**
  （`STOP_PIPE` 单例复用已关写端 → per-proc `StopPipe` + 幂等 `shutdown`；本批手工回归 `state=running`）。
  **Q-H 收口时必须勾选该条并复核，不得重开同一修**。
- 判据面已在 `docs/INTEROP-CRITERIA.md` §5.1 第 2 行登记（broken 路径行消失）。

---

## 8. 需上报项

1. **本批 2 处对设计文档的显式偏离**（§5.1：SIG_DFL 条件化、F1 探针形态）——均已在代码注释 + 本文件双处登记，
   代码门（r18）已复核第 5 条（认同）与探针理由（未提异议）。
2. **代码门 3 条「中」全部认同并已改码**（存量私钥归一 / F4 失败策略 / `n==0` 地板睡眠）；改码后
   `cargo test --workspace` 与 clippy 已复跑全绿。
3. **§6.3 移交项 2 条**（Q-H 立条：launchd 探测与 `--state` 不匹配；脚本 `go-client-add` 的 `our_pid` 形态参数）。
