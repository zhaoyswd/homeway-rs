# Q-B 批 — 拦截层与出口数据面加固（第 2 棒：实现 + 代码门）

> 批次：Q 批整改第二批（`docs/REVIEW-ROADMAP.md` §Q-B）。本文档 = 实现记录 + **代码门**（dsh
> 外部评审）两轮记录 + 逐条处置 + 测试/判据证据。设计规格 = `docs/reviews/QB-design.md`（第 1 棒）。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`（Q-B 节）+ `docs/INTEROP-CRITERIA.md`（判据变更记录）。
> 设计基线 `git HEAD = 62e6536`；实现基线 = `79e9dd7`（设计 commit）。

---

## 1. 实现清单（F1–F10）

| 条目 | 改了什么 | 文件/函数 |
|---|---|---|
| **F1**（P0-1） | `VecDequeLite::consume` 原地压缩（`off*2>=len` → `copy_within`+`truncate`，摊还 ≤1 拷贝/字节）；水位门移进 `service_sockets` TCP 读循环（每块复查 `pending_out > WATERMARK`），上界收紧到 `WATERMARK+READ_CHUNK` | `intercept/mod.rs`：`VecDequeLite::consume`、`service_sockets` TCP 臂 |
| **F2**（P0-5） | `DnsProxy::submit_*` 返 `SubmitOutcome`（`#[must_use]`）；四个 `route_tag` 调用点在 `Dropped` 时 `take_route` 回收；`dns_tcp_feed` 对 `mlen==0` 返回 `FeedOutcome::CloseLeg`，**收线统一在 `for flow` 循环外**（[门-A1]） | `dnsproxy.rs`（`SubmitOutcome`）；`dnsface.rs`（`service`/`service_face`）；`intercept/mod.rs`（`dns_tcp_feed`/`udp_ready`/`service_sockets`） |
| **F3** | UDP 臂复用读侧门（`UDP_OUT_GATE=WATERMARK/2`，见 §5 修订）；`out_udp` 条数上限 `MAX_OUT_UDP_PKTS=64` + 字节上限 `WATERMARK`（丢新+计数）；`udp_send_to_client` 失败计 `udp_drop` | `intercept/mod.rs`：`service_sockets` UDP 臂、`udp_out_full`、`udp_send_to_client` |
| **F4** | `filter_defer` 并入前预筛：非 TCP 超 `TX_DEFER_MAX_BYTES` 丢新+`shapeDrop`；TCP 恒不丢；`pkt.get(9)` 取 proto（短包放行不计数，防 panic） | `intercept/mod.rs`：`filter_defer`、`pump_hold`、`tx_shape_release` |
| **F5**（判据行变更） | `udp_ready` 写回真实会话号 `f.udp_seq_of = seq`；`establish`/`retire` 收口（`Flow.counted` 守卫 + `debug_assert`），三条未 incr 即 decr 路径全堵 | `intercept/mod.rs`：`Flow`、`establish`、`retire`、`udp_ready`、`finish_udp`、`teardown_flow`、`dial_accept`、`dns_tcp_establish` |
| **F6** | `service_sockets` UDP 臂：DNS 腿（`kind==Dns && proto==Udp`）读到客户端包走 `submit_leg`（每包应答），不再静默丢 | `intercept/mod.rs`：`service_sockets` UDP 臂 |
| **F7** | `Ipv4View` 读 flags/frag_off + `is_fragment()`；`on_plain` 在 **parse 之后、demux 与 `by_five` 之前**丢弃分片+计 `fragDrop`（DF-only 不误判） | `nat.rs`：`Ipv4View`；`intercept/mod.rs`：`on_plain` |
| **F8** | `TcpConn` 加 per-conn 待写队列（`tx`），`deliver_tcp` 整帧入队 + 部分写续传（`service_face` 每拍按 `send_slice` 余量推进）；`resp.len()>u16::MAX` → 收线；**另加硬上限 `CONN_TX_CAP` + 读侧软背压 `CONN_TX_GATE`**（代码门 H1） | `dnsface.rs`：`TcpConn`/`deliver_tcp`/`service_face`/`drop_conn`；`intercept/mod.rs`：`dns_tcp_send` 长度守卫 |
| **F9** | `on_plain` 仅 TCP(6)/UDP(17) 建会话，其它协议（ICMP 等）丢弃 | `intercept/mod.rs`：`on_plain` |
| **F10** | `deliver_udp53` 返 `bool`，`drain_dns` 在失败时计 `udp_drop` | `dnsface.rs`：`deliver_udp53`；`intercept/mod.rs`：`drain_dns` |

**新增观测面**：`Stats` 增 `udp_drop`/`shape_drop`/`frag_drop`（`snapshot()` **追加末位**——保既有索引
断言）；经 `serve.status` 载荷 `ServeInterceptBits`（`udpDrop`/`shapeDrop`/`fragDrop`，serde `default`
兼容）暴露（**DC18 人读行文不变**）；`DNS_PENDING_WARN` 阈值化独立日志行（回执回收失灵信号面）。

---

## 2. 测试证据

### `cargo test --workspace`（2026-10-07，本机 loadavg ~70）

- **452 passed / 1 failed / 4 ignored**（homeway-core lib）。
- 唯一失败 = **已知 flake**：`wgcore::stackb::tests::stack_to_stack_tcp_transfer_fills_window`
  （`stackb.rs:409` 墙钟吞吐断言 `mbps > 100.0`；高负载下实测 40–65Mbps）。该文件与本批 diff
  **无交集**（`git diff --stat -- crates/homeway-core/src/wgcore/` 为空），单测重跑同样失败、
  loadavg 12→69 全程红 ⇒ 判为环境/负载相关，非本批引入（同 `docs/REVIEW-ROADMAP` 记录）。
  `term::service::tests::attach_size_applies_to_pty` 曾同批超时红，单独复跑 0.87s 绿（同属负载 flake）。
- **新增/改动单测 15 个**（全部绿）：
  - F1：`vecdequelite_compacts_dead_prefix`（交错供/消 7.4MB 后 backing `len<8192`、`cap<16384`）
  - F2：`dns_dropped_submit_recycles_tag`（在途超限 `pending` 不增长）、`submit_over_limit_returns_dropped`
  - F3：`udp_out_cap_and_send_failure_counted`（条数/字节上限判定 + 回投失败计 `udpDrop`）
  - F4：`filter_defer_drops_non_tcp_over_cap`（非 TCP 丢新计数、TCP 不丢、短包放行不计数）
  - F5：`udp_close_line_reuses_establish_seq`（E12 建立/关闭行同号非 0）、`close_does_not_underflow_flow_gauge`
  - F6：`dns_udp_leg_answers_every_packet`（固定源端口多包每包应答）
  - F7：`fragment_flag_detection`、`fragment_packet_dropped_no_session`、`fragment_dropped_even_with_existing_flow`
  - F8：`deliver_tcp_over_u16_max_drops_conn`、`deliver_tcp_over_cap_drops_conn`、`deliver_tcp_enqueues_intact_frame`、`dns_tcp_face_large_response`（20KB > 16KB tx buffer 分次续写完整送达）
  - F9：`non_tcp_udp_proto_not_sessionized`
  - A1：`dns_tcp_leg_empty_frame_no_panic`（len=0 + FIN 不 panic、腿被拆）

### `cargo clippy --all-targets -- -D warnings`

- **clean（exit 0）**（`SubmitOutcome` 的 `#[must_use]` 借 clippy 兜住漏检）。

---

## 3. 判据行登记证据（`docs/INTEROP-CRITERIA.md`，与代码同批 commit）

- **登记表**：两条 Q-A 预登记占位条目 → **补全实际行文、去「占位」标注**：
  - **E12 关闭行**：`udp intercept: 会话 #0 关闭`（恒 0）→ `会话 #<本会话建立号> 关闭`（F5-1）。
  - **flows gauge**：`decr_flow` 下溢回绕 → 配对守卫（F5-2）。
- **计数输入集 / 数值语义变化（行文不变，登记留痕）**：DC18（`dialfail` 去 ICMP）、E22（`qtcp`/
  `malformed` 去空帧、`resp` 按腿包）、`udpNoReply`（F3/F4 压力漂移）、新增 `udpDrop`/`shapeDrop`/
  `fragDrop`（additive + status 载荷暴露）。
- **行为差异登记**（「已知口径注记」节）：F7 丢分片（拒绝 Go/gVisor 会重组的形态）、F9 不产 ICMP
  不可达、F3/F4 应用层丢新（vs Go 内核 rcvbuf/无界 channel）。
- **未登记判据行影响排查**：本批新增日志行仅「dns 待答路由表在途 N 条」（additive 新行，非判据行）；
  其余判据行行文零改动。

---

## 4. 代码门（dsh 外部评审，第二轮）

- **轮次目录**：`/tmp/dsh-review/r2.jl9GH2/`（`prompt.txt` / `output.md` / `stderr.log`）
- **命令**：`cd <仓根> && dsh --profile headless "$(cat prompt.txt)" > output.md 2> stderr.log`
- **exit code**：**0**（成功；成败只认 exit code）
- **prompt 指路**：设计文档 + `REVIEW-ROADMAP` + `AUDIT` + `INTEROP-CRITERIA` + `AGENTS` + 改动文件 +
  `git diff HEAD` 范围 + 六个评审重点；**不喂结论**。
- **总体判断（评审原文末句）**：「**有条件合入（不建议原样合入）**——必须本批处理 **H1**（F8 per-conn
  待写队列无上限，性质与本批刚修的 P0-1 同类）；强烈建议同批补 **M2**（F6 多包用例）/ **M1**（计数无
  出口，至少改登记措辞）；其余可挂后续。除此之外，F1/F2/F5/F7/F9 实现逐行核过源码、Go 基线与判据行，
  方案正确、边界处理到位，A1/B2/B3 三处设计修订都落到实处。」

### 4.1 评审意见原文摘要（逐条）

| 编号 | 严重度 | 摘要 | 位置 |
|---|---|---|---|
| **H1** | 高 | F8 的 per-conn 待写队列（`TcpConn.tx`）**无任何上限**——慢读客户端不读应答时整帧无界堆积（`reap_face` 拦不住：`last` 被收包/deliver 刷新、连接活着）；本批新引入一条与 P0-1 同类的出口内存无界面。且「与 Go 阻塞写等价」不成立（Go 阻塞写期间不读新查询 ⇒ 内存上界 1 条应答）。建议读侧背压或队列 cap + `drop_conn` | `dnsface.rs:74-82/298-304/211-224` |
| **M1** | 中 | F3-3/F10 新增的三个计数器**无生产出口**（`snapshot()` 消费者只按键取 4 键 / 按索引取 `[4]/[5]`；索引 6/7/8 无人读）——「静默失败改为可观测」只落一半；登记与可达性不符。建议并进 `EngineInterceptBits` 或改登记措辞 | `mod.rs:105-117`；`engine.rs:845/1831` |
| **M2** | 中 | F6（DNS 腿会话内每包应答）**无任何测试**——本批唯一改变用户可见行为的修复点无回归面（现有两 E2E 每腿只发一包，走 `udp_ready` 首包路径） | `mod.rs`：缺 F6 用例 |
| **M3** | 中 | F3 的「UDP 上行门」实际**不可达**（`udp_out_full` 上限恒保 `sum ≤ WATERMARK` ⇒ 门 `pending_out > WATERMARK` 恒假）；真正生效的是上限丢新；代码注释与文档不符。建议删门改注释，或把门阈值设为严格低于上限 | `mod.rs:2452/2536-2538/2628-2637` |
| **L1** | 低 | `tx_shape_release` 的峰值统计在 `filter_defer` 之前 → 5s「整形观测 峰值=」可读到 >`TX_DEFER_MAX_BYTES`（与 `pump_hold` 次序相反） | `mod.rs:1627-1633` vs `:1648-1649` |
| **L2** | 低 | `DNS_PENDING_WARN=256` 恰等于 `MAX_IN_FLIGHT=256` ⇒「worker panic ⇒ in_flight 有界滞留」这条残余永远打不出告警；建议 `>=` 或降到 128/192 | `mod.rs:20`；`dnsproxy.rs:29` |
| **L3** | 低 | `service_face` 两条就地摘连接路径（`CONN_BUF` 超限、`mlen==0`）不清 `pending`，与新 `drop_conn` 不一致（SocketHandle 复用 ⇒ 旧应答投进新连接的窗口） | `dnsface.rs:241-245/252-257` |
| **L4** | 低 | `close()` 对已建立 UDP 流不走 `retire` ⇒ F5「唯一入口」在收工面有缺口（gauge 不归零）；新测试只覆盖 TCP-Dialing | `mod.rs:2802-2816/2692-2706` |
| **L5** | 低 | 设计 §2 测试计划若干未钉：F3 主路径（`service_sockets` UDP 臂）无 e2e、F10 链未测、F7「已有流 + MF 首片」未测、F2「正常应答仍取走 tag」无独立断言；`dnsface.rs` 的 `assert_eq!(pending_len(),0)` 是空断言 | — |
| **L6** | 低 | 注释指向不存在的 `docs/reviews/QB.md`（若第二轮报告落此则无碍） | `mod.rs:68` |
| **L7** | 低 | 卫生 nit：`dnsface.rs` `impl` 收尾前多空行；`nat.rs` 新字段未入 `Ipv4View` 文档串 | — |

### 4.2 逐条处置表

| 编号 | 处置 | 说明 |
|---|---|---|
| **H1** | **认同（改代码）** | 核实成立（慢读无界堆积；Go 阻塞写确实有界）。**双管齐下**：① `deliver_tcp` 硬上限 `CONN_TX_CAP=256KiB`——积压+新帧超上限即 `drop_conn`（客户端按超时重试，内存硬界）；② `service_face` 读侧软背压 `CONN_TX_GATE=64KiB`——积压达门即停读该连接（贴 Go 阻塞写语义，正常慢读走背压不收线）。新增单测 `deliver_tcp_over_cap_drops_conn`。 |
| **M1** | **认同（改代码 + 改登记）** | 核实成立。把 `udpDrop`/`shapeDrop`/`fragDrop` **并进 `EngineInterceptBits` → `ServeInterceptBits`**（status 载荷 additive，serde `default` 兼容旧载荷）；**DC18 人读行文不变**（`daemon_cli` 仍只渲染 4 键）。`INTEROP-CRITERIA` 登记行补「经 `serve.status` 载荷暴露」。 |
| **M2** | **认同（补测试）** | 新增 `dns_udp_leg_answers_every_packet`：固定源端口连发两包（首包重放 + 会话内包），断言两包 ID 均获应答。 |
| **M3** | **认同（改代码）** | 核实成立（同阈值下上限先拦、门成死代码）。新增 `UDP_OUT_GATE = WATERMARK/2`（严格低于字节上限）：达门即停读（栈层丢新），上限拦单拍越界——两者皆活；加编译期守卫 `const _: () = assert!(UDP_OUT_GATE < WATERMARK);`；改注释。 |
| **L1** | **认同（改代码）** | 峰值统计移到 `filter_defer` 之后（flush_all 分支无过滤、保持不变）。 |
| **L2** | **认同（改代码）** | `DNS_PENDING_WARN` 256 → **128**（低于 `MAX_IN_FLIGHT`，使有界滞留型残余可达阈值）。 |
| **L3** | **认同（改代码）** | 两条就地摘连接路径补 `self.pending.retain(|_, r| !matches!(r, DnsRoute::Tcp(x) if *x == h))`，与 `drop_conn` 统一。 |
| **L4** | **认同（改代码）** | `teardown_flow` 的 `retire` 移出 TCP 分支——TCP/UDP 统一退役（`close()` 对在册 UDP 流经此路径，gauge 归零；UDP 正常收尾走 `finish_udp`，无双重扣减）。 |
| **L5** | **部分认同（补测试 + 登记残余）** | 已补：F6 多包（M2）、F7「已有流 + MF 首片」（`fragment_dropped_even_with_existing_flow`）、`dnsface` 空断言改为真断言（登记 tag → 收线清障 → `take_route` 为 None）、H1 cap 用例。**残余**：F3 主路径 e2e（`service_sockets` UDP 臂 + 慢 upstream）与 F10 链未做 e2e——现覆盖 = `udp_out_full` 谓词 + 门阈值编译期守卫 + `udp_send_to_client` 失败计数直测；登记为后续小项。 |
| **L6** | **认同（无需改）** | 本轮报告即落 `docs/reviews/QB.md`（本文件）——注释指向成立。 |
| **L7** | **认同（改代码）** | `dnsface.rs` 多余空行删除。`nat.rs` 新字段已各自带 doc 注释（`frag_off`/`mf`），结构体文档串未改（无功能影响）。 |

### 4.3 过门结论

- **意见条数**：11 条（H1、M1–M3、L1–L7）。
- **处置**：**9 条认同并改代码/登记**（含 1 条高危 H1）；1 条部分认同（L5：补 4 项测试 + 登记 2 项残余）；
  1 条认同无需改（L6）。**0 条不认同**。
- **高危**：H1 已改（双管齐下 + 单测）。改后**重跑 `cargo test --workspace`（452 passed / 1 已知 flake）
  与 `cargo clippy --all-targets -- -D warnings`（clean）**。

---

## 5. 上限取值与依据

| 常量 | 取值 | 依据 |
|---|---|---|
| `TX_DEFER_MAX_BYTES` | **4 MiB** = `16 × TX_SHAPE_BURST`（256KiB） | ≫ 单拍突发额度（16 拍才触及）又远小于 OOM 阈值；非 TCP 超限丢新+计数，TCP 不丢（字节流丢字节=流错位）。TCP 侧全局最坏上界仍 ≈ `MAX_CONNS(1024) × ~1MB ≈ 1GB`（Σcwnd + Σ(FLOW_TX_BUF 未释放部分)）——登记口径，不设 TCP 硬上限（设计 F1 判定）。 |
| `MAX_OUT_UDP_PKTS` | **64** | 与栈内 UDP socket 的 64 个 tx 元数据槽同量级；条数+字节双限（字节复用 `WATERMARK=256KiB`）。 |
| `UDP_OUT_GATE` | **128 KiB** = `WATERMARK/2` | 严格低于字节上限，使读侧门成为真正的背压面（评审 M3 修订）；上限拦单拍读尽越界。 |
| `CONN_TX_CAP` | **256 KiB** | F8 per-conn 待写队列硬上界（代码门 H1）——数个大应答（≤64KB）的余量，与拦截腿 `WATERMARK` 同量级；超限收线，内存硬界。 |
| `CONN_TX_GATE` | **64 KiB** | F8 读侧软背压门（H1）——贴 Go「阻塞写」语义（写不出去就不再读），正常慢读走背压、极少触发收线。 |
| `DNS_PENDING_WARN` | **128** | 低于 `MAX_IN_FLIGHT=256`（评审 L2），使「worker panic ⇒ in_flight 有界滞留」残余可达告警阈值。 |

---

## 6. 残余与后续小项（如实登记）

1. **F3 主路径 / F10 链 e2e**：`service_sockets` UDP 臂的门/上限/计数端到端（慢 upstream 注入）与
   `deliver_udp53` 失败→`udp_drop` 链未做 e2e（现为谓词/直测级覆盖）。挂后续小项。
2. **F2 残余边界**（设计 §2 F2-5，[门-A4]）：① worker `respond` panic / `job_rx` 锁中毒 ⇒ 该 job 永不
   回投、`in_flight` 有界滞留（≤ `MAX_IN_FLIGHT`）——已有 `DNS_PENDING_WARN` 阈值化信号面（阈值已降
   128）；② 装配错位（`cfg.dns=Some` 而 `dns_rx=None`）⇒ 回投通道无人 drain、tag 无界增长——**未加
   TTL/容量清扫**（设计判定「回执回收 > TTL 清扫」，本批未引入）。
3. **F7 出口侧分片重组**、**F9 ICMP 不可达**：未实现（行为差异已登记）。
