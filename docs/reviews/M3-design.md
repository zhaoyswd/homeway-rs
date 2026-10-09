# M3「服务流迁移（STREAM tag；客户端 stackb 退役）」设计

> 本文件 = M3 期的**设计真源**（设计门第一道）。分工照 M1/M2 先例：
> ① §0 复验（回源码重定位 + 绿基线 + 与路线文件旧记数的差异登记）；
> ② §1–§7 方案定稿（STREAM 协议 / 出口分发 / 阶梯重写 / 归因回传 / 退役清单 / 地址派生收窄 / 预算）；
> ③ §8 判据行影响与**登记条目草案**（逐条「日期 / 条目 / 从→到 / 原因 / 影响面」）；
> ④ §9 真机验证计划；⑤ §10 风险与未决；⑥ §11 实施清单（S1–S10）；⑦ §12 待拍板/待裁决；
> ⑧ **§13 实测锚点（`/tmp/m3lab` 验证台原始行，已落盘可复核）**；
> ⑨ **§14 设计门记录（dsh r16 首轮 27 条 + r17 复审 16 条，逐条处置；结论 = 有条件通过）**。
> **本棒不写产品代码**（只写本文件 + `/tmp` 验证台）；实现期订正由主会话按体例追加为 §15 起。
> 真源：`docs/QUIC-ROADMAP.md` M3 节 + 附录 E + 每期执行协议/评审协议；`docs/reviews/M2-design.md`
> §13/§14/§15；`docs/reviews/M1-design.md` §3.3/§2.5；`docs/INTEROP-CRITERIA.md`。

---

## 0. 复验（回源码重定位；行号以 **539567f** 工作树为准）

### 0.1 工作树与并发批（**订正**）

- 检出 = `/Users/zhaozhe/Documents/projects/homeway-rs-quic`（分支 `quic`）。本棒开工时 HEAD = `bf97fc5`；
  **设计门期间并行小批落了两个 commit ⇒ 现 HEAD = `539567f`**（`7097fe1` 探针卫兵+中继腿地址归一、
  `539567f` 候选/学习集/诊断面地址键归一，均 = 路线文件「下一步」第 2 条的同类归一收口批）。
- **隔离面（订正）**：该批**改了** `crates/homeway-core/src/facade/tun_exec.rs` 与
  `crates/homeway-core/src/session/mod.rs`（本设计的主战场）⇒ 隔离面**不再是「无交集」**，
  而是「已把 M3 的源码基线推进到 `539567f`；本设计的所有行号引用以 `539567f` 复核」。
  **实现棒开工必须先 `git log --oneline -3` 复看是否又有新 commit，并把行号再核一遍**（行号会漂）。
- 路线文件点名的同类归一已落地：`probe.rs` 的 `probe_addr_acceptable` 卫兵与 `server/relayleg.rs`
  的 `parse_relay_arg` 归一**已在 `7097fe1`/`539567f` 完成**（本设计**不碰**这两个文件的语义，
  只在 §5.3-A8 引用 `probe.rs` 的地址接受集作为**候选侧**卫兵先例）。

### 0.2 退役对象与消费点（逐条回源码重定位；路线文件旧行号已漂）

| 项 | 路线文件记 | 本树实测（`539567f`） | 备注 |
|---|---|---|---|
| 栈 B 本体 | `wgcore/{mod,stackb}` | `wgcore/stackb.rs` **548 行**（5 个 `#[test]`）+ `wgcore/mod.rs` **2273 行**（`.stack` 引用 **23 处**、`StackB/TunDevice/smoltcp` 名 **19 处**、9 个 `#[test]`） | 附录 B 记 2273+548 = 2821 行 |
| 隧道域消费点 | `tun_exec.rs:238/334/349` | **已漂移**：`tun_exec.rs:310-322`（`session_connect`，桥拨号）→ `healing_dial`（**407-455**，`connect_deadline` 在 **435/450**）；`tun_exec.rs:330-341`（`session_connect_target`，pf 裸拨，`connect_deadline` 在 **339**） | 旧 238≈桥拨号、334/349≈pf 裸拨 |
| 服务域消费点 | `session/mod.rs:648/667` | **已漂移**：`session/mod.rs:628/634`（`healing_dial_port`/`_addr` 公开面）→ `healing_dial`（**641-671**，`connect_deadline` 在 **645/664**） | 同函数被 **daemon/CLI** 复用（见下） |
| **路线文件漏记的消费点（本设计补全）** | — | `files.rs:207`（7802）、`facade/service_exec.rs:577`（7724/7803 桥）、`daemon/mod.rs:295/309`（carriers 拨号缝）、`daemon/mod.rs:393`（host 会话服务流）、`homeway-cli/src/main.rs:1583`（remote attach）、`daemon/carriers/dnsq.rs`（5300 解析腿）、`main.rs:1413`（`udp_*` dnstest） | **真消费点 = 12 处 / 7 文件**，其中 **6 处属 CLI/daemon（WG-only 面）** |
| UDS 豁免分支 | 「intercept 豁免命中端口 → UDS」 | `server/intercept/mod.rs:1196-1206`（`route_upstream`：`local_services.get(&port)` → `DialTarget::Unix`）；映射装配 = `server/engine.rs:392-396`；`DialTarget::Unix` 建流 = `mod.rs:308` | E5 行在 `mod.rs:866-869` |
| 阶梯结构 | `session/recover.rs` R1/R2/R3 | `recover.rs:30-58`（`Level::{R1,R2,R3}` + `level_name`）、`:170-260`（`run_ladder`）、`:24-28`（`PRE_PROBE=3s`/`VERIFY=10s`/`ACTION=2s`）；驱动器 = `session/mod.rs:1152-1338` + `tun_exec.rs:2355-2665`（`FAIL_STREAK_LADDER=3`） | 门槛真源 = 路线文件门槛表「断线恢复 ≤3.5s」 |
| 服务入口（出口侧） | 「从 UDS accept 换 stream 适配器」 | 三处 `listen_local_service` + `serve_stoppable`：`files_server.rs:255/269/304/332`（`UnixStream` 耦合 **32 处**）、`speedtest_server.rs:231/329/399/477`（**25 处**，`arm_io(&UnixStream, deadline)` = per-syscall `SO_RCVTIMEO`）、`term/service.rs:892`（3 处）+ `term/wire.rs:59-238`（**poll(2) 驱动的帧 I/O 内核**） | 「应用层零改动」的成本主体在这里（§2.2 已据此改方案） |
| 岛的流面 | 「巡检 = `STREAM[probe]` 占位」 | `crates/homeway-quic/src/cmd.rs:26-107` 的 `Cmd` **零 stream 成员**；`client/mod.rs:295-330` 的 `probe()` 注记「**不是端到端回显**（等 M3）」；`exit/conn.rs:165-330` 只消费**首条** bidi 控制流 | M3 需新增整条流面 |
| M2 交下未办 | — | M2 设计 §15-1：**出口 quic 快照的外部只读面（`serve status --json` 的平级 additive 段）留 M3 观测面期** | 本设计漏接过一次（设计门 F/3-4），**已补进 §11-S2 完成判据** |

### 0.3 绿基线（`cargo test --workspace`）

- 本树：**691 passed / 1 failed / 4 ignored**（`homeway-core --lib`）+ 其余目标全 ok ⇒ 唯一红例 =
  **`term::service::tests::attach_size_applies_to_pty`**（`term/service.rs:4012` 的 60s 硬期限 panic）。
  **隔离复跑**（`--test-threads=1` 单跑）= **绿（0.87s）** ⇒ 归**在册 flake 族**（路线文件「已知 flake
  登记」明列此例）。跑基线期间本棒在 `/tmp` 编译验证台（CPU 竞争）⇒ 按纪律**不判回归**。
- 结论：**基线绿**（在册 flake 1 例，隔离绿；无本设计引入的红）。

### 0.4 与既有记数的差异登记（**两处，均以本树实测为准**）

1. **M2 真机发现②的归因订正（32s/41s 的真正驱动源）**：M2 记「出口重启自愈 ≈32s/41s（**巡检拍驱动**）」。
   本棒 `/tmp` 复现**证伪**该归因：**驱动源是 QUIC 连接级空闲回收**——无流量连接在对端进程死亡后
   `close_reason()` 在 **40.029s / 40.021s** 才置位（两次；= `max_idle_timeout` 30s + `keep_alive_interval`
   10s 相位），**正是真机 32s/41s 的落点**；60s 巡检拍解释不了 32s（§13 原始行）。
   ⇒ **快探（应用层回显）是唯一能在 3.5s 内定音的判据**（§3 的承重点）。
2. **「5 处消费点」的不完全统计**：实测 **12 处 / 7 文件**（§0.2），其中 **6 处属 CLI/daemon 且只走 WG 承载**
   ⇒ 直接决定「stackb 与 UDS 分支能否在 M3 删净」（§5.2、§12-①）。

---

## 1. STREAM 协议定稿（tag / 首帧契约 / 并发 / 背压 / 读语义 / 半关 / 错误面）

### 1.1 tag 的线格式

- **一条 bidi 流 = 一个服务会话**；**首字节 = tag**（客户端→出口方向，紧随流开）：`1=files`、`2=term`、
  `3=speedtest`、`4=dial`（M4 换轨，本期限定协议）、`5=probe`；其余取值 ⇒ 拒（§1.6）。
- **多路复用语义**：tag 只在**流**上、不在连接上；同一设备连接上多流并发、独立排序、独立流控。
  tag **不复用**控制流（首条 bidi 流仍是 `hr-reg4` 准入面）⇒ **控制流与数据流严格分离**。
- **受理前置**：服务流**只在已绑定连接上受理**（准入完成 = 设备身份成立）；未绑定连接上的服务流按 §1.6 拒。
- **地址族（显式登记）**：**本期限定 IPv4**——`dial` tag 的目标 = `[4B IPv4][2B BE port]`（与今天
  `session_connect_target(dst: SocketAddrV4)` 同族）。**IPv6 目标为已知不支持**（今天亦不支持；M4 若需扩族另开登记）。
- 反向（出口→客户端）**不开流**：M3 全部服务是「客户端发起」形态（与今天拨号模型一致）。
- **`dial` tag 缺「出口本机」语义位（已登记为 M4 必解）**：今天 portfwd 的「出口自己」把环回/未指定
  **映射**成 `SERVER_TUNNEL_IP:port`（`portfwd.rs:152-170`），由出口的豁免臂拨自己的 `127.0.0.1`。
  新 wire 只带 6B 目标 ⇒ 「出口本机」无处安放（`SERVER_TUNNEL_IP` 送到出口后出口没有该地址）。
  **M3 只登记该缺口，M4 设计门必列**（§5.3-A1③）。

### 1.2 每类服务的首帧契约（**应用层帧逐字节不变**）

| tag | 流上首字节之后的内容 | 出口侧入口（今天 → M3） | 逐字节不变面 |
|---|---|---|---|
| **1 files** | 原样：出口先发问候行 `{"ok":…,"root":…,"ver":1}\n`，随后请求/响应行与 `[4B BE len]` 流式帧**零改动**（`files.rs:1-30`） | `FilesServer::serve_conn`（`files_server.rs:332`） | `fixtures/vectors/files_frames.json` + 23 个单测（**含 5 条 busy 路径用例**） |
| **2 term** | 原样：HSP 帧（`term/wire.rs`）；客户端桥侧 48B 首包令牌**不在本层**（那是 App→桥的 UDS 鉴权，`bridge_host.rs:11-18`） | `TermService::serve_conn`（`term/service.rs:892`） | `fixtures/term*` + `term_manifest_eval` + 29 个 term 单测 |
| **3 speedtest** | 原样：warmup/窗口/结算帧（`speedtest_server.rs:231-470`） | `SpeedtestServer::serve_conn` | 10 个 speedtest 单测（**含 busy 报表**） |
| **4 dial** | `tag ‖ [4B IPv4][2B BE port]` 后 = **裸字节管** | 出口本机拨号（M4） | —（M3 定协议 + 拒未启用 + 登记「出口本机」缺口） |
| **5 probe** | 客户端写 N(≥1) B ⇒ 出口**原样回显**直到客户端半关 | 出口回显任务（新增） | —（纯字节搬运，无帧） |

「零改动」的**保证手段（改方案后 = 构造性）**：服务入口**保持接收 `UnixStream`**（§2.2 方案 B′），
三服务的帧层/期限/poll/`try_clone`/`shutdown` 语义**一行不改**；对照证据 = ①既有单测原样全绿
（UDS 形态，同源回归网）+ ②**新增「异常序列对照矩阵」**：把同一请求序列（正常 / 客户端先半关 /
服务端先半关 / 双向同时半关 / 复位 / 超时 / 队列满）分别在 UDS 与 STREAM 上跑，比对
**响应字节流 + 终止原因（FIN / RST / 错误码）**（不只比对成功路径的 sha256——设计门 1-3）。

### 1.3 流的开/关与半关（实测锚点见 §13）

| 事件 | 实测（quinn 0.11.12） | M3 契约 |
|---|---|---|
| 出口 `finish()`（FIN） | 客户端 `read` = `Ok(None)`；重复读仍 `Ok(None)`（幂等 EOF） | 半关 = FIN |
| 客户端 `write` 于对端 FIN 之后 | `Ok(())`（半关**单向**） | 允许（files「写关流即提交、提前关流即取消」依赖单向半关） |
| 本地 `reset(code)` 后再写 | `Err(ClosedStream)` | 复位后写 = 错误 |
| 对端被 `reset(code)` | 对端读 = `Err(Reset(code))` | 复位**携带应用错误码**（§1.6） |
| 客户端 `close_write` | `SendStream::finish()` | 与 `WriteHalf::close_write`（UDS `shutdown(WRITE)`）同义 |
| 正常收工 | `finish()` + drop（**不用 reset 做正常收口**） | 与今天有界 `shutdown` 对齐 |

### 1.4 背压（形态照 `wgcore` 的 `WriteOut` 先例）

**实测**：对端不读时 `SendStream::write_all(8 MiB)` **阻塞**（1.5s 未完成、连接未断）⇒ STREAM 是**真背压**
（流控），**不是** DATAGRAM 的「满则丢 + 计数」（§13 `semantics` 台）。

- **岛内纪律**：命令循环**绝不允许**被单条流的背压挂住（单线程 runtime；一条慢流不得成为全岛队头阻塞）。
- **形态照 wgcore**：`Cmd::StreamWrite { id, data, reply }` 回执
  `StreamWriteOut { n: usize, back: Option<Vec<u8>> }`（**逐字段同形** `wgcore/mod.rs:120-123`）。
  **实现约束**：`n` 由**非阻塞**判定给出（`SendStream` 可用额度 / 有界待发队列余量），
  **不得**在命令循环里 `await` 到写满；每条服务流 = 一个写者任务 + 有界待发队列（默认 **64 KiB/流**，懒分配）。
- **`n=0` 的语义必须在文档里说死，且不许写成「按 io::Write 契约重试」**（设计门 2-2）：
  `io::Write` 的 `Ok(0)` = **通道关闭**（`write_all` 立即 `WriteZero`）——这正是本仓被真机打过的坑
  （`tun_exec.rs:242-250` 的长注释：上行 bulk 一进慢链路就整轮断流）。**正确表述**：调用方走**仓内既有的
  Ok(0) 分级退避环**（`tun_exec.rs:254-303`：2ms→10ms→20ms 封顶 + **10s 无进展上界** + 原 `Vec` 带回不重拷）。
  **登记**：该环的节拍/上界随承载变化（旧 1 MiB TCP 缓冲 → 新 64 KiB 待发队列），须同批登记。
- **读侧背压（设计门 2-3 部分处置）**：
  ①**岛侧 = 按需拉取**：岛**只在收到 `Cmd::StreamRead` 时才读**（每次读返回一块给该命令的回执），
  **禁止**岛侧主动排空 `RecvStream` 进无界通道；②**客户端侧交接通道仍无界**（**如实登记，见 §5.3-A14**）：
  `files.rs:213-229` 的读者线程以紧循环 `client.read(id) → tx.send()` 推入**无界 `std::sync::mpsc`**
  （今天与 stack B 的 1 MiB rx 缓冲同构）⇒ **`stream_receive_window` 不构成该通道的上界**；
  处置：M3 **登记**（与今天同形，不是回退），**后续切片**可给该通道加字节上界（消费慢 ⇒ 读者线程停发
  `StreamRead`）——列入 §11-S3 的「可后补」项。
- **bidi 额度自记账的定义域（设计门 N14，写死）**：控制流**终身占用 1 个额度**（`exit/conn.rs:141` 的
  `_keepalive_send` 刻意保活 + `refresh_loop` 的接收半边无期限）+ probe **持久流占 1** ⇒
  **自记账域 = {控制流, probe 持久流, 服务流}，有效服务流容量 = `max_concurrent_bidi_streams − 2`**
  （默认 64 ⇒ 有效 62）。该常量接进 §1.7/§8.2-16。

### 1.5 读/写/关的同步面契约（**读语义是原稿的重大缺口，本节补齐**）

今天的三条语义（`wgcore/mod.rs:1303-1330`）必须逐条保真：

| 操作 | 今天 | M3（STREAM）契约 |
|---|---|---|
| `read(id)` | **无限期挂起**（`rx.recv()`；引擎把 `wait_read` 挂到数据/EOF/错误才回执），**无预算** | **同样无限期挂起**；取消只经 `Cmd::StreamClose`（**不得**给读套 `QUIC_RPC_BUDGET`——files 空闲 5min（`files_server.rs:28 IDLE_TIMEOUT`）、term 腿可挂数小时，套 5s 预算 = **空闲即自断**） |
| `write(id, data)` | 有界回执（`WRITE_REPLY_BOUND=10s`）+ `WriteOut{n,back}`；`n=0` = 背压 | 同形（§1.4）；`n=0` 走仓内退避环 |
| `shutdown(id)` | 半关（FIN），有界变体 `shutdown_bounded` | `finish()`；有界变体保留（收工链用） |
| `close(id)` | 复位（abort）+ 有界变体 | `reset(code)`；有界变体保留 |

### 1.6 错误面（拒 / 超时 / 服务不支持 ⇒ 分类 + 归因行）

**双通道**：①**应用层帧**（协议自身能表达的拒绝——files 的 `server_busy` 稳定码、speedtest 的
`error:"busy"` 报表；**逐字节不变**）；②**流复位/连接关闭的错误码**（协议表达不了的拒绝）。

**流复位码表（新增；`VarInt`，出口写、客户端 `ReadError::Reset(code)` 读）**：

| 码 | 名称 | 触发 | 客户端 `StreamErr` | 出口归因行（新，additive） |
|---|---|---|---|---|
| `0x21` | `TAG_UNKNOWN` | tag ∉ {1..5} | `BadTag` | `quic: 服务流拒（dev=%s tag=%d；未知 tag；第 %d 次）` |
| `0x22` | `SERVICE_DISABLED` | 该服务未启用（`HOMEWAY_TERM=off` / 无 speedtest 监听 / files 监听失败） | `NotSupported` | `quic: 服务流拒（dev=%s tag=%s；服务不可用；第 %d 次）` |
| `0x23` | `INTAKE_FULL` | 入口队列满（§1.7 配额） | `Busy` | `quic: 服务流拒（dev=%s tag=%s；入口队列满 %d/%d；第 %d 次）` |
| `0x24` | `UNBOUND` | 未绑定连接上的服务流 | `Unbound` | `quic: 服务流拒（dev=— tag=%s；连接未绑定；第 %d 次）` |
| `0x25` | `DIAL_REFUSED` | `dial` 目标拒（**M4**） | `Refused` | （M4 登记） |
| `0x26` | `DIAL_TIMEOUT` | `dial` 目标超时（**M4**） | `Timeout` | （M4 登记） |
| `0x27` | `TAG_READ_TIMEOUT` | 开流后未在预算内读到 tag（§1.7 的队头阻塞面） | `BadTag` | `quic: 服务流拒（dev=%s tag=—；tag 读取超时；第 %d 次）` |

- **`StreamErr` 落成 enum**（thiserror + `#[non_exhaustive]`；**禁字符串错误**；不复刻
  `DialError::Stack(String)` 的既有债）。
- **`0x22` 含「files 监听失败」是*有意选择***（设计门 N16）：方案 B′ 的 QUIC 源本不依赖 UDS 监听器，
  但选「UDS bind 失败 ⇒ 该服务 QUIC 腿一并停用」是**保守且与今天同形**（`server/engine.rs:535-540`
  今天就是整服务不可用）——**写明是选择而非自然结果**（登记在归因行文案里）。
- **超时**：**读面无限期**（§1.5）；`open_bi` 会被对端并发上限阻塞（实测第 101 条阻塞）⇒
  客户端**额度耗尽 = 快速失败**（`StreamErr::Busy`，**不等 5s 超时**——设计门 5-3），
  实现 = 岛内先查本连接在册流数（自记账）再 `open_bi`。
- **`ConnErr::Refused` 的消费点必须逐点迁移（设计门 3-1，原稿只点了一处）**：
  `ConnErr::Refused` 是栈 B 的**状态启发式**（`wgcore/mod.rs:989-998`：SynSent→Closed 且非本地 abort），
  它撑起多条用户可见终态；换轨后「服务不存在」不再以 RST 出现 ⇒ 迁移表：

  | 消费点 | 今天 | M3 映射 | 后果（必须保真） |
  |---|---|---|---|
  | `facade/bridge_host.rs:712-717`（**App 核测速桥**） | refused 类**不回帧**、直接关 ⇒ 客户端零字节 EOF ⇒ not_supported | `0x22 ⇒ StreamErr::NotSupported ⇒ ConnErr::Refused` | **不变**（App 侧仍得 not_supported，不是 link_down） |
  | `facade/bridge_host.rs:860-866`（`is_refused_like`） | 同上 | 同上（谓词不改） | 不变 |
  | `facade/service_exec.rs:575-582` | `Refused → ErrorKind::ConnectionRefused` | 同上 | 不变 |
  | `facade/tun_exec.rs:361-367`（`conn_err_to_io`） | 同上 | 同上 + **新 `StreamErr::{Busy,Unbound,BadTag}` ⇒ `ErrorKind::Other`（新分支）** | 新增分支须有用例 |
  | `daemon/mod.rs:319`（CLI dial 分类） | refused ⇒ 可行动文案 | 同上（保 D1 下不变） | 不变 |
  | `daemon/mod.rs:393-398`（host 可达判定） | refused ⇒ NotSupported | 同上 | 不变 |
  | `daemon/carriers/speedrun.rs:388`（CLI 测速哨兵） | `Refused ⇒ NotSupported` | `0x22 ⇒ NotSupported`（**语义等价改写**） | 不变（S3 判据） |
  | `wgcore/mod.rs:105-106` 的 Display | `连接被拒（对端 RST）` | **去 RST 化**（换轨后该文案会说谎） | 文案登记（非判据行） |

- **EOF / 复位 的同形性必须显式收口（设计门 3-2，原稿缺）**：今天 **RST 与 FIN 在 API 上不可区分**
  （读结算把两者都归 `Err(ConnErr::Closed)`，`wgcore/mod.rs:1000-1017`），而消费侧把 `Closed` 当 **EOF**
  用（`tun_exec.rs:229`、`daemon/mod.rs:81`、`speedtest.rs:296`、`files.rs:222-224`、`main.rs:1397/1622`）。
  ⇒ **映射规则（定稿）**：**错误码白名单（0x21–0x27）⇒ typed `StreamErr`（错误）；其余（含 `0x00`/未知/对端
  FIN）⇒ `ConnErr::Closed`（= 今天的 EOF 语义）**。这样「半途复位 = 静默截断」的既有行为**逐字保留**，
  且新增的拒绝码能被消费点识别。**登记为 §5.3-A9 + 判据面变化**。
- **流级拒绝不得注入会话恢复阶梯（设计门 3-3，原稿缺）**：`healing_dial` 的语义是「**首试任何 Err ⇒ 进 R2 阶梯**」
  （唯一豁免 = portfwd 裸拨，`tun_exec.rs:326-329` 的 D11）。换轨后 `StreamErr::{NotSupported,Busy,Unbound,BadTag}`
  **都不是连接故障** ⇒ **阶梯豁免集 = {NotSupported, Busy, Unbound, BadTag}**（照 D11 先例），
  只有 `{Closed, Timeout, EngineGone}` 触发恢复。**S3 完成判据含「服务级拒绝不触发恢复」单测。**

### 1.7 流并发上限与配额

- **实测**：quinn 默认 `max_concurrent_bidi_streams = 100`，第 **101** 条 `open_bi()` **阻塞**（非报错）。
- **quinn 默认值的准确账（设计门 F5 订正）**（`quinn-proto-0.11.19/src/config/transport.rs:366-396`）：
  `max_concurrent_bidi_streams = 100`、**`max_concurrent_uni_streams = 100`（同样吃接收窗）**、
  `stream_receive_window = 1_250_000 B`（= **1.19 MiB**，非 1.25）、`receive_window = VarInt::MAX`（连接级不设限）、
  **`send_window = 8 × RWND = 10_000_000 B`（连接级，多流共享）**。
  ⇒ 最坏接收窗口 = **200 流 × 1.19 MiB ≈ 238 MB/连接**（quinn 自身文档警告：`transport.rs:66-67`）。
- **设计取值（显式设置；写入 `exit/transport.rs`，两端共用；实施期标定）**：
  `max_concurrent_bidi_streams = 64`、`max_concurrent_uni_streams = 0`（本期限定反向开流；
  **⚠ 落地顺序（设计门 N13）**：今天 QUIC 档的巡检判活 `client::probe` 走的是 `open_uni()` + reset
  （`client/mod.rs:301-333`），`uni=0` 与「`Cmd::Probe` 换真回显」**必须同切片落**（S1 内完成），
  否则中间切片上 QUIC 档巡检定音失效、C8 `判据=quic` 语义悬空；过渡期一律 `uni=1`）、
  `stream_receive_window = 256 KiB`、**`send_window = 2 MiB`（连接级，多流共享 ⇒ 吞吐影响须实测，见 §7）**。
- **入口（intake）配额 = 全局信号量（不是每连接）**（设计门 2-4 订正）：今天 `MAX_CONNS=16`（files）
  与 `MAX_CONNS=12`（speedtest）都是**跨 accept 副本共享的全局闸**（`files_server.rs:958-972` 的
  `ConnReservation`、`speedtest_server.rs:132-142` 的 `admit`）。若 intake 改成每连接，全局上界直接 ×32。
  ⇒ **intake = 每 tag 一个进程级信号量**，容量 = **服务在册上限 + K**（K=4，**留给应用层 busy 路径**；
  见下条），probe **不进队列**（回显任务直连，但仍计入每连接流额度）。
- **`term` 的容量依据（设计门 2-4 订正）**：term **今天没有任何连接级在册闸**——只有
  **会话**上限 `DEFAULT_MAX_SESSIONS=16`（`term/session.rs:21`，`HOMEWAY_TERM_MAX_SESSIONS`）。
  ⇒ §8.2-16 的 `term: 36` **没有代码依据**，必须改写为**新引入的连接级上限**（今天无上限）
  + 理由（防「一条设备开满 bidi 流」把 term 线程数打成无界）+ **登记为行为变化**；
  取值 = `max_sessions(16) + K(4) = 20`（与既有会话面同量级；实施期标定）。
- **不许用 intake 取代应用层 busy 语义（设计门 2-5，关键）**：files 的超限路径是**应用层**
  （先发 greeting、吞一条请求、再回 `{"code":"server_busy"}`，`files_server.rs:266-271/291-330`），
  speedtest 回 `{"error":"busy"}` 报表（`speedtest_server.rs:245`）。若第 17 条流在 intake 就被 `reset(0x23)`，
  这两条路径在 STREAM 面**不可达**（且 5 条 busy 用例零覆盖）。⇒ **intake 容量 = MAX_CONNS + K**，
  保证「服务在册闸先于 intake 满」触发 ⇒ 应用层 busy 文案逐字节保留；intake 满只在**极端洪泛**
  （>MAX_CONNS+K 并发）时出现。
- **出口线程/内存有界性**：每流 1 泵任务（异步，**不新增线程**——见 §2.2-B′）+ 服务自身线程
  （与今天 UDS 形态同）；泵的 socketpair 缓冲**显式 `SO_SNDBUF/SO_RCVBUF = 64 KiB`**（两端、两方向），
  纳入内存账（§7）。**残余（登记）**：恶意已认证设备仍可开满 64 流（今天 UDS 同样可开满 16/12）。

---

## 2. 出口分发与 UDS 退役（最小侵入形态 + 「逐字节不变」的钉法）

### 2.1 分发骨架（出口 QUIC 面）

```
accept_bi() ──► 每流一个 task（**并发**，不与 accept 串行——设计门 1-2）
                │  读 1B tag（预算 TAG_READ_BUDGET=5s；到点 ⇒ reset(0x27)，**reset 而非 drop**：
                │  quinn 的未 accept/未 reset 流不归还并发额度）
                ├─ 未绑定连接 ⇒ reset(0x24) + 行 + 计数
                ├─ tag ∉ {1..5} ⇒ reset(0x21) + 行 + 计数
                ├─ 服务未启用 ⇒ reset(0x22) + 行 + 计数
                ├─ 入口队列满 ⇒ reset(0x23) + 行 + 计数
                └─ 入队（per-tag 全局 intake）──► 服务受理循环：serve_conn(从队列取的 UnixStream)
```
- **probe（5）**：不进队列（不占服务资源），由回显任务直接承担；不记受理行，只计快照计数。
- **dial（4）**：本期限定——解析 tag + 6B 目标后即 `reset(0x22)` + 行；M4 换轨时改真拨号
  （协议面一次定死，M4 不再改帧）。

### 2.2 服务入口的连接形态（**方案改判：B′ 为推荐**；设计门 1-1 的直接结果）

> **原稿（方案 A：把三处服务入口泛型化到 `SvcStream`）经设计门逐条回码后判定不可按现稿实现**，
> 理由（全部已复核，见 §14 的 1-1 处置）：①**Cargo 环**——`homeway-core` 已依赖 `homeway-quic`
> （`crates/homeway-core/Cargo.toml:32`），而 `homeway-quic` 明令**不得**反向依赖 core
> （`Cargo.toml:8-9`）⇒ core 侧 trait + quic 侧 `impl` 编译期不可能；trait 反过来放 quic 则 core 反引
> `homeway_quic::…`，且 `impl SvcStream for UnixStream` 受孤儿规则只能写在 quic（破分层纪律）。
> ②**能力集不足**：三服务真正需要的 5 项原稿全缺——`try_clone`（`files_server.rs:335`、`term/service.rs:1161`、
> `speedtest_server.rs:257`）、`for<'a> &'a S: Read+Write`（`speedtest_server.rs:492-512` 的 `DeadlineIo{sock:&UnixStream}`）、
> **per-syscall 期限语义**（`arm_io` 每次 syscall 重算，`speedtest_server.rs:477-486`）、`shutdown_both`
> （`speedtest_server.rs:156` 的 `close_all`、`term/service.rs:3368-3373` 的 `drop_stream`）、
> **可写就绪等待**（`term/wire.rs:92-129` 的断尾/零进展判定）；而原稿给 `close_write()` 在三服务**零调用**（过度设计）。
> ③**「签名层面」不成立**：`term/wire.rs:58-238` 的 `FrameIo` 是 **poll(2)+fd 的 I/O 内核**（非编解码）⇒ 泛型化必重写。
> ④**「既有测试零改」证伪**：`files_server.rs:1662`/`:1682` 把 handler 传成 `|_| {}` ⇒ 泛型化后 `S` 不可推断（E0282 硬红）。

**推荐方案 B′（socketpair 适配器 + 异步泵）**：

```
QUIC 流 ──(tag 分发)──► ExitStreamAdapter（异步侧）
                          ├─ UnixStream::pair()：一端交服务受理，另一端由异步泵对接 QUIC 两侧半边
                          └─ 泵 = 两个方向的 copy + 半关语义 + 计数
服务受理 = per-tag ServiceIntake（两源合一：UDS 监听器[WG 服务腿/调试] + QUIC stream intake 队列）
```

- **「零改」的准确口径（设计门 N1 收窄）**：**帧层 / per-syscall 期限 / `poll(2)` 语义 / `try_clone` /
  `shutdown_both` / busy 路径 = 零改**；**监听循环改**（见下）——不再宣称「100% 零改」。
- **改动面（逐处写准）**：
  · `FilesServer::serve_stoppable`（`files_server.rs:255-264`，~10 行）、
    `SpeedtestServer::serve_stoppable`（`speedtest_server.rs:193-210`，~10 行）：形参
    `UnixListener → ServiceIntake` + 内部 `accept()` 转发；
  · **`TermService::serve_stoppable` 是 38 行自建 accept 循环**（`term/service.rs:832-871`：
    `libc::poll(fd, POLLIN, 200)` + `WouldBlock` 分支 + `guard_thread`）⇒ **必须重写为「poll intake 的
    就绪 fd」**，且须保住既有性质「**poll 到达即 accept——无 200ms 空闲延迟**」
    （`term/service.rs:840-843` 注释 + `:4168` 测试注释钉着它）；
  · **两处既有测试会因形参变更编译红**（`term/service.rs:4160-4177` 的 `serve_stoppable(ln, …)`、
    `speedtest_server.rs:1048-1060` 同款）⇒ **`ServiceIntake::from_listener(UnixListener)`**（无 QUIC 源）
    构造面 ⇒ 这两处**零改**（设计门 N2）。
- **`ServiceIntake` 契约五件（设计门 N1–N4，必须落纸，S2 的工作量集中在这里）**：
  ①`from_listener(UnixListener)`（WG-only/调试形态，零改测试）；②`accept() -> io::Result<UnixStream>`
  （两源）；③**唤醒面**——intake 内建 pipe/eventfd，QUIC 泵入队时 `write(1B)`，服务侧 poll 该 fd
  （**否则每条 QUIC 服务流最多 +200ms**：`files_server.rs:937-938` 把 `WouldBlock` 归 `Retry` ⇒
  `thread::sleep(200ms)`；term 的 P7 测试正是为消掉这 200ms 而写）；④**两源顺序/公平**——
  **UDS 源优先，QUIC 源连取上限 N（=8）后回让**（否则 D1 下 QUIC 洪泛可饿死 WG 服务腿，而该腿的消费者
  正是 CLI 远程面 DC14/DC15/CA1）；⑤就绪 fd 的登记（term 重写后的 poll 目标）。
- **泵（异步任务）**：`tokio::net::UnixStream::from_std` + 两个方向的手写 copy
  （**半关语义照 `tokio::io::copy_bidirectional`**）：服务侧 `shutdown(WRITE)` ⇒ 对端 QUIC `finish()`；
  QUIC 侧 FIN ⇒ socketpair `shutdown(WRITE)`。
  **`reset(code)` 的处置已按设计门 N12 订正**：AF_UNIX **没有** TCP 的「`SO_LINGER=0` ⇒ close 即 RST」
  语义（AF_UNIX 上对端观测到 EOF 还是 `ECONNRESET` 取决于关闭瞬间两队列是否非空，不受 LINGER 位控制），
  且**今天的出口侧本来就是普通 close**（`intercept/mod.rs:2734-2737` 注释自陈「『到期 Close{linger_rst}
  RST』Rust 侧**从未接线**——close() 一直走 FIN teardown」）⇒ **`reset(code)` = 关 socketpair（普通 close，
  与今天同款）**；**错误码只留在出口行/计数里，不跨 socketpair**。
  **「不新增线程」的准确说法（设计门 N1）**：泵与每流 task 走岛 runtime（不新增线程），但**服务自身的
  handler 线程不变**（`serve_stoppable_accepts` 的 handler 今天就在专用线程里跑）。
- **背压**：socketpair 内核缓冲（**显式 64 KiB/方向**）+ QUIC 流窗口 = 端到端真背压；
  服务侧的阻塞写/读**语义与今天 UDS 逐字相同**。
- **成本（必须实测并登记）**：每方向一次额外拷贝（files/speedtest 是 bulk 路径；M1 的每包 CPU 门
  不涉服务流，但**服务流吞吐须新增相对门槛**，见 §7）+ 一对 fd/流（64 KiB×2 显式收紧）
  + **term 监听循环重写的回归面**（P7 的「无 200ms 空闲延迟」性质须有用例钉住）。
- **方案 A（泛型化）降为 M5 收口候选**（届时 WG 面已删、`homeway-quic` 与 core 的边界可重划），
  **M3 不做**。**方案 B（泵到服务自身 UDS 文件）不取**：多一层 socket 文件生命周期 + 与
  `listen_local_service` 的「活实例占用」判别冲突。

### 2.3 UDS 豁免分支：**M3 的删除面取决于 D1/D3（设计门 F3 订正）**

> **原稿错误**：§2.3 无条件删 `local_services` 与 `DialTarget::Unix` 分支，同时 §5.2/§8.1 声称
> **D1 下 CLI/daemon 远程服务面不变**——**二者不可兼得**。事实链（已逐条回码）：CLI 远程面走
> **WG 承载 → 出口 WG 面 → intercept → `dst == tunnel_ip(100.64.255.1)` → exempt → LocalServices → UDS**；
> `local_services` 一删，该路径落到保留臂 `DialTarget::Tcp(loopback(port))`（`intercept/mod.rs:1210`），
> 而服务本体在 UDS 上 ⇒ **CLI 远程 term/files/speedtest 直接连不上**（DC14/DC15/CA1 的实采行就是这条路径）。

- **D1（推荐）**：`local_services` 装配与 `DialTarget::Unix` 分支**保留**——其唯一消费者 = **WG 承载的服务腿**
  （App 核 WG 回落 + CLI 远程面）；M3 的出口改动 = **零**（QUIC 档不再经它，是自然结果而非删除）。
  **登记**：「M3 后该分支的唯一消费者 = WG 服务面；随 WG 服务面退役（M5）」。
- **D3**（M3 真删净）：按原清单删——`server/engine.rs:392-396`、`intercept::Config.local_services`
  （`mod.rs:606`）、`route_upstream` 的 Unix 分支（`mod.rs:1196-1200`）、`DialTarget::Unix` 变体（`mod.rs:122`）
  与建流臂（`mod.rs:308`）、`uds_exempt_flow_end_to_end` 用例（`mod.rs:3491`）与 4 处 `local_services:
  HashMap::new()` 装配（`:2941/3155/3344/5060`）；**并接受 DC14/DC15/CA1/CA4/CA5 与 CLI 远程面停用**。
- **两案共同保留**：`dst == tunnel_ip` 的**其余豁免臂**（`DialTarget::Tcp(loopback(port))`）——
  portfwd「出口自己」（M4）与隧道 IP 回环路径仍需要。
- **E5 行（`intercept: 过境拦截就绪（隧道IP %v；豁免=转投本机同端口；TCP 并发上限 %d）`）**：
  **两案下都逐字不变**（UDS 映射的有无不影响该串的事实性）⇒ 登记「M3 零变更」（§8.2-4）。
- **UDS 的余下用途（登记）**：服务本体仍以本机 UDS 为入口（方案 B′ 下 `ServiceIntake` 两源之一）
  ⇒ 「UDS = 服务入口（本机/调试 + WG 服务腿）」，不再是**隧道服务的唯一路径**。

---

## 3. 阶梯重写（本期最重的行为变更）

### 3.1 新语义定稿：**迁移 / 重连 / 世代重建**（承载分档）

QUIC 档**不再有 R1/R2/R3 档位语义**；三动作（**动作判定已按设计门 4-1 改为显式信号**）：

| 动作 | 触发条件（**显式信号，不用「udp_rx 停增」推断路径变更**） | 做什么 | 代价 | 判据 |
|---|---|---|---|---|
| **M 迁移**（`Rebind`） | 快探**复探失败** 且**本机发送面报错**（socket 层 `ENETUNREACH`/`EADDRNOTAVAIL` 等——**M3 新增岛侧信号**，见下）| 换本地 socket，保连接；随后快探确认 | ~1 探活预算 | 已有 N-b 行族（保留）+「动作前置条件收窄」登记 |
| **R 重连** | 快探**复探失败** 且本机发送面**无**错（对端不可达/进程死） | 新 QUIC 连接（同端点、同本地 socket）+ 四帧准入；服务流**按需重开** | 实测握手 + 首流回显 ≈ **0.3s**（§13） | 新增 `quic: 链路重连完成（原因=%s，耗时 %v）` |
| **B 世代重建** | **连续 2 次 R 失败 且 累计失败窗 ≥10s** | 既有 `rebuild_session`（重赛跑/换候选/换 token 端点） | 现有量级（**含赛跑失败 ⇒ 回落 WG 承载**） | 现有 `REBUILD_COOLDOWN` 行族 + 新归因 |

- **M3 新增岛侧信号（承重；定义已按设计门 N5 订正——原稿的 `ClientSock::poll_send` 不存在）**：
  · 事实：本仓 socket 实现是 `AsyncUdpSocket::{create_io_poller, try_send}`
    （`client/relay_sock.rs:200-238`；写就绪在 `SockPoller::poll_send_ready`，`:325`）——
    **`poll_send` 是 quinn ≤0.10 的旧 API 名**，本仓没有。
  · **计数点**：`try_send` 的 **`Err(e) if e.kind() != WouldBlock`**（`WouldBlock` 是 quinn 契约下的
    **正常回执**「发送缓冲满，回去等 poller」——**照字面把 `WouldBlock` 计进去就会把上行拥塞误判成
    「本机发送面错误」，而拥塞恰好也是快探失败的时刻 ⇒ 会优先选 M（Rebind 对死对端无用）**，
    正是设计门 4-1① 要消灭的误判；出口侧同类路径的先例：`exit/socket.rs:197-199` 的腿路径
    「**不**返 WouldBlock」交丢包计数）。
  · **errno 白名单**：`{ENETUNREACH, EHOSTUNREACH, EADDRNOTAVAIL, ENETDOWN, EINVAL}` ⇒ 判 M；
    其余只记行（不进 M/R 判别）。
  · **新鲜度窗口 + 清零**：`SockStats` 的 `Arc` 在 `rebind` 时**刻意跨 socket 共享**
    （`client/mod.rs:149-155`）⇒ 必须给「末次错误时刻」设新鲜度窗（照 `demand::OUTBOUND_FRESH` 先例）
    并在 `rebind` 时清零，否则会用换网前的陈旧错误决定下一步动作。
  · 它同时承担：①M/R 判别；②**QUIC 档的环境噪声位**（补上 M1 S6 为 quic 档关掉的那一路）。
  · **S1 完成判据加负例**：上行拥塞（`WouldBlock` 高频）下 `sock_send_errs` **不增长**
    （注入型 socket 或纯分类函数单测）。
- **动作顺序（定稿，含复探抑制）**：
  1. 快探失败 ⇒ **不动连接**，**本拍内用加倍预算复探一次**（700ms → 1.4s）；
  2. 复探**成功** ⇒ 判「路径抖动」，记行 + 计数，**不进任何动作**（连续 3 次抖动 ⇒ 升格为失败，防 fail-silent）；
  3. 复探**失败** ⇒ 按上表选 M 或 R；**复探两次之后必有动作**（**修掉原稿「udp_rx 仍在增长 ⇒ 无限不动」的活性洞**）；
  4. 动作后快探确认：成 ⇒ 归零 + 行；败 ⇒ 另一动作（M↔R）；两动作都败 ⇒ 计入 B 的门（连续 2 + 窗 ≥10s）。
- **与 `migration_unconfirmed` 的关系（定稿）**：该位由「慢变量告警」升为**动作前置条件**——
  窗口从**巡检拍（60s）**收窄到 **≤1 个快探预算（≤1s）**：`Rebind` 后一个预算内无回显 ⇒ 置位 ⇒
  允许走 R。字段与行文不变，**语义登记**（§8.2-15）。

### 3.2 快探（fast probe）定稿

- **载体**：`STREAM[probe]`（tag=5）；**持久流**（连接建立后常驻，每次探活 = 写 1B + 等 1B 回显，
  不每拍开新流——省 RTT/流槽）。
- **节拍**（三段，写入常量 + 可配）：
  1. **在用档**（`demand > 0`，沿用 `demand.rs` 需求位）：`PROBE_FAST_BUDGET = 700ms`，
     **背靠背**（拍间 ≤300ms，由 runtime 拍内务驱动）；复探 1.4s（§3.1-2）；
  2. **待机档**（attached 无需求）：`PATROL_INTERVAL = 60s`（**不变**，保电池/CPU；与 C15' 同拍）；
  3. **挂起唤醒**（`gap > 2×PATROL_INTERVAL`）：沿用现有主动恢复行，动作改为「M→R」。
- **假阳性抑制（四重，全部可测）**：①复探（加倍预算）；②**本机发送面信号**决定 M/R（不再用 `udp_rx` 推断）；
  ③**抖动连续 3 次升格**（修 fail-silent）；④动作单飞（沿用 `quic_heal` 的 `swap` 单飞位）。
- **明确排除的判据**：ICMP 端口不可达（本仓 UDP socket 为**非 connect 的抽象 socket**，跨平台无保证；
  出口端口退让会让旧端口 ICMP 与「重启」不可区分）；`udp_rx` 停增（既可能是路径变更也可能是对端死亡，
  不可判定——设计门 4-1）。

### 3.3 「出口重启自愈 ≤3.5s」的达成路径（**计时起点已定义**；可 falsify）

**两个独立量（设计门 N6 订正：原稿把两套钟混在一张表里）**：

| 量 | 定义（计时起点 → 终点） | 值 | 来源 |
|---|---|---|---|
| **`T_detect`** | 出口最后一次可达 → 快探**复探失败**定音 | **0.7s（首探）+ 1.4s（复探）= ≤2.1s**（背靠背档首探实为 706ms） | 实测：**预算 700ms/间隔 0 ⇒ 706ms**（§13-T2）；预算 300ms/间隔 100ms ⇒ 393ms；间隔 200ms ⇒ 318ms |
| **`T_recv`**（**门槛量**） | **出口侧恢复监听**（以既有判据行 **E1 `serve 就绪`** 的时刻为锚）→ 客户端**首个回显成功** | 动作（M/R）+ 准入 + 首回显 = **≤0.3s + 17ms + ε** | 实测：回环 `handshake_ms=2.7 / first_stream_echo_ms=0.2`（§13-T3）；`heal` 台「重启 → 重连完成」**268ms**（§13-T4） |
| 对照：旧路径 | 出口不可达 → 客户端恢复 | **30–40s** | 实测 **40.028s**（§13-T1，两次 40.029/40.021）；真机 32s/41s |

- **门槛判据（写死，可 falsify）**：**`T_recv ≤ 3.5s`**。
  · **起点 = E1 `serve 就绪` 行的时刻**（该行本来就被 `tools/local-*.sh`/`matrix.sh` wait ⇒ **起点有日志锚、
    与 `T_total` 同源**；**不得**用 harness 拉起进程的时刻当起点——那会系统性漏算启动时间）；
  · **出口进程启动不计入**：本机实测**进程内启动到监听 = 0ms**（`SERVER_START` 与 `SERVER_READY` 同毫秒，
    §13-T3；**进程 exec 时间未单独实测 ⇒ 登记待测**，量级 ~10–50ms）；
  · **真机同时记录 `T_total`**（出口不可达 → 客户端恢复）并登记「含出口启动/人工启停 ⇒ 大于 3.5s 属预期」。
- **最坏动作序列的账（订正）**：`T_detect = 2.1s` **与** `T_recv ≤ 0.32s` 是**两段独立量**——
  当出口停机时长 < `T_detect` 时，客户端会在出口回来之前先发一次 R（必然失败），下一次动作要再等一个
  探活拍 ⇒ **`T_recv` 由「重连尝试与出口就绪的相位」决定**（最坏 = 一个快探拍 ≈0.7s + 动作 0.3s ≈ 1.0s
  ⇒ 仍在 3.5s 内）；**该相位关系须在 e2e 里用「出口停机 → 立刻重启」与「停机 5s → 重启」两种相位各测一次**。
- **验证方式（实现期，本地故障注入）**：`tools/` 新增 quic 恢复 e2e（沿用 R2 批手法 + M0 本地私有实例纪律）：
  起本地私有出口 → App 核形态（或 `quic-island-e2e.sh` 形态）建连 → **kill -9 出口** → **在 T_restart 重启**
  → 记录三个时刻（首探失败 / 复探失败 / 首回显成功）→ 断言 `T_recv ≤ 3.5s`（**上界断言，不设下界**——flake 口径
  照 M0 §9.2②）。**新增两条负向用例（设计门 4-1/4-1(5)）**：①**瞬时黑洞 1.5s**（用 M2 自写 UDP 中继手法
  制造丢窗）⇒ 断言**不产生 R/B**（只记抖动行）；②**出口服务面卡死**（回显任务挂起但连接活）⇒
  断言**必有动作**（复探两次后 R，不得无限静默）。

### 3.4 C11 判据行族（处置：**保留 WG 档 / QUIC 档新增行族**；关系写清）

- **C11 族（`RECOVER R1/R2/R3`）不退役**：WG 档在 M5 前仍走旧阶梯 ⇒ 该族对 WG 档**逐字有效**；
  **登记**：①「QUIC 档不再产生 C11 族行」；②**关系 = 替代而非并存**（QUIC 档的恢复时间线由新行族
  `C18` 承担，同一期同一承载不会两族并存）——否则排障读者会以为 QUIC 档恢复面无观测。
- QUIC 档新增行族（additive；**命名改 C18/C19**——原稿 C16'/C17' 与既有 `C16`/`C17`（`INTEROP-CRITERIA.md:62/63`）
  及既有 prime 约定（`C2'`/`C4'`/`C5'`/`C6'`/`C15'` = 「QUIC 档同义行」）双重冲突，设计门 4-3）：
  `quic: 链路快探失败（连续 %d，原因=%s）` / `quic: 链路探活抖动（%s，已复探）` /
  `quic: 链路重连中（原因=%s，第 %d 次）` / `quic: 链路重连完成（原因=%s，耗时 %v）` /
  `quic: 链路重连失败（原因=%s，第 %d 次）—— 交世代重建` / `quic: 世代重建（原因=%s；连续重连失败 %d）`；
  节流照仓内口径（首 3 + 每 100）。
- **tier `connection-lifecycle.md` 修订稿要点（草案，交付 = 用户触点）**：恢复阶梯节从
  「R1 重握手 / R2 换源 / R3 重赛跑」改为「**承载分档**：WG 档 = 旧三档（遗留，M5 退役）；
  QUIC 档 = 快探（700ms/背靠背 + 60s 待机）→ 复探 → 迁移/重连 → 世代重建（连续 2 + 窗 ≥10s）；
  门槛 = `T_recv ≤ 3.5s`（计时起点 = 出口重新监听）」；时间窗真源标注新增 `PROBE_FAST_BUDGET`。

### 3.5 与巡检/需求门控的接口（不变面）

- 需求位（`demand.rs` 的 `patrol_demand`）**语义不变**，只喂「快探节拍档位选择」。
- M1 S6 的**分档纪律**（`tun_exec.rs:2543-2548`：QUIC 档不读 WG 腿的 `last_local_send_err`）**必须保住**；
  QUIC 档的噪声位改由 §3.1 的**岛侧 `sock_send_errs`** 承担（等价替换，非新开面）。
- 岛的 `Cmd::Probe` 换成真回显（C8 语义更正 = §8.2-8）。

---

## 4. 准入失败归因回传客户端（M2 真机发现①）——**本期处置，零协议扩展**

**事实**（M2 真机）：出口能分 `hr-reg4 MAC 不符` / `revoked` / `table-full`；核侧三种一律
`登记失败（连接在登记窗内关闭）`，随后 WG 回落把世代撑成 `readyBy=wg` ⇒ 黑洞期设备侧不可见。

**方案**：用**既有的 CONNECTION_CLOSE 通道**（不发新帧、不改状态机）。

- **出口拒绝路径的 close 点必须枚举全（设计门 F4 订正）**——原稿只写了两处，实际有 **4 + 3** 处：

  | 落点 | 现文本 | 阶段 | M3 处理 |
  |---|---|---|---|
  | `exit/conn.rs:507`（`reject()`） | `close(0, b"registration rejected")` | **准入窗内** | 码细化（下表） |
  | `exit/conn.rs:525`（`time_out()`） | `close(0, b"admission timeout")` | **准入窗内** | `0x14` |
  | `exit/conn.rs:464` | `close(0, b"inbound queue full")` | 准入窗内（入境队列满） | `0x12`（资源桶）+ 归因行 |
  | `exit/bridge.rs:367` | `replaced by newer registration` | **准入后**（同设备新连接替换） | **不入准入映射**（会话级） |
  | `exit/bridge.rs:458` | `device removed`（吊销/移除） | **准入后** | **不入准入映射**（会话级） |
  | `driver.rs:568` | `replaced by newer race`（岛侧赛跑胜者替换） | 连接层 | 不入（非出口准入面） |

- **准入关闭码表（粗粒度两桶，避免给未认证对端更多 Oracle）**：

  | 码 | 桶 | 出口落点 | 稳定短语 |
  |---|---|---|---|
  | `0x11` | **凭证不被接受** | `reject()` 的 `MacMismatch` + 引擎 `EngineRejected(revoked/no-token)` | `凭证不被接受` |
  | `0x12` | **资源暂不可用** | `EngineRejected(table-full/表压)` + 入境队列满（`:464`） | `资源暂不可用（稍后重试）` |
  | `0x13` | 准入数据非法 | `reject()` 的帧格式/版本族 | `准入数据非法` |
  | `0x14` | 准入超时 | `time_out()` 两族 | `准入超时` |

- **客户端映射（必须限定在准入窗内——设计门 F4）**：只有**未绑定（准入窗内）**的
  `ConnectionError::ApplicationClosed{code,reason}` 才映射成 `IslandErr::AdmissionRejected{code}`；
  **准入后**的同类关闭走独立分支 `IslandErr::SessionClosed{reason}`（对应 `replaced`/`device removed`），
  否则「会话中被吊销/被替换」会被误报成「准入被拒」。
- 岛快照新增 `admit_reject_code` / `admit_reject_text`；打一行
  **`quic: 准入回执（code=0x%02x %s）——本世代回落 WG 承载`**（**行名前缀刻意与出口的
  `quic: 准入被拒（dev=…` 区分**——设计门 P3：同前缀会让按前缀 grep 的脚本混淆）；
  世代状态面把该串带进 `readyBy=wg` 的归因（App 可见「为什么走了 WG」）。
- **安全面（写清）**：两桶**不引入新 Oracle**（其可分辨性 ≤ 出口既有归因行对未认证对端给的信息）；
  **不发新帧、不改 admission 状态机、不放松任何闸**；出口侧详细归因行（E-q 族）**逐字不变**。
- **归属：本期**（切片 S5，半天量级）。理由（**订正**：设计门 3-4 指出原稿理由③引用不实——
  M2 §15-1 讲的是**出口** `serve status --json` 的 quic 段，与客户端归因不是同一面，已删）：
  ①路线文件把发现①标为「归 M2 设计面/M3」且它是真机最有价值发现；②零协议扩展 ⇒ 不重开 M2 安全面；
  ③另有一条**真·M2 交下项**（出口 quic 快照外部只读面）已补进 §11-S2 判据（不混在本次归因里）。

---

## 5. stackb 退役清单 + 删除后的遗留假设扫查

### 5.1 消费点全量（**12 处 / 7 文件**；路线文件记 5 处的补全版）

| # | 位置 | 服务 | 承载归属 | M3 处置 |
|---|---|---|---|---|
| 1 | `facade/tun_exec.rs:310-322` `session_connect` | 隧道桥（App 核服务） | App 核 | 改 STREAM（QUIC 档）/ 保 WG（WG 档） |
| 2 | `facade/tun_exec.rs:330-341` `session_connect_target` | portfwd 裸拨 | App 核 | **M4**（本期限定协议，不换轨） |
| 3-4 | `facade/tun_exec.rs:435/450`（`healing_dial` 内联） | 隧道域拨号 | App 核 | 同上（1） |
| 5 | `files.rs:207` | files 每命令一条流 | App 核 | 改 STREAM（tag=1） |
| 6 | `facade/service_exec.rs:577` | term/speedtest 服务会话桥 | App 核 | 改 STREAM（tag=2/3） |
| 7-8 | `session/mod.rs:645/664`（`healing_dial` 主体，经 `:628/634` 公开） | 服务域拨号 | **App 核 + CLI/daemon 共用** | App 核侧改 STREAM；**函数本体保留**（§5.2） |
| 9 | `daemon/mod.rs:295/309`（carriers 拨号缝） | forward/socks 承载 | **CLI** | **M3 不换轨**（WG-only，§5.2） |
| 10 | `daemon/mod.rs:393` | host 会话服务流（term 远程等） | **CLI** | 同上 |
| 11 | `homeway-cli/src/main.rs:1583` | remote attach | **CLI** | 同上 |
| 12 | `daemon/carriers/dnsq.rs`（拨 5300） | socks 域名远程解析（CA5） | **CLI** | 同上 |
| 13 | `homeway-cli/src/main.rs:1413`（`udp_*`） | dnstest | **CLI** | 同上 |
| 14 | `wgcore/mod.rs:1260-1281`（`path_probe`）/ `:1206`（`connect_deadline`） | 巡检判活（WG 档） | App 核 WG 腿 | **保留**（WG 档 C8 `判据=wg` 仍用） |

### 5.2 **范围订正（必须上报裁决）**：M3 删不净 stackb（**且 UDS 分支同命**）

- **事实链**：`stackb` 服务的是 **WG 承载**的客户端内部 socket 面；而 **CLI/daemon 的 host 会话是 WG-only**
  （M2 遗留项明记 daemon 过滤 QUIC 端点 ⇒ QUIC host 会话不在任何期范围），其远程
  term/files/forward/socks/5300 解析腿**全部**经 `Session`+stackb。⇒ M3 删净 stackb ⇒
  **CLI 远程服务面（DC14/DC15/CA1/CA4/CA5，恰是 M3 判据清单点名「语义对照」的行）全红**；
  且 WG 档的巡检判活、files/term/speedtest 一并失能。
  同一逻辑适用于 **intercept 的 UDS 豁免分支**（§2.3 的 F3）：其消费者 = WG 服务腿 ⇒ **同生共死**。
- **建议裁定 D1（推荐）**：M3 的退役目标改为
  **「客户端 QUIC 档服务流全走 STREAM + App 核侧消费点清零」**；`stackb.rs` 本体、
  `Session::healing_dial_*`、`dnsq` 腿、`udp_*`、`path_probe`、**intercept 的 `local_services`/`DialTarget::Unix`**
  **随 M5** 与 `wgcore` 同批删除（M5 范围原文即「删 `wgcore`（除 QUIC 岛共用类型）」+「intercept 收窄」）。
  M3 交付物 = 「stackb/UDS 分支的唯一余下消费者 = WG 承载」的**可核验事实**（脚本断言 + 消费点清单 +
  净删/改行数登记）。
- **备选 D3（不推荐）**：M3 真删净 ⇒ 接受 **WG 档服务面停用**（`_wg` 回退退化为「仅 L3 数据面」）。
  必须同批登记（**代价清单**）：①C11 族（WG 阶梯）停用；②C8 `判据=wg` 停用；③DC14/DC15/CA1/CA4/CA5 停用；
  ④`tools/quic-wg-e2e.sh` 三断言改写；⑤「A/B 一键回退」门槛从「功能等价」降为「连通性」；
  ⑥`intercept` 的 5 个 UDS 用例 + `local_services` 4 处装配删除；⑦E14/E17 的行文改写提前到 M3（D1 下
  这两行只描述「服务入口 + 本机 UDS」，**行文仍含「经拦截层转投」需改**——见 §8.2-1/2 的 D1 形态）。
- **备选 D2（越界）**：M3 同时把 CLI host 会话搬 QUIC（≈ M4+M5 工作量）——不做。

### 5.3 删除后的**遗留假设扫查**（设计门重点 5；A1–A13）

| # | 靠 stackb 兜住的假设 | 现状证据 | M3/后续处置 | 承接期 |
|---|---|---|---|---|
| **A1** | **环回不经隧道**（`stackb.rs:236-239` 出站拒 127/8） | 拒绝属**客户端**约束（栈 B 无 lo）；**「出口拨自己的回环」是 portfwd 的合法功能**——环回/未指定目标被映射成 `PfDialTarget::ExitPort`，客户端改拨 `SERVER_TUNNEL_IP:port`，出口经豁免臂拨自己的 `127.0.0.1`（`portfwd.rs:152-170` 注释：「Go 经出口过境重拨到出口的 127.0.0.1（能通），本仓栈显式拒环回——不映射就是『监听中但连不上』」） | **订正**：①客户端拨号缝**继承**「不得把 127/8 当隧道内目标」；②**不得**在出口侧加该拒绝（会打死合法目标）；③`dial` tag 的**「出口本机」语义位**（或保留 `SERVER_TUNNEL_IP` 约定）**M4 设计门必列**（= 下文的 A1③） | M3（①②）+ M4（③） |
| **A2** | 内层 MTU 1280 的分段面 | `stackb::MTU=1280`；App TUN 仍 1280 | 服务流改由 QUIC（1400/1362）承担；App TUN 1280 不变 | —（已核无问题） |
| **A3** | 每连接 TCP 缓冲（**`TCP_BUF` = 1 MiB/方向 ⇒ 2 MiB/连接**，`stackb.rs:33` + `:240-241` 分配 rx+tx） | `stackb.rs:33`（**行号订正**：原稿引 `:29`） | 等价面 = `stream_receive_window`（**显式 256 KiB**）+ 待发队列 64 KiB + socketpair 64 KiB；**默认值（1.19 MiB×200 流）不可用** | M3 |
| **A4** | 「栈内 TCP 会话终结 = 丢会话」（R1 语义基础） | `wgcore` 的 `Shutdown/Close` | QUIC 重连后**在途流全断**（实测旧连接上的流不随新连接复活）；term 会话在出口持存（`engine.rs:576-585`）⇒ 重连后 re-attach 恢复 | M3 |
| **A5** | DNS 远程解析腿（5300，CLI socks 域名面 CA5） | `dnsq.rs:17` | 随 stackb（D1） | M5 |
| **A6** | `path_probe` 的「出口拦截层可用」强判据（拨 `tunnel_ip:1` 拿 RST） | `wgcore/mod.rs:1260-1281`（**行号订正**） | QUIC 档换 `STREAM[probe]` 回显（更强：两向端到端）；WG 档保留 | M3 |
| **A7** | UDP socket 面（`udp_open/send/recv`） | `main.rs:1413` | CLI-only（dnstest） | M5 |
| **A8** | **地址接受集（安全面；原稿完全未覆盖）** | `stackb.rs:237` 是**唯一**的地址拒绝；`wgcore/mod.rs:676-679`（UDP 同款）亦然 ⇒ **RFC1918 / 169.254.169.254（云 metadata）/ 组播 / 广播 / 未指定 / 100.64-10 / 198.18-15 全放行**；候选侧另有卫兵（`probe.rs` 的 `probe_addr_acceptable`：拒 private/回环/链路本地/fake-IP/CGNAT —— `7097fe1` 刚归一） | **`dial`/tag 缝的「地址接受集判定表」= M4 设计门必列**（逐类给 accept/reject + 理由 + 用例）；M3 定协议时在 §1.1 附注登记该缺口 | M3 登记 / M4 定表 |
| **A9** | **EOF 与复位同形**（消费侧把 `Closed` 当 EOF） | `wgcore/mod.rs:1000-1017` 把 FIN/RST 同归 `Closed`；消费点 `tun_exec.rs:229`、`daemon/mod.rs:81`、`speedtest.rs:296`、`files.rs:222-224`、`main.rs:1397/1622` | **映射规则**（§1.6）：错误码白名单 ⇒ typed error；其余（含 0x00/未知/FIN）⇒ `ConnErr::Closed`（保今天语义） | M3 |
| **A10** | **会话收工不发 FIN/RST** ⇒ 出口靠自身超时清半开 | `wgcore/mod.rs:1588-1642`（收工只置 stop + 关唤醒 fd，**不对栈内连接做 TCP 收口**） | QUIC 有显式 CONNECTION_CLOSE ⇒ 出口**立即**拆链：E11（关闭行）/连接计数/目标侧 FIN-RST 时点与顺序都会变 ⇒ **登记 + e2e 对照「收工后出口侧会话与关闭行的时点」** | M3 |
| **A11** | 吞吐整形 / ACK 时钟随栈 B 退场 | 栈 B 独占的 ACK 密集时钟与消融 env（`wgcore/mod.rs:64-92`）+ 两个 smoltcp 行为钉子测试（`stackb.rs:418-509`）；对位 = QUIC 的 `ack_eliciting_threshold=16`/`max_ack_delay=5ms`（`exit/transport.rs:37-43`） | §7 登记「ACK 整形换轨」+ 服务流吞吐**相对门槛**；旧 `PERF-AB` 的 ACK 密度读数标注**跨承载不可比** | M3 |
| **A12** | `DialError::TooManyConns` 死变体 ⇒ 「并发上限从无到有」是**新增失败态** | `stackb.rs:42-43` 声明但**全仓零构造点**（`connect` 不做上限） | `max_concurrent_bidi_streams=64` 把「第 65 条并发流」从**必成功**变成阻塞/拒绝 ⇒ 客户端额度耗尽必须**快速失败 `StreamErr::Busy`**（§1.6）+ 顺手接线或删除该死变体 | M3 |
| **A14** | **客户端侧「读交接通道」无界**（设计门 2-3 的残余） | `files.rs:213-229`：读者线程紧循环 `client.read(id) → tx.send()` 推入**无界 `std::sync::mpsc`**（今天与 stack B 的 1 MiB rx 缓冲同构）；`Ok(chunk) if !chunk.is_empty()` 分支**无界增长** | **M3 登记**（与今天同形，非回退）：STREAM 接收窗**不构成该通道的上界**；后续切片可加字节上界（消费慢 ⇒ 读者线程停发 `StreamRead`）——列 §11-S3 可后补项 | M3 登记 / 后续 |
| **A13** | 虚拟端口退役的**对外可观测面/文案**未清 | `bridge_host.rs:200-213`（含 `HOMEWAY_TERM_PORT` 可配面）、`files.rs:32`、`speedtest.rs:23`、`daemon/mod.rs:378-383`、`files_op.rs:9`、`facade/term_op.rs:113-116`（**「出口 7724 端口上不是终端服务」**）、`wgcore/mod.rs:105`（**「对端 RST」**）、`daemon/vocab.rs:241-245`（MUST NOT 入词表，**已核无问题**）、判据行 `INTEROP-CRITERIA.md` 的 E1/E10/E11/E13 采样（含 `100.64.255.1:7803 ← <client>:<port>`） | §8.2 逐条登记（含**文案去端口/去 RST** 项）；`tools/local-*.sh`+`matrix.sh` 的 wait 面（E1）同批复核 | M3 |

---

## 6. 地址派生收窄（`tunnel_addr.rs`）

- **QUIC 档收窄**：源校验接受集从 `{tunnel_ip, tun_ip}` → **`{tun_ip}`**（App TUN 地址；落点
  `exit/bridge.rs` 的 `Bound{tunnel_ip,tun_ip}` 与 E-q3 明细文案 `src ∉ {tunnel_ip,tun_ip}`；`src_allowed`
  在 `exit/conn.rs:55-65`，`exit/tests.rs:825` 已按 `tun_ip` 为合法源——**方向与测试语义一致，已核无问题**）。
  QUIC 档 `tunnel_ip` 字段**保留但不再参与源校验**（登记「值域/输入集变化」）。收窄 = 安全面**收紧**。
- **保留**：`derive_tun_ip`（`hw-app`，App 接口地址）与 `SERVER_TUNNEL_IP`（出口常量 IP：DNS 目标 / M4 的
  「出口自己」锚点）。
- **退役（随 M5）**：`derive_tunnel_ip`（`hw-tun`）在 WG 删除后无消费者；`fixtures/vectors/tunnel_addr.json`
  的 `hw-tun` 样本同批退役。**M3 不动 fixtures**（WG 档仍用）。**登记**：M3 记「QUIC 档不再消费
  `derive_tunnel_ip`」，退役动作归 M5。
- **对 App 侧（tier）连锁**：`tunIp` 仍是接口地址（`tunStatusJSON`/`tunConfig` 面不变）；`tunnel_ip`
  不参与源校验不影响 App 可见面 ⇒ **无 tier 侧改动需求**（除 §3.4 的 `connection-lifecycle` 修订稿）。

---

## 7. 预算（体积 / CPU / 内存 / 流窗口；可测性；**含设计门 F2/2-4/M-1 订正**）

| 维度 | M3 增量（**按 D1 重算**） | 判据/方法 | 备注 |
|---|---|---|---|
| 体积 | **净增**（D1 下）：新增 = tag 分发 + `ServiceIntake` + 泵 + intake + 快探 + 归因 + 登记（估 **+0.6…1.2 千行**）；删除 = 仅「QUIC 档不再调用的少量分支」（**估 −0.2 千行**）⇒ `.so` **估 +30…+80 KB**（**实施期实测**） | `tools/build-app-core.sh` 三道门（`[size]`）+ `tools/quic-ab.sh size` | 3.8MB 阈值按设计属 **M5 判**（M2 现 4,685,216 B = 1.233×）；**登记**：M3 净增 X KB，M5 的删码余量被进一步吃紧 ⇒ M5 设计门须先实测删码余量。另：`tools/quic-ab/arms/size/src/lib.rs:21-30` 的 `ProbeWgTouch`（boringtun+smoltcp 保活）在 **D1 下保持**（体积臂 = 双栈期产品形态，不是 M3 客户端核读数——口径须写明） |
| 每包 CPU | **不涉 L3 每包路径**（服务流非每包）——**但设计门 M-1 指出**：出口 QUIC 面与岛**都是 `current_thread`**（`exit/mod.rs` 的 runtime 构造、`driver.rs:149-151`），**bulk 服务流拷贝与 L3 DATAGRAM 同线程**，且**共享 cwnd**（DATAGRAM **不**吃流控 `send_window`，它吃 cwnd + `datagram_send_buffer_size=1 MiB`；`stream_receive_window`/`send_window` 只管流）⇒ 相互拖累 | `tools/quic-ab.sh cpu`（独占机器）+ **新增「服务流 bulk 与 L3 时延/吞吐共存」本地 A/B** | **登记为 M3 风险与门槛面**（不是「不涉」，见风险 §10-5）；**服务流吞吐相对门槛（N15 订正为可操作口径）**：**同刻同承载、同一服务操作**（files 上传/下载、speedtest 一轮）的吞吐 **≥0.95× WG/UDS 档读数**（不是「L3 基线」——那没定义量什么） |
| 客户端读交接 | `files.rs:213-229` 的无界 mpsc（A14） | 登记（与今天同形） | 不设界；后续切片可加（§5.3-A14） |
| 内存（单连接） | 默认不可用（200 流 × 1.19 MiB ≈ **238 MB**）⇒ 设计取 **64 bidi × 256 KiB = 16 MiB 最坏/连接**（uni=0）+ `send_window 2 MiB`（连接级共享）+ 待发队列 64 KiB×活跃流 + socketpair 64 KiB×2×活跃流 | `tools/quic-ab.sh mem`（vmmap 三轮下中位）+ 稳态/负载态两档 | 门槛「单连接 ≤+320K」是**稳态**口径（不开流/无流量）；**最坏窗口是上界不是常驻**——两口径**分开登记、禁止混读**；**登记残余**：队列量随活跃流数线性（64 活跃流 ⇒ ≈8 MiB 潜在量），须与「最坏窗口」同表列出 |
| 出口内存/线程 | 泵 = **异步任务（不新增线程）**；服务线程数与今天 UDS 形态同 ⇒ **不新增量级**；intake = **全局信号量**（files 16+4 / term 32+4 / speedtest 12+4；probe 不进队列） | 出口 32 连接 × 64 流最坏 = 2048 流 —— 由**全局 intake** + 服务在册上限构造性压住 | **残余（登记）**：恶意已认证设备可开满 64 流（今天 UDS 同样可开满 16/12；不是新面） |
| ACK/整形换轨 | 栈 B 的 ACK 密集时钟退场；对位 = QUIC `ack_eliciting_threshold=16` / `max_ack_delay=5ms` | A11 登记 + 服务流吞吐对照 | 旧 `PERF-AB` ACK 密度读数**跨承载不可比** |
| 预算可测性 | 全部走既有 harness + 新增恢复 e2e（§3.3）+ 服务流吞吐/共存 A/B | 每项给「命令 + 读数落点」 | 见 §9/§11 |

---

## 8. 判据行影响 + 登记条目草案

### 8.1 分类总表

| 类 | 条目 | 动作 |
|---|---|---|
| **E 系列（出口）** | **E1**（`serve 就绪：… files=%d term=%d speedtest=%d …`） | **值域不变**（虚拟端口号仍是配置面）——但 `tools/local-exit.sh:104`/`local-rust-exit.sh:76`/`matrix.sh:210`/`qi-ab.sh` 全部 wait 该行 ⇒ **显式登记「M3 复核：零变更」**（原稿漏，设计门 4-1 补） |
| | E5（豁免就绪） | **零变更**（D1/D3 都成立；§2.3） |
| | E10/E11（`intercept: tcp %s …`） | **行文不变、输入集变化**（QUIC 档服务腿不再产生 `exempt` 行；D3 下连 WG 服务腿也没了） |
| | E12（`udp intercept: 会话 #…`） | **零变更**（UDP 面不经服务 tag） |
| | E13（speedtest 结算） | **零变更**（§9-R3 的判据行） |
| | **E14 / E17** | **行文改写**（D1/D3 两形态都改：服务入口不再经拦截层转投） |
| | E15 / E16 / E16a–d | **零变更**（行文不含隧道/端口；E16 已核） |
| | E-q1/E-q2/E-q4 | 零变更 |
| | E-q3 | **明细行文 + 接受集收窄**（§6） |
| | **新增 E-q5** | additive（服务流受理/拒/结束族） |
| | **准入关闭码表**（非编号；安全面登记） | additive（§4） |
| | **DC18**（`dialOk/dialFail/reject/flows` 输入集） | **输入集收窄**（服务腿不再经拦截）——登记 |
| **C 系列（客户端）** | C2/C4/C5/C6/C10/C13/C15（+C2'/C4'/C5'/C6'/C15'） | **零变更** |
| | C8（暖机判据位） | **quic 档依据收紧**（**措辞 = 「更正 M1 登记原文的过度声明」**，设计门 3-5） |
| | **C11（RECOVER 族）** | **保留**（WG 档有效）+ 登记「QUIC 档不产生」+「C18 与之**替代**关系」 |
| | **新增 C18**（链路恢复族） | additive |
| | **新增 C19**（服务流行族） | additive |
| **DC/CA 族** | DC14/DC15、CA1/CA4/CA5 | **D1：不变**（复核）；**D3：全部停用登记**（含 CA7/CA8/CA9/CA10——CA9 的 `refused（7803 回 RST）→ not_supported` **就是 §1.6 哨兵本体**，原稿漏列，设计门 4-2 补） |
| **计数输入集/配置** | drops 四类不变；**新增**服务流计数 + `quic` 段新键 + 准入码 | additive（§8.2-12） |
| | `migration_unconfirmed` | **数值语义变化**（落「计数输入集/数值语义」表，不是主表——设计门 4-3） |
| **fixtures** | `tunnel_addr.json`/`files_frames.json`/`term*`/`surface*` | **零改**（退役归 M5） |
| **tier 文档** | `connection-lifecycle.md` 恢复阶梯节 | **修订稿草案交付**（§3.4；tier 侧触点 = 用户） |

### 8.2 登记条目草案（照登记表 **五字段**：日期 / 条目 / 从 → 到 / 原因 / 影响面；日期 = 实施批当日）

1. **E14 行文改写**：`files 就绪：root=%s (rw) sock=%s（隧道IP:%d 经拦截层转投）` →
   `files 就绪：root=%s (rw) sock=%s（服务流 tag=1；QUIC STREAM 承载；本机 UDS 仍为 WG 服务腿入口）`
   ｜原因：M3 服务流入口改为 STREAM tag 分发（应用层帧不变）；D1 下 UDS 仍是 WG 服务腿入口 ⇒ 新行文
   须同时说清两者｜影响面：出口排障读者；`server/engine.rs` 的 files 就绪行；**按旧串写断言的脚本/文档须改**
2. **E17 行文改写（同条目 1，但要点不同——不作「同上」转引）**：
   `speedtest 就绪：sock=%s（隧道IP:%d 经拦截层转投；内存收发不落盘）` →
   `speedtest 就绪：sock=%s（服务流 tag=3；QUIC STREAM 承载；内存收发不落盘）`
   ｜影响面：出口排障读者；`server/engine.rs` 的 speedtest 就绪行；**E13 的模板与采样行**
   （`INTEROP-CRITERIA.md:31` 的 `speedtest: 会话 #%d role=%s …`；`:110` 是 **E17** 的 Rust 实采样本，
   不是 E13）+ 任何按旧串 grep 的脚本
3. **E10/E11 输入集变化（行文不变）**：QUIC 档服务腿（files/term/speedtest 的 `kind=exempt` 行）
   **不再产生**；隧道 IP 上的**回环同端口豁免**仍产生；**D3 下 WG 服务腿同批消失**
   ｜原因：M3 服务流改 tag 分发（D1）/ + UDS 映射退役（D3）｜影响面：`server/intercept` 行读者；
   按「每命令一条 exempt 行」写断言的脚本须改；**DC14/CA1 的实采样例走 CLI/WG 面（D1 下仍有效）**
4. **E5 复核（无变更显式登记）**：`intercept: 过境拦截就绪（隧道IP %v；豁免=转投本机同端口；TCP 并发上限 %d）`
   在两案下**逐字不变**｜原因：显式登记「无变更」防后续读成漏登（先例：E6/E7/E18）｜影响面：无
5. **E1 复核（无变更显式登记）**：`serve 就绪：… files=%d term=%d speedtest=%d …` 的**字段与值域不变**
   （虚拟端口号仍是配置面；M3 只是「隧道侧不再用它」）｜原因：同上（原稿漏，设计门 4-1 补）
   ｜影响面：`tools/local-exit.sh`/`local-rust-exit.sh`/`matrix.sh`/`qi-ab.sh` 的 wait 面（**均不变**）
6. **E-q3 明细行文 + 接受集**：`（本次：源校验拒 src=%v ∉ {tunnel_ip=…,tun_ip=…}（dev=…）；…）` →
   QUIC 档接受集收窄为 `{tun_ip}`（明细同步打印单元素集）｜原因：`tunnel_ip` 在 QUIC 档无合法来源（§6）；
   **四字段计数行逐字不变**｜影响面：`crates/homeway-quic/src/exit/*`、E-q3 读者、按双元素集写断言的脚本；
   **计数输入集变化**须落 `INTEROP-CRITERIA.md` 的 [[计数输入集 / 数值语义变化]] 表（`:662+`）——从→到 =
   「源校验拒的输入集 = 内层包源 ∉ {tunnel_ip,tun_ip}」→「∉ {tun_ip}」
7. **新增 E-q5 服务流出口行族（additive）**：无 → 有：`quic: 服务流已受理（tag=%s dev=%s 第 %d 次）`（节流）/
   `quic: 服务流拒（dev=%s tag=%s；%s；第 %d 次）`（`%s` ∈ {未知 tag, 服务不可用, 入口队列满 n/cap, 连接未绑定,
   tag 读取超时}）/ `quic: 服务流结束（tag=%s，↑%dB ↓%dB）`（节流）｜原因：M3 服务流面的唯一可观测面
   ｜影响面：出口排障读者；落点 `crates/homeway-quic/src/exit/**`
8. **C8 判据位的语义更正（quic 档；行文不变、值域不变）**：从 =「暖机判据位由岛 `Cmd::Probe`（**QUIC
   STREAM 回显**）满足」（M1 登记原文即如此写，但**当时的实现**是 `open_uni + reset` + 等 `udp_rx`，
   `client/mod.rs:301-333` 自陈「**不是**端到端回显」）→ 到 = **「更正 M1 登记原文的过度声明」**：
   M3 前 = 连接级判据；**M3 起才**为 `STREAM[probe]`（tag=5）真回显。政策允许追加「后续」说明
   （`INTEROP-CRITERIA.md:510-511`）｜原因：M3 定型 probe 流（设计门 3-5）｜影响面：C8 读者；
   实现 = `client::probe` 改走 tag=5（与 `uni=0` 同切片，§1.7-N13）
9. **C11（RECOVER 族）的处置（保留 + 不产生 + 替代关系）**：C11 族（`RECOVER R1/R2/R3` + 恢复/走完行）
   **行文与语义对 WG 档**逐字保留；**新增登记两条**：①「**QUIC 档不再产生 C11 族行**」；
   ②「**C18 与 C11 是替代关系**（同一期同一承载不会两族并存；QUIC 档恢复时间线的唯一行族 = C18）」
   ｜原因：M3 阶梯重写（QUIC 档无 R1/R2/R3），WG 档旧阶梯保留到 M5｜影响面：C 族读者；按 C11 grep 的
   排障脚本须按承载分档（否则会以为 QUIC 档恢复面无观测）
10. **新增 C18 链路恢复行族（additive；QUIC 档）**：无 → 有：`quic: 链路快探失败（连续 %d，原因=%s）` /
   `quic: 链路探活抖动（%s，已复探）` / `quic: 链路重连中（原因=%s，第 %d 次）` /
   `quic: 链路重连完成（原因=%s，耗时 %v）` / `quic: 链路重连失败（原因=%s，第 %d 次）—— 交世代重建` /
   `quic: 世代重建（原因=%s；连续重连失败 %d）`；**并登记 N-b 行的动作前置条件收窄**
   （`migration_unconfirmed` 窗口 60s → ≤1 快探预算）｜原因：M3 阶梯重写（QUIC 档无 R1/R2/R3）
   ｜影响面：C 族读者；`recover.rs`（WG 档保留）与新恢复面并存；`load` 类脚本
11. **新增 C19 服务流行族（additive；客户端）**：无 → 有：`quic: 服务流已开（tag=%s，耗时 %v）`（节流）/
   `quic: 服务流失败（tag=%s；%s）`（`%s` ∈ {未绑定, 服务不可用, 入口队列满, tag 读取超时, 超时, 承载重连}）
   ｜原因：服务流拨号缝换轨后的归因面（今天由 `healing_dial` 失败行承担）｜影响面：App 排障读者 / 核日志
12. **`quic` 段 / 计数输入集（additive）**：无 → 有：`quic` JSON 段新增
    `{streams_open, streams_refused{tag_unknown,disabled,intake_full,unbound,tag_timeout}, streams_active,
    stream_bytes{in,out}, stream_backpressure_events, sock_send_errs, admit_reject_code, admit_reject_text}`
    ｜原因：M3 服务流面 + §4 归因 + §3.1 的 M/R 判别信号都要观测落点；行与快照同源（M2 §15-1 口径）
    ｜影响面：`tunStatusJSON` 读者（additive）；**出口 `serve status --json` 的 quic 段（M2 交下项）同批落**
13. **准入关闭码表 + 客户端映射（非编号判据行；安全面 + 归因面登记）**：出口拒绝路径的
    `CONNECTION_CLOSE` 码从恒 `0` → `{0x11 凭证不被接受, 0x12 资源暂不可用, 0x13 准入数据非法, 0x14 准入超时}`
    （**仅准入窗内 3 处**：`conn.rs:507/525/464`）；客户端**限定未绑定态**映射 typed 归因 + 一行
    **`quic: 准入回执（code=0x%02x %s）——本世代回落 WG 承载`** + 岛快照两字段；**准入后**的同类关闭走
    `SessionClosed` 独立分支（`bridge.rs:367` / `:458` 两处**不**入准入映射）
    ｜原因：M2 真机发现①（三种拒绝原因在设备侧不可见 + WG 回落使黑洞期不可见）｜影响面：`exit/conn.rs`、
    `client/{register,race}.rs`、`IslandErr`、岛快照、E-q 归因行读者；**安全面：两桶粗粒度、不发新帧、不放宽任何闸**
14. **新增服务流复位码表（0x21–0x27）+ `StreamErr` 取值集（additive，wire 面）**：无 → 有（§1.6 表）；
    **并登记 `ConnErr::Refused` 消费点的逐点迁移表（§1.6）与 `ConnErr::Refused` 的 Display 去 RST 化**
    ｜原因：换轨后「服务不存在」不再以 RST 出现；`Refused` 的启发式不再成立｜影响面：`facade/bridge_host.rs`
    （`is_refused_like` + 测速桥）、`facade/service_exec.rs`、`facade/tun_exec.rs`（`conn_err_to_io`）、
    `daemon/mod.rs`（两处）、`daemon/carriers/speedrun.rs`；**App 侧须有「出口无测速 ⇒ not_supported」e2e**
15. **`migration_unconfirmed` 语义收窄（落「计数输入集/数值语义变化」表）**：判定窗口 60s（巡检拍）→
    **≤1 个快探预算（≤1s）**；字段与行文不变，**语义**登记｜原因：§3.1（该位升为动作前置条件）
    ｜影响面：状态 JSON 读者、N-b 行读者、M1 真机「路径变更已验」的复现脚本
16. **配置与常量（additive，须写清 `config.toml` 键 vs 硬编常量，并点全影响面）**：无 → 有：
    硬编常量（`exit/transport.rs`，两端共用）：`max_concurrent_bidi_streams=64` / `max_concurrent_uni_streams=0` /
    `stream_receive_window=256 KiB` / **`send_window=2 MiB`（quinn 真名；**连接级**，非每流）**；
    客户端常量：`PROBE_FAST_BUDGET=700ms`（+复探 2×）/ 待机 60s / `stream_pending_bytes=64 KiB`；
    出口常量：`TAG_READ_BUDGET=5s` / intake 容量 **`{files: 20, speedtest: 16, term: 20}`**
    （= 各服务**在册上限 + K=4**；files 在册闸 16（`files_server.rs:27`）、speedtest 12
    （`speedtest_server.rs:27`）、**term 今天无连接级闸（只有会话上限 16）⇒ 20 是「新引入的连接级上限」，
    须作为行为变化登记**，§1.7） / socketpair `SO_SNDBUF|SO_RCVBUF=64 KiB`。**若其中任何一项落 `config.toml`**，影响面须点
    `nodestate.rs`（配置解析）+ `serve_cli.rs`（CLI 面）+「对 Go 单向不兼容」说明（三条先例：
    M1 的 `serve.quic*`/`tunConfig.*` 条）｜原因：quinn 默认（200 流 × 1.19 MiB）与产品内存预算不符；
    并发上限与快探节拍必须显式｜影响面：`exit/transport.rs`、内存门槛读数口径、`tools/quic-ab.sh`
17. **零差异登记（防漏登）**：E12 / E13 / E15 / E16（+E16a–d）/
    E-q1 / E-q2 / E-q4 / C2'/C4'/C5'/C6'/C15' **显式登记「M3 零变更」**｜原因：先例 E6/E7/E18｜影响面：无
18. **stackb / UDS 分支退役面登记（按 §12-① 裁决形态收口）**：D1 ⇒ 记「QUIC 档消费点清零 + 余下消费者
    = WG 承载（12 处清单）」+ 净删/改行数 + 「`local_services` 保留」；D3 ⇒ 追加 C11/C8/DC14/DC15/CA1/CA4/CA5
    + CA7–CA10 停用条 + `tools/quic-wg-e2e.sh` 断言改写 + intercept 5 用例删除
    ｜原因：§5.2 的事实链（CLI/daemon WG-only）

---

## 9. 真机验证计划（App files / term / speedtest）

**设备**：`FMR0224116011480`（专用测试机，已授权）。**本棒只读侦察**：设备在跑的核 = **M2 版**
（`tier-core: quic: 注册刷新 → 192.168.3.12:52700（dev=aca645d3）`，末行 12:13:19 `quic: 岛收工`），
VPN 关闭、无 tun 接口、无 panic 面。**手册**：`docs/DEVICE-TEST-OHOS.md`（出包/装机/免点屏注入 token/
uitest/日志面，全部沿用）。

| # | 项 | 做法 | 判据 | 前置 |
|---|---|---|---|---|
| R1 | **files 全绿** | App files 页：列目录 / 上传 / 下载各一次 | 核 `quic: 服务流已开（tag=files…）` + 出口 `quic: 服务流已受理（tag=files…）`；文件 sha256 一致 | 本地私有出口（**绝不碰现役**） |
| R2 | **term 全绿** | terminal HSP：起会话、回显、`Ctrl-b d` 分离、re-attach | HSP 帧逐字节（向量已钉）+ 会话在出口持存（断开-重连后 re-attach 恢复） | 同上 |
| R3 | **speedtest 全绿** | App 首页测速卡片一轮（下+上） | 出口 E13 结算行在场 + 核 `tag=speedtest` 受理行 + 与 M2 同档读数（同刻 A/B 口径） | 同上 |
| R4 | **恢复 ≤3.5s（真机）** | 建连后 kill -9 本地出口 → 等其重新监听 → 记录核日志「快探失败/复探/重连完成」时刻 | **`T_recv`（出口重新监听 → 首个回显成功）≤3.5s**；同时记 `T_total`（并登记 T_total 含出口启动的预期超额）；`peer: +` 不新增条目 | 同上 + **App 前台/有流量**（在用档） |
| R5 | **瞬时黑洞不误动作** | 用 M2 自写 UDP 中继（`nat-rebind-proxy.py` 形态）制造 1.5s 丢窗 | **不产生 R/B**（只记抖动行）；窗口结束后恢复 | 同上 |
| R6 | **准入失败归因（§4）** | 注错 token（`forge_token.py` 形态）⇒ 看核侧归因行与状态面 | 核侧出现 `code=0x11 凭证不被接受`（不再是通串）；`readyBy=wg` 的归因可见 | 同上 |
| R7 | **服务面不外溢（A/B）** | `serve.quic=false` 档复跑 `tools/quic-wg-e2e.sh` | D1：三断言 + C 族原串**全绿**；D3：按改写后断言 | 同上 |

**做不了的（写明）**：①**WiFi→蜂窝换网**（设备无蜂窝数据，M1/M2 两次登记阻塞）；②真机窄路径 MTU 注入；
③TUN 口径吞吐（设备无 curl/wget）。

---

## 10. 风险与未决

| # | 风险/未决 | 影响 | 处置 |
|---|---|---|---|
| 1 | **CLI/daemon 是 WG-only ⇒ stackb + UDS 分支删不净**（§5.2） | M3 退出口「stackb 删除合入」「intercept 豁免端口→UDS 退役」**不可达** | **上报裁决**（§12-①）；建议 D1 |
| 2 | **服务入口换形态的隐藏耦合** | 「逐字节不变」承重点 | 方案 B′（零 diff）+ 异常序列对照矩阵；方案 A 的成本清单已落纸（§2.2） |
| 3 | **快探假阳性**（700ms 预算在弱网；误判进 B ⇒ 世代重建 ⇒ **可能回落 WG 承载**） | 在途流中断 + 承载降级 | 四重抑制（§3.2）+ 真机 R5（瞬时黑洞）+ 本地两条负向 e2e；参数可配 |
| 4 | **流窗口/队列内存最坏值**（64×256 KiB + 队列） | 内存门槛读数被误读/超预算 | 显式设值 + 稳态/最坏两口径分开登记（§7） |
| 5 | **单 runtime / 单连接上服务流 bulk 与 L3 相互拖累**（设计门 M-1） | 真机 R3 或「边下文件边浏览」时才暴露，且会被归因成网络问题 | §7 登记 + 新增「服务流 bulk 与 L3 共存」本地 A/B + 相对门槛 |
| 6 | **`open_bi` 阻塞式背压**（实测第 101 条阻塞） | 岛内单线程被挂 | 岛内**禁 await 到写满** + 自记账额度 ⇒ `Busy` 快速失败 + 单测负例 |
| 7 | 重连后**在途服务流全断** | files 传输中断（用户可见） | 与今天 WG 重建语义等价（A4）；term 会话可 re-attach；App 侧重试 |
| 8 | **`ConnErr::Refused` 分类学迁移**（6+ 消费点） | 误报「not_supported / link_down」 | §1.6 迁移表 + App 侧 e2e（S3 判据） |
| 9 | tier `connection-lifecycle` 修订稿 = 用户触点 | 文档与实现短期不一致 | M3 出**草案**（§3.4），交付由主会话转达 |
| 10 | 与并行小批的树冲突 | rebase 成本 | 已 rebase 到 `539567f`（§0.1）；开工先复看 HEAD |

---

## 11. 实施清单（S1–S10 + 依赖顺序 + 每项完成判据；供主会话切棒）

> 顺序：`S1 → S2 → {S3, S5} → S4 → S6 → S7 → S8 → S9 → S10`（S3 依赖 S1/S2；S4 依赖 S1；S6 依赖 S3/S4）。
> 每项完成判据含「落地 + 测试 + 判据行 + 登记」四件套（M2 体例）。

- **S1 流面协议与岛侧（协议层）**：`Cmd` 新增流族（Open/Write/Read/Close，`WriteOut` 同形；**读无限期 + 取消**）；
  岛内每流写者任务 + 有界待发队列（64 KiB，懒分配）；tag 常量 + 复位码表（0x21–0x27）+ `StreamErr`；
  **`SockStats` 增 `sock_send_errs` + 末次错误 kind/时刻**（**计数点 = `try_send` 的非 `WouldBlock` 错误 +
  errno 白名单 + 新鲜度窗 + rebind 清零**，§3.1-N5）；probe = 持久流 + **真回显**（**与 `uni=0` 同切片**，§1.7-N13）；
  自记账域 = {控制流, probe, 服务流}（有效服务容量 = 上限 − 2，§1.4-N14）。
  **完成判据**：单测（背压 `n=0` 回执 / 半关 / 复位码 / 未知 tag / 额度耗尽快速失败 / 读取消 /
  **上行 `WouldBlock` 高频时 `sock_send_errs` 不增长**）+ 隔离门九条 + 「岛内命令循环零 `await` 到写满」
  的可判定事实（脚本断言或结构断言）。
- **S2 出口分发与服务入口（B′）**：`ServiceIntake`（**五件套**：`from_listener` 构造面 / `accept()` 两源 /
  **唤醒面 pipe/eventfd** / **两源轮转（UDS 优先 + QUIC 连取上限 8）** / 就绪 fd）+ 泵（半关/复位传播）+
  tag 路由（**每流并发 task**）+ per-tag 全局 intake（在册上限 + K）+ 拒行/计数；
  `serve_stoppable` 形参 `UnixListener → ServiceIntake`（files/speedtest 各 ~10 行；**term 的 38 行自建
  accept 循环重写**并保住「poll 到达即 accept、无 200ms 空闲延迟」性质）；
  E14/E16/E17 行文 + E-q5 计数；**落 M2 交下的「出口 `serve status --json` 的 quic 段」**；
  **新异步文件命名先定死（如 `exit/intake.rs` / `exit/pump.rs`）并同批入 `tools/check-quic-isolation.sh`
  的 `ASYNC_FILES`**（该门**双向 fail-closed**：含异步名不入清单 = 红；入清单但零异步名 = 红）。
  **完成判据**：三服务**帧层**单测全绿 + `term/service.rs:4160-4177` 与 `speedtest_server.rs:1048-1060`
  两处监听面测试**改用 `from_listener`（零改语义）** + **异常序列对照矩阵**（正常/两种半关/复位/超时/
  队列满 ⇒ 字节流 + **终止原因按腿定义写死并断言**）+ busy 路径在 STREAM 面可达（第 17 条流拿到 `server_busy`）
  + **QUIC 持续开流时 UDS 源仍能受理**（公平性用例）+ **首帧延迟 ≤ 一个心跳**用例 +
  `serve status --json` 段有实测读数。
- **S3 客户端服务流换轨（App 核）**：`tun_exec.rs:310`（桥拨号）、`service_exec.rs:577`、`files.rs:207` 改开 STREAM；
  虚拟端口 → tag；`DialFn` 签名不变（换实现）；**阶梯豁免集**（§1.6）+ `Refused ⇒ NotSupported` 等价改写 +
  `conn_err_to_io` 新分支。
  **完成判据**：本地 e2e（App 核形态）files/term/speedtest 三条全绿 + 「服务级拒绝不触发恢复」单测 +
  **App 侧「出口无测速 ⇒ not_supported（不是 link_down）」e2e** + 文案去端口/去 RST（A13）。
  **可后补（登记）**：`files.rs:213-229` 的客户端读交接通道加字节上界（A14）。
- **S4 阶梯重写（QUIC 档）**：快探（700ms/背靠背 + 待机 60s）+ 复探 + M/R 判别（`sock_send_errs`）+
  抖动三连升格 + B 的门（连续 2 + 窗 ≥10s）+ `migration_unconfirmed` 收窄 + C18 行族 + 世代重建归因衔接；
  **WG 档阶梯逐字保留**（分档）。
  **完成判据**：`start_paused` 定时用例（预算/节拍/抖动/复探四分支）+ **故障注入 e2e**（kill -9 ⇒
  **`T_recv ≤3.5s` 上界断言，起点 = E1 `serve 就绪` 行时刻**；**两种相位各测一次**：出口停机 <`T_detect`
  与停机 5s）+ `T_detect` 单独登记读数 + **负向 e2e 两条**（瞬时黑洞 1.5s ⇒ 不动作；服务面卡死 ⇒ 必有动作）+
  WG 档 C11 族行逐字不变（对照用例）+ **`sock_send_errs` 的 M/R 判别两向用例**（源地址失效 ⇒ M；对端死 ⇒ R）。
- **S5 准入失败归因回传**（§4）：出口三处 close 码 + 客户端**准入窗内** typed 映射 + 准入后
  `SessionClosed` 分支 + 岛快照两字段 + `quic: 准入回执（…）`行。
  **完成判据**：三档注入（错 token / revoked / table-full）各产对应码 + 客户端行 + 状态面字段；
  **准入后关闭（替换/吊销）不误报为「准入被拒」**（负例）；安全面复核（不发新帧、闸的计数集不变）。
- **S6 stackb 退役与遗留假设扫查**：按 §12-① 裁决执行 D1（QUIC 档消费点清零 + 脚本断言 + 余下消费者清单）
  或 D3；A1–A13 逐条落「谁接管/何时接管」；`tools/check-quic-isolation.sh` 增一条「QUIC 档路径零 `stackb::`」。
  **完成判据**：脚本断言双向验证（负例 ⇒ 确定红）+ 净删/改行数登记 + A1/A8 的承接人写明（M4 设计门必列项）
  + A9/A10/A11/A12/A13 的用例或登记落地。
- **S7 判据行登记 + tier 修订稿草案**：§8.2 的 16 条逐条落 `docs/INTEROP-CRITERIA.md`（**含五字段**；
  与代码同批 commit）+ `connection-lifecycle.md` 修订稿草案。
  **完成判据**：`tools/check-vocab.sh` PASS + 登记表条目齐全 + tier 草案落 `docs/reviews/M3.md` 附录。
- **S8 门槛与真机**：`tools/quic-ab.sh`（CPU/size/mem，独占机器）+ **服务流吞吐相对门槛 + bulk/L3 共存 A/B**
  + 真机 R1–R7（§9）。**完成判据**：读数入 `docs/reviews/M3.md`；3.8MB 与内存格按 M5/用户口径只登记不判死。
- **S9 代码门**（第二道 dsh 门）：范围 = M3 实施 commit；**专项 = 删 stackb/UDS 分支后的遗留假设**
  （A1–A13 逐条给证据）+「`_wg` 回退面的功能等价性」（D1 形态）。
  **完成判据**：`docs/reviews/M3.md` 含两轮意见摘要 + 逐条处置 + 高危必改/豁免登记 + 测试/判据/实测证据。
- **S10 路线文件与收口**（主会话触点，本清单只提示）：按 §12-① 裁决形态更新 `docs/QUIC-ROADMAP.md` 的
  M3 判据/退出口（「stackb 删除」「依赖树不含 smoltcp」「intercept 豁免→UDS 退役」三条的从→到）+ 状态表。

---

## 12. 待用户拍板 / 待主会话裁决

1. **stackb / UDS 分支的退役形态（最关键）**：**D1**（推荐：M3 清零 QUIC 档消费点 + App 核服务流迁移；
   stackb 与 UDS 映射随 M5）vs **D3**（M3 删净 + WG 档服务面/巡检/C11/DC/CA 停用登记，代价清单见 §5.2）。
   **路线文件 M3 三条判据/退出口的从→到草案（裁决后由主会话同批落，§11-S10）**：

   | 路线文件原文（行号） | 从 | 到（D1） | 到（D3） |
   |---|---|---|---|
   | `QUIC-ROADMAP.md:396`（判据） | `客户端依赖树不再含 smoltcp（脚本验证）` | **`客户端 QUIC 档路径零 `stackb::` 可达引用`**（`tools/check-quic-isolation.sh` 新断言；依赖树口径因「core 内出口面共用 smoltcp」不可判 ⇒ 替换），**stackb 本体与 WG 档消费点保留** | 原句保留 + 附加「WG 档服务面停用登记」 |
   | `QUIC-ROADMAP.md:404`（退出口） | `stackb 删除合入` | **`QUIC 档消费点清零合入`**；**本体删除移 M5**（与 `wgcore` 同批） | `stackb 删除合入`（原句），代价见 §5.2-D3 |
   | `QUIC-ROADMAP.md:391-392`（范围） | `出口：intercept 的「豁免命中端口 → UDS」分支退役（只剩 DNS:53/:5300）` | **`QUIC 档不再经该分支`**；分支本体保留（消费者 = WG 服务腿）至 M5 | 原句（M3 即删） |

   理由：该三条**不是 `INTEROP-CRITERIA.md` 的行**，但属**期范围真源**；不改措辞 = 收口时无据（设计门 4-2/N10）。
2. **快探参数初值**（`PROBE_FAST_BUDGET=700ms` + 复探 2× / 在用档背靠背 / 待机档 60s /
   B 门 = 连续 2 + 窗 ≥10s）——是否需要 env 消融臂（照 `HOMEWAY_QUIC_MTU` 先例）？
3. **流窗口/并发/队列初值**（64 / 256 KiB / `send_window` 2 MiB / 待发 64 KiB / intake = 在册上限+4 /
   socketpair 64 KiB）——申请「实施期标定」授权。
4. **`dial` tag 的 M4 边界确认**：M3 只定协议 + 拒未启用（`0x22`）+ 登记「出口本机」语义缺口与
   地址接受集（A8），M4 换轨——与路线文件 M4 节一致。

---

## 13. 实测锚点（`/tmp/m3lab` 验证台；**本棒不写产品代码**，只做 `/tmp` 实验）

**台架**：`/tmp/m3lab/`（`Cargo.toml` + `src/main.rs` + `cert.pem/key.pem` + `run*.sh`）。
quinn 0.11.12 + rustls(ring)，`TransportConfig` **逐值照抄** `crates/homeway-quic/src/exit/transport.rs`
（`initial_mtu=1400` / `min_mtu=1320` / `max_idle_timeout=30s` / `keep_alive_interval=10s` /
`ack_eliciting_threshold=16` / `max_ack_delay=5ms` / datagram 缓冲 1 MiB）；回环 IPv4。
**命令（逐台）**：`m3lab serve <port>` / `probe <port> <budget_ms> <n> <gap_ms>` / `idle <port>` /
`dial <port>` / `heal <port> <budget_ms>` / `semantics` / `streams <port> <k>`。
**驱动脚本**：`run.sh` / `run2.sh` / `run3.sh` / `run5.sh` / `run6.sh`（v3 = 全台原始行落盘）。
**原始产物（复核用）**：`t1-idle.txt` / `t1-tl.txt` / `t2-probe.txt` / `t2-tl.txt` / `t3-serve.txt` /
`t3-dial.txt` / `t4-semantics.txt` / `t5-streams.txt` / `a-probe.txt` / `a-tl.txt` / `b-heal.txt` / `b-tl.txt`。
**复现**：`cd /tmp/m3lab && zsh run6.sh`（约 60s；含 kill -9 注入与重启）。

### 13.1 检测时延（**§3.3 的承重证据**；原始文件已落盘，见各条文件名）

```
[T1] 连接级死亡检测（无流量的纯连接；`idle` 台；**原始文件 `t1-idle.txt` / `t1-tl.txt`**）
  运行 1：IDLE_READY t=1791519544801 → IDLE_DEAD ms=40029 reason=TimedOut
  运行 2：IDLE_READY t=1791519625597 → IDLE_DEAD ms=40021 reason=TimedOut
  运行 3（复跑留证，kill 后口径）：IDLE_DEAD ms=40028 reason=TimedOut（距 kill 35993ms，kill 前已跑 4s）
  ⇒ 30s idle + ≤10s keep_alive 相位 ⇒ 与真机 M2 的 32s/41s 同带（**归因订正**见 §0.4-1）

[T2] 快探（`STREAM[probe]` 形态）检测时延（服务端进程 kill -9；**原始文件 `t2-probe.txt` / `t2-tl.txt`**）
  · 预算 300ms / 间隔 100ms：kill_at=1791519842154 → 首次失败 t=1791519842547 = **+393ms**
      `PROBE i=50 ok=false w_ok=true closed=false us=302510`
      `PROBE_FAIL_REASON None`（首 34 条**全为 None** ⇒ **传输层未察觉，应用层回显是唯一快判据**）
  · 预算 300ms / 间隔 200ms（`heal` 台，原始文件 `b-heal.txt`/`b-tl.txt`）：
      `HEAL probe_failed round=20 detect_ms=303.3 reason=None` ⇒ kill_at=1791519746237 → 失败 = **+318ms**
  · **预算 700ms / 间隔 0（背靠背；支撑 §3.2 的设计值 = §13 复跑）**：
      kill_at=1791521204520 → 首次失败 t=1791521205226 = **+706ms**（≈ 预算本身）
      `PROBE i=120021 ok=false w_ok=true closed=false us=703091`；`PROBE_FAIL_REASON` 7 条（首条 `None`）
```

### 13.2 重连代价与出口启动（`dial` / `heal` 台；**T5 已按设计门 N7 重测**）

```
[T3] dial（回环，服务端在场；原始文件 `t3-dial.txt` / `t3-serve.txt`）：
     DIAL handshake_ms=2.7 first_stream_echo_ms=0.2 total_ms=3.1 got=true
[T4] heal（kill -9 → 1s 后重启；探活失败即重连；原始文件 `b-heal.txt`/`b-tl.txt`）：
     kill_at=1791519746237 restart_at=1791519747295（重启脚本间隔 1058ms，**含脚本的 sleep 1**）
     HEAL reconnected round=20 reconnect_ms=1007.7 t=1791519747563
     ⇒ 重连完成距 kill **1326ms**、距**重启 268ms**（1007.7 里绝大部分是「等服务端回来」）
     ⇒ §3.3 的「动作 ≤0.3s」= **距重启口径**；**不得与 reconnect_ms 混读**（设计门 F1）
[T5] 出口启动到监听就绪（**设计门 N7 重测**；原始文件 `t3-serve.txt`）：
     SERVER_START t=1791521209549 / SERVER_READY t=1791521209549 ⇒ **进程内 = 0ms（同毫秒）**
     ⇒ 原稿引的「≈1.05s」是**脚本的 `sleep 1` + 取时开销**，**不是出口启动耗时**（已删该错误论证）；
     **进程 exec 时间未单独实测 ⇒ 登记待测**（量级 ~10–50ms）
[T6] 旧连接上的流**不会**随新连接复活（heal 台在服务端重启后仍持续失败到进程被杀）
     ⇒ 「重连后服务流按需重开」是**必需**而非优化（A4）
[T7] 出口进程死亡后**旧端口无 ICMP 快失败可依赖**（T1：连接级 40s 才定音；T2：应用层 706ms 定音）
```

### 13.3 半关 / 复位 / 背压语义（`semantics` 台逐行；原始文件 `t4-semantics.txt`）

```
SRV half_closed(finish) after 4B
CLI echo => ping
CLI read_after_peer_finish => Ok(true)        # 对端 finish ⇒ 本端 read = Ok(None)（EOF）
CLI write_after_peer_finish => true           # 半关单向：仍可写
CLI write_after_reset => Some(ClosedStream)   # 本地 reset 后写 = 错误
CLI read_after_local_reset => None            # 本地 reset 不影响读半边（EOF 幂等）
SRV read_after_finish => Err(Reset(7))        # 对端 reset ⇒ 本端读 = Reset(code)
SRV read_after_peer_reset => None
CLI write_8MiB_noreader => "blocked(timeout)" after_ms=1503   # 对端不读 ⇒ write_all 阻塞（真背压，不静默丢）
CLI stream_closed=false                                        # 阻塞期间连接未被判死
```

### 13.4 并发流上限（`streams` 台；原始文件 `t5-streams.txt`）

```
STREAM opened=1/26/51/76 ... STREAM open_blocks at=101 (1.5s 内未开成)
STREAM done held=100
⇒ quinn 默认 max_concurrent_bidi_streams=100；**第 101 条 open_bi 阻塞（不是报错）**
```

### 13.5 quinn 默认值（源码锚，用于 §1.7 的账）

`quinn-proto-0.11.19/src/config/transport.rs:366-396`：`max_concurrent_bidi_streams=100`、
`max_concurrent_uni_streams=100`、`stream_receive_window=1_250_000`、`receive_window=VarInt::MAX`、
`send_window=10_000_000`（= 8×RWND，**连接级**）、`max_idle_timeout=30_000`、
`datagram_receive_buffer_size=1_250_000`/`datagram_send_buffer_size=1_048_576`。

---

## 14. 设计门记录（dsh r16 首轮 + r17 复审）

### 14.0 首轮（r16）

- **轮次目录**：`/tmp/dsh-review/r16.GNt26M/`（`prompt.txt` / `output.md` 245 行 / `stderr.log`）；
  **exit code = 0**；评审基于 HEAD `539567f`（评审自己发现并行批又落了两个 commit——本设计已据此把
  §0.1 的隔离面订正）。
- **结论（评审自报）**：**阻塞**（设计修订后复审，不建议按现稿开工 S1/S2）；**高危 9**（F1/F2/F3、1-1、2-1、
  2-4、2-5、3-1、3-2、4-1）、中危 **12**、低危 **6**（合计 27 条 + 判据行专项 5 组）。
- **首轮处置总览（逐条见下表）**：**认同 25 / 部分认同 2 / 不认同 0**。**全部高危已在本文内改设计**
  （§2.2 换方案 B′、§1.5 读语义、§1.7 配额与内存账、§1.6 busy/EOF/迁移表、§3.1–3.3 阶梯触发与计时起点、
  §2.3 的 D1/F3 矛盾、§5.3 A1–A13、§8 登记重写、§7 体积账）。
  **部分认同 2 条**：①「2-4 的 intake 表自相矛盾」认同（已订正取值表），但「必须把 intake 改成全局」
  属**设计已选**（原稿 §1.6 已写「全局」，是我在 §8.2-16 的取值表抄写矛盾 ⇒ 归为**登记订正**而非设计变更）；
  ②「4-1④ fail-silent 洞」认同并已修（改为「复探两次后必有动作」），但**「抖动连续 3 次升格」的阈值 3**
  属**待实测标定**（初值，§12-2）。

### 14.1 首轮逐条处置表

| # | 位置（评审） | 问题（摘要） | 严重度 | 处置 |
|---|---|---|---|---|
| F1 | §3.6 不存在（6 处引用）；lab 原始行未落纸 | 证据链不可复核 | 高 | **认同 → 已改**：新增 **§13 实测锚点**（逐台原始行 + 命令 + 时间戳），并把「reconnect_ms 含等重启」与「距重启 268ms」分清（§13.2-T4/T5） |
| F2 | §7 体积按 D3 算却推荐 D1 | 方向算反 | 高 | **认同 → 已改**：按 D1 重算（净增 +30…80 KB）+ 登记「3.8MB 判定仍在 M5、删码余量被吃紧」+ `ProbeWgTouch` 口径写明 |
| F3 | §2.3 删 `local_services` ⟂ §5.2/§8.1 的 D1 承诺 | **硬矛盾** | 高 | **认同 → 已改**：§2.3 改为「D1 保留 / D3 删除」两形态；事实链落纸（CLI 远程面经 `dst==tunnel_ip → exempt → LocalServices → UDS`）；D1 下 M3 出口改动 = 零 |
| F4 | §4 只列 2 处 close；准入后 close 会误报 | 归因面错 | 中 | **认同 → 已改**：补 `conn.rs:464` + `bridge.rs:367/458` + `driver.rs:568`；映射**限定未绑定态**；准入后走 `SessionClosed` |
| F5 | §1.6 内存算术/单位/键名错 | 预算账错 | 中 | **认同 → 已改**：补 `uni_streams` 半边、`send_window` 连接级、1.19 MiB、真名 `send_window`（§1.7/§8.2-16） |
| F6 | §5.3 两处行号过期 + §0.1 隔离面断言不实 | 复核面 | 低-中 | **认同 → 已改**：A3 引 `stackb.rs:33`（+ 2 MiB/连接）、A6 引 `wgcore/mod.rs:1260-1281`；§0.1 重述为「并行批已落，行号基线推进到 `539567f`」 |
| F7 | 全文交叉引用错位（§3.6/§12-③/§9/§10/§11） | 施工可读性 | 低 | **认同 → 已改**：重编号（§13 实测、§14 设计门记录；待裁决条落 §12-① 等），全文引用复核 |
| 1-1 | §2.2 方案 A（trait 泛型化） | **Cargo 环 + 5 项能力缺 + poll 内核必重写 + 2 例 E0282** | 高 | **认同 → 换方案**：推荐 **B′（socketpair 适配器 + 异步泵）**，服务零 diff；A 降为 M5 候选（成本清单已落纸 §2.2） |
| 1-2 | §2.1 骨架（accept→读 tag 串行；tag 预算未定义） | 出口侧队头阻塞 + 额度泄漏 | 中 | **认同 → 已改**：每流并发 task + `TAG_READ_BUDGET=5s` + 超时 `reset(0x27)`（**reset 而非 drop**，归还额度） |
| 1-3 | §1.2 「逐字节不变」缺独立对照 | 承载语义漂移抓不到 | 中 | **认同 → 已改**：对照矩阵加**异常序列**维度（半关时序/复位/超时/队列满 ⇒ 字节流 + 终止原因） |
| 1-4 | §1.1 dial tag 缺「出口本机」语义位；IPv4-only 未写 | M4 误读 | 低-中 | **认同 → 已改**：§1.1 写死 IPv4-only + 登记「出口本机」缺口 = M4 必列（A1③） |
| 2-1 | §1.5 「每操作预算 5s」套到读上 | **空闲流自断** | 高 | **认同 → 已改**：§1.5 立「读无限期挂起 + 显式取消」表；明确**不给读套预算** |
| 2-2 | §1.4 `n=0` 「按 io::Write 契约重试」 | 与仓内教训相反 | 高 | **认同 → 已改**：改写为仓内 Ok(0) 退避环（2ms/10ms/20ms + 10s 无进展界），并登记节拍随承载变化 |
| 2-3 | 读侧无界交接 | 上界消失 | 中 | **认同 → 已改**：§1.4 读侧 = **按需拉取**（岛只在 `StreamRead` 时读，不超前读） |
| 2-4 | 队列内存未入账；「60/连接」非压缩；intake 表自相矛盾 | 内存/线程账 | 高 | **认同 → 已改**：intake = **全局信号量**（在册上限 + K）、队列量入内存账、取值表订正（§1.7/§7/§8.2-16） |
| 2-5 | intake 容量 = MAX_CONNS ⇒ busy 路径不可达 | **应用层语义被替换** | 高 | **认同 → 已改**：intake = **MAX_CONNS + K（K=4）**，保证在册闸先于 intake 满；busy 文案逐字节保留 |
| 3-1 | §1.5 只点一个 Refused 消费点 | 迁移不完整 + 文案说谎 | 高 | **认同 → 已改**：§1.6 迁移表 7 行（含 App 核测速桥）+ Display 去 RST + App 侧 e2e 入 S3 判据 |
| 3-2 | EOF/reset 同形性被打破 | 静默行为变更 | 高 | **认同 → 已改**：§1.6 定「**错误码白名单 ⇒ error；其余 ⇒ `Closed`（保今天 EOF 语义）**」+ 登记 A9 |
| 3-3 | 流级拒绝注入恢复阶梯 | 触发集漂移 | 高 | **认同 → 已改**：§1.6 定**阶梯豁免集**（照 portfwd D11 先例）+ S3 单测判据 |
| 3-4 | §4 理由③引用不实；M2 交下的 `serve status --json` 漏接 | 观测面缺 + 越界引用 | 中 | **认同 → 已改**：删理由③；**M2 交下项补进 §0.2/§11-S2 判据** |
| 3-5 | §8.2-6 的 C8「从」措辞 | 登记从→到不准 | 低 | **认同 → 已改**：改为「更正 M1 登记原文的过度声明」（§8.2-8） |
| 4-1 | §3.1/§3.3 快探触发不可判定、只算一次动作、引用不匹配、fail-silent 洞、假阳性代价 | 行为 + 判据风险 | 高 | **认同 → 已改**：①M 判据改**本机发送面信号**（新增岛侧 `sock_send_errs`），删 `udp_rx` 推断；②加**复探（2×）** + 抖动三连升格（**阈值 3 = 待标定初值**）；③B 门 = 连续 2 + 窗 ≥10s；④最坏序列重算 **2.4s ≤3.5s**；⑤**计时起点写死**（出口重新监听 → 首回显）+ 真机双读数；⑥新增两条负向用例（瞬时黑洞 / 服务面卡死） |
| 4-2 | 承载分档妥当，但缺路线文件判据变更登记 | 收口无据 | 中 | **认同 → 已改**：§12-① 附「路线文件三条判据的从→到草案」+ §11-S10；C18 与 C11 的**替代关系**写进 §3.4/§8.1 |
| 5-1 | A1 方向反了（环回拒绝是客户端约束；出口拨自己的回环是合法功能） | 会打死合法目标 | 中 | **认同 → 已改**：A1 重写（客户端继承 + 出口**不得**加 + M4 必列「出口本机」语义位） |
| 5-2 | **地址接受集（SSRF 面）未覆盖** | 安全缺口 | 高 | **认同 → 新增 A8**：`dial`/tag 缝的地址接受集判定表 = **M4 设计门必列**（逐类 accept/reject + 用例）；M3 在 §1.1 附注登记 |
| 5-3 | `TooManyConns` 死变体 ⇒ 限额=新增失败态 | 行为面 | 中 | **认同 → 新增 A12**：额度耗尽快速失败 `Busy` + 接线或删除死变体（S1/S3） |
| 5-4 | ACK 整形/时钟随栈 B 退场 | 读数不可比 | 中 | **认同 → 新增 A11**：§7 登记 + 服务流吞吐相对门槛 + `PERF-AB` 跨承载不可比标注 |
| 5-5 | 会话收工不发 FIN/RST ⇒ 出口回收时点改变 | 判据时点 | 中 | **认同 → 新增 A10**：登记 + S4 e2e 加「收工后出口侧会话/关闭行时点」对照 |
| 5-6 | 虚拟端口退役的对外可观测面/文案未清 | 文案说谎 | 中 | **认同 → 新增 A13**：§8.2 补文案去端口/去 RST 项 + `tools/local-*.sh` wait 面复核（E1 零变更条） |
| 判据行 1 | 14 条缺「日期」字段；6 条缺「无 → 有」；条目 14 条件式不可粘贴 | 登记不合规 | 中 | **认同 → 已改**：§8.2 抬头写明五字段 + 逐条补「无 → 有」；条目 16 改为「按裁决形态收口」的可粘贴条件式说明 |
| 判据行 2 | 漏登记：E1 / C11 / reset 码表 + `StreamErr` / E-q3 计数输入集 / DC18 / CA7–CA10 | 漏登 | 中 | **认同 → 已改**：全部补进 §8.1/§8.2（条目 3/5/6/8/12/16） |
| 判据行 3 | 命名：`C16'`/`C17'` 与既有 C16/C17 及 prime 约定冲突 | 命名 | 低 | **认同 → 已改**：改名 **C18/C19** |
| 判据行 4 | **P3**：客户端新行 `quic: 准入被拒（…）` 与出口既有行同前缀 | 脚本混淆 | 低 | **认同 → 已改**：客户端行改名 **`quic: 准入回执（code=…）`**（§4/§8.2-13） |
| 判据行 5 | 配置键影响面不全 + `stream_send_window` 不存在 | 登记不准 | 低 | **认同 → 已改**：§8.2-16 按「硬编常量 vs config.toml 键」分列 + 点 `nodestate.rs`/`serve_cli.rs`/单向兼容说明 + 真名 `send_window` |
| **M-1** | 单 runtime 上服务流与 L3 相互拖累（评审结论节的翻车点 3） | 真机才暴露 | 中 | **认同 → 已改**：§7 CPU 行重写为「不涉每包路径**但共存会拖累**」+ 新增共存 A/B + 相对门槛 + 风险 §10-5 |

### 14.2 不认同项

**无**（0 条）。两处「部分认同」见上（§14 总览：intake 全局属原稿已选、抖动阈值 3 属待标定初值）。

### 14.3 复审（第二轮 · dsh r17）

- **轮次目录**：`/tmp/dsh-review/r17.67FeEI/`（`prompt.txt` / `output.md` 140 行 / `stderr.log`）；
  **exit code = 0**。
- **结论（评审自报）**：**有条件通过**——「第一轮 9 条高危**全部实质改到位**；剩 7 条部分解决，
  其中 2 条需在对应切片开工前补齐」；**建议按修订稿开工 S1/S2**，但**开工前先落三条**；
  **S4 开工前**须补 §3.3 起点重算与 T1/T5 证据。
- **复审给的新问题 16 条（N1–N16）**，处置如下（**认同 16 / 不认同 0**；其中 3 条属「原文已改但处置表
  与正文不一致」——已按评审要求补齐）：

| # | 复审发现 | 严重度 | 处置 |
|---|---|---|---|
| N1 | B′ 的「服务代码 100% 零改」与自身改动面矛盾；**term 的 accept 循环不是 5 行**（`term/service.rs:832-871` 是 38 行 `poll(2)` 自建循环） | 高 | **认同 → 已改**：§2.2 口径收窄为「帧层/期限/poll 语义/`try_clone`/`shutdown` 零改」+ 逐处改动面写准 + term 循环重写与「无 200ms 空闲延迟」性质入 S2 判据 |
| N2 | S2 判据「三服务既有单测原样全绿」不成立（`term/service.rs:4160-4177`、`speedtest_server.rs:1048-1060` 传 `UnixListener`） | 高 | **认同 → 已改**：加 `ServiceIntake::from_listener(UnixListener)` 构造面 ⇒ 两处**零改**；S2 判据措辞改准（帧层单测全绿 + 两处改用 `from_listener`） |
| N3 | `ServiceIntake` 缺**唤醒面** ⇒ 每条 QUIC 服务流最多 +200ms（`files_server.rs:937-938` 的 `WouldBlock ⇒ Retry ⇒ sleep(200ms)`） | 中-高 | **认同 → 已改**：契约③「唤醒面 pipe/eventfd + 泵入队 write(1B)」+ 「首帧延迟 ≤ 一个心跳」用例入 S2 判据 |
| N4 | 两源顺序/公平未定义 ⇒ D1 下 QUIC 洪泛可饿死 WG 服务腿（消费者 = CLI 远程面） | 中-高 | **认同 → 已改**：契约④「UDS 优先 + QUIC 连取上限 8 后回让」+ 公平性用例入 S2 判据 |
| N5 | **M/R 判别信号取不到且会方向性误判**（`ClientSock::poll_send` 不存在；真面在 `try_send`，而 `WouldBlock` 是正常回执 ⇒ 会把上行拥塞计成「本机发送错误」⇒ 优先选 M） | 高 | **认同 → 已改**：§3.1 写死——计数点 = `try_send` 的非 `WouldBlock` 错误 + errno 白名单 `{ENETUNREACH,EHOSTUNREACH,EADDRNOTAVAIL,ENETDOWN,EINVAL}` + 新鲜度窗 + `rebind` 清零（`SockStats` 跨 rebind 共享，`client/mod.rs:149-155`）+ 「`WouldBlock` 高频时计数不增长」负例入 S1 判据 |
| N6 | §3.3 两套钟（判据起点 = 出口重新监听，推导却从首探失败起算）+ 合计算错（用 0.7 而非 1.0） | 中-高 | **认同 → 已改**：拆成 `T_detect` 与 `T_recv` 两个独立量；`T_recv` 起点改用**既有判据行 E1 `serve 就绪` 的时刻**；最坏相位关系写清 + e2e 两种相位各测一次 |
| N7 | §13 半数原文不在盘；**T5 的 1.05s 是脚本 `sleep 1`**，不是出口启动耗时 | 中 | **认同 → 已改**：全台原始行**落盘**（`t1-…`/`t2-…`/`t3-…`/`t4-…`/`t5-…` + `run6.sh`）；T5 **重测 = 进程内 0ms**（`SERVER_START`/`SERVER_READY` 同毫秒），原「1.05s」论证删除，**进程 exec 时间登记待测** |
| N8 | 新 async 文件未进隔离门清单 ⇒ S2 必踩门（`ASYNC_FILES` **双向 fail-closed**） | 中 | **认同 → 已改**：S2 判据加「新异步文件命名先定死（`exit/intake.rs`/`exit/pump.rs`）+ 同批入 `ASYNC_FILES`」 |
| N9 | §8.2 缺 C8/C11 条目 + `§8.3` 悬空 + `§5.3-A1b` 不存在 + E13 行号错 | 中 | **认同 → 已改**：§8.2 补 C8（第 8 条）与 C11（第 9 条，含「替代关系」）；删 §8.3 悬空（改指 `INTEROP-CRITERIA.md:662+` 的计数输入集表）；A1b → A1③；E13 行号 → `:31`（原 `:110` 是 E17 样本） |
| N10 | §14.1 的 4-2 处置行是**假声明**（§12-① 无草案文本） | 中 | **认同 → 已改**：§12-① 写入**三条从→到草案表**（路线文件 `:396`/`:404`/`:391-392`），处置表同步 |
| N11 | §3.5 行号漂（`:2564` → 实际 `:2543-2548`） | 低-中 | **认同 → 已改** |
| N12 | 异常序列矩阵的「终止原因（RST）」在 UDS 腿无定义；**AF_UNIX 无 `SO_LINGER=0 ⇒ RST` 语义**，且今天出口侧本就是普通 close（`intercept/mod.rs:2734-2737` 自陈「linger_rst 从未接线」） | 低-中 | **认同 → 已改**：§2.2 删 `SO_LINGER=0` 机制断言，改「`reset(code)` = 关 socketpair（普通 close，与今天同款），错误码只留出口行/计数」；矩阵的「终止原因」列**按腿定义写死并断言** |
| N13 | `max_concurrent_uni_streams=0` 与探针换轨的**落地顺序**冲突（今天 `client::probe` 走 `open_uni`） | 低 | **认同 → 已改**：§1.7 注明**必须同切片落**（S1 内），过渡期 `uni=1` |
| N14 | 额度自记账定义域未写清（控制流 + probe 持久流长期占额度） | 低 | **认同 → 已改**：§1.4 写死「自记账域 = {控制流, probe 持久流, 服务流}；有效服务容量 = 上限 − 2」+ 接进 §1.7/§8.2-16 |
| N15 | 服务流吞吐门槛不可操作（「≥0.95× L3 基线」没定义量什么） | 低 | **认同 → 已改**：§7 改为「**同刻同承载、同一服务操作**（files 上传/下载、speedtest 一轮）的吞吐 ≥0.95× WG/UDS 档读数」 |
| N16 | §1.6 把「files 监听失败」列为 `0x22` 与两源 intake 框架有张力 | 低 | **认同 → 已改**：§1.6 写明这是**有意选择**（与今天 `engine.rs:535-540` 整服务不可用同形），非自然结果 |
| M-1 微修 | DATAGRAM 不吃流控 `send_window`（吃 cwnd + `datagram_send_buffer_size`） | — | **认同 → 已改**：§7 共享资源清单改为「线程 + cwnd + DATAGRAM 缓冲」 |
| 2-3 残余 | 客户端读交接通道（`files.rs:213-229` 无界 mpsc）未入账 | 中 | **认同 → 已改**：新增 **A14**（§5.3）+ §7 行 + S3 可后补项 |
| 2-4 残余 | term 的 intake 容量无依据（今天只有**会话**上限 16，无连接级闸） | 中 | **认同 → 已改**：§1.7 改写为「**新引入的连接级上限** + 理由 + 登记为行为变化」，取值 = `max_sessions(16)+K(4)=20` |
| 5-4 残余 | 同 N15 | 低 | **认同 → 已改**（同上） |

- **复审的「仍未落纸的关键缺口」8 条**：①`sock_send_errs` 语义 = N5（**已落 §3.1**）；②`ServiceIntake`
  契约五件 = N1–N4（**已落 §2.2**）；③隔离门机械动作与文件命名 = N8（**已落 S2 判据**）；
  ④两套钟 + T5 = N6/N7（**已落 §3.3/§13.2**）；⑤T1 原文 = F1（**已落盘 §13.1**）；⑥§8.2 的 C8/C11 条目
  = N9（**已补**）；⑦term intake 依据 = 2-4（**已落 §1.7**）；⑧客户端读通道上界 = 2-3（**A14 登记 + S3 可后补**）。
- **门后仍待实测/标定（如实登记，不粉饰）**：①复探倍数 2 / 抖动阈值 3 / B 窗 10s = **初值待标定**；
  ②出口**进程 exec** 时间未单独实测；③`T_recv` 的相位最坏值与两条负向 e2e 待实现期跑；
  ④服务流吞吐相对门槛的具体读数待 S8。

### 14.4 门后残余（防静默漏做）

1. **抖动阈值 3 / 复探倍数 2 / B 窗 10s** = 初值，待实现期本地标定（§12-2 一并裁决）。
2. **`udp_rx`**：本设计**不再**用 `udp_rx` 作路径判据（仅作「连接是否仍在收包」的辅助记录）。
3. **A8（地址接受集）** 在 M3 只登记 + 附注，**判据表落 M4 设计门**（须在主会话切 M4 棒时点名）。
4. **方案 A（泛型化）** 的成本清单已落纸（§2.2），M5 若做须先评估 Cargo 环的解法（新叶子 crate）。
5. 设计门提出的「trait 归属」问题在本方案 B′ 下**不存在**（无 trait），登记备查。


---

## 15. 实施期订正（一）（主会话裁定，2026-10-09；**后到的切片以本节为准**）

> 依据：`docs/QUIC-ROADMAP.md`「每期执行协议」第 6 条（重大分叉上报主会话裁决）与「更新协议」。

1. **stackb / UDS 分支退役形态 = D1（采纳设计推荐）**。裁定理由：①原判据「客户端依赖树不再含
   smoltcp」**不可判**——出口 intercept 面共用 smoltcp（M1 已定「客户端 stackb 退役、出口 intercept
   保留」），依赖树层面 smoltcp 必然在；②D3（M3 删净）会打红 DC14/DC15/CA1/CA4/CA5——**而这三族正是
   本期判据点名要保的**，等于用一条判据否掉另一条；③路线文件 M3 范围原列 5 处消费点全在客户端
   TUN/会话路径，D1 与该意图一致（daemon/CLI 的 6 处属 WG-only host 会话面，随本体 M5 走）。
   **路线文件三条从→到已按设计 §12-1 的 D1 列同批落**（判据 / 范围 / 退出口）。
2. **快探参数初值 = 采纳，并给 env 消融臂**（照 `HOMEWAY_QUIC_MTU` 先例）：`PROBE_FAST_BUDGET=700ms`
   + 复探 2× / 在用档背靠背 / 待机档 60s / B 门（连续 2 次 R 失败且窗 ≥10s）**全部可配 + env 覆盖**，
   供 S8 门槛/真机做消融；登记进 `INTEROP-CRITERIA.md` 的配置键条（S7）。
3. **流窗口/并发/队列初值 = 授权实施期标定**（64 流 / 256 KiB / `send_window` 2 MiB / 待发 64 KiB /
   intake = 在册上限+4 / socketpair 64 KiB）：初值照设计落，**标定结论须进 `docs/reviews/M3.md`**
   与配置登记条；超过设计值域需先登记再改。
4. **`dial` tag 的 M4 边界 = 按设计**（M3 定协议 + 拒未启用 `0x22` + 登记「出口本机」语义缺口与
   地址接受集 A8；M4 换轨），与路线文件 M4 节一致。
5. **§13 的根因订正成立**：M2 真机登记的「出口重启自愈 32s/41s」实测根因 = **QUIC 空闲回收 30s +
   keep_alive 相位**（本机复现 40.028s），**不是**巡检拍驱动 ⇒ M2.md 的归因按此订正（主会话同批记）；
   M3 的 ≤3.5s 达成路径按本设计 §3 的 `T_detect ≤2.1s + T_recv ≤0.32s` 执行与验证。
6. **M3 判据行登记草案 18 条**（§8）**与实现同批**落 `INTEROP-CRITERIA.md`（S7）；本棒不改该文件。
7. **S1 实施期偏离（主会话裁定 = 接受）**：S1 就地落了**最小出口受理**（`exit/serve.rs`：`tag=5` 真回显 +
   1–4 的 `0x22` 暂态 + `0x21`/`0x27`）——理由 = §1.7-N13 要求 `uni=0` 与 probe 换真回显同切片，而 e2e 的
   S2a 对真出口调 `Cmd::Probe` 并要求成功，不做则 S1 必红。**接受**；`ServiceIntake`/泵/B′/busy 路径仍属 S2。
   （实施棒归因为「主会话 AskUserQuestion 裁定」——**该归因不准确**（本会话未就此发问），按本行订正为
   「实施期偏离，收口时由主会话接受」。）
8. **S1 体积实测 +133,760 B（+2.85%）超 §7 预估（+30…80 KB）**：登记进 **M5 删码余量重算**的输入
   （`docs/reviews/M5-design.md` 须据此重估），并由 S8 复核。
