# R3 技术设计（出口：Rust 出口对 Go 客户端透明替换）

> 开工前技术评审输入（ROADMAP「评审协议」第 1 道门；R1 决议「多 peer 的栈/线程所有权
> 按 R3 单独立项评审」在此兑现）。真源：baseline 621fe0e 的 `internal/server/`、
> `pkg/servercore/`、`pkg/intercept/`、`pkg/dns/`、`pkg/egress/`、`pkg/files/`、
> `pkg/wgnet/`、`pkg/probe/`、`pkg/speedtest/`、`internal/nodeconfig/`。

## 0. 范围与形态

- **目标**：`homeway-cli serve` 起本地 Rust 出口（端口 4264x/4274x 段错开，绝不绑 41641），
  Go 客户端（baseline 克隆统一进程 client 形态，local-exit.sh client-start 同款）与
  Rust 客户端（homeway-cli connect）都能挂上并全判据绿。
- **模块落位**（crates/homeway-core/src/ 下新增长效模块，Rust 惯例划分不映射 Go 包 1:1）：
  - `server/`（新 mod）：`device.rs`（多 peer WG device）、`bind.rs`（出口 ServerBind：
    腿帧分发/新源日志/STUN 观测）、`table.rs`（devTag 设备表）、`state.rs`（key/tokens/
    revoked 台账）、`intercept/`（拦截层：nat 重写 + TCP/UDP 会话 + Stats）、`dnsproxy.rs`
  - `files_server.rs`（files 六动词服务端，UDS 承载）、`speedtest_server.rs`
  - `egress.rs`（STUN 探测/公网判定/网卡枚举）、`upnp.rs`（SSDP 发现 + SOAP 映射）
  - CLI：`homeway-cli serve`（flag 集 = Go serve 子集）+ `serve token [list|revoke]`
- **不做**（对齐基线裁剪面，非范围）：relay 注册腿/控制客户端（--relay，R4 后接线；本期
  解析参数但拒绝/忽略并打一行）、term 服务（R6）、DDNS 自检、绑卡看护循环
  （bindwatch.go，换网重挑——保留 BindMode=auto 的启动期一次性挑卡）、UPnP 真实网关
  实测（本地无 IGD，实现+单测+mock，判据登记「不可本地测」）、role-management 的
  daemon 控制面（serve start/stop/status 的期望态管理——本期 CLI 只做前台单角色
  `serve` 与纯读 `serve token`）。

## 1. 必答①：多 peer device 的线程所有权与锁面

### 1.1 结论：一条 WG 驱动线程独占全部可变状态（R1 决议延伸）

```text
装配线程（CLI）：state/key/token 装配 → 驱动线程 spawn → 等待 stop 信号
WG 驱动线程（1 条，独占，无锁热路径）：
  poll(2) on {UDP fd, self-pipe}，timeout = min(拦截栈 poll_delay, GC/看门狗节拍, 250ms)
  → 命令（unbounded mpsc：worker 的 TunnelWrite/会话建立/关闭、装配层的 STUNQuery 等）
  → UDP 批量收（drain 到 WouldBlock）：
      解腿帧（0xBB）：data → 包分发进 device；reg → 设备表 Register；control → hint 钩子
      STUN 应答匹配 → 唤醒等待者；参照点探测 → 明文应答；非帧 → 丢弃计数
  → device 收包分发（见 1.2）→ 明文包 → 拦截层 RX（NAT 重写+分流）→ 拦截栈 poll
  → 拦截栈 TX → NAT 反重写 → 按 dst 查 peer → tunn.encapsulate → 腿帧 → sendto
  → 每 peer tunn.update_timers（出站包（keepalive/握手重传）同样经腿帧出）
拦截 worker 线程（每过境 TCP 连接/UDP 会话 1 条，小栈 256KB）：
  只做本机 socket（或 UDS）的阻塞 IO；与驱动线程经两条 unbounded 通道交互：
  worker→驱动 {TunnelWrite(flow, data)}；驱动→worker {ToUpstream(flow, data), Teardown(flow)}
```

- **为什么不需要 Go 的 opCh FIFO 队列**：Go 侧 applyDeviceOpAsync 的存在是因为
  wireguard-go device 的 IpcSet 拿 device 内部锁、不能在 ReceiveFunc 里同步等
  （收工互等死锁）。我们的自建 device 的 peer 表**就在驱动线程内**——Register 的
  AddPeer/RemovePeer 是驱动线程内的普通函数调用，天然串行、无锁、无死锁面。
  这是「自建」相对 wireguard-go 的结构性简化（不是直译）。
- **设备表归属**：`table.rs` 的 entries/lastReg 只被驱动线程读写（Register 在收包路径、
  GC 在驱动线程定时拍、快照 Briefs()/RejectCounts() 经一条 Mutex 只读面——低频诊断，
  不在热路径）。
- **三条死锁纪律沿用**（R1）：命令通道 unbounded；驱动线程绝不阻塞在 channel/锁；
  worker 不得持任何锁跨阻塞调用。
- **吞吐风险与缓解**：服务端单线程要做 decap+NAT+栈+encap（客户端形态的 ~1.5 倍工作）。
  R1 单线程锚 393-404Mbps（release）。判据「speedtest ±50%」的锚 = Go 出口同参数读数
  （R0 口径 690/760，±50% 下界 ≈345-380）——**临界**。缓解顺序：先实测；超差则优先
  优化（checksum 增量更新代替全重算、TX 批量出）；再不行才考虑把拦截栈拆独立线程
  （多一次队列交接）——**拆线程是最后手段**，因为跨线程包交接会破坏「单线程独占栈」
  的正确性论证面（smoltcp 非线程安全，交接面要重新审）。登记为 R3 顶级风险。
- **GC/看门狗节拍**：设备表 GC（10min ±10% 抖动）、UDP 会话空闲看门狗、TCP idle 检查
  都由驱动线程 poll 超时拍驱动（min 各节拍）；不需要独立线程。

### 1.2 device 收包分发（index 三表）

boringtun `Tunn` 是单 peer 状态机；多 peer 分发自建三张表（全在驱动线程）：

| 表 | 键 → 值 | 用途 |
|---|---|---|
| **pub 表** | peer 公钥 [32B] → peer | init 识别（`Tunn::parse_incoming_packet` → `noise::handshake::parse_handshake_anon` 解出发送者静态公钥——两者皆 boringtun 0.6 公开 API）+ 设备表 Register 结果落位 |
| **base 表** | receiver_idx >> 8（24-bit base）→ peer | data 包（type=4）分发。boringtun 的 local index 恒为 `(base<<8)+k`（`Tunn::new(index)` 的 index<<8 起步、握手换代会话时只递增低 8 位）——**先在双 Tunn 单测钉死这个不变量**，若上游行为不符再退「逐 peer 试解」并登记 |
| **pending init 表** | 我们发出的 init 的 sender_idx → peer | response（type=2）/cookie reply（type=3）分发——receiver_idx 是对端（客户端）的 index，不在 base 空间；我们发出 init 时（encapsulate/update_timers 返回的包 type=1）记录，收到 response 后消费 |

- **index 分配对齐 wireguard-go 语义**（「随机 24bit 去重」）：每 peer 建立时随机 24-bit
  base、全局 base 表查重、冲突重掷。与 Go 侧（wireguard-go 全局随机 index）行为等价
  （都是「随机不撞」；wireguard-go 的 index 不按 peer 分段，我们按 peer 分 256 槽是
  boringtun 的内在结构，对线协议透明——对端只回显）。
- **入站明文包的源检查**（对齐 wireguard-go receive.go 的 allowedips 反查）：decap 出的
  明文 IPv4 包 `src ∈ {该 peer 的 TunnelIP, TunIP}` 才接受，否则静默丢 + 计数
  （wireguard-go 同为丢+verbose 行）。**路由 peer 查找**：出站明文包按 dst 精确匹配
  peer 的两个 /32（cap=32，线性扫足够；不做前缀树）。
- **错误面**：对某 peer 的 decapsulate 返回 Err（InvalidMac 等）→ 计数继续，绝不影响
  其它 peer；ConnectionExpired（该 peer 540s 无会话）→ 该 peer 的 Tunn **原地重建**
  （新随机 base、重新登记 base 表、消费 pending init 表）——服务端是 responder，
  过期后等客户端重新握手即可，不主动重试。

### 1.3 peer 生命周期与 keepalive 口径

- **不写 persistent_keepalive**（`Tunn::new(keepalive=None)`）：对齐 Go 侧 ipc.go
  「AddPeer 刻意不写 keepalive」——保活 = 客户端 60s 巡检。boringtun 的**被动** keepalive
  （「收到过数据且自己 10s 没发」）保留——wireguard-go 出口同款行为，不是「出口主动保活」。
- **会话过期语义**：REJECT_AFTER_TIME=180s 后旧 session 失效（数据包丢弃）；responder
  不主动 rekey（REKEY_AFTER_TIME 只约束 initiator=客户端）；客户端 120s rekey 时服务端
  在 decapsulate(新 init) 建 new session（boringtun ring 保留最近 8 个 session，无缝换代）。
- **设备表事件 → device**：add（建 Tunn+三表登记）/ rotate（拆旧 Tunn（三表清除）再建新）/
  expire/stale/表满淘汰（拆 Tunn）。remove 时同时终结该 peer 名下的在途过境流？
  **不**——Go 侧 RemovePeer 只拆 WG peer，过境 TCP 由拦截层按 idle 自然收（对齐：
  peer 移除后栈内 socket 不再收到新包，连接随 TCP 语义超时/对端 FIN 收口）。

## 2. 必答②：漫游跟随的端点更新路径与 reg 的关系

**Go 真源口径**（wireguard-go receive.go 实读，v0.0.0-20260522210424）：

1. handshake **initiation** 验证成功 → `peer.SetEndpointFromPacket(elem.endpoint)`（:374）；
2. handshake **response** 验证成功 → 同上（:401）；
3. **数据包**解密成功（含 keepalive；批量包取最后一个有效包）→ 同上（:459/:515）；
4. **reg 与 endpoint 无关**：`ipc.go AddPeer` 不写 endpoint；`Table.Register` 也不碰
   endpoint——endpoint 100% 由「认证成功的包的源地址」学习。

**Rust 对齐**：

- 维护 `peer.endpoint: Option<SocketAddr>`；更新时机 = 该 peer 的 decapsulate 返回
  **非 Err 且包类型 ∈ {init 成功（返回 WriteToNetwork=应答）, response 成功（建会话）,
  data 认证成功（WriteToTunnel* 或空载荷 keepalive 的 Done）}**。boringtun 侧「认证成功」
  的判据：decapsulate 返回 `WriteToTunnelV4/V6` 或（data 包路径）`Done`（keepalive）或
  握手路径的 `WriteToNetwork`。Err 一律不更新。
- **reg 帧学习**：对齐 Go——reg 只进设备表（devTag 记账/刷新/轮换），**不改 endpoint**。
  新源首包日志（E23 `入站新源：%v（%s，%d 字节）`）按 src 去重记录（含 WG 消息类型名，
  容量 4096 满则清表）——这是「直连时好时坏」排查的判据面，必须同串。
- **hint（type=1 control 帧）**：Go 侧 OnHint 由装配层接（中继观察的客户端公网地址，
  打洞用）。R3 本期 OnHint 钩子保留接口、装配层打到细节日志（打洞动作依赖中继腿，
  R4 接线）；接线方校验 src 属中继的保护由装配层负责（钩子注释说明）。
- **验证用例**：3f 实测「客户端 rebind（换源端口）后链路不断/自愈」——R2 客户端
  Rebind 后首发出站包触发服务端方向无感知（服务端发包还是旧 endpoint → 客户端新
  socket 收不到 → 客户端重握手 → init 从新源到达 → endpoint 更新）。判据 = 出口侧
  `入站新源` 行 + 链路恢复。

## 3. 必答③：UDP 长会话的 pending 重放窗口语义

**Go 真源**（intercept.go handleUDPPacket/serveUDP/takePending）：

1. 新五元组首包 → 查 `udpSess[key]`：已在建 → 包进 `udpPend[key]`（≤16 包，超出**丢最新**
   即不再 append）并 return true（吞掉）；
2. 不在建 → 会话表满 4096 则计数+return false（netstack 回 ICMP 不可达）；否则置
   `udpSess[key]=true`（**先置位再 spawn goroutine**——同步置位防竞态重复建）；
3. serveUDP goroutine：建栈内 spoof 端点（SO_REUSEADDR/PORT 绑原目的地址）→ 建
   upstream socket（本机重拨，10s 拨号超时）→ **两端都就绪后** `takePending`（取走并清空
   队列）→ 按序写 upstream（first 首包 + pending）；
4. 端点注册（gVisor Connect 按全四元组注册）后，同五元组后续包走 demux 不再进 handler；
   pending 窗口 = 从首包到 upstream 就绪（含拨号失败路径：goroutine 退出时 dropUDP
   清两表）；
5. 双向泵 + 共享活跃时间戳看门狗（idle 默认 60s；DNS :53 会话 10s）；关闭时 transit
   会话上报 IncrUDPSession(downSeen)（udpcap 实测位）。

**Rust 形态（语义逐条对齐）**：

- 五元组键 = `(src_ip, src_port, orig_dst_ip, orig_dst_port)`；在建表 + pending 表在
  拦截层（驱动线程独占——**无锁**，比 Go 的 udpMu 更简单）。
- 首包处理（驱动线程）：置在建位 → 分配 NAT 重写端口 + 栈内 udp socket（bind 拦截栈
  地址:重写端口，connect 客户端源）→ 投 worker 线程建 upstream（本机 UDP connect 或
  DNS 进程内腿）→ worker 就绪后回命令 `UdpReady{flow}`，驱动线程重放 pending（按序写
  upstream：first 在 worker、pending 由驱动线程经 ToUpstream 投递——**顺序保持**：
  first 是 worker 发出的第一写，pending 随后按队列序，通道 FIFO 保证）。
- **重放窗口边界**：从首包到 `UdpReady` 回到驱动线程。窗口内同五元组的包（重写后到达
  拦截栈前）进 pending（≤16 丢最新）。upstream 拨号失败 → worker 回 `Teardown`，
  驱动线程清两表（后续同五元组包重新走「新会话」路径——对齐 Go 的 dropUDP 语义）。
- 会话上限 4096 / 每五元组 pending ≤16 / idle 60s（DNS 10s）：常量同值。看门狗节拍 =
  `min(idle/3, 30s)` 下限 20ms，由驱动线程定时拍检查（共享活跃时间戳 = 拦截层记
  last_active，双向任一方向有数据即刷新）。
- **ICMP 不可达**：会话满时 Go 侧 return false → gVisor 回 ICMP type3 code3。smoltcp
  0.11 无自动 ICMP responder——自构造 ICMP 包回注（TX 侧按对端 peer encap 发出），
  实现 ~40 行（echo 原包首 8 字节内嵌）；**不做**则降级为静默丢（与「回 ICMP」的
  spec 承诺不符，登记）。3c 内实现 ICMP responder。
- **DNS :53 特判**：dst 端口 53 且配置了代答 → kind=dns：upstream = 进程内代答腿
  （不开 socket；应答由代答器产包、经栈内 socket 回投，源地址 NAT 反重写回原 dst）；
  idle=10s；计 dialok/dialfail 不掺（Go 侧 dns 会话走 `newDNSLeg`，IncrFail 只在
  建腿失败时发生）。

## 4. 必答④：拦截层与 LocalServices UDS 的组装边界

### 4.1 smoltcp 无 Forwarder 的替代：包级 NAT 重写（本设计核心决策）

gVisor 的 `tcp.NewForwarder` + promiscuous/spoofing 在 smoltcp 0.11 **没有对应物**
（smoltcp 的 IP 层丢弃 dst≠本机地址的包，transport 层也没有「任意端口动态 accept」
的 handler 面）。替代方案 = **用户态 REDIRECT（NAT 重写）**：

```text
RX（WG decap 出的明文包）:
  dst == 隧道IP(100.64.255.1)?
    ├─ 是 → 直接注入拦截栈（栈地址=隧道IP；命中栈内真 listener：DNS :53/:5300；
    │        未命中监听端口 → smoltcp 回 RST —— PathProbe :1 / 端口转发 localhost 同款）
    └─ 否（过境/豁免到非监听端口）→ 查五元组映射表:
         无映射 + TCP SYN（或 UDP 首包）→ 分配重写端口 rw_port（避开栈内监听端口与
           已用重写端口，1024..65535 环形分配），建映射 {orig 4 元组 ↔ (隧道IP, rw_port)}，
           栈上 listen/绑定（TCP: 新 socket listen((隧道IP, rw_port))；UDP: socket
           bind+connect 客户端源），spawn worker 按重写前 orig dst 决定 upstream：
             · orig dst == 隧道IP（豁免）：LocalServices[port] 命中 → UDS 拨号；
               否则 TCP→127.0.0.1:同端口 / UDP→127.0.0.1:同端口
             · orig dst 端口==53 且代答开（进程内 DNS 腿，非隧道 IP 的 :53）
             · 其余（transit）：TCP connect / UDP connect orig dst（10s 拨号超时）
         有映射 → 重写 dst=(隧道IP,rw_port)，重算 IPv4 头 + L4 校验和，注入栈
         无映射 + 非 SYN 的 TCP → 丢（Go 侧 Forwarder 对非 SYN 首包同拒）
TX（拦截栈 poll 出的包，注入前）:
  src=(隧道IP,rw_port) 命中反查表 → 重写 src=(orig_dst_ip, orig_dst_port)，
  重算校验和；之后按 dst（客户端 TunnelIP/TunIP）查 peer encap 发出。
  服务真 listener 的应答（src=隧道IP:53 等）与豁免应答（src=隧道IP:7802，
  反查命中豁免映射）同路：豁免映射的反重写把 src 写回 (隧道IP,原端口)（恒等）——
  统一路径，豁免/过境只差 upstream 与日志 kind。
```

- **为什么豁免也走 NAT**：豁免连接（dst=隧道IP:7802 等）栈上没有 listener（files/term/
  speedtest 都在 UDS 上）——它们同样需要栈内 TCP 状态机。统一进重写路径后，「豁免/过境」
  的分叉只在 worker 的 upstream 选择与 kind 日志（E10 的 `exempt|transit` 文案）。
- **DNS 代答监听面在栈内**（对齐 Go 的 listenTunnelDNS）：栈上真 listener bind
  (隧道IP,53) UDP+TCP 与 (隧道IP,5300) TCP——demux 先命中它们，不进 NAT 路径；
  非隧道 IP 的 :53（写死公共 DNS）进 NAT 路径 → 进程内代答腿。
- **校验和**：IPv4 头校验和重算（20 字节）；TCP/UDP 因伪头部含 IP 而**全量重算**（包
  ≤1280B，正确性优先；增量优化挂 R5 性能批）。
- **worker 线程与栈内 socket 的桥**：worker 持本机 socket（阻塞 IO）；worker→驱动
  `TunnelWrite`（unbounded）；驱动→worker `ToUpstream`（unbounded per-worker）+ `Teardown`
  （idle/关闭/对端关）。TCP idle 5min、Drain 语义（serve 收工时 HaltNew + 宽限内自然收
  + 到期 RST——SetLinger(0) 的 Rust 对应 = `TcpStream::set_linger(Some(0))`）。
- **LocalServices 组装边界**（对齐 Go）：serve 装配层建三个 UDS listener（files.sock/
  term.sock/speedtest.sock——listen_local_service：死/活判别拨号探测 + chmod 0600 +
  own 身份删除）；拦截层只拿「端口→sock 路径」的静态映射（建后不改；服务被摘除时条目
  保留，拨号 ENOENT 快速失败回 RST）。term.sock 本期没有 term 服务（R6）——**不建
  term 服务也不建 term.sock**，映射表按「实际起了的服务」装（Go 侧 term.Disabled()
  时同样不起）。speedtest.sock 与 files.sock 起真实服务。
- **拦截层 Stats**（E10 判据同串）：`intercept: tcp %s %v ← %v（dialok）`（kind=transit|
  exempt；**dialok/dialfail 仅 TCP**——UDP 会话不计，观测走 E12 会话行）；计数面
  dialok/dialfail/rejects/flows/udp_sessions(带 downSeen) 与 Go stats.go 同字段。
- **并发上限**：TCP 1024 路（拒绝行 `intercept: tcp 拒绝 %v ← %v（并发上限 %d，在册 %d，
  累计拒绝 %d）` 同串）；UDP 会话 4096。

### 4.2 与 Go 侧的已知差异登记

| 面 | Go（gVisor） | Rust（smoltcp+NAT） | 影响 |
|---|---|---|---|
| Forwarder | SetTransportProtocolHandler + spoof 端点 | NAT 重写 + 动态 listen | 语义等价；新增「重写端口」概念（对客户端透明） |
| UDP spoof 端点 | SO_REUSEPORT 绑原目的地址 | bind 拦截栈地址:rw_port | 应答源地址由 NAT 反重写还原——客户端看到的 src 与 Go 相同 |
| UDP 满 | netstack 回 ICMP | 自建 ICMP responder | 同语义（实现面不同） |
| MSS | 栈协商（MTU 1280） | 同（MTU 1280，max_burst_size=None——R1 ③-2 教训） | 无 |
| SACK/Nagle | SACK 开/Nagle 关（逐端点） | smoltcp 0.11 SACK 恒开；`set_nagle_enabled(false)` 逐 socket | 无 |

## 5. 拆步与判据映射

| 步 | 内容 | 关键判据 |
|---|---|---|
| 3a | `server/device.rs` + `server/bind.rs`：多 peer device（三表分发/漫游/过期重建）+ 出口 ServerBind（腿帧/新源日志/probe 应答）+ **3a 集成测试**（Rust 客户端 connect → 挂 Go-免依赖的自环：先双 Tunn 多 peer 单测，再 Rust client ↔ Rust serve 环回握手+数据） | 单测：多 peer 握手/数据/base 表不变量（`(base<<8)+k`）/过期重建/漫游更新；E23 新源行 |
| 3b | `server/table.rs` + `server/state.rs`：devTag 设备表（cap/TTL/grace/绝不淘汰在线/轮换替换/拒绝归因计数）+ state（key.bin/tokens.jsonl/revoked.jsonl，一轮制 AppendToken/Revoke/Secrets/Ledger） | E6/E7/E8/E9 全族（stale 注入=表满+grace 过期）、E18 台账行、吊销后 `peer: ! reject reason=revoked` |
| 3c | `server/intercept/`：NAT 重写 + TCP 动态 listen + UDP 五元组会话 + pending 重放 + ICMP responder + Stats + LocalServices 映射 | E5 就绪行、E10/E11（对 Rust client 的 --dial transit + 豁免 exempt）、E12 会话行、拒绝行 |
| 3d | `dnsproxy.rs`（resolv.conf 跟随 + 转发 + ID 重写 + question 校验 + TC→TCP + TTL 钳制 + 过滤类）+ `files_server.rs`（六动词 + 路径沙箱 + 原子上传）+ `speedtest_server.rs` | E4/E14/E17/E22；files 对账（Rust client 与 Go files CLI 双对端）；E13 会话行 |
| 3e | `egress.rs`（STUN Binding 客户端/IsPublicAddr/物理网卡枚举/探针）+ `upnp.rs`（SSDP 发现/SOAP 映射生命周期/让位认领/退出缩租）+ STUNQuery（监听 socket 上观测） | UPnP 单测 mock + 「不可本地测」登记；STUN 真实观测一次（stun.cloudflare.com）；E20 显式端点路径行 |
| 3f | CLI `serve` 装配（config.toml 解析=flag 覆盖序、三层 state、日志双通道、token 打印一轮制、公网端点最小集=显式端点路径）+ 判据实测（Go client ↔ Rust exit 全判据、Rust 闭环、多 peer 混跑、TTL/吊销注入、DNS 代答实测）+ R2 移交微项 | 报告全量判据行 |

## 6. 风险与退出口

| 风险 | 缓解 | 退出口 |
|---|---|---|
| **单驱动线程吞吐不达 ±50%**（顶级风险） | 吞吐设计位前置（批量收/poll 出尽/scratch 复用/RX 队列 ≥256）；实测锚 = Go 出口同参数 | checksum 增量化 → 栈拆线程（重审交接面）→ 判据按「同量级」放宽并登记（主会话裁决） |
| boringtun index 不变量（`(base<<8)+k`）不成立 | 3a 首个单测钉死（多轮握手后 base 不漂） | 不成立则改「每 peer 预登记 256 个 index」或逐 peer 试解（+登记） |
| smoltcp 动态 listen 的端口/时序坑（SYN 到达时 socket 未 listen） | RX 路径**先建 socket+listen 再注入首包**（同一次驱动循环内，顺序天然正确）；1c 式自环单测 | — |
| Go 客户端对 Rust exit 的协议差异（腿帧/reg/PSK/时序） | R1 已证 Rust client↔Go exit 互通；服务端是同一线协议的镜像面 + 判据行同串 | 逐面回 §2 差异表排查（R1 同款方法论） |
| 会话过期后同 pubkey 快速重连丢包（R1 移交嫌疑面） | R2 已零复现 + 设备表 rotate/remove 时同步清三表（base/pending-init/pub）| 复现则登记「未再现」路径，挂 R5 fuzz |
| DNS 代答上游依赖本机 resolv.conf | 本地实测（Mac 有真实解析）；CI/离线用单测注入 fake 上游 | — |
