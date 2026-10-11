# 空闲唤醒事件化 · 设计稿 v3（IDLE-WAKE，2026-10-11）——**已实施**

> **实施记录（2026-10-11）**：A/B/C 三件落地（`d367a06` / `b0eaf0e` / `8668b83`），
> 含桥 stop 即时唤醒三轮回归测试；门禁读数见文末「实施后验证」。

> **修订记录**：
> - v1 被 dsh r2 否（清点漏桥/pusher、数字失真、condvar 门四个未定义问题）。
> - v2 按 r2 五条最小修订重写、以 P9 分片路线替代 condvar 门。
> - **v3 = 按 dsh r3 意见修订**（r3 结论「方案整体可进入实施，实施前定死 F5/F6/F7、
>   订正 F1/F2 口径与 F4/F9 登记」）。r3 原文 = `/tmp/dsh-review/r3.YILzgw/output.md`。
>   核心修订：去掉管道「消费字节」语义（F5，正确性）、poll 错误面写死（F6）、复用
>   `sysfd::pipe_cloexec`（F7）、稳态数字按「连接在位 = 常驻」订正（F2：稳态 8/s 而非
>   ~6/s）、P5 后果登记扩全（F4）、预算表改「等待片」口径（F9）。

## 一、现状盘点（口径先说清）

**稳态口径（连接在位空闲）是常驻形态**——`keep_alive=10s` 维持连接，空闲隧道的常态是
「连着但无流量」；「无连接」只在断连/重连期出现。两口径分开算：

| # | 源 | 周期 | 空闲唤醒 | 位置 | 性质 |
|---|---|---|---|---|---|
| 1 | 桥 accept ×3 线程 | 100ms | **30/s** | `bridge_host.rs:536-556/:320/:593-613` | fd 轮询（等 accept 就绪 + stop） |
| 2 | demand pusher 分片 | 100ms | **10/s** | `tun_exec.rs:1621-1640`（`PUSHER_TICK=1s`） | stop 等待（业务拍 1s） |
| 3 | 岛驱动 `sleep(TICK)` | 250ms | 4/s | `homeway-quic/src/driver.rs` | 真定时器（在用档快探地板对齐） |
| 4 | 世代主线程 | 200ms | 5/s | `tun_exec.rs:1086-1091` | stop 等待 |
| 5 | 世代巡检线程 | 200ms | 5/s | `tun_exec.rs:1476-1496` | stop 等待（60s 拍间隙） |
| 6 | stats 线程 | 100ms | 10/s | `tun_exec.rs:1675-1690` | stop 等待（60s 拍间隙） |
| — | **隧道态合计** | | **64/s**（4+10+5+5+10+30） | | |
| 7 | host patrol + **第二个岛 + 第二座桥**（服务会话面：`service_exec`/daemon 建 HostSession 才在） | 见左 | **+36/s**（岛 4 + 巡检 2 + 桥 30） | `host_session.rs:440-455/:685-714`、`service_exec.rs:184` | 与隧道态常不同世，单列 |

条件项（不计入稳态）：TUN 读非阻塞回退 500ms（真机形态）；pf accept 50ms×N（配了
转发规则才有）；附着窗口 200ms（瞬态）。零唤醒面（核实无误）：TUN 读（阻塞 read）/
写（channel recv）、tokio park、桥 conn 线程（阻塞 IO）。连着空闲另有 quinn
keep_alive 10s、待机探针 60s。

## 二、方案（全套四件）

### A. 桥 accept：`poll(POLLIN, -1)` + stop 唤醒管道 —— 30/s → 0

- **管道**：复用仓内单源 `sysfd::pipe_cloexec()`（`sysfd.rs:65`，linux/OHOS 走
  `pipe2(O_CLOEXEC)`、darwin `pipe`+`fcntl`——不手写平台分派）。**创建点 = 构造期
  （`new()`）**：`start()` 会早退而 `stop()` 照样被调 ⇒ 管道必须在构造期就绪；fd 所有权
  走 `OwnedFd` + `Arc<BridgeHost>`（实施时核实 BridgeHost 的单世代性——若存在同对象
  复用 `start()` 的形态，管道与 `stopped` 须在 `start()` 重置）。
- **一次性标志面，不消费字节（F5 的正确性修订）**：`stop()` 先 `store(stopped)`
  （全文件唯一 store 点，`:726`）再写 1 字节，**此后任何线程都不读管道**——字节留存，
  level-triggered 保证任何时刻进 `poll` 的线程立即返回；醒来只查 `stopped` 即退。
  「读端消费」是被 r3 证伪的形态：A 线程在「读到旧值 false」与「进 poll」之间被抢占、
  B 消费掉唯一字节 ⇒ A 永久阻塞 + `live` 永不归零 + 此后每次 `stop()` 烧满 2s。不消费
  ⇒ 两端连 `O_NONBLOCK` 都不需要（只写 1 字节，且 `stop()` 幂等多写无害）。
- **poll 错误面（F6 写死）**：`poll < 0 && EINTR ⇒ continue`；`revents` 含
  `POLLERR/POLLHUP/POLLNVAL` ⇒ 记行 + 致命退出（**不得**空转重 poll）；listener 就绪 ⇒
  accept，`WouldBlock` ⇒ 回 poll。
- **accept 错误分流（顺带收口既有静默死面）**：现状 `Err(_) => break` 把瞬时错误
  （探测者 connect+close 的 `ECONNABORTED`、`EINTR/EMFILE/ENFILE/ENOBUFS`）与致命错误
  一锅端 ⇒ 桥静默永久死亡而 `status()` 仍报路径。改：致命（`EBADF/EINVAL`）退出记行；
  瞬时族短退避后继续。
- **stop 即时的口径限定（F8）**：管道唤醒只覆盖**稳态 accept 环**；`listen_path` bind
  退避窗（单次最长 4s）与在途 `handle_conn`（鉴权 5s + 拨号 15s）不入此列（既有窗口，
  `stop()` 的 2s `live` 等待语义不变）。
- 不动：`listen_path` 重试环本身、conn 线程、sock 文件清理次序（r3 核过无相互作用）。

### B. 分片族统一 ≤1s（P9 路线，不加新原语）—— 32/s → 5/s（含 pusher）

| 处 | 现片长 | 改为 | stop 等待片尾部 |
|---|---|---|---|
| 世代主 | 200ms | 1s | ≤1s（预算：收尾链 join 2s、`STOP_WAIT=3s`） |
| 巡检 | 200ms 固定 | `min(下一拍剩余, 1s)` | ≤1s；60s 拍点不漂（最后一睡压在剩余上）；Kick 事件即醒不变；覆盖 `try_send` 丢 Stop 的尾部（不采「改 `send`」——阻塞 JS 线程，ANR 级） |
| stats | 100ms | `min(剩余, 1s)` | ≤1s |
| host patrol | 500ms | 1s | ≤1s（预算：`join_bounded` 6s） |
| pusher | 100ms | `min(剩余, 1s)` | = 业务拍（`PUSHER_TICK=1s`），1/s 是地板 |

**到点语义**：所有分片到点一律重新装填继续等，循环只认 stop 位/事件退出——绝不因超时
推进收工（r3 核过四循环结构成立）。登记（r3 低度备注）：Kick 亦 `try_send`，满丢一次
= 迟到下一拍（60s）——既有、后果仅迟探，不改。

### C. 岛拍自适应 —— 仅断连/重连期生效（4→1/s）

select 的 sleep 臂改 `if st.live.is_some() { TICK(250ms) } else { TICK_IDLE(1s) }`：
- 连接在位（稳态 + 在用档）：保持 250ms——在用档快探地板与 `fast_gap=250ms` 对齐是
  判据面（≤3.5s 恢复），**不动**；
- 无连接：r3 逐项核实 housekeeping 全 no-op（①②③③b⑤ 各自的 live/watch 门）、命令/
  任务回执照常唤醒、赛跑/R/确认探活全走任务回执或同步链（无「等下一拍」路径）、快照
  变更点全部自带立即到期 ⇒ 1s 拍纯兜底，无功能损失。
- **如实声明（F2）**：稳态（连接在位）下 C 一次都不生效——稳态收益全部来自 A+B。

### D. 不做清单

- **condvar 门（StopGate）**：不取（r2-P2/P3/P8 随方案一并不取——无新原语 ⇒ 命名/归属
  问题消失）；尾部以 B 的 ≤1s 承担。
- **pf accept 50ms**：条件项，将来复用 A 的形态单列。
- **P5 既有洞（后果登记扩全，F4）**：`tun_prepare` 窗口的 stop 被吞 ⇒ `tun_stop` 3s 后
  `-2` 强放锁 ⇒ `probe_running` 强清 ⇒ App 立即可再 prepare ⇒ `begin_generation` 清槽
  ⇒ **被吞 stop 的那个世代的停止位此后无人再置**——其世代主/巡检/pusher/stats + 岛 +
  桥全部活到进程退出（核是进程单例、capi 无 destroy；tier 侧整套重建若走扩展重启则由
  进程终结兜底）⇒ **每发生一次 -2 重建净增一份唤醒面**（改造后 ≈8/s/次）。可选一行
  修法「强放锁前先 `signal_stop()` 一次」可收掉这条路径，但动 `-2` 语义 ⇒ **单列后续
  批，不在本批**。
- **pusher 事件化**：超范围。

## 三、stop 等待片尾部与预算对照（F9 口径：**等待片**；动作在途不入列）

| 等待者 | 等待片尾部 | 所属收工预算 | 余量 |
|---|---|---|---|
| 世代主/巡检/stats | ≤1s | 世代收尾链 join 2s（总 `STOP_WAIT=3s`） | ≥1s |
| host patrol | ≤1s | `join_bounded` 6s | ≥5s |
| 桥 accept 环 | 即时（管道） | `stop()` 2s live 等待 | ≈2s（环内） |
| pusher | ≤1s | 随世代 stop 收口 | 同世代 |
| 岛 | 即时（`Cmd::Stop` 走通道） | `stop_within` 2s | ≈2s |

**既有残余登记（动作在途，非本设计引入）**：巡检 `l3_probe` 在途（`PROBE_TIMEOUT=10s`）
与岛 RPC（+5s 预算）期间 stop 不可见 ⇒ 2s join 预算可被烧穿（`tun_stop` 仍 `-2`）；
host patrol 的 `probe_once(10s)` 对 6s 同型；桥 bind 退避（≤4s）与在途 conn（≤20s）
同列。全部为「有界动作不可打断」的既有形态。

## 四、预期效果（验收口径）

| 口径 | 现状 | 改后 |
|---|---|---|
| **隧道态 · 连接在位空闲（稳态，验收基准）** | 64/s | **8/s**（岛 4 + 世代主 1 + 巡检 1 + stats 1 + pusher 1） |
| 隧道态 · 无连接（断连/重连期） | 64/s | 5/s |
| 服务会话面在场（单列） | +36/s | +5/s（连接在位）/ +2/s（无连接） |

可验收项：①稳态唤醒 ≤8/s；②**桥 stop 即时**（accept 环内，2s live 等待不再烧满）；
③各分片 stop 等待片尾部 ≤1s。定性沿用：卫生级（0.01–0.06% CPU），真实收益 = 干净 +
收工即时/不超预算 + 去掉 fd 轮询。

## 五、工量（F10 下限条件：复用 sysfd、不消费字节、错误面一次写对）

A 桥 ~60–70 行（管道复用 + poll 循环 + 错误分流）；B 分片 ~15 行；C 岛拍 ~5 行；
核心 **~80–100 行**；测试 ~60–80 行（桥 stop 即退 / 3 线程并发 stop × 多轮 live 归零 /
分片尾部上界 / 岛拍两态 / Kick 即时）⇒ 总 **~150–180 行**。

## 六、测试计划

- 桥：stop 即退 + live 归零（3 accept 线程并发 × 多轮——钉 F5 形态）；连接建立即时
  （e2e 服务流面）；poll 错误分流（EINTR/瞬时 accept 错误续跑、致命退出记行）。
- 分片族：`stop_within` 既有用例 + 「stop 后 ≤1s 退」上界；巡检 60s 拍准 + Kick 即时。
- 岛拍：无连接态快照按命令路径立即到期（既有流计数用例）；连接在位 250ms 不回归
  （ladder e2e）。
- 门禁：`ci-local` 八步 + e2e 四件套；无判据行变更。

## 七、非目标

不动：岛 TICK 在用档语义与快探节拍（判据面）；TUN 读写线程；quinn keep_alive/待机探针；
`listen_path` bind 重试环；pf accept（单列）；P5 既有洞（含其 `-2` 路径的一行修法——
单列后续批）；pusher 事件化。

---

## 实施后验证（2026-10-11，全绿）

- `tools/ci-local.sh`（quick 档）八步：前六步全绿；第 7 步矩阵冒烟**首跑红**（F-100MB
  「全候选未在预算内完成握手」+ E10，同窗 E13 已因环境抖动降档、release 构建耗时 10m23s、
  loadavg ~5）——按 flake 纪律隔离复跑**全绿**（F-100MB sha256 双侧一致 / E10 ✓ / E13 ✓ /
  TOTAL 318s）⇒ 判高负载环境 flake，非本批回归（失败形态 = 在册的握手期限 load-sensitive 族）。
- e2e 四件套 rc=0 ×4（island / ladder / pf / wg〔wipe 后跑〕）。
- 定向面：bridge_host 11 passed（含新增 `stop_wakes_all_accept_threads_promptly` 三轮）；
  facade 119 passed；homeway-quic lib 221 passed；clippy 全程零告警。
- 验收判据对照（设计 §四）：①稳态（连接在位）唤醒 = 岛 4 + 分片族 4 = **8/s** ✓；
  ②桥 stop 即时（三轮回归测试钉住）✓；③分片 stop 等待片尾部 ≤1s（预算表在册）✓。
  唤醒频次的直接仪器读数未取（本机无 sudo 侧 sleep 跟踪；以代码路径 + 上述行为测试为证）。
