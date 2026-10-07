# Q-B 设计文档 — 拦截层与出口数据面加固

> 批次：Q 批整改第二批（`docs/REVIEW-ROADMAP.md` §Q-B）。第 1 棒（设计）产出。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`（Q-B 节）+ `docs/INTEROP-CRITERIA.md`（判据行政策）。
> 基线：`git HEAD = 62e6536`。行号均为复验时（本 HEAD）实测值，实现时以符号定位为准。
> **边界**：本文档只设计，不含产品代码改动。
> **状态**：设计门（dsh 外部评审）已过——见 §4；本文档已按评审意见修订（v2）。

---

## 0. 复验方法

逐条回源码重定位：`VecDequeLite` 结构/消费/压入、DNS 路由表登记/回收、UDP 双方向读写、
流计数 incr/decr 配对、IPv4 解析分片字段、DNS-over-TCP 写回路径。凡引用 `baseline/homeway`
处（Go 只读 oracle）一并核对。标注 🔎 的条目（P2 性能面）按性能问题复核，不涉及真伪争议。

---

## 1. 复验结果表

| 条目 | 真伪 | 现行源码位置（HEAD 62e6536） | 结论 |
|---|---|---|---|
| **P0-1** `VecDequeLite` 死前缀无界 | ✅ 成立 | 结构 `intercept/mod.rs:136-166`；`push` `:146-152`；`consume` `:159-165`；消费点 `flush_out` `:1954`、`discard_out` `:1985`；压入点 `service_sockets` TCP `:2301`；水位门 `:2265`（`pending_out` `:2370-2379`） | `consume` 只 `off += n`，仅 `off == buf.len()` 恰逢其时才 `clear`；`push` 仅在「已空且恰逢」时清。上游持续供给（`push`）与部分写（`send` 返 `n < rem`）交错时 `off` 难命中 `buf.len()`，死前缀 `buf[..off]` 永不回收 ⇒ backing `Vec` 随累计传输量线性增长。水位只约束 `remaining()`（= `buf[off..]`），不管死前缀。**成立**。 |
| **P0-5** DNS 待答路由表丢弃路径泄漏 | ✅ 成立 | 登记 `dnsface.rs:149-154`（`route_tag`）、取走 `:157-159`（`take_route`）、清理 `:270-284`（`reap`，仅 TCP 连接回收时）；丢弃路径 `dnsproxy.rs:459-473`（`submit_leg_impl`）、`:475-494`（`submit`）；放大面 `intercept/mod.rs:1032-1058`（`dns_tcp_feed` 对 `mlen==0` 空帧照常 `submit_tcp`） | `route_tag` 无条件登记进 `pending`；`submit*` 在 in_flight 超限或 `try_send` 失败两条路径**直接 return、无回执**，`pending` 项永不回收。`route_tag` 的全部四个调用点（`dnsface.rs:166` Udp53、`:235` Tcp、`mod.rs:1053` TcpFlow、`mod.rs:1354` UdpFlow）都受影响。**成立**（放大面额外证实：`dns_tcp_feed` 未与隧道面 `decode_tcp_frame` 同口径对 `mlen==0` 收线）。 |
| **P1** UDP 上行门缺失 | ⚠️ 部分成立 | `gated` 计算 `:2265`、仅 TCP 臂使用 `:2281`；UDP 臂 `:2331-2364`（`push_back` `:2353`）；`read_upstream` UDP 臂 `:1830-1855`；`udp_send_to_client` `:1389-1400`（`send_slice` `:1396`） | **成立部分**：`service_sockets` 的 `Proto::Udp` 臂确实无条件读入 `out_udp`（栈→upstream 方向缺门），`udp_send_to_client` 的 `let _ = send_slice` 失败静默无计数，均成立。**修正**：审计把 `read_upstream`（`:1830-1855`）并列为「无条件读入 `push_back(to_vec())`」不准确——该臂是 upstream→client 方向，走 `udp_send_to_client`，**无 `push_back`**；其「无应用层预算」是设计口径（内核 rcvbuf 界定，reactor-design §四/R-11），不是缺陷。缺门的是 `service_sockets` 的栈→upstream 臂。 |
| **P1** `tx_deferred` 无上限 | ✅ 成立 | 注释 `:315-324`（「不设显式上限」）；`pump_hold` 并入路径 `:1468-1485`；`tx_shape_release` `:1492-1537` | 「滞留深度 ≤ Σcwnd 自钳制」仅对 TCP 成立；UDP/DNS/ICMP 无窗记账，ring 高水位 hold 期内存按 (到达−物理) 速率累积。**成立**。 |
| **P1** `udp_seq_of` 恒 0 | ✅ 成立 | 声明 `:660`；初始化 `:1114`（唯一写点 = 0）；读取 `:2239`（`finish_udp`）→ 关闭行 `:2245-2248` | 全仓 `udp_seq_of` 无赋值（`grep` 仅 660/1114/2239 三处）。E12 关闭行恒 `#0`。**成立**。 |
| **P1** `decr_flow` 下溢 | ✅ 成立 | `incr_flow` `:487-489`，调用点 `:1016`（DNS TCP 腿）、`:2078`（dial_accept TCP）、`:1336`（udp_ready）；`decr_flow` `:490-492`，调用点 `:2240`（finish_udp）、`:2439`（teardown_flow，仅 TCP） | 未 incr 即 decr 的路径：①`dns_tcp_establish` listen 失败 `:1000-1003` → `teardown_flow`（incr 在 `:1016`，之后）；②`dial_accept` listen 失败 `:2058-2062` → `teardown_flow`（incr 在 `:2078`，之后）；③**新增发现**：`close()` `:2554-2558` 对**全部**在册流（含 Dialing 未 incr 的 TCP 流）调 `teardown_flow` → decr。`fetch_sub` on `AtomicU64` ⇒ gauge 回绕。**成立**。 |
| **P1** DNS 腿只答会话首包 | ✅ 成立 | `udp_ready` `:1345-1360`（仅重放 `replays`）；`service_sockets` UDP 臂 `:2331-2364`（注释 `:2332-2334` 自认 `io=None` 静默丢）；Go `baseline/homeway/pkg/intercept/dnsleg.go:29-42` | Go `dnsLeg.Write` **每包**调 `ans.Answer(p)`；Rust 侧 DNS 腿仅答 `Dialing` 期缓存的首包，会话内后续包进 `service_sockets` 后因 `io=None` 静默丢。手机 stub resolver 固定源端口重传 ⇒ 10s 窗口内全黑洞。**成立**。 |
| **P1** IPv4 分片未处理 | ✅ 成立 | `nat.rs:34-96`（`Ipv4View::parse` 不读 flags/分片偏移）；`rewrite_dst` `nat.rs:184-194`；入口 `on_plain` `:853-924` | `parse` 未读 IP 头 bytes 6-7（flags + frag offset）。非首片（`frag_off>0`）被当独立会话：`body[0..4]` 被读成端口（实为载荷），`rewrite_dst` 往 `ihl+2..ihl+4` 写端口/校验和 = 污染分片载荷。>1280 的 UDP 被分片后吃满 4096 会话表。**成立**。 |
| **P1** DNS over TCP 写回两段非原子 | ✅ 成立 | `dnsface.rs:251-260`（`deliver_tcp`）；同类 `mod.rs:1377-1386`（`dns_tcp_send`，长度域） | `deliver_tcp` 先写 2B 长度前缀再写正文、两次 `let _ =`、`can_send()` 只判「非满」（不保证容纳整帧）；短写 ⇒ 流错位到 30s 收线。`resp.len() as u16`（`:257`、`:1382`）> 65535 回绕。**成立**（`deliver_udp53` `:242-246` 单次写、无此问题，但见 E2）。 |
| **P2** 热路径 64KB 栈缓冲/双拷贝/每拍 Vec | 🔎 成立 | `:1833`、`:2287`、`:2342`（`[0u8; 65536]` 每数据报零初始化）；`:2254`（`flows.keys().collect()` 每拍全表拷贝）；`:938-943`/`:1286-1291`（新流判定全表 `count`） | 逐条位置核对无误。**性能面**，归 Q-I。 |
| **P2** 非 TCP 协议一律当 UDP | ✅ 成立 | `:883`（`let proto = if v.proto == 6 { Proto::Tcp } else { Proto::Udp };`） | Go 仅 `SetTransportProtocolHandler` 注册 TCP/UDP（`baseline/.../intercept.go:182/186`），其它协议交 netstack 兜底；Rust 侧把 ICMP 等一律当 UDP：端口恒 0（`Ipv4View::parse` 的 `_ => (0,0,0,0,0,0)`），macOS 上每包 `socket/bind/connect` + `dialfail` 计数噪声 + 一条日志降噪表项。**成立**。（措辞订正见 E1：**非「占表」**——UDP 拨号失败走 `on_dial_failed` → `remove_flow` `:2167` 同调用内摘除，不持续占表。） |

### 误报 / 措辞订正记录

- **P1 UDP 上行门**：审计把 `read_upstream` UDP 臂列为「无条件读入 `push_back(to_vec())`」不准确（该臂无 `push_back`，是 upstream→client 方向，无应用层预算属设计口径）。**保留 `service_sockets` 缺门 + `udp_send_to_client` 静默失败两条**，订正 `read_upstream` 的措辞。
- **P2 非 TCP 当 UDP**：审计「占表污染」子句不成立（拨号失败同调用内 `remove_flow`，不留存 60s）。真实代价 = 每包一次 socket/bind/connect + `dialfail` 噪声。核心结论（全非 TCP 当 UDP、端口恒 0、每包 dialfail）成立。
- 无整条误报剔除（本批 P0/P1 全部成立）。

---

## 2. 修复清单

> 每条：方案 / 涉及文件 / 风险 / 测试计划 / 判据行影响。v2 已并入设计门意见（标注 `[门-Ax]` 等）。

### F1（P0-1）`VecDequeLite` 死前缀回收

- **方案**：保留 `Vec` + 前缀偏移结构（**不换 `VecDeque<u8>`**），在 `consume` 内做**原地压缩**：
  - `off += n` 后：若 `off >= buf.len()` → `clear` + `off=0`（现有）；
  - 否则若 `off * 2 >= buf.len()` → `buf.copy_within(off.., 0)` + `buf.truncate(buf.len()-off)` + `off=0`。
  - 不变量：压缩阈值下 `off < remaining` ⇒ `buf.len() = off + remaining < 2 * remaining`，摊还 ≤1 次拷贝/字节（`copy_within` 无分配）。
  - **有界性（`[门-A2]` 订正）**：现有水位门（`:2265`）在**读循环之前只判一次**，而读循环会把栈 socket 里已缓冲的全部读光，上界 = `FLOW_BUF`（256KB，非单读块 64KB）⇒ `remaining ≤ WATERMARK + FLOW_BUF = 512KB`，`buf.len() < 1MB/流`；全局最坏 ≈ `MAX_CONNS(1024) × 1MB ≈ 1GB`（与既有 `FLOW_TX_BUF=1MB/流` 的栈缓冲同量级，非新问题）。**收紧动作**：把门判移进读循环（每读一块后复查 `pending_out > WATERMARK`）⇒ 界收紧到 `WATERMARK + READ_CHUNK = 320KB`，`buf.len() < 640KB/流`；该改动不改变任何 wire 语义（只改每拍从栈读多少）。
  - **不换 `VecDeque<u8>` 的理由（`[门-A3]` 订正）**：真正的理由是「`as_slices` 需两次 `send`（多一次 syscall）、`make_contiguous` 是 O(n) 且需 `&mut`——与 `copy_within` 同价但改动面更大」；**不是**「换型会改变 wire 字节」（TCP 是字节流，拆两次 `send` 不改字节，此条订正）。结论「压缩优于换型」成立。
  - **不加 `out_tcp` 硬字节上限**：TCP 是字节流，超限丢字节 = 流错位（比内存增长更严重）；现有读门钳住 `remaining`、压缩后总量有界。**判定：不设 TCP 硬上限**（UDP 侧另设，见 F3）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`（`VecDequeLite::consume`；`service_sockets` TCP 读循环的门位置）。
- **风险**：低。压缩不改变字节内容与顺序，仅回收前缀；`is_empty()`（`off >= buf.len()`）、`remaining()` 语义不变。需回归「部分写续传」路径（`flush_out` `:1954`）。
- **测试计划**：① 单测：`push` + 部分 `consume` 交错序列，断言 `buf.len() < 2*remaining` 且 `remaining()` 内容正确（现有 `interests_for` 单测 `:3097-3135` 可扩）；② 复现实验化：饱和供/消下传输 N MB，断言 backing `capacity` 不随累计传输量增长（对齐审计 128MB 复现）；③ 回归 `udp_session_end_to_end`/`exempt_flow_end_to_end`。
- **判据行影响**：无（wire 字节不变）。

### F2（P0-5）DNS 待答路由表丢弃路径回收 + 空帧收线

- **方案**：
  1. `DnsProxy::submit_*`（`submit`/`submit_leg_impl`/`submit_udp`/`submit_tcp`/`submit_leg`）**返回 `SubmitOutcome`**（`Accepted`/`Dropped`；`[门-A5/D2]` 不用裸 `bool`，标 `#[must_use]` 以借 `clippy -D warnings` 兜住漏检）。保持现有 `dropped` 计数不变。
  2. 全部 `route_tag` 调用点在返回 `Dropped` 时**回收 tag**（`faces.take_route(tag)`）：`dnsface.rs:166`（`service` Udp53）、`:235`（`service_face` Tcp）、`mod.rs:1053`（`dns_tcp_feed`）、`mod.rs:1354`（`udp_ready`）。
  3. `dns_tcp_feed` 对 `mlen == 0` 与隧道面（`dnsface.rs:41-50` `decode_tcp_frame` + `:217-237` 空帧判异常）**同口径收线**。**`[门-A1/B1 高]` 收线不得在 `dns_tcp_feed` 内直接 `teardown_flow`**：`dns_tcp_feed` 由 `service_sockets` TCP 臂在**同一次迭代内**调用（`:2305-2308`），之后该臂仍用快照 `h` 取 socket（`:2320-2325` CloseWait 分支）；`teardown_flow → remove_flow → sockets.remove(h)`（`:2453-2462`）后再 `get_mut(h)` 会 **panic**（smoltcp `socket_set.rs:116`）——触发成本仅「2B 空帧 + FIN」。**改为**：`dns_tcp_feed` 返回 `enum FeedOutcome { Ok, CloseLeg }`（或 `bool`），由 `service_sockets` 把待拆流收进 `Vec<u64>`，在 `for flow` 循环**外**统一 `teardown_flow`。
  4. **不引入** `pending` 容量/TTL 清扫：回执回收已堵住全部已知泄漏源（应答必回投；`drain_dns` `:1683-1686` 先 `take_route` 再判 `resp=None`，畸形回包也回收）。**判定：回执回收 > TTL 清扫**。
  5. **残余边界（`[门-A4]` 显式登记）**：① worker 内 `respond` panic 或 `job_rx` 锁中毒 ⇒ 该 job 永不回投、`in_flight` 永久滞留（有界于 `MAX_IN_FLIGHT=256`，最终走丢弃路径），tag 泄漏有界但 DNS 静默失能且**不可观测**；② 装配错位（`cfg.dns=Some` 而 `dns_rx=None`）时回投通道无人 drain ⇒ tag 无界增长。**动作**：既有 5s 观测行或 `serve.status` 加 `pending.len()`（additive、不改行文）作为「回执回收失灵」的信号面；两边界写入残余清单。
- **涉及文件**：`crates/homeway-core/src/server/intercept/dnsface.rs`、`crates/homeway-core/src/server/dnsproxy.rs`、`crates/homeway-core/src/server/intercept/mod.rs`。
- **风险**：中。`submit_*` 签名变更波及所有调用点（`#[must_use]` 兜底）；空帧收线改变 DNS TCP 腿对畸形帧的行为（对齐 Go），且收线时序须按 A1 落在循环外。
- **测试计划**：① 单测：`submit` 在 in_flight 超限 / `try_send` 失败时返 `Dropped`；② 单测：`route_tag` → 丢弃 → 断言 `pending.len()` 不增长；③ 单测（`[门-A1]`）：喂 `len=0` 帧 + 同段 FIN → **不 panic**、腿被拆、`pending` 不增长；④ 回归正常应答仍取走 tag。
- **判据行影响**：无行文变更；**计数输入集变化**——收线后空帧不再进 `submit_tcp` ⇒ `qtcp`/`malformed` 不再被空帧抬高，**与 Go 对齐**（Go `server.go:294-300` 读失败即 `return`、不计 qtcp），见 §3。

### F3（P1）UDP 上行门 + `out_udp` 上限 + 回投失败计数

- **方案**：
  1. `service_sockets` 的 `Proto::Udp` 臂读取前加**同款门**（复用 `:2265` 的 `gated`）：`gated` 时不读（栈 rx 缓冲填满 → 栈层丢新，天然「丢新」语义）。`[门-B7]` 补两句：① `pending_out` 对 `io=None`（DNS 腿）恒 0 ⇒ 门对 DNS 腿天然不生效（F6 恰好同臂，勿误伤）；② 单拍读循环可把栈内 64 槽/64KB 全取走 ⇒ 真实上界 = `WATERMARK + 单拍上界`，字节上限（=WATERMARK）与门同阈值时先被门拦，上限的价值是拦单拍溢出。
  2. `out_udp` 加**条数上限**（常量，如 `MAX_OUT_UDP_PKTS`）+ 复用 `WATERMARK` 作字节上限：超限时 `push_back` 改**丢新 + 计数**（不阻塞、不排队）。
  3. `udp_send_to_client`（`:1396`）`send_slice` 失败（栈 tx 满）改为**计数**（现 `let _ =` 静默）。
  4. 计数载体：`Stats` 增 `udp_drop: AtomicU64` + `incr_udp_drop()`，并入 `snapshot()`（**追加末位**，保 `snapshot()[2] == flows` 既有索引断言 `:2716/2791/3196/3463`）。可选：串到 `EngineInterceptBits`/`ServeInterceptBits`（status 面，additive）——**独立小项**。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`；可选 `crates/homeway-core/src/server/engine.rs`、`crates/homeway-cli/src/unified_cli.rs`、`crates/homeway-core/src/daemon/proto.rs`。
- **风险**：中低。门引入后高负载 UDP 场景丢包率上升（设计预期：UDP 丢新优于无界内存）；`read_upstream` UDP 臂不动（设计口径）。**观测漂移**：压力下 `udpNoReply` 上升，见 §3-C4。
- **测试计划**：① 单测：注入慢 upstream（不读）+ 持续 UDP 上行，断言 `out_udp` 有界且 `udp_drop` 增长；② 单测：`udp_send_to_client` 在栈 tx 满时计数；③ 回归 `udp_session_end_to_end`。
- **判据行影响**：无行文变更（`rejected` 语义不变，新增 `udp_drop` 独立计数）；`udpNoReply` 数值语义漂移见 §3。

### F4（P1）`tx_deferred` 字节上限（无窗流丢新）

- **方案**：`tx_deferred_bytes` 加常量上限 `TX_DEFER_MAX_BYTES`。并入（`pump_hold` `:1468-1485` 与 `tx_shape_release` 并入路径）时按内层 IP 头 proto 区分——
  - **TCP（proto 6）**：保持不丢；
  - **非 TCP（UDP/DNS/ICMP 等）**：并入会超上限则**丢新 + 计数**。
  - `[门-B3]` 取 proto 用 **`pkt.get(9)`** 而非 `pkt[9]`：`tx_out` 允许非 IPv4/短包（`on_tx` parse 失败 `:1708-1710` 原样 push；测试 `:3601-3610` push 8B vec），裸索引会 panic。**策略**：取不到 proto（短包）的包**放行不计数**（不属数据面）。明确上限判定在「并入前预筛 `produce`」逐包判，丢弃时同步更新 `tx_deferred_bytes` 与峰值计数。
  - `[门-B4]` **TCP 侧上界写清**：滞留 = Σcwnd + Σ（`FLOW_TX_BUF` 中已产出未上线部分）≤ `MAX_CONNS(1024) × ~1MB ≈ 1GB`（有界；比改动前无界好，但仍是全局最坏量级，需在文档给数）。若不可接受，TCP 侧改用「暂缓 `service_sockets` 读端」而非丢字节（保持不丢 TCP 原则）——本轮取「不丢 + 登记上界」。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`（`pump_hold`/`tx_shape_release` + 注释 `:315-324` 更新：禁区指 TCP，UDP 丢新可接受）。
- **风险**：中。上限取值需 ≫ 单拍 burst 且 ≪ OOM 阈值（建议 `≥ 16 × TX_SHAPE_BURST`，实现门定值）；短包策略需写死。
- **测试计划**：① 单测：`pump_hold` 连续并入非 TCP 包超上限 → 丢新 + 计数 + TCP 包不受影响；② 单测：短包（<10B）不 panic、放行；③ 现有 `tx_deferred` 单测（`:3600-3660`、`:3751-3810`）回归。
- **判据行影响**：无（新增独立计数）。

### F5（P1）`udp_seq_of` 赋值 + `decr_flow` 配对守卫（**判据行变更**）

- **方案**：
  1. `udp_ready`（`:1334-1336` 附近）在 `self.udp_seq += 1; let seq = self.udp_seq;` 后 `f.udp_seq_of = seq;`。关闭行 `:2245-2248` 即打真实会话号。
  2. `[门-D1]` 不裸加 `bool`：把「incr 与进入 Established」折进一个 `fn establish(&mut self, flow, sock)`（同时写 `phase=Established`、`sock`、`incr_flow`），`decr` 收口到 `fn retire(&mut self, flow)`；守卫状态由状态转移伴生（`debug_assert` 兜底）。语义不变：gauge 仍 = 已建立会话数。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`（`Flow`、`udp_ready`、`dial_accept`、`dns_tcp_establish`、`finish_udp`、`teardown_flow`）。
- **风险**：低。守卫不改计数语义；需覆盖 `close()` 期在途 Dialing 流（`:2554-2558`）。
- **测试计划**：① 单测：listen 失败注入（或 `close()` 于 Dialing 期）→ 断言 `flows` gauge 不回绕；② 单测：E12 建立行 `:1338` 与关闭行 `:2246` 同号；③ 回归 `udp_session_end_to_end`（`:3463`）。
- **判据行影响**：**E12 关闭行 + flows gauge**——见 §3（两条已预登记）。

### F6（P1）DNS 腿会话内每包应答

- **方案**：`service_sockets` 的 `Proto::Udp` 臂中，当 `f.kind == Kind::Dns && f.proto == Proto::Udp`（`[门-B5]` **不用 `io.is_none()`**——`io=None` 只是当前实现下恰好等价、非不变量，且对 TCP DNS 腿也成立；与同文件 TCP 臂既有判别口径 `:2276-2280` 一致）时，不再静默丢——对每个收到的客户端数据报走 `route_tag(DnsRoute::UdpFlow(flow))` + `dns.submit_leg(tag, data)`（与 `udp_ready` 首包同路径；丢弃时按 F2 回收 tag）。对齐 Go `dnsleg.Write` 每包应答。测试计划补一句 `last_active` 刷新确认（现由 `on_plain:916` 覆盖）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`（`service_sockets` UDP 臂 + 注释 `:2332-2334`）。
- **风险**：中低。改变 DNS 腿行为（答更多包）；`submit_leg` 不计 `q`（既有口径，保持）。**观测漂移**：`resp` 按腿包增长（对齐 Go），见 §3-C2。
- **测试计划**：① 单测：DNS 腿连续发多包（固定源端口）→ 每包均获应答；② 回归 `udp_session_end_to_end`。
- **判据行影响**：无行文变更（`submit_leg` 不计 `q`）；`resp` 计数输入集变化见 §3。

### F7（P1）IPv4 分片识别丢弃 + 计数

- **方案**：`Ipv4View` 增读 IP 头 bytes 6-7：`mf = flags & 0x2000 != 0`、`frag_off = flags & 0x1FFF`，暴露 `is_fragment()`（`frag_off != 0 || mf`；`[门-B6]` `*8` 无量纲意义可省）。`[门-B6]` **插入位置写死**：`on_plain` 中 **parse 之后、`served_ports` demux（`:876-882`）与 `by_five` 查表（`:885-919`）之前**——分片与正常包共享五元组，若落在查表之后，非首片/首片会被 `rewrite_dst` 污染并注入。命中 `is_fragment()` → **丢弃 + 计数**（`Stats` 增 `frag_drop` 或复用限频日志），不进会话创建。DF-only（flags=0x4000、off=0、MF=0）**不**算分片，照常处理。
- **涉及文件**：`crates/homeway-core/src/server/intercept/nat.rs`（`Ipv4View::parse`）、`crates/homeway-core/src/server/intercept/mod.rs`（`on_plain`）。
- **风险**：低。丢弃分片会拒绝 >1280 的 UDP（客户端按不可达/超时重试）；出口侧重组另议。DF-only 误判需单测守住。**行为差异登记**（见 C5）：丢分片 = 拒绝 Go/gVisor 会重组的形态。
- **测试计划**：① 单测：非首片/MF 首片 → `is_fragment()` 为真、被 `on_plain` 丢弃、不建会话；② 单测（`[门-B6]`）：已有流存在时喂 MF 首片 → 不重写、不注入；③ 单测：DF-only 不误判；④ 回归 `udp_rewrite_and_checksum`。
- **判据行影响**：无（新增丢弃计数/日志）。

### F8（P1）DNS over TCP 写回：字节流不丢不乱

- **方案（`[门-B2 中偏高]` 重写）**：`deliver_tcp`（`dnsface.rs:251-260`）原设计「余量不足即整体丢帧」在隧道 TCP 面 **tx buffer = 16KB**（`dnsface.rs:88-91`）下是**必然失败**——任何 `resp.len() > 16KB-2` 的应答每次都整体丢（客户端重试同尺寸 ⇒ 永久失败），把「错位」换成了「丢帧」，未解决根因（大应答送不出）。Go 侧 `net.Conn.Write` 会阻塞到窗口放开、最终送达。**改为**：在 `TcpConn`（`dnsface.rs:74-78`）加 per-conn 待写字节队列（`Vec<u8>` + 已写偏移），`deliver_tcp` 把「2B BE 长度 + 正文」**整帧**入队，`service_face` 每拍按 `send_capacity() - send_queue()` 续写、帧边界天然保持——与拦截腿既有 `tx_backlog` + `flush_backlog`（`mod.rs:2174-2193`）完全同构，与 Go 阻塞写等价。长度钳制保留（长度域 u16 是协议事实）：`resp.len() > u16::MAX` → **收线**（对齐 Go：`writeTCPMessage` 返错 → `ServeStream` 返回 → 连接关闭，而非 F8 原设计的「丢帧留连接」）。`dns_tcp_send`（`mod.rs:1377-1386`）走 `tx_backlog` 字节流、部分写余量留住，无错位问题，仅加长度守卫。
- **涉及文件**：`crates/homeway-core/src/server/intercept/dnsface.rs`（`TcpConn`/`deliver_tcp`/`service_face`）、`crates/homeway-core/src/server/intercept/mod.rs`（`dns_tcp_send` 长度守卫）。
- **风险**：中低。整体丢帧 → 字节流续写是改进；需确认 smoltcp 0.14 `TcpSocket::send_capacity`/`send_queue` 可用；per-conn 队列需随 `reap_face`/`close_face` 释放。
- **测试计划**：① 单测：`resp.len()` 略大于 16KB → 分多次续写、最终完整送达（无错位）；② 单测：`resp.len() > 65535` → 收线；③ 回归 DNS-over-TCP 现有测试面。
- **判据行影响**：无行文变更；`resp.len() > u16::MAX` 收线时连接关闭（与 Go 一致），可能触发既有 TCP DNS 关闭行——非新行文。

### F9（P2）非 TCP 协议不再当 UDP

- **方案**：`on_plain`（`:883`）仅对 `proto == 6`（TCP）/`proto == 17`（UDP）建会话；其它协议（ICMP 等）**丢弃**（不再走 `udp_new` 建端口 0 会话）。是否补 ICMP 不可达对齐 Go netstack 兜底：**本轮不做**（`build_icmp_unreachable` 现仅对 UDP 生效，扩展面另议）。**收益（`[门-E1]` 订正）**：移除每包 `socket/bind/connect` 开销与 `dialfail` 计数噪声（**非**「移除占表」）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`（`on_plain`）。
- **风险**：低。**行为差异登记**（见 C5）：不产出 ICMP 不可达 = Go netstack 会回。`dialfail` 输入集变化见 §3-C3。
- **测试计划**：① 单测：喂 ICMP 包 → 不建会话、`flows`/`dialfail` 不增长；② 回归 TCP/UDP 建会话。
- **判据行影响**：无行文变更（`dialfail` 是计数器非判据行）；数值语义修正见 §3。

### F10（P1，`[门-E2]` 顺带）`deliver_udp53` 静默失败计数

- **方案**：`dnsface.rs:242-246` 的 `let _ = send_slice` 加失败计数（与 F3-3 同族，复用 `udp_drop`）。本批正在改 `dnsface.rs`（F2/F8）且正给同类静默失败加计数，**顺带认领**（AUDIT 原挂 Q-E P2，但 `REVIEW-ROADMAP` 的 Q-B/Q-E 范围清单都没列它，两批都有漏掉风险）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/dnsface.rs`。
- **风险**：低。
- **测试计划**：单测：UDP 面 tx 满 → 计数（并入 F3 测试）。
- **判据行影响**：无。

### P2 性能面（🔎）——**移交 Q-I**

`:1833`/`:2287`/`:2342`（64KB 栈缓冲零初始化）、`:2254`（`flows.keys().collect()` 每拍拷贝）、`:938-943`/`:1286-1291`（新流全表 `count`）确认成立，属性能面。**本批不改**：与 Q-I「热路径缓冲复用」同文件面，按 `REVIEW-ROADMAP.md` 依赖条款「Q-I 触 Q-B 刚修的热路径 → 排最后」串行，避免同文件冲突与重复改动。登记为 Q-I 输入。

---

## 3. 判据行影响清单

**政策**（`INTEROP-CRITERIA.md` §判据变更记录）：判据行变更须登记（日期/条目/从→到/原因/影响面）并与代码变更同批 commit。

| 判据行 | 是否变更 | 本批动作 |
|---|---|---|
| **E12 关闭行**（`udp intercept: 会话 #%d … 关闭`） | **变更** | 由恒 `#0` → 真实会话号（F5-1）。**已在 Q-A 预登记占位**（`INTEROP-CRITERIA.md:321`），本批实现时**补全实际行文、去「占位」标注**，同批 commit。 |
| **flows gauge**（`intercept` stats / `serve.status`） | **变更** | `decr_flow` 下溢修复（F5-2）：gauge 不再回绕。**已在 Q-A 预登记占位**（`INTEROP-CRITERIA.md:322`），同批补全登记。 |
| **DC18**（`intercept：dialOk=N dialFail=N reject=N flows=N`，`INTEROP-CRITERIA.md:237`） | **数值语义修正，行文不变** | F9：ICMP 不再计入 `dialfail`。`[门-C3]` 与 flows gauge 同属「`serve.status` 面、行文不变数值变」形态，须在本表列明。**取口径**：计数器输入集变化**不属判据行变更**（行文未动），但**必须在本节列明**——本条即登记。 |
| **E12 建立行**（`udp intercept: 会话 #%d %s 建立`） | 不变 | `udp_ready` 行文与次序不动。 |
| **E4**（`dns 代答就绪：tunnel=…:53（UDP+TCP）resolve=…`） | 不变 | F6 改腿应答行为，非就绪行。 |
| **E22 / DNS 计数行**（`q=`/`qtcp=`/`malformed=`/`resp=`） | **输入集变化，行文不变** | `[门-C1]` F2-3 收线后空帧不再进 `submit_tcp` ⇒ `qtcp`/`malformed` 不再被空帧抬高（**与 Go 对齐**，`server.go:294-300`）。`[门-C2]` F6 使 `resp` 按腿包增长（**与 Go `Answer()` 每包计数一致**，`dnsleg.go:35`）。均不改行文。 |
| **`udpNoReply`**（udpcap 实测位；`INTEROP-CRITERIA.md:119` 引用） | **数值语义漂移，行文不变** | `[门-C4]` F3（UDP 丢新）+ F4（非 TCP 滞留丢新）压力下使 `udpNoReply` 上升（`Stats::incr_udp_session` `:499-506`）。属丢包策略的观测漂移。 |
| UDP/DNS/分片/整形 新增丢弃计数（`udp_drop`/`frag_drop`/`shape_drop`） | 新增观测 | **非判据行**（新增独立计数器，不改现有行文与既有计数器语义）。 |

**未登记的判据行影响排查结论**：除上述两条已预登记项外，**行文字节层面无其它未登记影响**；**计数输入集层面**有 F2-3（`qtcp`/`malformed`）、F6（`resp`）、F9（`dialfail`/DC18）、F3/F4（`udpNoReply`）四处——均**行文不变**，按上表登记列明。

> **登记计划**：实现（第 2 棒）时，随 F5 的 commit 一并更新 `docs/INTEROP-CRITERIA.md` 登记表两行（去「占位」、补「从 → 到」实际行文）；DC18/`udpNoReply`/计数输入集变化按上表在本节留痕；**行为差异登记**（`[门-C5]`）落到 `docs/INTEROP-CRITERIA.md` 的「已知口径注记」节（F7 丢分片、F9 不产 ICMP 不可达、F3/F4 应用层丢新 vs Go 内核队列/无界 channel），给可复核的差异描述。

---

## 4. 设计门记录（dsh 外部评审）

### 4.1 轮次信息

- **轮次目录**：`/tmp/dsh-review/r1.jhb9HE/`（`prompt.txt` / `output.md` / `stderr.log`）
- **命令**：`cd <仓根> && dsh --profile headless "$(cat prompt.txt)" > output.md 2> stderr.log`
- **exit code**：**0**（成功；成败只认 exit code）
- **prompt 指路**：设计文档 + `REVIEW-ROADMAP` + `AUDIT` + `INTEROP-CRITERIA` + `AGENTS` + 五个代码面 + Go 基线；**不喂结论**。
- **总体判断（评审原文末句）**：「方案方向正确、取舍基本站得住（F1 压缩优于换型、F2 回执回收优于 TTL 清扫我都认同），可以进入实现阶段；但建议先做三处设计修订再开工——A1/B1（F2-3 空帧收线改为循环外统一收线）、B2（F8 改为 per-conn 待写队列 + 部分写续传）、B3（F4 用 `pkt.get(9)`），其余为文档/登记口径修订。」

### 4.2 评审原文摘要（逐条）

| 编号 | 严重度 | 摘要 | 位置 |
|---|---|---|---|
| A1 | 高 | F2-3 空帧就地 `teardown_flow` 会踩悬垂 `SocketHandle`：`service_sockets` 同迭代内先快照 `h` 读、再 `dns_tcp_feed`；拆腿 `sockets.remove(h)` 后 CloseWait 分支再 `get_mut(h)` → panic（2B 空帧 + FIN 即触发） | 设计 F2-3；`mod.rs:2305-2308`/`:2320-2325`/`:2458-2460` |
| A2 | 中 | F1 有界性数字失真：门只判一次、读循环读尽 ⇒ `remaining ≤ WATERMARK + FLOW_BUF = 512KB`，`len < 1MB/流`（非文档的 320KB/640KB）；建议把门移进循环 | 设计 F1；`mod.rs:2265/2284-2304` |
| A3 | 低 | F1 拒绝 `VecDeque` 的理由「会扩大 wire 回归面」不成立（拆两次 send 不改字节）；真实理由是多一次 syscall + `make_contiguous` 同价 | 设计 F1 |
| A4 | 中 | F2「回执回收堵住全部泄漏源」基本成立，但两边界未写：worker panic/锁中毒（tag 有界泄漏 + DNS 静默失能）、`dns_rx=None` 装配错位（无界）；建议登记残余 + 观测 `pending.len()` | 设计 F2-4；`dnsproxy.rs:418-425` |
| A5 | 低 | F2 的 `bool` 回执加机器门（`#[must_use]`）或 `SubmitOutcome` | 设计 F2-1 |
| B2 | 中偏高 | F8「余量不足整体丢帧」在 16KB tx buffer 下是必然失败（>16KB 应答永久送不出）；Go 是阻塞写到底。建议 per-conn 待写队列 + 部分写续传（同 `tx_backlog`/`flush_backlog`） | 设计 F8；`dnsface.rs:88-91/251-260` |
| B3 | 中 | F4 判别式 `pkt[9]` 裸索引会 panic（`tx_out` 允许短包；现有单测 8B vec） | 设计 F4；`mod.rs:1707-1710/3601-3610` |
| B4 | 中 | F4「TCP 自钳制」需给数：滞留含 `FLOW_TX_BUF` 未释放部分，最坏 ≈1GB | 设计 F4 |
| B5 | 低 | F6 用 `kind==Dns` 判别而非 `io.is_none()` | 设计 F6；`mod.rs:2276-2280` |
| B6 | 低 | F7 插入位置须写死「parse 后、demux 与 by_five 之前」；`frag_off*8` 可省 | 设计 F7；`mod.rs:876-919` |
| B7 | 低 | F3 门要写清「对 `io=None` 的 DNS 腿不生效」与单拍上界 | 设计 F3；`mod.rs:2265/2341-2357` |
| B8 | — | 溢出/下溢/部分写面看过，没发现问题 | — |
| C1 | 中 | F2-3 改 `qtcp`/`malformed` 计数输入集，§3 写「计数语义不变」不准确（实为对齐 Go） | 设计 §3；Go `server.go:294-300` |
| C2 | 中 | F6 使 `resp=` 增长，§3 漏了（对齐 Go） | 设计 §3；`dnsleg.go:35` |
| C3 | 中 | F9 的 `dialfail` 输入集变化与 flows gauge 口径应一致（列 DC18） | 设计 §3；`INTEROP-CRITERIA.md:237` |
| C4 | 中 | F3/F4 丢包使 `udpNoReply` 漂移，§3 未列；丢包策略应做行为差异登记 | 设计 §3 |
| C5 | 低 | F7/F9「接受的差异」未指登记位置 | 设计 F7/F9 |
| C6 | — | 判据行行文面看过，没发现问题（F5 两条变更已在 `:321-322` 预登记） | — |
| D1 | 低 | `Flow.counted: bool` 建议折进状态转移（`establish`/`retire` 或由 `phase` 派生 + `debug_assert`） | 设计 F5-2 |
| D2 | 低 | `submit_* -> bool` 布尔盲视（同 A5） | 设计 F2-1 |
| D3 | — | Go 直译痕迹看过，没发现问题（无多余 Arc/Mutex、无字符串错误、无接口仿写；F1/F3/F4/F7 形态地道） | — |
| E1 | 低 | P2「非 TCP 占表」子句不成立（拨号失败同调用内 `remove_flow`）；真实代价 = 每包拨号开销 + 计数噪声 | 设计 §1 P2；`mod.rs:1152-1155/2167` |
| E2 | 低 | `deliver_udp53` 静默丢没人认领（Q-B/Q-E 范围清单都没列），建议本批顺带或显式登记 | `dnsface.rs:242-246` |
| E3 | — | 其余复验条目逐条回源码核对，行号与结论与 HEAD 一致；`read_upstream` 误报订正成立 | — |
| F1 | 中（流程） | §4 设计门记录待填，须回填本轮摘要 + 处置表 | 设计 §4 |
| F2 | 低 | `snapshot()` 6→7 兼容性核实正确（唯一外部消费面按键查找；测试面按索引） | 设计 F3-4 |

### 4.3 逐条处置表

> 认同 → 已改设计（标注处）；不认同 → 给证据。

| 编号 | 处置 | 说明 |
|---|---|---|
| **A1** | **认同（改设计）** | 核实成立：`remove_flow` `:2453-2462` 确实 `sockets.remove(h)`；smoltcp `socket_set.rs:116` 空槽 `get_mut` panic。F2-3 改为 `dns_tcp_feed` 返回 `FeedOutcome`、待拆流收 `Vec<u64>` 在 `for` 循环**外**统一 `teardown_flow`；补 `len=0 + FIN` 不 panic 单测。 |
| **A2** | **认同（改设计）** | 核实成立：门 `:2265` 在读循环前只判一次，读循环读尽 `FLOW_BUF=256KB` ⇒ 界为 `WATERMARK + FLOW_BUF`。F1 数字订正为 512KB/1MB、全局 ~1GB；采纳「门移进循环」收紧到 320KB/640KB。 |
| **A3** | **认同（改措辞）** | 核实成立（TCP 字节流拆两次 send 不改字节）。F1 理由改为「多一次 syscall + `make_contiguous` O(n) 同价、改动面更大」。 |
| **A4** | **认同（改设计）** | 两边界成立（`in_flight` 有界 256 的永久滞留 + `dns_rx=None` 无界）。F2-5 显式登记残余 + 观测 `pending.len()`。 |
| **A5/D2** | **认同（改设计）** | F2-1 改 `SubmitOutcome` + `#[must_use]`（借 `clippy -D warnings` 兜漏检）。 |
| **B2** | **认同（改设计）** | 核实成立：隧道 TCP 面 tx buffer = 16KB（`dnsface.rs:89-90`），大应答必丢；Go `writeTCPMessage` 阻塞写。F8 重写为 per-conn 待写队列 + 部分写续传（同 `tx_backlog`/`flush_backlog`），`>u16::MAX` 改收线对齐 Go。 |
| **B3** | **认同（改设计）** | 核实成立：`on_tx` `:1708-1710` 原样 push 非 IPv4；测试 `:3601-3610` 推 8B vec。F4 改 `pkt.get(9)`，短包放行不计数。 |
| **B4** | **认同（改设计）** | F4 补 TCP 侧上界数（≈1GB 全局最坏）；本轮取「不丢 TCP + 登记上界」。 |
| **B5** | **认同（改设计）** | F6 改 `f.kind == Kind::Dns && f.proto == Proto::Udp`（与 `:2276-2280` 一致）。 |
| **B6** | **认同（改设计）** | F7 写死插入顺序（parse 后、demux 与 `by_five` 前）+ MF 首片单测；`frag_off*8` 省。 |
| **B7** | **认同（改设计）** | F3 补两句（DNS 腿门不生效、单拍上界）。 |
| **B8** | 认同（无需改） | 无问题。 |
| **C1** | **认同（改 §3）** | 核实成立（Go `server.go:294-300` 读失败不计 qtcp）。§3 增「F2-3 使 `qtcp`/`malformed` 输入集变化 = 对齐 Go」。 |
| **C2** | **认同（改 §3）** | 核实成立（Go 每包 `Answer` 计入 `resp`）。§3 增 `resp` 说明。 |
| **C3** | **认同（改 §3）** | §3 增 DC18 行 + 一般口径「计数器输入集变化不属判据行变更，但须列明」。 |
| **C4** | **认同（改 §3 + 登记）** | §3 增 `udpNoReply` 漂移 + 行为差异登记（含上限取值依据）。 |
| **C5** | **认同（改设计）** | F7/F9 明确登记载体（`INTEROP-CRITERIA.md`「已知口径注记」节）。 |
| **C6** | 认同（无需改） | 无问题。 |
| **D1** | **认同（改设计）** | F5-2 改 `establish`/`retire` 收口 + `debug_assert`。 |
| **D3** | 认同（无需改） | 无问题。 |
| **E1** | **认同（改措辞）** | §1 P2 行 + F9 收益描述订正（非「占表」）。 |
| **E2** | **认同（改设计，本批认领）** | 新增 F10：`deliver_udp53` 失败计数（与 F3-3 同族）。 |
| **E3** | 认同（无需改） | 复验表经评审逐条核对一致。 |
| **F1** | **认同（本节即处置）** | §4 已回填。 |
| **F2** | 认同（无需改） | snapshot 兼容判断经核实正确。 |

### 4.4 过门结论

- **意见条数**：27 条（A1-A5、B2-B8、C1-C6、D1-D3、E1-E3、F1-F2；其中 B8/C6/D3/E3 为「看过无问题」）。
- **处置**：**21 条认同并改设计**（含 3 条高/中偏高结构性修订：A1 空帧收线时序、A2 有界性论证、B2 F8 重写）；**0 条不认同**；4 条「看过无问题」无动作；F1（§4 待填）本节回填。
- **结论**：**设计门通过**。三处结构性修订（A1/B2/B3）已并入 F2/F8/F4，其余为文档/登记口径修订，均已落进 §1/§2/§3。设计可进入实现阶段（第 2 棒）。
