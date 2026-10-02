# R3 技术设计（出口：Rust 出口对 Go 客户端透明替换）

> 开工前技术评审输入（ROADMAP「评审协议」第 1 道门；R1 决议「多 peer 的栈/线程所有权
> 按 R3 单独立项评审」在此兑现）。真源：baseline 621fe0e 的 `internal/server/`、
> `pkg/servercore/`、`pkg/intercept/`、`pkg/dns/`、`pkg/egress/`、`pkg/files/`、
> `pkg/wgnet/`、`pkg/probe/`、`pkg/speedtest/`、`internal/nodeconfig/`。
>
> **v2 = 技术评审（dsh，2026-10-02，4 高/15 中/11 低）整改后版本**；评审原文与逐条
> 处置见 `docs/reviews/R3.md` 第一道门。本版本即 R3 实现基线，再偏离须回评。

## 0. 范围与形态

- **目标**：`homeway-cli serve` 起本地 Rust 出口（端口 4264x/4274x 段错开，绝不绑 41641），
  Go 客户端（baseline 克隆统一进程 client 形态，local-exit.sh client-start 同款）与
  Rust 客户端（homeway-cli connect）都能挂上并全判据绿。
- **模块落位**（crates/homeway-core/src/ 下新增长效模块，Rust 惯例划分不映射 Go 包 1:1）：
  - `server/`（新 mod）：`device.rs`（多 peer WG device）、`bind.rs`（出口腿帧分发/新源
    日志/STUN 观测；与客户端 `wtransport/bind.rs` 同名不同模块——全路径区分，头注释
    点名防混淆）、`table.rs`（devTag 设备表）、`state.rs`（key/tokens/revoked 台账）、
    `intercept/`（拦截层：nat 重写 + TCP/UDP 会话 + Stats）、`dnsproxy.rs`
  - `files_server.rs`（files 六动词服务端，UDS 承载）、`speedtest_server.rs`
  - `egress.rs`（STUN Binding 客户端/IsPublicAddr/物理网卡枚举/探针）、`upnp.rs`
    （SSDP 发现 + SOAP 映射生命周期）
  - CLI：`homeway-cli serve`（flag 集 = Go serve 子集）+ `serve token [list|revoke]`
- **不做**（对齐基线裁剪面，非范围；**双栈/v6 面整族登记 R5 补**——Go 侧双栈监听
  `bind.go:517-527` 与 STUN6 观测是 v6 面，本地 A/B 全 v4 可测）：relay 注册腿/控制
  客户端（--relay 参数解析后拒绝并打一行——R4 接线）、term 服务（R6）、DDNS 自检、
  绑卡看护循环（保留启动期一次性 auto 挑卡）、UPnP 真实网关实测（本地无 IGD，实现
  + 单测 + mock 判据，真实网关行登记「不可本地测」）、daemon 控制面（serve start/stop/
  status 期望态管理——本期只有前台 `serve` 与纯读 `serve token`）。

## 1. 必答①：多 peer device 的线程所有权与锁面

### 1.1 结论：一条 WG 驱动线程独占 + 固定小 worker 池（poll(2) 多路复用）

```text
装配线程（CLI）：state/key/token 装配 → 驱动线程与 worker 池 spawn → 等待 stop 信号
WG 驱动线程（1 条，独占 device/拦截栈/设备表，无锁热路径）：
  poll(2) on {UDP fd, self-pipe}，timeout = min(拦截栈 poll_delay, 各看门狗节拍, 250ms)
  → 命令（unbounded mpsc：worker 池的 UpstreamData/UpstreamEof/UpstreamErr/UdpReady、
    装配层 STUNQuery/GC kick 等）
  → UDP 批量收（drain 到 WouldBlock）→ 腿帧分发（§1.2，含容器帧）
  → device 分发 → 明文包（源校验）→ 拦截层 RX（NAT/分流/建流/缓存 SYN）→ 拦截栈 poll
  → 拦截栈 TX → NAT 反重写 → 按 dst 查 peer → tunn.encapsulate → 腿帧 → sendto
  → 每 peer tunn.update_timers（含服务端主动握手场景，§1.3）
桥 worker 池（固定 N=8 条，每条 = poll(2) over {自己名下的 upstream fd 集, 命令管道}）：
  过境 TCP/UDP 会话的本机 socket（或豁免 UDS）全部注册进池（轮转分配）；
  事件驱动读写：fd 可读 → 读 → UpstreamData 投驱动；命令 Out{flow,bytes} → 写 fd；
  Eof/Err/Close{linger} 同枚举面。不阻塞在无超时的调用上（poll 唤醒即中断等待）。
DNS worker（独立 1-2 条）：DNS 代答的上游转发（阻塞面最长 2.5s/查询 + TC→TCP 腿），
  与驱动线程经有界通道（容量 = MaxInFlight=256，满则按 Go drop 计数丢弃）。
files/speedtest UDS 服务线程：每服务一条 accept 循环 + 每连接处理（files 命令短，
  直接在服务线程串行；speedtest 会话 spawn 短命线程——服务端限额 MaxConns=12）。
```

- **worker 池而非每流一线程**（评审 H4 整改）：Go 的 goroutine 近零成本，Rust 的 OS
  线程不是——上限 1024 TCP + 4096 UDP 会话若每流一线程 = 5000+ 线程。固定池 + poll(2)
  多路复用与 R1「自管线程」同一习惯，fd 数量级 5k 在 poll(2) 下无压力（epoll 优化挂
  R5 性能批）。
- **消息面（显式枚举，评审 H4/M9）**：
  - worker→驱动：`UpstreamData{flow, Vec<u8>}`、`UpstreamEof{flow}`（upstream EOF/错误
    ——对齐 Go bridgeConns「任一方 EOF/错误即双向拆」无半关）、`UdpReady{flow}`、
    `DialFailed{flow}`（H2 的 RST 触发点）；
  - 驱动→worker：`Out{flow, Vec<u8>}`、`Close{flow, linger_rst: bool}`（Drain 到期 RST =
    worker 对该 fd `set_linger(Some(0))` 后 close，评审 M9——所有权在 worker，驱动只发令）、
    `UdpSend{flow, Vec<u8>}`（pending 重放也走此面，顺序 = 通道 FIFO）。
- **背压纪律（第 4 条，评审 M8）**：双向 per-flow 高水位（256KB）：驱动侧 in-flight
  （已投 Out 未被 worker 消化的字节）超水位 → 暂停 drain 该栈内 socket（smoltcp 通告
  窗口随 rx buffer 自动收缩，客户端随之降窗）；worker 侧 in-flight（已读未投出）超水位
  → 暂停读该 fd。**通道本身 unbounded 不变**（死锁纪律 1/2 不破），水位只门控源头。
- **为什么不需要 Go 的 opCh FIFO 队列**：Go 侧因 wireguard-go IpcSet 的内部锁不能在
  ReceiveFunc 同步等（收工互等死锁）；自建 device 的 peer 表就在驱动线程内——Register
  的 add/remove 是普通调用，天然串行无锁（评审复核成立）。
- **吞吐风险与缓解**：服务端单线程 = decap+NAT+栈+encap（客户端形态 ~1.5 倍）。
  判据锚 = Go 出口同参数读数（±50%）。缓解顺序：实测 → checksum 增量化 → 栈拆线程
  （最后手段，重审交接面）。**通告窗口**：过境 TCP 栈内 socket rx buffer = 256KB
  （Go Forwarder rcvWnd=4096——**有意放宽并登记**：窗口是性能参数非协议面，回环 RTT
  下 Go 靠 4KB 窗也到 690Mbps；Rust 大窗只会更快，判据更容易过）。
- **GC/看门狗节拍**：设备表 GC（10min ±10%）、UDP/TCP idle、Drain 宽限都由驱动线程
  poll 超时拍驱动。
- **共享状态 × 属主 × 同步原语表**（评审 M14 整改）：

| 状态 | 写者 | 读者 | 同步 |
|---|---|---|---|
| device peer 三件（Tunn/pub/base 表/endpoint） | WG 驱动线程 | 同左（独占） | 无锁 |
| 设备表 entries/lastReg | WG 驱动线程（Register/GC） | Briefs/RejectCounts 快照（CLI status） | 驱动独占写；快照经 Mutex 只读面（低频） |
| 拦截层映射/会话/水位 | WG 驱动线程 | 同左 | 无锁 |
| worker 池 fd 表/命令 | 各 worker 自有名下 fd 集 | 驱动→worker 经各 worker 命令管道 | poll(2) 唤醒，无锁 |
| token/revoked 台账文件 | 装配线程（AppendToken）+ CLI（revoke） | 驱动线程 Register 验证（secrets 内存集）+ 吊销跟随（mtime 缓存 1s 重读 revoked.jsonl；**吊销只拒新注册，不拆已在表设备**——对齐 Go revokedFollower） | 文件 append-only + mtime 缓存 |
| 日志双通道（摘要/细节） | 全部线程 | 终端回显 | 单条 Mutex 包 writer（低频行级） |
| 公网端点观测/UDPCap | 装配层线程（周期） | token 打印/probe 应答 | 经命令面与驱动线程交互（STUN 走 socket 共享） |

### 1.2 device 收包分发（base 表一表 + pub 表）

boringtun `Tunn` 是单 peer 状态机；多 peer 分发（评审 M1 整改：**删 pending-init 表**——
type=2（response）与 type=3（cookie reply）的 receiver_idx 同样落在**我们自己 Tunn 的
index 空间**内（boringtun handshake.rs：response 按 receiver 匹配 in-flight 握手、cookie
按 cookies.index，两者都是本端分配的 base 空间 index）：

| 表 | 键 → 值 | 覆盖的包类型 |
|---|---|---|
| **pub 表** | peer 公钥 [32B] → peer | init（type=1）：`Tunn::parse_incoming_packet`（公开 API，只解析 init 形状）→ `noise::handshake::parse_handshake_anon`（公开 API）解出 sender 静态公钥 → 查表。命中后交给该 peer 的 tunn.decapsulate 完整处理（timestamp/重放/MAC 由它判） |
| **base 表** | receiver_idx >> 8（24-bit base）→ peer | response（2）/cookie（3）/data（4）。**3a 首个单测钉死不变量**：boringtun local index 恒为 `(base<<8)+k`（`Tunn::new(index)` 的 index<<8 起步，`inc_index` 后自增、k 从 1 起、低 8 位回绕只复用同 base 的 256 槽）——不符则退「每 peer 预登记 256 index」并登记 |

- **index 分配**：每 peer 随机 24-bit base、全局查重重掷。（wireguard-go 实为 32-bit
  随机 + 全局 IndexTable，我们按 peer 分 256 槽是 boringtun 内在结构——对线协议透明，
  对端只回显；行为等价，登记。）
- **rate_limiter**（评审 M5）：`Tunn::new(..., None)`——各 Tunn 自建 limiter（等价 per-peer
  限速；不共享 Arc，避免全局 10/s 压过 per-IP 语义）。under-load 时 boringtun 自产
  cookie reply（decapsulate 返回 WriteToNetwork）——照常 send_wg 回**包源地址**，
  endpoint 不因它更新（M2 判据面排除）。
- **入站明文源校验**（对齐 wireguard-go allowedips 反查）：decap 出的明文 IPv4 包
  `src ∈ {该 peer TunnelIP, TunIP}` 才投拦截层，否则静默丢 + 计数。**出站路由**：明文包
  dst 精确匹配 peer 两 /32（cap=32 线性扫）。
- **错误面**：单 peer 的 decapsulate Err → 计数继续不影响其它 peer；**ConnectionExpired
  不重建**（评审 M3）：Tunn 可自愈（下一入站 init 或出站 encapsulate 都会重建握手态，
  mod.rs:446-448）；expired 只打一行日志（每 peer 一次，抑制每拍重复——L6），
  **base 不换**（换 base 会丢客户端在途 index 引用）。

### 1.3 keepalive 与服务端主动握手口径（评审 M4 订正）

- **不写 persistent_keepalive**（`Tunn::new(keepalive=None)`）——对齐 Go ipc.go
  「AddPeer 刻意不写」。但 boringtun 的 responder **会**在两个条件下主动发 init：
  ① 发过数据后 15s 无回包（timers.rs:271-277 强制握手）；② 被动 keepalive 到点而无会话
  （timers.rs:307-309 `encapsulate(&[])` 起握手）。这与 wireguard-go 出口行为同族
  （keepKeyFreshReceiving/SEND_PERSISTENT_KEEPALIVE 语义差异属实现内政，线协议等价）。
  **3a 加「服务端发起握手」用例**（Rust 客户端被动应答）。
- **会话过期**：REJECT_AFTER_TIME=180s 旧 session 失效；客户端 120s rekey 时服务端
  跟随换代（boringtun 8-session ring 无缝）。

## 2. 必答②：漫游跟随的端点更新路径与 reg 的关系

**Go 真源口径**（wireguard-go receive.go 实读，4 处 SetEndpointFromPacket）：
init 验证成功（:374，**先更新后发应答** :381）、response 验证成功（:401）、数据包解密
成功（:459 ReceivedWithKeypair / :515 批量最后一个有效包）。reg 与 endpoint 无关
（ipc.go AddPeer 不写 endpoint；Table.Register 不碰 endpoint）。

**Rust 对齐**（评审 M2 整改——显式分派枚举，不用 TunnResult 判）：

- 驱动线程收包后先**按线协议解析包类型**（LE u32：1/2/3/4）：
  - type=1（init）：`parse_handshake_anon` 命中 pub 表 → 投该 peer 的 decapsulate；
    返回 WriteToNetwork（= 验证通过产应答）⇒ **先更新 endpoint = src，再 send_wg 应答**；
  - type=2（response）：base 表命中 → decapsulate；返回非 Err（建会话）⇒ 更新 endpoint；
  - type=4（data）：base 表命中 → decapsulate；返回 WriteToTunnelV4/V6（解密成功）
    ⇒ 更新 endpoint。**排除两类假阳性**：空数据报重调（返回 Done 但不是新认证）与
    cookie reply（WriteToNetwork 但非认证成功）——判定基于**本次收到的包的类型**，
    而非 TunnResult 形状；
  - keepalive（type=4 空载荷，解密成功返回 Done）：Go 侧 :515 的 validTailPacket 含
    keepalive ⇒ 也更新。判定 = type=4 且 decapsulate 非 Err。
- **reg 帧学习**：reg 只进设备表，不改 endpoint（同 Go）。新源首包日志（E23）按 src
  去重（shape 串 11 种：STUN应答/参照点探测/腿帧数据/腿帧注册/腿帧控制/容器数据/容器
  （无数据）/畸形容器/畸形腿帧/非帧包/非帧（%s，首字节=0x%02x）），容量 4096 满清表
  并打「入站新源表满（%d 条），清表重记」。
- **hint**：OnHint 钩子保留、装配层接零输出（对齐 Go 本期形态——L5：R3 不给 hint 加
  新日志行；R4 中继接线时再定）。
- Go 的 endpoint 更新与 allowedips 源拒绝不互斥（:515 先置 validTailPacket 再逐包校验
  源地址）⇒ 「endpoint 已更新但包被源校验拒」的中间态 Rust 同序——终态差异极小，登记。

## 3. 必答③：UDP 长会话的 pending 重放窗口语义

**Go 真源**（intercept.go）：首包先置 `udpSess[key]=true` 再 spawn；在建窗口同五元组包
进 `udpPend[key]`（≤16 **丢最新** = 不再 append）；会话满 4096 → return false（ICMP）；
serveUDP：栈内 spoof 端点 → upstream（10s 拨号）→ **两端就绪后** takePending → 按序写
（first 先于 pending）；端点注册后后续包走 demux 不进 handler；失败 dropUDP 清两表；
idle 60s（DNS 10s），看门狗 `min(idle/3, 30s)` 下限 20ms，共享活跃时间戳；关闭时仅
transit 上报 IncrUDPSession(downSeen)。

**Rust 形态（逐条对齐 + 评审补写）**：

- 五元组键 = `(src_ip, src_port, orig_dst_ip, orig_dst_port)`；在建/pending 表在驱动线程
  （无锁）。首包：置在建位 → 分配 rw_port + 栈内 udp socket（bind 拦截栈地址:rw_port；
  **smoltcp udp 无 connect API——唯一 rw_port + 读时校验源**（不匹配丢弃），评审 M11b）
  → 投 worker 池建 upstream（本机 UDP connect / DNS 进程内腿）→ `UdpReady` 回驱动后
  **重放走 upstream 直投（UdpSend 命令面），不再注入拦截栈**——防双投（评审 §3 补写）。
- **重放窗口** = 首包 → UdpReady 回到驱动线程；窗口内同五元组包进 pending（≤16 丢最新）。
  upstream 失败 → `DialFailed` → 清两表（后续同五元组重新走新会话路径 = dropUDP 语义）。
- 会话就绪后：后续包经 NAT 重写命中栈内 socket（唯一 rw_port 语义上等同 connect），
  驱动线程从 socket 读出 → `Out/UdpSend` 投 worker → upstream；worker 读 upstream →
  `UpstreamData` → 驱动线程写栈内 socket → 包经 TX 反重写回投客户端。
- **ICMP responder**（覆盖三类，评审 L3/L4 扩面）：① 会话满 4096 的 UDP 首包；② 无映射
  的非首包 UDP（Go = gVisor 自动 port-unreachable）；③ **无映射的非 SYN TCP 回 RST**
  （Go = gVisor protocol.go:166-181 的 RST，评审 L4——设计 v1 写「丢」订正为 RST）。
  实现面：驱动线程构造 ICMP type3code3 / TCP RST（源 = orig dst），直接进 encap 出站。
- **DNS :53 特判**：dst 端口 53 且代答开 → kind=dns：upstream = 进程内代答腿（DNS
  worker），idle=10s，dialok/dialfail 不掺（dns 会话走 dnsleg 形态）。

## 4. 必答④：拦截层与 LocalServices UDS 的组装边界

### 4.1 包级 NAT 重写（smoltcp 无 Forwarder 的替代）+ **拨号先行**（评审 H2 整改）

```text
RX（WG decap 出的明文包，源校验通过后）:
  dst == 隧道IP?
    ├─ 是 → 直接注入拦截栈（栈地址=隧道IP，**不开 any_ip**——评审 ④：any_ip 只绕目的
    │        地址检查、仍无 spoof 源地址面，且引入静默接受面，禁用）：命中栈内真
    │        listener（DNS :53/:5300）→ 服务；未命中监听端口 → smoltcp 回 RST
    │        （PathProbe :1 同款）；
    └─ 否 → NAT 路径（过境与豁免统一）:
         TCP SYN（无映射）→ **不立即注入栈**：记 pending flow{orig 四元组}，分配
           rw_port（避开监听端口与在用，环形），投 worker 拨 upstream：
             · orig dst == 隧道IP（豁免）：LocalServices[port] 命中 → UDS；否则
               127.0.0.1:同端口
             · orig dst 端口==53 且代答开（非隧道 IP 的 :53）→ DNS 进程内腿
             · 其余（transit）：connect orig dst（10s）
           窗口期后续 SYN 重传/ACK 缓存（小队列 ≤4 包）。
           UdpReady/DialOk 回驱动 → 建栈内 socket listen((隧道IP, rw_port)) →
           注入缓存的 SYN（此时才有 SYN-ACK——**对齐 Go「拨号成功才应答」**）。
           DialFailed → 构造 RST（源=orig dst）回客户端（对齐 Go r.Complete(true)）。
         TCP 有映射 → 重写 dst=(隧道IP,rw_port)，重算校验和，注入栈。
         TCP 无映射非 SYN → **回 RST**（§3 ICMP responder 同族；评审 L4）。
         UDP 首包（无映射）→ §3 会话路径（先拨号后注入：会话建立时投栈的只有
           worker UdpReady 之后到达的包；首包与 pending 走 upstream 直投）。
         UDP 有映射 → 重写注入（命中栈内 socket → 读出投 worker）。
TX（拦截栈 poll 出的包，encap 前）:
  src=(隧道IP,rw_port) 命中反查表 → 重写 src=(orig_dst_ip, orig_dst_port)（豁免流
  的 orig dst 就是隧道IP ⇒ 恒等重写，统一路径）；服务真 listener 应答不重写。
  之后按 dst（客户端地址）查 peer → encap → 腿帧 → sendto(peer.endpoint)。
```

- **建连时序等价性**（H2 整改核心）：Go = 「upstream 拨号成功才 CreateEndpoint
  （SYN-ACK）；失败 r.Complete(true)（RST）；拨号期 SYN 重传被 Forwarder in-flight
  吞住」。Rust = 「SYN 缓存 + TcpReady 后建 socket 注入 + DialFailed 构造 RST +
  窗口期包缓存」——三态（成功时点/失败可见性/黑洞 10s）等价。
- **映射生命周期**（M11）：TCP 映射随栈内 socket 生命周期——socket 推进到 Closed 并
  从 SocketSet 移除后映射才删（TIME_WAIT 期保留）；rw_port 复用 = 映射删除后可再分配
  （分配器跳过在用）。UDP 映射随会话（idle/关闭即删）。**Drain/HaltNew 语义**
  （M13）：① HaltNew 后新 TCP 一律 RST、新 UDP 一律 ICMP（无日志——Go 同）；② 关 UDS
  listeners；③ 宽限内自然收销账、到期 `Close{linger_rst}` RST；④ 关 WG socket；
  ⑤ DNS/服务/观测收工。
- **栈内 socket 选项**（M12，接 R2 低-10）：过境/豁免 TCP socket `set_timeout(Some(idle))`
  （smoltcp 0.11 socket/tcp.rs:612——**精确 idle 回收**，替代 R2 客户端侧的「拍检查
  兜底」）；Nagle 关（Go SetDelayOption(false) 同款）。
- **校验和**：IPv4 头重算 + TCP/UDP 因伪头部全量重算（≤1280B；增量优化挂 R5）。
- **Stats**：`intercept: tcp %s %v ← %v（dialok）`（dialok/dialfail 仅 TCP）、E11 关闭行、
  E12 UDP 会话行（`udp intercept: 会话 #%d %s 建立（%v ← %v）`，%d 为进程级递增会话号）、
  拒绝行（并发上限 + 在册 + 累计）。字段面与 Go stats.go 同（dialok/dialfail/rejects/
  flows/udp_sessions(downSeen)）。
- **LocalServices 组装边界**：serve 装配层建 UDS（死/活判别 200ms 拨号探测 + ENOENT/
  ECONNREFUSED 才清残留 + chmod 0600 + own 身份删除 + 路径 <100B 上限）；拦截层只拿
  静态映射（端口→sock 路径，建后不改；服务被摘时条目保留，ENOENT 快速失败回 RST）。
  term 不起不建 term.sock（term.Disabled 同款）；files.sock/speedtest.sock 起真实服务。

### 4.2 与 Go 侧的已知差异登记（客户端可观察面）

| 面 | Go（gVisor） | Rust（smoltcp+NAT） | 处置 |
|---|---|---|---|
| 通告接收窗 | Forwarder rcvWnd=4096 | socket rx buffer 256KB | **有意放宽**（性能参数非协议面），登记 |
| MSS | 栈协商（MTU 1280） | 同 | 无 |
| SACK/Nagle | SACK 开/Nagle 逐端点关 | 同 | 无 |
| UDP spoof 端点 | SO_REUSEPORT 绑原目的地址 | bind 栈地址:rw_port + 读时校验源 | 应答源由反重写还原，客户端不可见 |
| 拨号失败 | RST（Complete(true)） | 构造 RST | 等价（H2 整改后） |
| UDP 满/无端点 | ICMP（netstack 自动） | 自建 ICMP responder | 等价 |
| 未知非 SYN TCP | RST（protocol.go） | 构造 RST | 等价（L4 整改后） |
| v6/双栈监听 | 双栈 socket | v4 单栈 | **登记 R5 补**（M10） |

## 5. 拆步、判据矩阵与常量映射

| 步 | 内容 | 判据（INTEROP-CRITERIA #） | R2 移交项落点 |
|---|---|---|---|
| 3a | `server/device.rs`+`server/bind.rs`：多 peer device（pub/base 表/漫游/日志抑制）+ 腿帧分发（**含容器帧**）/新源日志/probe 应答 | E23；单测钉 base 不变量/容器帧/服务端主动握手/漫游 | — |
| 3b | `server/table.rs`+`server/state.rs`：设备表（cap/TTL/grace/轮换/拒绝归因）+ 台账（key/tokens/revoked/一轮制/吊销跟随） | E6/E7/E8/E9 全族（stale=表满+grace 注入）/E18；`peer: ! reject reason=revoked` | 低-7 字符串载荷收窄（错误面在此定型） |
| 3c | `server/intercept/`：NAT/拨号先行/UDP 会话/ICMP+RST responder/Stats/LocalServices/背压水位 | E5/E10/E11/E12/拒绝行 | 低-10 `set_timeout` 精确形态 |
| 3d | `dnsproxy.rs` + `files_server.rs` + `speedtest_server.rs` | E4/E14/E17/E22/E13；files 双对端对账 | — |
| 3e | `egress.rs` + `upnp.rs` + STUNQuery | E20 显式端点路径；UPnP mock 单测 + 「不可本地测」登记；STUN 真观测一次 | — |
| 3f | CLI serve 装配 + 判据实测 + 收口 | E1/E2/E3/E19/E21 全补；Go↔Rust 两侧全判据；多 peer 混跑；TTL/吊销注入 | 低-5/6/8/12/16 微项 |

**常量映射表**（全部与 Go 同值；出处 = baseline 克隆）：

| 常量 | 值 | 真源 |
|---|---|---|
| 设备表 cap/TTL/grace | 32 / 7d / 10min（严格 > 比较） | peers.go:104-107 |
| TCP 并发/空闲/拨号超时 | 1024 / 5min / 10s | intercept.go + serve.go:344-345 |
| UDP 会话上限/pending/读粒度 | 4096 / 16（丢最新）/ 30s | intercept.go:53-56,141 |
| UDP idle / DNS idle | 60s / 10s | intercept.go:48-49 |
| 隧道 IP / files/term/speedtest/dns 端口 | 100.64.255.1 / 7802/7724/7803/5300 | serve.go:33-38 |
| reg 窗口 | ±90s | proto/reg.go |
| speedtest 服务限额 | MaxConns=12/超时 30s/warmup≤5s/window≤15s（**serve.go:515 注释「≤8」过时**，M6 以 speedtest.go:89 为准） | speedtest.go:80-95 |
| DNS 预算/在途/兜底/TTL 钳/UDP 上限 | 2.5s / 256 / 223.5.5.5 / 60s / 1232 | dns/server.go:64-74 + proto/dns.go |
| files 并发/空闲/陈旧回收/read 缺省/image 缺省/内联上限 | 16 / 5min / 24h / 512KB / 8MB / 16MB | files/server.go:20-41 |
| 公网端点刷新（成/败） | 10min / 2min | publicendpoint.go:34-35 |
| STUN 缺省 | stun.cloudflare.com:3478 | nodeconfig Default |
| UDPCap 探测周期 | 10min（Caps 位 = DNS:53 探针 + STUN 通用面） | udpcap.go |
| 拦截栈 | MTU 1280 / Medium::Ip / max_burst None / 过境 TCP rx 256KB（有意放宽登记） | — |

## 6. 风险与退出口

| 风险 | 缓解 | 退出口 |
|---|---|---|
| **单驱动线程吞吐不达 ±50%**（顶级风险） | 吞吐设计位前置；实测锚 = Go 出口同参数；窗口已放宽 256KB | checksum 增量化 → 栈拆线程（重审交接面）→ 判据按「同量级」放宽并登记（主会话裁决） |
| boringtun index 不变量不成立 | 3a 首个单测钉死（多轮握手 base 不漂） | 每 peer 预登记 256 index（+登记） |
| 容器帧/reg 分发遗漏面（H1 类） | 3a 单测含「Go 客户端形态首包」向量（容器 [reg][init]） | — |
| DNS 代答上游依赖 | 本地实测（Mac 真实解析）+ 单测 fake 上游 | — |
| L10：Rust 客户端 speedtest.rs:460 `first_frame` 恒 true（R2 既有 bug） | 3d 顺手修（服务端判据会依赖客户端帧序） | — |
