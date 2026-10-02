# R2 技术设计（客户端全量：行为逐常量对齐 + 恢复阶梯 + 中继腿 + files/portfwd + facade 预留）

> 开工前技术评审输入（ROADMAP「评审协议」第 1 道门）。行为常量唯一真源 =
> `tier:docs/agents/connection-lifecycle.md`（只读；本程序不改 Go 侧，只对齐）。
> Go 源码引用一律出自 `baseline/homeway`（基线 621fe0e）。
>
> **v2 = 技术评审（dsh，2026-10-02）整改后版本**：评审 1 高/15 中/20 低，高危与全部
> 中危已整合（高-1 解锁补发 reg 搭车、中-6 DirectFirst 照 Go 首包副本重写、中-9/10/11/12/13
> 线程与 gate、中-14/15 服务域语义、中-17 Rust 形态、中-18 三档注入、中-2 前缀口径
> 〔含 R1 实错的修正〕等），逐条处置见文末「评审整改记录」。本版本即 R2 实现基线。

## 0. 范围与拆步

R1 交付 = 直连垂直切片（无 TUN 栈内客户端 = Go「服务会话」形态）。R2 在其上补全
**客户端行为面全部**，拆步与依赖序（每步 cargo test 绿再进下一步）：

| 步 | 内容 | 主要 Go 真源 |
|---|---|---|
| 2a | WG 重连语义专项：不 wipe 快速重连复现 + ResetPeerSession/RefreshReg 同语义恢复 | `wgcore/core.go::ResetPeerSession`、`wtransport/bind.go::RefreshReg` |
| 2b | 恢复阶梯 R1–R3 全档位（常量逐条 + 判据行同串 + 单飞合并 + 耗尽整会话重建） | `hostsession/recover.go`、`hostsession/service.go` |
| 2c | 漫游/换源（handover 双发/Rebind/Rearm 家族）+ 端点缓存落盘与消费者接线 + 读错误退避 | `wtransport/bind.go`、`endpointcache.go`、`wgcore/transport.go` |
| 2d | 中继腿：[0xAA][relayID] 信封、DirectFirst 窗口、解锁补发、via=relay、hint→缓存→打洞 | `bind.go`、`proto/frame.go`、`internal/relay/*`（只读对端） |
| 2e | files 客户端（问候/行 JSON/4B 帧/write 关流即取消）+ portfwd + 60s 巡检（Session 状态机） | `pkg/files/{proto,client}.go`、`app_portfwd.go`、`hostsession/service.go::patrol` |
| 2f | facade trait 预留（tun prepare/attach 两阶段）+ 状态 JSON 契约（serviceSnapshotJSON 形状） | `app_service.go::serviceSnapshotJSON`、`tunmode.go::tunStatusJSON` |
| 2g | 收口：R1 移交 12 项处置 + E9/C11/E12 判据补采 + 性能/正确性回归 | R1 代码评审 output |

**形态不变**：无 TUN 的栈内客户端（服务会话形态）。TUN/两阶段启动的真实现象属 R7，
本期只做 trait 面。

## 1. 架构：Session 层的引入与线程布局

R1 的 `Client`（wgcore 引擎句柄）保持不动，其上引入 **`session::Session`**
（对齐 Go `hostsession.Session` 的服务会话状态机，但按 Rust 形态收敛）：

```text
主线程（CLI/调用方）
  │ Session::start(cfg) ──► 起 Client（WG 驱动线程，R1 既有）
  │                        起 patrol 线程（60s 拍）
  │                        起缓存落盘线程（去抖 1s）
  ▼
patrol 线程：每拍 PathProbe(10s) → NotePathAlive/link 行/补注册(5min)/
             中继升直连(5 拍 RearmSoft)/旁路探测(ProbeCandidates，8s 看门狗)/
             失败证据门(10min 窗) → 3 连败 recover_stale(R2 起跑) → 耗尽 2 次整会话重建(10min 限频)
拨号路径（files/portfwd/CLI）：healing_dial = 4s 首试 → 失败 recover_stale(R2) → 重试（余预算）
恢复阶梯：在**调用方线程**同步跑（probe/action 全是 Client RPC，本地动作恒快）；
         RecoverGate 单飞合并（Mutex+Condvar，后来者等当前轮共享 rc，不抬档）
```

- **阶梯在调用方线程同步跑**（与 Go 相同：Go 在 patrol goroutine / 拨号 goroutine 里跑）。
  预算语义照抄：先行探测 3s、动作 2s、验证 10s。Rust 的本地动作（换 socket/重建 Tunn/
  rearm）都是引擎内的快操作，不存在 Go `IpcSet` 无界阻塞的形态，`runBoundedAction`
  等价物退化为「调用方对动作 RPC 设 2s 超时，超时按 rc=-3 记」——保留返回码契约。
  超时动作的「迟到生效」与 Go 同款接受并留档（命令已入队，迟到 Rebind 会在阶梯返回后
  又换一次 socket——幂等无害；recover.go:210-220 同义登记）。
- **探测预算下沉引擎**（评审中-13）：`Cmd::Connect` 携带 per-call deadline（引擎侧到点
  abort 连接，非 caller 弃等）——「3s 先行」必须是引擎侧截止，否则残留 SYN 在下一档动作
  后落到新 socket 上「白捡」成功、污染档位归因。
- **RecoverGate（单飞合并）**（评审中-9/中-10 整改）：
  - 结果落在**轮次对象**上：`struct Round { done: Mutex<bool>, rc: Condvar 伴随, rc 值 }`
    装进 `Arc`；waiter 持 `Arc<Round>` 等自己那一轮——不会读到紧接下一轮的 rc
    （Go recoverRound 注释的 ABA 教训同防）。
  - 执行者：锁内取「当前无轮 → 置位本轮」后**立即释放锁**再跑阶梯（绝不持 gate 锁跨
    45s 阶梯——R1 纪律 3）；跑完取锁发布 rc + notify_all。
  - panic 兜底：执行侧 RAII guard（Drop 时若未发布则发布失败态并 notify）——执行线程
    panic 不让后来者死等。
  - 按 Session 实例隔离（服务域单闸）；patrol 与拨号路径同 gate。
- **Session 可变状态的所有权模型**（评审中-11 整改，防 Go `sync.Mutex` 直译）：
  - **controller（patrol 线程）独占**全部可变节拍状态：`patrol_streak / patrol_last_counted /
    patrol_last_reg / patrol_noise_since / relay_streak / ladder_exhausted / rebuild_at / state·reason·since`；
  - 拨号线程**不碰**这些字段——只投事件（经 gate 跑阶梯）+ 读快照（`Arc<Mutex<Snapshot>>`
    只读面，写者 = controller 与暖机路径）；
  - 当前世代：`Arc<RwLock<Arc<Client>>>`（healing_dial 每次拨号前取当前世代；rebuild
    换代后重试自动落到新 Client——评审中-12）；recover_stale 入口防陈旧（世代号比对，
    手里的世代非当前就不跑——Go curSession 比对同义）。
- **patrol 线程与阶梯的互斥**：patrol 触发 recover_stale 走同一 gate；拨号路径同。
  整会话重建（rebuild）在 gate **之外**（Go 评审③-1 同义：重建决策在 merge 返回后）。
  patrol 拍时间基准照 Go：**拍头**取 now/gap 并更新 last（阶梯耗时计入下一拍间隔——
  若放在阶梯后，45s 阶梯 + 60s 拍会逼近 2×60s 空窗阈值误判「挂起唤醒」，评审低-8）。

## 2. 常量映射表（connection-lifecycle.md §9 Go 核侧 → Rust）

| Go 常量 | 值 | Rust 落点（R2 新增/既有） | 语义备注 |
|---|---|---|---|
| `recoverPreProbeTimeout` | 3s | `session::recover::PRE_PROBE` | 每档探测先行（活着则零动作） |
| `recoverVerifyTimeout` | 10s | `session::recover::VERIFY` | 动作后验证（容纳首发丢包 5s 重发） |
| `recoverActionTimeout` | 2s | `session::recover::ACTION` | 本地动作预算；超 rc=-3 |
| `serviceWarmTimeout` | 12s | `session::WARM_TIMEOUT` | 暖机 = 一发出口可达探测；超时软失败继续 |
| `servicePatrolInterval` | 60s | `session::PATROL_INTERVAL` | 巡检拍 |
| `serviceProbeTimeout` | 10s | `session::PROBE_TIMEOUT` | 每拍探测预算 |
| `serviceFailStreakReset` | 3 | `session::FAIL_STREAK_RESET` | 连败进阶梯（R2 起跑） |
| `serviceDialFirstTry` | 4s | `session::DIAL_FIRST_TRY` | healing dial 首段短预算 |
| `serviceStopWait` | 6s | `session::STOP_WAIT` | Stop 等收工上限 |
| `serviceLadderExhaustRebuild` | 2 | `session::LADDER_EXHAUST_REBUILD` | 连续耗尽 → 整会话重建 |
| `serviceRebuildCooldown` | 10min | `session::REBUILD_COOLDOWN` | 重建限频 |
| `servicePatrolFailWindow` | 10min | `session::PATROL_FAIL_WINDOW` | 连败证据时间窗 |
| `serviceNoiseWindow` | 15s | `session::NOISE_WINDOW` | 本地噪声回看窗（10s+5s） |
| `regRefreshEvery` | 5min | `session::REG_REFRESH_EVERY` | 补注册周期（成功才推进时刻） |
| `relayUpgradeEvery` | 5 拍 | `session::RELAY_UPGRADE_EVERY` | 中继停留 → RearmSoft 升直连 |
| `suspendGap` | 2×60s | `session::suspend_gap()` | 巡检空窗 → R2 起跑（判据行同串） |
| `NoiseEscalateAfter` | 3min | `session::NOISE_ESCALATE_AFTER` | 本地错误长停逃逸 |
| `handoverGrace` | 10s | `wtransport::bind::HANDOVER_GRACE` | 未知来源切换的双发宽限 |
| `DirectFirst`（0→2s，<0 关） | 2s | `wtransport::bind::Config.direct_first` | 直连优先窗口 |
| `LearnedEndpointTTL` | 7 天 | `endpoint_cache::LEARNED_ENDPOINT_TTL`（既有） | 新鲜度=max(learn,verify)；tier §9「失败上限 3」基线已删（tier §11 自认），不移植〔登记：真源表陈旧行〕 |
| 接收读错误重试 | 300ms / 限流 5s | `bind`（读退避） | R1 中-7；真源 = bind.go:474-492（tier §9 引 :443-452 已漂移〔登记〕） |
| `saveDebounce` | 1s | `endpoint_cache` 落盘去抖 | FIX-16 |
| MIRROR 节流 | ≤3 行/轮 + ≥1s | 既有 | FIX-13 |
| 路径确立/切换节流 | 3s | 既有 | — |
| punchTo 节流 | 5s | `session` | hint 打洞风暴防护 |
| 域名软赛跑补投节流 | 15s | `session` | refreshDomain 补投（本期域名面裁剪，见 §7） |
| files `MaxChunk` | 256KiB | `files::MAX_CHUNK` | 客户端单帧上限（`MaxConns=16` 是服务端并发闸，客户端不落——R3 对齐） |
| files `MaxRequestLine` | 64KiB | `files::MAX_REQUEST_LINE` | 请求/响应行上限 |
| probe `MaxEndpoints` / `probePad` | 8 / 200 | `probe::MAX_ENDPOINTS` / `session::PROBE_PAD` | 端点列表段上限 / 旁路探测填充 |
| portfwd `dialTimeout` / `maxTCPFlows` | 15s / 4096 | `portfwd::DIAL_TIMEOUT` / 上限校验 | app_bridge.go:181 / tunmode.go:484 |

**前缀口径（硬规则，评审中-2 整改）**：Go 服务会话 logger = `WithPrefix("服务会话: ")`
（service.go:206），链上**全部**产出者都带前缀——本地 Go 客户端 `client.log` 真样例证实
（`服务会话: MIRROR 镜像包#1 …`、`服务会话: wgcore: 隧道侧就绪…`、`服务会话: link: via=…`）。
R1 把 wgcore/MIRROR/赛跑族按无前缀输出是**实错**（R1-design §5「bind/wgcore 行不带」系
误读）——**R2 修正：Session 的一切判据行恒带 `服务会话: ` 前缀**；无前缀形态属隧道域
（App 扩展核，R7）。INTEROP-CRITERIA 的 C10 样例随实现订正。

**判据行同串清单**（新增产出者，模板逐字——**全部带 `服务会话: ` 前缀**，见上；出处 = baseline 克隆）：
`RECOVER R1 重握手（原因=%s）：补注册 + 丢会话（保采纳）`（recover.go:181）、
`RECOVER R2 换源（原因=%s）：换本地 socket（保采纳）`（:188）、
`RECOVER R3 重赛跑（原因=%s）：清采纳，学习缓存候选兜底`（:198）、
`RECOVER 恢复于 %s（原因=%s，起跑=%s，耗时 %v）`（:201，耗时 = Go Duration 格式——既有
`fmt_duration_go_ms`）、`RECOVER 走完 R1→R3 仍未恢复（起跑=%s，原因=%s，耗时 %v）—— 交上层升级`
（:206）、`RECOVER 已恢复（%s 起查，原因=%s，零档位动作，耗时 %v）`（:155）、
`RECOVER %s 动作生效（%s 复探通过——上一发验证探测只是丢包，原因=%s，起跑=%s，耗时 %v）`（:151）、
`RECOVER %s 本地动作失败（原因=%s，rc=%d）：%v`（:223）、
`RECOVER R1 重握手（原因=%s）：补注册未发出（bind 已收工或无采纳地址）`（:168）、
`RECOVER R1 重握手（原因=%s）：丢会话失败（%v）—— 按既有状态验证`（:175——**Rust 形态豁免**：
单体重建无「移除失败/回写失败」两子态，此行不可达，登记不产）、
`wgcore: 已丢弃本地会话（peer 移除并写回）—— 下一发出站包将全新握手`（core.go:244）、
`REBIND 本地端口 → %d（Identity 不变）`（bind.go:895）、
`RARM 候选赛跑重启（直连优先：中继在 %v 后才解锁）`（bind.go:274）、
`RARM 软赛跑（中继立即参与，同时试直连）`（bind.go:289）、
`MIRROR 直连窗口 %v 内无响应 → 解锁中继候选 %d 个并补发一次`（bind.go:720）、
`⚠️ 链路走了中继（本应直连，属需排查的 bug）：ep=%v，直连候选 %d 个全未响应（可能原因：直连地址不可达 / 出口公网映射失效 / 打洞失败）`（bind.go:560）、
`路径切换：%s %v → %s %v`（bind.go:552，中继形态补 pathKind）、
`候选集更新：%d 条（%s）`（bind.go:178——bind 族；另有 hostsession 族的
`候选端点（%d 条，标记·学习=来自巡检缓存/中继 hint）：%s`〔C13，session.go:249〕——两条产出者分工：前者 = 候选集变化时刻，后者 = 建会话清单）、
`接收读错误（%v）：原地重试等换源/收工（不交回 wg-go——读 goroutine 死亡=永久失聪）`（bind.go:489）、
`发送失败：%v（%v；本地错误=该候选在本机就发不出去，与对端无响应是两回事）`（bind.go:844，**每目标 5s 限流**——R1 低-2）、
`HINT 收到对端地址线索 %s（来自 %v）`（bind.go:583）/`HINT 收到对端地址线索 %s，但没有处理器（缓存未接？）`（:585）、
`赛跑结算：胜出 %s %v（镜像 %d 包，耗时 %v）；响应过=%v；未响应=%v`（bind.go:538，中继胜出形态 `胜出 中继 …`）、
`巡检失败（连续 %d）：%v`（service.go:746/712）/`巡检空窗 %v（判为进程被挂起）—— 主动重绑本地 socket`（service.go:604）/
`连续 %d 次失败：已重绑本地 socket 并补注册（下一发探测全新握手）`（:648）/
`巡检失败被门控拦下（%s）→ 计数清零仅记录`（:736）/`需求恢复（%s）：巡检失败重新计入证据`（:743——CLI 形态无需求源，**不可达**，登记不产）/
`本地发送错误持续 %s（长停逃逸）：按质量失败计，进入正常升级链`（:719）/
`REBUILD 整会话重建（%s）：拆旧会话换新（force-stop 同机理，进程内完成）`（:800）/
`REBUILD 新会话已换入（首个出站包将重新注册+赛跑）`（:833）/
`REBUILD 整会话重建被限频（%v 内已重建过，继续观察）：原因=%s`（:791）/
`启动（无 TUN 服务会话）`（:244，C12）/`就绪（会话在位，无桥直通）`（:423，C16——CLI 形态无桥）/
`暖机就绪（出口可达，rtt=%dms）`（:405）/`暖机 %v 内出口未应答：按软失败继续（首个拨号会触发注册）`（:398）/
`暖机硬失败：%v`（:400）/
`link: via=%s ep=%s rtt=%dms（服务会话巡检）`（:640，既有——R2 起带前缀）/
`旁路探测：%d 个直连候选，应答端点列表 %d 条已入学习缓存（来源=探测线索）`（transport.go:490）/
`中继 hint %v → 重新武装候选赛跑，打一发握手兼打洞`（transport.go:172）/`打洞后探测成功：会话可能已漂移到直连（看 link 行确认）`（:189）/
`RELAY-UPGRADE：已在中继停留 %v，重新武装赛跑试直连（下一发出站包镜像到全部候选）`（tunmode.go:990——**服务域新增**：Go 该行为隧道域产出，R2 的服务会话按同节拍移植，登记为域外新增〔评审中-7〕）/
`port-forward: 127.0.0.1:%d -> %s 监听中` / `port-forward: 监听 127.0.0.1:%d 失败（%v）——该条映射不可用，不影响隧道`（app_portfwd.go:101/99）。

## 3. 2a：WG 重连语义专项（R1 头号遗留）

**R1 现象**：同身份对「含旧 peer 会话状态的出口」快速重连，握手可完成、先到的包在出口侧
解密成功、后续静默消失；客户端 decap/发包零错；wipe 配对即愈。

**Go 客户端同场景的恢复路径**（真源事实）：
1. 会话自身**不主动检测**这种半死——靠**巡检**（60s 拍 PathProbe）失败累计；
2. 单拍失败（隧道域）/ 拨号失败（服务域 healingDial 4s 首试）→ 阶梯；服务域 **R2 起跑**；
3. 阶梯动作链：R1 = `RefreshReg()`（独立 reg 帧，出口按 devTag 刷新设备记录）+
   `ResetPeerSession()`（UAPI peer 移除再写回 = 本地会话丢弃，**保采纳**）→ 下一发出站包
   全新握手；R2 = `Rebind()`；R3 = `Rearm()`（清采纳 + 学习缓存兜底）。
4. wireguard-go 侧机理：peer 移除再写回后无 keypair ⇒ 首个出站包发起 init（时戳取当前墙钟，
   单调 ⇒ 出口必接受）⇒ 新会话替换旧状态。

**Rust 对齐实现**：
- `Cmd::ResetPeerSession`（引擎内）：`self.tunn = make_tunn(同身份, 同 secret, 同 peer)`——
  boringtun 的 Tunn 即「peer+会话状态」整体，重建 = Go「移除再写回」的等价物（新随机
  index 前缀，评审 ②-2 既有纪律）；**采纳/reg 状态都在 Bind，不受影响**（保采纳）。
  判据行同串 `wgcore: 已丢弃本地会话（peer 移除并写回）—— 下一发出站包将全新握手`。
- **与 R1 既有 `rebuild_tunn_once`（ConnectionExpired 引擎自愈）的关系**（评审中-4 整改）：
  - 两条路径**共用同一个引擎内 `rebuild_tunn()`**（唯一重建点：换 Tunn + 保采纳 + 补注册）；
  - expired 路径 = **引擎兜底**（boringtun 特有义务：wireguard-go 自管 rekey，Go 侧无对应物），
    登记为「不产 RECOVER 行」——它打自己的行（`wgcore: 会话过期已重建…`）；
  - 阶梯动作**清 `expired_pending` 位**（重建后 expired 事实已消化）；阶梯验证探测期间
    引擎不再抢先重建（位已清，明文包到达才复位——语义不变）；
  - 归因口径：若 expired 兜底先于阶梯触发并恰好救活，下一拍巡检成功 ⇒ 阶梯先行探测通过 ⇒
    打「RECOVER 已恢复（…零档位动作…）」——归因正确（Go 对「上一发验证探测丢包」同款处理）。
- **Go 两段 UAPI vs Rust 单体重建差异表**（评审中-5 整改）：

  | 维度 | Go（peer 移除再写回） | Rust（重建 Tunn） | 影响 |
  |---|---|---|---|
  | 排队包 | `flushStagedPackets` 丢弃 | 重建丢队列 | 等价（验证探测驱动新握手） |
  | 失败子态 | 移除失败=原会话仍在 / 回写失败=无会话（:175 判据行为它存在） | 单体构造无失败面（OOM 即 panic） | **:175 行 Rust 不可达**，登记形态豁免 |
  | allowed_ip 空窗 | 移除↔回写间短暂无路由 | 无（对象整体替换） | 无影响，登记 |
  | allowed_ip 范围 | 0.0.0.0/0 + ::/0 | boringtun 内置全路由 | 等价 |

- R1 档动作后的**驱动**：阶梯的验证探测（PathProbe）本身就是「下一发出站包」——probe 的
  SYN 经 encapsulate 无会话 ⇒ init + 排队 ⇒ 恢复秒级。无需额外触发。
- **复现与验证方案**（判据 = 不 wipe 快速重连后 speedtest/对账恢复；评审低-4 加判定谓词）：
  1. `tools/local-exit.sh start`（不 wipe）→ Rust 客户端连接 + speedtest（基线绿）→ stop 客户端
     （出口保留 peer 会话状态）→ ≤5s 内重连（同 `--identity-dir`）；
  2. **复现判定三条件**（满足即复现）：①连续 3 发 PathProbe 全超时（60s 巡检或手动）；
     ②出口侧 peer 记录在位（`peer: ~ dev=…` 或台账）；③出口日志无「新握手完成」行
     （有握手完成而仍失败 = 另一类故障，单独归因）；
  3. 复现后让巡检跑（或拨号触发 healingDial）→ 期望序列（服务域 R2 起跑 ⇒ **先补 R1 动作**，
     评审低-5）：`服务会话: RECOVER R1 重握手（原因=拨号失败/巡检连续失败）：补注册 + 丢会话
     （保采纳）` → `服务会话: RECOVER R2 换源（…）：换本地 socket（保采纳）`（若 R1 动作即
     救回则直接 `RECOVER 恢复于 R1`〔起跑=R2 归因到补齐档〕）→ speedtest 恢复；
  4. 差异分析（哪一档救回、耗时、出口侧行为）记入 docs/reviews/R2.md；
  5. **若不复现**：提高概率手段 = SIGKILL 客户端（跳过 stop 收尾，出口侧保留半开会话与
     NAT 态）后重连；采样 ≥3 轮；仍不复现 → 按「未能复现」登记（记录测试参数与轮数），
     以阶梯时间窗实测为替代判据（§9），不阻塞 2b–2g。

## 4. 2b：恢复阶梯

`session::recover` 模块（结构对齐 `runRecoverLadder`，Rust 形态——评审中-17 整改：enum 承载
返回码与动作，不搬 Go 的裸 int/三 bool）：

```rust
pub enum Level { R1 = 1, R2 = 2, R3 = 3 }   // recoverLevel；clamp(R1..=R3)；显式判别值（CLI/NAPI 面）
#[non_exhaustive]
pub enum LadderRc {                          // 替代 Go 的裸 int（0/-1/-3/-4；i32 只在 CLI 边界转换）
    Recovered(Level),
    Exhausted,                               // -1：走完 R3（交上层整会话重建）
    ActionTimeout,                           // -3
    ActionFailed(String),                    // -4
}
#[non_exhaustive]
pub enum Action { RefreshReg, ResetPeerSession, Rebind, Rearm }
pub trait LadderTransport {                  // 动作面收敛为单一 apply（假实现只实现一个方法）
    fn apply(&mut self, a: Action) -> Result<(), String>;
    fn note_path_alive(&mut self);
}
pub fn run_ladder(deps, from: Level, cause: &str) -> LadderRc
```

逐常量/逐行对齐点：
- **只能向上补齐**：`BTreeSet<Level>`（已执行档）去重（直入 R3 先补 R1/R2 动作——
  **服务域 R2 起跑时 R1 动作先执行**，期望序列见 §3）；
- **探测先行 + 归因到上一档**：复探通过打「动作生效（…上一发验证探测只是丢包…）」，
  同档（lvl==from）打「已恢复（…零档位动作…）」；
- **验证通过 → NotePathAlive**（采纳地址落已验证——endpoint-freshness C-2）；
- **耗时 = Go Duration 格式**（Round(1ms)，既有 `fmt_duration_go_ms`）；
- **返回码契约**：`Recovered/Exhausted/ActionTimeout/ActionFailed`（-2 无 attached 隧道是
  隧道域概念，服务域不产——登记）；
- **单飞合并 gate**：`merge(from, cause, run)`——执行中触发等当前轮（per-round Arc，§1）；
- **耗尽计数按轮记**（run 回调内 +1 / 恢复清零 / 巡检确认健康也清零）→ 到 2 触发
  `rebuild_session`（限频 10min；旧会话 Client.stop + 缓存 Save → 新 Client 装配换入
  `Arc<RwLock<Arc<Client>>>` 当前世代）。

**入口与起跑档**（服务域语义，service.go）：
- 拨号失败（healingDial 4s 首试失败）→ `recover_stale("拨号失败")` R2 起跑 → 重试（余预算）；
- 巡检 3 连败（证据门推进后）→ `recover_stale("巡检连续失败")` R2 起跑 + 判据行
  `连续 %d 次失败：已重绑本地 socket 并补注册（下一发探测全新握手）`；
- 巡检空窗（>2×60s）→ `recover_stale("挂起唤醒")` + 判据行同串；
- 隧道域入口（单拍失败 R1 起跑/换网 R3 起跑）是 **App 扩展侧**概念——Rust 本期无扩展，
  以 `Session::recover(from, cause)` 公开面 + CLI `--recover-from N` 测试钩子覆盖
  （时间窗实测用，见 §9）。

**时间窗预算**（时延记账，对齐 connection-lifecycle.md §3）：
每档未命中烧满 3s 先行 + 10s 验证 ≈13s；命中档 = 3s + 动作(≤2s) + 握手往返（典型 <1s，
首发丢包等 5s 重发）。⇒ R1 命中典型 ≤4s；R2 ≈16s；R3 ≈29s；最坏 ≈45s。

## 5. 2c：漫游/换源 + 端点缓存全量

### Bind 扩展（`wtransport::bind`）

1. **Rebind**（换本地 socket，保采纳）：新 `UdpSocket::bind(0.0.0.0:0)` 非阻塞 → 换入 →
   关旧。**驱动线程 poll 的 fd 必须跟随**：driver 循环每轮从 engine 取当前 fd（R1 的
   `udp_fd` 是启动时快照——R2 修为每轮 `engine.bind.socket().as_raw_fd()`，开销可忽略）。
   判据行 `REBIND 本地端口 → %d（Identity 不变）`。
2. **Rearm 家族**：`rearm()`（硬赛跑：valid=false、reg armed、race_start=now、
   relay_unlocked = 无直连候选、mirror 配额复位、race_seen 清空）；`rearm_soft()`
   （软赛跑：中继立即参与）。判据行同串。R3 档 = rearm + 候选重投（cache.merge）。
3. **handover 双发**（FIX-09）：稳态下切到「未知来源」（非候选/非中继）→ 旧路径保留为
   次要发送目标 10s（尽力双发，不进发送计数）；切到已知来源/首采 = 清过渡。
4. **候选集动态更新**：`set_candidates()`（SetCandidates 同义）——sameCandidates 忽略序
   比较（relay 位参与）；变化打 `候选集更新：%d 条（%s）`；relayEps 同步重建。
5. **读错误退避**（R1 中-7 + Go bind.go:474-492）：drain_udp 遇非 WouldBlock 错误 → 记
   `recv_backoff_until = now + 300ms` + 限流 5s 打一行同串判据 → 退避期内跳过收包。
   **poll 超时参与退避**（评审低-20）：持续 POLLERR 形态下 poll 立即返回、250ms 钳制失效，
   驱动超时 = `min(poll_delay, 250ms, backoff 余量)`——退避窗内不空转（Go sleep(300ms) 同义）。
   收工类（socket 已换新）自动接新。
6. **发送失败限流**（R1 低-2）：每目标（SocketAddr）5s 一条，同串；计数分类导出
   （`local_err_total`/`local_err_adopted`——Go localErrCount/adoptedLocalErrCount）+
   `local_send_err_within(d)`（巡检噪声判定数据源，Go LocalSendErrWithin 同义）。

### 端点缓存落盘（`wtransport::endpoint_cache` 扩展）

- **落盘格式字节对齐 Go**：`<dir>/<peerID hex>.json`，`{"peer":"<hex64>","entries":[…]}`
  （Go `endpointFile`：Peer omitempty、Entries omitempty；entry：endpoint/source/learnedAt/
  verifiedAt omitempty——serde `skip_serializing_if` 复刻 omitempty，键序 = 声明序 = Go
  结构体序）。数字 i64 毫秒。**同数据 ⇒ 与 Go 字节相同**（对照测试用 Go 向量钉住）。
- **原子写 + 内容未变不写 + 写前重读合并**（D8）：tmp 名唯一（pid+nanos）→ rename；
  合并语义 = 磁盘上本实例没有的条目并入（同址以内存为准）。
- **去抖落盘线程**（FIX-16）：`schedule_save()` 非阻塞投信号（cap=1 合并）；落盘线程等
  信号 → 1s 去抖窗 → save。Session stop 时收口（最终一次同步 Save）。
- **消费者接线**（R1 低-8）：
  - hint 帧（Bind 收包路径）→ `cache.observe(Hint)` → `set_candidates(cache.merge(&static))`
    → schedule_save → `punch_to(addr)`（节流 5s：RearmSoft + 内部拨 :1 打洞 + 判据行）；
  - 巡检旁路 `probe_candidates()`（probePad=200，4s/候选并发，消费卫兵 probeAddrAcceptable：
    全球单播、非私网/链路本地/回环、非 fake-IP 198.18/15、非 CGNAT 100.64/10）→
    `observe(Probe)`；
  - 真实往返（拨号成功/PathProbe 判活/阶梯验证通过）→ `mark_round_trip(addr)`（同址 1h
    去重）→ `mark_verified`；**中继采纳路径直接 return**（transport.go:374-377/385-388——
    中继地址不落已验证，评审低-19）；来源判定：地址在 static 集内 = `SourceToken`，否则
    `SourceHint`（transport.go:411-417）；
  - **markSessionReady**：栈 B 拨号成功即标（session.rs 在 healing_dial 成功路径调）；
  - **merge 带 Relay 位**（评审低-19）：`EndpointCache::merge(&[Candidate]) -> Vec<Candidate>`
    ——学习地址一律 direct，但同址在 static 里是中继条目时沿用 Relay 标记
    （endpointcache.go:236-256 逐条；R1 的裸 SocketAddr 签名随此重构）。
- **probe 协议**（pkg/probe 明文一问一答）：`"HWQ"‖ver(1)‖type(1)‖nonce(8)‖pad` /
  `"HWR"‖…‖buildLen(1)‖build‖flags(1)?‖epCount(1)?‖epCount×[16B 4in6‖2B BE port]`。
  实现 `probe::ping_ex`（旁路探测 + 端点列表 + 能力位）与 `probe::respond` 判别（客户端
  只做请求方；respond 用于测试桩）。响应 ≤ 请求+45B 不变量只影响服务端，客户端 pad=200。

### 域名条目（裁剪登记）

Go 的 DomainEndpoint/重解析/15s 软赛跑补投（transport.go D5）——**本地实例 token 恒为
IP 字面量端点**，R2 裁剪：`token` 解码保留域名字符串端点并解析一次（`to_socket_addrs`），
**不做** Rearm 时重解析与补投节流。登记为已知裁剪，R5 矩阵/真机前补（挂账到 R3+）。

## 6. 2d：中继腿

**拓扑**：中继（Go `homeway relay` 本地实例）监听 UDP；出口持 rl1 token 向中继注册
（UDP 注册腿 + TCP 控制面）；客户端 token 里 type=1 端点 = 中继地址。

**客户端协议面**（bind.go + frame.go 真源）：
- 出站到中继候选：`[0xAA][relayID(8B)] ‖ 已编码腿帧`（relayID = `proto.RelayID(peerID)` =
  SHA-256(peerID)[..8]——`frame.go:36` 实现照抄）；**容器帧在内、路由头在外**（有 reg 搭车时
  = `[0xAA][id]‖[容器帧[reg][data]]`，`EncodeTaggedFrame` 包已编码帧——bind.go:733-746 同序）。
- 入站：中继回包**从中继主 socket 发出**（assoc.go:220 `r.pc.WriteToUDPAddrPort(pkt, client)`，
  hint 帧 :235 同源）⇒ 客户端看到的源地址 = token 里的中继端点 ⇒ `relay_eps` 集合判 relay 腿。
  **4in6 归一**（Go `unmap`，bind.go:494）：v6 承载下 `[::ffff:1.2.3.4]:p` 归一为 v4 再比对，
  否则 relay 判定恒假（评审低-6）。
- 采纳语义：`adopted_is_relay = relay_eps.contains(src_unmap)`；采纳/镜像/结算与直连同构；
  via=relay；中继胜出/切入打 ⚠️ 告警行（同串）。
- **DirectFirst 窗口（照 Go 重写，评审高-1/中-6 整改）**：
  - `rearm()` 起 `race_start`；窗口内（默认 2s）只打直连候选；无直连候选 = 立即解锁；
  - **捕获 `(pkt, reg)` 二元组**：本轮**首个**未采纳出站包的副本 + 该发搭车的 reg 报文
    （Go `lastReg := regPkt` + `append(buf)`，bind.go:668-672——**reg 必须随补发重投**：
    中继是唯一可达路径时出口从未收到 reg，裸 data 帧的握手 init 会被按未知 pubkey 丢弃，
    且 `reg_armed` 已消费 ⇒ 本会话再无补注册手段 ⇒ 中继腿永远建不起来）；
  - `unlock_once` 位保证每轮只捕获一次；解锁时（窗口到点且仍 `!valid && !relay_unlocked`）
    → `relay_unlocked = true` + **以容器帧 `[reg][pkt]` 重投到全部中继候选**（判据行同串）；
  - 实现：驱动线程 pump_once 每轮检查解锁到点（≤250ms 节拍精度足够——补发时机差异
    ≤250ms，登记；Go 用 timer goroutine，语义等价）。
- **中继升直连**（评审中-7 整改）：patrol 拍计数 via==relay 连续 5 拍 → `rearm_soft()` +
  **立刻发一发 PathProbe(10s)**（rearm 只重武装，这发出站包才是「镜像出去试直连」的载体——
  只 rearm 不发包 = 升级动作空转）+ 判据行 `RELAY-UPGRADE：已在中继停留 %v，重新武装赛跑
  试直连（下一发出站包镜像到全部候选）`（tunmode.go:990）。**登记：服务域新增**——Go 该
  行为隧道域（tunmode.go:984-996）产出，R2 的服务会话按同节拍移植（域外新增，判据行同串）。
- **hint 打洞**：中继在起会话时给客户端发 hint 帧（后端公网地址）→ §5 的 hint 链路。

**本地测试设施**：`tools/local-relay.sh`（新）：
- `start [n]`：`homeway relay --state /tmp/homeway-rs-relay-n --listen :42741+n --advertise 127.0.0.1:port`
  （rl1 token 从日志取；`homeway relay` 无 token 子命令——cli.go 明确报错）；`stop/token
  （grep 日志）/wipe` 同 local-exit 形态；
- local-exit.sh 加 `start` 的 **flag 追加入口**（环境变量 `EXIT_EXTRA_FLAGS`，评审低-12——
  E9 的 `--peer-ttl` 与本节的 `--relay` 共用）+ `start-with-relay [n]` 组合命令（起中继 →
  取 rl1 → 起 exit 带 `--relay <rl1>` → token 并入中继端点 type=1）；
- 端口族错开：中继 42741+（现役阿里云 41741/launchd 无关——本机隔离靠 /tmp state +
  端口错开，与 R0 同纪律）。

**判据与「压制直连」注入**（评审中-8 整改）：本地出口 token 默认带回环/LAN 直连端点且即时
应答 ⇒ `via=relay` 永远不出现。注入形态：**出口公布死直连端点**——`EXIT_EXTRA_FLAGS` 传
`--public-endpoint 127.0.0.1:<死端口>`（如 42699，无监听）：token 仍带该直连端点但全部
不可达；中继→出口腿走注册腿/控制面（不依赖公布端点）⇒ DirectFirst 窗口烧满 → 解锁中继 →
`link: via=relay` + `MIRROR 直连窗口…解锁中继…` + ⚠️ 告警行 + 经中继腿 speedtest 跑通
（吞吐无判据下界，连通即证）。

## 7. 2e：files / portfwd / probe 巡检

### files 客户端（`files.rs` + CLI 动词）

- **协议**（proto.go 逐条）：问候帧 `{"ok":true,"root":…,"ver":1}\n` → 每命令一行 JSON
  （≤64KB）→ 响应行；流式 = `[4B BE len][payload]`，len=0 终止帧；动词 list/stat/mkdir/
  read/download/write。
- **每命令一条流**（Open→命令→Close）；Open 失败 = `stream_open` 稳定码（CodeStreamOpen）。
- **download 断读收口**（FIX-40）：终止帧到达时对照响应行 size——少收报
  `下载不完整：服务端声明 %d 字节，实收 %d`；多收容忍。
- **write 关流即取消**：错误/取消路径不发终止帧直接关流（服务端删 .tierpart）；
  正常路径 帧→终止帧=提交 → 读提交结果行。
- **错误码词表**：invalid_arg/invalid_name/not_found/permission/is_dir/already_exists/
  op_failed/canceled/server_busy/stream_open（thiserror 枚举 + code() 面，R1 低-7 类型化）。
- **retryTx 语义**（tier 侧 FilesTransferController 的重试，客户端库面）：Rust `Session`
  层提供 `healing_dial` 承载（拨号失败自愈一次再重试）——Go 的 files 流本身无重试
  （重试在 App 层），Rust 对齐为「拨号面 healing，命令面不自动重放」（重放有重复副作用
  风险，client.go 注释同义）。
- **CLI 动词**：`homeway-cli files --token … [--identity-dir …] list|stat|mkdir|read|download|upload <args>`
  （download/upload 支持 >100MB 样本与 sha256 对账）。
- 拨号目标 = 隧道 IP:7802（`DialTCPPort(FILES_PORT)`——files 端口常量 7802，
  `docs/agents/constants.md` 单一真源）。

### portfwd 客户端（`portfwd.rs` + CLI 动词）

- `homeway-cli portfwd --token … --map 15432:1.2.3.4:5432 [--map …]`：
  每条起 `127.0.0.1:<listen>` TCP 监听 → accept → `session.dial(target)`（TargetIp 空 =
  `DialTCPPort(targetPort)` 拨出口本机；否则 DialTCP 任意目标）→ 双向泵（每连接一线程）。
- 状态面：`listening | failed`，bind 失败码 = `bind_failed`（Go `portfwd.ErrCodeBindFailed`
  值串查 pkg/portfwd）；单条失败不阻断其它映射（判据行同串）。
- 整表热替换语义在 facade trait 面预留（2f），CLI 只做一次性表。

### 60s 巡检（Session.patrol，§1 已列）

- **会话状态机**（评审中-15 整改，对齐 service.go:78-86 + run/finish 全路径）：
  `Idle | Starting | Ready | Failed | Stopping`；暖机两条出口：超时 → 软失败继续
  （判据行同串 + 桥照常起）；**其它错误 → `暖机硬失败：%v` + finish(Failed)**（判据行同串）；
  Stop 在途置 Stopping（收工上限 6s）；产出 `启动（无 TUN 服务会话）`（C12）与
  `就绪（会话在位，无桥直通）`（C16）行。
- **失败证据门**（评审中-14 整改——**取门控路径**，对齐桌面门 `notePatrolResult`：
  CLI 形态最接近 daemon 客户端，且 `local_send_err_within` 在 2c 已有数据源）：
  - 五分支真值表（PatrolEvidenceGate 纯函数）：成功拍清零 / localNoise 清零 / 无需求清零
    （**CLI 无需求源 ⇒ demand 恒 true，此分支不可达**——`需求恢复（…）` 行登记不产）/
    10min 窗作废 / 正常计数 +1；
  - localNoise = `bind.local_send_err_within(15s)`（探测窗内有采纳路径本地发送错误）；
  - 长停逃逸（NoiseEscalated，3min）按质量失败计（判据行同串）；
  - 门控态进入打 `巡检失败被门控拦下（%s）→ 计数清零仅记录`（noise 形态可达）。
- 每拍（**拍头**取 now/gap 更新 last——§1 低-8）：PathProbe(10s) → 成功：link 行 +
  NotePathAlive + mark_ladder_healthy + 证据清零；失败：证据门推进 → 3 连败
  recover_stale("巡检连续失败") + 判据行 `连续 %d 次失败：…`；
- 每拍附带：补注册（ShouldRefreshReg 5min，成功才推进）；中继升直连（5 拍，§6）；
  旁路探测 ProbeCandidates（**8s 总看门狗**——超时即放弃本轮，结果只进缓存；
  登记：Go 该 8s 看门狗在隧道域 tunmode.go:969，服务域移植同款）；
- 暖机（Session::start 内）：12s 一发探测，超时软失败继续 + 首拍 link 快照；
- 空窗检测：patrol 线程被冻结后唤醒（间隔 > 2×60s）→ recover_stale("挂起唤醒")。

## 8. 2f：facade 预留 + 状态 JSON 契约

- **`facade.rs` trait 面**（不接 NAPI，R7 实装）：
  `TunFacade { fn prepare(&self, cfg) -> Result<Generation>; fn attach(&self, fd, mtu) -> Result<()>;
  fn status_json(&self) -> String; fn stop(&self) -> i32; fn recover(&self, from: i32, cause) -> i32;
  fn running(&self) -> i32; fn set_port_forwards(&self, json) -> i32; }`——语义注释对齐
  probe_lib.go 的 rc 契约（prepare 0/-1/-2/-3；attach 0/-1/-3/-4/-5；stop 0/-1/-2；
  recover 0/-1/-2/-3/-4/-9 钳位）。Session 提供第一个实现（服务形态：prepare=装配；
  **attach 恒 -1**（无 ready 世代可接 TUN——服务形态无 TUN 面；-3 是「fd≤0 参数错」，
  语义不符，评审低-16）。
- **状态 JSON**：`status_json::snapshot_json(&SessionSnapshot) -> String` 对齐
  `serviceSnapshotJSON`（app_service.go:82-113）**逐键逐缺省**：state/reason 恒有；
  elapsedMs 随 Since；bridge* 随桥（CLI 无桥 = 缺省）；link 在时 identity 绑定出现
  （Go：identity 嵌在 `if snap.Link != nil` 内）；stats 随会话在位（`if snap.Stats != nil`，
  不预设必在——评审低-17）。**键序**：Go `json.Marshal(map)` = 字典序 ⇒ Rust 用
  `serde_json::json!`/`Value`（BTreeMap 序）并在对照测试**断言键序**（不只键集合，
  评审低-18）。idle 短路 = `{"state":"idle"}` 逐字节。
- **对照测试**：Go 侧真源产出 = `homeway host status <name> --json`（daemon 真服务会话，
  local-exit client-start/client-add 后取）——对照 Rust `homeway-cli connect --status-json`
  同场景产出：link.via/link.ep（at/rttMs/elapsedMs 时间类豁免）、identity.dev/pub、
  state、stats 键集合 diff 为空。**tunStatusJSON（隧道形态完整键面：meowed/readyBy/
  portForwards/demand/exitIp/tunIp…）R7 实装**——无 TUN 的形态产不出隧道键面，本期
  对照面 = serviceSnapshotJSON（link/identity 段逐字段对齐，即任务判据的「link/identity
  段」口径）；**ROADMAP R2 判据行需随此改写**（「tunStatusJSON 与 Go 版快照 diff 为空」→
  「状态 JSON（serviceSnapshotJSON 形状）与 Go 快照 diff 为空（时间类字段除外）；tunStatusJSON
  完整键面归 R7」），在 R2 收口提交里同步（评审中-16）。

## 9. 测试与判据实测计划

**单测**（每步随行）：
- recover：假 transport（动作序列注入；常量 `#[cfg(test)]` 缩短——Go 用 var 同义）；
  档位补齐/归因/单飞合并（per-round rc）/耗尽计数/LadderRc 契约；
- bind：relay 信封编解码字节（对照 frame_test golden）、DirectFirst 解锁补发（**含
  「只有中继可达 + DirectFirst 开启」形态**——评审高-1 验收）、handover 双发、读退避、
  发送失败限流、候选集更新、中继告警行；
- endpoint_cache：落盘 JSON 与 Go 字节对照（向量由 vecgen 增族——`tools/vector-gen` 模板
  加 endpointcache 段）、合并/去抖语义、merge 的 Relay 位沿用；
- files：行 JSON/4B 帧/终止帧/**golden 补采**（fixtures 现无 files 样例——vecgen 同批
  从 Go `WriteLine/WriteFrame` 真源产出向量族，评审低-11）+ 断读收口 + write 关流取消；
- speedtest（2g）：reason 码面（busy/link_down/not_supported/timeout/cancelled/interrupted）。

**实测**（全部真实入报告）：
1. **恢复阶梯时间窗**（三档分别注入，评审中-18 整改——每档注入后 `--recover-from 1`
   直测，计帐起点 = ladder 起点到 `RECOVER 恢复于` 行）：
   - **R1 命中（≤4s）**：注入 = **重启出口进程**（`local-exit.sh stop && start`，同 state
     同端口——出口 WG 会话清空、peer 表（devTag 台账）在位；客户端会话陈旧但采纳地址活）
     ⇒ R1 动作（RefreshReg+ResetPeerSession）即救回；
   - **R2 命中（≈16s）**：注入 = **客户端 socket 失效**（CLI 测试钩子 `--inject poison-socket`：
     引擎命令把 UDP socket 置为已关形态——模拟冻结唤醒后 OS 作废 socket；登记为测试缝，
     Go 集成测试注入假 transport 同义）⇒ R1（保采纳，死 socket）败 → R2 Rebind（新 socket）
     过；
   - **R3 命中（≈29s）**：注入 = **出口换端口 + 学习缓存兜底**：`local-exit.sh stop` →
     `EXIT_EXTRA_FLAGS`/`start` 用 `--listen <新端口>` 同 state 起（身份不变）；客户端旧
     采纳/静态候选全指向旧端口 ⇒ R1/R2 败 → 预热学习缓存（跑一发旁路探测：出口 probe
     应答带新公布端点 → cache.Observe）→ R3 rearm 候选 = cache.Merge(static) 含新端点 ⇒
     赛跑获胜（正是「学习缓存候选兜底」语义）；
   - **最坏档（≈45s）**：注入 = 出口停机不重启（全候选死）⇒ 走完 R1→R3 全预算；
   - 首发握手丢包的样本 R1 可 >4s（等 5s 重发）——**已登记形态，不判失败**（评审低-13）。
2. 不 wipe 快速重连恢复（2a 判据，§3 方案与判定谓词）；
3. `link: via=relay` + 经中继 speedtest（local-relay.sh 拓扑 + 死直连端点注入，§6）；
4. files 上传/下载 ≥100MB 字节对账（sha256 双侧，偏差 0）；
5. 状态 JSON 对照（§8 方案）；
6. E9（出口 peer TTL/淘汰行——注入 = `EXIT_EXTRA_FLAGS='--peer-ttl 30s'` 短值 + 等
   3 连败窗口，评审低-12 的 flag 追加入口）/ C11（RECOVER 全族——阶梯实测顺手采）/
   E12（UDP 拨号面——**本期客户端仍无 UDP 拨号面**（Go 客户端 L3 直通后也无），Rust
   客户端无 TUN ⇒ 无法造应用 UDP。**登记：E12 归 R3**（Rust 出口侧拦截层测试时从出口侧采）。

## 10. R1 移交 14 项处置表（2g 范围；评审低-15 订正计数）

| 项 | 处置 |
|---|---|
| 中-3 镜像失败限流（剩余面） | 2c：每目标 5s 限流同串（低-2 同源） |
| 中-7 读错误退避 | 2c |
| 中-9 speedtest reason 码面 | 2g：enum 归因（busy/link_down/not_supported/timeout/cancelled）+ 写失败 2s 短读窗 probeReport |
| 中-10 热路径三处（wg_buf memset / TX 队列 Arc-Mutex / write RPC 拷贝） | 2g：①去 clear/resize ②TX token 借用式 ③Write 借用面（改动后复测吞吐锚） |
| 中-10/低-8 端点缓存消费者接线 | 2c |
| 低-2 发送失败文案/限流 | 2c |
| 低-4 report 手解逗号截断 | 2g：引号感知扫描 |
| 低-7 字符串错误（normalized/transit_dial 等） | 2g：thiserror 类型化 |
| 低-9 hint 无处理器形态 + UTF-8 校验 | 2c：按实况选变体（接缓存后恒「有处理器」，保留两分支测试） |
| 低-10 已建立连接无 idle 超时 | 2g：smoltcp `set_timeout`（对齐 Go wgnet 拨号超时面；巡检/阶梯依赖探测超时，不复依赖此） |
| 低-11 上行无 250ms 分片泵 | 2g：补 live 字节与取消检查点（engine.go:695-712 形态） |
| 低-12 payload_len u16 截断 + FrameReader 死分支 | 2g：加校验/删分支 |
| 低-13 身份三支文案 | 2g：照 session.go:190-203 同串 |
| 低-15 ② reg/frame golden 向量族 | 2g：vecgen 增 reg/endpointcache/files 族（①③④ 随各项顺手） |

（低-1/3/5/6/14 已在 R1 收口轮整改，不在本表——低-8 端点缓存接线**在**本表〔2c〕，评审低-14 订正。）

## 11. 风险与退出口

| 风险 | 缓解 | 退出口 |
|---|---|---|
| 2a 现象复现不了 | §3 的提高概率手段（SIGKILL 保半开会话）+ 判定三条件；仍不复现 → 按格式登记「未能复现」，以阶梯时间窗实测为替代判据 | 不阻塞 2b-2g |
| DirectFirst 解锁补发的 250ms 节拍误差 | 判据只看行产出与最终采纳，窗口 2s ≫ 250ms | 登记 |
| boringtun 会话重建后出口仍不回（非会话死形态） | 阶梯自动升 R2/R3；实测覆盖 | — |
| relay 本地拓扑起不来（rl1 注册腿/TCP 控制面） | local-relay.sh 按 exits.md 形态配；失败先查出口日志 `中继：后端 … 注册` | 挂账 R4（届时 Rust 中继本体要做） |
| R3 注入的学习缓存兜底不生效（probe 应答端点未入缓存） | 先单验 probe 协议往返（旁路探测行/落盘 JSON）；再走 R3 | 挂账实测轮 |
| smoltcp set_timeout 语义与预期不符（abort vs 静默） | 单测钉住（超时后 state 迁移） | 用拍检查回收兜底 |
| 落盘 JSON 与 Go 字节不同（omitempty/键序） | vecgen 增族钉字节 | — |
| 吞吐回归（热路径改借用式） | 改后复测 speedtest（±50% 判据不变） | 回滚该项保留登记 |

## 12. 评审重点（v1 所列，v2 已处置）

1. 常量映射表逐条（中-1/低-1/低-2 已修：MaxConns 拆行、files/probe/portfwd 族补齐、
   真源表陈旧行登记）；
2. 2a 恢复语义对照（中-4/中-5/低-4/低-5 已整合：统一重建点 + 差异表 + 判定谓词 +
   R1 补齐期望序列）；
3. 中继腿信封（高-1/中-6/中-7/中-8/低-6 已整合：(pkt,reg) 捕获重投、升直连补
   PathProbe、死直连端点注入、4in6 归一）；
4. 阶梯线程模型（中-9/10/11/12/13/低-7/低-8 已整合：per-round rc、锁纪律与 panic
   兜底、所有权模型、世代换新、探测预算下沉、迟到生效登记、拍头基准）；
5. files 协议边界（评审确认无问题；golden 向量族补齐）；
6. Go 直译痕迹（中-17 已整合：LadderRc enum + apply(Action) 收敛）。

## 13. 评审整改记录（v1 → v2）

技术评审渠道 = `dsh --profile headless`（`/tmp/dsh-review/r3.djAAHk/`，意见全文存
output.md）。**1 高 / 15 中 / 20 低**，处置：

- **高-1（整改）**：DirectFirst 解锁补发补 reg 搭车——照 Go 捕获 `(pkt, reg)` 二元组、
  解锁以容器帧重投（§6）；单测加「只有中继可达 + DirectFirst 开启」形态。
- **中-1（整改）**：MaxConns/MaxChunk 拆行，files/probe/portfwd 常量族入表（§2）。
- **中-2（整改）**：前缀口径硬规则入 §2——真样例证实全部行带 `服务会话: ` 前缀，
  R1 实错修正（C2/C4/C5/C6/C10 族）。
- **中-3（整改）**：C12/C13/C16 + 暖机硬失败行入判据清单（§2）。
- **中-4/中-5（整改）**：统一 `rebuild_tunn()` + expired 兜底不产 RECOVER 行 + 归因口径 +
  Go 两段 UAPI 差异表 + :175 行形态豁免（§3）。
- **中-6（整改）**：DirectFirst 照 Go 首包副本 + once 位 + fire 时判 valid 重写（§6）。
- **中-7（整改）**：中继升直连补 PathProbe 载体、行号订正 tunmode.go:990、服务域新增登记（§6）。
- **中-8（整改）**：via=relay 验收的死直连端点注入（§6）。
- **中-9/中-10（整改）**：gate 结果落 per-round Arc + 执行者即释放锁 + RAII panic 兜底（§1）。
- **中-11（整改）**：Session 所有权模型（controller 独占可变态 / 拨号线程只投事件读快照）（§1）。
- **中-12（整改）**：`Arc<RwLock<Arc<Client>>>` 当前世代 + 拨号前取当前 + 入口防陈旧（§1）。
- **中-13（整改）**：`Cmd::Connect` 带 per-call deadline（探测预算下沉引擎）（§1）。
- **中-14（整改）**：证据门取门控路径（五分支真值表 + localNoise 数据源 + 不可达分支登记）（§7）。
- **中-15（整改）**：会话状态机补全（硬失败/Stopping/词表/C12/C16 行）（§7）。
- **中-16（整改）**：tunStatusJSON 对照面改 serviceSnapshotJSON 的判据改写，登记在
  §8 + R2 收口时同步 ROADMAP（主会话核对该提交）。
- **中-17（整改）**：LadderRc enum + apply(Action) 收敛动作面（§4）。
- **中-18（整改）**：三档分别注入方案（重启出口 / poison-socket 测试缝 / 换端口+缓存兜底 /
  停机最坏档）（§9）。
- **低 1-20**：全部采纳（常量族/陈旧行登记/省略号补全/判定谓词/R1 补齐序列/4in6/
  迟到生效/拍头基准/行号/dialTimeout/表自洽计数/attach -1/stats 条件/键序断言/
  MarkRoundTrip 中继跳过与 Relay 位/退避 poll 超时）。
- **豁免 1 项**：中-16 的最终判据改写属主会话权限——本设计先按「link/identity 段对齐」
  实现（与主会话任务指令的口径一致），ROADMAP 判据行的正式改写在 R2 收口提交里同步，
  如主会话否决则补产 tunStatusJSON 完整键面（需要 TUN 桩）。
