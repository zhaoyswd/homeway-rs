# Q-F-B（portfwd 实装监听器）设计文档

> **v3（2026-10-08）**：v1 过设计门第一轮（dsh `r25.snY8bb`，**exit=0**）= 骨架无问题、**5 条必改** + 12 条中低危；
> v2 = 第一轮整改稿；第二轮复校（dsh `r26.vcqR9Q`，**exit=0**）= 12 条整改**逐条「已闭合」**，但新暴露
> **4 条实现前必须钉死**（N-4 `target_ip` 非法可达缺口 / N-1 accept 致命错误后状态与 fd 语义矛盾 /
> N-2 阀位置与线程创建上界 / N-3 ack 预算自相矛盾且超时路径无测试缝）+ 1 条数值论证不完整（N-5），
> 判 **「有条件通过」**。**v3 = 第二轮整改稿**：N-1/N-2/N-3/N-4/N-5 全部钉死 + N-6…N-12 一并收口（§8.6）。
> **v2→v3 关键改动**：① Fatal accept 错误 ⇒ 该条状态**转 `failed`**（空码 + 精确 err；Go 是「留 listening、fd 不关」——
> Rust 的 fd 单属线程必然释放 ⇒ 必须改状态，否则复现本批要消灭的谎报）；② 阀与准入**移回 accept 线程**（Go 形）
> + RAII `FlowGuard` 结构保证回退（spawn 失败不泄漏）；③ ack 等待统一为**共享 400ms**（消除 per-entry/共享 两口径）
> + 超时路径**确定性测试缝**；④ F8 防御面补 `target_ip` 非法（可达且会复现「监听中但连不上」）；
> ⑤ 阀值 1024 → **256**（内存口径可数：引擎缓冲最坏 2×256 MiB）+ conn 线程栈入账。

- **批次**：Q-F-B（真源 = `docs/REVIEW-ROADMAP.md`「Q-F-B」行 + `QF.md` §7 交接块 + `QF-design.md` §2.4 + `AUDIT-2026-10-07.md` Q-F 条目）。
- **需求真源（只读）**：`~/Documents/projects/tier/openspec/specs/port-forwarding/spec.md`——本批落地后其
  「端口映射的建立与访问」SHALL 由**已知不达标**转**达标**；「映射状态可见」的失败码 MUST 同时转达标（§3）。
- **基线**：HEAD `5f1c7b0`（Q-J 收口），工作树干净。
- **本棒实跑证据**：① 交接块/轮廓逐条回源码重定位——§0.2；② Go 只读 oracle 回读（`app_portfwd.go` 全文、
  `tunmode.go` 的 `maxTCPFlows`/调用点/`defer` 序/`dialTimeout`、`hostsession/config.go` 的 `DialMs`、
  `internal/wgcore/transport.go` 的 `DialTCPPort`=裸拨、`pkg/netpipe/netpipe.go`、`app_bridge.go` 的独立闸）；
  ③ tier 只读消费面回读（`PortForwardRules.ets`/`HostStore.ets`/`PortForwardsPage.ets`/`TierVpnExtensionAbility.ets`/`Vocab.ets`）；
  ④ 词表门面（`check-vocab.sh` 缺席表 + `vocab_dump.rs`）；⑤ `fixtures/` grep `portForwards` = 0 命中；
  ⑥ 复用面回读（CLI `cmd_portfwd`、`bridge_host::accept_loop`/`pump`/`set_dial`、`daemon::carriers::rst_close_tcp`、
  `files_server::classify_accept_err`、`wgcore/stackb.rs` 的 `TCP_BUF`/环回拒绝/默认路由）；⑦ 基线门复跑（§0.5）。
- **不越界**：不改 tier / `~/Documents/projects/homeway` / `baseline/`；不碰两台生产出口；不发 tag/Release/PR；
  Q-F-B 收口（ROADMAP 状态回填、commit）由主会话统一做。

---

## 0. 复验（证据先行）

### 0.1 方法

1. 交接块（`QF.md` §7）与轮廓（`QF-design.md` §2.4）→ **回源码重定位**（行号按 HEAD `5f1c7b0` 重取）。
2. 「语义/文案/返回码」断言一律追 **Go oracle + tier 消费代码**两处再定性。
3. 并发/生命周期断言沿调用链确认：**谁关 fd、谁 join、锁持多久、停止位怎么传**。
4. 词表/夹具面以 `check-vocab.sh` 缺席表与 `fixtures/` 实际内容为准。
5. **两轮设计门意见全部回源码复验**（§8.3/§8.6 逐条给取证），不接受「评审说得像」的结论。

### 0.2 交接块与实现轮廓逐条复验表

| # | 交接块/轮廓原文（摘要） | 真伪 | 现行位置（HEAD `5f1c7b0`） | 结论 |
|---|---|---|---|---|
| 1 | `GenRun` 增 `pf: Mutex<PfRuntime>`（`lns`/`states`/`accepted`/`fails`） | ✅ 成立（今日只有表，无运行态） | `tun_exec.rs:372-373`（`pub pf_rules: Mutex<Vec<PortForwardRule>>`）；构造 `:906-928`（`:922` 装表）；`synthetic_for_test` `:730` | 按轮廓落地（§1-F1） |
| 2 | 整表替换：停旧 + 清状态 → 逐条 bind：成功 ⇒ `listening` + accept 线程；失败 ⇒ `failed(err)`（`bind_failed`）；返回 0 | ✅ 成立（现无执行体） | 热替换 `tun_exec.rs:675-685`（只存表恒 -1）；状态组装 `:688-692`；`PfState::listening/failed`（`portfwd.rs:150-171`） | 落地；原子性/端口释放见 §1-F1 + §2-D3（两轮门整改） |
| 3 | accept 线程：并发阀 → `conns += 1` → `session_connect(&run, target_port, 15s)` → `into_halves()` → 复用 `bridge_host::pump`；目标语义走 `dial_target()` | ⚠️ **成立但五处需订正** | `session_connect`：`tun_exec.rs:205-223`（硬编码 `SERVER_TUNNEL_IP`）；`pump`：`bridge_host.rs:791`（私有；**EOF 有流量即记行**）；`WriteHalf` impl 3 个（`bridge_host.rs:58/65`、`tun_exec.rs:198`）；`dial_target()`：`portfwd.rs:91-103` | 订正见 §0.3 N1/N2/N3/N6/N7/N12（含两轮门并入项） |
| 4 | 世代收工：`Finish` guard 内与 `bridge.stop()` 同序停全部监听器 | ✅ 成立 | 收工段 `tun_exec.rs:1337-1366`；`Finish::drop` `:978-1021` | 落点：正常路径 `pf.stop_all()` 在 `bridge.stop()` 之前（**均在 client 关闭之前**；Go 的 defer LIFO 实为桥先停——§0.4 订正②）；`Finish::drop` 头部幂等兜底——§1-F4 |
| 5 | 状态面 `portfwd_states` 读真 `PfState` | ✅ 成立 | `tun_exec.rs:688-692` → `portfwd::pf_states`（`:210-218` 恒产 `Unavailable`）；`snapshot()` 唯一组装点（`:187-198`） | 接线后 `snapshot()` 仍是唯一组装点；`unavailable*` 残基待删（§2-D2） |
| 6 | `pf_accepted/pf_fails` 与 `stats` 行接真计数（**行文不变**） | ✅ 成立（今日两处硬编码 0） | `runner_of`：`:761-773`（`:767-768`）；`stats_loop`：`:1962-1965`（内联 `format!`；tick 下限 `STATS_SECS_MIN=5` `:67`） | 接线 + 抽 `stats_line` 纯函数（§1-F3-2） |
| 7 | `code` 死结：实装后 `code` 有真值 | ✅ 成立（**本批只需产出 `bind_failed`**） | `lib.rs:53-74`（仅 `BindFailed`）；`ALLOWED_ABSENT` 两条在册；tier `pfFailText`（`PortForwardRules.ets:207-219`） | §3：**词表门零改动**；「有意偏离」判回**达标**（限定语见 §3） |
| 8 | 安全面：仅 `127.0.0.1`、`listen ≥1024` 且同表唯一、条数上限（建议 8）、并发流阀（Go `maxTCPFlows=4096`）、拨号 15s 期限、世代收工全关 | ✅ 前两项已有；后四项需新增（**阀值不照抄**，§2-D4） | `validate_table`（`portfwd.rs:47-67`，`MIN_PORT=1024` `:18`、`DupListen` `:60-64`）；条数上限无；**客户端核无同类「拒绝型」阀**（桥 `ConnGate` 是挤最老型；files `MAX_CONNS=16`/speedtest 12/intercept 1024 在其它角色面；`stackb.rs:43` 的 `TooManyConns` 是**死变体**）；拨号预算 = `cfg.dial_ms`（`tun_exec.rs:601-605`，缺省 15000 `:79`）= Go `dialTimeout`（`tunmode.go:714` + `config.go:62-63`）✅ | §1-F5/F6 + §4 |
| 9 | 地基清单（`PfState`/`snapshot()`/`pf_target_text`/`dial_target`/`pf_states`/存储面/接线位） | ✅ 全部在 | `portfwd.rs:71-218`；`tun_exec.rs:679-692/767-771` | 直接受益；`pf_states(rules)`/`unavailable*` 落地后失义（§2-D2） |
| 10 | 可复用先例：CLI `cmd_portfwd` | ✅ 成立 | `homeway-cli/src/main.rs::cmd_portfwd`（`Session` `Box::leak` 成 `'static`）；`tools/matrix.sh` RRR 臂 | 复用「链路已通」结论；生产面须另加阀/计数/收工/状态 |
| 11 | 增量估算 ≈150–250 行 + 守卫 ≈+80 + 测试 | ✅ 量级成立（偏乐观） | — | 复验后 ≈300–400 行 + 测试 ≈250 行（§0.4 订正⑤） |
| 12 | tier 连带失义文案「B 批或 tier 批需同步」 | ⚠️ **部分订正** | `TierVpnExtensionAbility.ets:1002-1016`（rc 两分支文案）；`PortForwardsPage.ets:481-497`（`dirty` 兜底提示） | **B 落地即自愈** ⇒ **不需 tier 改动**（§0.4 订正①） |

### 0.3 复验新增项（★ = 第一轮门并入；☆ = 第二轮门并入）

| # | 新增项 | 位置 | 定性 | 处置 |
|---|---|---|---|---|
| **N1**★ | `session_connect` 只拨出口自己 ⇒ 漏 spec Scenario 2 | `tun_exec.rs:205-223` | 功能缺口 | 新增 pf 专用拨号面（§1-F2） |
| **N2**★ | `pump` 私有 + **EOF 日志口径与 Go 不同** | `bridge_host.rs:790-822`；Go `netpipe.go:30-37`（只在非 EOF/非 ErrClosed 错误记行） | 复用障碍 + 日志量 | `pub(crate)` + `label`/`eof_log` 参数（§1-F2-4） |
| **N3** | `WriteHalf` 无 `TcpStream` 实现 | `bridge_host.rs:44-46` | 复用障碍 | 增实现（`shutdown(Write)`） |
| **N4** | tunConfig 路径不经 `validate_table` | `tun_exec.rs:922`；门在 `facade/mod.rs:480-497` | 校验面缺口 | 装配期逐条防御（§1-F8）；**可达性**：`parsePortForwards`（`PortForwardRules.ets:129-150`）只丢 id/端口为 0 的条目、`HostStore.updateForwards`（`HostStore.ets:334-342`，注释自认调用方负责校验）不校验 ⇒ 手改/损坏记录可达 |
| **N5** | `SO_REUSEADDR` 缺失 ⇒ 热替换/重加同端口可能假 `bind_failed` | 无现成 bind helper；`sysfd.rs:64` 可复用 | 可靠性缺口 | `pf_bind`（§1-F6） |
| **N6**★ | 拨号失败 RST 收口；**本仓已有单源** `rst_close_tcp` | Go `app_portfwd.go:231-236`；`daemon/carriers/mod.rs:293-303`（`socks_srv.rs:189/268`、`forward.rs:356/375/426` 在用） | 行为缺口 + 重复造轮子 | 上移 `sysfd` 单源复用（§1-F2-3） |
| **N7** | **Arc 循环**：拨号闭包持 `Arc<GenRun>` ⇒ 世代泄漏 | `tun_exec.rs:930-970` 已有 `Weak` 先例 | 生命周期缺陷 | 注入闭包持 `Weak<GenRun>`（§2-D7） |
| **N8** | accept fd 归属 / 端口释放 / ABA | `bridge_host.rs:557-611`（桥不 join） | 并发正确性 | fd 单属 accept 线程 + **退出 ack**（§1-F1-3） |
| **N9**★ | **阀语义**：Go `maxTCPFlows` 的自证注释（`app_portfwd.go:209`「与 **gVisor 流共用**」）已陈旧——`tcpFlows` 读写只在 `app_portfwd.go:210/211/221`、应用侧 netstack 已退役（`tunmode.go:718`）、桥明文不占（`app_bridge.go:98-99`）⇒ **事实上同为 pf 专属阀**，Rust 专属阀 = **同形** | 同上 | 对齐结论（v1 写反） | 三处改写（N9/§4.4/§6.3） |
| **N10**★ | **阀值资源账**：引擎每连接 2×1 MiB 缓冲（`stackb.rs:33` `TCP_BUF`，`:240-241`；无上限、`TooManyConns` 死变体）；出口 `MAX_CONNS=1024`（`server/intercept/mod.rs:46`，`:399` 已按「1024×~1MB≈1GB」登记；`:1051-1066` 超限回 RST） | 同上 | 安全面 | 阀值 **4096 → 256**（口径 = 手机内存预算，§2-D4）+ §4.2 资源账 |
| **N11**★ | **loopback/unspecified 目标**：`stackb::connect` 拒环回（`:236-239`）；tier 视 `127.0.0.1` 为合规；Go 经出口过境重拨能通 | 同上 | 功能缺口（假象复活） | `Remote(环回/0.0.0.0)` ⇒ `ExitPort`（§1-F2-2） |
| **N12**★ | **pf 拨号不应复用 `healing_dial`**（首试失败无条件起 R2 阶梯 + 未节流 RECOVER 行）；Go `pfDial` = 裸拨（`transport.go:195-207`） | `tun_exec.rs:279-322` | 行为偏离 + 放大 | pf = **裸拨** `connect_deadline`（§2-D11） |
| **N13** | 无鉴权（Go 同形） | Go `app_portfwd.go:77-113` | 接受的已知面 | 登记（§4.1/§6.3） |
| **N14**★ | accept 非 WouldBlock 错误处置未定（忙转风险） | 共享件 `files_server.rs:869-919`（Q-E F5 建的 `classify_accept_err`） | 缺陷预防 | 复用分类件（§1-F1-4） |
| **N15**★☆ | 阀/计数归属与回退（v1 描述矛盾 ⇒ v2 挪进 conn 线程 ⇒ **第二轮指出削弱阀的线程上界**） | `bridge_host.rs:594-602/670-691`（RAII 守卫先例） | 缺陷预防 | **阀与准入回 accept 线程**（Go 形）+ RAII `FlowGuard`（§1-F1-5、§2-D14） |
| **N16**★ | 测试面：`stats_loop` 行不可直喂（5s tick + 内联 format）；固定测试端口有互撞 flake 先例 | `tun_exec.rs:67/1944-1965`；flake 表 19990/20001 | 可测性 | `stats_line` 纯函数 + 空闲端口 helper（§1-F9） |
| **N17**★ | 门清单缺交叉 check（新增 libc 面） | `ci.yml` 的 OHOS/musl job + `tools/ci-local.sh` | 流程 | §5.1 显式列出 |
| **N18**☆ | **`target_ip` 非法（非空且非 IPv4 字面量）在旁路可达**，且 `dial_target()` 只在**每连接**求值 ⇒ bind 成功、状态 `listening`、每次连接必失败（只有 `pfFails` + RST、无映射级归因） | `portfwd.rs:96-99`（`BadTarget`）；可达性同 N4 | 功能缺口（与本批主题冲突） | F8 增该形态（§1-F8-3） |
| **N19**☆ | **Fatal accept 错误后状态语义矛盾**：Rust 的 fd 单属 accept 线程 ⇒ 退工即释放端口，而状态仍是 `listening`（Go 是「留 listening、fd 不关」——后果不同，不是同形） | Go `app_portfwd.go:199-207`；本设计 §1-F1-4/§4.4 | 状态诚实性（与本批主题冲突） | Fatal ⇒ 该条**转 `failed`**（迟到失败位；空码 + 精确 err）（§1-F1-4、§2-D15） |
| **N20**☆ | ack 预算两口径（per-entry vs 共享）+ 超时路径无测试缝 | 本设计 §1-F1-3 步骤 2/2b vs §2-D10/§4.2/§6.3 | 自相矛盾 | 统一**共享 400ms** + 可注入 wait 缝 + 强制超时用例（§1-F1-3、§5.2 #3b） |

### 0.4 订正记录

1. **订正 ①（tier 文案）**：交接块称两处 tier 文案「B 批或 tier 批需同步」——**B 落地即自愈**，不产生 tier 需求。
2. **订正 ②（Go 收工序）**：v1 写「Go 顺序：先关监听器」**不准确**——Go `defer t.stopPortForwards()` 先注册、`defer t.bridge.stop()` 后注册 ⇒ LIFO 实为 **桥先停、pf 后停**；两序都在 `closeClientOnce`（`tunmode.go:708/829-834`）之前。Rust 取 **pf 先停**（更早释放端口）并写明理由。
3. **订正 ③（阀语义）**：v1 的「Go 与 gVisor 数据面共用阀 ⇒ 值同语义异」**不成立** ⇒ 三处改写为「Go 注释陈旧（锚 `app_portfwd.go:209`）；读写只在端口转发面 ⇒ **同形**」。
4. **订正 ④（spec 达标口径）**：`bind_failed` 真值满足失败码 MUST；`dial_failed`/`invalid_target` 保持登记保留 ⇒ 词表门/声明集零改动。
5. **订正 ⑤（增量）**：≈300–400 行 + 测试 ≈250 行。
6. **订正 ⑥（引用）**：tier 行号按复核值改（`pfFailText` = 207-219；`pushPortForwards` = 1002-1016；`HostStore.ets:334-342`；`EVENT_TICK_MS=5000` `:61`、`STATUS_POLL_MS=250` 仅暖机循环 `:120/843`）——**v1 的「250ms 轮询既有路径」废弃**，改为「既有状态通道：`pfSet` 后扩展主动 `readTunStatus` 重采样回推 + 事件泵（5s）；App 页读内存缓存」，「不新增轮询路径」结论不变。
7. **订正 ⑦（计数）**：`pf_rules` 全仓引用 = **7 处 = 生产 4（`:373/681/771/922`）+ 测试 3（`:730/2140/2191`）**（v2 曾写「生产 6 + 测试 4」）。

### 0.5 基线门（本棒实跑）

| 门 | 命令 | 结果（实测） |
|---|---|---|
| 单元测试 | `cargo test -p homeway-core --lib` | **642 passed / 0 failed / 4 ignored**（24.13s，exit 0；第二轮评审独立复跑同计数） |
| 词表门 | `zsh tools/check-vocab.sh` | **PASS**（声明 5 单元 / 26 值；缺席表 4 项在册——`portfwd/err/{dial_failed,invalid_target}` 本批保持不动） |

---

## 1. 修复清单（F1–F9）

> 每条给：方案 / 涉及文件 / 风险 / 测试 / 判据行影响。**实现纪律**：`AGENTS.md`「地道 Rust」条 +
> Q-F 三条硬约定（不把旧写法带进代码 / 判据变更同批 commit / 发现矛盾不得静默降级）。

### F1（P0）真监听器运行时：`PfRuntime` + 两阶段原子替换

**方案**

1. **落点**：`facade/portfwd.rs` 增 `PfRuntime`（规则/校验/状态同文件）；**不依赖 `GenRun`**（拨号与停止位全注入）⇒ 纯回环可测。
2. **结构**（单源、无环）：

   ```rust
   pub struct PfLimits { pub max_rules: usize, pub max_flows: u64 }              // 生产 = 8 / 256（§2-D4）
   pub struct PfCounters { accepted: AtomicU64, fails: AtomicU64,
                           flows: AtomicU64, flow_rejected: AtomicU64 }         // 全部 AtomicU64（类型一致）
   pub struct PfRuntime {
       install: Mutex<()>,                    // 串行化 install/stop_all（长动作不持状态锁）
       inner: Mutex<PfInner>,                 // 短临界区：单次换入 / 读状态
       counters: Arc<PfCounters>,
       stop: Arc<AtomicBool>,                 // = GenRun.stop 克隆（世代收工位）
       dial: PfDialFn,                        // Arc<dyn Fn(SocketAddrV4, Duration) -> io::Result<Box<dyn BridgeStream>> + Send + Sync>
       logf: Logf, budget: Duration, limits: PfLimits,
       wait_ack: AckWaitFn,                   // 【测试缝】默认 = 有界等 Receiver；测试注入「永不 ack/慢 ack」
   }
   struct PfInner { rules: Vec<PortForwardRule>, states: Vec<Arc<PfState>>, lns: Vec<PfListener> }
   struct PfListener { listen: u16, stop: Arc<AtomicBool>, acked: mpsc::Receiver<()>, handle: JoinHandle<()> }  // **不持 listener fd**
   ```

   - **锁纪律**：runtime 内全部锁走 `syncutil::lock_unpoison`（毒锁不 panic——`Finish::drop` 可能取到；Q-F F6 纪律）；
     补「持锁线程 panic 后 `stop_all`/`Drop` 仍能收工」单测。
   - **拨号闭包持 `Weak<GenRun>`**（§2-D7）；`upgrade()` 失败 ⇒ 按拨号失败收口（`fails += 1` + RST + 记行）。
   - `PfState` 的**可变位只有两处**：`conns: AtomicI64` 与 **`late_fail: Mutex<Option<String>>`**（迟到失败，见 F1-4）；
     `snapshot()` 优先读 `late_fail`（有值 ⇒ `state="failed"`、`err=late`、`code=""`）——两轮门后定死的唯一形态（§2-D15）。
3. **install（整表替换）**——`install` 锁内全程串行；**状态面对外只看到「全旧」或「全新」**（Go 持 `pfMu` 的等价语义，§2-D3）：
   1. 短锁**只 `take` 旧 `lns`**（`rules`/`states` **原地保留**至单次换入——否则窗口内 `states()` 返回空表 = 第三态，tier 显示「启动中…」）；逐条置旧 `stop`；
   2. **只等「端口被新表复用」的旧监听器**的 **ack**（accept 线程 drop `Arc<TcpListener>` 后投 ack = 「fd 确已关」的可观测证据）。预算 = **全部待等监听器共享 400ms**（单一口径；不做 per-entry 预算——第二轮门 N-3）；
      `Err(Disconnected)` 视为「线程已退出 = fd 已关」（不是超时；第二轮门 N-12②）；其余旧线程 detach 自退。
   2b. 预算耗尽仍有未 ack 的端口 ⇒ **不静默**：对这些端口做有界重试 bind（**3 次 × 50ms**），仍 `EADDRINUSE` 才记 `bind_failed`，
       并**额外**记行标注 `port-forward: {listen} 旧监听器未在预算内释放——已重试 bind 仍失败`（**不允许把内部竞态伪装成用户可见的「端口被占用」**）；
   3. 逐条 `pf_bind(127.0.0.1, listen)`（F6）→ 成功造 `PfState::listening` 并 spawn accept 线程（fd 单属该线程）；失败造 `PfState::failed(err)`（`code = bind_failed`）+ 记行（Go 行文）；
   4. **spawn 失败 ⇒ 该条改 `failed`**（err = `线程启动失败（{e}）`）+ 关监听器 + 记行（不静默、不谎报 listening）；
   5. 短锁**一次性换入** `{rules, states, lns}`（单次 swap ⇒ 无半表）；
   6. 记行：成功条目 Go 行文 `port-forward: 127.0.0.1:{listen} -> {target} 监听中`；停旧 `port-forward: 已停止全部监听器（{n} 个）`；
      detach 分支另加 additive 行（`port-forward: 旧监听器 {listen} 未在预算内退出——已在后台自退`）。
   - **无 panic 结构（第二轮门 N-12⑤）**：步骤 1–5 之间不得有可 panic 的调用（bind/spawn 走 `Result` 分支、日志闭包不 panic、
     锁全走 `lock_unpoison`）⇒ 「states 仍 `listening` 而 `lns` 已空」的第三态**不可达**；万一仍 panic（如分配失败 = abort，不在可处置面），
     由世代 `Finish::drop → stop_all` 清 states 收口（窗口止于世代收工）。
4. **accept 线程**：`poll(2)`（≤50ms）→ `accept()`（非阻塞）→ 准入（F1-5）→ spawn conn 线程；每轮先查 `self.stop || gen_stop` ⇒ 退出（drop `Arc<TcpListener>` + 投 ack）。
   - `poll` 而非纯 sleep：pf 是用户可见延迟路径（连接就绪即接）；50ms 只是停止延迟上界。
   - **accept 错误复用共享分类件** `files_server::classify_accept_err`（`pub(crate)`）：`Retry`（WouldBlock）轮询 /
     `Backoff`（EMFILE/ENFILE/ENOBUFS/ENOMEM/ECONNABORTED/EPROTO/EINTR）退避 200ms→1s + 节流记行（`<=3 || %100`）/
     `Fatal`（EBADF/EINVAL/…）⇒ 记行（`port-forward: accept 致命错误（{e}）——该条映射不可用`）+ **该条状态转 `failed`**
     （`late_fail` 置位，空码 + 精确 err；见 §2-D15）+ 退出循环。
     **订正（第二轮门 N-1）**：v2 写「Fatal 后状态保持 `listening`——Go 同形」**不成立**——Go 的 `ln` 仍被 `t.pfLn` 持有
     （监听 socket 不关、端口仍绑，只是无人 accept），Rust 的 fd 单属 accept 线程 ⇒ 退工即**释放端口**；
     两边后果不同，而「状态 `listening` + 端口已释放」正是本批要消灭的谎报 ⇒ 本批取「转 `failed`」并**登记为偏离 Go**（§6.3 注记 A）。
5. **准入（阀）在 accept 线程**（Go 形；第二轮门 N-2/N-15）：`flows.fetch_add(1) > max_flows` ⇒ **`fetch_sub(1)` 回退** +
   `flow_rejected += 1` + 关连接（**不回 RST、不计 `fails`**）+ 节流记行（Go 行文，**逐字**：`port-forward {listen}: 并发流已达上限 {max}，拒绝（累计拒绝 {r}）`）；
   否则建 **RAII `FlowGuard`**（同时管 `conns += 1`、`flows += 1`、`accepted += 1`）→ spawn conn 线程并把 guard move 进去。
   - **spawn 失败 ⇒ guard 在 accept 线程 drop ⇒ 三条计数自动回退**（结构保证，无逐路径补丁——评审 ⑦-7.3/N-6）；
     连接关闭 + 记行。**阀先于线程创建** ⇒ 阀同时是「在册流」与「线程创建」的上界（Go 同形，第二轮门 N-2 整改）。
   - conn 线程：拿 guard → 裸拨（F2）→ 成功：`into_halves()` + `try_clone` 本地 fd，**spawn 两枚泵线程**（`stack_size(128 KiB)`，
     conn 线程自身也显式 128 KiB），guard 以 `Arc` 分给两泵——**最后一枚泵结束才回退计数**（`bridge_host::GateGuard` 同形）；
     失败：`fails += 1` + **RST** + 节流记行（`<=5 || %20`），guard 随线程结束 drop（`conns/flows` 自动回退）。

**涉及文件**：`facade/portfwd.rs`、`facade/tun_exec.rs`、`facade/bridge_host.rs`、`sysfd.rs`。

**风险**：中。替换原子性、端口释放与计数回退是本批最复杂面；缓解 = 只 take `lns` + ack + 有界重试 + RAII 守卫 + 全注入测试。

**测试**：§5.2 #1–#6/#11/#11b。**判据行影响**：无编号行；契约/数值/观测行登记见 §6。

### F2（P0）拨号（裸拨 + 目标映射）与泵接线

**方案**

1. **裸拨（Go 同形）**：`session_connect_target(run, dst, budget)` = `client.connect_deadline(dst, budget)` → `SessionStream::shared(client, id)`；
   **不复用 `healing_dial`**（避免常态拒绝触发 R2 恢复动作 + 未节流 RECOVER 行）；桥路径 `session_connect` 保持 `healing_dial`。
   恢复能力不因此丢失（`patrol` 派生线程独立驱动恢复，第二轮门已核）。
2. **目标派生**（`dial_target()` 扩面，`portfwd.rs:91-103`）：
   - 空 `target_ip` ⇒ `ExitPort(target_port)`（⇒ `SocketAddrV4(SERVER_TUNNEL_IP, p)`）；
   - `target_ip` 为**环回**（`is_loopback()`）或**未指定**（`0.0.0.0`）⇒ **`ExitPort(port)`**（语义 =「出口本机」——Go 经隧道过境重拨到出口的 127.0.0.1 同效；Rust 栈显式拒环回 ⇒ 不映射就是「监听中但连不上」）；
   - 其余 IPv4 ⇒ `Remote(SocketAddrV4)`；`target_port == 0` 在 IP 分支折为 `listen`（FIX-46 既有；NAPI 门拒 `targetPort=0` ⇒ 仅旁路可达——登记 §6.3）；
   - **非法 `target_ip`**（非空且非 IPv4 字面量）⇒ 该条**在装配期即失败**（F8-3，不 bind），不在每连接阶段才暴露（第二轮门 N-4/N-18）。
3. **RST 单源**：`rst_close_tcp` 从 `daemon/carriers/mod.rs:293-303` **上移到 `sysfd.rs`**（`pub(crate)`；daemon 侧 `pub(super) use` 保住既有三处调用点零改动），pf 复用（不自建第四份）。
4. `pump` → `pub(crate) fn pump(r, w, logf, label: &'static str, dir, eof_log: bool)`：桥传 `("桥泵", true)`（**行文逐字不变**）；pf 传 `("port-forward", false)`（EOF 零日志——对齐 Go `netpipe`；错误仍记行）。
5. **预算**：每连接独立 `budget = cfg.dial_ms`（缺省 15s，Go `context.WithTimeout` 同形）；到点即失败 ⇒ `fails` + RST。
6. **分流独立性（实证）**：拨号 = `Client::connect_deadline`（`wgcore/mod.rs:1206`）→ 栈 B `add_default_ipv4_route(server_tunnel_ip)`（`stackb.rs:207-208`）⇒ 出站全走隧道栈、**不经 TUN 路由表**；监听口是回环（不入 TUN）⇒ spec Scenario 3 成立。

**涉及文件**：`facade/tun_exec.rs`、`facade/portfwd.rs`、`facade/bridge_host.rs`、`sysfd.rs`、`daemon/carriers/mod.rs`（可见性/转出口）。

**风险**：低。**残余**：pf 泵错误行是 Rust 侧新面（登记 §6.2 ⑨）。

**测试**：§5.2 #7/#8/#10。

### F3（P1）状态面接线 + 失义残基清理

**方案**

1. `runner_of`：`pf_accepted/pf_fails` 取 `counters()`；`port_forwards` = `pf.states()`（每元素 `snapshot()`）。
2. `stats_loop`：抽 `fn stats_line(rd, wr, a, f) -> String` 纯函数 ⇒ 行文 `stats: fdReadBytes={rd}B fdWriteBytes={wr}B ｜ pf={a}/{f}`（形态逐字不变）；单测直喂。
3. **删除失义残基**：`PfStateKind::Unavailable`、`PfState::unavailable`、`unavailable_err_text`、`portfwd::pf_states(rules)`、`tun_exec::portfwd_states(rules)`；
   连带改写/删除既有单测（`portfwd::unavailable_state_snapshot`、`pf_states_reports_unavailable_with_target_text`、`tun_exec::portfwd_states_matches_pure_source`），
   并**按实际清单**逐处处置 `pf_rules` 引用（**生产 4：`:373/681/771/922`；测试 3：`:730/2140/2191`** —— §0.4 订正⑦）。
4. `PfState::failed()` 保持 `code = Some(BindFailed)`；**新增** `failed_with(err, code: Option<PortfwdErr>)` 供防御面/迟到失败（`code: None`）。
5. **不新增状态通道**：状态仍只经 `runner_of → tunStatusJSON`（既有通道；`pfSet` 后 App 主动重采样回推 ⇒ 「监听中」当拍上屏）⇒ spec「经现有状态通道推送（不新增轮询路径）」满足。

**涉及文件**：`facade/tun_exec.rs`、`facade/portfwd.rs`。**风险**：低。

**测试**：§5.2 #12/#13/#16。

### F4（P1）生命周期挂点：装表 / 收工 / 热替换 rc 回 Go

**方案**

1. **装配**：`GenRun` 构造时建 `PfRuntime`（注入 `Weak<GenRun>` 闭包 + `stop` 克隆 + `dial_ms` 预算 + limits），表先入 runtime、**不 bind**（未 attached 不监听 ⇒ 状态表为空 ⇒ tier「启动中…」，与 Go「attach 才 `setPortForwards`」一致）。
2. **attach 装表**：`tun_exec.rs:1248-1251`（`stage=Attached`）之后、桥构造（`:1253-1266`）之前 `pf.install(cfg.port_forwards)`（Go 同序）；装表后 stale 复查（`gen` 比 + `stop` 位）⇒ stale 则 `stop_all()` + 记行（不给死世代留孤儿监听器）。
3. **收工**：正常路径在 `bridge.stop()`（`:1360-1364`）**之前**调 `pf.stop_all()`（两序都在 client 关闭之前；Rust 取 pf 先停以更早释放端口——§0.4 订正②）；`Finish::drop`（`:984`）头部再幂等调一次（覆盖 panic/早退）+ **清空 states**（Go `stopPortForwards` 语义）。`stop_all` 的 ack 等待预算 = 共享 200ms（到点 detach；ack 缺位与 detach 均记 additive 行）。
4. **热替换**：`request_port_forwards`（`:675-685`）改：有世代 ⇒ `install` → stale 复查 ⇒ `0`；无世代/stale ⇒ `-1`；`facade::tun_set_port_forwards` 的门**保持不动**（Q-F N5）+ `validate_table` 新增条数上限 ⇒ `-2`。
5. **Noop/Fake 执行体**：`TunExecutor::request_port_forwards` 默认实现**保持 -1**（无承载 = 真话）；`facade::port_forwards_gate` 对 `FakeExec` 的既有断言**仍然成立**，新增「承载执行体 ⇒ 0」在 `tun_exec` 侧（§5.2 #17）。

**涉及文件**：`facade/tun_exec.rs`、`facade/mod.rs`。**风险**：中低（预算叠加登记 §6.3 注记 B④/⑤）。

### F5（P1）安全守卫：条数上限 / 阀 / 拨号期限 / 线程栈

**方案**

1. `portfwd.rs` 常量：`MAX_PF_RULES = 8`（spec 镜像；Go 桌面 `facade/forward.go:38` 同值）、**`MAX_PF_FLOWS = 256`**（**偏离 Go 的 4096**；口径 = 手机内存预算，§2-D4/§4.2）。
2. `validate_table` 增 `TableErr::TooMany { n }`（`> MAX_PF_RULES`）⇒ NAPI `-2`。
3. 装配路径（tunConfig 旁路）逐条防御（§1-F8）。
4. 阀行为：**拒绝型**（Go 同形；不复用桥的「满员挤最老」——桥是另一套语义）。**措辞精确化**：客户端核**无同类「拒绝型」阀**；同 crate 另有 `bridge_host::ConnGate`（挤最老型）、`files_server::MAX_CONNS=16`、`speedtest_server` 12、`server/intercept::MAX_CONNS=1024`（其它角色面）。
5. **线程栈显式化**：泵线程与 conn 线程均 `stack_size(128 KiB)`（默认 2 MiB ×3 线程/连接无谓吃虚拟内存）。
6. **资源账**（写进 §4.2）：每连接 = 2 泵线程 + 1 conn 线程（各 128 KiB 栈）+ 引擎 2×1 MiB 缓冲（`TCP_BUF` 固定）；阀 256 ⇒ 上界 ≈**引擎缓冲 512 MiB（RSS 随流量趋近）** + 栈 ≈96 MiB（虚拟）；**注记**：`intercept::MAX_CONNS=1024` 是**出口进程全局配额**（覆盖所有客户端与所有腿）⇒ pf 达阀会挤压其它隧道流（登记，§4.3/§6.3）。

**涉及文件**：`facade/portfwd.rs`、`facade/mod.rs`、`facade/tun_exec.rs`。

**风险**：低（参数面）。**测试**：`validate_table` TooMany、阀拒绝 + `flows` 归零（§5.2 #6）。

### F6（P1）bind 语义对齐 Go（`SO_REUSEADDR` + CLOEXEC + 单次尝试）

**方案**：`pf_bind(listen: u16) -> io::Result<TcpListener>`：`sysfd::socket_cloexec(AF_INET, SOCK_STREAM, 0)` → `setsockopt(SO_REUSEADDR)` → `bind(127.0.0.1:listen)` → `listen(backlog=128)` → `from_raw_fd` + `set_nonblocking(true)`。
**单次尝试不重试**（Go `net.Listen` 一次；桥的 `listen_path` 有界重试是桥的语义）；地址**硬编码 `127.0.0.1`**（无配置面、无 `0.0.0.0` 退路）。
**登记**：Go 的 backlog Linux = `somaxconn`、darwin = 128 ⇒ 本批固定 128（回环普通负载无行为差异）。

**涉及文件**：`facade/portfwd.rs`。**风险**：低。**测试**：§5.2 #1/#10。

### F7（P0，收口条）`code` 词表收口（本批第一件事）

**方案**：`PfState::failed(err)` 的 `code = Some(BindFailed)` = **真值**；成功态空码（Go 同形）；`dial_failed`/`invalid_target` **不产出**（spec 自证为登记保留）⇒ `ALLOWED_ABSENT`/`vocab_dump.rs`/manifest **零改动**；Q-F 的「`code` 空 + MUST 有意偏离」→ **达标**（限定语见 §3）。

**涉及文件**：`facade/portfwd.rs`、`docs/INTEROP-CRITERIA.md`（§6 草稿）。**风险**：极低。

### F8（P1）tunConfig 旁路防御面（破损配置不谎报；不 bind、逐条 failed）

**方案**（逐条、不影响其余条目）：
1. 三类**在装配期就不 bind**并记 `failed_with(err, None)`（空码 + 精确 err）+ 记行：
   ① `listen == 0 || listen < MIN_PORT` ⇒ err `监听端口 {listen} 不在 1024–65535（本机未建立监听）`；
   ② 超出 `MAX_PF_RULES` 的条目 ⇒ err `映射数超过上限（8）——本条未建立监听`；
   ③ **`target_ip` 非空且 `parse::<Ipv4Addr>()` 失败** ⇒ err `目标地址非法：{target_ip:?}——本条未建立监听`（**第二轮门 N-4/N-18**：v2 漏此形态；不拦截则条目会 bind 成功、状态 `listening`，而每次连接在 `dial_target()` 才失败 ⇒ 复现「监听中但连不上」）。
2. `target_port == 0` 走 `dial_target` 折叠语义（IP 分支折 `listen`；空 IP 分支 `ExitPort(0)`）——**登记**（Go `pfDial` 原样拨 `TargetPort`；NAPI 门拒 0 ⇒ 仅旁路可达）。
3. **必须有条目**（否则 tier 永远「启动中…」，`PortForwardsPage.ets:225-239`）；同表重复 listen **不**前置拒绝（第二条真 `EADDRINUSE` ⇒ 真 `bind_failed`，Go 逐字同形）。
4. **可达性取证**：表单路径不可达；**手改/损坏的持久化记录可达**（`parsePortForwards` 只丢 id/端口为 0 的条目、`HostStore.updateForwards` 不校验）。

**涉及文件**：`facade/portfwd.rs`。**风险**：低。**测试**：§5.2 #9。

### F9（P2）测试缝与环境一致性

**方案**：`PfRuntime::new(..., limits: PfLimits)`；拨号注入；**阈值注入**（小阀值）；**`wait_ack` 注入**（②轮门 N-3：强制走 2b 超时路径）；**确定性换入缝**（「换入前阻塞」钩子 ⇒ 直喂「读侧只见全旧/全新」，替代不可信的并发采样）；
`GenRun::synthetic_for_test` 补 `tun_shared.gen` 与 `run.gen` 一致（现 `1` vs `0`，否则 stale 复查恒 `-1`）；测试端口用**探测空闲端口** helper；「spawn 失败」按 Q-F 口径用纯函数缝 + 如实标注。

---

## 2. 「二选一」类决策的取证与裁定

| # | 决策 | 选项 | 取证 | 裁定 |
|---|---|---|---|---|
| **D1** | `PfRuntime` 落点/锁形 | ① `tun_exec.rs` 内联 ② `portfwd.rs` 独立全注入 | ② 可纯回环测（`BridgeHost::set_dial` 同款缝） | **②** |
| **D2** | `unavailable*`/`pf_states(rules)` 残基 | ① 保留 ② 删除 | 无生产者 = 死码 + 保留「谎报能力」；`as_str` 同串无线上差异 | **② 删除 + 单测改写** |
| **D3** | 替换原子性与 fd 归属 | ① 全程持状态锁 ② 两阶段 + install 锁 + **只 take lns** + ack | ① Rust 持锁做 bind/spawn/join 会钉住状态查询；② 只 take `lns` 才能保证「全旧或全新」 | **②**（Go 是「读者被阻塞」，Rust 是「读者看到全旧或全新」= 等价语义） |
| **D4** | 阀值 | ① 4096（Go 同值）② 1024（出口同值）③ **256（内存口径）** | 引擎 2 MiB/连接缓冲；出口 1024 是**全局共享配额**（拿它当依据不成立，第二轮门 N-5）⇒ 需手机内存口径：阀 N ⇒ 缓冲最坏 2N MiB；256 ⇒ ≈512 MiB，仍 ≥8× 于浏览器常态并发 | **③ 256** + 线程栈显式 128 KiB；**登记为偏离 Go 数值**（§6.3） |
| **D5** | 破损配置状态码 | ① `bind_failed` ② **空码 + 精确 err** ③ 不落状态 | ① 假归因「端口被占用」② App 空码兜底原样展示 err ③「启动中…」= 另一种谎报 | **②**（与 D15 的迟到失败同族；正常路径零空码） |
| **D6** | 条数上限强制点 | ① 只 NAPI 门 ② 门 + 装配 | 旁路可达 ⇒ 放大面 | **②** |
| **D7** | 拨号闭包回指 | ① `Arc<GenRun>` ② `Weak<GenRun>` | ① 环 ⇒ 世代泄漏（`DomainRefresher` 三回调已是 Weak 先例） | **②** |
| **D8** | CLI `cmd_portfwd` | ① 切核心 ② 不切 | CLI 走 `Session` 面；测试动词 | **② 不切 + 登记**（残留第三份泵拷贝；另 `daemon::carriers::pipe_half_close` 是 TCP 对半关闭单实现、`StreamConn` 面，域外；本批只做 RST 单源） |
| **D9** | 热替换时已建立连接 | ① 强关 ② 不强关（Go） | Go：`ln.Close()` 不影响已 accept 连接 | **②** |
| **D10** | 收工 ack/join 预算 | ① 无限等 ② detach ③ **共享 200ms + ack** | accept 线程 50ms 节拍必退；`STOP_WAIT` 3s 已近饱和 | **③**（detach 记 additive 行） |
| **D11** | pf 拨号面 | ① 复用 `healing_dial` ② **裸拨** | Go `pfDial`=裸拨（`transport.go:195-207`）；复用会把常态「目标拒绝」升级成 R2 恢复动作 | **②**（阶梯留给桥路径） |
| **D12** | 环回/未指定目标 | ① 原样 `Remote(127.0.0.1)`（栈拒 ⇒ 恒失败）② **映射为 `ExitPort`** ③ 防御面拒绝 | Go 经出口过境重拨到出口本机（能通）；tier 视 `127.0.0.1` 为合规输入 | **②** |
| **D13** | RST helper | ① 新写 ② **复用 `daemon::carriers::rst_close_tcp`**（上移 `sysfd`） | 已有单源 + 三处调用点 | **②** |
| **D14**（新） | 阀/准入位置 | ① conn 线程内（v2）② **accept 线程内**（Go 形） | ① 削弱阀的线程创建上界（超限连接仍会 spawn 线程再销毁）② Go 先在 accept 循环里比阀、超限连接**不进 goroutine**；RAII 守卫同样能保证 spawn 失败回退 | **②** + RAII `FlowGuard`（第二轮门 N-2/N-15） |
| **D15**（新） | Fatal accept 错误后的状态 | ① 保持 `listening`（v2，照 Go 字面）② **转 `failed`**（迟到失败位）③ 不释放 fd | ① Rust 的 fd 单属线程 ⇒ 退工即释放端口 ⇒ `listening` 是谎报；Go 的 fd 仍绑（后果不同，不是同形）③ 与「fd 单属线程」的 ABA 论证冲突 | **②**（`late_fail` + 空码 + 精确 err；**登记为偏离 Go**）（第二轮门 N-1） |

---

## 3. `code` 词表对应表（Q-F-B 第一件事的收口）

| `code` | tier 文案（`PortForwardRules.ets:207-219`） | 本批能否产出 | 由谁产出 / 落在哪 | 词表门 | 登记从→到 |
|---|---|---|---|---|---|
| `bind_failed` | 「端口被占用」 | ✅ **能（真值）** | 真 `bind()` 失败（EADDRINUSE/EACCES/…）⇒ `PfState::failed(err 原文)` | 已声明**不变** | Q-F：`code` 空 +「MUST **有意偏离**」→ **达标**（真 bind 失败，文案准确） |
| `dial_failed` | 「无法连接目标」 | ❌ 不产出（**登记保留**） | 每连接拨号失败是**瞬态**：`pfFails += 1` + 节流日志 + RST；不构成映射级失败（spec 原文） | `ALLOWED_ABSENT` **不变** | 无（保持） |
| `invalid_target` | 「目标地址非法」 | ❌ 不产出（**登记保留**） | 目标非法在**前置校验处直接拒绝、不落映射状态**（NAPI 门 `-2` + 装配面防御性拒绝——两处都**不产出该 code**；spec 原文语义，第二轮门 N-4 后仍成立） | `ALLOWED_ABSENT` **不变** | 无（保持） |
| （空串） | 空码兜底：原样展示 `err`、不误归因 | ⚠️ 三类**非常态**来源：① 破损配置（F8：`listen` 非法 / 超条数 / `target_ip` 非法 / `target_port=0` 旁路）；② accept **致命**错误后的迟到失败（D15）；③ （未来）新枚举值 | 装配期/运行期异常面 | 与词表门无关（空串不是词表值） | Q-F：空码是**主路径** → 本批：空码**只剩非常态面**；正常路径（真 bind 失败 / 成功）零空码 |

**结论**：spec「失败原因 MUST 携带稳定枚举 `code`」在 B 落地后**由 `bind_failed` 真值满足**；Rust 声明集、
`ALLOWED_ABSENT`、tier 码表、manifest **四处零改动** ⇒ 登记把 Q-F 的「有意偏离」改回「达标」
（**限定语**：除 §6.3 注记 B 的非常态面——spec 明文允许空/未知码走 App 兜底路径且「不误归因」）。

---

## 4. 安全面评估（本批新增的暴露面）

### 4.1 暴露面

| 面 | 形态 | 取证/理由 |
|---|---|---|
| 监听地址 | **硬编码 `127.0.0.1`**（无配置面、无 `0.0.0.0` 退路） | spec「仅回环，不暴露局域网」；Go `app_portfwd.go:94` 同形；`pf_bind` 只收 `u16` ⇒ 局域网不可达由绑定地址保证 |
| 同机访问 | 本机任意进程/用户可连（**无鉴权**，Go 同形） | spec 未要求鉴权；加鉴权与「浏览器 SHALL 能访问」冲突 ⇒ 接受的已知面 |
| 端口值域 | `[1024, 65535]`（NAPI 门 + 装配防御） | `MIN_PORT=1024`；特权端口不落 |
| 同表唯一 | NAPI `DupListen`；装配期第二条真 EADDRINUSE | `validate_table:60-64`；Go 同形 |

### 4.2 上限（新增）

| 上限 | 值 | 依据 | 触发行为 |
|---|---|---|---|
| 规则条数 | **8** | spec 镜像（Go 桌面同值） | NAPI `-2`；装配期逐条 `failed`（不影响其余） |
| 并发转发流 | **256**（偏离 Go 的 4096——§2-D4/§6.3） | 手机内存口径：引擎缓冲 **2 MiB/连接**（`stackb.rs:33/240-241`，`TCP_BUF` 固定、无引擎侧上限）⇒ 最坏 ≈512 MiB；仍 ≥8× 于浏览器常态并发 | 关新连接 + `flow_rejected` + 节流日志；**不回 RST、不计 `pfFails`** |
| 每连接资源 | 2 泵线程 + 1 conn 线程（各 **128 KiB 显式栈**）+ 引擎 2×1 MiB 缓冲 | 同上 + `bridge_host` 泵先例 | 阀 256 ⇒ 栈 ≈96 MiB 虚拟 + 缓冲 ≈512 MiB（RSS 随双向流量趋近） |
| 拨号期限 | `dialMs`（缺省 15s，每连接独立，**裸拨**） | Go `dialTimeout` | 到点 = 拨号失败 ⇒ `fails` + RST |
| 收工 | 监听器全关 + states 清空；ack/join 共享 200ms（到点 detach 记行） | Go `stopPortForwards` | 端口释放（fd 随线程退出关） |

### 4.3 洪水面 / 放大面

- **仅本机可触达**（回环）⇒ 无远程洪水面；本机失控进程 = 唯一来源，由**阀（先于线程创建）+ 条数上限**兜住。
- **本机放大面（如实登记）**：① 占满阀 ⇒ 后续连接被拒（有界，且不再为超限连接创建线程）；② 「连上即断」循环 ⇒ 每连接一次 `accepted/fails` 计数 + 节流日志（**裸拨**下不触发恢复阶梯——D11）；③ 日志面节流（阀 `<=5||%50`、拨号失败 `<=5||%20`、accept `<=3||%100`）。
- **出口配额面**：`intercept::MAX_CONNS=1024` 是**出口进程全局**（覆盖所有客户端与所有腿）⇒ pf 达阀会挤压桥/files/term 与其它客户端（登记；客户端阀 256 已低于该配额一个量级）。
- **无放大攻击面**：屏蔽局域网 + 只监听已配置端口 + **无动态目标入口**（对比 SOCKS）。
- **已建立连接**：可跑满带宽（Go 同形，无速率整形）。

### 4.4 与 Go 的对照

| 维度 | Go | Rust（本批） | 差异登记 |
|---|---|---|---|
| 阀 | `maxTCPFlows=4096`；自证注释称「与 gVisor 流共用」（`app_portfwd.go:209`）**已陈旧**（读写只在端口转发面 + netstack 退役 + 桥明文不占）⇒ 事实专属 | **pf 专属 256** | §6.3（**同形语义、不同数值**；数值口径 = 手机内存预算） |
| 拨号 | `DialTCPPort`/`DialTCP` = 裸拨 + ctx 期限 | **裸拨** `connect_deadline(dst, budget)` | 无 |
| 失败收口 | `SetLinger(0)` + Close（RST） | 复用 `sysfd::rst_close_tcp`（上移单源） | 无 |
| bind 选项 | `net.Listen` 默认 `SO_REUSEADDR`；backlog Linux=somaxconn / darwin=128 | `SO_REUSEADDR` 显式；backlog 固定 128 | §6.3 |
| 状态面 | `pfStatusJSON` 持 `pfMu` | `states()` 短锁克隆 → `snapshot()` | §6.3（D3 等价语义说明） |
| 热替换 stale | 应用后复查 `currentTunRun`/`done` ⇒ 收起 + `-1` | 同形（`gen`/`stop` 复查） | 无 |
| accept 错误 | 记一行 + 退出循环（`ln` 仍绑、状态仍 `listening`） | 复用 `classify_accept_err`：瞬态退避重试；**致命 ⇒ 记行 + 该条转 `failed`** | §6.2/§6.3（**偏离 Go**：Rust 的 fd 单属线程 ⇒ 退工即释放端口，`listening` 会是谎报） |
| 条数上限 | 无（App 面） | 核侧镜像 8 | §6.3（加固） |
| 破损配置 | `listen=0` 绑随机端口（状态面与实况不符） | 拒绝 + `failed` + 精确 err | §6.3（**偏离 Go 的加固**） |
| 环回目标 | 经出口过境重拨（通） | 映射 `ExitPort`（同效） | §6.3（实现路径不同、语义同） |

---

## 5. 测试与验收计划

### 5.1 门（沿用批协议）

- `cargo test --workspace` 全绿（已知 flake 按「隔离复跑 + 与改动面无交集 + 基线可复现」放过）。
- `cargo clippy --workspace --all-targets -- -D warnings` exit 0。
- `zsh tools/check-vocab.sh` PASS（**期望零改动**）。
- **交叉 check**：`cargo check --target aarch64-unknown-linux-ohos`（或 `tools/ci-local.sh`）——新增 libc 面（`socket`/`setsockopt`/`poll`/`SO_LINGER`）须过 OHOS/musl（`ci.yml` 已含该 job）。
- 判据/登记与代码**同批 commit**；**修前红证据**：状态面真值（`listening`/`bind_failed`）、rc（`-1 → 0`）、Fatal 迟到失败。

### 5.2 逐条判绿 / 证伪

| # | 面 | 用例（new/改） | 判绿证据 | 证伪/退出条件 |
|---|---|---|---|---|
| 1 | pf runtime | `install_reports_per_entry_states` | 成功条 `listening`（端口真可连）；占用条 `failed(code=bind_failed, err 原文)` | 用探测空闲端口 helper，不钉固定端口 |
| 2 | pf runtime | `install_atomic_single_swap`（**确定性缝**） | 阻塞期读 `states()` = **全旧**（非空、非半表）；换入后 = 全新 | 不采信采样式断言 |
| 3 | pf runtime | `hot_replace_frees_reused_port` | 复用同端口的替换成功（ack 生效）；旧端口替换后可外部重 bind | 若 ack 超时路径被走到 ⇒ 必须看到重试 + 记行（不许静默 `bind_failed`） |
| 3b | pf runtime | `install_ack_timeout_retries_then_marks`（**新**：注入「永不 ack」的旧监听器） | 断言 3×50ms 重试 + 双记行；最终 `bind_failed` 行**必须伴随**「旧监听器未在预算内释放」标注行（或重试成功 ⇒ 全绿） | 无该缝则该路径不可证 |
| 4 | pf runtime | `hot_replace_keeps_established_conns` | 替换后原连接双向仍通（D9） | — |
| 5 | pf runtime | `stop_all_clears_states_and_frees_ports` | stop 后 states 空、端口可重 bind、线程在预算内退（ack 收到） | — |
| 6 | pf runtime | `flow_valve_rejects_over_limit` | `limits.max_flows=2` ⇒ 第 3 连接被关（**且未 spawn 线程**——以「线程名计数」或注入 spawn 钩子断言）、`flow_rejected==1`、`flows` **归零**、行文含「并发流已达上限 … 累计拒绝」 | — |
| 7 | pf runtime | `dial_failure_rst_and_counters` | 桩拨号 Err ⇒ 对端 `read` = `ECONNRESET`（非 EOF）+ `accepted==1/fails==1` + `conns/flows` **归零** | RST 断言若平台不稳 ⇒ 降级「`SO_LINGER` 设置成功 + 对端非 EOF」并如实标注 |
| 8 | pf runtime | `dial_target_semantics`（五形态） | 空 IP ⇒ 隧道IP:port；`127.0.0.1`/`0.0.0.0` ⇒ 隧道IP:port（D12）；其余 IP ⇒ 该 addr；`target_port=0` IP 分支 ⇒ listen；**非法 IP ⇒ `Err(BadTarget)`（装配期拒绝）** | — |
| 9 | pf runtime | `bypass_config_entries_are_failed_not_bound` | `listen=0`/`80`、9 条、**`target_ip="example.com"`/`"::1"`** ⇒ **均不 bind**、状态 `failed` + 空码 + 精确 err | 第二轮门 N-4 形态必须有 |
| 10 | pf runtime | `bind_uses_reuseaddr`（`getsockopt` 回读）+ 连续两轮 bind 同端口 | 位已设 + 二次成功 | 分平台差异则分别断言并登记 |
| 11 | pf runtime | `pump_half_close_and_counters` | 上行 EOF ⇒ 对侧 `close_write` 被调；双向完成后 `conns/flows` 归零；pf 侧 EOF **零日志**、错误行带 `port-forward` 标签 | — |
| 11b | pf runtime | `accept_fatal_marks_entry_failed`（**新**，可注入的 accept 错误） | Fatal ⇒ 记行 + 该条 `snapshot()` 转 `failed`（空码 + err）+ 端口已释放 | 第二轮门 N-1 |
| 12 | tun_exec | `runner_of_reports_real_pf_state`（改） | `listening`/`code==""`/占端口条 `bind_failed`/`conns` 真值 | 替换 `runner_of_reports_honest_port_forwards` |
| 13 | tun_exec | `stats_line_reports_real_pf_counts` | `stats_line()` 纯函数直喂（形态 + 值） | — |
| 14 | tun_exec | `request_port_forwards_rc_zero_with_live_gen_stale_minus_one`（改） | 活世代 ⇒ `0` 且真装表；`stop`/gen 不符 ⇒ `-1` 且无孤儿监听器 | `synthetic_for_test` 的 gen 对齐 |
| 15 | tun_exec | `generation_teardown_stops_all_listeners` | 收工后端口可重 bind、states 清空 | — |
| 16 | 既有面 | `pf_states`/`unavailable*` 残基清理：按**生产 4 + 测试 3** 的实际 `pf_rules` 清单逐处处置；`portfwd_states_matches_pure_source` 删/改 | `cargo test` 全绿 + `grep -rn pf_rules` 核对 | — |
| 17 | facade | `port_forwards_gate`（改注释 + 增例） | `FakeExec` ⇒ 仍 `-1`；`TooMany ⇒ -2` 新例；「承载执行体 ⇒ 0」在 tun_exec 侧覆盖 | — |

### 5.3 回归面（不得破）

- C 族判据行逐字不变；`tunStatusJSON` 键序/键集不变（`fixtures/vectors/tun_status.jsonl` 10 案无 `portForwards`）。
- 桥面：`bridge_host` 既有 6 例（`pump` 参数化后桥侧行文逐字不变）。
- `facade::port_forwards_gate`、`service_op` 门、`dial_port` 锁纪律测试、`daemon/carriers` 三个 `rst_close_tcp` 调用点（可见性改动零行为）。
- 词表门/夹具门零改动（§6.5）。

### 5.4 用户触点（本棒不可执行，实现棒必办）

| 场景（tier spec） | 判据 |
|---|---|
| Scenario 1：浏览器访问出口主机自己的 8080 | 真机 + 真出口：`http://127.0.0.1:8080` 正常加载 |
| Scenario 2：目标是出口可达的其它 IP | `5000 → 192.168.3.5:5000` 浏览器可访问 |
| Scenario 3：与分流模式无关 | IP 分流开启且目标网段不在隧道网段 ⇒ 仍可转发（**取证已在设计内**：`connect_deadline` → 栈 B 默认路由到隧道 IP，不经 TUN 路由/绕过名单） |
| Scenario 4：端口被占用单条失败、其余正常 | 页面「失败 · 端口被占用」（`bind_failed` 分派），其余「监听中」，隧道正常 |
| 状态可见（不新增轮询路径） | 既有状态通道（§0.4 订正⑥） |

---

## 6. 判据行影响与登记草稿（`docs/INTEROP-CRITERIA.md`，与代码同批 commit）

### 6.1 「判据变更记录」+1 行（**从→到**）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-__（Q-F-B 批落地） | **`portForwards[]` 状态文案与 `ClientCoreTunSetPortForwards` 返回码**（契约面行为变更，非编号判据行；**接续 Q-F 该条**） | ① `state`：恒 `"failed"`（诚实态）→ **`"listening"`（真监听）/`"failed"`（真 bind 失败 / 迟到失败）**；② `err`：恒「手机核未提供端口转发监听…」→ **空（成功）/ 真 bind 错误原文（失败）**；③ `code`：空 → **`bind_failed`（真值；MUST 由「有意偏离」转「达标」——限定语：除 §6.3 注记 B 的非常态面）**；④ `conns`：恒 0 → **真连接数**；⑤ **未 attach / 未装表期 与 收工后**：`portForwards` 为空数组（tier 显示「启动中…」）——HEAD 是「失败 · 未提供…」；⑥ rc：`-1` → **`0`（有承载，已受理）/ `-1`（无世代或世代已收口）/ `-2`（JSON/校验不过，含新增条数上限）** | Q-F-B：实装真监听器（Q-F 挂账项收口）；`code` 死结随真值消失 | `tier:openspec/specs/port-forwarding`「建立与访问」SHALL 由**已知不达标**转**达标**；「映射状态可见」失败码 MUST 转**达标**（§3）；tier 两处失义文案（`PortForwardsPage.ets:481-497`/`TierVpnExtensionAbility.ets:1002-1016`）**随本批自愈**；`facade/portfwd.rs`/`facade/tun_exec.rs` 单测；**`fixtures/` 无 portForwards 夹具 ⇒ 无字节夹具变更**；`tools/check-vocab.sh` **零改动** |

### 6.2 「计数输入集 / 数值语义变化」+4 行（行文不变/新增观测）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-__（Q-F-B） | **`stats.pfAccepted`/`stats.pfFails`** | 「恒 0 的真值（无监听器）」→ **真计数**（accept 准入数 / 拨号失败数） | 实装监听器 | `runner_of`；tier 只校验键存在 |
| 2026-10-__（Q-F-B） | **`portForwards[].conns`** | 恒 0 → **真连接数** | 同上 | tier「监听中 · N 条连接」真实可用 |
| 2026-10-__（Q-F-B） | **`stats:` 日志行的 `pf=a/f`** | 恒 `pf=0/0` → 真值（**行文形态逐字不变**） | 同上 | 世代日志读者 |
| 2026-10-__（Q-F-B） | **新增观测行（additive）** | 无 → 有：① `port-forward: 监听 127.0.0.1:{listen} 失败（{e}）——该条映射不可用，不影响隧道`；② `port-forward: 127.0.0.1:{listen} -> {target} 监听中`；③ `port-forward: 已停止全部监听器（{n} 个）`；④ `port-forward {listen}: 并发流已达上限 {max}，拒绝（累计拒绝 {r}）`；⑤ `port-forward: {listen} -> {target} 拨号失败 #{n}: {e}`；⑥ 防御面（`listen` 非法 / 超条数 / `target_ip` 非法）三条；⑦ 旧监听器未在预算内退出（detach）+ 「未及时释放 ⇒ 已重试 bind」；⑧ accept 瞬态/致命（复用 Q-E F5 分类件行文）+ **该条转 `failed`**；⑨ 连接/泵线程 spawn 失败 + **pf 泵的读错误/写失败行**（`port-forward[up|down] …`，桥侧旧行文不变）；⑩ Fatal 迟到失败置位行 | 监听器全生命周期可观测（Q-F「spawn 失败不静默」纪律延伸）；①②③④⑤ 与 Go 行文**逐字同形**（`app_portfwd.go:87/99/101/213/229` —— ④ 含「累计**拒绝**」二字已核），其余为 Rust 侧新增面 | 世代日志读者；**非编号判据行** |

### 6.3 「已知口径注记」

1. **改判既有 Q-F 注记**：`【Q-F 批】portfwd 诚实态（F1，行为差异 + 功能缺口）` → 追加一行
   「**已由 Q-F-B 收口（2026-10-__）：实装真监听器；本注记的『未实装/已知不达标』不再成立**」（保留历史原文 + 收口指针）。
2. **新增注记 A（真监听器 + 安全守卫，含偏离 Go 的四处）**：`【Q-F-B 批】portfwd 真监听器`：
   仅 `127.0.0.1`（地址不可配置）；规则 ≤8；**并发流阀 256（偏离 Go 的 4096**——口径 = 手机内存预算：引擎每连接
   2×1 MiB 缓冲（`stackb.rs:33`）无上限 ⇒ 256 ⇒ 最坏 ≈512 MiB；Go 注释称「与 gVisor 流共用」**已陈旧**，其读写只在端口转发面）；
   拨号 = **裸拨** + `dialMs`（缺省 15s）；世代收工全关 + 清状态；`SO_REUSEADDR`（Go 默认）；backlog 固定 128（Go Linux=somaxconn/darwin=128）；
   RST 收口复用 `sysfd::rst_close_tcp`（单源）；每连接线程显式 128 KiB 栈；accept 瞬态错误退避重试（Q-E F5 分类件）；
   **accept 致命错误 ⇒ 记行 + 该条状态转 `failed`**（**偏离 Go**：Go 留 `listening` 且不释放 fd；Rust 的 fd 单属 accept 线程 ⇒ 释放端口，故必须改状态）。
3. **新增注记 B（非常态面 + 残余）**：① `tunConfig` 旁路破损配置（`listen` 非法 / `target_ip` 非法 / `target_port=0` / 超条数）
   **不 bind** → `failed` + **空码** + 精确 err（**表单路径不可达；手改/损坏的持久化记录可达**——`parsePortForwards`/`HostStore.updateForwards`
   不重复校验）；Go 对 `listen=0` 会绑随机端口 ⇒ **偏离 Go 的加固**；② 环回/`0.0.0.0` 目标映射为 `ExitPort`（Go 经隧道过境重拨，实现路径不同、语义同）；
   ③ 跨世代换代的同端口瞬时 EADDRINUSE 窄窗（Go 同形）；④ install 等待旧监听器 ack 的预算 = **共享 400ms**（到点走 3×50ms 重试 bind + 双记行）；
   收工 `stop_all` 的 ack 预算 = 共享 200ms（到点 detach 自退，fd 随线程退出关）；⑤ **收工预算叠加**：pf ≤200ms + 派生线程 join ≤2s + 桥 ≤2s
   vs `STOP_WAIT=3s` ⇒ 极端形态走既有 -2 强制放锁路径（Q-F §7-5 同族，挂 Q-G）；⑥ 已建立连接无空闲回收/无速率整形（Go 同形）；
   ⑦ 无鉴权（Go 同形，本机面）；⑧ 引擎侧连接表无独立上限（阀是唯一界；`stackb::DialError::TooManyConns` 是死变体，本批不接线）；
   ⑨ **出口配额全局共享**：`intercept::MAX_CONNS=1024` 覆盖所有客户端与所有腿 ⇒ pf 达阀会挤压桥/files/term 与其它客户端。

### 6.4 收口面（多文档）

- `docs/REVIEW-ROADMAP.md`：Q-F-B 行状态回填（**主会话**）；Q-F 行「落地前…已知不达标」→ 补「**已由 Q-F-B 收口（达标）**」。
- `docs/ROADMAP.md:88`（「Q 批之后的未开批」第 3 条现列 Q-F-B）→ 注销/记完成（AGENTS：ROADMAP 是唯一进度真源）。
- `docs/reviews/AUDIT-2026-10-07.md` Q-F 条目尾注 → 补收口指针（不改原文）。
- `QF.md` §7 / `QF-design.md` §2.4 = 历史交接，不改写；收口记录落 `QFB.md`。

### 6.5 零改动面（核对确认）

- **编号判据行（E/C/R 族）**：零改动。**`fixtures/`**：零改动。**词表门**：零改动（声明集/缺席表/manifest/tier 码表）。
- **NAPI 符号面**：零改动（符号不变，仅返回值值域变化）。

---

## 7. 不做与移交登记（防「静默漏做」）

| # | 项 | 处置与理由 |
|---|---|---|
| 1 | **真机浏览器 E2E 四场景**（§5.4） | **用户触点**；实现棒交包后执行。 |
| 2 | tier 两处文案 | **不改 tier**：本批落地即自愈（§0.4 订正①）。 |
| 3 | CLI `cmd_portfwd` 切核心实现 | **不做**（D8）；与 `daemon::carriers::pipe_half_close`（`StreamConn` 面）合计两份域外实现——本批只做 RST 单源（D13），余者登记。 |
| 4 | 已建立连接的带宽/时长占满 | **登记**（Go 同形）：无空闲回收、无速率整形。 |
| 5 | 跨世代换代的同端口瞬时 EADDRINUSE | **登记**（Go 同形窄窗）。 |
| 6 | 收工 join/ack 到点 detach | **登记**（+ additive 行）；无泄漏路径（无 Arc 环）。 |
| 7 | `TunExecutor` 默认 `request_port_forwards` 仍 `-1` | **保持**（无承载 = 真话）。 |
| 8 | 破损配置的空码路径（F8） | **登记**（§6.3 注记 B，含可达性取证）。 |
| 9 | 引擎侧连接表无独立上限 | **登记**（§6.3 注记 B⑧）。 |
| 10 | 引擎缓冲按连接固定 2×1 MiB（不可缩放） | **登记**；阀值 256 已按此定。 |
| 11 | `stats_loop` 的 tick 下限 5s | 不变量；`stats_line` 纯函数解决可测性。 |
| 12 | accept 致命错误后「转 `failed`」偏离 Go | **登记**（§6.3 注记 A）；Go 的「留 listening、不释放 fd」在 Rust 结构下不可照抄。 |

---

## 8. 设计门记录（dsh 外部评审）

### 8.1 轮次档案

| 轮 | 目录 | exit | 产物 | 性质 |
|---|---|---|---|---|
| 第一轮（设计门） | **`/tmp/dsh-review/r25.snY8bb`** | **0**（前台捕获） | `output.md` **136 行（已 Read 全文）**；`stderr.log` 1271 行（推理流，不参与结论） | v1 → v2 整改 |
| 第二轮（复校） | **`/tmp/dsh-review/r26.vcqR9Q`** | **0**（前台捕获） | `output.md` **185 行（已 Read 全文）** | v2 → v3 整改；判「有条件通过」 |

- 两轮评审**均未修改仓库任何文件**（第二轮自述 + `git status` 复核：仅未跟踪的 `docs/reviews/QFB-design.md`）。
- 第一轮独立做的事：复核基线 HEAD 与工作树；回读 Go/tier 原文；**抽查 30+ 处行号引用**；独立复跑 `tcpFlows` 读写点、
  `TCP_BUF`、`stackb` 环回拒绝、`rst_close_tcp`、`netpipe` 日志口径、Go defer LIFO、`SO_REUSEADDR`/backlog、
  `STATS_SECS_MIN`、tier 校验覆盖面。
- 第二轮独立做的事：复跑基线门（`642 passed/0 failed/4 ignored`、词表门 PASS）；**逐条裁定 12 条整改**（全部「已闭合」）；
  回读第一轮原文并比对本记录；新查 `pf_rules` 全仓引用（7 处）、`dial_target` 非法目标路径、Go `pfAccept` 退工语义、
  `intercept::MAX_CONNS` 的全局性。

### 8.2 第一轮评审原文摘要（编号为评审者原编号）

- **① 生命周期**：**1.1【中】** install 步骤 1「取出旧 `{rules,states,lns}`」+ 步骤 5「换入」⇒ 字面实现窗口内返回**空表**（第三态），#2 采样断言无法证伪；**1.2【中】** join 预算 50ms = poll 超时上界 ⇒ 常态超时 + 超时后 bind 同端口 ⇒ 假 `bind_failed`；**1.3【低】** 「Go 顺序：先关监听器」不成立（defer LIFO = 桥先停）；**1.4【低】** panic/毒锁路径未写锁纪律；**1.5【低】** 漏列既有单测 `portfwd_states_matches_pure_source`；**1.6【低】** detach 无可观测位。
- **② 资源/安全**：**2.1【高】** 阀 4096 漏算引擎 2 MiB/连接（`TCP_BUF`×2）⇒ 最坏 ≈8 GiB，且出口 1024 ⇒ 3/4 额度注定被拒；**2.2【中】** 「Go 与 gVisor 数据面共用计数器」为假；**2.3【中】** 「全仓 grep 无同类」过强（同 crate 有 intercept/files/speedtest 的 `MAX_CONNS`、桥 `ConnGate`）；**2.4【低】** 本机放大面只写了「占流」；**2.5 正面**：回环-only 成立、无鉴权与 Go 同形、条数 8 对齐、日志节流逐字同形。
- **③ 转发正确性**：**3.1【中】** loopback 目标 Go 通、Rust 恒失败且无归因；**3.2【中】** pf 复用 `healing_dial` ⇒ 常态拒绝触发 R2 阶梯 + 未节流 RECOVER 行；**3.3【中】** `pump` 复用漏了日志量/行文；**3.4【低】** accept 非 WouldBlock 错误处置未定义（忙转）；**3.5【低】** `pf_rst_close` 重复造轮子（`rst_close_tcp` 已存在；另有 `pipe_half_close`）；**3.6【低】** 防御面漏 `target_port==0`；IP 分支 `0⇒listen` 与 Go 不同形。
- **④ 状态/词表**：**4.1【低】** 「MUST 达标」需限定防御面；**4.2【低】** 「对合规 App 不可达」论证可加强；**4.3【低】** `PfState::failed` 硬编码 code ⇒ 空码表达不出来；**4.4 正面**：真值路径、`dial_failed`/`invalid_target` 与 spec 逐字一致、词表门/夹具/只按 code 分派**均已独立复验**。
- **⑤ 分流无关**：**5.1【低】** 取证现在就能写进设计；其余正面。
- **⑥ Go 对齐**：**6.1【中】**（= 2.2 重复行）；6.2（=1.3）/6.3（=3.5）/6.5（=3.6）/6.4 backlog 为低危；**6.6 正面**：rc/门序/stale 复查/状态清空/四条日志行文逐字、拨号期限链路、`listen=0` 缺口、条数 8 来源、Go 测试 8 例被 §5.2 覆盖且超出。
- **⑦ 可行性/可测性**：**7.1【中】** #13 缺缝；**7.2【中】** #2/#3 需确定性缝；**7.3【低】** 阀/计数归属矛盾 + conn 线程 spawn 失败泄漏槽位；**7.4【低】** 门清单缺交叉 check；**7.5【低】** 测试端口卫生；**7.6 正面**：缝可行、D1 成立、`Weak` 破环正确、**无 Go 直译痕迹、无过度设计**。
- **⑧ 登记面**：**8.1【中】** 漏「新增观测行」登记；**8.2【低】** 漏「未 attach 期条目集为空」；**8.3【低】** 漏 `ROADMAP.md:88`；**8.4【低】** 登记落点错配；**引用准确性【低】**：tier 行号两处、「250ms 轮询」表述错、`WriteHalf` impl 计数不精确。
- **计数（第二轮核对原文后的准确值）**：**1 高 + 10 中 + 23 低**（6.1 = 2.2 的重复行；正面结论不计）。

### 8.3 第一轮逐条处置表（v1 → v2）

| 评审编号 | 严重度 | 处置 | 落到（v2/v3） |
|---|---|---|---|
| 1.1 | 中 | **认同并改设计**（只 take `lns`；#2 改确定性缝） | §1-F1-3、§1-F9、§5.2 #2 |
| 1.2 | 中 | **认同并改设计**（退出 ack + 有界重试 + 双记行） | §1-F1-3 步骤 2/2b、§2-D3 |
| 1.3 | 低 | **认同并订正**（Go defer LIFO = 桥先停） | §0.4 订正②、§1-F4-3 |
| 1.4 | 低 | **认同**（`lock_unpoison` + 毒锁收工单测） | §1-F1-2 |
| 1.5 | 低 | **认同**（补测试清单 + 引用面穷举） | §1-F3-3、§5.2 #16 |
| 1.6 | 低 | **认同**（detach additive 行） | §1-F1-3 步骤 6、§6.2 |
| 2.1 | **高** | **认同并改**（4096 → 1024 → v3 再订 256；资源账重算） | §2-D4、§1-F5、§4.2、§6.3 A |
| 2.2 | 中 | **认同并改**（三处删假断言 ⇒ 专属阀 = 同形） | §0.3-N9、§4.4、§6.3 A |
| 2.3 | 中 | **认同**（措辞精确化 + 桥「挤最老」取舍说明） | §0.2 #8、§1-F5-4 |
| 2.4 | 低 | **认同**（放大面写全；D11 已消除阶梯放大） | §4.3 |
| 3.1 | 中 | **认同并改**（环回/0.0.0.0 ⇒ `ExitPort`；v3 再补 `target_ip` 非法） | §1-F2-2、§2-D12、§6.3 B |
| 3.2 | 中 | **认同并改**（pf = 裸拨） | §1-F2-1、§2-D11 |
| 3.3 | 中 | **认同并改**（`pump` label/eof_log） | §1-F2-4、§6.2 ⑨ |
| 3.4 | 低 | **认同并改**（复用 `classify_accept_err`；v3 再订 Fatal 状态语义） | §1-F1-4、§2-D15 |
| 3.5 | 低 | **认同并改**（`rst_close_tcp` 上移单源） | §1-F2-3、§2-D13 |
| 3.6 | 低 | **部分认同**（认同「补 `target_port=0` 处置」；不认同「去掉 0⇒listen」——FIX-46 既有、NAPI 门拒 0 ⇒ 保留 + 登记） | §1-F8-2、§6.3 B |
| 4.1 | 低 | **认同**（§6.1 ③ 加限定语） | §3、§6.1 |
| 4.2 | 低 | **认同**（可达性取证改为 `parsePortForwards`/`HostStore`） | §0.3-N4、§1-F8-4 |
| 4.3 | 低 | **认同**（新增 `failed_with`） | §1-F3-4 |
| 5.1 | 低 | **认同**（分流独立性取证前移） | §1-F2-6、§5.4 |
| 6.4 | 低 | **认同**（backlog 登记） | §1-F6、§6.3 A |
| 7.1 | 中 | **认同**（`stats_line` 纯函数） | §1-F3-2、§5.2 #13 |
| 7.2 | 中 | **认同**（确定性缝） | §1-F9 |
| 7.3 | 低 | **认同**（v2 = 挪 conn 线程；v3 = 回 accept 线程 + RAII） | §2-D14、§1-F1-5 |
| 7.4 | 低 | **认同**（交叉 check 入门清单） | §5.1 |
| 7.5 | 低 | **认同**（空闲端口 helper） | §1-F9、§5.2 #1 |
| 8.1 | 中 | **认同**（+1 行 additive 观测行） | §6.2 |
| 8.2 | 低 | **认同**（§6.1 ⑤ 补未 attach/收工后） | §6.1 |
| 8.3 | 低 | **认同**（收口面补 `ROADMAP.md:88`） | §6.4 |
| 8.4 | 低 | **认同**（预算叠加落 §6.3 B⑤） | §6.3 |
| 引用准确性 | 低 | **认同并订正**（tier 行号 / 废弃「250ms 轮询」/ `WriteHalf` 计数） | §0.4 订正⑥、§0.2 #3/#12 |

### 8.4 第二轮评审原文摘要（复校；编号为评审者原编号）

- **裁定 12 条整改**：① 阀值/资源账 **已闭合**（残余 N-5）；② 假断言三处改写 **已闭合**（并指出 v2 比第一轮转述更准——陈旧注释的准确锚点是
  `app_portfwd.go:209` 的「与 gVisor 流共用」）；③ install 只 take `lns` + 单次换入 **已闭合**（与 Go 持 `pfMu` 的语义等价性被确认）；
  ④ ack + 重试 **已闭合**（链路成立，残余 N-3）；⑤ 环回映射 **部分闭合**（新暴露 N-4）；⑥ 裸拨 **已闭合**（并确认「恢复能力不丢：`patrol` 独立驱动」）；
  ⑦ `pump` label/eof_log **已闭合**（残余 N-8）；⑧ RST 单源 **已闭合**；⑨ `classify_accept_err` 复用 **部分闭合**（Fatal 状态语义错——N-1）；
  ⑩ 阀/计数单属 **已闭合**（但引出 N-2）；⑪ `stats_line` **已闭合**；⑫ 登记/引用订正 **大部分闭合**（残余 N-7/N-9/N-10/N-11/N-12）。
- **新暴露问题**：**N-1【中】** Fatal accept 后「状态 `listening` + fd 已关」矛盾（Go 是「留 listening、fd 不关」，两者后果不同）；
  **N-2【中】** 阀在 conn 线程 ⇒ 不再约束线程创建（与 §4.2 的「阀 = 上界」冲突）；**N-3【中】** ack 预算两口径 + 2b 超时路径无测试缝；
  **N-4【中】** F8 漏 `target_ip` 非法（可达 + 复现「监听中但连不上」+ 与 spec 自证冲突）；**N-5【中】** 1024 的论证与资源账仍不完整
  （conn 线程栈未入账；出口 1024 是**全局共享配额**、「同刻度」不构成安全性论证）；**N-6…N-12【低】**：计数回退未写死、
  ④ 行文漏「拒绝」二字却列入「逐字同形」、pf 泵错误行未入观测行枚举、§8 计数与原文不符（实为 1 高 + 10 中 + 23 低；§8.4 中危列表漏 2.3）、
  `pf_rules` 引用计数错（实为生产 4 + 测试 3）、「无同类阀」仍偏宽（桥 `ConnGate` 在客户端核数据路径上）、以及类型/Disconnected 语义/
  install 中途 panic 第三态/小引用等 7 处细节。
- **保真度**：§8 档案事实（目录/行数/exit）、§8.2 摘要（全部 25 个编号条目）、§8.3 处置表与三条硬约定 **逐条比对无失真**，唯一失真是计数行。
- **门结论建议原文**：上一轮 5 条必改「**已实质回写并回源码成立**」「没有一条整改是纸面敷衍」；但新暴露 4 条须实现前钉死
  （N-4 与 N-1「与本批『消灭谎报』主题直接冲突，不宜留到实现棒自行处置」）⇒ **建议判定「有条件通过」**：出 v3 钉死 N-1…N-5，
  「即可直接进实现棒；N-6…N-12 可随 v3 一并收口」（**不需要第三轮**）。

### 8.5 判定

- **第二轮对 12 条整改的裁定 = 全部闭合**（1 条「部分闭合」= ⑤ 引出 N-4，已在本稿处置）；**无一条为纸面敷衍**。
- **第二轮新暴露 5 条（N-1…N-5）已在 v3 全部钉死**：N-1 ⇒ §2-D15 + §1-F1-4（转 `failed`，登记偏离 Go）；N-2 ⇒ §2-D14 + §1-F1-5（回 accept 线程 + RAII）；
  N-3 ⇒ §1-F1-3 步骤 2/2b（共享 400ms 单口径）+ §1-F9/§5.2 #3b（强制超时缝）；N-4 ⇒ §1-F8-1③ + §5.2 #9 + §6.2 ⑥；
  N-5 ⇒ §2-D4（256 + 内存口径）+ §4.2（conn 线程栈入账）+ §4.3/§6.3 B⑨（出口配额全局共享）。
- **N-6…N-12 一并收口**：回退写死（RAII）/行文补「拒绝」/泵错误行入枚举/§8 计数订正/`pf_rules` 实清（生产 4+测试 3）/「无同类**拒绝型**阀」措辞 +
  列名/类型统一（`AtomicU64`）/ack `Disconnected` 语义/install 无 panic 结构/HostStore 行号/交叉引用——逐条见上表与相应小节。
- **第三轮**：第二轮明确「不需要第三轮评审」——本稿即按要求钉死 N-1…N-5（+ N-6…N-12）后的 v3。
- **结论：设计门通过（v3），可进实现棒**。
- **实现棒须遵守的三条硬约定**（承 Q-F 协议）：① 不得把 v1/v2 的旧写法带进代码（复用 `healing_dial`、take 全 `inner`、
  阀 4096/1024、阀放 conn 线程、Fatal 后保持 `listening`、假「共用阀」陈述）；② 判据/数值/观测行登记与代码**同批 commit**；
  ③ 实现期发现设计与代码新矛盾（或本稿某条不可实现）**不得静默降级**——记 `QFB.md` 并回主会话。
