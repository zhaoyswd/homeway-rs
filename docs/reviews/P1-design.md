# P1 技术设计：出口发送路径管线并行化（浅拆——密文→sendto 独立成线程）

> 开工前技术评审输入（ROADMAP「评审协议」第 1 道门）。范围 = PERF-AB §9.10-§9.12
> 下行瓶颈归因链的主剩余项：出口驱动线程（R1 决议单线程 + poll(2) 三唤醒源）串行
> 做五道工序——收包/解封/smoltcp/封装/逐包 sendto，**sendto 占驱动线程 78% 样本**
> （§9.3 profile，线程内占比——**进程 CPU 未饱和是已知口径**，见 §0 收益模型），
> 下行倾泻期间上行 ACK 消费被排后（自时钟回路互卡的假设面），发送节奏被 1ms 拍
> 钉死（40MB/s 时每包预算仅 31µs）。本批拆分发送路径为独立线程。
> 版本落点 = v0.2.2（tag 纪律照 release.yml：改码→commit→推 main→tag 在 main 头上）。
>
> **v2 = 技术评审（dsh，2026-10-06）整改后版本**：3 高危（I-1 收益模型/G-1 SIGPIPE/
> C-1 join）+ 19 项中低全部处置（处置表见 `docs/reviews/P1.md` 文末）；本版本即
> P1 实现基线，再偏离须回评。

## 0. 范围与形态

- **目标**：驱动线程（收包/解封/smoltcp/**整形放行/封装**）产出密文包 → 无锁有界
  队列 → 发送线程（pselect 微秒级唤醒 + 单轮有界批量 sendto；Linux 出口 sendmmsg）。
  三个收益独立成立：① sendto 不再占驱动线程（ACK 消费不等发送批）；② 发送节奏
  脱离驱动线程 poll 拍（排空循环微秒级连续、单轮团块上界保持）；③ 批量化有归宿
  （sendmmsg 在发送线程不再与收包争拍）。
- **收益模型（评审 I-1 整改后的诚实版）**：§9.3 的 78% 是**线程内占比**、出口进程
  CPU 未饱和（离满载差两个量级）——「sendto 阻塞 ACK 消费」的**延迟剂量从未
  直接测过**；§9.10 ④ 的剩余归因还有第二个因子（拍粒度×空口交织 + 接收端 ACK
  时钟）。因此本批：**(a) 判据面以机制门为主**（T1 剂量前后对照 / T3 队深 / T4
  上行不回归 / T5 聚合生效），吞吐门（夜间带 B ≥35MB/s、B/A ≥0.8）与「带数据链
  的架构结论」同列——判红即按 §7 预登记路径归因，不事后解释；**(b) P1b-0 前置
  剂量基线轮**：先在现状代码加「发送面耗时」插桩（send_wire wall-time 累计，
  5s 行），本地 4265x 跑 speedtest 拿「sendto 占驱动线程 X ms/s」剂量——拆分收益
  的直接对照面；**(c) 无收益分支预登记**（§7 T2 判红 ⇒ 发送循环按时戳散布
  〔§9.10 ④ 对症面〕立项为下一档——浅拆恰好把发送循环交出来，该形态的插桩面
  本批已备好）。
- **拆分点 = 浅拆**（任务书首推）：封装（device.encapsulate + 腿帧帧化）留驱动线程，
  只搬「密文→sendto」。理由：Tunn 单状态机零共享（encapsulate 是 `&mut self`——
  搬走即两线程共享状态机或跨线程命令化，锁面大增）；腿表/判定面是驱动线程独占
  状态（#17 丢弃/腿 socket 派发）——判定留驱动线程，发送线程拿到的每条队列元素
  已是「可执行的发送动作」；sendto 是 78% 的大头，收益主体先落袋。
  深拆（封装也搬 + 短临界区互斥）**本批不做**，只在浅拆后 profile 显示封装成新
  瓶颈时再立项（§8 退出口）。
- **明确不做**：拦截层整形器（令牌桶 + pacing 时刻表，R8 §九/§十二）**原地不动**
  ——放行决策（哪些明文本拍可发）仍在驱动线程，语义零变化（过载态语义边界见
  §4）；**发送线程内不做逐包 pacing/时刻表**（8r 全套机制保留在驱动线程整形器，
  它决定放行时刻；发送线程在放行后微秒级连续发出，不添第二级节奏器——两级
  节奏器 = 职责漂移禁区，§4 H-1）；手机核不在本批（tier 只读，纯出口侧）。

## 1. 现状盘点（改动点的精确位置）

驱动线程 `driver_loop`（`server/engine.rs`）一拍内串行五道工序：

```text
① 命令 try_recv → ② poll/pselect 等待 → 腿可读 + recv_packet drain
   → handle_inbound（regs→设备表；data→device.decapsulate → send_wire① ←解封应答）
   → ③ intercept.pump()（栈 poll ×2 + tx_out + tx_shape_release 放行）
   → route_encap（device.encapsulate + send_wire② ←主体下行）
   → ④ device.tick_timers → send_wire③ → ⑤ 周期任务/5s 插桩
```

sendto 的**三个调用点**全在 `ServerBind::send_wire`（`server/bind.rs`）：
`handle_inbound` 内（解封应答/握手响应）、`route_encap` 尾（下行主体）、
`tick_timers` 后（keepalive/重传）。`send_wire` 内部分派：腿表命中 → 腿 socket
直发（稀疏路径）；#17 丢弃（腿已摘）；主 socket → staging 帧化连排 →
`udpbatch::send_batch`（Linux sendmmsg ≤64/批、macOS 逐包 sendto）。

主 socket 的全部发送面清单（评审 B-3 补全）：send_wire 主路径（本批搬）、
STUN 请求（`stun_query` inline）、**probe 应答（`process_packet` 内 inline）**、
`send_raw_to`（3e 面）——后三者量小，留驱动线程 inline 直发（UDP 逐数据报原子性
+ 不同 dst 无同流序约束；同 socket 并发发送先例 = relay-leg 线程，`engine.rs:515`）。

## 2. 拆分形态拍板

```text
驱动线程（homeway-serve-drv，现状名不变）：
  收包/解封/smoltcp/整形放行/encapsulate/腿帧帧化/腿表派发/#17 判定/STUN/probe
  → 主 socket 路径：帧化产物（适配族后的 dst + wire 字节）入 SPSC ring
  → 唤醒（socketpair 上 send 1 字节，MSG_NOSIGNAL；pending 位合并，次序不变量见下）
发送线程（homeway-serve-tx，新增）：
  pselect(wake_fd)（空窗长眠无超时——收工靠 stop 唤醒字节；5s 观测行由驱动线程打）
  → 排空循环：单轮取队 ≤ burst(256KiB) 字节 → send_batch（Linux sendmmsg 64/批 /
    macOS 逐包）→ 记账（私有 + 共享原子）→ 队列有余量 ⇒ 立即下一轮（真实工作，
    非自旋）；队列空 ⇒ 回 pselect 长眠
```

工序归属表：

| 工序 | 拆分前 | 拆分后 |
|---|---|---|
| 收包/解封（recv/decapsulate） | 驱动 | 驱动（不变） |
| smoltcp 栈 poll（intercept.pump） | 驱动 | 驱动（不变） |
| 整形放行（tx_shape_release/shape_slice） | 驱动 | 驱动（不变——R8 不变量原地；ring 高水位背压见 §4） |
| 封装（device.encapsulate + encode_frame） | 驱动 | 驱动（不变——浅拆承诺） |
| 腿 socket 发送 / #17 丢弃 | 驱动 | 驱动（不变——稀疏路径零共享） |
| STUN 请求 / probe 应答 / send_raw_to | 驱动 inline | 驱动 inline（不变，量小） |
| **主 socket 密文 sendto** | 驱动 | **发送线程（本批）** |
| tx_bytes/批形态统计/直方图 | 驱动（ServerBind 字段） | 发送线程（共享原子快照，§3④） |

**发送节奏脱离 poll 拍的体现**：驱动线程投递密文后立即回 poll 收包（ACK 消费
不再等 sendto 批完成）；发送线程的排空 = 入队事件驱动（唤醒粒度可达微秒级），
排空循环内单轮 ≤ burst、轮间无拍隙——发送执行不再与驱动线程的 1ms/5ms 拍绑定，
**同时单轮团块上界与整形器单拍上界同值（256KiB）**（评审 A-3：多拍积压一次倾泻
= §9.10 修掉的形态，不允许回归）。整形放行节奏（拍粒度）本身不变——这是浅拆的
有意边界；「发送循环按时戳散布」是 T2 判红时的下一档（§0 收益模型 (c)）。

## 3. 必答五题（评审门要求）

### ① 无锁有界队列选型与背压语义（满时丢弃必须 TCP 友好、不丢整形语义）

- **选型 = 自建 SPSC 有界 ring**（单生产者 = 驱动线程、单消费者 = 发送线程）。
  **不引新依赖**（R1 决议依赖面纪律：crossbuf/crossbeam 不进——`udpbatch` 的
  unsafe 自建先例同款纪律，~140 行 + 单测钉死序/满/空/丢失唤醒）。SPSC 是唯一
  需要的形态：发送路径天然单生产单消费。
- **unsafe 不变量（评审 F-1，写进实现注释）**：
  - 索引语义：head/tail 单调 `usize`（不回绕），槽位 = `index & (CAP-1)`——
    CAP=4096=2^12 恰为 2 的幂；
  - 原子序配对四点：producer `write(slot) → tail.fetch_add(1, Release)`；
    consumer `tail.load(Acquire) → read(slot) → head.fetch_add(1, Release)`；
    producer 复用槽前 `head.load(Acquire)`——写槽与复用判定之间无第三者（SPSC）；
  - 槽 = `MaybeUninit<Slot>`，`Slot { dst: SocketAddr, buf: Vec<u8> }`；
  - 伪共享：head/tail 与各自 cached 游标分 `#[repr(align(64))]` 段（producer
    缓存 **head**〔回收游标〕+ 独占 tail；consumer 缓存 **tail** + 独占 head
    ——方向按「缓存对端才需要 Acquire 重读」）；
  - Drop：consumer 停止后 producer 侧 Drop 逐槽 `assume_init_drop`（未消费
    元素不泄漏；**无 clear() API**——评审 C-4：consumer 活着时 clear 是数据
    竞争，且收工路径不需要它）；
  - 满 = `tail - cached_head == CAP`（push 返回 false，不写槽）；空 =
    `cached_tail == head`（`len()` = 单调差快照，producer 侧读为近似上界——
    高水位背压用近似值即可）。
- **容量 = 4096 槽的口径（评审 A-1 诚实版）**：**工程估算而非上界**——按主用
  形态（4 流测速 × FLOW_TX_BUF 1MB/流 ≈ 3200 密文包）留余量；**四个已知例外
  不受 TCP 窗记账**：(a) UDP 过境/DNS 应答/ICMP 回显（R8 §九 自述不受窗约束）；
  (b) RTO/快重传是**新入队元素**（in-flight 不增、队列元素增）；(c) 流数上限
  MAX_CONNS=1024 ≫ 4；(d) 高 BDP 形态 cwnd 可超 1MB/流。⇒ **溢出是设计内丢弃
  路径**（见下），且常态有两级前置背压（§4：栈 buffer → 整形 FIFO → ring），
  T3 观测门（稳态队深峰 ≪ 200 包）监控估算是否失真。容量与 FLOW_TX_BUF/
  MAX_CONNS 的推导关系写进 `txring.rs` 头注释。
- **满丢策略 = 丢新（队尾拒绝，push false）+ 计数 + 节流行**（首 3 次 + 此后每
  1000 包一行，**含 dst**——评审 A-2：多 peer 归因面）。**绝不丢队头/队中**
  （乱序禁区）。TCP 友好性（评审 A-2 认账版）：丢新对单流 = 尾丢（dup-ACK/RTO
  恢复，网络路径丢包的自然形态）；**但丢的是整形器已放行、TCP 已计入在途的
  额度**——「放行 ⇔ 线上」配对在过载态断裂，这正是两级前置背压（§4）把満丢
  压到「发送线程病态才发生」的原因；共享队列的队头阻塞无按流份额（R8 §九
  同款限制的叠加，小流可被压 ~队深/排空率，稳态队深门控制其量级）；握手应答/
  keepalive 与数据同队（分列/保留位的复杂度不配满丢常态=0 的稀有度——首丢
  记行含 dst 已可归因，5s 级重试放大是接受语义）。
- **整形语义不丢（常态）**：整形器放行决策（令牌桶 credit + pacing 时刻表 +
  补账量子，R8 全套不变量）上游于队列；正常态「放行 ⇔ 最终发出」一对一（ring
  只是已放行包的短暂驻留，两级背压保证）。**队列不参与整形决策**——它不是
  第二级整形器（过载态的语义边界清单见 §4 H-1）。
- **入队所有权**：驱动线程每包产 `Vec<u8>`（帧化直写）move 入队，发送线程
  move 出发完即弃。每包 ~1.3KB 一次分配（39k 包/s ⇒ ~0.3% CPU 量级）——跨线程
  所有权移交的最简形态；现状 `send_wire` 的 `&InboundOut` 签名在主 socket 分支
  改为按值取出 wire 项（借用语义对腿分支不变）；**Vec 回收 ring / device 侧
  预留帧头复用原 Vec**（评审 E-3）登记 §8，本批不做（后者触 device/wire 前缀
  不变量，改动面出浅拆边界）。

### ② FIFO 保序（批内乱序会加重手机端重排）

- SPSC ring 单生产单消费天然全局 FIFO；批量取队按 slot 序 move，批内序 = 入队序。
- `send_batch` 现状语义保序：sendmmsg **前缀语义**（短返余量本拍丢弃不重发——
  同 peer 不会后发先至）；首错即弃余量（有意语义，评审 r1-F8 在册）。原样复用。
- 唤醒合并不乱序：socketpair 字节只是信号（EAGAIN = 已 pending），包序唯一
  真源是 ring。**唤醒次序不变量（评审 G-2，写死）**：
  producer = `push → if !pending.swap(true, AcqRel) { send(1B, MSG_NOSIGNAL) }`；
  consumer = `pselect 醒 → 读干 socketpair（while recv > 0）→ pending.store(false)
  → 取队排空到空 → 无包才回 pselect`。次序颠倒的两类故障（残留字节空转 /
  丢唤醒睡死）由丢失唤醒 soak 单测钉死；读干写法沿用 `pool.rs:155` 先例。
- **降级切换（评审 B-1 整改版）**：`TxMode` enum（`Queued | Inline`）是
  ServerBind 字段（驱动线程独占），**一次性切换**——驱动线程发现发送线程失活
  （alive=false 或 spawn 失败）时：置 `TxMode::Inline` → drain ring 存量 →
  内联 send_batch → 本批同批发出。切换后 send_wire 恒走内联（不再入队）——
  切换点串行化（单线程做切换 + 排空 + 发出），无并发乱序窗口；「读到旧 alive
  → 入队 → 失活 → drain 之间包躺 ring」的窗口由 TxMode 切换语义消除（切换即
  drain）。**panic 时发送线程已 pop 未 send 的批是真丢**（洞非乱序，量 ≤ 单轮
  burst 256KB，TCP 重传恢复）——接受语义，降级行记「在途批放弃」计数。
- **endpoint 迁移窗口**（评审 B-2）：同 peer 旧路径（主 socket）在 ring 里的
  存量包 vs 新路径（腿）的直发包之间存在超越窗口——常态（ring 近空）= 微秒级；
  积压态可放大到 ~队深/排空率（百 ms 级）。**量化**：WG 重放滑窗 2048 包 ≫
  单轮 burst 200 包，乱序重排窗在协议容忍内；该窗口写进已知限制口径。
- 腿路径（驱动线程直发腿 socket）与主 socket 路径（发送线程）是不同 socket、
  不同路径——镜像/腿冗余本就是多路径并发形态（协议内在容忍乱序采纳）；STUN/
  probe inline 直发同 socket 但不同 dst——UDP per-datagram 原子性保证不交错
  损坏，无同流序约束。

### ③ 发送线程生命周期 / panic 收口（RunGuard 纪律沿用）

- **在世位**：`tx_alive: Arc<AtomicBool>`，**初值 false**（评审 C-3：spawn 失败
  不许恒 true），线程体首指令置 true + AliveGuard（任何出口含 panic 展开都清零
  ——`driver_alive` 同款纪律）。spawn 失败（Err）⇒ 直接 `TxMode::Inline` +
  一次性记行（发送面回拆分前形态，隧道不中断）。
- **panic 收口 = 降级不重启**：发送线程异常退出 ⇒ alive=false ⇒ 驱动线程下一拍
  `send_wire` 检测失活走 §3② 降级分支 + 一次性记行（`⚠️ 发送线程异常退出
  （已降级内联发送；ring 残量已排空 N 包）`）。**不自动重启**：降级态即安全态
  （吞吐回旧基线），重启逻辑的复杂度/风险不配触发稀有度（panic 只可能来自新
  代码自身 bug——发版要拦的事，非运行时自愈）。
- **SIGPIPE 面（评审 G-1 高危整改）**：唤醒原语 = **socketpair(AF_UNIX,
  SOCK_DGRAM) + send(MSG_NOSIGNAL)**（`pool.rs:301` 同款先例——本仓 main 已把
  SIGPIPE 恢复默认处置，裸写死管道 = 进程死）；两个 fd 的**生命周期锚定
  ServerBind**（裸 `RawFd` 字段随对象 Drop 关闭），发送线程只拿裸值不持所有权
  ——发送线程 panic 展开不会 close 任何 fd，驱动线程的 send 永不面对 EPIPE/
  SIGPIPE（socketpair 双端都在 ServerBind 手里）。不用 Darwin 无绑定的 `pipe2`
  （评审 G-3）。
- **收工序（评审 C-1/C-2 整改版）**：驱动线程宽限循环（pump_grace → route_encap
  → 入队）结束、`intercept.close()` 前：置 tx_stop + 写唤醒字节 → `join`
  （**无超时**——与 `engine.rs:781` 驱动线程 join 同款）。**join 必返论证**：
  发送线程全部阻塞面 = pselect（stop 唤醒字节可醒）与排空循环（真实工作）；
  send 面是非阻塞 socket（EAGAIN 短返丢弃，不长阻塞）⇒ stop+唤醒后至多一轮
  排空（≤ 队列存量/排空率，4096 包×1.3KB ≈ ms 级）即返。**收工总上界 = stop_grace
  + ms 级 join**（评审 C-2：无独立 drain/join 双预算叠加；发送线程退出语义 =
  **drain-then-exit**——收到 stop 先排空剩余再退，宽限期尾数据不丢）。超时/
  放弃路径**不存在**（detach 会泄漏 dup fd → socket 占端口 → 下次启动端口漂移
  → token 端点漂移——评审 C-1(b) 的风险面，直接不采纳）。
- **阻塞面承诺**（R1 三条死锁纪律延续）：驱动线程入队 **永不等待**（满即丢）；
  发送线程无锁可阻塞（ring + socketpair 都是 syscall 边界，无 Mutex）；双方都
  不持任何锁跨 channel/阻塞调用。

### ④ 唤醒记账与观测（新线程的唤醒/排空深度进插桩面）

- **观测行由驱动线程打**（评审 D-2 采纳推荐项：驱动每拍必醒、窗口最稳；发送
  线程 pselect 长眠无超时，空闲但存活时本就不该打行——liveness 由降级行承担）。
  5s 窗语义、空闲静默（与「UDP 出站」行同拍口径）：
  `serve: 发送线程 唤醒+N 排空+X包/Y轮(均Z包/唤醒,R KiB/轮) 队深峰P包 深度分布
  [log2直方图] 满丢D(+d)`。
- **口径标注（评审 D-1）**：`tx_calls` 语义从「每次 send_wire（每拍 1-3 次）」变
  「每次排空轮」——现有「UDP 出站」行的均批/峰值/桶分布**不可跨批直比**（PERF-AB
  历史数字带「拆分前口径」注记，本批 PERF-AB 新节落新基线）；新行字段面以
  「轮」为分母并标注。
- 记账迁移（评审 D-3 收敛）：`tx_bytes/tx_calls/tx_pkgs/tx_batch_max/tx_hist/
  tx_dropped` 迁发送线程，经 `Arc` 共享原子暴露（驱动线程 5s 读一次做行——
  窗口语义字段用 `swap(0)` 取）；腿路径直发记账留 ServerBind 分列
  （`tx_bytes_leg`）；`bind.rs` 既有单测断言面同步改名。
- **发送面耗时插桩（P1b-0 前置，收益剂量的直接对照面）**：现状 = send_wire 的
  wall-time 累计（驱动线程被发送占住的 ms/s，5s 行）；拆分后 = 入队+判定耗时
  累计（应骤降）+ 发送线程排空耗时（其自身负载）。改前先跑本地基线轮拿剂量。

### ⑤ 与 R1 决议的演进关系声明

R1 决议「单 WG 驱动线程独占 Tunn/Interface/UDP socket，无锁热路径」的本意是
**Tunn 单状态机的 `&mut self` 串行性**（§1.1）与**依赖面纪律**——不是「永远
单线程」的教条（R3 已加 DNS worker 池/拦截 worker 池/relay-leg 线程/观测线程，
均为「无锁交接」的平行线程）。P1 的演进保持 R1 不变量：

- 驱动线程**仍独占** device（Tunn）/拦截栈/设备表/腿表/ServerBind 判定面——
  收包/解封/封装热路径零新增锁；
- 发送线程**独占主 socket 的发送执行面与 ring 消费端**；**fd 共享语义（评审
  E-1 改口径）**：dup fd 共享同一 socket 内核态（SO_SNDBUF/内核发送缓冲/源
  端口/钉卡状态）——由驱动线程**单方变更**（repin 看护调用在驱动线程内），
  不构成数据竞争，但存在「已入队包的发送环境在排空前被 repin」的决策后环境
  变化窗口（与 §3② endpoint 迁移窗同族，量级同受重放窗容忍）；socket 内核态
  有内核级串行化，多 fd 并发 send 安全（relay-leg 先例）；
- **锁面（评审 E-2 限定）**：数据面热路径零锁——ring（无锁）+ socketpair
  （syscall 边界）；观测面复用既有 `Logf`（内部 Mutex，与现有多线程日志同款）；
  **隐藏共享态清单**：socket 内核态（上述）、`Logf` 内部锁、`PSELECT_BROKEN`
  退化位（`engine.rs:939`——**分面**：驱动/发送各自 static，发送线程版退化 =
  排空循环间 `thread::yield` + 记行，不混用驱动位的「退 poll」语义）、
  `udpbatch::NO_BATCH` OnceLock（只读，安全）；
- R1 三条死锁纪律逐条延续（③ 阻塞面承诺）；收工纪律（stop 位 + 唤醒 + 无超时
  join）同款。

## 4. 与 R8 整形的关系（不变量保持 + 过载态语义降级清单）

- 令牌桶（R 200MiB/s / burst 256KiB = 单拍放行上界）、pacing 时刻表（est×1.2/
  补账量子/50µs 量化下限）、FIFO 滞留队列、宽限全量释放（pump_with_flush）、
  off 臂直通——**全部原地不动**（`intercept/mod.rs` 零改动面）。
- **仍成立的不变量**（ring 不注入字节）：任意窗口 w 内线上深度 ≤ burst + rate·w
  的速率类上界；FIFO 保序；整形器不注入流量。
- **过载态语义降级清单（评审 H-1 认账）**：ring 存量使「单次到达网络的团块」
  可超整形器单拍上界（发送线程一轮排空 ≤ burst 钳住下界语义：每轮一团 ≤256KiB，
  但轮间无拍隙 ⇒ 连续团流）——**8r pacing 的按拍散布在发送线程侧被「连续排空」
  部分抵消**（pacing on 时放行节奏仍由驱动线程时刻表决定，发送线程只跟随放行，
  不主动合并多拍——单轮 ≤ burst 只是上界防御，稳态放行小批时发送批 = 放行批）；
  「放行 ⇔ 最终发出」配对在 ring 满丢时断裂（§3① 两级背压压制到病态才发生）。
  **判据面**：稳态队深峰/分布、单轮发送字节、满丢计数（§3④ 新行全覆盖）。
- **两级前置背压（评审 A-4 采纳）**：ring 深度 ≥ 高水位（3840 = 15/16 容量）时，
  驱动线程本拍 `tx_shape_release` 退化为「只并入不释放」——包留在整形 FIFO
  （不丢、对 TCP 是真背压、FIFO 深度本就被 R8 建模观测），满丢成为最后兜底；
  ring 深度经共享原子供驱动读（`tx_shape_wait` 不改——高水位检查在
  tx_shape_release 调用点，不进拦截层〔拦截层零改动的承诺保持〕）。整形 off 臂
  无 FIFO ⇒ 满丢是该臂唯一兜底（消融态接受）。
- 驱动线程等待原语（poll/pselect 两档 + tx_shape_wait 提示 + FD_SETSIZE 防御 +
  PSELECT_BROKEN 退化位）不变——pacing on 时驱动线程仍按 µs 拍跑时刻表，醒来
  后不再做 sendto（省下时间给 ACK 消费）。

## 5. 实装拆步（模块与测试映射）

| 步 | 模块 | 内容 | 测试 |
|---|---|---|---|
| P1b-0 | `bind.rs` 插桩（改前） | 发送面耗时累计 + 5s 行；本地 4265x 基线轮（speedtest × N）拿「sendto 占驱动线程 ms/s」剂量 | 手跑（数字入 PERF-AB 新节） |
| P1b-1 | `server/txring.rs`（新，~140 行） | SPSC ring：`push(Slot) -> bool` / `pop_batch(&mut Vec<Slot>) -> usize` / `approx_len`；容量 4096；无 clear | 单线程序/满/空；双线程压满-排空往返序一致（10k 包 soak）；丢失唤醒 soak（慢生产者/消费者错拍）；Drop 无泄漏（计数 Vec 析构） |
| P1b-2 | `server/bind.rs` | `send_wire` 拆分：判定面（腿/#17/族适配/帧化）+ 入队 or 降级内联；`TxMode` 状态位；socketpair 唤醒 fd（生命周期锚 ServerBind）；`HOMEWAY_TX_SENDTHREAD` 消融臂（值匹配）；发送面耗时插桩拆两段（入队侧/排空侧） | 回环端到端：入队→发送线程→sink 收齐保序；降级路径（置 Inline 后直发 + 存量排空 + 在途批计数）；腿路径不变（既有测试）；高水位背压（填 ring → 断言放行停滞 + FIFO 持有） |
| P1b-3 | `server/engine.rs` | 发送线程 spawn/收工（§3③）；观测行迁移 + 新行（驱动线程打）；`ServeEngine` 面板字段 | stop 后 drain-then-exit（入队 N 包 → stop → sink 全收）；spawn 失败 → Inline + 记行 |
| P1b-4 | 插桩收口 | §3④ 全部观测面 + PSELECT_BROKEN 发送面分面 | 手跑核（真机轮判读用） |

消融臂：`HOMEWAY_TX_SENDTHREAD`（值匹配：未设/on/1/true = 开〔产品默认〕，
off/0/false = 关〔= 拆分前串行形态，真机 A/B 对照臂〕）——与 `HOMEWAY_TX_SHAPING`
同惯例（presence 语义会把消融方向搞反，r2 自补1 教训）。

## 6. 验证面（反过拟合三约束延续）

1. **单测**（每步配，上表）。
2. **harness 三臂（慢/中/快）**：发送线程面的排空能力 + 背压正确性——
   - 慢臂（2.5MB/s 形态）：sink 人为节流，入队 4096+ 包 ⇒ 断言：到达序号 =
     连续前缀（满丢丢新）、满丢计数 >0、无 panic、高水位背压在整形 FIFO 形态
     下满丢 = 0；
   - 中臂（深队列 24MB/s 形态）：回环全速 sink，10k 包入队 ⇒ 全收 + 保序 +
     满丢 = 0；
   - 快臂（120MB/s 形态）：回环全速 + 大批注入 ⇒ 发送线程排空吞吐 ≥ 内联
     send_batch 基线的 0.7×（唤醒开销不吞批化收益——0.7 门与 pacing 三臂同
     惯例）。
   既有 `pacing_three_path_arms` / `downlink_lossy_link_recovery`（拦截层面）
   原样复跑——本批不动拦截层，它们是「不回归」判据。
3. **真机 2×2 同刻**（FMR0224116011480，本地 Rust exit 4265x）：改前/改后出口 ×
   直连锚（Go exit 42643 同刻 A 臂）交错轮（R,G,R,G,R,G 同刻口径，§9.12 形态）；
   判据 = **机制门为主**（发送面耗时前后对照 / 发送线程行 / tcp 观测行 / 整形
   观测行 / 上行不回归）+ B/A 比值前后对照（参考目标：夜间带 B ≥35MB/s、
   B/A ≥0.8，或带数据链的架构结论——剩余瓶颈新归因 + 下一档路径）；日间带
   同样跑（比值口径，绝对数字带时段限定）。
4. **消融数据每步配**：真机每臂配 `HOMEWAY_TX_SENDTHREAD=off` 对照轮（拆分收益
   的直接归因面）+ 整形 on/off 既有臂不回归；P1b-0 的改前剂量基线是消融链首环。

## 7. 可证伪预测（先登记后测；机制门为主）

| # | 预测 | 判红处置 |
|---|---|---|
| T1 | **剂量**：拆分后驱动线程「发送面耗时」从基线 X ms/s 骤降（≤ 1/4）；发送线程排空耗时 ≈ 原基线量级（工作量转移而非消失） | 未降 ⇒ 拆分点错（sendto 未真正离开驱动线程——查降级误判/路径遗漏） |
| T2 | 热态 B 较同刻 off 臂提升 ≥15%（sendto 让出的线程时间转为 ACK 消费收益） | 未达 ⇒ 剂量数据链归因：若 T1 达标而吞吐不动 ⇒ ACK 延迟非绑定约束 ⇒ 发送循环按时戳散布（§9.10 ④ 对症）立项下一档，本批插桩面已备好 |
| T3 | 满丢常态 = 0 且稳态队深峰 ≪ 200 包（p50 ≈ 0） | 非零 ⇒ 容量/背压论证失真，查发送线程排空能力与两级背压接线 |
| T4 | 上行不回归（≥ 同带基线） | 回归 ⇒ 降级排查（唤醒写在驱动线程热路径的成本） |
| T5 | Linux 出口（阿里云）sendmmsg 批均 ≥8 包（64 上限的聚合生效） | 未聚合 ⇒ 查唤醒粒度（逐包唤醒 = 投递拍过细——入队合并策略复核） |

## 8. 风险与退出口

| 风险 | 缓解 | 退出口 |
|---|---|---|
| SPSC ring 的 unsafe 内存安全 | §3① 逐点不变量 + soak/Drop 单测；评审 unsafe checklist（udpbatch 先例） | 实现困难 ⇒ 退 `Mutex<VecDeque>` + try_lock（**代价：锁进热路径，与无锁声明冲突，须回评**） |
| 发送线程 panic 未被察觉（静默降级吃掉收益） | 降级一次性记行 + 发送线程新行字段停更即信号 | — |
| 唤醒 send 在驱动线程热路径加 syscall | pending 位合并（本拍已有 pending 则跳过 send）+ socketpair DGRAM 不落盘 | profile 显示成本 ⇒ 合并窗口加大（批尾一次） |
| 浅拆后封装（encapsulate）成新瓶颈（§9.3 的 78% 移除后剩余） | T1 剂量插桩天然暴露（发送面耗时降后驱动拍耗时分布）+ T2 判红数据链 | 深拆立项（封装搬发送线程 + 短临界区） |
| 收工竞态（队列残留 / join 卡死） | §3③ 收工序（stop+唤醒+无超时 join 必返论证）+ drain-then-exit 语义 | — |
| Vec 每包分配成本 / device 侧 Vec 复用 | 预期 0.3% CPU 量级；T1 剂量行可见 | Vec 回收 ring 或 device 预留帧头复用（评审 E-3——后者触 wire 前缀不变量，出浅拆边界须单独评审） |

## 9. 评审重点（预答 checklist）

1. **Go 直译痕迹**：无接口仿写/无字符串错误/enum 承担形态（`TxMode`）；`Arc`
   原子快照是最小共享面（非 Mutex 化 metrics）。
2. **unsafe 面**：仅 txring 的 MaybeUninit 槽位（§3① 四点原子序 + Drop 语义进
   注释）——比 sendmmsg 的批量 unsafe 面小。
3. **行为对齐**：wire 字节零变化（同一帧化代码路径）；腿/STUN/probe 路径零变化；
   整形器零变化。唯一可观测行为变化 = 发送时序（本批目标本身）与新增观测行。

## 10. 实装偏离记录（P1b 收棒口径，2026-10-06 深夜；定位批更新见文末）

- **等待原语**：设计写 pselect 微秒节奏，实装 = **poll(-1) 长眠 + 排空循环事件驱动**
  （发送线程无需亚毫秒定时唤醒——排空节奏由事件驱动自带；设计 §2 的"pselect"
  面由此不必要，PSELECT_BROKEN 分面问题随之消失）。
- **harness 快臂**：macOS 回环内核面不可靠（60k 大包形态 flaky 挂死——inline
  臂 tx socket 默认 ~9KB SNDBUF 的流控坑已修〔4MB 同产品〕，更大包量仍偶发
  卡内核流控），移除并以本地引擎端到端 + 真机终验为快臂口径（登记在
  bind.rs 测试注释）。

### 定位批更新（2026-10-06 接棒批；详见 docs/reviews/P1.md「P1 定位记录」）

- **控制面直发（send_wire_ctl）已删**：该偏离是对「队列路径握手应答客户端
  拒收」的误诊缓解——真根因 = 唤醒合并位 `pending` 在 `tx_start` 里被 new 了
  两个实例（生产侧 swap / 消费侧 store 各打各的旗 ⇒ 首次唤醒后永无唤醒字节，
  首排空窗之外的包永驻 ring）。修复后全量 Queued（本设计 v2 形态：控制面
  同队）恢复，握手应答走 ring 实测全通（客户端 warmup pong 判据=wg）。
  同批修复：`alive` 预置 true（spawn 成功 ↔ 线程首指令窗口内驱动线程误判
  失活 ⇒ 降级 join 永等的启动竞态）+ `tx_degrade` join 前 stop+唤醒
  （join 必返论证在降级路径补全）。
- **产品默认 off 维持**：根因已修、本地全绿（434 lib 全绿 + 本地引擎级
  speedtest 全绿 + P1c 本地臂 T1/T3/T4 过），但 P1c 真机 2×2 同刻消融未跑
  ——收益未经终验的行为不上产品默认，P1c 全绿后翻 on 发 v0.2.2。
