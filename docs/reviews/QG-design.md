# Q-G 进程卫生与 fd（CLOEXEC / poll revents / 停管道生命周期 / 权限与 sun_path）设计文档

> **v3（2026-10-08）**：v2 过设计门**第二轮复校**（dsh `r17.lQ201f`，**exit=0**）——上轮 34 条判「30 真闭合 / 4 部分 / 0 否」，
> 另提 **8 条 v2 新问题**（**N1 高**：n==0 判死条件在 darwin 上不可达 ⇒ 热自旋仍在；**N2 中**：`sun_path` off-by-one；
> **U1 高**：写侧期限收口悬空 ⇒ 会退化成无睡眠死循环）+ **9 条实现棒未定义点**；**v3 = 第二轮整改稿**（§7.5/§7.6 逐条勾销）。
> **v2→v3 关键改动**：F2 判死**只看 hup/err/nval**（删「且无 readable」——本棒实测 darwin 关写端后 `poll(POLLIN)`
> **恒返 `POLLIN|POLLHUP`**，旧条件永不成立）+ 确认拍改**真睡眠** + 写侧期限检查落点写死；F4 判据统一 **`len > SUN_PATH_MAX`**
> （off-by-one 订正）+ 目录面补 umask 归一 + 权限面补全扫描（新增 `endpoint_cache`/`session_lock` 同类两处）；
> F3 补 `r` 交接/`armed`/退出路径/`SIG_DFL` 四细节；F1 补探针 `dup2` 高位与 CLI 管道 RAII 归属；F5 补常量落点。
>
> **v2（已并入）**：v1 过设计门第一轮（dsh `r16.W8cBAM`，**exit=0**）——1 高危（权限面漏掉
> 出口私钥与 token 台账 `server/state.rs`）+ 8 中 + 25 低；v2 = 第一轮整改稿（高/中全改；F4 扩面 + 权限面真扫描；
> 扫描计数订正为「12 生产 raw 创建点 = 11 缺 + 1 正确，另 2 测试点」；§0.4 `SOCK_CLOEXEC` 取证改编译探针；
> F2 改返回 revents 派生状态；F3 定死前台 stop 读端 API + arm/disarm 绑 `shutdown()`；F5 补第 5 处 `tun_exec.rs:1040`）。

- **批次**：Q-G（整改批；真源 = `docs/REVIEW-ROADMAP.md`「Q-G 进程卫生与 fd」+ `docs/reviews/AUDIT-2026-10-07.md`「Q-G」节）
  + **Q-F 移交关联项**（`docs/reviews/QF.md` §6.3 第 5 条 / `QF-design.md` §7-4·§7-5）。
- **本棒范围**：复验 + 设计 + 设计门（**不写产品代码**）。
- **基线**：HEAD `8b96bb6`（Q-F 收口），工作树干净。
- **本棒实跑**（证据可复现）：
  1. 全仓 raw fd 创建点静态扫描（§1 给出确切命令与计数）；
  2. **darwin `sun_path` 上限实测探针**（`/tmp/sunpath_test.rs`：`UnixListener::bind` 长度 99→108 逐点）；
  3. **relay stop→start 重启失效实测复现**（真二进制统一进程 + 控制面，临时 state `/tmp/qg-relay-repro`，§0.5，已清理）；
  4. **三目标 libc 编译探针**（`/tmp/fdprobe`：`x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-ohos` / `aarch64-apple-darwin`
     的 `SOCK_CLOEXEC`/`SOCK_NONBLOCK`/`pipe2` 可用性，§0.4）；
  5. **std `Command` 继承行为实测探针**（`/tmp/fdleak_probe.rs`：未设 CLOEXEC 的管道 fd 出现在子进程 `/dev/fd` 列表中，§0.4）；
  6. portable-pty 0.9.0 与 libc 0.2.190 源码取证；
  7. **darwin `poll(2)` EOF 语义实测探针**（`/tmp/poll_eof_probe.rs`：管道关写端后读端 `poll(POLLIN)` **恒返 `POLLIN|POLLHUP`**
     ——这是 v2→v3 改判死条件（§2-F2）的依据）。
- **不越界**：Q-I 尾段（DNS TTL/files 拷贝/`dnsface.rs` 64KB）与 Q-H（CLI/daemon 控制面协议与解析）不在本批；
  本批只碰 daemon 的**文件权限与 sun_path 常量**。

---

## 0. 复验（证据先行）

### 0.1 方法

1. 读派单条目 → **回源码重定位**（行号按 HEAD `8b96bb6` 重取；Q-B/Q-C/Q-I 前段/Q-F 都改过本批文件）。
2. **扫描优先于清单**：P1-1 的修法是「扫描确认全部 socket 创建点」——扫描本身就是本批核心证据（§1）；
   权限面同法（§1.5，设计门 4.1 要求）。
3. **平台事实与行为断言一律实跑探针**（darwin 实测；跨目标用编译探针）；不可实跑的（Linux 运行期）如实标注。
4. **第三方行为读依赖源码**（portable-pty）+ 实跑对照（std `Command` 继承探针）。
5. 可复现的行为缺陷（relay 重启）跑**真二进制**复现，证据落盘（§0.5）。

### 0.2 逐条复验结果表

| # | 审计条目（摘要） | 真伪 | 现行位置（HEAD `8b96bb6`） | 结论 / 订正 |
|---|---|---|---|---|
| 1 | **P1** 拦截层自建 fd 全部缺 `FD_CLOEXEC`（`intercept/mod.rs:184-197`、`udpbatch.rs:42/80`；term 真 exec ⇒ shell 继承 + 悬挂流；对照 `bridge_host.rs:108-114` 正确示范） | ✅ **成立**（**危害归因两项订正**） | 创建点：`server/intercept/mod.rs:224`（`libc::socket(domain, ty, 0)`）、`udpbatch.rs:42/80`；非阻塞设置 `intercept/mod.rs:342-350`（只 `F_SETFL`）。正确示范：`facade/bridge_host.rs:108-121`。**扫描新发现**：`server/bind.rs:809`（tx 线程唤醒 `socketpair`）+ 7 处 `libc::pipe`（§1.1） | 修（F1，全域补齐）。**订正①（shell 继承不成立）**：`term/pty.rs:252` 的 shell spawn 走 **portable-pty 0.9.0**，其 `spawn_command` 的 child `pre_exec` **无条件调用 `close_random_fds()`**（枚举 `/dev/fd` 关掉 ≥3 的全部 fd；`portable-pty-0.9.0/src/unix.rs:276`，体 `:152-176`，失败静默）⇒ **「shell 继承 + 悬挂流」在该路径不成立**。**订正②（真实继承面 = 3 处 std `Command` exec，且已实测证实）**：`term/pty.rs:71`（`/usr/bin/dscl`）、`term/agent.rs:513`（`ps`，term 检测周期调用）、`homeway-cli/src/daemon_cli.rs:192`（**自 exec 长命统一进程**，`Command::new(exe)`）。**实测**（`/tmp/fdleak_probe.rs`）：未设 CLOEXEC 的管道 fd **确实出现在 std `Command` 子进程的 `/dev/fd` 列表**里 ⇒ 继承面真实。**审计原文的 `relay/mod.rs:508-513/970-978` 裸 fd 写**行号漂移 → 见第 3 条。**定性订正**：Go 侧拦截层用 `net.Dialer`（`baseline .../pkg/intercept/intercept.go:150/340`）、Go `net` 建的 fd 一律 CLOEXEC ⇒ 本项是**移植回退（修缺陷、对齐 Go）**，不是「卫生纪律」 |
| 2 | **P1** `wgcore::poll_fd` 忽略 revents + `n==0` 分支再 poll ⇒ HUP/ERR 下读-空-poll 热自旋 + 永不报 `TunFdDead`；写侧白等 5s | ✅ **成立**（「永不报」需限定） | `poll_fd`：`wgcore/mod.rs:1656-1679`（`r<0` 只判 EINTR，否则**无条件 `Ok(())`**，不看 revents）；写侧 `write_fd_all`：`:1633-1652`（`WRITE_BUDGET=5s`）；读侧 `tun_read_loop`：`:1689-1750`（EAGAIN 分支 `:1710-1723` 已把 poll 失败当死；`n==0` 分支 `:1739-1750` **`.ok()` 吞错 + `continue`**） | 修（F2，v2 起改「返回 revents 派生状态」，§2-F2）。**订正**：`TunFdDead` 并非「永不报」——**读错误路径**（`read` 返 EBADF/EIO，`:1728-1735`）会报；**永不报的是 HUP/ERR/NVAL 形态**（`read` 返 0 或持续 EAGAIN）⇒ poll 因 HUP/ERR 立即返回、每轮不睡 = **满核热自旋**且无死亡上报。写侧同理（HUP/ERR 下忙重试到 5s 期限，烧 CPU 而非「白等」）。**Go oracle**：`baseline .../tunfd_unix.go` EAGAIN 分支 `unix.Poll(pfd, 500)` **同样不看 revents**（同族缺陷；Go 未爆是因为 `n==0` 直接 `return 1, nil` 交上游、不在这里 poll）⇒ 本项 = 修缺陷 + **偏离 Go 的补偿面加固**（登记）。**v3 补充关键事实（设计门 r17 N1）**：darwin 上「EOF 与 `POLLIN` 同置」——关写端后读端 `poll(POLLIN)` 恒返 `POLLIN\|POLLHUP`（本棒 `/tmp/poll_eof_probe.rs` 实测三次同值 17；Linux `pipe_poll` 仅在非空时给 `POLLIN`）⇒ 死判定**不得**依赖「无 readable」 |
| 3 | **P1** relay 唤醒 pipe 生命周期（`STOP_PIPE` 单例 / `RelayProc::stop` 关写端 ⇒ start 复用同一对 fd、run 立即自退、supervisor 无限退避；第二次 stop 写落复用 fd 号；另存裸 fd 写） | ✅ **成立**（**本棒独立实测复现**，§0.5） | 单例：`relay_cli.rs:371-380`；`stop()`/`Drop`：`:107-146`（写 `stop_w` → join → close；`stop(mut self)` ⇒ 函数尾 **Drop 再写一次 + 再关一次**）；run 侧 `stop_fd` **从不 close**：`relay/mod.rs:350/462-472`（`POLLIN\|POLLERR\|POLLHUP` 判 stop）；收工只关 wake 两端：`:584-585`；裸 fd 写：`:1153-1161`（握手线程把 `wake_w` 的 **i32 值**拷进闭包）+ `relay_cli.rs:117/141` | 修（F3）。**新增项**：① **stop 管道读端无人关**（`relay/mod.rs:350` 不 close、`RelayProc` 只持写端）——但**订正量级**：`OnceLock` 全进程只建一根管道 ⇒ 实测残留 fd（lsof `16 PIPE`）是**一次性**残留，**不是「每次 rebuild 泄漏 1」**；改成 per-proc pipe 后读端必须随 `RelayProc` 关（否则才真成为每次泄漏）⇒ N2 仍是必修项，措辞改「单例残留 + 改造后的泄漏面」；② **`RelayProc::stop` → `Drop` 的 double-write + double-close**（副注释自称「write 对已关 fd 只 EBADF」不成立：`close()` 打在**已复用**的号上会关掉别人的 fd）；③ `TestRelay`（`:1596-1636`）是**同一缺陷类**；④ `PollSource::Stop` **不认 `POLLNVAL`**（`:470`，若读端先关则 poll 立返 NVAL 不匹配任何位 ⇒ 满核忙转） |
| 4 | **P2** daemon 文件权限先建后 chmod（`carriers/mod.rs`、`listen.rs`）；`sun_path` 上界写死 100（Linux 107 可用） | ✅ **成立**（**范围远宽于审计：权限面 10 处，含 1 处高危**） | 写死 100 **3 处**：`daemon/listen.rs:21`（文案）+ **`:43`（判定）**、`files_server.rs:964-969`（files/speedtest/**term** 三服务共用）、`facade/bridge_host.rs:187`（`MAX_UNIX_SOCKET_PATH`，用点 `:491/:516`）。
create-then-chmod **12 处**（§1.5 A 组；其中 3 处为敏感面）见 §1.5，其中 **`server/state.rs` 三处 = 出口身份私钥 + token 台账**（`:155-158` `fs::write(key.bin)` + **静默** `set_permissions`；`:413-421` `append_file` 同款双层静默；`:131-132` state 目录 0700 同款静默） | 修（F4）。**订正①**：原文案「darwin 104 / linux 108」把字段容量当可用长度——真正可用 = 容量 −1：**darwin 103 / linux(含 OHOS) 107**（darwin 本棒实测：99–103 OK、104+ `InvalidInput "path must be shorter than SUN_LEN"`）。**订正②（高危面）**：Go **本来就是原子 0600**（`baseline .../internal/server/state.go:70` `os.WriteFile(key.bin, k, 0o600)`；`:181/:221` `os.OpenFile(..., O_CREATE\|O_APPEND, 0o600)`）⇒ `server/state.rs` 的 create-then-chmod 是**移植回退（修缺陷）**，且 chmod 错误被 `let _ =` 吞（私钥可能**永久** 0644 且无告警）；现有兜底 = state 目录 0700（同文件 `:131`，同样是「create 0755 → chmod 0700」两步）。**订正③**：`files_server.rs:977` 的 chmod 是 `let _ =` **静默**，而 Go `chmodTighten`（`serve.go:569-576`）**会告警** ⇒ 该处是「**新增**告警 + 对齐 Go」，不是「保持」 |
| 5 | **（Q-F 移交）** 隧道域三条 `c.stop()` 无界 | ✅ **成立**（**实为 4 处 + 1 处同族**） | ① `facade/tun_exec.rs:637`（`request_stop` 的 Preparing 分支）② `:989`（`Finish::drop`）③ **`:1040`**（`gen_loop` 生产函数内的「装配窗口收到 stop」分支——设计门 r16 8.1 揪出，v1 漏列）④ `session/mod.rs:1363`（`rebuild_session → old.stop()`）⑤ `:1349`（同函数 `new.stop()`）。`Client::stop()` 本体 = `wgcore/mod.rs:1502-1515`（无界 join）；有界版 `stop_within` = `:1524-1560`（Q-F 已备） | 修（F5）：**5 处**改 `stop_within(now+2s)`。`Drop for Client`（`wgcore/mod.rs:**1564-1568**`）**保持无界**（§3-D3）。**残余登记**：`rebuild_session` 的 `Client::start` 与缓存落盘仍无期限（Q-F `QF.md` §6.3-1 在册） |

### 0.3 复验新增项（审计未覆盖，同族缺口）

| # | 项 | 位置 | 处置 |
|---|---|---|---|
| N1 | **`server/bind.rs` tx 线程唤醒 `socketpair` 无 CLOEXEC**（生产；出口发送线程唯一唤醒通道） | `server/bind.rs:806-816`（`wake_r` 归 `ServerBind` Drop 收口 `:914-916`；`fds[1]` 给 `QueuedFace`） | F1（§1.1 第 4 行） |
| N2 | **relay stop 管道读端无人关**（**量级订正**：OnceLock ⇒ 一次性残留；per-proc 改造后成为必修） | `relay/mod.rs:350`（`run` 不 close `stop_fd`）↔ `relay_cli.rs:107-123`（只持写端） | F3（`RelayProc` 持两端 + 统一 shutdown） |
| N3 | **relay wake 写端未设 `O_NONBLOCK`**（合并位语义下写满即阻塞握手线程） | `relay/mod.rs:402-408`（只给读端设）；写点 `:1161` | F3 |
| N4 | **relay 收工后握手线程裸写 wake fd**（得值拷贝，非共享所有权） | `relay/mod.rs:1153-1161` + `:584-585` | F3（`WakeHandle` 锁内 take/write/close） |
| N5 | **`daemon_cli.rs` spawn 日志先建后 chmod** | `daemon_cli.rs:171-182` | F4 |
| N6 | **`nodestate.rs` config 模板 tmp 先建后 chmod**（`std::fs::write` + 静默 chmod） | `nodestate.rs:303-308` | F4 |
| N7 | **`term_cli.rs` poke 管道建立的早退路径漏 `drop_signal_pipes(pipes)`**（SIG_W 留在活 fd 上 + 两 fd 泄漏） | `term_cli.rs:1415`（装信号）→ `:1423-1428`（poke 失败 `return Err`，未收口；对照 `:1437` 的 split 失败**有**收口） | F1 顺手（该面本就要改 CLOEXEC）；`drop_signal_pipes` 定义在 `:1334` |
| N8 | **`server/state.rs` 私钥/token 台账 create-then-chmod**（设计门 4.1 高危；审计未收录） | `server/state.rs:155-159`、`:413-421`、`:131-132` | F4（**最高优先**） |
| N9 | **positive 先例**：`daemon_cli.rs:1350-1357` `drop_status_watch_signals` 已是「先 `store(-1)` 再 close」的正确 arm/disarm | —— | F3 沿用该形态 |

### 0.4 平台事实取证（v2：改实跑探针，纠正 v1 的 grep 取证错误）

| 事实 | 值 | 取证方式（v2） |
|---|---|---|
| `sockaddr_un.sun_path` 容量 | darwin **104** / linux(含 OHOS) **108** | libc 0.2.190 源：`src/unix/bsd/mod.rs:146-151`、`src/unix/linux_like/mod.rs:211` |
| **可用路径长度** | darwin **103** / linux **107** | darwin = **本棒实测**（bind 99–103 OK、104–108 `InvalidInput`）；linux = 容量 −1（NUL），**未实机运行验证（如实标注）**；三目标**编译期** `size_of−offset_of−1` 断言全部通过（107/107/103，设计门 4.7 独立复现） |
| `SOCK_CLOEXEC` | linux-gnu ✅ / **OHOS ✅** / **darwin ❌**（`cannot find value SOCK_CLOEXEC`） | **三目标编译探针**（`/tmp/fdprobe`，`cargo check --target …`，本棒实跑；**v1 的「gnu 未导出」是错的**——定义经 `libc/src/lib.rs:235 pub use new::*` + `new/glibc/sysdeps/unix/linux/bits/socket_type.rs:17` / `new/musl/sys/socket.rs:86-90` 两路导出） |
| `SOCK_NONBLOCK` | linux/OHOS ✅ / darwin ❌ | 同上探针 |
| `pipe2` | linux/OHOS ✅（`linux_like/mod.rs:1943`；探针通过）/ darwin ❌（探针 `cannot find function pipe2`） | 同上探针 + 源 |
| `O_CLOEXEC` | 三平台**都有**（darwin `0x01000000`；linux 各 arch 值不同 ⇒ 须用 `libc::O_CLOEXEC` 字面常量名） | libc 源 + 探针 |
| `target_os` for OHOS | `target_os="linux"` + `target_env="ohos"`（`cfg(target_os="linux")` 覆盖 OHOS；OHOS 走 libc 的 musl 分支） | 本棒实跑 `rustc --print cfg --target aarch64-unknown-linux-ohos` |
| **std `Command`（exec 面）是否继承未设 CLOEXEC 的 fd** | **是**（macOS 实测：管道 fd 3/4 出现在 `/bin/sh` 子进程 `/dev/fd` 列表） | **本棒实跑探针** `/tmp/fdleak_probe.rs`（§0.2 第 1 条订正②的依据） |
| **darwin `poll(2)` 的 EOF 语义** | 管道「读端 + 写端已关」：`read` 返 0 且 `poll(POLLIN)` **恒返 `POLLIN\|POLLHUP`（revents=17）**（连测三次同值）⇒ **EOF 与可读位同置**；Linux `pipe_poll` 仅在管道非空时给 `POLLIN`（**跨平台差异，v3 的死判定不再依赖该位**） | **本棒实跑探针** `/tmp/poll_eof_probe.rs`（§2-F2 判死条件的依据） |
| portable-pty 0.9.0 | `openpty` 自设 master/slave `FD_CLOEXEC`（`unix.rs:20-63`）；`spawn_command` 的 child `pre_exec` **无条件** `close_random_fds()`（`unix.rs:276`，依赖 `/dev/fd`，失败静默） | 依赖源码 |
| Go 对齐基线 | 拦截层 `net.Dialer`（`pkg/intercept/intercept.go:150/340`）；私钥/台账 `os.WriteFile(...,0600)`/`os.OpenFile(...,0600)`（`internal/server/state.go:70/181/221`）；UDS `listen.go:33` 用 100 + `serve.go:570 chmodTighten`（bind 后 chmod、**失败告警**） | 只读 oracle 逐点核对 |

### 0.5 relay stop→start 重启失效：实测复现（本棒独立复现）

**姿势**（真二进制、临时 state、本地端口 43981、serve 显式停用；跑完已 kill + 清目录）：

```bash
ST=/tmp/qg-relay-repro
printf '[serve]\nenabled = false\n\n[relay]\nenabled = false\nlisten = "127.0.0.1:43981"\n' > $ST/config.toml
homeway-cli relay start  --state $ST    # 自动拉起统一进程（pid 56464）→ state=running
homeway-cli relay stop   --state $ST    # → stopped（此处 close stop_w，读端留存）
homeway-cli relay start  --state $ST    # → started
homeway-cli relay status --state $ST    # → enabled=true state=stopped   ← 起来即死
```

**证据（`$ST/cache/daemon-events.log`）**：

```
09:53:45.402 [homeway] relay: 按期望态装配（config relay.enabled=true，经控制面）
09:53:53.700 [homeway] relay: 期望停用（config relay.enabled=false，经控制面）——已收工
09:53:54.803 [homeway] relay: 按期望态装配（config relay.enabled=true，经控制面）
09:53:55.313 [homeway] role relay: 失败（relay run 线程退出（异常终结））——退避 500ms 后进程内重建
09:53:56.334 [homeway] role relay: 失败（relay run 线程退出（异常终结））——退避 1s 后进程内重建
09:53:57.856 [homeway] role relay: 失败（relay run 线程退出（异常终结））——退避 5s 后进程内重建
09:54:03.372 [homeway] role relay: 失败（relay run 线程退出（异常终结））——退避 30s 后进程内重建
09:54:33.889 [homeway] role relay: 失败（relay run 线程退出（异常终结））——退避 30s 后进程内重建
```

`$ST/cache/relay.log` 显示每次重建都真跑到「中继就绪」（`09:53:54.805 / 55.826 / 57.348 / 54:02.870`）——
**起来即退**，与「stop 读端处于 EOF（写端已关、读端从未关）⇒ `poll` 立返 `POLLHUP` ⇒ `PollSource::Stop` 立即收工」吻合。

**旁证（`lsof -p 56464`）**：`4 PIPE / 5 PIPE`（serve 面 `install_stop_signals` 的进程级单例，**有意保留**）+ `16 PIPE`
（**只有一端**：relay stop 管道残留读端，N2 实证）+ `10u unix`（control.sock）。
第二次 `relay stop` 返回 `already`（supervisor 已清槽）——**「写落复用 fd 号」需「槽未清 + 已关号被复用」双条件**，
本次未撞上；该项以**代码面证明**为准（`relay_cli.rs:117/141` 写前无守卫 + `close()` double-close），
**如实标注「时序依赖、未在本次实测命中」**（v1 措辞保留，v2 补「量级订正」见 §0.2 第 3 条）。

---

## 1. 全仓 fd / socket 创建点扫描表（本批核心证据）

**扫描命令**（v2 给定，供复核复现）：

```bash
grep -rnE "libc::(socket|socketpair|pipe|pipe2|open|openat|dup|dup2|dup3|accept|accept4|eventfd|memfd_create|creat)\(" --include="*.rs" crates/
# → 14 行 = 12 处生产 + 2 处测试（逐点定性见 §1.1/§1.2/§1.3）
```

**口径与四类「不创建」面核查结论**（v2 补，设计门 1.2）：
- **收养点**（`OwnedFd::from_raw_fd`）：`intercept/mod.rs:228`、`bridge_host.rs:179`、`listen.rs:100`、`udpbatch.rs:321`、
  `intercept/mod.rs:3575`（测试）——**非新建面**（本仓自建或 std 的 fd），不适用；
- **`try_clone`/`dup`**：全走 std（`F_DUPFD_CLOEXEC`）；**无 raw `dup`/`F_DUPFD`**（grep 空）；
- **accept**：全走 std `ln.accept()`（bind/relay/speedtest/files）；**无 raw `accept`/`accept4`**；
- **fd 传递**：**无 `SCM_RIGHTS`/`sendmsg`/`recvmsg`**（grep 空）、无 `fork`/`posix_spawn` 自建；
  依赖面无旁路 fd 创建者（`libc` + `portable-pty` 之外无 `socket2`/`nix`/`mio`）；
- **继承进来的 fd**：只有 capi 的 App tun fd（借用，**不得**改 flags）。
- std 面（`UdpSocket::bind`/`TcpListener::bind`/`UnixListener::bind`/`UnixStream::pair`/`OpenOptions`/`File::create`）
  **一律自带 CLOEXEC** ⇒ 不在缺口表内。

### 1.1 生产路径（缺口 ⇒ 补）：**11 处**

| # | 位置 | 形态 | 现状 | 处置 |
|---|---|---|---|---|
| 1 | `server/intercept/mod.rs:224` | `libc::socket(domain, ty, 0)`（TCP/UDP/UDS 拨号，逐流） | **无** | F1：linux/ohos `ty \| SOCK_CLOEXEC`（三目标实测可用，§0.4）/ darwin fcntl；与 `set_fd_nonblocking`（`:342-350`）合并为一次 flags 收口 |
| 2 | `udpbatch.rs:42`（`bind_dual_stack`） | `libc::socket(AF_INET6, SOCK_DGRAM, 0)` | **无** | F1（手建原因 = bind 前要设 `IPV6_V6ONLY`） |
| 3 | `udpbatch.rs:80`（`bind_v6_only`） | 同上 | **无** | F1 |
| 4 | `server/bind.rs:809`（`tx_start`） | `libc::socketpair(AF_UNIX, SOCK_DGRAM, 0, …)`（**N1 新增项**） | **无** | F1 |
| 5 | `wgcore/mod.rs:1096` | `libc::pipe`（wg 驱动 wake，两端） | **无** | F1 |
| 6 | `relay/mod.rs:402` | `libc::pipe`（中继 wake，两端） | **无**（写端未设非阻塞，N3） | F1 + F3 |
| 7 | `relay_cli.rs:377`（`new_stop_pipe`） | `libc::pipe`（relay stop，两端，**OnceLock 单例**） | **无** | F1 + F3（去单例） |
| 8 | `serve_cli.rs:505`（`install_stop_signals`） | `libc::pipe`（serve stop；**进程级单例、每进程只建一次、无 restart 面**） | **无** | F1（不改结构）；注：`wait_stop_pipe()` 的 `>= 0` 判据缺陷归 Q-H 面，不在本批 |
| 9 | `daemon_cli.rs:1313`（`install_status_watch_signals`） | `libc::pipe`（`status --watch`，每命令一次） | **无** | F1（arm/disarm 已是正确形态，N9） |
| 10 | `term_cli.rs:1316`（`install_signal_pipes`） | `libc::pipe`（attach 信号） | **无** | F1 + **N7 早退补收口** |
| 11 | `term_cli.rs:1423`（poke 管道） | `libc::pipe`（读腿 → 主循环 poke） | **无** | F1 |

### 1.2 已正确 / 不需改动：**1 处生产**

| 位置 | 形态 | 结论 |
|---|---|---|
| `facade/bridge_host.rs:108-121` | `libc::socket(AF_UNIX, SOCK_STREAM, 0)` + `F_SETFL O_NONBLOCK` + **`F_SETFD FD_CLOEXEC`** | **已正确**（审计点名的示范；F1 起改调统一 helper，保持单一实现） |
| 其余 std 面（列举点见 §1 口径） | std | 不适用（std 保证；`server/bind.rs:800-804` 的「dup fd 给发送线程」因此安全） |
| `term/pty.rs:249`（`openpty`） | portable-pty 自设 CLOEXEC | 不适用 |
| `term/pty.rs:252`（`spawn_command`） | portable-pty child 净化（`close_random_fds`） | **不作为 F1 豁免理由**（依赖 `/dev/fd`、库行为、非本仓契约） |
| App tun fd（capi 传入） | 外部所有权 | **不得改 flags**（明确排除） |

### 1.3 测试态：**2 处**（顺手补，非本批判据）

| 位置 | 形态 | 处置 |
|---|---|---|
| `relay/mod.rs:1608`（`TestRelay`） | `libc::pipe` | F1 + F3（同款 `take()` 形态，消 double-close，设计门 2.4） |
| `intercept/mod.rs:3550` | `libc::socketpair`（写侧重试测试） | 顺手可选；**不改断言** |

> 扫描结论一句话：**生产 raw libc 创建点 12 处 = 11 处缺 CLOEXEC + 1 处已正确**（另有 2 处测试点）；
> 审计点名的 intercept/udpbatch **成立**，`server/bind.rs:809` **为审计未见缺口**（N1）。
> **定性**：Go 全走 `net`/`os`（fd 天然 CLOEXEC）⇒ 本项 = 移植回退（对齐 Go），非纯加固。

### 1.4 exec 面清单（继承风险面，v2 订正：**3 处**，无 `launchctl`）

| 位置 | 子进程 | 是否净化 | 备注 |
|---|---|---|---|
| `term/pty.rs:252`（`spawn_command`） | 用户 shell（长命） | **是**（portable-pty `close_random_fds`） | 依赖 `/dev/fd`；失败静默 |
| `term/agent.rs:513` | `ps`（周期调用） | 否 | **实测证实继承**（§0.4 探针同机制） |
| `term/pty.rs:71` | `/usr/bin/dscl`（macOS） | 否 | 同上 |
| `daemon_cli.rs:192` | **统一进程自身**（长命） | 否 | CLI 侧 fd 少但长命；CLOEXEC 补齐后闭合 |

> v1 误把 `launchctl` 列为 exec 面——**全仓无 `launchctl` exec**（只有 launchd plist 探测），v2 删除（设计门 1.3）。

### 1.5 权限面真扫描（v2 建立、v3 补全口径；设计门 4.1 要求「列清单 → 真扫描」）

**扫描命令**（v3 按设计门 4.1 的口径补全四条并全部实跑）：

```bash
# ① 权限两步法（set_permissions）
grep -rn "set_permissions" --include="*.rs" crates/*/src/          # → 12 行生产（下表 A 组）
# ② 敏感文件的创建面（fs::write / File::create / OpenOptions）
grep -rn "fs::write(\|File::create(\|OpenOptions::new\|File::options()" --include="*.rs" crates/*/src/   # → 逐点看下表 B 组
# ③ 目录创建（umask 掩码同样作用于 mkdir）
grep -rn "create_dir_all\|DirBuilder" --include="*.rs" crates/*/src/
# ④ 密钥/台账/凭据面交叉扫描（fs::write|File::create|create(true) × key/token/secret/hosts/identity）
```

**A 组：`set_permissions` 12 处逐点定性**

| # | 位置 | 现状 | 定性 | 处置 |
|---|---|---|---|---|
| A1 | **`server/state.rs:155-158`**（`key.bin` 私钥） | `fs::write`（默认 0644）→ **静默** chmod 0600 | **create-then-chmod（高危）**；Go = 原子 0600（`state.go:70`） | **改**（F4：`.mode(0o600)` + fchmod 归一 + 失败告警） |
| A2 | **`server/state.rs:413-421`**（`append_file`：tokens.jsonl / revoked.jsonl） | `File::options(create,append)` 默认权限 → **双层静默**（`let _ = f.metadata().map(...)`，连 metadata 错误都吞） | 同款；Go 原子（`state.go:181/221`） | **改** |
| A3 | **`server/state.rs:131-132`**（state 目录 0700） | `create_dir_all` → **静默** chmod | 同款（目录面；**umask 掩码同样作用于 mkdir**） | **改**：`DirBuilder::mode(0o700)` + **create 后无条件 chmod 0700** + 失败告警 |
| A4 | `nodestate.rs:56`（InstanceLock 的 state 根） | create_dir_all → chmod（**有告警**） | 两步但可观测 | 顺手：`DirBuilder::mode` + 保留告警 |
| A5 | `nodestate.rs:292`（state 目录）/ `:298`（4 子目录） | create_dir_all → chmod（**有告警**） | 同上（子目录告警在 `:299`） | 顺手（同 A4） |
| A6 | `nodestate.rs:307-308`（config 模板 tmp，N6） | `fs::write` → **静默** chmod | create-then-chmod | **改** |
| A7 | `daemon/carriers/mod.rs:231-250`（`save_json_atomic`） | OpenOptions 默认 → **静默** chmod（rename 在 `:249`） | create-then-chmod（审计点名） | **改** |
| A8 | `daemon_cli.rs:171-182`（spawn 日志，N5） | OpenOptions 默认 → **静默** chmod | 同款 | **改** |
| A9 | `files_server.rs:976-977`（UDS bind→chmod） | **静默** `let _ =` chmod | UDS 不可原子（§3-D2）；**Go 会告警** | **改（新增告警 + 目录前置）** |
| A10 | `daemon/listen.rs:43`（`>= 100` 判定）/ `:65`（chmod）/ `:73`（目录 0700） | chmod 失败**有** `ChmodSock` 错误返回；**目录收紧在 bind 之后** | UDS 不可原子；顺序可改 | **改（目录前置）+ 保留** |
| A11 | `artifact.rs:711-717` | 测试面 mode 断言（`0o600`/`0o700` 白名单） | **测试** | 不改（回归面，见 §4.3） |
| A12 | `files_server.rs:1642` | 测试面 | **测试** | 不改 |

**B 组：敏感文件创建面（`fs::write`/`OpenOptions`/`File::create`）——**已正确**行也列出（防「漏列 ⇒ 下轮再抓」）**

| 位置 | 形态 | 定性 |
|---|---|---|
| `daemon/hosts.rs:517-533`（hosts.json tmp） | `#[cfg(unix)]` 分支 `.mode(0o600)`（**非 unix 分支才裸 `fs::write`**） | **已正确**（hosts-2 整改产物，注释自述「先写后 chmod 存在短窗口」） |
| `identity.rs:300-310`（master.key）/ `identity.rs:222`（目录） | `.mode(0o600)` / `.mode(0o700)` | **已正确** |
| `artifact.rs:97-108`（工件写出）/ `:375-385`（还原）/ `:197/361/424/463`（目录） | `.mode(0o600)` / `.mode(0o700)` | **已正确** |
| `logfile.rs:102-110`（日志）/ `:41`（缓存目录） | `.mode(0o600)` / `DirBuilder::mode(0o700)` | **已正确**（但见下「目录面 umask」注） |
| `unified_cli.rs:243-258`（config.toml tmp） | `#[cfg(unix)]` 分支 `.mode(0o600)`；`:258` 的裸 `fs::write` 是 `cfg(not(unix))` 分支 | **已正确** |
| `relay/rltoken.rs:87-90`（relay.key）/ `:76`（目录） | `create_new + mode(0o600)` / `DirBuilder::mode(0o700)`；`:98` 的裸 `fs::write` 在 `#[cfg(not(unix))]` | **已正确**（设计门 r17 独立复核确认） |
| `session_lock.rs:70-78`（identity 锁文件） | `DirBuilder::mode(0o700)`（目录 ✓）+ **锁文件 `OpenOptions::open` 无 mode** | **新增观察（低）**：Go 为 `os.OpenFile(lockFile, O_CREATE\|O_RDWR, 0o600)`（`identity_store_unix.go:21`）⇒ 一行 `.mode(0o600)` 对齐（文件内含 pid/动词，非凭据） |
| `wtransport/endpoint_cache.rs:201`（端点缓存 tmp） | **裸 `std::fs::write`（完全无权限收紧）** → rename | **新增观察（低-中）**：Go = `os.WriteFile(tmp, raw, 0o600)`（`endpointcache.go:290`）⇒ **移植回退**（内容 = 学到的候选端点，非密钥，但目录 0700 之外无兜底）→ F4 一并 `.mode(0o600)` |
| `server/engine.rs:384`（listen_port.txt）/ `:1429/1583`（public_endpoint.txt） | 裸 `fs::write` | **不适用**（公开信息面；Go 同形） |
| `files_server.rs:645`（共享根 part 文件）/ `files_op.rs:588`（CLI 下载本地文件）/ `main.rs:915/1029`（CLI 落地） | `File::create` | **不适用**（用户文件面；Go = `0o644`，见 `pkg/files/server.go:492` / `files_cli.go:742`） |

**目录面统一注**（设计门 r17 N6）：`DirBuilder::mode(0o700)` / `create_dir_all` **同样受 umask 掩码** ⇒「创建即 0700」≠「mode 恒 0700」。
本批对**状态目录面**（`state.rs`/`nodestate.rs`）统一为「`DirBuilder::mode` + create 后无条件 chmod 0700 + 失败告警」；
对**用户文件面**不动（Go 同形）。

---

## 2. 修复清单（v3）

> 每条给「方案 / 涉及文件 / 风险 / 测试计划 / 判据行影响」。**不写产品代码**（第 2 棒实施）。

### F1（P1｜修缺陷·对齐 Go）自建 fd 全域 CLOEXEC：唯一收口 + 11 点改造

**方案**
1. 新增 `crates/homeway-core/src/sysfd.rs`（模块名取「**平台系统事实**」义，收 fd flags 与 `sun_path` 上限——
   设计门 7.2 的命名意见；`pub` 供 CLI crate 复用），**全部 RAII 面**（设计门 7.1）：
   - `pub fn set_cloexec(fd: BorrowedFd<'_>) -> io::Result<()>`；
   - `pub fn pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)>`
     - `#[cfg(target_os = "linux")]`（含 OHOS）：`libc::pipe2(fds, libc::O_CLOEXEC)`（**原子**，三目标实测可用）；
     - 其余（darwin）：`libc::pipe` + 立即 `set_cloexec` 两端；
   - `pub fn socket_cloexec(domain: c_int, ty: c_int, proto: c_int) -> io::Result<OwnedFd>`
     - linux/ohos：`libc::socket(domain, ty | libc::SOCK_CLOEXEC, proto)`（**实测可用**，§0.4；语义名优于 `O_CLOEXEC`）；
     - darwin：`libc::socket` + 立即 `set_cloexec`；
   - `pub fn socketpair_cloexec(domain, ty, proto) -> io::Result<(OwnedFd, OwnedFd)>`（同 cfg 分派；第二个 fcntl 失败 ⇒ 两个都关）。
   - 附 `pub const SUN_PATH_MAX: usize`（F4 用）。
2. **11 处创建点**改走 helper（§1.1 逐行）。`intercept` 的 `set_fd_nonblocking` 扩为 `set_fd_flags(fd: BorrowedFd, nonblocking: bool)`
   （一次 `F_SETFL` + 一次 `F_SETFD`，任一失败即 `Err`；调用方持 `OwnedFd`，错误路径 fd 由 RAII 关）。
   `bridge_host.rs:108-121` 改调 helper（单一实现）。
3. **N7 顺手**：`term_cli.rs:1423-1436` 的 poke 建立失败早退补 `drop_signal_pipes(pipes)`。
4. **不做**：std 面、portable-pty 面、App 传入的 tun fd（§1.2）。

**涉及文件**：`crates/homeway-core/src/sysfd.rs`（新）、`lib.rs`（`mod sysfd;`）、`server/intercept/mod.rs`、`udpbatch.rs`、
`server/bind.rs`、`wgcore/mod.rs`、`relay/mod.rs`、`relay_cli.rs`、`serve_cli.rs`、`daemon_cli.rs`、`term_cli.rs`。

**风险**
- ① **darwin 仍有「创建→fcntl」窗口**（无 `SOCK_CLOEXEC`/`pipe2`，三目标探针实证）：窗口内并发 fork/exec 仍会继承。
  缓解：窗口 ≈ 数十纳秒；exec 面 3 处（§1.4）。**登记残余**（不引入进程级 fd 锁——代价/收益不成比例）。
- ② linux 原子路径**无本机运行期验证**（只有编译探针 + 交叉 check）——如实登记。
- ③ 误伤面：对**非本仓所有**的 fd 设 flags（App tun fd）——明确排除 + 评审 checklist 项；helper 签名只收 `BorrowedFd`/`OwnedFd`，
  调用点必须以「本仓创建」为前提。
- ④ RAII 化会触及 `relay`/`wgcore` 的 fd 生命周期（见 F3 的 `WakeHandle`），实现顺序建议 **F3 先结构化、F1 再补 flags**（或同批，避免两次改同一段代码）。

**测试计划**（设计门 r16 8.5 + r17 U6 采纳）
- 单测（`sysfd`）：`pipe_cloexec`/`socket_cloexec`/`socketpair_cloexec` 产出的 fd **`F_GETFD & FD_CLOEXEC != 0`**；`set_cloexec` 幂等；
  `SUN_PATH_MAX` 平台值（darwin 103 / linux 107，`cfg` 分档）。
- **端到端（真 exec，确定性）**：`Command::new(std::env::current_exe())` + 测试名过滤 + 环境变量传 fd 号；
  **先 `dup2` 到高位号（≥900）再传**（U6：低位号在子进程 test harness 里可能被复用 ⇒ EBADF 断言假失败）；
  子进程侧分支断言 `fcntl(fd, F_GETFD) == -1 && errno == EBADF`，然后 `std::process::exit(0/1)`。
  **已实测的等价机制**：本棒 `/tmp/fdleak_probe.rs` 证明「未设 CLOEXEC 的 fd 会进子进程 `/dev/fd`」（修前红可证）。
  备选（更简）：`/bin/sh -c 'ls /dev/fd'` 断言该（高位）fd 号**不出现**（依赖 `/dev/fd`，本机已实测存在）。
- 回归：`intercept`/`udpbatch`/`relay`/`wgcore` 全量（CLOEXEC 不改本进程语义 ⇒ 期望零红）。

**CLI 侧管道的 RAII 归属**（设计门 r17 U9）：`serve_cli`/`term_cli`/`daemon_cli` 三处是**既有 `i32` 字段结构体**
（`SigPipe`/`WatchSigPipe`/serve `STOP_PIPE`），本批的纪律是「**helper 建、立即 `into_raw_fd()` 交既有字段**」——
生命周期已由各自的 `drop_*` 收口（`term_cli.rs:1334`、`daemon_cli.rs:1350`）⇒ 不强行改 `OwnedFd`（避免与 Q-H 面撞车）；
**新代码**（`relay_cli::StopPipe`、`relay::WakeHandle`、`intercept`/`udpbatch`/`bind` 的创建点）一律 `OwnedFd`。

**判据行影响**：**无**（不改任何 E/C/R 行文与计数；纯 fd 标志）。

---

### F2（P1｜修缺陷 + 加固）`poll_fd` 返回 revents 派生状态：掐热自旋、补死亡上报、防误报

**方案**（设计门 r16 3.1/3.3/3.4 + r17 N1/U1/U2/U8 采纳：签名定死 + 读写判序相反 + 判死不依赖 readable + 期限落点写死）
1. 签名定死：`fn poll_fd(fd: RawFd, events: c_short, deadline: Instant) -> io::Result<Ready>`，
   `struct Ready { readable: bool, writable: bool, hup: bool, err: bool, nval: bool }`。
   - **期限语义保留在 `poll_fd`**（U1 的修补）：入口 `if Instant::now() >= deadline → Err(TimedOut)`；
     否则 `poll(..., min(500ms, deadline−now))`；`r == 0`（超时）⇒ `Ok(Ready::default())`（全 false）。
   - `EINTR` 在内部 `continue`（**删掉 v1 里不可达的 Interrupted 外抛描述**；读侧 `:1715-1717` 的
     `pe.kind() == Interrupted` 分支随之成为死代码 ⇒ **同批删除**，r17 3.3）；
   - `r < 0` 其余 ⇒ `Err`；`r > 0` 但**只有未知位**（`POLLPRI`/`POLLRDHUP` 等）⇒ `err = true`（保守异常，r16 3.4）。
2. **写侧**（`write_fd_all`，`:1633-1652`）**期限检查写死**（U1）：
   ```
   loop {
       if Instant::now() >= deadline { return Err(TimedOut) }      // ← 必须在循环顶，不能只靠 poll_fd
       match write(...) {
           EAGAIN => { let r = poll_fd(fd, POLLOUT, deadline)?;     // 到点由 poll_fd 返 TimedOut
                       if r.hup || r.err || r.nval { return Err(...) }   // HUP/ERR/NVAL-first：立即出线，不烧 5s
                       continue }                                  // writable 或超时 ⇒ 回循环顶复检
           ... }
   }
   ```
   **注释理由订正**（r17 N5）：HUP/ERR/NVAL-first 的理由 = **立即出线、不烧 5s 预算**；
   **SIGPIPE 风险不在本路径**（`write_fd_all` 唯一调用点是 `wgcore:575` 的 tun 字符设备——
   SIGPIPE 只在写「无读者的 pipe/socket」时产生；`bind.rs:806` 的自述场景是 socketpair，不是这里）。
3. **读侧**（`tun_read_loop`）——**判死只看 `hup || err || nval`，不看 readable**（r17 N1，本机实测依据见 §0.4）：
   - EAGAIN 分支：`ready.readable` → continue（**可读优先**，不丢最后一包）；`ready.hup || ready.err || ready.nval`
     → **不立即判死**，落到下一轮 `read`（HUP 给出 `n==0`、ERR 给出 errno ⇒ 由既有两条路径定性）；超时 ⇒ continue。
   - `n==0` 分支（`:1739-1750`）：poll → **`hup || err || nval` ⇒ 判死**（记行 + `Cmd::TunFdDead` + return；
     **不再要求「无 readable」**——darwin 上 EOF 恒带 `POLLIN`，旧条件永不成立 ⇒ 会热自旋）；其余 ⇒ continue。
     判死前加**确认拍**（U2）：`std::thread::sleep(50ms)` **后再 poll 一次**，仍 `hup|err|nval` 才判死
     （**显式睡眠**，不是 v2 的「再 poll 50ms」——HUP 下 poll 立即返回，那样等于没确认）。
   - **极端的健康形态不误判**（U8）：`read` 返 0 且 `poll` 无 HUP/ERR/NVAL 的 fd 在可移植范围内构造不出（管道 EOF 必带 HUP），
     故**不写**这条断言，改为 `poll_fd` 层单测「`readable` 且无 hup ⇒ 不判死输入」。
4. **误报降险与未知面登记**（r16 3.2）：确认拍（真睡眠 50ms）+ 可读优先 + ERR 交回 read 定性；
   OHOS VPN fd 的精确内核语义本仓不可取证（capi 只收 App 传入的裸 fd；仓内史实仅「非阻塞」）⇒ **如实登记**；
   §5.2 影响面写「**更早/更准**触发 `unhealthyReason=fd` ⇒ 重建频率变化」。
5. `Cmd::TunFdDead` 处理与 `on_error` 链**不动**（`wgcore/mod.rs:875-882`、`facade/tun_exec.rs:1210-1218`）。

**涉及文件**：`crates/homeway-core/src/wgcore/mod.rs`（`poll_fd` / `tun_read_loop` / `write_fd_all` + 注释）。
**常量落点**（r17 U7）：确认拍 `const DEAD_CONFIRM_DELAY: Duration = Duration::from_millis(50);` 与
`POLL_SLICE: Duration = Duration::from_millis(500)`（现为字面量）落 `wgcore` 模块内 `pub(crate) const`，注释写明取值依据
（成因是「同一观察重复一次」而非精确时延）。

**风险**
- ① **误报**（代价 = App `FailGate` 整套重建）：三重降险（确认拍 + 可读优先 + ERR 交回 read 定性）；残余（OHOS fd 语义未知）登记。
- ② 读侧「可读优先」在 `POLLIN|POLLHUP` 同置时先读、下一轮 `n==0`+HUP/确认拍再判死 ⇒ **不丢最后一包**（r16 3.1 采纳）。
- ③ Go 差异：Go 的 EAGAIN 分支同样不看 revents ⇒ 本项**偏离 Go 的加固**（登记）。
- ④ 写侧行为变化：**不可写且无 HUP** 的形态仍走满 5s 预算（既有语义不变，只是不再烧 CPU）；有 HUP/ERR/NVAL 时**立即** Err。

**测试计划**（全可确定性构造；r16 8.2 + r17 N4/U1/U8 采纳）：
- `poll_fd_ready_masks`：空管道对端不写 ⇒ 超时 `Ok(Ready::default())`；关写端后对读端 poll ⇒ `hup = true`（**断言只看 hup**，
  darwin 上 `readable` 恒真——**不**断言 readable 假）；写满管道对写端 poll ⇒ `writable = true`；
  **`nval` 用例**：先 `dup2` 一个 fd 到**高位号（≥900）**、关它、再 poll 该号（N4：低位号会被并行测试复用 ⇒ 假红）；
- `write_all_deadline_returns_timedout`（U1 的守卫用例）：一个**永不可写且无 HUP** 的 fd（读端不读的 8KB 管道**且写端不关**）
  ⇒ `write_fd_all` 在 `WRITE_BUDGET` 内返 `TimedOut`（**修前/错误实现下会无睡眠死循环 —— 该用例正是拦它的**）；
- `write_all_hup_returns_promptly`：8KB 管道 + **关读端** ⇒ `write_fd_all` **< 1s** 返 `Err`（修前 ≥5s 期限）；
- `tun_read_loop_n0_with_hup_reports_dead`：**管道读端 + 写端已关**（read 恒 0、poll 恒 `POLLIN|POLLHUP`）⇒ 线程 ≤1s 退出
  且 `cmd_tx` 收到 `TunFdDead`（修前：热自旋不退出——**修前红**；**darwin 必须实测通过**才算判绿，r17 N1）；
- `read_side_prefers_readable_on_hup_with_data`：写端关闭但缓冲区有数据 ⇒ 读端 poll 得 `POLLIN|POLLHUP`
  ⇒ 断言「仍先把数据读出、下一轮才判死」。

**判据行影响**：无编号行；`unhealthyReason=fd` 触发集扩大 + 重建频率语义 ⇒ §5.2。

---

### F3（P1｜修缺陷）relay 停/唤醒管道：去单例、可重建、裸 fd 写收口、消泄漏、双关

**方案**（设计门 r16 2.1/2.6 + r17 U3/U4/U5/N9 采纳：API 定死 + arm/disarm 归 `RelayProc`）
1. **管道具名**：`struct StopPipe { r: OwnedFd, w: OwnedFd }`；`fn new_stop_pipe() -> io::Result<StopPipe>`（`sysfd::pipe_cloexec`，**每次新建**）。
2. **`RelayProc` 持两端**：`{ stop: Option<StopPipe>, join: Option<JoinHandle<()>>, exited: Arc<AtomicBool> }`；
   `fn shutdown(&mut self)`（`stop()` 与 `Drop` **共用**，**幂等**）：
   `STOP_FD.store(-1)`（disarm）→ **恢复 SIGINT/SIGTERM 为 `SIG_DFL`**（N9：沿用 `daemon_cli.rs:1350-1357` 先例——
   只做 store(-1) 仍留有「handler 已过 `>=0` 检查、稍后 write 到复用号」的窗口）→ `write(w,"x")`（EAGAIN/EBADF 忽略）
   → `join()` → `close(w)` → `close(r)`（`take()` 保证不可重入 ⇒ 结构上消除「写已关号 + double-close」与「读端泄漏」）。
   - **`r` 的交接**（U3）：`Relay::run(stop_fd: i32)` 签名不动（`:350`）⇒ 传 `r.as_raw_fd()` **裸值**，所有权留 `StopPipe`；
     不变式 =「**`shutdown` 先 join 再关两端**」，写进 `shutdown` 的文档注释（`BorrowedFd` 过不了 `thread::spawn` 的 `'static`，
     这是刻意的借用契约；`Relay::run` 注释同步写明「借用、不 close」）。
3. **前台等待循环改单通道**（r16 2.1 采纳）：**删掉「前台读同一根管道」的双读者竞态**（现状 `relay_cli.rs:306` 读的就是
   run 线程在 poll 的同一个 `r`）。新形态：
   - `install_relay_stop_signal()`：装 SIGINT/SIGTERM handler（handler 只做两件事：`STOP_FD.load()` 若 ≥0 则 `write`；
     `SIGNAL_HIT.store(true)`）；
   - `RelayProc::arm_signal(&self)`：`STOP_FD.store(w)`（**无 `armed` 字段**，r17 U4：`&self` 配普通 bool 编译不过 ⇒
     删掉该字段，armed 状态由 `STOP_FD >= 0` 单源表达）；调用时机 = `cmd_relay` 装配后、等待循环前
     （**未 armed 窗口**由「`exited()` 判失败 + `SIGNAL_HIT` 判信号」兜底，不会漏 Ctrl-C）；
   - 前台循环：`loop { if SIGNAL_HIT → break（正常 Ctrl-C 路径）; if proc.exited() → 记行 + proc.stop() + exit(1); sleep(200ms) }`
     → `proc.stop()`。**U5 订正**：失败分支**保留现状 `proc.stop()` + `exit(1)` 顺序**（`relay_cli.rs:318-321`
     现已如此——v2 伪码漏写 `proc.stop()`，与本条风险①「所有路径都关两端」矛盾，v3 补齐）。
     **信号 → run 的通道唯一**（写 w → run poll r 醒 → run 收工 → exited 置位）。
4. **`relay/mod.rs` wake pipe**：`wake_r: OwnedFd`（局部，随 `run` 作用域 RAII 关，替代 `:584-585` 手写 close）；
   写端 `Arc<WakeHandle>`（`struct WakeHandle { fd: Mutex<Option<OwnedFd>> }`，`wake()` = 锁内 `if let Some(fd) → write`、
   `close()` = 锁内 `take()`+close）供握手线程持**所有权克隆**（消除 `:1153-1161` 的 i32 值拷贝裸写）；
   `wake()` 前置：写端设 `O_NONBLOCK`（N3；EAGAIN 丢弃无害——合并位语义）。
5. **防御一行**（r16 2.5）：`relay/mod.rs:**469**` 的 stop 判定补 `POLLNVAL`（`POLLIN|POLLERR|POLLHUP|POLLNVAL`）。
6. **`TestRelay`（`:1596-1637`）**：改同款 `take()` 形态 + 用 `sysfd::pipe_cloexec`（r16 2.4）。

**涉及文件**：`crates/homeway-cli/src/relay_cli.rs`、`crates/homeway-core/src/relay/mod.rs`。

**风险**
- ① 关 `r` 的时序：`shutdown` 先 join 再关两端 ⇒ 「run poll 已关 fd」不可达；若 join 被跳过（无 handle）仍按已收处理并关两端（relay run 无 detach 语义）。
- ② `WakeHandle::wake()` 锁内 write：写端非阻塞 ⇒ 不阻塞在锁内。
- ③ 语义**加强**：stop 后 start 恢复可用（消费 Q-H 条目「`relay stop` 后 `relay start` 不可恢复」的根因面，见 §6 第 7 行）。
- ④ `SIGNAL_HIT` 是进程级一次性标志：前台单角色一次运行可接受；统一进程**不装**该 handler（`arm_signal` 只在 `cmd_relay` 调用）。

**测试计划**（设计门 2.3/8.4 采纳）
- **CLI 层（真修前红）**：`new_stop_pipe()` 两次调用返回**不同 fd 对**（修前：OnceLock 恒同 ⇒ 红）；
  `RelayProc::shutdown` 幂等（`stop.is_none()` 结构断言——**不用 `F_GETFD` 判 EBADF**，并行测试下 fd 号复用会假红）+ 事后再 `drop` 不 panic；
  `arm_signal` → `shutdown` 后 `STOP_FD == -1`。
- **core 层**：把「单例语义」在测试里复刻——**复用同一根管道**：建 pipe → 关写端 → 用同一读端 `Relay::run`（新线程）⇒ 断言
  run **立即退出**（复刻 §0.5 的生产机制；**修前该断言绿、修后（per-proc 实现）该场景不再被代码路径产生**——故此测试定位为
  「机制复刻 + 文档化」，真前红在 CLI 层与手工脚本）。
  **不写**「TestRelay 二次 start 存活」类断言（设计门 2.3：TestRelay 每次新建管道 ⇒ 修前也绿，是假红）。
- **`WakeHandle` 单测**：`close()` 后 `wake()` 不写不 panic；**fd 号复用对抗测**（close 后立刻新建管道占用同号，再 `wake()`，
  断言新管道可读字节数 0）。
- **手工回归**（§0.5 脚本）：修后 `relay start → stop → start → status` ⇒ `state=running` 且日志无「role relay: 失败…重建」
  （修前必现，本棒已实测）。

**判据行影响**：无编号行文变更；登记「broken 路径日志行消失」⇒ §5.1。

---

### F4（P2｜修缺陷 + 加固）权限原子化（**含高危私钥面**）+ `sun_path` 平台上限单源

**方案**
1. **普通文件（含高危）改「创建即 0600 + fchmod 归一」**（r16 4.2 采纳：`.mode()` 受 umask 掩码，病态 umask 会把 0600 掩成
   不可读；旧代码的后置 chmod 恰好归一，故**两步都留、但顺序变成「创建时即 0600」+「拿到 handle 后 fchmod 归一」**）：
   - **`server/state.rs:155-158`（key.bin 私钥，最高优先，A1）**：`OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)`
     + `f.set_permissions(Permissions::from_mode(0o600))` + 写 seed + **失败告警**（r17 N7：§5.2 已承诺该告警，方案里必须落地）；
   - **`server/state.rs:413-421`（`append_file`，A2）**：`.mode(0o600)` + handle 后 fchmod + 告警（**去掉双层静默 `let _`**）；
   - **`server/state.rs:131-132`（state 目录，A3）**：`DirBuilder::new().recursive(true).mode(0o700)` + **create 后无条件 chmod 0700**
     （r17 N6：mkdir 同样受 umask 掩码）+ **失败告警**（对齐 `nodestate.rs:292` 的告警形态）；
   - `nodestate.rs:307-308`（A6）、`daemon/carriers/mod.rs:231-250`（A7）、`daemon_cli.rs:171-182`（A8）：同款 `.mode(0o600)` + fchmod；
   - **A4/A5（`nodestate.rs:56/292/298`）**：已两步+告警 ⇒ 顺手改 `DirBuilder::mode` + 保留告警；
   - **新增同类两处（B 组，本批扫描补出）**：`wtransport/endpoint_cache.rs:201`（端点缓存 tmp **完全无权限收紧**，Go =
     `os.WriteFile(...,0o600)` ⇒ 移植回退）与 `session_lock.rs:77-83`（identity 锁文件无 mode，Go = `0o600`）⇒ 各一行 `.mode(0o600)`。
2. **UDS 面（无法原子；§3-D2 裁定保留）**：
   - `daemon/listen.rs`：**目录 0700 前置到 bind 之前**（现在在 `:73` bind 之后）；
   - `files_server.rs:964-978`（`listen_local_service`）：加「bind 前 `set_permissions(dir, 0700)`（失败告警不阻断）」，
     **并在函数文档写明契约**：`dir` 必须是本仓 state 布局持有的目录（三个生产调用点都传 `serve_dir`：`engine.rs:395/431/473`）
     ——r16 4.4 采纳；
   - 两处 `chmod 0600`：`listen.rs:65` 保留（有错误返回）；`files_server.rs:977` **改 `let _ =` 为告警**（与 Go `chmodTighten`
     同串形态：`⚠️ … chmod 0600 失败（{e}）—— 纵深加固未生效，state 目录权限仍是边界`，r16 4.3 采纳）。
3. **`sun_path` 单源（含 off-by-one 订正，r17 N2）**：
   - `sysfd::SUN_PATH_MAX = size_of::<libc::sockaddr_un>() - offset_of!(libc::sockaddr_un, sun_path) - 1`
     （darwin 103 / linux·OHOS 107；三目标编译期断言已由设计门独立复现）；
   - **判据统一为 `len > SUN_PATH_MAX`**（拒 104+/108+，**放行 103/107**）——v2 写的 `>= SUN_PATH_MAX` 会把合法的 103 拒掉，
     并与本设计自己的「100 < len ≤ 103 从拒变接受」放宽口径、以及深路径正例**自相矛盾**；文案同步为「> {SUN_PATH_MAX}（sun_path 上限，平台值）」；
   - **五处使用点**：三处写死 100（`daemon/listen.rs:21`（文案）+ `:43`（判定）、`files_server.rs:964-969`、`facade/bridge_host.rs:187`
     + 用点 `:491/:516`）改走 `SUN_PATH_MAX`；两处已平台正确（`facade/bridge_host.rs:100`、`server/intercept/mod.rs:308`
     现为 `>= addr.sun_path.len()`）= **与 `len > SUN_PATH_MAX` 同义** ⇒ **只注明同源，不字面改调**（照字面改会因 off-by-one 静默收紧）；
   - **量法统一**（r16 4.4 次生项）：`files_server.rs:967` 用 `display()`（lossy）而 `listen.rs:42` 用 `as_os_str().len()` ⇒
     统一为 `as_os_str().len()`（字节口径，与判据一致）；
   - **去掉 v2 的编译期白名单断言**（r16 4.6：会硬失败其它 unix 目标）⇒ 改为**单元测试**断言平台值，并在 `SUN_PATH_MAX`
     文档注「新平台若 `sun_path` 布局不同须显式核对该常量」；不复用无意义的 `pub(crate) use` 再导出。
4. 不改：state 目录权限语义、socket 属主语义（Go 同源）、用户文件面（`files`/CLI 落地，Go = `0o644` 同形）。

**涉及文件**：`sysfd.rs`（SUN_PATH_MAX）、`server/state.rs`（**新纳入**）、`nodestate.rs`、`daemon/carriers/mod.rs`、`daemon/listen.rs`、
`files_server.rs`、`facade/bridge_host.rs`、`daemon_cli.rs`、`server/intercept/mod.rs`（仅常量收口）。

**风险**
- ① **行为放宽（偏离 Go 的 100）**：100 < len ≤ 103/107 的路径从「拒」变「接受」——本批**唯一**宽松变更；已核**仓内无脚本/文档**
  依赖这些错误串（`grep -rn "≥ 100\|路径超长" crates/ docs/ tools/ fixtures/` 只中代码与本文档），且 tier 侧
  `transport.cpp:268` 本来就用 `sizeof(addr.sun_path)`（= 103）⇒ 放宽后与 tier **一致**（设计门 4.7 独立核查）。
- ② `SUN_PATH_MAX` 表达式随平台自动跟随；若无 `sun_path`（非 unix 目标）无法编译——本仓仅 unix（darwin/linux·OHOS）。
- ③ 权限：`.mode()` + fchmod 双步后 mode 恒 0600（与 umask 无关）⇒ 测试可确定断言。
- ④ `state.rs` 属**出口身份面**：改动只碰创建路径（不碰读写语义/文件格式）⇒ 既有 key 文件不受影响（已存在则直接用）。

**测试计划**（r16 8.3/8.6 + r17 N6/U-4.1 采纳）
- `SUN_PATH_MAX` 平台值单测（`cfg` 分档）；
- 深路径正例：**按「完整 socket 路径」长度**构造——`listen_control` 的完整路径 = `state_dir + "/control.sock"`（**后缀 13 B**，含分隔符）
  ⇒ darwin 断言 `dir.len() + 13 == 103` 可 bind、`+13 == 104` 必 `PathTooLong`（v2 写 +12 差 1，r17 8.6 订正）；
  linux 分档 107/108（**linux 面未实机跑，如实标注**）；其余服务后缀不同（files +11 / term +10 / speedtest +15 / 桥再 +7）⇒ 测试按各自拼接计算；
- `save_json_atomic` / `state.rs` 的 key.bin / `append_file` 台账 / `endpoint_cache`：断言产物 `mode & 0o777 == 0o600`
  （`.mode`+fchmod 后与 umask 无关 ⇒ 确定断言）；**须用全新的 state 目录**（既有 key.bin 走早退路径、不归一）；
- **窗口面不可断言 ⇒ 如实降级**（r17 三）：修前在正常 umask 下最终 mode 也是 0600（那个静默 chmod 会执行），
  所以上面的 mode 断言是**回归守卫**，不是「修前红」；「0644 窗口」与「chmod 失败」在单测里不可确定观测
  （除非动进程级 umask——并行下有竞态，不做）⇒ §4.2 表的 F4 证伪面按此降级；
  **tmp 中间态**：`save_json_atomic` 成功路径上 tmp 已被 rename（`carriers/mod.rs:249`）⇒ 直测 tmp 不可行；
  改为「`path` 指向一个**已存在的非空目录** ⇒ rename 失败、tmp 留存」构造（接受 `Err` 后断言 tmp 的 mode），或只做产物断言 + 注明；
- `State::open` 的目录面（A3）：在 **0755 临时目录**上调 ⇒ 返回后 0700（r17 三：这是 4.1 点名的第三处，v2 漏了断言）；
- `listen_control`：在 **0755 临时目录**上调 ⇒ 返回后目录 == 0700 且 socket == 0600；既有
  `listen_binds_and_second_listen_refuses`（0600 断言）保留；
- `files_server::listen_local_service`：0600 + 告警路径可测（用只读目录构造 chmod 失败 ⇒ 断言告警行，若不可构造则登记）。

**判据行影响**：**权限面 = 修缺陷（对齐 Go 的原子 0600）⇒ 不登记**（v1 误写「权限创建即 0600」要登记，v2 删除——设计门 5.2）；
**新增 additive 观测**：`files` UDS chmod 失败告警（Go 有、本仓无）⇒ §5.2；`sun_path` 放宽 + 文案 ⇒ §5.1。

---

### F5（Q-F 移交｜修缺陷）五处无界 `c.stop()` 落回预算

**方案**
- 新增 `pub(crate) const CLIENT_CLOSE_BUDGET: Duration = Duration::from_secs(2);` —— **落点 `wgcore`**
  （与 `stop_within` 同模块导出，`session`/`facade` 两处调用点均 `use` 它；r17 U7：跨模块常量必须有单一行头），
  注释写明取值依据 = 「与 Q-F 的 `EXIT_RPC_BUDGET` 同值（收工链的既有量级）」。
- **5 处** `Client::stop()` → `stop_within(Instant::now() + CLIENT_CLOSE_BUDGET)` + 记行（沿用 Q-F 段⑤文案形态）：
  `facade/tun_exec.rs:637`（`request_stop`）、`:989`（`Finish::drop`）、**`:1040`**（`gen_loop` 装配窗口分支，设计门 8.1 新增）、
  `session/mod.rs:1349`、`:1363`（`rebuild_session`）。
  - 注：`tun_exec.rs:1040` 在**世代线程内联**执行（非调用方线程）⇒ 预算语义与 `session/mod.rs:1349` 一致（均为「本线程不无限等」）。
- `Drop for Client`（`wgcore/mod.rs:1564-1568`）**保持 `stop()` 无界**（§3-D3）：显式 5 处已先置 `stop` 位并 `take()` handle ⇒
  其后 Drop 幂等早退；兜底面保留「退出前真关 wake fd」。

**涉及文件**：`facade/tun_exec.rs`、`session/mod.rs`。

**风险**
- ① detach 后引擎线程可能存活到自行退出（持 UDP fd/缓冲）——既有残余（`QF.md` §6.3-2），本批只扩适用面；
  `hw-engine-reap` 起不来时 fd 泄漏一枚（同款残余）。
- ② `Finish::drop` 内 spawn 收割线程（`stop_within` detach 分支）：Drop 里 spawn 允许（Q-F 段⑤先例），失败则记行 + 保 fd 打开。
- ③ 无回退风险：期限内返回时与 `stop()` 逐字同效。

**测试计划**（设计门 8.7 采纳「维持登记」）
- 既有 `wgcore::stop_within_detaches_and_reaper_closes_wake_fd` / `session::stop_within_normal_path_and_reentrant` 覆盖机制面；
- 调用点**覆盖有限**（`Client` 非 trait、无桩缝）：以代码面 + 预算表核对 + 手工观察为准 ⇒ **如实登记**（不假造断言）；
- 回归：`session`/`facade` 全量（含收工五段既有用例）。

**判据行影响**：无编号行；**隧道域世代收尾等待上界**从无界 → ≤2s/处 ⇒ §5.2（**注意**：`tun_stop` 的 4s 串行上界（Q-F §7-5）
本批**只减不消**，仍可能 > `STOP_WAIT=3s`——如实写进影响面）。

---

## 3. 「二选一」类决策的取证与裁定（v2 建立，v3 不变）

**D1：CLOEXEC 实现形态——平台原子 flag vs 统一 fcntl**
- 事实（v2 探针订正）：`SOCK_CLOEXEC`/`SOCK_NONBLOCK`/`pipe2` **linux/OHOS 都有、darwin 都没有**（三目标编译探针）；
  Linux 上 `SOCK_CLOEXEC == O_CLOEXEC`（内核 `include/linux/net.h`）。
- **裁定：平台分派**（linux/ohos 原子 flag + darwin fcntl）。理由：本进程有周期 fork/exec（`ps`、PTY spawn，§1.4），
  linux 侧原子写法零成本；darwin 无替代物 ⇒ 窗口只能登记。统一 fcntl 会白白保留一个可消除的缺陷面。**残余**：darwin 窗口 + linux 路径无运行期验证。

**D2：UDS 权限「创建即 0600」是否可实现**
- 事实：`bind()` 无 mode 参数（POSIX）；umask 是进程全局且**被子进程继承**（PTY shell 会继承 ⇒ 不可接受）；Go 亦为 bind→chmod + 告警。
- **裁定：UDS 不追求原子**，改「目录先 0700 + bind→chmod 0600（保留/补告警）+ 残余登记」；**普通文件**严格「创建即 0600 + fchmod 归一」。
  理由：跨用户暴露面由目录权限关闭；同用户窗口在单用户设备威胁模型下不构成越权。**残余**：同用户窗口。

**D3：`Drop for Client` 是否也改 `stop_within`** —— **不改**（显式 5 处有界；Drop 是最后兜底，改成 detach 会把「未收工」窗口放大）。登记。

**D4：`n==0` 是否改回 Go 形态（返回 0 长度包）** —— **不改**（Go 形态的 0 长度包要交上游 wireguard-go，本仓不可控、无 oracle；
保留补偿 poll + 补 revents 即达成目标）。登记为「与 Go 不同形」。

**D5：`sun_path` 放宽是否值得偏离 Go** —— **改**（审计 P2 要求 + 拒真需求 + tier 侧本就按 103 算）；登记跨实现差异。

---

## 4. 测试与验收计划（v3）

### 4.1 门（沿用批协议）

| 门 | 命令 / 判据 |
|---|---|
| 测试 | `cargo test --workspace` 全绿（基线 Q-F 收口 `623 passed / 0 failed`） |
| 静态 | `cargo clippy --workspace --all-targets -- -D warnings` clean |
| 词表 | `zsh tools/check-vocab.sh` PASS（本批不动词表） |
| 交叉 check | OHOS/musl 交叉 check（CI 既有）——**F1 linux 分支路径必须过** |
| 手工回归 | §0.5 relay 重启脚本（修后 `state=running` 且无「失败…重建」行） |
| 判据登记 | 同批 commit 写 `docs/INTEROP-CRITERIA.md`（§5） |

### 4.2 逐条判绿 / 证伪

| 条目 | 判绿（可执行） | 证伪（修前红） |
|---|---|---|
| F1 | `sysfd` 单测 + **自重入 exec 探针**（真 exec 后子进程 fd 已关；`dup2` 高位号）+ 交叉 check | 修前同探针红（11 处无 CLOEXEC；本棒 `/tmp/fdleak_probe.rs` 已证机制） |
| F2 | `Ready` 掩码单测（`nval` 用高位号构造）/ `write_all_deadline_returns_timedout` / `write_all_hup_returns_promptly`(<1s) / `tun_read_loop` n==0+HUP 退出并报死（**darwin 实测**）/ 可读优先不丢包 | 修前：HUP 返 `Ok`、写侧 ≥5s、读循环热自旋不退出 |
| F3 | `new_stop_pipe` 两次不同 fd 对（CLI 层真红）/ `shutdown` 幂等结构断言 / `WakeHandle` 复用对抗测 / **手工脚本**（§0.5） | 修前：两次同 fd 对 + 二次 run 立即退（**本棒已实测**） |
| F4 | `SUN_PATH_MAX` 平台值 + 深路径正例（**按完整 socket 路径长度**：`dir + 13`）+ mode 0600 断言（key.bin/台账/endpoint_cache，**须全新 state 目录**）+ `State::open` 目录 0700 + 目录先 0700 | **降级为「回归守卫 + 代码面复核」**（r17 三：窗口/chmod 失败在单测不可确定观测；正常 umask 下修前最终 mode 亦为 0600）——深路径/目录面仍有真前红 |
| F5 | 代码面核对 5 处 + 既有 `stop_within` 单测 + 手工观察 | （无修前红可构造——**如实登记覆盖有限**） |

### 4.3 回归面（不得破）

- `relay` 全量（`relay_vectors` 集成轨 + `tools/local-rust-relay.sh`）；
- `intercept`/`udpbatch`/`wgcore` 全量；出口 `serve` 启动面（**`server/state.rs` 改动在出口启动路径上**：key.bin 读/写、
  token 台账 append —— 必跑 `tools/local-rust-exit.sh` 一趟）；
- daemon 控制面（`listen_control` 既有测试 + files/speedtest/term UDS 启动）；
- 模板/工件：`artifact.rs` 的工件面板含 `serve/key.bin`（测试 seed `:694`、mode 白名单断言 `:711-717`）⇒ 权限改动后工件打包不受影响（只读）。

---

## 5. 判据行影响（登记草稿，v3）

### 5.1 `INTEROP-CRITERIA.md`「判据变更记录」表（拟登记 2 行）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-G） | **`sun_path` 上限**（三处错误串：`路径超长（%d 字节 ≥ 100，sun_path 上限）` / `socket 路径超长（… ≥ 100 …）` / `桥路径超长（%d ≥ 100 字节…）`） | 判据 `>= 100`（Go 同形）→ **`> SUN_PATH_MAX`**（= 拒 104+/108+，放行 103/107；`SUN_PATH_MAX` = darwin 103 / linux·OHOS 107，编译期单源）；文案「≥ 100」→「> {SUN_PATH_MAX}（sun_path 上限，平台值）」 | F4：深 state 路径被**误拒**（审计 P2；darwin 实测 104+ 才真超限；tier `transport.cpp:268` 的阈值 `>= sizeof(addr.sun_path)` 与本条同义——允许 103） | `daemon/listen.rs:21/43`、`files_server.rs:964-969`、`facade/bridge_host.rs:187`（+ 用点 `:491/:516`）+ 两处已同义点注明同源（`bridge_host.rs:100`、`intercept/mod.rs:308`）；新增深路径正例单测；**已核无脚本/文档消费方**；**行为放宽**（Go 仍拒 100–107） |
| 2026-10-08（Q-G） | **`relay stop` 后 `relay start` 的 broken 路径行消失**（`role relay: 失败（relay run 线程退出（异常终结））——退避 … 后进程内重建`） | 修前：`relay start` 后 run 立即自退 ⇒ 该行**无限刷**（500ms/1s/5s/30s…，本棒实测）→ 修后：不出现（重启后正常驻留） | F3：`STOP_PIPE` 单例复用已关写端（§0.5 实测） | `relay_cli.rs`/`unified_cli.rs`（supervisor）；排障脚本若 grep 该行需知它「不是常态」；Q-H 条目「relay start 不可恢复」由本批消费（§6） |

### 5.2 计数输入集 / 数值语义变化（行文不变）

| 日期 | 条目 | 从 → 到（数值语义） | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-G） | **`unhealthyReason=fd` 触发集与时机** | 「读错误型（EBADF/EIO）」→ 新增 **HUP/ERR/NVAL 型**（TUN fd 失效/EOF 现在被**确认一拍后**上报）；同一会话内更早报 | F2 | `facade/tun_exec.rs:1210-1218`（`on_error → mark_unhealthy_if_current("fd")`）、App `FailGate` **重建频率**（更早/更准，也可能在极端形态下更频繁——**如实写明**） |
| 2026-10-08（Q-G） | **隧道域世代收尾等待上界** | 无界 → ≤2s/处（`request_stop`/`Finish::drop`/`gen_loop` 装配窗/`rebuild_session` 的 new·old；`Drop for Client` 兜底仍无界，§3-D3） | F5 | `tun_stop` 的 `-2 强制放锁` 频率；**注**：Q-F §7-5 的「4s 串行上界 > `STOP_WAIT=3s`」**只减不消** |
| 2026-10-08（Q-G） | **新增 additive 观测** | 无 → 有：`files` UDS `chmod 0600` 失败告警（Go 有、本仓原为静默 `let _ =`）；`state.rs` 的 key/台账/目录收紧失败告警（原静默）；F5 的 detach 记行 | F4/F5 | 各日志族读者；**非编号判据行** |

### 5.3 已知口径注记（追加草稿）

- **【Q-G】TUN fd 失效判据（F2）**：`poll_fd` 现在区分「可读/可写/HUP/ERR/NVAL」并**把 HUP/ERR/NVAL 当异常**（判死条件
  **只看 `hup||err||nval`**——不依赖「readable 为假」：darwin 上管道 EOF 恒返 `POLLIN|POLLHUP`，实测见 §0.4）；
  **超时不判死**；读侧**可读优先**（不丢最后一包）、判死前**确认一拍（真睡眠 50ms）**。Go 基线（`tunfd_unix.go`）在 EAGAIN 分支
  同样不看 revents ⇒ 本项为**偏离 Go 的加固**；可见差异仅在「fd 已死」的极端形态（Go 静默转，Rust 上报并触发重建）。
- **【Q-G】fd 继承纪律（F1）**：全仓自建 fd 一律 CLOEXEC；**App 传入的 tun fd 不改 flags**（所有权在扩展）。
  真实继承面 = 3 处 std `Command` exec（`ps` / `dscl` / daemon 自 exec；**已实测证实 std 会继承未设 CLOEXEC 的 fd**）；
  PTY shell 由 portable-pty 的 `close_random_fds()` 净化（库行为，依赖 `/dev/fd`，非本仓保证）。
- **【Q-G】权限口径（F4）**：普通文件「创建即 0600 + fchmod 归一」（Go 同为原子 0600）；UDS 因 `bind()` 无 mode 参数，
  采用「目录先 0700 + bind→chmod 0600 + 失败告警」，**同用户窗口为已知残余**。

---

## 6. 不做与移交登记（防「静默漏做」）

| # | 项 | 理由 |
|---|---|---|
| 1 | Linux 原子路径（`pipe2`/`SOCK_CLOEXEC`）的运行期验证 | 本机 darwin；以三目标编译探针 + 交叉 check 为准（**如实登记**，`QG.md` 实现批回填） |
| 2 | `Drop for Client` 改有界 | §3-D3 裁定不改（登记） |
| 3 | 3 处 std `Command` exec 的「主动净化」（自建 fd 清理） | 不在本批（CLOEXEC 补齐后继承面已闭合；std 无该 API） |
| 4 | `serve_cli`/`term_cli`/`daemon_cli` 的 stop 管道结构改造 | 单次生命周期、无 restart 面（§1.1 第 8–11 行）——只补 CLOEXEC（+ N7 早退） |
| 5 | daemon SOCKS/hosts 控制面项（Q-H）、`dnsface.rs` 64KB（Q-I 尾段）、DNS TTL/files 拷贝（Q-I 尾段） | 越界 |
| 6 | `rebuild_session` 的 `Client::start` 与缓存落盘无期限、`stop_within` detach 后引擎线程在飞 | Q-F 在册残余（`QF.md` §6.3-1/-2） |
| 7 | **（跨批消费登记）** Q-H 条目「`relay stop` 后 `relay start` 不可恢复」的**根因面由本批 F3 修掉** | `AUDIT-2026-10-07.md` 该条在 Q-H 段；Q-H 收口时须勾选该条并复核（**防 Q-H 重开同一修**） |
| 8 | `serve_cli::wait_stop_pipe` 的 `>= 0` 判据缺陷 | Q-H 面（控制面/CLI），本批只补 CLOEXEC |

---

## 7. 设计门记录（dsh 外部评审）

### 7.1 轮次与结论

| 轮次 | 目录 | exit code | 结论 |
|---|---|---|---|
| 设计门第 1 轮（v1 → v2） | `/tmp/dsh-review/r16.W8cBAM`（`prompt.txt` / `output.md` 235 行 / `stderr.log`） | **exit=0** | **v1 不放行原样实施**：1 **高**（4.1 权限面漏掉 `server/state.rs` 私钥/token 台账）+ **8 中** + 25 低 + 5 组「看过没问题」。**评审独立取证**：全仓 raw fd grep、三目标 libc 编译探针、darwin `sockaddr_un` 实测探针、portable-pty 源码、Go oracle 逐行、tier 侧字符串 grep |
| 设计门第 2 轮（v2 → v3，增量复校） | `/tmp/dsh-review/r17.lQ201f`（`prompt.txt` / `output.md` 161 行 / `stderr.log`） | **exit=0** | 上轮 34 条判「**30 真闭合 / 4 部分 / 0 否**」，无「处置表写了正文没写」的假勾销；**另提 8 条 v2 新问题**（**N1 高**：n==0 判死条件在 darwin 上不可达 ⇒ 热自旋仍在；**N2 中**：`sun_path` `>= SUN_PATH_MAX` off-by-one；**U1 高**：写侧期限收口悬空 ⇒ 无睡眠死循环）+ **9 条实现棒未定义点**（U2–U9）。**评审独立复跑**：darwin `sun_path` 99–103/104+、darwin `poll(2)` EOF 语义、libc 三目标导出面、全仓 fd/权限互补扫描、r16 原文逐条比对 |

### 7.2 第 1 轮评审原文摘要（编号为评审者原编号；保留其严重度）

- **1.1（低）** 扫描计数与表不符（评审独立扫描 = 14 行 = 12 生产 + 2 测试；v1 写「13 处生产」）。
- **1.2（低）** 扫描口径未声明收养点/`try_clone`·dup/accept/继承 fd 四类（评审独立核查结论：**均无缺口**）。
- **1.3（低）** 「std::Command 四处 exec」实为 3 处，`launchctl` 是幻影（全仓无该 exec，只有 launchd plist 探测）。
- **1.4（低）** `term_cli.rs` poke 建立失败早退**漏** `drop_signal_pipes(pipes)`（SIG_W 留活 fd + 两 fd 泄漏）。
- **1.5** 看过没问题（补 CLOEXEC 无残留继承面；`wake_r` 各有收口）。
- **2.1（中）** 前台 stop 读端归属与 API **完全没定义**（删单例后 `relay_cli.rs:306` 直接编译不过；随手写会变成两根互不相关管道 ⇒ 正是 F3 要消灭的错配）。
- **2.2（低）** N2「每次 stop/rebuild 泄漏 1 fd」与自身证据矛盾（OnceLock 只建一根 ⇒ 残留是一次性；真正缺陷是单例复用已关写端）。
- **2.3（中）** core 层「修前红」测试按描述**不会红**（`TestRelay::start` 每次新建管道 ⇒ 与生产单例缺陷无关，假绿）。
- **2.4（低）** `TestRelay` 自身 double-write/double-close 未纳入。
- **2.5（低）** `PollSource::Stop` 不认 `POLLNVAL`（若读端先关 ⇒ poll 立返 NVAL、事件不匹配 ⇒ 满核忙转）。
- **2.6（低-中）** `STOP_FD` 的 disarm 没绑定到唯一收口点（`shutdown()`）。
- **2.7** 看过没问题（`WakeHandle` 消掉裸写；EAGAIN 丢弃唤醒无害；join-first 使「poll 已关 fd」不可达；N3 属实）。
- **3.1（中）** 共用 `poll_fd` 把读/写语义绑死；读侧「HUP-first」会**丢最后一包**，写侧 HUP/ERR-first 的真实理由是 **SIGPIPE**（本仓 `SIGPIPE=SIG_DFL`）而非「防误判可写」。
- **3.2（中）** `TunFdDead` 误报代价高（拆健康世代），而「tun 无瞬态 ERR」只是断言；建议取证 fd 类型或**降险**（连续 N 拍/一次确认），并把误报写进影响面。
- **3.3（低-中）** `poll_fd` 返回形态二选一未定；且设计里的 `Err(Interrupted)` 分支**永远不可达**。
- **3.4（低）** 未识别 revents 位（`POLLPRI`/`POLLRDHUP`）缺兜底 ⇒ 写侧可能忙转。
- **3.5** 看过没问题（`n==0` 补偿 poll 站得住；写侧错误链路属实）。
- **4.1（高）** 权限扫描漏掉**出口私钥与 token 台账**（`server/state.rs:157-158/409-418/130-132`）：create-then-chmod + **静默** chmod 失败，而 Go 是原子 `0o600` ⇒ 属**修缺陷**；并指出本批权限面是「列清单」而非「扫描」。
- **4.2（中）** 去掉后置 `set_permissions` 会丢 umask 归一（病态 umask 下 0600 被掩成不可读）；建议 `.mode(0o600)` **+** handle 后 fchmod 归一。
- **4.3（中）** `files_server.rs:977` 的 chmod 失败**静默**，设计却称「保持 Go 的告警」——实际必须**新增**。
- **4.4（低-中）** `listen_local_service` 的「目录先 0700」**没有代码落点**（函数内无目录操作）⇒ 须写明契约/落点。
- **4.5（低）** `sun_path` 单源漏了 `bridge_host.rs:96-101` 与 `intercept/mod.rs:309` 两处「已平台正确」的点。
- **4.6（低）** 编译期断言 `SUN_PATH_MAX == 103 || 107` 会硬失败其它 unix 目标；`pub(crate) use` 再导出无意义。
- **4.7** 看过没问题（darwin/三目标 `sun_path` 与公式独立复现成立；OHOS 走 musl 分支、`O_CLOEXEC`/`pipe2` 可用；影响面已核无消费方，tier 本按 103 算）。
- **5.1（中）** §0.4 的 `SOCK_CLOEXEC` 取证行**是错的**（gnu/ohos 实编译通过；原因是 `pub use new::*` + `new/glibc|musl` 两路导出；用 `O_CLOEXEC` 的选择仍正确，但取证行会误导）。
- **5.2（低）** F4 登记口径与 §5.1/§5.2 表不一致（表里没有权限行；权限是修缺陷 ⇒ 应删或补理由）。
- **5.3（低）** F1 可更硬：Go 拦截层用 `net.Dialer` ⇒ Rust raw socket 缺 CLOEXEC 是**移植回退**。
- **5.4（低）** relay 重启项的**跨批移交未登记**（AUDIT 该条在 Q-H 段）。
- **5.5** 看过没问题（可见行为差异都进了草稿）。
- **6** 看过没问题（**不越界**）。
- **7.1/7.2（低）** helper 签名不统一（建议 RAII + `OwnedFd`/`BorrowedFd`）；`SUN_PATH_MAX` 放 `fdutil` 名不副实。
- **7.3** 看过没问题（无 Go 直译痕迹）。
- **8.1（中）** F5 说「五处」只列 4 处，漏 **`tun_exec.rs:1040`**（生产 `gen_loop` 内，与 `session:1349` 同族）。
- **8.2–8.8（低/低-中）** F2 伪码不完整；F4 测试三处不可执行（umask 相关断言 / clippy 不做验证 / 未定义钩子）；F3 的 EBADF 断言在并行测试下有竞态；F1 端到端可用**自重入**做；深路径测试量纲要写死（目录 vs 完整 socket 路径）；F5 修前红认可已登记；§7 仍是「待回填」。

### 7.3 第 1 轮逐条处置表

| 评审编号 | 处置 | 落点 |
|---|---|---|
| 1.1 | **认同改**：计数订正为「12 生产 = 11 缺 + 1 正确，另 2 测试」+ 给出扫描命令 | §0.2 第 1 条、§1 / §1.1（11 行）/ §1.2 / §1.3 |
| 1.2 | **认同改**：口径补四类核查结论（收养点/`try_clone`/accept/继承/无 fd 传递/无旁路依赖） | §1 口径段 |
| 1.3 | **认同改**：exec 面改「3 处」，删 `launchctl`（本棒复核：全仓无该 exec） | §0.2 订正②、§1.4、§5.3 |
| 1.4 | **认同改**：N7 纳入 F1（poke 失败早退补 `drop_signal_pipes`） | §0.3 N7、§2-F1.3 |
| 1.5 | 记（无改动） | —— |
| 2.1 | **认同改**：定死「单通道 + `SIGNAL_HIT`」形态（删双读者竞态）；`arm_signal()` 只在前台装 | §2-F3.3 |
| 2.2 | **认同改**：量级订正（OnceLock ⇒ 一次性残留；per-proc 后读端才成必修） | §0.2 第 3 条①、§0.3 N2 |
| 2.3 | **认同改**：订正 core 层测试形态（复用同一读端复刻单例；真前红移到 CLI 层 + 手工），删假红断言 | §2-F3 测试计划 |
| 2.4 | **认同改**：`TestRelay` 同款 `take()` 形态 | §2-F3.6 |
| 2.5 | **认同改**：`POLLNVAL` 并入 relay stop 判定（防御一行） | §2-F3.5 |
| 2.6 | **认同改**：`STOP_FD.store(-1)` 绑进 `shutdown()`（幂等） | §2-F3.2 |
| 2.7 | 记（无改动） | —— |
| 3.1 | **认同改**：`poll_fd` 返回 `Ready` 掩码；读侧**可读优先**、写侧 HUP/ERR/NVAL 优先（注释理由按 r17 N5 改为「立即出线」） | §2-F2.1–2.3 |
| 3.2 | **部分认同（采纳降险 + 登记）**：①「写清 OHOS VPN fd 类型依据」**不可取证**（不在本仓：capi 只收 App 传入的裸 fd，仓内史实仅「非阻塞」）⇒ 如实登记未知；②③ 采纳：**确认一拍（r17 起改真睡眠）** + 影响面写明重建频率变化 | §2-F2.4、§5.2 |
| 3.3 | **认同改**：签名定死为 `Ready`；**并删读侧死分支**（r17 3.3 补充） | §2-F2.1 |
| 3.4 | **认同改**：无已知位 ⇒ `err=true`（保守异常） | §2-F2.1 |
| 3.5 | 记（无改动） | —— |
| 4.1 | **认同改（高危必改）**：`server/state.rs` 三处纳入 F4（最高优先）+ 权限面改**真扫描表**（v3 补全四条 grep 口径，A 组 12 处 + B 组逐点） | §0.2 第 4 条订正②、§0.3 N8、§1.5、§2-F4.1 |
| 4.2 | **认同改**：`.mode(0o600)` **+** handle 后 fchmod 归一 | §2-F4.1、§2-F4 风险③ |
| 4.3 | **认同改**：`files_server` chmod 失败**新增**告警（Go 同串形态），定性改「新增观测」 | §2-F4.2、§5.2 |
| 4.4 | **认同改**：写明 `listen_local_service` 的 `dir` 契约 + bind 前收紧的落点 + 量法统一（v3 补 `as_os_str().len()`） | §2-F4.2/§2-F4.3 |
| 4.5 | **认同改**：两处已平台正确的点一并收口（v3 按 r17 N2 改为**只注明同源**，避免 off-by-one 静默收紧） | §2-F4.3 |
| 4.6 | **认同改**：去编译期白名单断言（改平台值单测 + 文档注）；去无意义再导出 | §2-F4.3 |
| 4.7 | 记（无改动）；v3 修正其括注（tier 阈值 = `>= 104` 口径） | §5.1 |
| 5.1 | **认同改**：§0.4 行改「三目标编译探针」结论；实现用平台语义名 `SOCK_CLOEXEC`（linux/ohos）/`pipe2`，darwin fcntl | §0.4、§2-F1.1、§3-D1 |
| 5.2 | **认同改**：删「权限创建即 0600 需登记」的错句；只留「files chmod 告警」+ state 面告警为 additive | §2-F4 判据行影响、§5.2 |
| 5.3 | **认同改**：F1 定性改「移植回退（对齐 Go）」 | §0.2 第 1 条、§1 结论句 |
| 5.4 | **认同改**：§6 加跨批消费登记行（Q-H 勾选） | §6 第 7 行 |
| 5.5 | 记（无改动） | —— |
| 6 | 记（无改动） | —— |
| 7.1 | **认同改**：helper 全部 RAII（`OwnedFd`/`BorrowedFd`）；CLI 既有 i32 结构体的归属按 r17 U9 写明 | §2-F1.1、§2-F1「CLI 侧管道的 RAII 归属」 |
| 7.2 | **认同改**：模块改名 `sysfd`（「平台系统事实」）+ 文档说明 | §2-F1.1、§2-F4.3 |
| 7.3 | 记（无改动） | —— |
| 8.1 | **认同改**：F5 补 `tun_exec.rs:1040` ⇒ 计数统一为 **5 处** | §0.2 第 5 条、§2-F5 |
| 8.2 | **认同改**：F2 伪码定死（v3 再补期限落点） | §2-F2 |
| 8.3 | **认同改**：F4 测试改「可确定断言」；删 clippy 条；tmp 中间态改「rename 失败构造」（v3 按 r17 三再订正：明确「回归守卫」定性） | §2-F4 测试计划、§4.2 |
| 8.4 | **认同改**：F3 改结构断言（`stop.is_none()`）+ 复用对抗测，**不用** EBADF 断言（v3 把同纪律推广到 F2 的 `nval` 用例） | §2-F3 测试计划、§2-F2 测试计划 |
| 8.5 | **认同改**：F1 端到端改**自重入 exec 探针**（v3 补 `dup2` 高位号） | §2-F1 测试计划 |
| 8.6 | **认同改**：深路径测试按**完整 socket 路径长度**写死量纲 + cfg 分档（v3 按 r17 8.6 订正 `+13`） | §2-F4 测试计划 |
| 8.7 | **认同改（维持登记）**：F5 无修前红 + 覆盖有限如实登记 | §2-F5 测试计划 |
| 8.8 | **认同改**：§7 回填（本轮） | §7 |

**第 1 轮处置统计**（r17 N3 订正）：**33 条**带问题项被处置（**认同改 32 / 部分认同 1 / 不认同 0**）+ **1 条**（8.7）判「认同改（维持登记）」；
另有 7 组「看过没问题」记档不动（1.5/2.7/3.5/4.7/5.5/6/7.3——
其中 **3.5 与 4.7 的结论在 r17 被牵连**：3.5 因 N1 部分回退、4.7 的括注订正，见 §7.5）。

### 7.4 第 1 轮过门结论（v2）

- **高危 1 条已改**（4.1 ⇒ F4 扩面 + 权限真扫描表）；**中危 8 条已改**（2.1/2.3/3.1/3.2/4.2/4.3/5.1/8.1）；
  低危 25 条：**22 条回写**、**3 条为纯「记」**（1.5/2.7/3.5/4.7/5.5/6/7.3 中除 3.5·4.7 外的项与 8.7 的「维持登记」）。
- v2 即第一轮过门稿；**第 2 轮复校**（§7.5）对其结论与正文交叉核对，判「记账诚实、无假勾销」，另提新问题 ⇒ v3。

### 7.5 第 2 轮评审原文摘要（增量复校；编号为评审者原编号）

**一、34 条逐条判定**：**30 真闭合 / 4 部分 / 0 否**。4 条「部分」：
- **3.2 部分**：影响面已写 ✓，但「确认一拍」对 HUP 是**瞬时复 poll**（poll 不睡）⇒ 降险≈0；且判死条件「且无 readable」在 darwin 上**永不满足**。
- **4.1 部分**：修法与三处代码**逐点一致**、**修法充分**；**扣分在扫描口径**——v2 只跑了 `set_permissions`，未跑
  `fs::write|File::create|OpenOptions`；评审**补跑**后判「**无新的高危遗漏**」（`rltoken.rs` 非 unix 分支、`identity.rs`/`hosts.rs`/`unified_cli.rs` 均原子），
  但要求按原口径补全并列出「已正确」行（否则代码门会再抓一次）。
- **8.3 部分**：umask 断言与 clippy 条已改 ✓；「tmp 名可预测 ⇒ 直测 `metadata(tmp)`」**仍不可执行**（成功路径上 tmp 已被 rename 掉）。
- **8.6 部分**：量纲写死但**数值差 1**——`control.sock` 后缀是 **13 B**（含分隔符），doc 的 `+12` 实际构造 104 B。

**二、v2 新引入问题 8 条**：
- **N1（高）** n==0 判死条件在 darwin 上不可达，且「可读优先」假设与实测相反：**写端关闭的管道，`read→0` 后 `poll(POLLIN)` 恒返 `POLLIN|POLLHUP`**（连测三次同值；Linux `pipe_poll` 只在非空时给 `POLLIN`）⇒ v2 的「且无 readable」永不成立 ⇒ 落回 continue ⇒ `read` 立即 0 + poll 立即返回 = **满核热自旋**（正是 F2 要掐的形态）。最小修补：判死只看 `hup||err||nval`；确认拍加真睡眠；该测试须 darwin 实跑通过才算判绿。
- **N2（中）** `>= SUN_PATH_MAX` 与「放宽口径 + 实测（103 可 bind）+ 深路径正例」自相矛盾（off-by-one）：判据必须是 `len > SUN_PATH_MAX`；照字面改 `bridge_host:100`/`intercept:308` 会**静默收紧**两处「已正确」点；tier `transport.cpp:268` 用 `>= sizeof(...)` 同 104 阈值。
- **N3（低）** §7.3/§7.4 统计算术错（33≠34；「3 条纯记」三处不实）。
- **N4（低）** F2 测试计划重犯 8.4 已纠正的错误（「关闭一个 fd 后 poll ⇒ nval」在并行下 fd 号复用假红）⇒ 先 `dup2` 到高位号。
- **N5（低）** SIGPIPE 作为写侧 HUP-first 的「主要理由」对 tun fd 不成立（`write_fd_all` 只写 tun 字符设备；`bind.rs:806` 的场景是 socketpair）。
- **N6（低）** 目录面 umask 归一没跟上（`mkdir` 同样受掩码）⇒ 目录也要「create 后无条件 chmod 0700」；§1.5 把 `DirBuilder.mode` 称「已正确（原子）」不准（无窗口 ≠ 免 umask）。
- **N7（低）** §5.2 承诺 key 收紧失败告警，但 F4.1 的 key.bin 步没写告警。
- **N8（低）** 一串行号漂移：`listen.rs` 判定在 `:43`（非 `:59-62`）、`artifact.rs` 白名单在 `:711-717`、`carriers` 在 `:231-250`、`nodestate` 子目录告警 `:299`、`Drop for Client` `:1564-1568`。
- **N9（低，附）** F3 未沿用 `daemon_cli.rs:1349-1357` 先例的 **SIG_DFL 步骤**（只 `store(-1)` 仍留 handler in-flight write 窗口）。

**三、高危 4.1 专答**：**修法足以闭合**；关键两项可断言（key.bin/台账 mode；**须用全新 state 目录**——既有文件走早退不归一）；
但**不是修前红**（正常 umask 下修前最终也是 0600）⇒ 定性应为「回归守卫 + 代码面复核」；`State::open` 目录面缺断言 ⇒ 补一条；
扫描口径**半闭合**是唯一实质扣分。

**四、实现棒未定义点 9 条**：**U1（高）** 写侧期限检查无落点（照字面实现 = 无睡眠死循环，`Client::stop()` join 挂死）
+ 缺「不可写且无 HUP ⇒ 5s 内 TimedOut」用例；**U2（中）** 确认拍构造未定（须真睡眠或 read 版）；**U3（中）** `r` 交接未定
（`i32` 裸值 + 不变式须写进文档）；**U4（低）** `armed: bool` 与 `arm_signal(&self)` 编译不过；**U5（低）** 失败分支伪码漏 `proc.stop()`；
**U6（低）** 自重入探针的 EBADF 断言有 fd 号复用假失败面；**U7（低）** 两个常量无归属；**U8（低）** 「n==0 + 健康 fd 不误判」构造不出；
**U9（低）** CLI 侧三根管道的 RAII 归属未定。

**五、「看过没问题」7 组**：1.5/2.7/5.5/6/7.3 **无回退**；**3.5 有部分回退**（「配 HUP 检测后不再热自旋」被 N1 推翻）；
**4.7 结论不回退但登记措辞有新错**（tier 阈值口径）。

**六、一句话结论**：v2 记账基本诚实、无假勾销；但改稿自身带回三类实质问题（N1 高 / N2 中 / U1 高），应与 8.3 的 tmp 构造一并
在实现棒开工前定死，其余为记账/行号级修补。

### 7.6 第 2 轮逐条处置表

| 编号 | 处置 | 落点（v3） |
|---|---|---|
| 3.2（部分） | **认同改**：确认拍改**真睡眠 `sleep(50ms)` 后复 poll**；判死条件删「且无 readable」；未知面维持登记 | §2-F2.3/§2-F2.4、§0.4、§0.2 第 2 条 |
| 4.1（部分） | **认同改**：§1.5 换成**四条 grep 口径** + A 组 12 处 + B 组逐点（含「已正确」行）；补出 `endpoint_cache`/`session_lock` 两处同类；补 `State::open` 目录断言 | §1.5、§2-F4.1、§2-F4 测试计划 |
| 8.3（部分） | **认同改**：确立「回归守卫 + 代码面复核」定性；tmp 中间态改「rename 失败构造」；§4.2 表 F4 证伪面降级 | §2-F4 测试计划、§4.2 |
| 8.6（部分） | **认同改**：量纲改 `dir + 13`（`control.sock` 12 B + 分隔符）；并列出各服务后缀差异 | §2-F4 测试计划 |
| **N1（高）** | **认同改**：判死**只看 `hup\|\|err\|\|nval`**（删「且无 readable」）；确认拍真睡眠；`tun_read_loop_n0_with_hup_reports_dead` 明确「darwin 实跑通过才算判绿」；3.5 的部分回退随之恢复 | §2-F2.3、§0.4、§4.2 |
| **N2（中）** | **认同改**：判据统一 `len > SUN_PATH_MAX`；文案改「> {SUN_PATH_MAX}」；两处已正确点**只注明同源不改字面**；tier 括注修正 | §2-F4.3、§5.1 |
| N3（低） | **认同改**：统计改「33 条处置（32 认同 / 1 部分）+ 1 条维持登记」，删「3 条纯记」错句 | §7.3 统计行、§7.4 |
| N4（低） | **认同改**：F2 的 `nval` 用例先 `dup2` 高位号（≥900） | §2-F2 测试计划 |
| N5（低） | **认同改**：注释理由改「立即出线、不烧 5s」；并写明 SIGPIPE 面不在本路径（socketpair 才在） | §2-F2.2 |
| N6（低） | **认同改**：目录面统一「`DirBuilder::mode` + create 后无条件 chmod 0700 + 告警」；§1.5 加「目录面统一注」 | §1.5、§2-F4.1 |
| N7（低） | **认同改**：key.bin 步补失败告警 | §2-F4.1 |
| N8（低） | **认同改**：行号订正（`listen.rs:43`、`artifact.rs:711-717`、`carriers:231-250`、`nodestate:299`、`Drop for Client:1564-1568`、`relay:469`、`term_cli:1334`） | §0.2/§0.3/§2/§1.5 各处 |
| N9（低） | **认同改**：`shutdown` 关 w 前恢复 SIGINT/SIGTERM 为 `SIG_DFL`（沿用 `daemon_cli.rs:1350-1357` 先例） | §2-F3.2 |
| **U1（高）** | **认同改**：`write_fd_all` 循环顶**显式期限检查**写进伪码 + 补 `write_all_deadline_returns_timedout` 用例 | §2-F2.2、§2-F2 测试计划 |
| U2（中） | **认同改**：确认拍 = 显式 `sleep` + 复 poll（同 N1） | §2-F2.3 |
| U3（中） | **认同改**：写明传 `r.as_raw_fd()` 裸值 + 不变式「先 join 再关两端」进 `shutdown` 文档注释 | §2-F3.2 |
| U4（低） | **认同改**：删 `armed` 字段（armed 状态由 `STOP_FD >= 0` 单源）+ 写明安装时机与未 armed 窗口的兜底 | §2-F3.3 |
| U5（低） | **认同改**：失败分支保留 `proc.stop()` + `exit(1)`（与现码 `relay_cli.rs:318-321` 一致） | §2-F3.3 |
| U6（低） | **认同改**：探针先 `dup2` 到高位号（≥900） | §2-F1 测试计划 |
| U7（低） | **认同改**：`CLIENT_CLOSE_BUDGET` 落 `wgcore`（`pub(crate)`）；`DEAD_CONFIRM_DELAY`/`POLL_SLICE` 命名落位 | §2-F5、§2-F2 |
| U8（低） | **认同改**：删「n==0 + 健康 fd」断言，改 `poll_fd` 层「readable 且无 hup ⇒ 不判死输入」 | §2-F2.3 |
| U9（低） | **认同改**：写明「helper 建 → `into_raw_fd()` 交既有 i32 字段（生命周期由既有 `drop_*` 收口）」；新代码一律 `OwnedFd` | §2-F1「CLI 侧管道的 RAII 归属」 |

**第 2 轮处置统计**：**8 条 v2 新问题 + 9 条未定义点 + 4 条「部分」= 21 条全部认同改、0 不认同**；
「看过没问题」7 组中 3.5（部分回退）与 4.7（措辞）已随 N1/N2 修正，其余 5 组无回退。

### 7.7 过门结论（v3）

- 第 1 轮：1 高危 + 8 中 + 25 低 ⇒ v2 全改（§7.3）。
- 第 2 轮：上轮 34 条判 **30 真闭合 / 4 部分 / 0 否**；**v2 自身新问题 8 条（含 2 条高：N1 与 U1）+ 未定义点 9 条** ⇒ **v3 全改**（§7.6）。
- **过门**：两轮全部高/中项（含 v2 自身带回的 N1/N2/U1）**已全部落在 v3 的 F1–F5 与 §1.5/§5/§6 内**；
  v3 为实现基准。实现批纪律（沿用 Q-F 先例）：**不得带旧写法进代码 / 判据变更同批 commit / 发现设计与代码矛盾
  不得静默降级（双处登记）**。
- **残余（如实登记，不视为未过门）**：darwin 创建→fcntl 窗口、linux 原子路径无本机运行期验证、OHOS VPN fd 语义未知、
  UDS 同用户权限窗口、F5 无修前红、F4 的窗口面不可单测——均已在 §2/§5.2/§6 逐条写明。
