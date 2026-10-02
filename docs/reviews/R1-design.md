# R1 技术设计（客户端垂直切片：Rust 客户端 ↔ 本地 Go 出口）

> 开工前技术评审输入（ROADMAP「评审协议」第 1 道门）。**v2 = 技术评审（dsh，2026-10-02）
> 整改后版本**：评审 1 高危设计项全部整改（见文末「评审整改记录」）；本版本即 R1
> 实现基线，再偏离须回评。评审原始意见与逐条处置见 `docs/reviews/R1.md` 第一道门。

## 0. 范围与形态

- **目标**：Rust 测试客户端（`homeway-cli connect --token <hmw1>`）与 `tools/local-exit.sh`
  起的本地 Go 出口（baseline 621fe0e 构建）建立 WG 隧道，跑通数据面判据：
  出口 `peer: +` / 客户端 `warmup pong: 就绪（判据=wg）` / `link: via=direct ep=…` /
  speedtest 双向吞吐与 Go 客户端同量级（±50%）/ 出口 `intercept: tcp transit` 行
  （**须有专门产出步骤**，见 §5 1e——PathProbe/speedtest 都是隧道 IP 目标，出口只产
  `exempt` 行；transit 要经隧道拨**非隧道 IP** 的目标）。
- **客户端形态** = 无 TUN 的栈内客户端，对齐 Go 侧「服务会话」形态：
  smoltcp 栈 B（隧道 IP 上拨 TCP）+ WG noise（boringtun）。APP 形态（TUN fd、
  prepare/attach 两阶段）是 R7。
- **明确不做**（R2 范围）：漫游/换源、中继腿、恢复阶梯档位（注意：roadmap 的「R1 期」
  ≠ 恢复阶梯的「R1 档」）、端点缓存**落盘**（本期内存版——任务指令明确要求，评审建议
  移出挂 R2，按指令保留最小实现）、60s 周期巡检（**一次性巡检**打出 C10 行——Go 侧
  C10 巡检形态的天然产出者是 60s 巡检，本期以一次性巡检替代，频率偏离登记、R2 归位）。

## 1. 架构决策：异步运行时选型 —— **不引入异步运行时，自管线程 + 唤醒多路复用**

**结论**：R1 数据面用「一条 WG 驱动线程 + `poll(2)` 唤醒多路复用 + unbounded mpsc 命令」，
**不引入 tokio/async**。

理由（评审后保留的成立项）：

1. **会话形态是单会话单生命周期**：boringtun 0.6 的 `Tunn` 是同步独占状态机
   （`&mut self` 的 `encapsulate/decapsulate/update_timers`），UDP 单 socket 读写天然
   串行，async 无 IO 重叠收益。
2. **依赖面纪律**：tokio 全家显著膨胀 cdylib 体积（R7 红线 1.0–1.5MB；PoC 的 1.0MB
   实测不含 tokio）。`libc` 已在依赖树（Cargo.lock 既有项，经 getrandom 等），
   `poll(2)` 不引新依赖。
3. **smoltcp 0.11 是轮询模型**（`Interface::poll` + SocketSet），线程 + 定时唤醒是它的
   自然形态；异步化需自封装 waker，多一层胶水。
4. **R2+ 扩展点不受堵**：恢复阶梯/巡检是定时动作，落在驱动线程节拍循环；R7 napi
   层自带线程。

**唤醒原语（评审 ①-1 整改）**：WG 线程的等待点 = `poll(2)` on {UDP fd, self-pipe}；
主线程投命令时写 self-pipe（`wake`）。超时 = `min(iface.poll_delay(), 250ms)`（smoltcp
的延迟 ACK 10ms/窗口更新/RTO 全由 poll 驱动，超时取 `poll_delay` 让栈定时器不迟到）。
**唤醒源三个**：socket 可读 / 定时到点 / 命令到达。

**三条死锁纪律（评审 ③-3 整改，硬约束）**：
1. 主线程→WG 线程的命令通道必须 **unbounded**（或 WG 侧只 `try_recv`，绝不阻塞）；
2. WG 线程绝不阻塞在任何 channel 上（通知方向 unbounded，宁可背压内存也不用锁等待）；
3. 任何线程不得持有 `Status` 锁跨 channel 等待/阻塞调用。

**收工路径（评审 ①-4）**：stop 原子位 + self-pipe 唤醒；主线程 join WG 线程（上限 2s，
超时放弃 join 并报告——线程退出路径必须无锁等待）。

**线程布局（R1）**：

```text
主线程（CLI）：token decode → identity → 装配 → warmup（阻塞等 WG 线程通知）
              → speedtest / --dial / 一次性巡检
WG 线程（1 条，独占 Tunn + Interface + UDP socket，无锁热路径）：
  loop { poll({udp_fd, self_pipe}, timeout=min(poll_delay, 250ms))
       → 命令：connect/write/close 挂 socket 操作
       → 收包：drain socket（recv 循环直到 WouldBlock——批量收是吞吐设计位）
               帧解码 → tunn.decapsulate（含空数据报重调协议，见 §3）
               → 明文包过 hub 判据 → Device RX 队列
       → iface.poll（一次 poll 出尽 TX 队列——批量发是吞吐设计位）
               → TX 包逐个 tunn.encapsulate → send_wg（唯一发送收口，见 §3）
       → tunn.update_timers → 若 WriteToNetwork 同样过 send_wg }
```

**吞吐预算（评审 ①-2 整改，锚重算）**：
- Go 客户端实测口径：speedtest 每方向 10s 窗（down/up 顺序两段，4 流/方向），
  858MB/10s ≈ **690Mbps** 下行、951MB/10s ≈ **760Mbps** 上行；「±50% 同量级」下界
  ≈ **345–380Mbps**。
- 上限锚：PoC 实测 boringtun+ring 垫片 seal+open 一对 1.5–1.65Gbps ⇒ 单向
  ~750–825Mbps。**余量 ~2×（对判据下界），临界可行**——必须同时装下 smoltcp、帧
  编解码、每包 syscall（单向 ~76k pps ⇒ 双向 ~152k syscall/s）与拷贝。
  （「smoltcp 栈对栈 22Gbps」只证明栈本身不贵，不作 R1 余量论据。）
- **吞吐设计位（前置，非「超了再优化」）**：批量收（一次唤醒 drain 到 WouldBlock）、
  单次 poll 出尽 TX、容器帧与腿帧编码预分配 scratch、encapsulate/decapsulate 缓冲复用。
- A/B 口径（评审 A2）：与 Go 客户端**同参数**对拍（4 流/10s 窗/2s 预热），读数用
  speedtest 报告的分方向 bytes/window（服务端 report），不用 25s 累计流量当分母。

**R3 预判降级（评审 ①-3）**：本布局只断言 R1 单 peer 形态；R3 的栈/线程所有权
（多 peer device 三件套：全局 index 表、共享 RateLimiter、parse_handshake_anon 分发；
boringtun 自带 `crate::device` ~3.2k 行可抄）在 R3 技术评审单独立项，本期不预设。

## 2. ring 垫片 vendor 策略确认（要点①）

R0 已就位：`tools/ring-shim/` + `[patch.crates-io] ring = { path = "tools/ring-shim" }`
（配方源自 `tier:tools/spikes/rust-ohos-poc/ring-shim/`，PoC 实测 OHOS 交叉一次过）。

- **现状核查**：`Cargo.lock` 已钉 boringtun 0.6.0 → 本地 shim（R0 `cargo tree` 验证）；
  shim 消费面不止 aead/chacha——还有 `ring::constant_time::verify_slices_are_equal`
  （rate_limiter/handshake 的 MAC 比较）与 `ring::error::Unspecified`，垫片均已覆盖，
  `cargo check --workspace --all-targets` 全绿（评审复核）。
- **升级风险**：boringtun 0.6.0 无后续上游演进（cloudflare 仓库已不活跃；活跃 fork =
  defguard_boringtun，R8 长期化方案备选，届时再评估——用户触点清单项）。垫片锁定在
  `[patch]` 路径上，cargo 不会自动改解析。**结论：维持现状**。
- **约束**：本期**不得**动 `[patch]` 依赖组合；`Tunn::new` 的 `rate_limiter` 参数
  **必须传 `None`**（见 §3 勿改清单——传 `Some` 时客户端会对出口的握手应答回 cookie
  reply 并丢包）。

## 3. boringtun 握手/会话时序 vs wireguard-go 差异表（要点②）

WireGuard 线协议是冻结规范，字节互通无虞（boringtun ↔ wireguard-go 是多年生产事实）。
行为差异在时序面（评审 ②-2/3/5/7 整改后的准确口径）：

| 维度 | wireguard-go（Go 出口= responder） | boringtun 0.6（Rust 客户端= initiator） | 对互通的影响 |
|---|---|---|---|
| 握手发起 | 有流量/keepalive 时发起；重试 5s+jitter≤334ms | 无会话时 `encapsulate` 排队包并发 init；重试 REKEY_TIMEOUT=5s（**无 jitter**，代码未实现注释所述） | 无——客户端恒 initiator |
| index 分配 | 随机 24bit 去重 | 24bit peer index + 8bit 循环 session index：`Tunn::new(index)` 的 `next_index = index<<8`，`inc_index` 只递增低 8 位（第 256 次握手起复用，无害）；对端只回显 | 无——但**重建 Tunn 时传参 index 必须 <2^24 且换随机前缀**（`<<8` 丢高位） |
| PSK | noise_psk2（标准） | 同（x25519-dalek） | 无 |
| 会话存活 | REJECT_AFTER_TIME=180s；发数据侧 REKEY_AFTER_TIME=120s 主动换钥 | 同两常量；仅 initiator 主动 rekey（客户端恰是） | 无——持续流量下长会话由 rekey-on-send 覆盖 |
| keepalive | **被动保活**：对端静默 10s（KeepaliveTimeout）即发；peer 的 persistent_keepalive 可选（**Go 客户端不设**，出口不发） | 被动保活触发条件 =「收到过数据且自己 10s 没发」；persistent_keepalive 可配 | 无影响——**R1 选 `None`**（对齐 Go 客户端 peerIPC 无此键；自创 25s 常量=行为漂移，评审 ②-6 否决） |
| 数据包排序/重放 | 滑动窗口（标准） | 同 | 无 |
| cookie | under load 发 cookie reply | 支持（rate_limiter）；**verify_packet 只对握手 init/response 计数**，data 不计数——本地回环压不出 under-load 的真正理由 | 无——见 §2 勿改清单 |
| 握手超时 | REKEY_ATTEMPT_TIME=90s 放弃 | 同 90s `ConnectionExpired`；另有 REJECT_AFTER_TIME×3=540s 无会话过期 | 无——R1 失败即报错退出（见下「expired 三条」） |
| padding | 按 MTU 补零 | 不补 | 无（R5 A/B 字节计数时勿误判） |

**expired 恢复的三条事实（评审 ②-4，实现必须照此）**：
1. `update_timers` 开头 `is_expired() → Err(ConnectionExpired)`——**每 tick 都回错、
   不会自愈**；驱动循环捕获后「重建 Tunn」**只执行一次**（置标志位），否则每拍重建；
2. 真正的复活路径也可以是「下一次 `encapsulate`」（`format_handshake_initiation` 会把
   handshake state 从 Expired 换走）——R1 采用显式重建（新随机 index 前缀）+ 重发 reg，
   更干净；这是恢复阶梯 R1 档的**最小兜底变体且丢采纳**（Go R1 档 = 补注册 + 丢会话
   保采纳），登记为有意降级、R2 归位（评审 ⑥-6）；
3. 过期两条路径（90s 握手无应答 / 540s 无会话）分开看待：90s 形态在 warmup 等待里
   就会暴露为失败。

**错误面策略（评审 ②-9）**：`WireGuardError` 分两类——**静默丢包类**
（InvalidPacket/InvalidMac/NoCurrentSession/InvalidAeadTag/DuplicateCounter/
InvalidCounter/UnderLoad 等：计数 + 继续，绝不退出循环）与 **ConnectionExpired**
（→ 一次性重建）。第一个畸形包打死客户端是评审点名的失败形态。

**「空数据报重调」协议（评审 ③-7，R1 必需件）**：`decapsulate` 返回 `WriteToNetwork`
后必须以**空 datagram 反复重调**直到 `Done`——这是冲掉握手期排队内层包（SYN！）与
发握手响应 keepalive 的唯一出口（`MAX_QUEUE_DEPTH=256`）。注意与「网络上真收到的空
UDP 报文」区分（boringtun 以 `len==0` 为重调信号，真收到空包走同路径无害）。

**关键互通点（homeway 封装层，评审 ④ 整改）**：
1. **腿帧封装**：WG 报文外层 `[0xBB][0][wg]`（数据帧）；出口只认帧包（非帧一律丢）。
2. **reg 搭车收口 = 唯一 `send_wg(bytes)` 函数**（评审 ④-1 高危整改）：boringtun 产
   `WriteToNetwork` 有**四个来源**（`encapsulate` / `update_timers` /
   `decapsulate` 握手响应 keepalive / `send_queued_packet`）——reg 搭车不能绑在
   「encapsulate 返回处」，必须绑在「**真正写出 UDP 数据报的那一次**，不论来自哪个
   分支」。可达反例（评审推演）：若绑 encapsulate 且首个网络包由 update_timers 产出
   （如启动延迟 >REKEY_TIMEOUT 的重传路径），init 裸奔 → 出口按未知公钥丢弃 → 握手
   卡死到 90s 过期。实现 = 全部出站 WG 包经唯一 `send_wg` 收口，函数内做「reg armed
   则搭容器帧」。
3. **arm 消费时机（评审 ④-2，有意收紧）**：Go 在 `Send` **入口**消费 arm（走到已采纳
   分支就把 reg 丢掉，其注释自认）；Rust 采用「**首个真正写出的数据报**才消费」——
   比 Go 稳，登记为有意收紧非漂移（R1 单轮场景两者等价）。
4. **镜像每份都带 reg**（评审 ④-3）：未采纳期每个候选的镜像数据报都是
   `[0xBB][4]{[type=2][len][reg][type=0][len][wg]}` 容器（2 个直连候选 ⇒ 任一路径
   都能完成注册）。
5. **adopted 语义**：收到任何来源的合法帧包即采纳该来源，之后单发；回环场景两个
   端点都通，先回先采。

## 4. 栈 B 装配的会话语义（要点③）

**连接生命周期谁驱动**：**WG 驱动线程单线程驱动全部生命周期**——smoltcp 的 socket
状态机由 `poll()` 推进，`poll()` 由三唤醒源驱动（§1）。没有第二个驱动者（主线程只投
命令、等通知）。

**Device 层契约（评审 ③-1 高危整改）**：smoltcp 0.11 的 `Interface` **没有 inject API**
（只有 poll/poll_at/poll_delay）；入站/出站走自建 `phy::Device`：
- RX 侧：`Device::receive` 返回 token，`consume` 从「入站明文包队列」取包（队列深
  ≥256 包，满则丢——计数）；
- TX 侧：`Device::transmit` 返回 token，`consume` 把发出的 IP 包推「出站队列」，
  poll 返回后由驱动线程逐个 `encapsulate → send_wg`；
- **`DeviceCapabilities` 必须显式**：`medium = Medium::Ip`（Default 派生在 default
  features 下是 **Ethernet** ⇒ 走 ARP，隧道直接不通——评审点名的静默故障）、
  `max_transmission_unit = 1280`（MSS 由它推导；坑 4/23 对齐）。
- **通告窗口约束（评审 ③-2 高危）**：`DeviceCapabilities.max_burst_size` 若设
  `Some(N)`，smoltcp 会把 TCP 通告窗口**硬截到 N×MSS**（iface/packet.rs 实测确认）——
  照官方示例写 `Some(1)` = 1 MSS 窗口，下行静默打 1/50。**R1 取 `None`**（不截，
  通告窗口由 socket 缓冲决定），RX 队列 ≥256 包兜突发；1c 验收含「自环 TCP 单流
  跑满窗口」防回归。

**TCP 语义对齐（Go wgnet 口径）**：SACK 默认开（smoltcp 0.11 无 feature 门）；
每 socket `set_nagle_enabled(false)`（对齐 wgnet「Nagle 关」——小 JSON 步进交互延迟）；
socket 收发缓冲按 speedtest 块大小配（64KB-1MB 量级，1c 实测定）。

**入站分流（评审 ③-5 整改，照 Go 真口径）**：无 TUN 时 Go hub 的 `isForB` 判据 =
`pkt[16:20] == 本机派生隧道IP[:4]`（不是 100.64.255.1），**非 B 包静默丢弃（无计数
无日志）**。Rust 照抄：dst ≠ 本机隧道 IP 的入站包**静默丢 + 内部计数器**（计数器是
新增诊断位，不打日志、**不用 debug_assert**——对端可控数据不能 panic）。

**loopback 语义（评审 ③-6，显式决策）**：Go 的 gVisor 栈带 lo(127.0.0.1/8) +
HandleLocal ⇒ 拨 127.0.0.1 **不出隧道**；smoltcp 只有默认路由会把 127/8 发进隧道。
**R1 决策：127/8 出站连接在拨号入口直接拒绝（错误语义对齐「本地栈内无此路由」）**，
transit 判据用非环回地址造（§5 1e）。

**PathProbe refused 判据（评审 ③-4 高危整改）**：smoltcp `tcp::Socket` **没有
aborted/refused 访问器**（只有 state()/is_open()/is_active()），RST/本地 abort/超时
都落到 `Closed`——字面实现会把超时判成 refused ⇒ 假就绪。实现：probe 侧自记
「进入过 SynSent」；只有「SynSent → Closed 且未被本地 abort() 或超时打断」才判
refused（= RST = 隧道通）；超时（仍 SynSent 或 Closed 但被本地超时打断）= 不通。
配 blackhole 反例单测（拨不存在的出口 ⇒ 恒 SynSent 超时 ≠ refused）。

**socket 生命周期（评审 ③-8）**：连接关闭后 socket 保留在 SocketSet 里由 poll 继续推进
（CLOSE_DELAY=10s 的 TIME_WAIT 语义）直到 `state()==Closed` 才移除——不写这条，
speedtest 4+4 流 + 反复重连会撑爆 SocketSet。connect 的本地端口自管分配
（ephemeral 计数器 + 在用表查重，smoltcp 无分配器，评审 ③-9）。

## 5. 模块拆步与判据映射

| 步 | 模块（crates/homeway-core/src/） | 内容 | 测试 |
|---|---|---|---|
| 1a | `identity.rs` + `tunnel_addr.rs` + `psk.rs` | master.key 加载/创建（0600、O_EXCL 原子语义、损坏归档重建）、HKDF 派生 WG 私钥/devTag/PSK；DeriveTunnelIP/DeriveTunIP 含撞车守卫 | 吃 `fixtures/vectors/{identity,tunnel_addr,psk}.json` 逐字节（**psk 族本期新增**，评审 S1） |
| 1b | `wtransport/`（`frame.rs` + `bind.rs` + `endpoint_cache.rs`） | 腿帧/容器帧/hint 帧编解码（借用零拷贝）；直连 Bind：单 UDP socket、候选镜像、首回包采纳、**唯一 send_wg 收口的 reg 搭车**、Status 快照；端点缓存内存版（Observe/MarkVerified/Merge/TTL 语义同 Go，无落盘——任务指令要求，评审建议挂 R2 已知悉） | 帧编解码对拍 baseline frame_test 样例；`fmt_duration_go` 单测（0/500µs/500ms/5.025s/1m0.5s）；Bind 内存 UDP 对走「镜像→采纳→搭车」 |
| 1c | `wgcore/`（`engine.rs` WG 驱动 + `stackb.rs` smoltcp 栈 + hub 判据） | boringtun Tunn 装配（身份/peer/PSK、rate_limiter=None、keepalive=None）；驱动线程（§1 三唤醒源 + 三条死锁纪律 + expired 一次性重建 + 空数据报重调）；smoltcp Device 层（Medium::Ip/MTU 1280/max_burst_size=None）+ 拨号/读写命令面 | 双 Tunn 自环（encapsulate↔decapsulate 对接、PSK 互通、握手往返）；smoltcp 自环 TCP 单流**跑满窗口**（评审 ③-2 验收）；blackhole probe 反例 |
| 1d | `probe.rs` + `speedtest.rs` + cli `connect` | PathProbe（SynSent→Closed 判 refused）；speedtest 客户端引擎（帧协议 SPED LE、4 流/方向、warmup 2s/window 10s 对齐 t0、接收端报数、下行对账行）；`homeway-cli connect --token <hmw1> [--speedtest] [--dial <ip:port>]`（`--dial` = transit 产出步骤） | speedtest 帧编解码往返；LE/BE 与未知类型语义（speedtest 与 proto 帧相反的三处，评审 S4）单测钉住 |
| 1e | 判据实测 | local-exit 全流程 + **transit 步骤**：出口主机（本 Mac）起临时 TCP listener 绑**非环回**地址（LAN IP 或 0.0.0.0），客户端 `--dial <LAN-IP>:<port>` 经隧道拨（dst≠隧道IP ⇒ 出口打 `intercept: tcp transit`）；顺手补采 E12 若能造（不造则标注 R2） | 判据行原文入报告 |

**判据行文案对齐（INTEROP-CRITERIA 同串；参数构造规则评审 ⑤ 整改后）**：
- `服务会话: 身份：新建（dev=%s pub=%s，目录 %s）` / `服务会话: 身份：复用（dev=%s pub=%s）`
  （C1；**前缀口径**：Go hostsession 的行带「服务会话: 」前缀，bind/wgcore 的行不带——
  真实样例证实；Rust 对齐此前缀形态）；
- `wgcore: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 B 拨隧道 IP）`（C2）；
- `MIRROR 镜像包#%d → %d 候选（直连优先：本次直连 %d / 中继 %d；本行每轮限 3 条）`
  （C4；**计数口径**：`镜像#%d` 是 Bind 生命周期累计；`%d 候选` 是实际写出数；
  「限 3 条」实为 ≤3 行**且**两行间隔 ≥1s 的双条件节流）；
- `赛跑结算：胜出 %s %v（镜像 %d 包，耗时 %v）；响应过=%v；未响应=%v`
  （C5；**耗时 %v 是 Go Duration 格式**：先 Round(1ms)，0→`0s`、5.025s→`5.025s`、
  90s→`1m30s`——Rust `{:?}` 输出 `0ns`/`90s` 必不匹配，须 `fmt_duration_go`；
  **两个列表不对称**：`响应过` = 裸地址、`未响应` = `tag 空格 地址`（tag∈中继/IPv6/
  LAN/公网v4）、`、` 连接）；
- `路径确立：%s %v（首个回包来源）`（C6）；
- `warmup pong: 就绪（判据=wg）`（C8 模板 `判据=%s`，R1 恒 `wg`；此为 APP 核形态文案
  **按 ROADMAP 判据移植**（Go 服务会话原生打的是 C7），一次会话只打一次——登记）；
- `link: via=%s ep=%s rtt=%dms（服务会话巡检）`（C10 巡检形态；**rtt = 一次 PathProbe
  的墙钟 Milliseconds 截断**（环回 ⇒ `0ms`）；via/ep 取 Bind.Status()；Go 侧该行的
  产出者是 60s 巡检，R1 用**一次性巡检**替代——频率偏离登记，R2 归位 60s）；
- 出口侧行由 Go 出口产生，只采不产。

## 6. 风险与退出口

| 风险 | 缓解 | 退出口 |
|---|---|---|
| boringtun 与 Go 出口握手不通 | 先双 Tunn 自环钉 noise 层，再上真出口；按 §3 差异表逐项排查（index/时序/cookie/空包重调/reg 搭车） | 记录现象 → 退到 R4 中继最小（ROADMAP R1 退出口条款） |
| 单线程吞吐不达 ±50% | §1 吞吐设计位（批量收/单 poll 多包/scratch 复用）前置；实测锚 = 同参数 A/B | 仍超差 → 优化窗口/缓冲；再不行登记 R2 分线程 |
| smoltcp 与 gVisor（出口拦截栈）TCP 语义冲突 | MTU 1280/Nagle 关/SACK 开对齐；规范 TCP 冲突面小 | 单流降速可接受（量级判据） |
| `Medium::Ip`/`max_burst_size` 类静默配置坑 | 1c 验收「自环跑满窗口」+ blackhole 反例单测 | — |
| UDP 大包 | 本地回环 MTU 65535 无约束；真实网络 R2+ | — |

## 7. 评审重点（v1 所列，v2 已处置）

1. 异步选型论证遗漏面：唤醒原语与死锁纪律已补（①-1/③-3）；R3 措辞降级（①-3）。
2. 差异表「无影响」判定：index/cookie/keepalive/jitter/expired 的事实依据已修正，
   判定保留但补实现约束（勿改清单）。
3. 单驱动线程吞吐风险：锚重算（~2× 余量，临界）+ 吞吐设计位前置（①-2）。
4. reg 搭车时机：**不等价**——已改为唯一 send_wg 收口 + arm 收紧语义（④-1/④-2）。

## 8. 评审整改记录（v1 → v2）

技术评审渠道 = `dsh --profile headless`（`/tmp/dsh-review/r1.6SfVBt/`，意见全文与
逐条处置入 `docs/reviews/R1.md`）。高危整改 8 项：reg 收口（④-1）、transit 产出步骤
（⑥-1）、Device 层契约（③-1）、max_burst_size 窗口（③-2）、唤醒原语+死锁纪律
（①-1/③-3）、吞吐锚重算（①-2）、PathProbe refused 判据（③-4）、Go Duration 格式
（⑤-1）；中低危全部采纳进 §3/§4/§5 的对应小节（index 结构、rate_limiter=None、
expired 三条、keepalive 事实、loopback 决策、空数据报重调、socket 回收、判据参数
构造规则、一次性巡检、PSK 向量、S4 语义钉住等）。**未采纳 1 项**：端点缓存移出 R1
（评审 ⑥-3）——主会话任务指令明确要求内存版，保留最小实现并登记。
