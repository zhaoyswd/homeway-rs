# Q-F 客户端核与桥（session / facade）设计文档

> **v3（2026-10-08）**：v1 过设计门第一轮（dsh `r12.kyMDLA`，**exit=0**，34 条意见 → 3 组高危 + 14 条中危 + 13 条低危）；
> v2 = 第一轮整改稿，经第二轮复校（dsh `r13.4ux4qT`，**exit=0**）判「三条高危**方案骨架全部正确**
> （H2/H3 到位、H1 三个具名洞已覆盖），但 H1 的强声明仍有两处反例（C1/C2），另有 F2 伪码不自洽（C3）、
> F7b 标志位置不上（C5）、F8e 漏第三消费面（C6）、两条登记缺口（C4/C7d）」。**v3 = 第二轮回写稿**：
> C1/C3/C5/C6 已回写设计，C2/C4/C7a–j/D3 已并入并登记（§6.6 逐条勾销）。
> **v1→v3 关键改动**：F2 改采 Go 同形的「保留失败实例 + 可替换」并定死三分支伪码；F6 扩为「五段共享预算」
> （段 4 改 `try_lock` 快跳、段 5 定 `wake_wr` 归属）；F3 的 `Deadline` 映射改 `-1` 且不计入耗尽、等待方到点跳过重建决策；
> F8e 改「分档令牌池 + 保留额度」；F7 统一为「记行 + 残余登记」且标志位置在 spawn 点；
> 两处「与 Go 同形」的登记口径改判为「本批加固（偏离 Go）」。

- **批次**：Q-F（整改批；真源 = `docs/REVIEW-ROADMAP.md`「Q-F 客户端核与桥」+ `docs/reviews/AUDIT-2026-10-07.md`「Q-F」节）。
- **本棒范围**：复验 + 设计 + 设计门（**不写产品代码**）。
- **基线**：HEAD `905928c`（Q-E 收口），工作树干净。
- **本批复验实跑**：`cargo test --workspace` → **554 passed / 1 failed**，唯一红 = `speedtest_server::tests::serve_send_end_to_end`
  —— 已在 `REVIEW-ROADMAP.md`「已知 flake 登记」表在册（Q-E 复跑观察项）；本棒**隔离复跑 3/3 全绿**（loadavg 1.83/2.02/2.25）；
  **第二轮 dsh 复校独立复跑 → 590 passed / 0 failed（该 flake 未复现）** ⇒ 按甄别口径判 **flake，不算回归**。本批设计不触 `speedtest_server.rs`。
- **不越界**：Q-G（`wgcore::poll_fd`/relay pipe/daemon 权限）、Q-I 尾段（DNS TTL/files 拷贝）不在本批；「状态快照分层缓存」已由 Q-I 前段**按设计门裁决整条不做**（`QI-design.md` §2 F8/§6），本批只登记、不重开（§7）。
- **判据行影响（按 §5 实际计数，第三版回填）**：**§5.1「判据变更记录」3 行**（服务会话巡检失败行 / 服务会话收工等待行 / portfwd 状态与 rc 的契约面变更）+ **§5.2「计数输入集·数值语义变化」6 行**（含 additive 新行与两条数值语义改判）+ **§5.3「已知口径注记」6 条**。

---

## 0. 复验（证据先行）

### 0.1 方法

1. 读派单条目 → **回源码重定位**（行号按 HEAD `905928c` 重取，不沿用审计行号）。
2. 对照两侧真源：Go 基线 **只读 oracle** = `baseline/homeway/`（`app_portfwd.go` / `app_bridge.go` / `tunmode.go` / `hostsession/{service,recover}.go`）；需求真源 = `tier:openspec/specs/`（**只读**）；消费面真源 = `tier:entry/src/main/ets/**` 与 `tier:tailcat/src/main/cpp/types/libtailcat/Index.d.ts`（**只读**）。
3. 凡「状态/文案/返回码」类条目，一律追到 **tier 侧的消费代码**（页面渲染、rc 分派、健康分类）再定性——避免「core 认为无害、App 侧是谎报」的漏判。
4. 静态无界性问题：沿调用链确认**谁 join 谁**、被 join 线程内是否有无界等待（不只看名义预算）。

### 0.2 逐条复验结果表

| # | 审计条目（摘要） | 真伪 | 现行位置（HEAD `905928c`） | 结论 |
|---|---|---|---|---|
| 1 | **P0** portfwd 假成功 + 假状态 | ✅ **成立**（且假状态面比审计列的多两处） | 热替换只存表返 0：`facade/tun_exec.rs:602-608`；状态恒 `listening`/`conns=0`：`:611-627`（`portfwd_states`）；`runner_of` 的 `pf_accepted/pf_fails` 恒 0：`:632-662`；`stats_loop` 恒打 `pf=0/0`：`:1789-1791`。消费面：`tier:entry/.../pages/PortForwardsPage.ets:225-239`（`listening`→「监听中」，另读 `conns`）+ `:484` 文案「若状态列迟迟没变成『监听中』…」 | 修（F1，诚实语义；监听器版见 §2 备选） |
| 2 | **P1** 服务会话暖机硬失败状态机分裂 | ✅ **成立**（**v2 订正后果描述**） | 打 Failed 后仍 `return Ok`：`session/mod.rs:436-447`（非 Timeout 的 `Err(e)` 分支）；`service_exec.rs:165-183` 不读快照直接 `set_state(Ready)`（`:180-182`）；该路径**无 patrol**（Session 在 `:452-457` 才 spawn patrol） | 修（F2）。**订正**：状态面**没有**谎报 ready——`ServiceExec::status()` 在会话句柄在场时返回**会话快照**（`:296-315`），而会话快照此时是 `failed` + 「出口不可达：…」（`session/mod.rs:442-445`），App 可见状态一直是 failed+真实原因。真正成立的后果：**① rc 面谎报（域态 Ready ⇒ 重复 `service_start` 恒返 0「幂等」）；② 运行槽不清 ⇒ 任务不可重建；③ 无 patrol ⇒ 无自愈（files/term 桥持续拨号失败、无补注册/巡检）** |
| 3 | **P1** `healing_dial` 预算不含恢复阶梯等待 | ✅ **成立**（量级与结构订正） | 隧道域：`tun_exec.rs:235-268`（`:257` `run.recover(Level::R2,…)` 未被 `budget` 包）；服务域：`session/mod.rs:547-569`（`:562` `recover_stale` 未被预算包）；等待面无期限：`session/recover.rs:272-280`（`cv.wait` 无 timeout） | 修（F3）。**量级订正**：阶梯自身有界（每档 pre_probe 3s+2s slack、动作 2s、verify 10s+2s slack ⇒ 最坏 ≈60s），`Round::wait` 等的是同一上界的在途轮 ⇒ 击穿幅度 = **15s 预算 → 最坏 ≈64s（≈4×）**；**另有一条结构性无期限**：服务域 `recover_stale → Session::recover → maybe_rebuild_if_exhausted → rebuild_session`（`session/mod.rs:490-533`、`:1099-1147`，含 `Client::start`/旧 client `stop`）**完全不设期限**（§7-4 登记，挂 Q-G）；**无限**挂起另有其因（§0.3 N1 的无界 RPC） |
| 4 | **P1** 桥宿主 `dial_port` 锁跨拨号全程 | ✅ **成立** | `bridge_host.rs:612-617`（`let dial = lock_host(&self.dial_port); let r = dial(port, budget); drop(dial);`）。注释理由（换轨窗口防半换）在两处生产路径都**不成立**：隧道域 `tun_exec.rs:1137-1148` 构造即注入真闭包（此后再不 `set_dial`）；服务域 `service_exec.rs:134/206` 只在 `start()` 内换一次（此前无在途拨号） | 修（F4）：`Mutex<Arc<dyn Fn…>>` + 克隆后**出锁拨号** |
| 5 | **P1** `tun_attach`/`tun_stop` 同步阻塞 JS 线程 | ⚠️ **成立但落点不在本仓** | `facade/mod.rs:338-369`（attach：投 fd 后 20ms×250 轮询 ≤5s）、`:407-431`（stop：`wait_done(STOP_WAIT=3s)`）。**Go 逐值同形**：`baseline …/tunmode.go:1185-1209`（attach：2s 投递窗 + 5s 轮询）、`:1229-1253`（tunStopWait 3s）。**异步壳在 tier**：`tier:tailcat/src/main/cpp/types/libtailcat/Index.d.ts:12/25/33` = `clientCoreTunAttach`/`clientCoreTunStop` **同步**，只有 `clientCoreTunRecoverAsync` 是 Promise；实现在 `tier:tailcat/src/main/cpp/tailcat_napi.cpp:95-150`（`napi_create_async_work` 包同步符号） | **本批不做 core 改动**（D5）：① 5s/3s 是与 Go 对齐的合同值；② 异步壳须新增 napi 导出 = **tier 触点**（四处同步清单），本仓不可达。登记为移出项 + 给 tier 的建议（§7）。**同族**：`clientCoreServiceStop` 也是同步面（tier `ServiceSession.ets:314`；core 侧 ≤2s 桥停 + ≤6s 等待）——本批以 F2/F6 收窄**实际**耗时，不改合同。**残余**：世代收尾 = 派生线程 join（≤2s 共享预算，`tun_exec.rs:1206-1220`）+ `bridge.stop()`（≤2s live 等待，`bridge_host.rs:690-702`）串行 4s > `STOP_WAIT=3s`（极端下 -2 强制放锁）——归 Q-G 登记（§7-5） |
| 6 | **P1** session 域 28 处 `expect("…锁中毒")` 与 facade `lock_unpoison` 双轨；Drop 路径 double-panic | ✅ **成立**（处数订正 + 范围订正） | 实际 **31 处**（`session/mod.rs`）+ **5 处**（`session/recover.rs` gate 锁）。Drop→stop→expect：`session/mod.rs:621-625`（`Drop`）→ `:583-618`（`stop`）内 `:608`（缓存锁）、`:611`（快照锁）两处 `expect`。**v2 订正**：facade **未**「已全走 `lock_unpoison`」——`facade/tun_exec.rs` 仍有 3 处 `expect("缓存锁中毒")`（`:850`/`:1309`/`:1498`，其中 `:1309` 在 hint 线程且该线程无 `catch_unwind`）；且 `Drop → stop() → Client::stop()`（`wgcore/mod.rs:1462-1476`）内还有 **2 处 `expect`（`:1468` join 锁 / `:1471` wake 锁）+ 无界 `h.join()`** ⇒ double-panic 链比 v1 描述的长 | 修（F6，**范围扩至 `tun_exec` 3 处 + `wgcore::Client` 面**） |
| 7 | **P1** `ServiceStop` 6s 上界可被击穿 | ✅ **成立**（比审计写的更宽） | `session/mod.rs:583-618`：patrol 有界（`:588-600`），**hint `join()` 无上界**（`:601-603`）、**save `join()` 无上界**（`:604-606`），三者**串行**；hint 在途成本 = `punch_to` 内 `client.rearm_soft()`（**无界 RPC**，`wgcore/mod.rs:1346-1349`）+ `path_probe(5s)`（`session/mod.rs:780`）⇒ 单拍 ≤7s+；save = 1s 去抖 + fsync。**v2 追加**：其后还有 `cache.save`（`:607-609`；`EndpointCache::save` 持锁做 `merge_disk()` 读盘 + `create_dir_all` + `write` + `rename`，**全无期限**，`endpoint_cache.rs:171-212`）与 `Client::stop()`（`:610`，`h.join()` 无期限）。Go 侧口径是「各阶段都有界且响应取消，6s 只是兜底」（`baseline …/hostsession/service.go:57-59`）——现状**违反该口径** | 修（F6b：五段共用一个 6s 预算；**段 4 用 `try_lock` 快跳**——设计门 C1） |
| 8 | **P2** spawn 静默失败 | ✅ **成立**（站点比审计列的多 4 处，且另有一类「spawn 失败即 panic」未在审计内） | `Builder::spawn` 静默（`.ok()`/`let _ =`）：`session/mod.rs:451-457`（patrol）、`:730-752`（hint）、`:798-819`（save）、`tun_exec.rs:1155-1186`（status-dump）、`:1236-1255`（`spawn_derived`，调用点 `:1190-1196` 只用 `.flatten()` 收尾）、`:1272-1293`（hint）、`:1335-1355`（save）、`bridge_host.rs:526-532`（accept ×3）、`:566-569`（conn）、`:636-649`（pump ×2）。**v2 新增面（N6）**：`std::thread::spawn`（失败 = **panic**，非静默）在退出关键路径上：`service_exec.rs:251`（stop 的收尾线程——失败 ⇒ `fully_stopped` 永不置位 + 槽不清 + 域停 `Stopping` ⇒ **此后 `service_start` 恒 -1**）、`session/mod.rs:1002`（服务域 patrol 的旁路探测）、`tun_exec.rs:1449/1467/1756`（隧道域 patrol/pusher 内，被 `spawn_derived` 的 `catch_unwind` 兜住 ⇒ 归因成 panic） | 修（F7） |
| 9 | **P2** `SessionWriteHalf::write` 2ms 忙等重试 | ✅ **成立**（有界性无问题，是唤醒税） | `tun_exec.rs:139-169`：`Ok(0)` ⇒ `sleep(2ms)` 重试，10s 无进展上界（`:157-162`）。停滞期 ≈500 次/s `client.write` RPC 唤醒 | 修（F8a，分级退避；**保留** 10s 上界与「空写短路」语义） |
| 10 | **P2** 状态快照每 250ms 全量重建 | ✅ 成立但**已裁决不做** | `facade/mod.rs:374-390` + `tun_status.rs:102-198`；250ms 来自 tier `STATUS_POLL_MS`（`tier …/TierVpnExtensionAbility.ets:120`） | **不重开**（Q-I 前段已评估整条不做，`QI-design.md` §2 F8/§6）；本批仅登记（§7-3） |
| 11 | **P2** `stats_loop` 死分支 | ✅ **成立** | `tun_exec.rs:1791-1795`：`if !base_done \|\| (diag_fd_secs > 0 && elapsed - last_diag >= diag_fd_secs) { base_done = true; last_diag = elapsed; }` —— 分支体只改两个账面变量，**无任何观测产出** | 修（F8b）：删死分支；`diag_fd_secs` 保留为配置面并登记「无消费（OHOS 沙箱 fd 快照受限）」 |
| 12 | **P2** 巡回归因固定「探测超时」 | ✅ **成立** | `session/mod.rs:1044`（`"巡检失败（连续 {n}）：探测超时"`——`probe_ok` 在 `:927` 由 `.is_ok()` 丢弃了错误）。**对照**：隧道域同族行用真因（`tun_exec.rs:1585-1590` 的 `probe.unwrap_err()`） | 修（F8c）：打真因（行文变更，§5.1 登记） |
| 13 | **P2** `Secret` 未 zeroize | ✅ **成立** | `token.rs:128-134`（宏 `byte_array_newtype!` 产 `#[derive(Clone, Copy, …)]`，无 Drop）；全仓 `grep -rn zeroize --include=*.rs crates/` = **0 命中**（文档注释自称「R2 接入 zeroize」= 从未落地）；`Secret` 由 `wgcore::CoreConfig`、`GenRun.secret`、`Shared.secret`、`bind::Ctx.secret` 等多处持有。**v2 追加**：`PeerId` 与 `Secret` **共用同一宏**（`token.rs:91-137`），去 `Copy` 必须拆宏（否则殃及全仓 `PeerId` 拷贝点） | 修（F8d，拆宏） |
| 14 | **P2** 域名解析超时 detach 线程无上界 | ✅ **成立** | `wtransport/domain_eps.rs:63-96`（`lookup_host`：worker 线程跑 `to_socket_addrs`，超时即 `.join()` 不到、线程自生自灭）。**放大面**：`refresh_sync` 由巡检每 60s 调用（`session/mod.rs:832`、`tun_exec.rs:1678-1680`），每次对每个域名条目各 spawn 一枚 ⇒ 空黑洞 DNS 下**每分钟可漏 N 枚卡死线程**；`refresh_async` 有单飞位（`:344-349`），同步面没有 | 修（F8e）：**分档令牌池（阻塞获取，等待计调用方预算）**——v1 的「超限快失败」经设计门 3.3 否决（会把瞬时空洞放大成持续失效），v2/v3 改形（两次改形见 §6.3/§6.6） |

### 0.3 复验新增项（审计未覆盖，同族缺口）

| # | 新增项 | 位置 | 定性 | 处置 |
|---|---|---|---|---|
| **N1** | `rearm_soft()` / `refresh_reg_result()` / `close()` / `shutdown()` 是**无界 RPC**（`rx.recv()` 无 timeout） | `wgcore/mod.rs:1346-1349`（rearm_soft）、`:1310-1314`（refresh_reg_result）、`:1292-1302`（shutdown/close）；对比 `write` 有 `WRITE_REPLY_BOUND`（`:1279`）、阶梯动作用 `*_bounded(ACTION)` | **有界性缺口**：调用点全在「**必须退出**的线程」——hint（`tun_exec.rs:1307`、`session/mod.rs:766`）、巡检（`tun_exec.rs:1525`、`session/mod.rs:932`）、DomainRefresher 回调线程（**两域**：`session/mod.rs:377`、`tun_exec.rs:848`——后者后果更隐蔽：单飞位 `inflight` 被永久占用 ⇒ 该会话**所有域名重解析静默停摆**，见 `domain_eps.rs:344-349`）、桥泵收口（`SessionWriteHalf::close_write` `tun_exec.rs:177`、`SharedConn::drop` `:93`）。引擎线程若卡住（不 drop 命令、不回复），这些线程**永不返回** ⇒ 条目 7 的 6s 上界在**结构上**不可保 | 进 F3b（最小面：**只给这些退出路径**加有界包装；**替换清单经两轮穷举完整**：`rearm_soft` = `tun_exec.rs:848/1307/1496` + `session/mod.rs:377/766/964`，`refresh_reg_result` = `tun_exec.rs:1525` + `session/mod.rs:932`；不改 NAPI 面、不改阶梯既有 `*_bounded`） |
| **N2** | 桥宿主三座桥的 **accept/conn/pump 线程 spawn 失败静默**，且 `status()` 仍上报 socket 路径 + `auth_hex` | `bridge_host.rs:526-532`（accept）、`:566-569`（conn）、`:636-649`（pump）；`listen_path` 重试耗尽 `:297-332`；`status()` `:707-729` | **同族「假状态」**（与 P0 同类）：accept 线程起不来或 bind 失败时，App 拿到非空 `bridgeFilesSock` 却连不上 | 进 F7b：**置位点在 `start()` 的 spawn 点**（`accept_loop` 根本不会跑——设计门 C5）+ bind 失败点；该桥路径在 `status()` 里**置空**（**本批加固**：Go 的 `sockJSON` 只要 token 在就返回路径——`baseline …/app_bridge.go:388-395`，故这是**偏离 Go**，登记口径见 §5.3） |
| **N3** | `clientCoreServiceStop` 也是同步阻塞面（≤2s 桥停 + ≤6s 等待） | `service_exec.rs:215-281`；tier 同步调用 `ServiceSession.ets:314` | 同条目 5 族，**落点同样在 tier**（`Index.d.ts:61` 无 Async 变体） | 登记（§7-2）；core 侧以 F2/F6 收窄实际耗时 |
| **N4** | 「假 listening」在隧道域还有两处未列面：`runner_of` 的 `pf_accepted/pf_fails` 恒 0 与 `stats_loop` 的 `pf=0/0` | `tun_exec.rs:632-662`、`:1789-1791` | 数值面（真值恰好为 0，但**语义未标注**：读日志的人会以为端口转发在跑） | 进 F1（注释 + 状态面语义登记） |
| **N5** | `facade/mod.rs:480-497` `tun_set_port_forwards` 的门（probe_running + attached）本身**正确**；缺的是「执行体没有承载面」的表达 | `facade/mod.rs:488-496` | 非缺陷（复核结论） | 随 F1 一并说明 |
| **N6** | **`std::thread::spawn` 失败 = panic**（非静默），其中 `service_exec.rs:251` 会把服务会话打进**永久 `Stopping`**（`fully_stopped` 永不置位 + 槽不清 ⇒ 此后 `service_start` 恒 -1） | `service_exec.rs:251`；`session/mod.rs:1002`（服务域巡逻的旁路探测线程，**该线程无 `catch_unwind`**）；`tun_exec.rs:1449/1467/1756` | 退出关键路径上的 panic 面（审计只列了 `Builder::spawn` 的静默面） | 进 F7 |
| **N7** | 端口转发目标文案在**第三处**另有实现且与 Go 语义有两处不等 | `homeway-cli/src/main.rs:1392-1402`（`cmd_portfwd` 的 `target_text`）：`--map L:IP:0` 打 `IP:0`（Go `pfTargetText` 为 `IP:<listen>`）；`--map L:PORT` 打「主机（同端口）」（Go 为「主机:PORT」） | 文案偏差（CLI 测试动词，非判据行；但 `pf_target_text` 的三份拷贝本身是隐患） | 进 F1（**低优先顺手项**：统一到 `pf_target_text`；不做则 §7 登记） |

### 0.4 误报 / 订正记录

1. **订正条目 2 的后果**（设计门 H2 指出，回读 `service_exec.rs:296-315` 确认）：状态面**没有**谎报 ready（`status()` 在会话在场时读会话快照 = failed+原因）；真正问题是 rc 面谎报 + 不可重建 + 无自愈。
2. **订正条目 6 的处数与范围**：31 处（session）+ 5 处（recover）+ **3 处（`tun_exec`）**；Drop 链上还有 `wgcore::Client::stop` 的 2 处 `expect` + 无界 join（v1 漏）。
3. **订正条目 3 的量级与结构**：15s 预算 → 阶梯最坏 ≈64s；**另有一处结构性无期限**（服务域 `rebuild_session`）。
4. **订正条目 5 的落点**：异步壳在 tier 的 napi 层（证据在表内），本仓不可达 ⇒ 移出 + 登记 + 给 tier 建议，不虚报完成。
5. **无整条误报剔除**：14 条派单条目全部回源码复验成立（含 🔎→实测）。P0 的定性由「假成功 + 假状态」精确为「假成功（rc=0）+ 假状态（listening/conns=0）+ 目标文案错（targetPort=0 呈现 `:0`，三处拷贝）+ 两处数值面未标注」。
6. **`facade/mod.rs:488-496` 不是缺陷**（N5）——列此以免实现棒误改热替换门。
7. **两处「与 Go 同形」改判为「本批加固（偏离 Go）」**（设计门 6.1/6.2 指出，两轮均独立回读 Go 确认）：① 桥路径置空（Go `sockJSON` 不因 bind 失败清路径）；② `healing_dial` 的阶梯/闸等待纳入调用方预算（Go `healingDial` 的 `recoverStaleSession` 不带 ctx、`recoverGate.merge` 的等待方是裸 `<-r.done`——`hostsession/service.go:481-499`、`hostsession/recover.go:294`）。
8. **v2 复校的三处文档记账订正**（设计门 C7）：见 §6.6 勾销表（含 tier 行号精修：`PortForwardsPage.ets` `statusText` = `225-239`、`PortForwardRules.ets` `pfFailText` = `207-218`、`allForwardsCovered` = `196-200`、`TierVpnExtensionAbility.ets` `-3` 文案 = `1102-1103`、`-1` 文案 = `1116`、rc 日志 = `1010`）。

---

## 1. 修复清单

> 编号 F1…F8 与 §0 表格条目对应。每条给：方案 / 涉及文件 / 风险 / 测试 / 判据行影响。
> **实现纪律**：全部改动遵守 `AGENTS.md`「地道 Rust」条（类型承担不变量、借用优先、`thiserror` 收口、`pub(crate)`）。

### F1（P0）portfwd 诚实语义

**方案（默认落地；实装监听器版见 §2 备选）**

1. **状态产出改真值**：`tun_exec.rs::portfwd_states` 不再手工拼 `listening`，改为构造 `portfwd::PfState` 的**不可用态**：
   - `state = "failed"`、`conns = 0`、`err = "手机核未提供端口转发监听（127.0.0.1:{listen} 未监听）——该映射在当前版本不可用，不影响隧道"`、`code = ""`（理由与登记见 D2）。
   - `target` 一律走现有 `portfwd::pf_target_text(rule)`（`portfwd.rs:71-80`，单测 `target_text_forms` `:203-209`）——一处消灭 `targetPort=0` 呈现 `:0` 与 `target_ip` 空时呈现 `:port`（缺「主机」措辞）两个文案缺陷。
2. **`PfState` 接线**：`portfwd.rs` 增 `PfState::unavailable(listen, target, err)`（`code: ""`），保留 `PfState::failed(…, err)`（`code = PortfwdErr::BindFailed`，供监听器版与未来真 bind 失败使用）；`snapshot()` **接线后成为** `tunStatusJSON` 元素的唯一组装点（今日生产组装点是 `tun_exec::portfwd_states`，`PfState::snapshot()` 仅单测用——设计门 C7b 订正），接线后不再有第二份拼装。
   - **顺手项（可选，低优先）**：`PfState.state`/`code` 由 `&'static str` 改 `enum PfStateKind { Listening, Failed, Unavailable }` + `as_str()`（仓规「enum 替代字符串」）；判定收益不足则登记不做。
3. **热替换不报假成功**：`TunnelExec::request_port_forwards` 仍旧存表（状态面需要），返回值 **-1**（理由与登记见 D3）。
4. **数值面语义标注**：`runner_of` 的 `pf_accepted/pf_fails` 与 `stats_loop` 的 `pf=0/0` 保持 0（**真值**：无监听器 ⇒ 无 accept/失败），补注释与 §5.2 登记。
5. **文案三份拷贝收敛（N7，低优先）**：`homeway-cli/src/main.rs:1392-1402` 的 `target_text` 与 Go `pfTargetText` 有两处不等（`IP:0` 应为 `IP:<listen>`；`L:PORT` 应为「主机:PORT」）⇒ 改为复用 `pf_target_text`（或登记不做，§7-13）。
6. **状态函数抽纯函数**：`portfwd_states(rules: &[PortForwardRule]) -> Vec<PfStateIn>`（不依赖 `GenRun`），消除「同一语义两处拼装」并让单测直喂。

**涉及文件**：`crates/homeway-core/src/facade/tun_exec.rs`、`facade/portfwd.rs`、`homeway-cli/src/main.rs`（可选）。

**风险**：低（无监听器、无 wire 变更）。**唯一契约面**：`portForwards[].state` `listening` → `failed`、`code` 保持空（语义由「成功」变「未知/不适用」）、`rc` 0 → -1；tier 页面按既有码表渲染「失败 · <err 原文>」（`pfFailText` 空/未知码分支 `PortForwardRules.ets:207-218`），**不触发**任何重连/弹窗逻辑（`PortForwardsPage.ets:225-239` 只读 state/code/err/conns）。**连带失义的 tier 既有文案**（`PortForwardsPage.ets:484` 的「点『立即重连』兜底」与 `TierVpnExtensionAbility.ets:1010` 的「连接建立后随 tunConfig/补推生效」）⇒ 并入 §7-1 的 tier 建议条目。**残余**：端口转发功能本身仍不可用（= 用户可见的真话）——功能缺口按 §2 挂账。

**测试**：
- `portfwd.rs` 单测：`unavailable_state_snapshot`（state/code/err/conns 四元组）。
- `tun_exec.rs` 单测：`portfwd_states_reports_unavailable_with_target_text`（**纯函数直喂**四形态：空 ip/port 0、空 ip/port N、ip/port 0、ip/port N）。
- 状态面集成：**经 `runner_of` + 构造 `RunnerIn`** 断言 `state=="failed"`、`err` 含「未提供端口转发监听」、`target` = 「主机（同端口）」（`targetPort=0` 形态）——**注意**：`facade` 现有 `FakeExec::runner()` 恒返 `None`（`facade/mod.rs:655-657`），`tun_status()` 里**不会**有 `portForwards`（设计门 9.2），故必须在 `runner_of`/纯函数层断言（或给 `FakeExec` 加 runner 面，二者择一并在 `QF.md` 记明）。
- 热替换：无世代 → -1；有世代 → -1 **且表已存**（状态面随之更新）。

**判据行影响**：无编号判据行变更；**契约面行为差异注记 + 计数语义登记**（§5.1/§5.2/§5.3）。

### F2（P1）服务会话暖机硬失败：不发布 Ready + 失败可见 + 可重建

**方案（v2/v3：采 Go 同形「失败保留 + 可替换」；三分支伪码经设计门 C3 定死）**

1. `ServiceExec` 的会话线程在 `Ok(s)` 分支**先读快照**，按**显式三分支**处置（顺序即优先级，`stopping` 分支语义**逐字不变**）：
   ```
   if run2.stopping.load() {
       // ① 既有分支（:169-176 逐字保留）：停会话 + 停桥 + 清槽 + 域 Idle
   } else if publish_ready(snap.state) {      // publish_ready = (state == Ready)
       *run2.session.lock(..) = Some(Arc::new(s));   // ② 就绪发布（既有行为）
       run2.domain.set_state(Ready); run2.domain.set_reason("");
   } else {
       // ③ 硬失败（Ok-but-failed）——v3 定死：
       logf("服务会话暖机硬失败（原因={snap.reason}）——不发布就绪");
       run2.bridge.stop();                                  // 桥先停（Go finish 同序）
       let s = Arc::new(s);
       s.stop();                                            // 会话内收尾（幂等；failed 终态保留）
       *run2.session.lock(..) = Some(Arc::clone(&s));       // **失败实例入槽**：status 继续走会话快照
       {  // 若收尾线程到点 detach（F6），stop() 的「等会话出现」轮询会白等 2s ⇒ 见下方测试项
          let (lock, _cv) = &*run2.fully_stopped; *lock.lock(..) = true;   // = Go isDone() 等价位
       }
       // 域态写回**加身份守卫**（照 clear_slot_if_same 范式）：仅当槽仍 ptr_eq(run2) 才写
       if slot_is_same(&run_slot, &run2) { run2.domain.set_state(Failed); run2.domain.set_reason(&snap.reason); }
       // **保留 run 槽**：service_status 继续报 failed+原因（Go m.cur 语义）
   }
   ```
   - `set_reason(&snap.reason)` **建议断言非空**（硬失败必带 reason；空则回落固定文案「出口不可达」）。
2. **可重建 = Go 同形（不是清槽）**：`ServiceExec::start` 的运行槽门从 `if guard.is_some() → -1` 改为「槽内实例**已收尾**（`fully_stopped` 置位）⇒ 允许替换（`*guard = None` 后按新 start 走），否则 -1」——对齐 Go `service.go:992-1018`（`svcStateFailed` + `isDone()` ⇒ 换新会话）。**双持钥第二道防线不受影响**：域门（`Starting/Ready→0`、`Stopping→-1`）在槽门之前，`stop` 超时保留槽 + 域 Stopping 时 `start` 仍被域门挡住（设计门 D3 复核结论）。
3. **状态面**：`service_status` 保持既有分支（会话在场 ⇒ 会话快照 = failed+原因）。**可选一致性小修**（设计门 C7d）：槽空 + 域 `Failed` 时输出 `{"state":"failed","reason":<域 reason>}`（覆盖 `Session::start` 返 `Err` 的清槽路径）——**注意** `ServiceExec::status(&self)` 今日拿不到域句柄（域在 `ClientCore.service`），需改签名为 `status(&self, domain: &ServiceDomain)`（调用点 `facade/mod.rs:552` 一处）或在 `ServiceExec` 里存 `Arc<ServiceDomain>`；**若判定超范围 ⇒ 明确不做 + §7 登记**（二者择一，不得静默）。
4. **发布判定收口为纯函数**：`fn publish_ready(state: SessState) -> bool`（`state == Ready`；`stopping` 由外层分支处理，不并入）。
5. **与 F6 的耦合**：`s.stop()` 因 F6 而**有界**（五段共享 6s 预算），失败路径的收尾不会拖死 `service_stop`；但**若段 5 detach**（`fully_stopped` 弱化），替换窗口内旧引擎线程可能仍活 ⇒ §5.3 + §7-6 登记（设计门 D3）。

**涉及文件**：`crates/homeway-core/src/facade/service_exec.rs`、`crates/homeway-core/src/session/mod.rs`（新增 `#[cfg(test)]` 构造缝）。

**风险**：中低。行为变化 = ① Ok-but-failed 时 rc 面不再谎报「已在跑」（可重建）；② `service_status` 在 failed 期内容**保持** failed+原因（不变）；③ 可选小修会让「start 返 Err 后」的 status 由 idle 变 failed+原因（更诚实，但属额外变化 ⇒ 见上，择一登记）。
**测试缝（v2/v3 定死）**：
- `#[cfg(test)] impl Session { pub(crate) fn synthetic_failed_for_test(reason: &str) -> Session }`——crate 内可见；用 `Identity::ephemeral()` + 一枚 TEST-NET 候选建**惰性真 Client**（可行性由 `wgcore/mod.rs:1792-1818` 的 `engine_probe_blackhole_times_out` 实证），随后直接置 `Shared.snapshot.state = Failed`（需复刻 `Shared` 装配段 ≈40 行；若 >100 行则退「极简替身 + 如实标注」，§4.2）。
- **`ServiceExec` 需要 `#[cfg(test)]` 会话工厂**（设计门 D6 补）：`start()` 今日**硬编码** `Session::start(...)`（`service_exec.rs:158`）——不加工厂就无法注入假会话 ⇒ 设计里明确加一个 `#[cfg(test)]` 工厂字段 + 构造器（**不改生产 API**；集成测试（`tests/`）看不到 `cfg(test)`，如需要则改 `feature = "test-seams"` 口径）。

**测试**：
- 纯函数 `publish_ready` 真值表。
- 集成（构造缝 + 工厂）：断言 `domain.state()==Failed`、`domain.reason()` 非空、运行槽**仍在**且 `fully_stopped` 置位、`service_status()` 含 `"state":"failed"` 与原因；随后 `start()` 能受理（可替换死实例）⇒ **可重建**；**并断言 `stopping` 竞态分支仍走「清槽 + 域 Idle」**（回归 C3a）。
- **空等回归**：失败实例入槽后调 `stop()`，断言不出现 2s 空等（`fully_stopped` 已置位 ⇒ 走正常路径）。
- 回归：`service_op` 门测、`bridge_dial_closure_weak_only`、既有 `service_exec` 测试不变。

**判据行影响**：新增 1 条 additive 行（暖机硬失败不发布就绪）；`service_start` rc 门语义变化（失败实例可替换）**登记**（§5.3）；不触 C 族行文。

### F3（P1）`healing_dial` 预算纳入阶梯等待（两域）

**方案（v3：`Deadline` 映射 -1、不计耗尽、等待方到点跳过重建决策）**

1. **阶梯带期限**（`session/recover.rs`）：
   - `LadderDeps` 增 `deadline: Option<Instant>`；`run_ladder` 在**每档起点、每个动作前**检查（到点 ⇒ 返回 `LadderRc::Deadline`）；`(deps.probe)(d)` 的实参按剩余夹取。**残余越界上界 = 一个动作预算（`ACTION`=2s）**——验收按「预算 + 2s/动作容差」口径。
   - **`LadderRc::Deadline` 的边界映射 = `as_rc()` -1**（**不是** -3：隧道域 gate 与 NAPI `tun_recover` **共用**，NAPI 作为等待方可能读到 `Deadline`；tier 对 -3 的文案是「本机网络栈没准备好」（`TierVpnExtensionAbility.ets:1102-1103`）、对 -1 是「阶梯走完仍对端不可达（网络侧已尽力）」（`:1116`）⇒ -1 是正确归因；D9）。
   - 日志：到点打 `RECOVER 预算耗尽（起跑=%s，原因=%s，预算 %v 已用尽）—— 放弃等待`（additive，§5.2）。
2. **闸等待带期限**（`RecoverGate`）：
   - 增 `merge_until(from, deadline: Option<Instant>, run: impl FnOnce(Level, Option<Instant>) -> LadderRc) -> (LadderRc, bool)`（**第二返回值 = 是否等到了结果**；`merge` = 旧签名的薄壳，`deadline=None` 恒 `true`）。
   - **等待方**：`cv.wait_timeout(remaining)`；到点返回 `(Deadline, false)`（**不**发布、**不**清闸）。
   - **执行方**：deadline 透给 `run` 回调，两域轮实现传进 `run_ladder`。
3. **两个 `healing_dial` 传期限 + 抽可测接缝**：
   - 隧道域 `tun_exec.rs:235-268`：`deadline = t0 + budget`；首试 `connect_deadline(dst, FIRST_TRY.min(budget))`；`recover_until(Level::R2, cause, deadline)`；尾试预算 = `budget - t0.elapsed()`（下界 1ms）。
   - 服务域 `session/mod.rs:547-569`：同形；**v3 定死（设计门 C4）**：`Session::recover_until` 在**等待到点**（第二返回值 false）时**跳过** `maybe_rebuild_if_exhausted()`（既有结构是「merge 返回即认为本轮结束再判重建」；等待方提前返回不得在在途轮未完时触发 `rebuild_session` → `old.stop()` 掉在途轮正持有的 Client）。
   - **`Deadline` 的记账（C4）：不计入 `exhausted`**（既有两个埋点：`Session::note_ladder_result` 的 `_ =>` 分支与 `session_recover` 的内联分支——两处都要把 `Deadline` 显式排除；等待方超时不构成「阶梯走完未恢复」的证据；执行方那一轮照常记账一次）。**并登记**（§5.2）。
   - **可测接缝（v2 定死）**：`healing_dial` 主体抽为私有泛型辅助 `fn dial_with_recover<F>(client: &Arc<Client>, dst: SocketAddrV4, budget: Duration, recover: F) -> io::Result<u64> where F: FnOnce(Level, Option<Instant>, &str) -> LadderRc`；生产两域各传自己的闭包，**单测传「阻塞到期限」的桩闭包** ⇒ 断言「预算内返回 Timeout」。**测试注意（设计门 C7g）**：桩预算必须 **> `FIRST_TRY`(4s)**（否则首试吃满预算、桩永不执行 ⇒ 假绿），且须**断言桩被调用过**；两域各一次 ⇒ 该单测耗时 ≥ ~10s（可接受，或把 `FIRST_TRY` 做成可注入常量）。
4. **N1 有界 RPC（F3b）**：`wgcore` 增 `rearm_soft_bounded(d)` / `refresh_reg_result_bounded(d)` / `shutdown_bounded(d)` / `close_bounded(d)`（`rx.recv_timeout`，超时 = `ConnErr::Timeout`）；替换清单（**两轮穷举完整**）：`rearm_soft` = `tun_exec.rs:848/1307/1496` + `session/mod.rs:377/766/964`；`refresh_reg_result` = `tun_exec.rs:1525` + `session/mod.rs:932`；再加 `SessionWriteHalf::close_write`（`tun_exec.rs:177`）与 `SharedConn::drop`（`:93`）的 `shutdown`/`close`。原无界方法保留（CLI/其它调用点不动）。
   - **残余登记**：`Client::read`（桥泵数据面）不纳入（§7-7）；`Client::stop` 由 F6b 的 `stop_within` 收口。

**涉及文件**：`session/recover.rs`、`session/mod.rs`、`facade/tun_exec.rs`、`wgcore/mod.rs`。

**风险**：中。期限语义只影响**带预算的拨号**路径；两域巡检与 NAPI 恢复走 `deadline=None` ⇒ 行为逐字不变（测试钉住）。`LadderRc` 是 `#[non_exhaustive]`（内部匹配安全）。
**消费面（两轮补全）**：`healing_dial_*` 全部调用方 = `session/mod.rs:537/543`（服务域内部）、`service_exec.rs:387`（服务桥，恒 15s）、`tun_exec.rs:193`（隧道桥，`dial_ms` 缺省 15s）、`files.rs:205`（调用方 budget）、`homeway-cli/src/main.rs:1419`（15s）、`daemon/mod.rs:295/309`（`budget.min(15s)`）、`:393`（30s）。**行为变化面**：以上全部由「预算只覆盖首试+尾试」变为「覆盖首试+阶梯+尾试」⇒ daemon（Q-H 面）拨号最长等待**缩短**，登记 §5.2/§5.3。

**测试**：见上述 + `merge_until` 三形态（① 等待方到点返回 `(Deadline,false)` 且**在途轮继续**（随后 `publish` 正常、闸正常清）；② **NAPI × 带期限轮并发**：等待方 `as_rc()==-1`（H3 回归）；③ `deadline=None` 逐字同旧行为）+ `Deadline` 不计耗尽的记账回归。

**判据行影响**：新增 1 条 additive 行（RECOVER 预算耗尽）+ 数值语义登记（`Deadline` 不计 `exhausted`）；C11 族行文不变。

### F4（P1）`dial_port` 锁出拨号

**方案（v2 修正类型写法）**：
- `pub type DialFn = Arc<dyn Fn(u16, Duration) -> io::Result<Box<dyn BridgeStream>> + Send + Sync>;`（**单个** `Arc`；**不留** `Arc<Mutex<…>>` 退路）；
- `BridgeHost.dial_port: Mutex<DialFn>`；`handle_conn`：`let dial = Arc::clone(&lock_host(&self.dial_port));`（短临界区）→ **出锁调用** `dial(port, budget)`；
- `set_dial(f: Arc<dyn Fn…>)` 原子换 Arc；构造点（`service_exec.rs:134/206`、`tun_exec.rs:1143`）由 `Box::new` 改 `Arc::new`；`bridge_host.rs:433` 的 `set_dial` 签名同步；注释订正（原「换轨窗口排队」理由不成立）。

**涉及文件**：`crates/homeway-core/src/facade/bridge_host.rs`、`service_exec.rs`、`tun_exec.rs`。

**风险**：低（两处生产路径的 `set_dial` 都发生在**无在途拨号**时，§0.2 条目 4 证据）。
**测试**：并发单测——dial A 阻塞在通道上；dial B 在 A 未返回时**已进入** dial（`AtomicU64` 计数 + `Instant` 断言）。

**判据行影响**：无。

### F5（P1/移出）`tun_attach`/`tun_stop`/`service_stop` 同步阻塞

**裁定（D5，含证据）**：**core 侧不做改动，移出本批**（理由与证据见 §0.2 条目 5）。交付物 = §7-2 的 tier 建议条目（`clientCoreTunAttachAsync` / `clientCoreTunStopAsync` / `clientCoreServiceStopAsync`，照 `TunRecoverAsync` 先例）。

**判据行影响**：无（不改代码）。

### F6（P1）session 域锁纪律统一 + Drop 禁 panic + 收工五段共享预算

**方案（v3：段 4 快跳、段 5 fd 归属定死、「本预算只覆盖 `session::Session::stop()`」写明）**

1. **`lock_unpoison` 单源**：上移为 crate 级 `crate::syncutil::lock_unpoison`（新文件 `syncutil.rs`，`pub(crate)`），`facade::tun_shared` 处 `pub(crate) use` 重导出（facade 既有调用点零改动）。
2. **替换面**：`session/mod.rs` 31 处 + `session/recover.rs` 5 处 + **`facade/tun_exec.rs` 3 处**（`:850`/`:1309`/`:1498`）+ **`wgcore::Client` 面**（`stop` 的 `:1468`/`:1471`、`snapshot` 的锁等**非测试** `expect`）。
3. **Drop 路径零 panic**：`stop()` 内全部锁走 `lock_unpoison`（含 `:607-609`、`:611`）；`Drop → stop() → Client::stop()` 链上不再有 `expect`；**carve-out 写明**：**分配失败 = abort**（`handle_alloc_error`），不在本批可处置面；删除死函数 `service_exec.rs:417-421`（`dial_unix_direct` 的 `unreachable!`）。**`EndpointCache::save` 内的 2 处 `expect`**（`endpoint_cache.rs:195/196`，`dir` 已判在 ⇒ 逻辑不可达）登记为残余（§7-10）。
4. **收工五段共用一个 6s 预算**（`deadline = Instant::now() + STOP_WAIT`；**范围声明**：本预算只覆盖 `session::Session::stop()`，不含隧道域 `Finish::drop`/`request_stop` 的 `c.stop()` 与 `rebuild_session→old.stop()`——§7-4/§7-5 登记）：
   1. `join_bounded(patrol)`（既有语义保留：到点 detach）；
   2. `join_bounded(hint)`；
   3. `join_bounded(save)`；
   4. **缓存终写 = `try_lock` 快跳（设计门 C1）**：`lock_unpoison` 换成 `try_lock`（`LockResult` → 拿不到即**不等待**）→ 拿到才 `save`（I/O 仍在锁内，但**锁一定空闲**）；拿不到 ⇒ 记行 + 跳过终写（去抖线程近期写已在盘上，终写尽力而为，**与 v2 自己的措辞自洽**）。若实现棒选择保留阻塞获取，则必须同时给出「段 4 到点放弃」的截止实现（`save` 内部无期限 ⇒ 只能在锁外设界，故**推荐 try_lock**）；
   5. **`Client::stop_within(deadline) -> bool`（设计门 C2：fd 归属定死）**：
      - 已 `stop.swap` 过的重入调用：直接返回（不重复等待/不重复关 fd）；
      - 首次调用：`send(Cmd::Stop)` → `take(handle)` → `join_bounded`：
        - **joined** ⇒ 关闭 `wake_wr`（现行为），返回 `true`；
        - **到点 detach** ⇒ 把「JoinHandle + `wake_wr`」交给一枚**收割线程**（`hw-engine-reap`：`h.join()` 完成后 `close(wake_fd)`）——**wake 写端只在引擎线程确认退出后关闭**（否则驱动 `poll` 会立刻 POLLHUP 忙转，`wgcore/mod.rs:1524` 只判 POLLIN），返回 `false` + 记行；
      - 不变式：`wake_wr` 不得留在 Option 里无人关（要么本次关，要么收割线程关）；`Option::take` 天然防 double-close。
   - 抽 `fn join_bounded(h: JoinHandle<()>, deadline: Instant) -> bool`。
5. **在途成本收窄**：`punch_to`（`session/mod.rs:757`）与隧道域同族 `tunnel_punch_to`（`tun_exec.rs:1296`）入口检查 stop 位，停机中**不发起**新探测（`path_probe(5s)` 预算不改）。
6. **残余登记（v2/v3 新增）**：① `stop_within` 到点 detach 后引擎线程仍可能存活到自行退出（持 UDP fd/缓冲），由收割线程收口（§7-6）；② **`fully_stopped` 弱于 Go 的 `isDone()`**（设计门 D3）：Go 的 done 在 `bridge.Stop→sess.Close→cache.Save` 真做完后置位，本设计允许 detach ⇒ 替换窗口内**旧引擎线程可能仍在**（同进程同钥双引擎的窄窗）——缓解证据：`Cmd::Stop` 已投递、新引擎新 UDP 口、出口按 `peer_id` 覆盖注册；**登记**并在 §5.3 说明（若要更强保证：给 detach 单独记「引擎未必已死」位并把 `start` 的替换门改为查该位——本设计**不采**，保持门简单）。

**涉及文件**：`syncutil.rs`（新）、`facade/tun_shared.rs`、`session/mod.rs`、`session/recover.rs`、`facade/tun_exec.rs`、`wgcore/mod.rs`、`wtransport/endpoint_cache.rs`（只读注意点）。

**风险**：中低（锁纪律替换机械；期限共享后 patrol 最坏等待 = 共享预算内剩余；`stop_within` 的 detach 语义须在注释/文档写清，避免后人混用）。
**测试**：
- 毒锁：持锁线程 panic → `snapshot()`/`stop()`/`Drop`（`catch_unwind` 包住 `drop(session)` 断言 `Ok`）；**附「修前红」证据**。
- `join_bounded` 两形态。
- **段 4 断言（C1）**：持缓存锁线程 sleep > 预算 ⇒ `stop()` 仍在预算内返回（`try_lock` 快跳生效）+ 记行。
- **段 5（C2）**：短期限 + 引擎慢退出 ⇒ detach 后 `wake_wr` 已在收割线程（Option 为 None）、引擎退出后 fd 被关（`F_GETFD` 探测或 `/dev/fd` 计数，择一）；若不可测则如实标注。
- `stop()` 全链计时：构造长任务 hint 线程（可达则端到端；不可达则逐段单测 + 标注）。

**判据行影响**：既有「收工等待巡检线程超时（STOP_WAIT）——放行自退」措辞改 per-thread ⇒ **§5.1「从→到」登记**；C17 行文不变。

### F7（P2）spawn 失败不静默（+ 桥状态诚实性）

**方案（v3：三类 spawn 全覆盖；置位点在 spawn 点；policy 统一「记行 + 残余登记」）**

1. **`Builder::spawn`（Result）静默站点**：全部改「失败 ⇒ 记行（稳定文案）+ 该站点定义的降级动作」。行文案：`<域>: <线程名> 启动失败（{e}）—— <后果>`。
2. **`std::thread::spawn`（panic 面，N6）**：
   - `service_exec.rs:251`（stop 收尾线程）：改 `Builder::spawn`；**失败路径不得留永久 `Stopping`**——就地同步收尾（置 `fully_stopped` + 清槽 + 域态/原因按现状）+ 记行；
   - `session/mod.rs:1002`（服务域巡逻的旁路探测）：改 `Builder::spawn` + 记行；**并给服务域 patrol 线程套 `catch_unwind`**——落空行为**定死（C7h）**：记行（`服务会话: 巡检线程 panic（已兜住）——本会话失去自愈巡检`）+ **不改域态**（服务域无 `unhealthy` 通道；不把可用会话打成 Failed）；
   - `tun_exec.rs:1449/1467/1756`：保留 `std::thread::spawn`（失败即 panic，被 `spawn_derived` 的 `catch_unwind` 兜住 ⇒ `mark_unhealthy("panic")`）——**登记**为「panic 归因（已兜）」。
3. **降级策略（两域统一 = 记行 + 残余登记，D6）**：patrol / hint / save / status-dump / 探测 / 恢复派生线程 spawn 失败：**只记行**（不把隧道打 `unhealthy("patrol")`——两域数据面都不依赖 patrol，而 `unhealthy` 会经 App `FailGate`/需求门控触发**整套重建**，代价大于收益）。**注意（C7e）**：这只针对**新增的 spawn 失败处置**；**既有** `tun_exec.rs:1611-1615`（巡检 3 连败 + 阶梯耗尽 ⇒ `mark_unhealthy_if_current(gen,"patrol")`）**必须保留不动**，`unhealthyReason` 取值集不变（§5.2）。残余登记：「本世代/本会话无自愈巡检」。
4. **N2 桥状态诚实性（置位点 = spawn 点，C5）**：`BridgeSock` 增 `unavailable: AtomicBool`；**置位两处**：① `start()` 的 `for idx in 0..3` spawn 点检 `Result`（失败即置位 + 记行——**不能**放在 `accept_loop` 入口，那里在 spawn 失败时根本不会执行）；② `listen_path` 返回 `None`（重试耗尽）处。`status()` 对该 sock 的路径**返回空串**；`auth_hex` 不变。**登记为「本批加固（偏离 Go）」**（Go `sockJSON` 只要 token 在就上报路径，`app_bridge.go:388-395`）。**已知形态**：`start()` 到 `listen_path` 有结论之间路径短暂非空（异步窗口）——不算回归，写进注释。
5. **可测性**：抽 `pub(crate)` 的 `fn on_spawn_failed(what: &'static str, err: io::Error, logf: &Logf, …)`，单测直接调用断言（真 `spawn` 失败不可注入）。

**涉及文件**：`facade/tun_exec.rs`、`session/mod.rs`、`facade/bridge_host.rs`、`facade/service_exec.rs`。

**风险**：低（桥路径置空只在「本该不可用」的失败态发生）。
**测试**：`on_spawn_failed` 单测；`status()` 对 `unavailable` 桥返回空串（**构造 `BridgeSock` 直置位**）；`service_exec` stop 的 spawn 失败分支不留 Stopping；服务域 patrol 的 panic 兜底；回归：`bridge_host` 既有 6 例全绿。

**判据行影响**：additive 行登记（§5.2）；桥状态空串属「已知口径注记（本批加固/偏离 Go）」（§5.3）。

### F8（P2 族）五项小修

- **F8a `SessionWriteHalf::write` 退避**（`tun_exec.rs:139-169`）：2ms 定拍 → 分级（前 50 拍 2ms ⇒ 其后 10ms ⇒ 100 拍后 20ms 封顶）；10s 无进展上界、空写短路、`pending` 复用语义**全部保留**。测试：`write_backoff_schedule` 纯函数。
- **F8b `stats_loop` 死分支**（`tun_exec.rs:1791-1795`）：删 `base_done/last_diag/elapsed` 三账面量（`diag_fd_secs` 形参保留 + 注释/登记「OHOS 沙箱无 fd 快照消费」）。
- **F8c 巡检真因**（`session/mod.rs:927`/`:1044`）：`probe_ok` 保留 `ConnErr`，失败行打 `巡检失败（连续 {n}）：{err}`（对齐隧道域同族形态）。**行文变更**（§5.1）。
- **F8d `Secret` zeroize**（`token.rs:91-137`）：
  - **拆宏**：`byte_array_newtype!` 增 `copy` 形参（或 `Secret` 单独展开）——**只对 `Secret` 去 `Copy`**，`PeerId` 保持 `Copy`；
  - `Secret` 增 `impl Drop` 调 `fn wipe_bytes(&mut [u8;32])`（volatile 写 + `compiler_fence`；**不引新依赖**）；
  - `GenRun.secret`/`Shared.secret`/`bind::Ctx.secret` 由 `[u8;32]` 改 `Secret`；**设计预期改动（C7f 抽查）**：至少 `session/mod.rs:635`（`secret: token.secret` 从 `&Token` 拷 ⇒ 须 `.clone()`）与 `tun_exec.rs:943`（`&cfg.token.secret` 在 `:915` 已 move ⇒ use-after-move，须前置 clone）；其余（`:806`/`:341` 的 `*…as_bytes()`、`session/mod.rs:1153` 的 `Secret::from(sh.secret)`）为机械改；
  - **波及面**：实测远小于 40 处（`wgcore::Client::start`/`bind`/`reg` 全是 `&Secret`，不受影响）⇒ D7 的降级条款大概率用不上；
  - **残余登记**：`Psk`、boringtun 内部副本、`CoreConfig` 移交引擎后的副本；
  - **API 面说明**：workspace 内部 crate（无发布声明）⇒ **不 bump workspace version**；
  - 测试：`wipe_bytes` 纯函数单测（填 0xAB → wipe 后全 0）。
- **F8e `lookup_host` 在飞上限（v3：分档令牌池 + 保留额度）**：
  - **池形（C6 改形）**：`ResolverLimiter`（crate 级 newtype，`Mutex<usize>` + `Condvar`）**分两档 lane**：`Critical`（= 建会话 `split_and_resolve`（`domain_eps.rs:166`）与 daemon `host reach`（`daemon/hosts.rs:634`，用户可见结论））与 `Background`（= 巡检 `refresh_sync`/`refresh_async`）；额度建议 **critical 4 / background 4（合计 8）**——理由：域名条目典型 ≤2-4，关键路径必须**永远拿得到**额度；daemon/CLI 进程可并发多会话，分档避免后台刷新饿死用户可见路径。
  - `acquire(lane, budget) -> Result<Permit, Timeout>`：阻塞等待上限 = **调用方预算**；实现必须**先扣获取耗时再传给 `recv_timeout`**（否则总耗时 = 2× 预算——设计门 D4）；拿不到 ⇒ 既有形态的 `TimedOut` + 文案「域名解析并发已达上限（N），本次等待超时」+ 记行。
  - **归属**：默认进程级单例（`OnceLock`），但池本身是**类型**（可注入/可替换）；测试复位面用 **`feature = "test-seams"` 口径**（集成测试看不到 `cfg(test)`——设计门 D4）。
  - **残余（如实登记）**：黑洞下 critical 档 4 枚卡死线程可被占满 ⇒ 退化为「**有界地失败**」（该轮解析超时），不是「不会失效」；`resolve_domains` 对每个域名条目各用整份预算（N×budget）的既有形态不变（登记）。

---

## 2. portfwd 取舍论证（诚实语义 vs 实装监听器）

> 本节是 P0 的核心决策材料，**含我方的取证结论与建议**；最终由设计门与主会话裁决（本棒不实现）。
> **默认落地 = 诚实语义（F1）**，实装监听器作为备选记录并挂账。

### 2.1 事实基线（全部回源码取证）

| 事实 | 证据（只读面） |
|---|---|
| Go 版**真 bind**：`net.Listen("tcp","127.0.0.1:<listen>")` + 逐条 accept + 经隧道拨目标 + `pipeBoth`；失败态落 `failed` + `ErrCodeBindFailed`；收工 `stopPortForwards()` 全关 | `baseline/homeway/clientcore/cmd/clientcore/app_portfwd.go:77-113`（`setPortForwards`）、`:200-242`（`pfAccept`）、`:245-255`（`pfDial`）、`:257-273`（`pfStatusJSON`）；目标文案 `:39-53` |
| Rust 版**无监听器**（只存表 + 恒 `listening`） | `facade/tun_exec.rs:602-627`；`GenRun.pf_rules` 注释自认「监听器执行体后续接」（`:317-318`） |
| **需求真源要求实装**（SHALL） | `tier:openspec/specs/port-forwarding/spec.md`「端口映射的建立与访问」：「App SHALL 按当前主机配置的每条映射在手机上监听 `127.0.0.1:<监听端口>`（仅回环，不暴露局域网），并把每条入站 TCP 连接经隧道转发到该条映射的目标」+ 三个 Scenario |
| 状态契约要求「监听中 或 失败及原因」+ **失败映射 MUST 带枚举 code** | 同 spec「映射状态可见」：`code` 稳定枚举 = `bind_failed`/`dial_failed`/`invalid_target`；**当前可产出的只有 `bind_failed`**；「未知/空 `code` SHALL 落默认文案并原样展示 `err`、**不误归因**」 |
| App 侧按 code 分派文案（MUST NOT 匹配 err 原文） | `tier:entry/src/main/ets/model/PortForwardRules.ets:207-218`（`pfFailText`：`bind_failed` ⇒ 「**端口被占用**」；空/未知 ⇒ `err` 原文或「未知原因」） |
| App 页面三态 + 覆盖判定 | `tier:entry/src/main/ets/pages/PortForwardsPage.ets:225-239`；`PortForwardRules.ets:196-200`（`allForwardsCovered`） |
| 词表是**冻结契约**：新增 code 需改 tier 台账/manifest（只读，本批不可）；`bind_failed` 必须继续被声明 | `tools/check-vocab.sh:1-13/67-77`、`crates/homeway-core/tests/vocab_dump.rs:23-26`、`crates/homeway-core/src/lib.rs:47-68` |
| **原语齐备、有实证先例**：CLI 已有「本地监听 → 恢复感知拨号 → 双向泵」整条实现（PERF-AB/matrix 在跑） | `crates/homeway-cli/src/main.rs:1297-1485`（`:1403` bind、`:1419` `healing_dial_addr(…,15s)`、`:1430-1477` 泵）；消费于 `tools/perf-ab.sh:229-234`（RRR 臂）、`tools/matrix.sh:12` |
| 隧道域可复用的原语 | `session_connect`（`tun_exec.rs:183-230`）、`BridgeStream::into_halves`、`bridge_host::pump`（`:747-778`）、`ConnGate`（`:226-273`） |
| 出口侧同路径已被验证可达（任意目标 IP:port） | PERF-AB RRR 臂（`--map 42901:<lan_ip>:42807` 真跑）；`server/intercept/mod.rs:7` |

### 2.2 两个方案的对照

| 维度 | **A. 诚实语义（默认落地 = F1）** | **B. 实装监听器** |
|---|---|---|
| 用户可见 | 每条映射恒「失败 · 手机核未提供端口转发监听（127.0.0.1:… 未监听）——该映射在当前版本不可用，不影响隧道」；浏览器连不上（**如实**） | 映射真可用；端口被占/目标拒绝时显示真实原因 |
| 与 tier spec 的关系 | **不达标**（违反 SHALL 监听）；失败映射的 `code` 只能取 `""`（违反 MUST 带枚举 code → 走 App **空码兜底**，不误归因）或取 `bind_failed`（满足 MUST 但让 App 显示「**端口被占用**」= 具体化假归因，更坏） | **达标**（三态与 code 全是真值） |
| 代码量 | 小（≈60-100 行含测试） | 中（≈150-250 行 + 守卫三件套 ≈+80 + 测试） |
| 安全面 | **零新增** | **新增回环监听口**（仅 `127.0.0.1`，spec/Go 同形）；守卫：每主机 ≤8 条、并发流阀（Go `maxTCPFlows=4096`）、拨号 15s 期限、世代收工全关 |
| 风险等级 | 低（纯状态/返回码） | 中（新线程/fd/生命周期挂点 + **真机 E2E 验证成本**） |

### 2.3 取证结论与建议

1. **事实层**：不是「少做一个可选功能」，而是「**已登记需求（SHALL）的实现在 Rust 侧缺失**」（Go 有、spec 要求、tier 页面按「监听中」设计）。
2. **可行性层**：B 可复用既有原语且**已有整条 CLI 先例在 PERF-AB 真跑**；增量 = 并发阀/计数/世代收工三件套 ⇒ **改动量中小**。
3. **风险层**：新增面 = **回环监听口**（非局域网暴露），守卫可照 Go 逐条落地；真正代价 = 真机 E2E 验证成本 + 本批定位外延。
4. **诚实性层（决定性）**：A 的失败态在冻结词表下**说不清**（`bind_failed` 假归因 / 空码违反 MUST）⇒ **A 拿不到既不违规又不误导的表达**。
5. **建议**：**按主会话定调落 A**，同时把 B **登记为挂账的独立功能项**，并**明确记录**：A 落地后 tier `port-forwarding` spec 处于「已知不达标」状态——**本批必办**（写进 `REVIEW-ROADMAP.md` + §5.1 登记 + §7-1；设计门 5.2 要求升级为必办，不是「建议」）。
   - 若改判「本批直接做 B」：按 §2.4 实现，F1 的 `PfState::unavailable` 不引入。

### 2.4 B 方案实现轮廓（备查；若采纳则替代 F1 的状态面）

1. `GenRun` 增 `pf: Mutex<PfRuntime>`（`lns: Vec<(u16, Arc<AtomicBool>, JoinHandle)>`、`states: Vec<Arc<PfState>>`、`accepted/fails: AtomicU64`）。
2. 整表替换：锁内停旧监听器 + 清状态 → 逐条 `TcpListener::bind(("127.0.0.1", listen))`：成功 ⇒ `PfState::listening` + spawn accept 线程；失败 ⇒ `PfState::failed(err)`（`bind_failed`）；返回 0（真有承载面）。
3. accept 线程：并发阀 → `conns += 1` → `session_connect(&run, target_port, 15s)` → `into_halves()` → 复用 `bridge_host::pump`；目标语义走 `portfwd::dial_target()`（现成，`portfwd.rs:91-103`）。
4. 世代收工：`Finish` guard 内与 `bridge.stop()` 同序停全部监听器。
5. 状态面：`portfwd_states` 读真 `PfState`；`pf_accepted/pf_fails` 与 `stats` 行接真计数（行文不变）。
6. 安全守卫：仅 `127.0.0.1`；`listen ≥1024` 且同表唯一（已有）；规则条数上限（新增，建议 8）；并发流阀；拨号 15s 期限。
7. 测试：单测（两态 + `conns`）+ E2E（本地 Rust 出口 + 回环集成）+ **真机浏览器验证**（用户触点）。

---

## 3. 「二选一」类决策的取证与裁定

| # | 决策 | 选项 | 取证 | 裁定 |
|---|---|---|---|---|
| **D1** | portfwd 走 A 还是 B | A 诚实语义 / B 实装监听器 | §2.1-2.3 | **A 默认落地**；**B 写进设计供设计门复核**并**挂账为独立功能项**；**最终由主会话裁决** |
| **D2** | 失败态 `code` 取值 | `bind_failed` / `""` | tier spec MUST 要枚举 code；App 对 `bind_failed` 文案是「端口被占用」⇒ 具体假归因；空码走 App 登记的兜底路径；词表门不受影响（声明集仍含 `bind_failed`） | **取 `""` + 明确 err**；**不接受** `bind_failed`（报告一个没发生的失败原因）；spec MUST 偏离**显式登记** |
| **D3** | `ClientCoreTunSetPortForwards` 返回码 | `0` / `-1` | rc 契约：`0`=已应用、`-1`=没有已接管数据面的世代；现「0」= 谎报；tier 只把 rc 写日志（`:1010`），页面状态来自 status | **取 `-1`**（读法 = 「没有已接管（端口转发）数据面的世代」= 事实）+ 登记 |
| **D4** | 阶梯期限落在哪一层 | ① 只给等待方 ② 只给阶梯本体 ③ 两者 | 只做 ① 时执行方仍越界 ≈60s | **③ 两者都做**；巡检/NAPI 传 `None` ⇒ 逐字不变 |
| **D5** | `tun_attach`/`tun_stop`/`service_stop` 同步阻塞 | 本仓改 / 登记 + 移出 | Go 同值；异步壳在 tier napi（`Index.d.ts` 证据） | **只登记 + 移出**（§7-2） |
| **D6** | spawn 失败处理强度 | ① 只记行 ② 记行 + 降级 | ② 需分类值/状态位，且 `unhealthy("patrol")` 会触发 App 整套重建（两域数据面都不依赖 patrol） | **①**（v2 改判）+ **例外**：桥 accept/bind 失败 ⇒ 路径置空 |
| **D7** | `Secret` zeroize 侵入面 | ① 拆宏后只对 `Secret` 去 `Copy` + Drop 擦 ② 只登记 | 审计明文列此条；`Copy` 使多份栈副本无法统一擦除；同宏另有 `PeerId`（保持 `Copy`） | **①（拆宏）** + 残余登记；波及面 >40 处则降级并记 `QF.md` |
| **D8** | 「状态快照分层缓存」 | 重开 / 登记 | `QI-design.md` §2 F8 已裁决不做 | **只登记不重开**（§7-3） |
| **D9** | `LadderRc::Deadline` 边界 rc | `-3`（v1）/ `-1`（v2/v3） | gate 为 NAPI 与桥拨号**共用**；-3 ⇒ tier 渲染「本机网络栈没准备好」（`:1102-1103`）= 错误归因；-1 = 「阶梯走完仍不可达」（`:1116`） | **取 `-1`** + 并发回归单测 |
| **D10** | 服务会话失败实例的槽位 | 清槽（v1-a）/ 保留槽（v2-c） | Go：`svcStateFailed && isDone()` ⇒ 换新会话，替换前 `Snapshot` 仍报 failed+原因（`service.go:992-1018`）；tier `bridgeHostOf` 把 `SVC_FAILED` 映射为 `BRIDGE_MODE_SERVICE/FAILED`（清槽变 `NONE/ABSENT`，丢失败态） | **保留槽位（c）**：失败实例入槽 + `fully_stopped` 作「已收尾」位 ⇒ **失败可见 + 可重建** |
| **D11** | F8e 池形 | 全局静态快失败（v1）/ 分档令牌池阻塞（v2/v3） | 快失败把瞬时空洞放大成持续失效（`split_and_resolve` 无兜底）；daemon `reach` 是第二处用户可见路径；进程级单桶会让后台刷新饿死用户可见路径 | **分档令牌池**：`Critical`（建会话 + daemon reach，保留额度 4）/ `Background`（巡检刷新，4）；获取耗时扣进调用方预算；默认进程单例但**池是类型**（可注入） |
| **D12** | `Deadline` 的耗尽记账与重建时序 | 计入 `exhausted`（v2）/ 不计 + 等待方到点跳过重建 | `merge` 之后无条件 `maybe_rebuild_if_exhausted`；等待方提前返回会在在途轮未完时触发 `rebuild_session → old.stop()` 掉在途轮的 Client（设计门 C4） | **不计入**（两处埋点显式排除）+ **等待方到点跳过重建决策** + 登记 |
| **D13** | `fully_stopped` 弱于 Go `isDone()` 的窗口（F6b detach 引入） | 加「引擎未必已死」位（替换门改查它）/ 登记窗口 + 缓解论证 | Go 的 done 在 `bridge.Stop→sess.Close→cache.Save` 真做完后才置位（`service.go:838-858`）；本设计允许 detach ⇒ 窄窗内旧引擎线程可能仍活 | **登记 + 缓解论证**（`Cmd::Stop` 已投递、新引擎新 UDP 口、出口按 `peer_id` 覆盖注册），**不**加位（保持门简单；如需更强保证，加位是本设计已列的替代） |

---

## 4. 测试与验收计划

### 4.1 门（沿用批协议）

- `cargo test --workspace` 全绿（已知 flake 按「隔离复跑 + 与改动面无交集 + 基线可复现」三者齐备方可放过；`QF.md` 列明本轮观察）。
- `cargo clippy --workspace --all-targets -D warnings` 无新告警。
- 判据/契约面：`docs/INTEROP-CRITERIA.md` 登记与代码同批 commit；`tools/check-vocab.sh` 绿（不新增词值）。
- **修前红证据**（Q-E 口径）：F6 的毒锁/Drop abort 面与 F2 的状态面须附「改前红 + 改后绿」。

### 4.2 逐条判绿 / 证伪

| 项 | 判绿（必须给证据） | 证伪/退出条件 |
|---|---|---|
| F1 | 单测：`unavailable` 快照四元组；`portfwd_states` 目标文案四形态；`request_port_forwards` 恒 -1 且表已存 | 若 tier 侧有按 `state=="listening"` 做**功能门**（非仅展示）⇒ 回设计（已复核：仅展示） |
| F2 | 三分支各自可达（stopping 竞态走清槽 + Idle；Ready 发布；硬失败保留槽 + 域 Failed + `fully_stopped`）+ `service_status` 报 failed+原因 + 随后 `start()` 可替换 + `stop()` 无 2s 空等 | 若 `synthetic_failed_for_test` 构造成本 >100 行 ⇒ 退极简替身 + 如实标注（不得跳过验收面） |
| F3 | `run_ladder(deadline)` 到点退出 + 行文；`merge_until` 三形态 + `as_rc()==-1` + `Deadline` 不计耗尽 + 等待方到点**不**触发重建；`dial_with_recover` 桩被调用且预算内 Timeout（预算 > `FIRST_TRY`） | 若「执行方中途放弃」破坏 `RunGuard` 单飞/发布不变量 ⇒ 改设计 |
| F4 | 并发 dial 单测（B 不等待 A） | 若 `Arc<dyn Fn>` 触发 `Sync` 约束 ⇒ 换等价形态并记录（**不许**退回锁内拨号） |
| F5 | 无代码；§7-2 条目在 + tier 建议成文 | — |
| F6 | 毒锁 + Drop 不 panic（含 `Client::stop` 链）；`join_bounded` 两形态；段 4 `try_lock` 断言；段 5 fd 归属（detach 后 fd 由收割线程关）；全链计时 | 若隧道域 `stop_within` 引发 fd 归属问题（TUN fd 由扩展持有，不受影响）⇒ 回退为「只对服务域用 `stop_within`」（= 现状语义，措辞按 D1 复核简化） |
| F7 | `on_spawn_failed` 行文/副作用；桥 `unavailable` ⇒ status 空串（置位在 spawn 点）；`service_exec` stop spawn 失败不留 Stopping；服务域 patrol panic 兜底 + 记行 | — |
| F8a-e | 各自单测（退避表、stats 行、巡检真因行文、`wipe_bytes`、令牌池分档/扣时/复位） | F8d 见 D7 偏差条款 |

### 4.3 回归面（不得破）

- C 族判据行（C1/C3/C7/C10/C11/C13/C16/C17）逐字不变；E 族不在本批。
- `tunStatusJSON` 键序/键集：`fixtures/vectors/tun_status.jsonl` 10 案（**全为 `runner=None` 形态 ⇒ 不含 `portForwards`**）+ `tun_status.rs::full_key_face_attached`。
- `service_op` 门（`0/-1/-3/-4` 与幂等语义）、`bridge_dial_closure_weak_only`、`RecoverGate` 既有 4 例、`bridge_host` 既有 6 例。

---

## 5. 判据行影响（登记草稿）

### 5.1 判据变更记录（`INTEROP-CRITERIA.md` 「判据变更记录」表）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-F 批落地） | **服务会话巡检失败行**（`巡检失败（连续 %d）：探测超时`——**非 C1–C17 判据行**） | `巡检失败（连续 %d）：探测超时`（**恒写「探测超时」**）→ `巡检失败（连续 %d）：<探测的真实错误>` | F8c：归因写死 = 报错信息失真（对齐隧道域同族行 `probe.unwrap_err()` 形态） | `session/mod.rs` 巡检失败行、grep 该行排障的脚本；**成功拍/门控行不变** |
| 2026-10-08（Q-F 批落地） | **服务会话收工等待行**（非编号判据行；**既有行文变更**） | `收工等待巡检线程超时（STOP_WAIT）——放行自退` → per-thread 形态（`收工等待 <线程名> 超时（STOP_WAIT）——放行自退`；覆盖巡检 join / hint join / 缓存落盘 join / 缓存终写跳过 / client 收工五段） | F6b：五段共用一个 6s 预算（此前仅巡检有界） | `session/mod.rs`；C17「已收工（state=%s）」不变 |
| 2026-10-08（Q-F 批落地） | **`portForwards[]` 状态文案与 `ClientCoreTunSetPortForwards` 返回码**（**契约面行为变更**，非编号判据行） | ① `state`：恒 `"listening"` → `"failed"`；② `err`：空 → `"手机核未提供端口转发监听（127.0.0.1:<listen> 未监听）——该映射在当前版本不可用，不影响隧道"`；③ `code`：空（不变）+ **失败映射带枚举 code 的 spec MUST 被有意偏离**（D2）；④ `target`：`":0"`/`":port"` → `pf_target_text` 四形态；⑤ rc：`0` → `-1` | F1（P0 假成功 + 假状态）；对照 Go 真 bind 与 tier spec（SHALL 监听）——**功能缺口挂账见 §7-1（本批必办）** | `tier:port-forwarding` spec（**已知不达标**）、`tier:pages/PortForwardsPage.ets`（`:484` 提示失义）、`tier:…/TierVpnExtensionAbility.ets:1010`（rc 日志文案失义）、`facade/tun_exec.rs`/`portfwd.rs` 单测；**`fixtures/` 无 portForwards 夹具 ⇒ 无字节夹具变更**；`tools/check-vocab.sh` 不受影响 |

### 5.2 计数输入集 / 数值语义变化（行文不变）

| 日期 | 条目 | 从 → 到（数值语义） | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-F） | **`stats.pfAccepted`/`stats.pfFails`** 与 `stats:` 行的 `pf=a/f` | 值不变（恒 0）——**语义由「未接线的占位」改为「真值：无监听器 ⇒ 无 accept/失败」** | F1 | `facade/tun_exec.rs::runner_of`、`stats_loop`；tier 无消费（只校验键存在） |
| 2026-10-08（Q-F） | **新增观测行（additive）** | 无 → 有：① 桥 `<桥名> accept 线程启动失败/监听失败——该桥本轮不可用`；② 隧道域/服务域各派生线程 `启动失败（{e}）—— …`（含「本世代无自愈巡检」）；③ `服务会话暖机硬失败（原因=…）——不发布就绪`；④ `RECOVER 预算耗尽（起跑=%s，原因=%s，预算 %v 已用尽）—— 放弃等待`；⑤ `域名解析并发已达上限（N）—— 本次等待超时`；⑥ `service_exec` stop 收尾线程起不来 ⇒ 就地收尾行；⑦ 服务域 `巡检线程 panic（已兜住）` | F2/F3/F6/F7/F8e：静默路径改可观测（审计明文要求「spawn 失败落判据行」） | 各日志族读者；**非编号判据行** |
| 2026-10-08（Q-F） | **`unhealthyReason` 取值集不变** | 仍 = {patrol, fd, panic, stop}；**既有** `tun_exec.rs:1611-1615`（3 连败 + 阶梯耗尽）不动；spawn 失败不再新增该面 | F7/D6 | `facade/tun_exec.rs`；tier `FailGate` 判据不变 |
| 2026-10-08（Q-F） | **`LadderRc::Deadline` 的记账语义** | `Deadline` **不计入** `LadderRc → exhausted`（`Deadline` 不是「阶梯走完未恢复」的证据）；等待方到点返回时**跳过** merge 后的重建决策（既有「merge 返回即本轮结束」的结构不变） | F3/D12（设计门 C4）：防「调用方预算紧张把会话推向 REBUILD」与「在途轮未完被 rebuild 掉 Client」 | `session/mod.rs`（`note_ladder_result` 与 `session_recover` 两处埋点）、`tun_exec.rs`（隧道域无 exhausted 面，仅 rc） |
| 2026-10-08（Q-F） | **`healing_dial_*` 所有调用方的最长等待** | 从「首试 + 尾试各受预算约束（阶梯可越界 ≈4×）」→「首试 + 阶梯 + 尾试合计受同一预算约束」；预算表：服务桥/tun 桥 15s、`files.rs` 调用方预算、CLI `portfwd` 15s、daemon `budget.min(15s)` / 30s | F3（**偏离 Go 的加固**） | daemon 拨号（Q-H 面，最长等待缩短）、files/term/speedtest 桥、CLI `portfwd` |
| 2026-10-08（Q-F） | **域名解析并发上限（F8e）** | 无上限 → 分档令牌池（critical 4 / background 4），获取等待计调用方预算；**有界地失败**（黑洞下该轮解析超时），不是「不会失效」 | F8e（两轮设计门 3.3/3.3'/C6） | `wtransport/domain_eps.rs`、建会话路径、巡检刷新、daemon `host reach`（用户可见结论面） |

### 5.3 已知口径注记（`INTEROP-CRITERIA.md` 「已知口径注记」追加草稿）

- **【Q-F 批，2026-10-08】portfwd 诚实态（F1，行为差异 + 功能缺口）**：手机核**未实装**端口转发监听器，映射状态由恒 `listening` 改为恒 `failed` + 明确 err（空 `code`），热替换 rc 由 `0` 改为 `-1`。Go 基线真 bind ⇒ **功能缺口如实化**；挂账与实装轮廓见 `docs/reviews/QF-design.md` §2/§7-1。
- **【Q-F 批，2026-10-08】桥状态诚实性（F7b，本批加固 ≠ Go 同形）**：桥的 accept 线程 spawn 失败或 `listen_path` 重试耗尽时，`bridgeFilesSock/bridgeTermSock/bridgeSpeedSock` 由「上报路径」改为**空串**——**Go 的 `sockJSON` 不这么做**（`app_bridge.go:388-395`）⇒ **偏离 Go 的加固**；`bridgeAuth` 不变；`start()` 到 listen 结论之间的短暂非空窗口为已知形态。
- **【Q-F 批，2026-10-08】服务会话暖机硬失败（F2，行为差异）**：`Session::start` 返回 Ok 但快照 `state=failed` 时：① rc 面不再「幂等返 0」；② 失败实例**保留**在运行槽（`service_status` 继续报 `failed`+原因；tier `bridgeHostOf` 的 `SVC_FAILED → BRIDGE_MODE_SERVICE/BRIDGE_HEALTH_FAILED` 保持一致）；③ `fully_stopped` 置位后 `start` 可**替换**该实例（Go `svcStateFailed && isDone()` 同形）。此前行为 = rc 面谎报 + 不可重建 + 无巡检。
- **【Q-F 批，2026-10-08】桥拨号期限（F3，本批加固 ≠ Go 同形）**：带预算的桥拨号把**恢复阶梯与阶梯等待**计入同一预算（此前可越界 ≈4×）；**Go 的阶梯与闸等待同样不受调用方预算约束** ⇒ 偏离式加固。两域巡检与 NAPI `tun_recover` 不受影响（`deadline=None`）；`LadderRc::Deadline` 对外映射 `-1` 且不计入耗尽。
- **【Q-F 批，2026-10-08】收工等待（F6，行为差异 + 残余）**：服务会话 `stop()` 的**五段**（巡检 join / hint join / 缓存落盘线程 join / 缓存终写（`try_lock` 快跳）/ `Client::stop_within`）共用一个 6s 预算；到点放行自退 + 新行。**残余**：`stop_within` 到点 detach 后引擎线程可能存活到自行退出（由收割线程收口）；`fully_stopped` 因此**弱于** Go 的 `isDone()` ⇒ `start` 替换窗口内旧引擎线程可能仍在（缓解：`Cmd::Stop` 已投递、新引擎新 UDP 口、出口按 `peer_id` 覆盖注册）。
- **【Q-F 批，2026-10-08】域名解析并发上限（F8e，本批加固）**：`lookup_host` 的 worker 线程数分档上限（critical 4 / background 4；阻塞获取，等待计调用方预算）；Go 无上限（每调用新 goroutine）。

---

## 6. 设计门记录（dsh 外部评审）

### 6.1 结论

- **第一轮**：`dsh --profile headless`，目录 **`/tmp/dsh-review/r12.kyMDLA`**，**exit=0**（前台捕获）。结论 = **不建议原地过设计门**（3 组高危 + **14 条中危** + **13 条低危**），本棒全部认同（0 条不认同）并整改为 v2。
- **第二轮（复校）**：目录 **`/tmp/dsh-review/r13.4ux4qT`**，**exit=0**（前台捕获）。结论 = **v2 不能原地过门，但只差一轮「回写式」小修（v3），不需要第三轮评审**：三条高危**方案骨架全部正确**（H2/H3 已整改到位；H1 的三个具名洞已覆盖），14 条中危 13 条已整改、1 条（10.1 计数）部分整改；**新发现 C1–C6（必改/并入）+ C7a–j（记账）+ D3（新窗口）**。
- **第三版（v3，本稿）**：C1/C3/C5/C6 **已回写设计**；C2/C4/C7/D3 已并入设计并登记（逐条见 §6.6）。**过门结论 = 判过**（§6.8）。
- 两轮均**未推翻方案骨架**；两轮均**未发现整条误报**（本棒 14 条派单条目复验全部成立）。

### 6.2 第一轮评审原文摘要（编号为评审者原编号）

**一、方案是否解决根因**：**1.1【高】**`QF-design.md:27` 的「App 侧 `service_status` 恒 ready」与源码相反（`status()` 返回会话快照 = failed+原因；域态 Ready 只污染 rc 门/幂等），真正后果是「rc 面谎报受理 + 不可重建 + 核心不自愈」，与 §F2-3 自相矛盾，§5.3 登记草稿同样不实；**1.2【中】** §0.2 条目 3 量级不完整（服务域 `rebuild_session` 无任何期限 ⇒ 结构性）；**1.3【低】** §0.2 条目 8 漏 `std::thread::spawn` panic 面；**1.4【低】** §0.2 条目 5 尾注漏 `Client::stop()` 内部 join。

**二、panic/abort 面**：**2.1【高】** F6「Drop 路径禁 panic」不完整（`wgcore/mod.rs:1468/1471` 的 `expect` 在 Drop 链上，§7 未点名）；**2.2【中】** F7 只覆盖 `Builder::spawn`，`std::thread::spawn`（失败即 panic）5 处未列——`service_exec.rs:251` 最险（永久 `Stopping`）、`session/mod.rs:1002` 服务域 patrol 无 `catch_unwind`、`tun_exec.rs:1449/1467/1756` 靠 `catch_unwind` 兜住（归因 panic，未登记）；**2.3【中】** §7 清单漏 `tun_exec.rs:850/1309/1498`，且「facade 侧已全走 `lock_unpoison`」不实；**2.4【低】** `service_exec.rs:419-421` 的 `unreachable!`；分配失败 = abort 未 carve-out。

**三、有界性**：**3.1【高】** 「三 join 共用 6s ⇒ `ServiceStop`/`session.stop()` 有界」不成立（其后 `cache.save` fsync 与 `Client::stop()` 无界 join）；**3.2【高】** `Deadline` 在 NAPI 面不可达的推理不成立（共用 gate；-3 ⇒ 用户可见错误归因）；**3.3【中】** F8e「超限快失败」把 DNS 黑洞放大成持续失效（`split_and_resolve` 无兜底）+ static 计数与并行测试冲突；**3.4【中】** F3b 漏 `tun_exec.rs:846-853`（后果含单飞位 `inflight` 永久占用）；`Client::read`/`Client::stop` 未登记；**（正面）** F3-1/2 的 `RunGuard` 不变量分析自洽、`merge_until` 薄壳正确、残余 ≤1 个 `ACTION` 越界建议写明。

**四、状态机正确性**：**4.1【高】** v1 推荐 (a) 会丢用户可见失败原因（`domain.reason()` 不经 NAPI 暴露；tier `ServiceSession.ets:348-358` 映射 `SVC_FAILED→BRIDGE_HEALTH_FAILED`，idle→`ABSENT`）；(b) 的排除理由是伪二选一（Go：失败会话保留在 `m.cur` + `failed && isDone()` 换新会话）；建议改采 (c)；**4.2【中】** F2 测试缝建不起来 Ok-but-Failed 假会话（`Session` 字段私有），退路验不到验收面；**4.3【低】** F2-1 伪码顺序与 Err 分支不一致（Go `finish` = `bridge.Stop() → sess.Close()`）；`set_reason` 建议断言非空。

**五、诚实性**：**5.1【高】** 同 4.1（F2 造成新的用户可见信息丢失）；**5.2【低】** F1 与 spec 的关系需要「知情落点」：§7 的「建议在 ROADMAP 增行」应升级为**本批必办**。

**六、Go 对齐**：**6.1【中】** 「桥状态诚实性…（等价 Go『桥未起=空串』）」不实（Go `sockJSON` 只要 token 在就返回路径）；**6.2【中】** F3 实为偏离 Go（Go `recoverStaleSession` 无 ctx、`merge` 等待方裸 `<-r.done`）；**6.3【中】** F3 消费面漏 CLI/daemon/files；**6.4【低/正面】** F1 的 Go 同值声明属实。

**七、Go 直译痕迹**：**7.1【中】** F4 写法自相矛盾（双层 Arc；退路把锁搬回热路径）；**7.2【中】** F8d 宏级去 `Copy` 连带 `PeerId`；**7.3【低】** `PfState.state/code` 可改 enum。

**八、越界**：**8.1【低/正面】** 总体未越界；**8.2【低】** F7-2 两域降级强度不一致且理由不成立（隧道域数据面同样不依赖 patrol；把 `running` 打成 0 触发整套重建，代价大于收益）。

**九、可测性**：**9.1【中】** F2/F3 的测试缝缺明确设计（`healing_dial` 依赖真 `Client`/`Session`）；**9.2【低】** F1 的 facade 集成断言落空（`FakeExec::runner()` 恒 `None`）；**9.3【低】** F8e static 计数与并行测试冲突。

**十、登记完整性**：**10.1【中】** §0 登记预告与 §5 不一致；**10.2【高】** F2 可见面变化未按事实登记（同 1.1/4.1）；**10.3【中】** 桥路径置空登记口径（同 6.1）；**10.4【低】** `Secret` 去 `Copy` 的版本面；**10.5【低】** F6b 改了既有行文 ⇒ 应走「从→到」而非 additive。

**总体**：不建议原地过门；必须改 H1/H2/H3；建议改 12（实为 14）条中危；肯定复验方法、D 表、§2 A/B 对照与 §7 登记达到 Q-D/Q-E 水准。

### 6.3 第一轮逐条处置表

| 评审编号 | 严重度 | 处置 | 落到 |
|---|---|---|---|
| 1.1 | 高 | **认同** | §0.2 条目 2 重写；§0.4-1；§5.3 |
| 1.2 | 中 | **认同** | §0.2 条目 3；§7-4 |
| 1.3 / 1.4 | 低 | **认同** | §0.3 N6；§0.2 条目 5 尾注；§7-5 |
| 2.1 | 高 | **认同** | F6-2/3/4（含 `stop_within`）；§7-6 |
| 2.2 | 中 | **认同** | F7-2 |
| 2.3 | 中 | **认同** | §0.2 条目 6；F6-2 扩面 |
| 2.4 | 低 | **认同** | F6-3 |
| 3.1 | 高 | **认同** | F6-4 五段预算；§5.1；§5.3 |
| 3.2 | 高 | **认同** | D9；F3-1 |
| 3.3 | 中 | **部分认同**（改形接受；「永久失效」按条件化表述） | F8e（v3 再改形为分档池）；§5.2/§5.3 |
| 3.4 | 中 | **认同** | F3b 补 `tun_exec.rs:848`；§0.3 N1；§7-7 |
| 4.1 | 高 | **认同** | D10；F2 改采 (c) |
| 4.2 | 中 | **认同** | F2 测试缝（v3 追加工厂缝，见第二轮 D6） |
| 4.3 | 低 | **认同** | F2 伪码顺序 + `set_reason` 断言 |
| 5.1 | 高 | **认同** | 同 4.1 |
| 5.2 | 低 | **认同** | §2.3-5 + §7-1（升级为本批必办） |
| 6.1 / 6.2 | 中 | **认同** | §5.3 两段改「本批加固（偏离 Go）」；§0.4-7 |
| 6.3 | 中 | **认同** | F3 消费面段；§5.2 |
| 6.4 | 低/正面 | 记录 | — |
| 7.1 | 中 | **认同** | F4 重写（单 Arc，删退路） |
| 7.2 | 中 | **认同** | F8d 拆宏；§0.2 条目 13 |
| 7.3 | 低 | **部分认同**（顺手项，可登记不做） | F1-2 附注 |
| 8.1 | 低/正面 | 记录 | — |
| 8.2 | 低 | **认同** | D6 改判；F7-3 |
| 9.1 | 中 | **认同** | F2 缝 + F3 `dial_with_recover` |
| 9.2 | 低 | **认同** | F1 测试项 |
| 9.3 | 低 | **认同** | F8e 复位面（v3 改 `feature="test-seams"`） |
| 10.1 | 中 | **部分整改→v3 已补** | 文档头计数按 §5 实际回填（§6.6 C7a） |
| 10.2 | 高 | **认同** | §5.3 服务会话段重写 |
| 10.3 | 中 | **认同** | §5.3 桥段 |
| 10.4 | 低 | **认同** | F8d「API 面说明」 |
| 10.5 | 低 | **认同** | §5.1 新增「收工等待行」从→到 |

### 6.4 第一轮不认同项

**无。** 34 条逐条回源码复核后全部成立。两条补充证据（不影响处置）：① `service_exec.rs:251` 的 `std::thread::spawn` 失败在 capi 侧会被 `guard` 兜成 -9（不静默），但对 core 状态机是**永久 Stopping**；② tier 把 `SVC_FAILED` 映射为 `BRIDGE_MODE_SERVICE/BRIDGE_HEALTH_FAILED`（`ServiceSession.ets:349-356`），故 F2 保留槽位对 App 的桥选择有实际意义。

### 6.5 第一轮过门结论

**修订后判过（v2）**：三条高危全部改设计（F6 扩面 / F2 改采 Go 同形 / `Deadline` 映射改 -1）；14 条中危全部并入；13 条低危并入或登记。**v2 并于第二轮复校**（§6.6）。

### 6.6 第二轮复校记录（v2 → v3）

**轮次**：`dsh --profile headless`，目录 **`/tmp/dsh-review/r13.4ux4qT`**，**exit=0**（前台捕获；`output.md` 197 行）。
**评审者实跑**：`cargo test --workspace` → **590 passed / 0 failed / 17 ignored**（本批 flake 未复现）。

**A. 三条高危复核**：**A1 H1 → 部分整改**（三个具名洞 F6-2/F6-4/2.3/2.4 都覆盖✅；但强声明仍有两处反例 = **C1**（段 4 的锁/IO 无界）、**C2**（`stop_within` 的 `wake_wr` 归属未定义）；域内安全性：`stop_within` 只用于服务域，隧道域三处 `Client::stop()` 仍原方法 ⇒ 无隧道域 detach 问题）；**A2 H2 → 已整改**（§0.2 条目 2 重写与源码一致、D10 的 Go 同形经独立回读确认；但伪码有三处不自洽 = **C3**）；**A3 H3 → 已整改**（-1 的归因正确；`LadderRc` 消费点**穷举**确认无第二处误映射；未登记的一点 = **C4**）。

**B. 14 条中危复核**：1.2/2.2/2.3/3.3/3.4/4.2/6.1/6.2/6.3/7.1/7.2/9.1 **已整改**（各自给了回源证据：`rebuild_session` 无期限属实；5 处 `std::thread::spawn` 实测齐；`tun_exec.rs:848` 存在且 `rearm_soft`/`refresh_reg_result` 全站点穷举 ⇒ F3b 清单完整；单 Arc 定死；拆宏方向正确但必引出 2 处编译错；`dial_with_recover` 可测但预算须 > `FIRST_TRY`）；**10.1 → 部分整改**（预告计数仍不一致）；**10.3 已整改**。低危 13 条逐条对得上，无静默漏项。

**C. 新发现（必改/并入）**：

| 编号 | 级别 | 问题 | v3 处置 |
|---|---|---|---|
| **C1** | 中 | F6-4 段 4「缓存终写」的 `lock_unpoison` 无期限，`EndpointCache::save` 持锁做读盘/建目录/写/rename ⇒ 6s 预算可被击穿（与前置判断不自洽） | **已回写**：段 4 改 **`try_lock` 快跳**（拿不到即记行跳过）；§4.2 加断言测试 |
| **C2** | 中 | `Client::stop_within` 的 `wake_wr` 归属未定义：早退在 take 之前 ⇒ detach 后 fd 永不关（每命中泄漏一枚）；反之 detach 时立刻关会影响驱动 `poll`（POLLHUP 忙转） | **已回写**：detach 路径把「JoinHandle + `wake_wr`」交给**收割线程**（join 后关）；不变式「wake 写端只在引擎确认退出后关」；§4.2 加测试 + §7-6 登记 |
| **C3** | 中 | F2 伪码三处不自洽：(a) 把 `stopping` 竞态与硬失败合并（会误判且不清槽）；(b) 失败实例没写进会话槽（status 走域态分支 + 每次 `stop()` 白等 2s）；(c) 失败分支的域态写入无「仍是当前 run」校验（与并发 `stop()` 分裂） | **已回写**：F2-1 改**显式三分支**（stopping 逐字不变 / Ready 发布 / 硬失败）；硬失败**把实例存入槽**；域态写入**加 ptr_eq 身份守卫**；§4.2 加「stopping 竞态回归」与「stop 无空等」两项 |
| **C4** | 中 | 等待方到点提前返回改变了重建时序（`merge` 之后无条件 `maybe_rebuild_if_exhausted` ⇒ 可能在在途轮未完时 `rebuild_session → old.stop()` 掉其 Client）；且 `Deadline` 落 `_ => exhausted += 1`（预算中止 ≡ 阶梯走完失败，连续几轮会把会话推向 REBUILD）未登记 | **已回写**：`merge_until` 返回 `(rc, 是否等到)`；等待到点**跳过**重建决策；`Deadline` **不计入** `exhausted`（两处埋点显式排除）；D12 + §5.2 登记 |
| **C5** | 中 | F7b 写「accept 线程 spawn 失败由 `accept_loop` 入口置位」——spawn 失败时 `accept_loop` 根本不运行 ⇒ 标志位永远置不上（假状态原样保留） | **已回写**：F7-4 置位点改到 **`start()` 的 spawn 点**（检 `Result`）+ `listen_path` 返回 `None` 处；异步窗口写明为已知形态 |
| **C6** | 中 | F8e 漏第三条消费面 `daemon/hosts.rs:634`（`host reach`，预算 3.5s 是 spec MUST；池饥饿 ⇒ 端点展开空 ⇒ 把可达主机误报不可达）；进程级单桶让后台刷新可饿死用户可见路径；CAP 依据未写 | **已回写**：F8e 改**分档令牌池**（Critical 4 = 建会话 + daemon reach / Background 4 = 巡检刷新）；获取耗时扣进调用方预算；池是类型（默进程单例）；残余「有界地失败」如实登记；D11 + §5.2/§5.3 |
| **C7a–j** | 低（10 条文档记账） | a 计数不一致（12→**14** 中危、低危 13；预告 §5.1/§5.2/§5.3 计数）；b 「`PfState::snapshot()` 已是唯一组装点」不实；c CLI 第三份目标文案且与 Go 两处不等；d F2-3 可选 status 小修未登记且 `status(&self)` 拿不到域句柄；e F7-3 措辞易被读成删除既有 `1611-1615`；f F8d 波及面低估（`session/mod.rs:635` / `tun_exec.rs:943` 必然编译错）；g `dial_with_recover` 单测预算 ≤ `FIRST_TRY` 会假绿；h 服务域 patrol `catch_unwind` 落空行为未定；i tier 页面提示与 rc 日志文案永久失义；j tier 行号精修（`225-239`/`207-218`/`196-200`/`1102-1103`/`1116`/`1010`） | **全部并入 v3**：a 文档头 + §6.1 回填；b F1-2；c F1-5 + §0.3 N7；d F2-3（含签名说明 + 「不做则登记」）；e F7-3；f F8d；g F3-3 测试注意；h F7-2；i §7-1；j §0.2 各条目行号 |
| **D3** | 中 | **新引入**：F6b 的 detach 使 `fully_stopped` **弱于** Go 的 `isDone()`（Go 的 done 在 `bridge.Stop→sess.Close→cache.Save` 真做完后置位）⇒ 替换后旧引擎线程可能仍活（同钥双引擎窄窗）——正是 `service_exec.rs:213-214`/`service_op.rs:8-9` 注释所依赖的保证被削弱处 | **已并入 + 登记**：D13 + §5.3 + §7-6（缓解论证：`Cmd::Stop` 已投递、新引擎新 UDP 口、出口按 `peer_id` 覆盖注册）；替代方案（加「引擎未必已死」位）已写明但**不采**（保持门简单） |

**D. 评审者对五个复核重点的直接回答**：D1 段枚举**完整无第六段**（`set_state`/`current()`/`stop.swap` 都是短临界区），但段内各有洞（C1/C2），且提醒 §7-4/§7-5 已把隧道域 `Finish::drop→c.stop()`、`request_stop→c.stop()`、`rebuild_session→old.stop()` 登记为残余（**v3 已在 F6-4 加范围声明**）；D2 无第二处误映射，新发现的只是记账/重建（C4）；D3 `start`/`status`/`stop` 对外结论自洽（`fully_stopped` 只置 true 不清零无害），唯一实质隐患 = detach 窗口（D3 本条）；D4 三条路径都成立但**实现须先扣获取耗时再传 `recv_timeout`**，且 `#[cfg(test)]` 对 `tests/` 不可见 ⇒ 建议改 `feature="test-seams"`，池形建议**类型化**（v3 采纳 C6/D11）；D5 拆宏能隔离 `PeerId`，2 处必然编译错已点名；D6 缝成立（`synthetic_failed_for_test` 构造 ≈40 行；**但 `ServiceExec::start` 硬编码 `Session::start`，须加 `#[cfg(test)]` 工厂**——v3 已补）；D7 口径与覆盖基本齐，两处登记缺口（C7a/C7d）已补；**越界检查：未越界** ✅。**D8 明确「看过，没发现问题」的面**：F4、F5、F8a/F8b/F8c 的现状定性、F3b 替换清单（另行穷举确认完整）、全部「Go 同形/偏离 Go」与「tier 会怎么渲染」的断言、复验方法学。

**E. 第二轮总体结论（原文要点）**：**v2 不能原地过门，但只差一轮「回写式」小修（v3），不需要第三轮评审**；放行条件 = C1/C3/C5/C6 **必须先回写文档**；C2/C4/C7/D3 可并入实现棒首件但**须登记**；**不建议把 C1/C3/C5 带进实现**（它们正是「照抄一个已知不成立的声明 / 一个置不上的标志位 / 一个会竞态的伪码」）。

### 6.7 第二轮逐条处置表

| 编号 | 级别 | 处置 | 落到 |
|---|---|---|---|
| C1 | 中 | **认同并已回写**（段 4 `try_lock` 快跳 + 测试） | F6-4 段 4；§4.2 F6 行 |
| C2 | 中 | **认同并已回写**（收割线程收口 fd + 不变式 + 测试 + 残余登记） | F6-4 段 5；§7-6 |
| C3 | 中 | **认同并已回写**（三分支 + 实例入槽 + 身份守卫 + 两项测试） | F2-1；§4.2 F2 行 |
| C4 | 中 | **认同并已回写**（`merge_until` 返回「是否等到」；等待方到点跳过重建；`Deadline` 不计耗尽 + 登记） | F3-2/3；D12；§5.2 |
| C5 | 中 | **认同并已回写**（置位点在 spawn 点） | F7-4 |
| C6 | 中 | **认同并已回写**（分档池 + 扣时 + daemon reach 入 Critical + 残余如实） | F8e；D11；§5.2/§5.3 |
| C7a | 低 | **认同**（计数回填） | 文档头；§6.1 |
| C7b | 低 | **认同** | F1-2 |
| C7c | 低 | **认同**（可选顺手项 + 登记兜底） | F1-5；§0.3 N7；§7-13 |
| C7d | 低 | **认同**（可选小修 + 签名说明 + 不做则登记） | F2-3 |
| C7e | 低 | **认同**（明示既有 1611-1615 不动） | F7-3；§5.2 |
| C7f | 低 | **认同**（两处必然编译错入设计） | F8d |
| C7g | 低 | **认同**（预算 > `FIRST_TRY` + 断言桩被调用） | F3-3 测试注意 |
| C7h | 低 | **认同**（落空 = 记行 + 不改域态） | F7-2 |
| C7i | 低 | **认同** | §7-1 |
| C7j | 低 | **认同**（行号已按复核值订正） | §0.2 各条目；§0.4-8 |
| D3 | 中 | **认同并入 + 登记**（不加位；缓解论证写明） | D13；§5.3；§7-6 |
| A1/A2/A3 复核意见 | — | **全部接受**（H1 部分整改的两处反例 = C1/C2 已回写；C3/C4 已补） | 同上 |

### 6.8 过门结论（v3 最终）

- **两轮门条均满足**：高危必改（H1/H2/H3 → v2 改，第二轮的两处反例 C1/C2 与 D3 → v3 改/登记）；中危（14 条）并入；第二轮新发现（C1–C6 必改/并入、C7 记账、D3 窗口）**全部处置**。
- **放行条件勾销**（第二轮 E 节要求）：C1 ✅（F6-4 段 4）／C3 ✅（F2-1 三分支 + 入槽 + 守卫）／C5 ✅（F7-4 置位点）／C6 ✅（F8e 分档池 + daemon reach）；可并入实现棒首件但须登记者：C2 ✅（F6-4 段 5 + §7-6）／C4 ✅（F3 + D12 + §5.2）／C7a–j ✅（全部并入）／D3 ✅（D13 + §5.3 + §7-6）。
- **结论：设计门通过，可进实现棒**（v3 自评；第二轮已明确「只差一轮回写式小修、不需要第三轮评审」，本稿即该轮回写）。
- **实现棒须遵守的三条硬约定**：① **不得**把 C1/C3/C5 的旧写法带进代码（照 v3 回写稿实现）；② 判据/数值/契约面变更与代码**同批 commit**（§5 草稿→正式登记）；③ 实现期若发现设计与代码现状新矛盾（或 v3 的某条不可实现），**不得静默降级**——在 `docs/reviews/QF.md` 记偏差并回主会话。

---

## 7. 不做与移交登记（防「静默漏做」）

| # | 项 | 出处 | 处置与理由 |
|---|---|---|---|
| 1 | **实装端口转发监听器**（§2 的 B 方案）+ **tier 侧连带失义文案** | 审计 P0 后半；设计门 5.2 / C7i | **挂账为独立功能项**且**本批必办登记**：在 `docs/REVIEW-ROADMAP.md` 增行；**知情项**：落地前 tier `port-forwarding` spec 的 SHALL 处于已知不达标状态；tier 页面 `PortForwardsPage.ets:484` 的「立即重连兜底」提示与 `TierVpnExtensionAbility.ets:1010` 的 rc 日志文案随 F1 永久失义 ⇒ 一并写进 tier 建议 |
| 2 | **`tun_attach`/`tun_stop`/`service_stop` 的 async 壳** | 审计 P1 条目 5 | **移出本批 → tier 触点**（`Index.d.ts` 三处同步符号 + `tailcat_napi.cpp` 的 `napi_create_async_work` 先例）。建议条目：`clientCoreTunAttachAsync` / `clientCoreTunStopAsync` / `clientCoreServiceStopAsync`（core 侧零改动即可支持） |
| 3 | **状态快照分层缓存** | 审计 P2 | **已由 Q-I 前段裁决不做**（`QI-design.md` §2 F8/§6）⇒ 只登记，不重开 |
| 4 | **服务域 `rebuild_session` 无期限** | 本批 v2 复验（设计门 1.2） | 登记为残余：重建是必要自愈动作，本批不设界（设界会与阶梯/桥预算产生新叠加语义）；**挂 Q-G** |
| 5 | **`tun_stop` 世代收尾 4s 串行上界**（2s 派生线程 join + 2s 桥 live 等待 > `STOP_WAIT=3s`）；同族：隧道域 `Finish::drop→c.stop()`、`request_stop→c.stop()`、`rebuild_session→old.stop()` **不在 F6-4 的五段预算内** | 本批复验 + 设计门 D1 | 登记为残余（极端形态走 -2 强制放锁）；F6-4 已加**范围声明**；修它动世代收尾时序 ⇒ 归 **Q-G** |
| 6 | **`Client::stop_within` 到点 detach 的窗口** | F6-4 段 5；设计门 C2/D3 | 登记：① 引擎线程可能存活到自行退出（由收割线程 join 后关 `wake_wr` 收口）；② `fully_stopped` 弱于 Go `isDone()` ⇒ 替换窗口内旧引擎线程可能仍在（缓解：`Cmd::Stop` 已投递、新引擎新 UDP 口、出口按 `peer_id` 覆盖注册） |
| 7 | **`Client::read` 的无界阻塞**（桥泵数据面） | F3b 说明 | 不纳入本批有界化（数据面阻塞语义；pump 不参与收工 join）；引擎卡死时 pump 驻留持流为已知残余 |
| 8 | **`Secret` zeroize 残余**：`Psk`、boringtun 内部 PSK 副本、`CoreConfig` 移交引擎后的副本 | F8d | 登记（引擎生命周期内必然存在；擦除需 boringtun 侧支持） |
| 9 | **`diag_fd_secs` 无消费** | F8b | 保留字段 + 登记（删字段会动 `TunConfigJson` serde 面/Go 配置对齐） |
| 10 | **`session`/`facade` 以外同族 `expect("…中毒")`**（`wgcore` 余项 / `speedtest` 6 / `engine` 6 / `ddnscheck` 4 / `domain_eps` 3 / `stackb` 2 / `dnsproxy` 2 / `speedtest_server` 1）+ `EndpointCache::save` 的 2 处逻辑不可达 `expect` | 本批全仓扫 | 本批做 **session + recover + `tun_exec` + `wgcore::Client` 面**；其余按域登记（Q-G/Q-H 面优先） |
| 11 | **`stats_loop` 的 fd 快照基线行**（Go 有） | F8b | 不补：OHOS 沙箱 fd 快照受限（既有注释口径） |
| 12 | **无自愈巡检残余**（两域 patrol/hint/save 等派生线程 spawn 失败后只记行） | F7-3 | 登记（进程线程资源耗尽极端形态；不引入 `unhealthy` 以免触发无谓整套重建——D6） |
| 13 | **CLI `cmd_portfwd` 的目标文案第三份拷贝**（与 Go 两处不等） | F1-5 / §0.3 N7 | 低优先：做则复用 `pf_target_text`；**不做则本行即登记**（CLI 测试动词，非判据行） |
| 14 | **F2-3 的可选 status 一致性小修**（槽空 + 域 Failed ⇒ 输出 failed+原因） | 设计门 C7d | 做则登记契约面变化（签名 `status(&self, domain)`）；**不做则本行即登记**（明确「不做」，不得静默） |

---

## 8. 实现注记（第 2 棒回写，2026-10-08；与设计正文不一致处以此节为准）

> 约定同 Q-E：设计正文不改写，实现期的偏离/补充集中记在本节（每条给「设计原文 → 实现」）。

1. **F2-1 三分支的落地形态**：① stopping 竞态分支**逐字保留**（含 `return` 语义）；② Ready 发布；
   ③ 硬失败 = 记行 → `bridge.stop()` → `s.stop()` → **失败实例入槽** → `mark_fully_stopped()`
   → `slot_is_same` 守卫后写域态。**与设计伪码的一处正确偏离**：入槽**先于** `mark_fully_stopped`
   （设计伪码把置位写在入槽前）——这样并发/后续 `stop()` 的 2s 暖机轮询能立刻取到会话（有单测
   断言 `stop()` < 1.5s 返回）。
2. **F2 依赖的 `Session::stop()` 前提修正**：设计假定「failed 终态保留」成立，但 HEAD 的
   `set_state(Stopping)` 在读取之前 ⇒ `was_failed` 恒 false、分支**恒不可达**（失败原因必被 Idle
   覆盖）。实现：入口先读（决定是否写 Stopping），**收工末尾再读一次**（收工窗口内巡检/重建新产生的
   Failed 不得被 Idle 覆盖——保留 HEAD 的窗口语义）。已在 `INTEROP-CRITERIA.md` §5.3 登记。
3. **F2-3（可选小修）已采**：`ServiceExec::status(&self, domain)`（唯一调用点 `facade/mod.rs`）；
   槽空 + 域 Failed ⇒ `{"state":"failed","reason":…}`。**tier 可见后果**已登记（`SERVICE+FAILED`
   ⇒ `BRIDGE_ACTION_HEAL_HOST`：一次暖机失败后 App 自动再拉起一次服务会话）。
4. **F3 日志文案**：设计 §4.2/§5.2 草稿写 `RECOVER 预算耗尽（起跑=%s，原因=%s，预算 %v 已用尽）`；
   实现与正式登记为 **`RECOVER 预算耗尽（起跑=%s，原因=%s，预算已用尽）—— 放弃等待`**（不渲染
   `%v` 数值——预算量已在调用方语义里，行文从简）。
5. **F6-4 段 5 的测试落点**：detach + 收割线程收口 fd 的覆盖放在 `wgcore::tests::
   stop_within_detaches_and_reaper_closes_wake_fd`（需 `Client` 私有字段访问；`session` 侧只覆盖
   正常路径 + 重入）；**段④残余**（拿到锁后的落盘 I/O 无期限）已登记（代码门 ②-2）。
6. **F7 同族补充（设计清单未列）**：`homeway-svc` **会话线程体**也套 `catch_unwind`（panic 会让
   线程死掉、域态永久停在 `Starting` ⇒ `service_start` 恒 0）；落空 = 记行 + 桥停 + 清槽 + 域 Failed
   （有单测）。桥 `unavailable` 用普通 `bool`（全部读写都在 `Mutex<HostInner>` 内——代码门 ⑥-1）。
7. **F8d 波及面**：`bind::RegCtx.secret` 在本批之前已是 `Secret`（设计按 `[u8;32]` 预估——已更早
   收敛）；实际改动 = 宏拆分 + `GenRun`/`Shared` 字段类型 + 三处 `.clone()`（`wgcore::Client::start`
   的 `RegCtx`/`CoreConfig`、`build_client`、`gen_loop`）。
8. **F8e 额度归属定死为「在飞解析」**：`Permit` 随 worker 闭包（调用方超时返回**不**归还额度）——
   这才使登记文字（「worker 线程数分档上限」）与设计残余（「黑洞下该档 4 枚卡死线程可被占满 ⇒
   有界地失败」）成立（代码门 ②-1 整改）。
9. **顺手项（记账）**：① facade 四文件（`service_op`/`demand`/`events`/`files_op`）的三份私有
   `LockUnpoison` trait（`.lup()`）收敛到 `syncutil::lock_unpoison`（~40 处；F6-1「单源」目标的
   自然延伸，设计写的是「facade 既有调用点零改动」——实际改了）；② CLI `cmd_portfwd` 的
   `--map L:<ip>:0` 实际拨号端口由 0 改为 L（与 Go 桌面 facade 同义，登记在 §5.3）；
   ③ `daemon/hosts.rs` 一行 lane 参数（F8e 的 Critical 档）。
10. **文件集口径**：改动 = 19 改 + 2 新（含 `syncutil.rs` 与 `lib.rs` 的模块注册；`docs/` 两处），
    派单写的「14 + 1」只数了主要面。
11. **代码门整改轮追加**（r14 → r15；均记入 `docs/reviews/QF.md` §4.2/§4.5）：F8e 的额度归属定死为「在飞解析」
    （`Permit` move 进 worker；`ResolverLimiter` 内部 `Arc<LimiterInner>` ⇒ `Permit` 无生命周期、可 move 进线程）
    + 抽可注入主体 `lookup_host_with(limiter, host, budget, lane, resolve)`（测试缝：真钉「调用方超时后仍占额度」）；
    `Session::stop()` 的两处置态改 `Shared::set_state_unless_failed`（同一临界区判定 + 写，消两次读—写窗口）；
    `homeway-svc` 线程 panic 落空分支顺序对齐 `Err(e)`（写域态 → 停桥 → 清槽）；`lookup_host` 的零预算归因区分两态。
