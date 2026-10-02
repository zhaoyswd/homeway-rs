# R4 技术设计（中继：Rust 中继 + Rust exit 注册腿 + 升级条纹对齐）v2

> 真源：baseline 621fe0e `internal/relay/`（relay/leg/control/assoc/reap/status/token/role/cli/logging
> 共 9 文件非测试 ~1,957 行）、`pkg/proto/{relay,relayctl,relaytoken,frame}.go`、
> exit 侧 `internal/server/{relayclient,relayctl,serve,publicendpoint}.go` + `pkg/servercore/bind.go`
> 腿表面、客户端侧升级条纹 `clientcore/cmd/clientcore/tunmode.go:139-150, 862-1002`。
> Rust 侧现状：客户端中继腿（信封/解锁/赛跑）R2 已全量；`serve_cli.rs` 已解析
> `serve.relay`/`[relay]` 节但按裁剪面拒绝（R4 接线点）；session 巡检已有 relay_streak
> 骨架但存在**两处实错**（§5）。
>
> **v2（2026-10-02 技术评审整改后定稿）**：评审 1 高（TCP DH 恒校）+ 6 阻塞项全部并入
> 正文；逐条处置表见文末附录。评审轮次目录 `/tmp/dsh-review/r4d.CSFAfE`。

## 0. 范围与形态

- **4a Rust 中继角色**：`homeway-cli relay`（前台单角色，flag > config.toml `[relay]` 节 >
  内置默认，与 serve 同覆盖序）+ `crates/homeway-core/src/relay/` 模块。零落盘运行态
  （重启即清），唯一 state = `<state>/relay/relay.key`（加载/生成 0600）+ `<state>/cache/relay.log`
  （两级日志：ulogf 终端只出 token/端点、logf 全量入文件，2MB×3 轮转）。
- **4b 升级条纹**：客户端侧行为，R2 已有骨架——本设计 §5 列出与 Go 的两处偏差并修正
  （rearm 硬/软错用 + 升级成功行缺失）。无新增面。
- **4c Rust exit 注册腿**：`serve --relay <rl1…|host:port>` / config `serve.relay` 接通——
  UDP 注册腿（relayclient.go 全语义）+ TCP 控制面（relayctl.go 全语义）+ ServerBind 腿表
  （RegisterLeg/RemoveLeg/ClearLegs + Send 分腿派发 + 空闲回收）+ 客户端 token 并入中继端点
  （`EndpointKind::Relay`，含去重降级与 relayWanted 闸门，§4.3）。
- **4d 判据实测**：三链路（全 Go 只换 relay / Rust 全栈 / Go exit + Rust relay + Rust client）
  + 升级条纹实测 + 两道评审门。**前置**：`tools/local-rust-relay.sh`（镜像 local-relay.sh
  的 state/端口 4274x 段隔离 + wait 行 + 取 token）——4d 第一条判据的载体。
- **裁剪面**（登记不实现）：relay.status/relay.token 控制面命令族（daemon 面归 R7 之后的
  MVP 裁剪；token 终端打印即可）；instance lock（nodestate.AcquireInstanceLock——CLI 单进程
  形态本地测试无并发双跑需求）；v6 承载（与 R3 出口同口径 v4-only，登记 R5；**渲染差异**：
  Go 双栈 wildcard 监听地址打印 `[::]:port`，Rust v4 wildcard 打 `0.0.0.0:port`——4d 对拍
  按语义比对，登记）。

## 1. 必答①：信封换壳的字节面与 relayID 分配/回收

### 1.1 字节面（三条路径，全部零改动转发密文）

| 路径 | 线格式 | Rust 落点 |
|---|---|---|
| 客户端 → 中继 | `[0xAA][relayID(8B)] ‖ [0xBB][type][payload]`（type=0 数据 / type=4 容器搭 reg / 其它 type 原样照转） | `frame::decode_tagged`（R2 已有）剥 9B 路由头，**原样**（含 `[0xBB]` 头）经该客户端的分配 socket 发往 backend |
| 中继 → 客户端 | 后端回程**恒为腿帧**（FIX-91 统一线格式）——中继从分配 socket 收到后**原样**经主监听 socket 发回 `a.key.client`，不做任何转换 | 同左；LEGUP 认证标记（`"LEGUP"‖cookie(16)‖MAC(16)` 恰 37B）吞包不外泄（5B/37B 两形态都吞，bind.go:406-409 防御） |
| 后端 → 中继（注册腿） | `[0xAA][relayID]‖[0xBB][3][子协议]`（type=3 = FrameTypeRelayReg，唯一被中继**消费**不转发的类型） | `relaywire` 子协议编解码（新增**中立模块** `crates/homeway-core/src/relaywire.rs`——relay 与 exit 两角色共用，不挂角色模块） |

子协议族（逐字节对齐 `pkg/proto/{relay,relayctl}.go`）：
`Hello 0x01 pub(32)`（33B）/ `Challenge 0x02 ephPub(32)‖nonce(16)`（49B）/
`Proof 0x03 nonce(16)‖macDH(16)‖macPSK(16)‖ver(1)`（恰 50B，ver 必须 =2；33B/49B 历史形态拒）/
`OK 0x04`（UDP 腿 1B；TCP 控制面 = `0x04‖MAC(16)` 恰 17B，MAC 不足 16B 补零占位）/
`Keepalive 0x05`（0B）/ `Again 0x06`；TCP 控制面专属
`SESSION 0x10 sid(8BE)‖dataPort(2BE)‖cookie(16)`（恰 27B）/
`RELEASE 0x11 sid(8BE)`（恰 9B）；TCP 流分帧 `[2B BE len][msg]`，单条 ∈(0,256]B。
MAC **四族**（域分隔逐字节对齐，实现于 `relaywire`）：
`RelayProofMAC = HMAC-SHA256(dh, "hmac-relay"‖nonce‖pub)[:16]`、
`RelayAuthMAC = HMAC-SHA256(secret, "relay-psk"‖nonce‖pub)[:16]`、
`OKAuthMAC = HMAC-SHA256(secret, nonce‖"ok")[:16]`、
`LegupMAC = HMAC-SHA256(key, "legup-v2"‖sid(8BE)‖cookie)[:16]`。
比较用常量时间（复用 `server/table.rs` 的 `const_time_eq_16`——升 `pub(crate)`）。

### 1.2 relayID 的「分配/回收」= 腿（leg）生命周期，不是中继铸造

relayID = `SHA-256(后端 WG 静态公钥)[:8]`（`frame::relay_id`，R2 已有）——**后端身份的确定性
派生**，中继不分配、不铸造。中继侧的「分配/回收」是 `legs: HashMap<[u8;8], Leg>` 表项：

| 事件 | 表项处置 | 真源行 |
|---|---|---|
| UDP Hello（label 匹配 `RelayID(pubkey)` 才受理） | 表项不存在则建（`last=now`，`legBootstrap=30s` 注册窗起点）；存在则**复用原对象绝不替换**，且**只覆写挑战字段**（pubkey/ephPriv/nonce/challAt）——不刷 last、不清 verified/addr/ctl（防「重 Hello 降级」与「刷窗续命」） | leg.go:61-80 |
| **数据面准入闸**（非 type=3 包） | `leg == nil 或 !(verified ‖ ctlVerified)` → `Dropped++` 拒转发——纯控制腿（无 UDP addr）也过此闸；「准入证明」与「可达路径」（§2.1）是两件事 | relay.go:362-366 |
| TCP 控制面认证通过 | `legForControl`：同样建/复用 + `MaxLegs` 闸（控制路径也过闸，#18）。**闸不过 = 先回 OK 后立即断连**（可回 OK，但绝不保持长连、绝不通告会话——exit 侧已置 handshakeOK 并 ClearLegs，保持长连会让拨腿模式静默死 90s） | control.go:169-180, 240-256 |
| Proof 验证通过 | `verified=true, addr=src, last=now, ephPriv 清零`；地址迁移（addr 有效且 ≠ src）→ 该 label 全部 assoc 作废（关 socket）+ sid 会话补发 RELEASE | leg.go:138-171 |
| reap 过期 | 挂控制连接/已验证（任一）：`LegTimeout 90s` 无活动摘腿（连带全部分配）；未验证无控制：`legBootstrap 30s` 摘 | relay.go:60-66, reap.go:89-111 |
| 控制连接断开的孤儿清理 | 纯控制腿（无 UDP verified 无 addr）**且无活跃会话**才立即删；有会话留 90s 给 LegTimeout | control.go:194-205 |

会话号 sid（`nextSid` 单调）与每会话 cookie（16B 随机，只经控制通道下发）是拨腿模式的
分配物：sid 0 保留 = 「无控制面时代的 fallback 会话」（backend=lg.addr 旧敲洞路径），
控制重连的 replaySessions 把 sid==0 **提升**为拨腿会话（换新 sid + 新 cookie + dialUp，
D1 收敛路径）。

## 2. 必答②：per-client socket 生命周期

### 2.1 为什么存在 + 创建

后端 WG 靠**源地址**区分客户端 endpoint——共用一条 socket 会让多客户端互踩。assoc 键 =
`(label, client_addr)`；首个客户端包（forwardUp）到达且腿**过准入闸**（§1.2）且有可达路径
（`addr` 有效 = 旧敲洞，或挂控制连接 = 拨腿模式）时创建：ephemeral **未连接** UDP socket
（poll 面统一 recvfrom，源校验在认证状态机）。`MaxPerPeer=32`/后端 超限丢弃 + 判据行；
`rand` 失败**不**退化全零 cookie——拆会话让客户端重试（assoc.go:85-94）。

### 2.2 两形态状态机

```text
无控制连接（fallback，sid=0）:
  New ──► backend=lg.addr ──► 稳态（收发均刷 last/lastDown）
  ⚠️ lg.addr 变化（重注册）→ Proof 的 moved 路径全量作废重建；forwardUp 每包比对
     a.backend != lg.addr 也拆旧建新（assoc.go:44-52）；拨腿会话豁免——dialed 会话的
     backend=腿源地址是另一个概念，按 lg.addr 比对会恒不等 → 每包拆会话重建

有控制连接（拨腿会话，sid!=0）:
  New(dialUp=true, dialed=true)   ★ dialed 在建会话时就置 true（与 sid!=0 等价，
     含等腿期——否则等腿窗内 forwardUp 的豁免失效）
  ── SESSION(sid,dataPort,cookie) 通告 ──► 等腿窗（客户端包缓冲 pend ≤16 丢最新；
     DialWait 15s 看门狗——后端拨腿失败不回报，中继必须自持）
  合法 LEGUP‖cookie‖MAC（源任意：首拨/重拨/换源都认）──► authOK+authSrc+backend=src，
     放行 pend（first 时逐条补发）
  已认证源漂移 → 必须重新出示合法认证：未知源不改变 backend、不放行 pend、不续命
     （#3 安全语义：LegRejected 计数 + legRates 独立限速桶 ×10 + 节流日志 n≤3 或 n%100==0）
```

### 2.3 回收（reap 每 `min(5s, min(Idle,DialWait,DownSilent)/2)` 一轮，下限 20ms）

判定顺序（Go reap.go:56-70 同序）：**Idle → DialWait → DownSilent**；命中 Idle 不打逐条日志
（只进聚合计数行），后两者打原因行。聚合行只在 `reclaim>0` 时打。assoc socket 的读错误
（recvfrom 返回错误）= 立即走回收路径（摘表 + RELEASE + `Reclaimed++`）——不留给 reap 轮。

| 判据 | 阈值 | 动作 |
|---|---|---|
| 任一方向空闲 `a.last` | 90s | 摘（无逐条日志） |
| 等腿窗超时 `dialUpAt` | 15s | 摘 + 「拨腿等待超 15s（通告后无 LEGUP——后端拨腿失败/通告丢失）」 |
| 下行静默 `a.lastDown`（上行仍活跃；仅 !dialUp 会话参与——dialUp 会话归 DialWait 管，b1） | 5min | 摘 + 「下行静默超 5m0s（上行仍活跃——半死会话兜底）」 |
| 摘除 | — | 关 socket + sid!=0 时经控制面 RELEASE（写失败即断连——半死 TCP 上 RELEASE 恒丢，关掉逼重连走重放对账）+ 聚合计数行 |

腿过期摘腿连带全部分配；closeAll（收工）：先尽力给全部 sid 会话补发 RELEASE、关全部
socket 与控制连接（确定性收工——Run 返回即可重用同端口）。

### 2.4 hint 递送（**唯一**推送点 = 建会话时）

真源 `assoc.go:104-105`：只在**建会话一处**、两端各推一次（给客户端 = 后端注册腿源地址，
addr 无效不推；给后端 = 客户端在中继眼里的源地址，经分配 socket 发往 backend，dialUp
等待中无 backend 不推）。relay.go:24 包注释的「三个时机」过时；moved 后由客户端下一包
重建会话时自然重推，不做额外时机。hint 一律不可信线索，中继不保证送达。

## 3. 必答③：注册腿挑战时序（exit 侧 4c 同图）

### 3.1 UDP 注册腿（两种模式的校验公式分列）

```text
exit                          relay
 │ ── Hello [0xAA][label]‖[BB][3][01‖pub(32)] ──►  label == RelayID(pub)？
 │                                                ├─ 否 → Forged++（静默）
 │                                                ├─ legs 满（256）→ Denied++ + 判据行
 │                                                └─ 是 → 复用/建腿（只覆写挑战字段），
 │                                                   出题（ephPriv+nonce16，challAt=now）
 │ ◄── Challenge [BB][3][02‖ephPub(32)‖nonce(16)] ─┤ （明文回 src）
 │ dh = X25519(exit 静态私钥, ephPub)               │
 │ ── Proof [AA][label]‖[BB][3][03‖nonce‖macDH‖macPSK‖02] ──►
 │                                                  ├─ 腿在？被换？（对象同一性守卫）
 │                                                  ├─ 挑战 TTL 15s 内 + nonce 匹配
 │                                                  ├─ ver==2（v2-only 门）
 │                                                  ├─ **UDP 面：二选一**——token 模式只校
 │                                                  │   macPSK == RelayAuthMAC(secret,nonce,pub)
 │                                                  │   （DH 谁都算得出，不当准入）；开放模式只校
 │                                                  │   macDH == RelayProofMAC(dh,nonce,pub)
 │                                                  └─ 过 → verified+addr=src+ephPriv 清零；
 │                                                      moved → 旧分配作废+RELEASE；
 │                                                      Registered++ + 判据行
 │ ◄── OK [BB][3][04] ─────────────────────────────┤ exit 侧：first → 判据行「注册成功」
 │ ── Keepalive（25s 拍）──►  刷 last；三分支：腿不在 → Again；换了地址 → Again；
 │                           无 addr 且源 IP ≠ 控制连接 IP → Again（FIX-68 防影子保活）；
 │                           其余静默续命
```

exit 侧循环（`relayclient.go` 全语义）：`relayRetryEvery=5s` 未验证重发 Hello；30s 未确认
一次性告警；验证后 25s 保活。**全部出站走 WG 同一 socket**（§4.2 try_clone——注册腿与
数据面同本地端口，NAT 映射一致，design D4 硬约束 #1）。**入站控制帧源判据 = `src == relay`
全等（IP+端口）**——否则任何人可伪造 OK 置 verified（停重注册）/伪造 Again（DoS 刷注册）。
hint 是另一条判据：src 的 **IP** == 中继 IP（端口可不同——中继 per-client socket 端口动态）
→ 盲打 `3 包 × 150ms 间隔`，每地址 3s 节流，lastPunch 表 >64 清空。

### 3.2 TCP 控制面（同号端口，独立端口空间）

同套 X25519 挑战载荷 + 六点差异：
① **DH MAC 恒校（两种模式都校）**：`RelayProofMAC(dh,nonce,pub)` 先过，token 模式**再叠加**
PSK 校验（control.go:130-146）——控制面必须证明持有 peerId 私钥，否则持 rl1 token 者可
冒用他人 pub/label 接走别人的 SESSION（#29 会话劫持面）。UDP 面才是 §3.1 的二选一。
② 握手 10s 硬期限 + **TCP 面没有 15s TTL**（期限就是 10s 连接 deadline；nonce 逐字节比对）；
握手中并发 ≤16（慢握手洪水防护；认证后释放，长连另设 64 总闸）。
③ OK 恒 17B 带中继身份 MAC（token 模式 `OKAuthMAC(secret, nonce)`——后端认证中继）；
开放模式无密钥可算，零 MAC 占位（后端开放模式本就不校验）。
④ exit 侧 `relayAuthed := secret == 零`——**开放模式即视为已认证、SESSION 照收**；
token 模式 OK-MAC 不过即断连（`!relayAuthed` 分支保留为结构守卫并注明不可达，与 Go 同形）。
⑤ 成功后 attach（**顶掉**旧连接——重连语义；MaxLegs 闸不过 = §1.2 的「先 OK 后断连」）+
**replaySessions**（重连 = 后端已 ClearLegs，中继重放全部活跃会话；sid==0 fallback 会话
提升为拨腿；通告失败**从失败那条起**回滚提升）。
⑥ 读循环：KEEPALIVE 刷 last + **被动回显** KEEPALIVE（B3：后端 25s 发、中继必答），
读超时 90s（= 3×25s+15s）。

**TCP 同号监听失败不致命**：打一行
`⚠️ 控制面 TCP %v 监听失败（%v）—— 退回纯 UDP 中继（拨腿特性缺席）`，继续纯 UDP
中继（UDP 注册腿与转发全功能在；本地矩阵/纯 UDP 部署形态归零不炸）。

exit 侧控制客户端（`relayctl.go`）：拨号 5s 超时 → HELLO → CHALLENGE → PROOF(ver=2) →
OK 形状 17B + token 模式验 OK-MAC → `ClearLegs()` + 判据行「已连——已清腿表，等待会话重放」
→ 保活 25s + 读循环。断线退避 1s→×2→30s 封顶；**曾健康（握手完成）的断线从最小退避重新起**。
SESSION：DataPort 校验（0 / <1024 保留段拒绝）→ `RegisterLeg(sid, relayIP:DataPort,
LEGUP‖cookie‖MAC)`；RELEASE → `RemoveLeg`。

### 3.3 竞态与守卫清单（逐条从 Go 平移）

腿对象同一性（Proof 时 `legs[label] == lg` 才作数）；挑战 **UDP：TTL 15s + nonce** /
**TCP：10s 握手期限 + nonce**（分列）；ephPriv 用完即弃；控制面 attach 顶旧连 + detach
只认现任；MaxLegs 两路同闸；keepalive 的无 addr 腿必须同源 IP 于控制连接（FIX-68）；
replay 的 sid 锁内取值（replaySessions 会在锁下改写 a.sid）；exit 控制帧源全等（§3.1）。

## 4. 线程模型（R1 决议：自管线程 + poll(2)，不引 tokio）

### 4.1 relay 侧：单驱动线程独占全部可变状态 + 短命握手线程

- **驱动线程**（1 条）：独占 legs/assocs/rates/stats/nextSid。poll(2) over
  `[cmd 管道（含 stop）, UDP 监听, TCP 控制监听, 全部 assoc socket, 全部已建立控制 conn fd]`
  ——**cmd 管道与 stop 同一 self-pipe 并列入 poll**（握手线程的交接、测试注入都走它，
  交接延迟上界 = 一次 poll 超时 ≤500ms，不是「等下一轮」）；poll 超时 =
  min(下一 reap 轮, 下一分钟统计, 500ms)。UDP 包 → 限流（rates 200pps/源，先于一切解析）
  → probe 应答 → tagged 解析 → 腿控制/forwardUp；assoc fd 可读 → 认证状态机 → 回程转发
  （经 UDP 监听 socket 写客户端）；控制 fd 可读 → 增量分帧 → KEEPALIVE 刷 last + 回显。
- **控制面读侧落法**（④-1 整改，四件套）：交接后 conn fd `set_nonblocking`；每 conn 一条
  **增量读缓冲 + `[2B len]` 半帧状态机**（poll 可读 ≠ 整帧，绝不做阻塞 read_exact、绝不按
  UDP 习惯 drain 到 WouldBlock——一次半包就把整个中继挂死）；90s 读超时 = `last_read`
  时间戳 + reap 轮**惰性判死**（非 SO_RCVTIMEO/阻塞读）；握手线程交接前**清掉握手期 10s
  deadline**（否则首个读立即超时被当断连）。
- **控制面写侧**：**通告/回显类写全在驱动线程**（SESSION/RELEASE/KEEPALIVE 回显——
  与 forwardUp 的通告天然串行，Go 的 per-conn 写锁 + opCh 语义被线程模型构造性消除）；
  conn fd 非阻塞，单次 `send` 整帧（≤29B）；EAGAIN/短写 = 写失败 → 关连（对端死——
  恢复路径与 Go「写失败即断连」相同：重连 + 重放对账）。**有意差异登记**：Go 的
  announceSession 在唯一 UDP 读循环里同步写、**无期限**（半死对端 = 全中继无限停摆）；
  Rust 非阻塞写 = 有界（最坏丢一帧通告即断连）。握手线程自身的 CHALLENGE/OK 写
  在握手线程（阻塞 fd + 10s deadline，线程短命可挂）。
- **probe 应答契约**（互操作可见）：`probe::respond_ex(pkt, build, 0 /*caps*/, &[] /*无端点列表*/)`
  ——合法探测必应答（带 Build 标记，缺省 `relay-dev`）、无状态（不进任何计数）、非法探测
  静默落 `Dropped++`、防放大 ≤req+45B。Go 客户端 `probe.Reach` 会探测中继端点并据应答
  分档 `TierRelay`——不应答 = relay-only token 被判端点全死（4d 验收面）。
- **握手线程**（短命，并发 ≤16）：TCP accept 后由驱动线程 spawn，**槽位 RAII 守卫**
  （drop 即减——失败也释放，防 16 次失败后永久拒绝新握手）；阻塞跑 HELLO→CHALLENGE→
  PROOF→OK（10s 期限），成功把 established fd 经 cmd 管道交还驱动线程（attach + replay
  在驱动线程做——replay 与 forwardUp 的通告写同线程无竞态；MaxLegs 闸不过 = 回 OK 后
  关连丢弃）；失败即弃（槽位随 drop 释放）。
- **统计与日志**：`中继统计：…` 每分钟一行（驱动线程按 deadline）；relay 侧 14 条判据行走
  logf（只进 relay.log）、ulogf 5 条（终端 + 抄文件）；比对时剥时间戳 + `[relay]` 前缀。

### 4.2 exit 侧：relay-leg 线程 + relay-ctl 线程 + punch worker + 驱动线程腿表扩展

- **装配路径**（④-2 整改，4c-2 第一步）：`ServerBind` 加 `try_clone_socket(&self) ->
  io::Result<UdpSocket>`（std `UdpSocket::try_clone`——dup 出同 fd 副本，生命周期由 std
  管理，不用裸 dup+RawFd）；`engine.rs` 在 bind 创建后（290 行附近）、**move 进驱动线程
  前**（356 行前）clone 一份传给 relay-leg 线程——这是唯一窗口。`ServeConfig` 加
  `relay: Option<String>`；`parse_serve_flags` 加 `--relay` 值参 flag；覆盖序
  flag > config.toml `serve.relay` > 无。`ParseRelayArg` 在装配期解析（rl1 →
  `(端点[0], secret)`，Direct 端点优先、无 Direct 用全端点、端点非 IP:port 报错；裸
  host:port → 开放模式零 secret）。
- **relay-leg 线程**：持有 verified/pending nonce/ephPriv/lastPunch；ticker 5s（可被 stop
  通道打断——收工上界：一轮 tick ≤5s + 在途 punch 收尾）；经 cloned WG socket 发
  Hello/Keepalive/Proof；从 cmd 通道收驱动线程转发的 type=3 控制帧（源全等校验在本线程）
  与 hint 事件（源 IP 校验在本线程）；hint → 投 **punch worker**（单独 1 条线程 + 有界
  队列容量 16、满则丢——3×150ms 节奏 sleep 不进 relay-leg 线程（Challenge 处理不被拖），
  也不 per-hint 起线程（防线程风暴，⑦-4））。
- **relay-ctl 线程**：阻塞 TCP（拨 5s 超时、stop 通道打断 dial/sleep——收工上界 4s 量级）
  + 重连退避循环；SESSION/RELEASE/对账 → `EngineCmd::LegRegister{sid,remote,marker}` /
  `LegRemove{sid}` / `LegsClear` 投给驱动线程；失败只记日志绝不影响出口主服务。
- **驱动线程扩展**：三条新 cmd（拨腿 = 连接 UDP socket + 写 marker + 入腿表 + pollfd
  集合；LegRegister 失败由驱动线程打日志）；pollfds 从定长数组改 Vec；腿 fd 可读 →
  同一 process_packet 解析路径（腿上帧与主 socket 同构）+ LEGUP 标记吞包防御（5B/37B）；
  **POLLERR / recv 错误 / send 失败 → 摘腿 + 判据行「腿（会话 #%d → %v）读循环退出，摘除」**
  （ICMP 拒绝同走此路——Go 每腿读协程退出的等价物）；腿 socket 建时即非阻塞；**recv/send
  双向刷 last**（纯下行活跃的腿不误回收）；**send_wire 派发**：`out.wire` 的 endpoint 命中
  `leg_by_r` → 走该腿 socket 发（回程五元组与拨出映射一致，严格 NAT 构造性穿透）；
  不命中但命中 `leg_recent`（5min 窗）或 `leg_ports`（曾当过腿，4096 满清）→ 丢弃 + 节流
  判据行（#17：往主 socket 发会打到中继主口/被复用的数据口）；否则主 socket。腿空闲 3min
  回收（sweep 30s）；腿上限 64（只拦新 sid，重放替换不拦——C3）；非阻塞腿 send 的
  WouldBlock = 丢包（UDP 无背压语义，登记 vs Go 阻塞写差异——实际影响 = 内核缓冲满的
  瞬时突发，与主 socket send_to 同形态）。收工：清全部腿。
- **type=3 钩子**：`server/bind.rs` 的未知 kind 分支（现为忽略）改为：kind==3 且已装
  `on_leg_frame` 钩子 → 投递（驱动线程只转投不解析——解析在 relay-leg 线程）；未装钩子
  照旧忽略（R3 前形态兼容）。

### 4.3 serve.relay 接线与 token 端点合并（⑥-8 整改，四块语义）

`relay_ep` 在 **TokenCtx 构造前**解析好（rl1/裸地址）并**先于**公网端点线程起跑配置
（token 首轮打印必须带中继端点——Go serve.go:406-409 用户口径）。`print_client_token`
的端点组装四块：
① `add` 闭包收**整个 Endpoint（含 kind）**——relay 端点标 `EndpointKind::Relay`
（57012ad 事故的防线：签名只收字符串时 Relay 标记靠调用点记得带，丢标 = 客户端分桶
全失效）；
② 去重先到先得：**中继地址与已有直连端点重合（--relay 指向出口自己地址的退化形态）
按先到的直连保留** + dlogf 降级行「中继端点 %s 与已有直连端点相同，按直连处理（中继腿不生效）」；
③ 顺序：LAN → 已公布公网 → relay（叠加不踢除）；
④ **relayWanted 闸门**（按地址比对、fail-open）：`relay_wanted && !eps 中含 relay_ep.addr
→ 整轮不打 token`（先打一版不带中继的只会误导）；退化形态（重合被去重）时 hasRelay
按地址仍为 true、闸门放行——有意 fail-open（按 flag 判会让退化形态永久拿不到 token）。

## 5. 升级条纹对齐核查（4b——两处实错，本设计即为整改承诺）

Go 真源（tunmode.go:139-150 + 987-1002）：`relayUpgradeStreak(via, streak)` 纯函数 +
`relayUpgradeDue = via=="relay" && streak>=5`；触发时 `tr.RearmSoft()`（**软赛跑**）+
一发 PathProbe（10s 窗）+ 成功且 via 变化 → 「RELAY-UPGRADE：升级成功 → via=%s ep=%s
rtt=%dms」+ setLink 刷新。

Rust 现状（session/mod.rs:826-839）两处偏差：
1. **`client.rearm()`（硬）误代软赛跑复合动作**：Go `Transport.RearmSoft()` 是复合动作 =
   `Bind.RearmSoft()` + `SetCandidates(cache.Merge(static))` + `refreshDomainAsync()`
   （transport.go:148-154）。硬 rearm 后中继在 DirectFirst 2s 窗内**锁定**，窗口内所有
   出站包跳过中继（不止「下一发」）——直连已死时在用中继路径被中断 2s（正是 RearmSoft
   注释点名禁止的形态），且 RARM 行文案错（应打 `RARM 软赛跑（中继立即参与，同时试直连）`）。
   Rust 修复 = **组合**：`rearm_soft()` + `set_candidates(merged_candidates())`
   （Rust 会话在 hint 打洞与阶梯 R3 都补了候选重投，唯此处漏——自身不一致的闭合）；
   Rust 无域名候选，无 refreshDomain 对应面（登记）。
2. **升级成功行缺失**：探测后未复查 `snapshot().via` 变化、未打「RELAY-UPGRADE：升级成功
   → …」、未刷 set_link（link/UI 最长滞留 60s）。

口径注记：成功拍上的 `NotePathAlive` 是 Go **隧道域**每拍调（tunmode.go:979）、**服务域**
不调（service.go:635-643）；Rust session 收敛服务域 ⇒ 现状一致，条纹从隧道域移植时
以服务域口径为准。其余已对齐（实测核查）：`RELAY_UPGRADE_EVERY=5`/`PATROL_INTERVAL=60s`/
探测窗 10s（perTry）/条纹只在成功拍推进、非 relay 清零/失败拍不动条纹/补注册 5min 正交。

## 6. 拆步、判据与常量

### 6.1 拆步

| 步 | 内容 | 单测锚 |
|---|---|---|
| 4a-1 | `relaywire.rs`：子协议编解码 + MAC 四族 + Ctl 分帧 + **golden 向量**（`tools/vector-gen` 扩 relay 族：Hello/Challenge/Proof/OK17/SESSION27/RELEASE9/LEGUP37 + 四族 MAC 定值 + 分帧 0/256/257 边界 → `fixtures/vectors/relay.json`，走既有 gen-vectors.sh 管线入 SHA256SUMS） | 向量逐字节 + 长度门 + **TCP DH 恒校负例（PSK 对 DH 错 → 拒）** |
| 4a-2 | `relay/mod.rs`：腿/assoc/reap/rates/stats + 驱动线程 + UDP 面 | Hello/Proof 状态机（含 re-Hello 不降级）、moved 作废、keepalive 三分支、reap 三判据顺序、准入闸 |
| 4a-3 | `relay/ctlface.rs`：TCP 控制面（握手线程 RAII 槽 + attach/replay/SESSION/RELEASE/回显保活 + 增量读缓冲状态机 + 监听失败降级） | 握手 OK-MAC、DH 恒校、replay 提升、顶旧连、MaxLegs 闸断连形态、半帧状态机 |
| 4a-4 | `relay/rltoken.rs`（rl1 铸/解析）+ CLI `homeway-cli relay`（relay.key/log/两级日志轮转/端口退让/BuildToken/--open 走隐藏开关 `--open`（测试形态，不进生产命令面文档；构造冲突 = CLI 层 typed error 非 panic）） | token 布局 = Go encodeBody 同构（向量） |
| 4b | session 升级条纹两处修正（组合动作） | RARM 行 + 升级成功行 |
| 4c-1 | `server/bind.rs` 腿表（try_clone_socket/fd 集/poll 派发/send 分腿/POLLERR 摘腿/#17 丢弃/type=3 钩子） | 派发与丢弃表驱动单测 |
| 4c-2 | `server/relayleg.rs`（UDP 注册腿 + punch worker）+ `server/relayctl_client.rs`（TCP 控制）+ EngineCmd 扩展 | 状态机单测（mock socket 面） |
| 4c-3 | serve_cli/engine 接线：`--relay` flag + config + relay_ep 四块语义（§4.3） | 装配序 + 去重降级 + 闸门单测 |
| 4d | `tools/local-rust-relay.sh` + 三链路实测 + 升级条纹实测 + 评审门 + ROADMAP 收口 | — |

### 6.2 常量映射表（判据 = 判据行产出点；时长渲染**一律 Go `Duration.String()` 等价**
= `go_fmt::fmt_duration_go_secs`：90s→`1m30s`、5min→`5m0s`、3min→`3m0s`）

| 常量 | 值 | 真源 | 判据 |
|---|---|---|---|
| IdleTimeout | 90s | relay.go:43 | 聚合回收行 |
| LegTimeout | 90s | relay.go:44 | 「注册腿过期（%v 无保活）—— 摘掉」 |
| legBootstrap | 30s | relay.go:65 | — |
| DialWait | 15s | relay.go:53 | 「拨腿等待超…」 |
| DownSilent | 5min | relay.go:59 | 「下行静默超…」 |
| MaxPerPeer / MaxLegs / MaxCtlConns / ctlHandshakeMax | 32 / 256 / 64 / 16 | relay.go:45-48, control.go:56 | 「分配腿已达上限」「注册腿总数已达上限」×2（leg.go:67 与 control.go:249 **两条不同尾串**）「已建立控制连接达上限」「并发握手上限」 |
| RateLimit / legRate ×10 / 桶清理 2s | 200 / 2000 / 2s | relay.go:46, reap.go:15-25, 113-123 | — |
| challengeTTL | 15s | relay.go:49 | — |
| ctlKeepaliveEvery / ctlReadTimeout | 25s / 90s | control.go:29-32 | — |
| ctlPendMax | 16 | relay.go:141 | — |
| legRejectLog | n≤3 或 n%100==0 | leg.go:217 | 「已拒绝…累计 %d 次」 |
| reapInterval | min(5s, min(Idle,DialWait,DownSilent)/2) 下限 20ms | reap.go:27-38 | — |
| exit：relayRetryEvery / relayKeepaliveEvery / 30s 告警 | 5s / 25s / 30s | relayclient.go:26-31, 119 | 「30s 未确认注册」 |
| exit：punchBurst/Gap/MinInterval/表 64 | 3 / 150ms / 3s / 64 清空 | relayclient.go:28-30, 204 | 「盲打 3 包」 |
| exit：ctlDialTimeout / 重连 1→30s | 5s / 1s×2 封顶 | relayctl.go:28-32 | 「…后重连」×2 形态 |
| exit 腿表：max 64 / idle 3min / sweep 30s / recentTTL 5min / legPortsMax 4096 | — | servercore/bind.go:189-198, 123 | 「空闲超 3m0s，回收」「读循环退出，摘除」「丢弃出站 %d 包」 |
| 条纹：relayUpgradeEvery=5（tunmode.go:137-139）/ patrol 60s（:132）/ probe 10s（:861） | — | tunmode.go | RELAY-UPGRADE 两行 + RARM 软赛跑（目标串真源 wtransport/bind.go:289） |

**Stats 字段 ↔ 失败路径映射**（Go 测试钉死，Rust 对齐）：
`Registered++`（Proof 过，leg.go:160）/ `Forged++`（label 不匹配、Proof 畸形、TTL/nonce
不符、PSK 不过、TCP DH 不过——admission_test.go:56）/ `Denied++`（MaxLegs 两路、
MaxCtlConns）/ `Assigned++`（建会话）/ `Reclaimed++`（一切回收路径**含 DialWait 超时**
——control_test.go:472）/ `Dropped++`（限流、非 tagged、准入闸、无路径、MaxPerPeer、
ctlPendMax 满、合法探测**不**计——probe_respond_test.go:43-49）/ `LegRejected++`（拨腿
会话未知源）/ `ForwardedUp==0`（未知 peer 的数据帧——relay_test.go:353）。

### 6.3 判据行清单（relay 侧新增入 INTEROP-CRITERIA；全串以 baseline 为准、逐字对齐；
**relay 侧 logf 只进 relay.log、ulogf 终端兼抄文件；exit 侧进 serve 日志——比对先剥
时间戳与前缀**）

`中继就绪：%v（%s；分配回收 %v，注册腿过期 %v，每源限速 %d pps，每后端最多 %d 条分配，腿总数上限 %d）`、
`中继控制面：TCP %v 就绪（后端拨腿模式可用）`、
`⚠️ 控制面 TCP %v 监听失败（%v）—— 退回纯 UDP 中继（拨腿特性缺席）`、
`⚠️ 监听端口 %d 被占用（%v）—— 自动往后找`、`中继改用端口 %d（token 里写的就是它）`、
`中继：注册腿总数已达上限 %d，拒绝新的 %x（防匿名洪水）`、
`中继：注册腿总数已达上限 %d，拒绝控制面新腿 %x`、
`中继：后端 %x 注册成功（腿 %v）`、
`中继：后端 %x 注册腿地址变化 → %v（旧分配 %d 条已作废，等客户端重建）`、
`中继：后端 %x 的 token 校验不过（密钥不对/没带 token）—— 拒绝`、
`中继：后端 %x 注册证明协议版本 %d 不符（需要 %d）—— 拒绝`、
`中继：后端 %x 控制面就绪（%v；SESSION 通告启用拨腿模式）`、
`中继：后端 %x 控制面重放 %d 条活跃会话`、
`中继：控制面 %v 协议版本 %d 不符（需要 %d）—— 拒绝`、
`中继：控制面 %v 的 DH 校验不过 —— 拒绝`、
`中继：控制面 %v 的 token 校验不过 —— 拒绝`、
`中继：并发握手上限 %d 已满，拒绝 %v（慢握手洪水防护）`、
`中继：已建立控制连接达上限 %d，拒绝 %v（label %x）`、
`中继：客户端 %v 起会话 #%d（拨腿模式）→ 后端 %x（数据口 %v）`、
`中继：客户端 %v 起一条分配腿 → 后端 %x（中继侧出口 %v）`、
`中继：后端 %x 的分配腿已达上限 %d，丢弃新客户端 %v`、
`⚠️ 中继：会话随机数不可用（%v）——已放弃本次会话，客户端重试即可`、
`中继：会话 #%d 的后端腿重拨 → %v（cookie 认证通过，跟随）`、
`中继：会话 #%d 收到未知源 %v 的包（%dB）—— 已拒绝（未认证不得成为腿；累计 %d 次）`、
`中继：会话 #%d 回收：拨腿等待超 15s（通告后无 LEGUP——后端拨腿失败/通告丢失）`、
`中继：会话 #%d 回收：下行静默超 5m0s（上行仍活跃——半死会话兜底）`、
`中继：回收 %d 条空闲分配腿（当前 %d 条）`、
`中继：后端 %x 注册腿过期（%v 无保活）—— 摘掉`、
`中继统计：注册腿 %d（累计成功 %d，伪造 %d）｜分配腿 %d（累计 %d，回收 %d）｜转发 上 %d / 下 %d 包｜丢弃 %d`、
ulogf 族：`已生成中继鉴权密钥（%s/relay.key，0600）—— 重启不变，token 因此稳定`、
`日志：%s —— 终端只出 token 与端点变化`、`中继 token：%s`、`端点：%s`、
`⚠️ 公布的地址都在内网：公网中继请加 --advertise <公网IP:端口>`、
`⚠️ --advertise %q 的端口 %d 与实际监听口 %d 不一致：token 里写的是 %d —— 除非前面有 NAT 端口映射，否则后端连不上`；
exit 侧：`中继：注册腿开跑（中继 %v，%s）`、`中继：注册成功（腿 %v → %v）—— 客户端可经它到达本机`、
`中继：对方要求重新注册（中继重启过或注册腿过期）—— 重新走挑战响应`、
`⚠️ 中继 %v 30s 未确认注册：检查地址是否正确、中继是否在跑、UDP 是否通（本机 → 中继）`、
`中继：忽略来自未知源 %v 的地址线索（应为中继 %v）`、
`中继：收到对端地址线索 %v → 盲打 3 包（开自己 NAT 过滤；能否直连仍由 WG 握手决定）`、
`中继控制面：中继身份已认证（OK-MAC 通过）`、
`中继控制面已连（%v）—— 已清腿表，等待会话重放`、
`中继控制面断开（%v）—— %v 后重连` / `中继控制面退出 —— %v 后重连`、
`中继控制面：会话 #%d 已拨腿 → %v（认证腿）`、`中继控制面：会话 #%d 已拆腿（RELEASE）`、
`中继控制面：会话 #%d 拨腿失败（→ %v）：%v`、
`中继控制面：会话 #%d 的数据口非法（port=%d，保留段）—— 忽略`、
`中继控制面：SESSION 通告畸形（%v）—— 忽略`、
`中继控制面：拒绝未认证通道下发的 SESSION（疑似伪造/降级；累计 %d 次）`、
`腿（会话 #%d → %v）空闲超 3m0s，回收`、`腿（会话 #%d → %v）读循环退出，摘除`、
`腿已摘或非现任（%v）丢弃出站 %d 包（等控制面重放重建腿）`（dlogf）；
客户端：`RARM 软赛跑（中继立即参与，同时试直连）`、
`RELAY-UPGRADE：已在中继停留 5m0s，重新武装赛跑试直连（下一发出站包镜像到全部候选）`、
`RELAY-UPGRADE：升级成功 → via=%s ep=%s rtt=%dms`。

## 7. 风险与有意差异登记

1. **v4-only**（R3 同口径）：中继监听/端点/腿全部 v4 面；v6 承载（含 BuildToken 的 /64
   去重对 v6 端点）登记 R5 与出口 v6 批一起补。本地矩阵纯 v4 无差异。**渲染差异**：
   Go 双栈 wildcard 监听打印 `[::]:port`，Rust v4 wildcard 打 `0.0.0.0:port`——4d 对拍
   按语义比对并登记。
2. **控制面写侧有界化**（§4.1）：Go 半死控制连接 = 唯一 UDP 读循环内同步写、无期限
   （全中继停摆）；Rust 非阻塞写最坏丢一帧通告即断连——「有界且更优」，登记。
3. **relay.log 轮转**：2MB×3 同参实现；打不开不致命（同 Go，终端一行提示）。
4. **测试形态 `--open`**：显式开放注册仅测试用（与 Go 的「Secret/Open 恰好一者」同校验，
   冲突 = CLI 层 typed error，非 panic）；不进生产命令面文档。
5. **token 模式恒生产默认**：`--advertise` 缺省时公网探测只取公网地址（egress 已有
   `is_public_addr`）+ 同 /64 去重（v6 面虽不承载，去重逻辑就位待 R5 启用）。
6. **exit 腿非阻塞 send WouldBlock = 丢包**（与主 socket send_to 同形态；Go 为阻塞写——
   UDP 内核缓冲满的瞬时突发场景，登记）。

---

## 附录：技术评审处置表（v1 → v2，2026-10-02）

评审渠道：dsh 外部评审（`/tmp/dsh-review/r4d.CSFAfE`，prompt/output 存档）。结论
1 高（①-1）/6 阻塞 + 中低若干，逐条处置：

- **①-1 TCP DH 恒校（高）** ✅ §3.2① 分列两面校验公式 + 负例单测锚（4a-1）。
- **④-1 驱动线程读侧** ✅ §4.1 四件套（非阻塞/增量半帧状态机/lastRead 惰性判死/清期限）。
- **④-2 try_clone 装配路径** ✅ §4.2 装配路径（访问器/唯一窗口/ServeConfig+flag/stop 通道）。
- **⑤-1 rearm 复合动作** ✅ §5 改组合动作（+set_candidates），登记无域名候选面。
- **③-3 MaxLegs 失败形态** ✅ §1.2/§3.2⑤ 「先 OK 后断连、绝不保持长连」。
- **⑥-8 token 端点合并** ✅ §4.3 四块（kind 结构流动/去重降级行/顺序/relayWanted 闸门 fail-open）。
- **①-2 TCP 监听失败降级** ✅ §3.2 末段 + 判据行入 §6.3。**①-3 准入闸** ✅ §1.2 表行 +
  §2.1 分列。**①-4 re-Hello 不降级** ✅ §1.2。**①-5 文案（四族/type=4/渲染）** ✅ 全改 +
  §7.1 登记。**①-6 模块归属** ✅ `relaywire.rs` 中立模块。
- **②-1 hint 时机** ✅ §2.4 改「唯一推送点 = 建会话」。**②-2 dialed 时机** ✅ §2.2 创建即
  true。**②-3 reap 顺序/日志口径** ✅ §2.3。**②-4 assoc 读错误** ✅ §2.3（立即回收路径）。
- **③-1 exit 控制帧源全等** ✅ §3.1。**③-2 开放模式 SESSION 放行** ✅ §3.2④（relayAuthed :=
  secret==零，`!relayAuthed` 分支注明不可达保留）。**③-4 TCP 无 15s TTL** ✅ §3.3 分列。
- **④-3 cmd 管道入 poll** ✅ §4.1（同一 self-pipe，交接延迟上界 500ms）。**④-4 腿
  POLLERR/双向 last/判据行** ✅ §4.2。**④-5 握手槽 RAII** ✅ §4.1。**④-6 线程收工上界**
  ✅ §4.2（stop 打断 tick/dial/sleep）。**④-7 probe 应答契约** ✅ §4.1。**④-8 措辞收窄**
  ✅ §4.1（「通告/回显类写全在驱动线程」）。**④-9 短写** ✅ §4.1（非阻塞单次 send 整帧，
  EAGAIN/短写 = 关连；与读侧非阻塞一致，弃 SO_SNDTIMEO 方案）。**④-10 澄清** ✅ §7.2
  改述（Go 无期限全停摆 → Rust 有界，登记为更优）。
- **⑥-1 3m0s** ✅ §6.2/§6.3。**⑥-2 渲染口径** ✅ §6.2 表头。**⑥-3/⑥-4 判据行全串补全**
  ✅ §6.3 重写为全串 + 备注以 baseline 为准。**⑥-5 Stats 映射表** ✅ §6.2 末段。
  **⑥-6 出处修正** ✅。**⑥-7 频道/前缀标注** ✅ §4.1/§6.3 表头。
- **⑦-1 golden 向量** ✅ 4a-1 走 vector-gen 管线。**⑦-2 local-rust-relay.sh** ✅ 4d 前置。
  **⑦-3 --open 不进生产命令面** ✅ §7.4。**⑦-4 punch 有界单 worker** ✅ §4.2。
  **⑦-5 rltoken 改名/const_time_eq pub(crate)** ✅ 4a-4/§1.1。**⑦-6 typed error** ✅ §7.4。
