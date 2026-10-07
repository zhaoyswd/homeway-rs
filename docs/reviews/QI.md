# Q-I 前段（性能细节批）— 实现与代码门记录

> 批次：Q 批整改 · Q-I 前段（`docs/REVIEW-ROADMAP.md` §Q-I 前段；2026-10-08 拆前段/尾段并提前）。
> 规格真源 = `docs/reviews/QI-design.md`（v2，设计门已过，见该文档 §5）；派单 = AUDIT「Q-I 性能细节批」。
> 第 2 棒（实现）产出本文：两轮评审摘要 + 逐条处置 + 测试证据 + 性能证据 + 判据登记 + 不做项 + 残余。
> 基线 commit = `36fd197`（A 臂**二进制** sha256 `e75a55bf…`）；实现后二进制 sha256 `c040c001…`
> （评审对象版本）/ `572bb689…`（并入代码门 L1/L3 处置后的版本，release 行为等价：`debug_assert`
> 与 `Copy` 值传参不产生 release 差异）。

---

## 1. 实现清单（逐条，符号定位）

| 条 | 改了什么 | 文件 / 函数 |
|---|---|---|
| **F1** | `tx_backlog` 由 `Vec<u8>` + `drain(..w)`（尾部整搬）换 `VecDequeLite` 前缀偏移 + 摊还压缩：字段换型、初始化、DNS 腿 push、读门（`remaining().len() >= WATERMARK`）、兴趣位、cc 观测、读入 push、`flush_backlog` 的 `send_slice(remaining())` + `consume(w)`；11 个代码站点 + 1 处测试内 `len()` | `server/intercept/mod.rs`：`Flow.tx_backlog` / `dns_tcp_send` / `cc_stats_line` / `reactor_turn` / `read_upstream` / `flush_backlog` / 测试 `pump_*` |
| **F2** | 删 `consume_step` 的 `wg_buf.clear()+resize(WG_BUF,0)`（每出站包 65KB memset）；删 `wgcore::decapsulate_in` 空数据报重调循环内的同款残留（订正 2：注释与实现不符）；两处入口加 `debug_assert_eq!(wg_buf.len(), WG_BUF)` 长度不变量守卫 | `server/device.rs`（`consume_step`）、`wgcore/mod.rs`（`decapsulate_in`） |
| **F3** | ① `read_upstream` UDP/TCP 两臂读缓冲循环外提（每调用一次零初始化）；② `Interceptor` 加 `rx_scratch: Box<[u8;64KiB]>` 字段，`service_sockets` TCP/UDP 两读循环共用（`while let Ok(...)` 形态保持 Err→break 语义） | `server/intercept/mod.rs`（`read_upstream`、`service_sockets`、结构体 + `attach`） |
| **F4** | 新增 `envflag` 模块（`OnceLock` 缓存）；6 个热路径 getenv 站点全部改走缓存：`device.rs:344`、`wtransport/bind.rs:542`、`server/bind.rs:792/999/1045`（`HOMEWAY_TX_DBG`）+ `wgcore/mod.rs:731`（`HOMEWAY_WG_DEBUG`）；语义登记：环境变量**启动前**设置生效（首次读取即定型） | 新文件 `crates/homeway-core/src/envflag.rs`、`lib.rs`（模块声明）及各站点 |
| **F5-1** | `Engine` 加 `udp_rx_buf: Vec<u8>`（构造期一次分配）；`resolve_udp` 每待决 id 每拍的 `vec![0u8;65535]` 改为复用缓冲，交付走 `[..n].to_vec()`（防别名） | `wgcore/mod.rs`（`Engine` 结构体、`resolve_udp`） |
| **F6** | ① `Relay.frame_scratch` 复用编帧（直发路径 `clear+encode_frame`；等腿窗 `pend` 仍走 owned `frame_bytes`）；② assoc 读缓冲提到主循环外（局部量，避免 E0502）+ `assoc_read(pkt: &[u8])` 借用零拷贝；③ 主 UDP 读缓冲同法外提；④ 孤儿扫描 O(L×A)→O(L+A)（`HashSet` 标签聚合） | `crates/homeway-core/src/relay/mod.rs` |
| **F7** | `udpcap_loop` 等待三态显式化（`Ok(kick)`/`Timeout` 立即重探；`Disconnected` 补睡一个节拍防自旋），抽纯函数 `udpcap_disconnected_backoff`（值传参）+ 单测 | `server/engine.rs` |

**不做（按设计裁决，未实现）**：`encap_peer`/`step_of` 的 `to_vec`（F2b）· 状态快照分层与单快照微项（F8 整条）· relay sendmsg iovec · `resolve_udp` 池化（F5-3）· `resolve_pending` 64KB 复用（F5-2）· 4c/4d/6d 低频项。理由与证据见设计文档 §2/§6。

---

## 2. 测试证据

| 项 | 结果 |
|---|---|
| `cargo test --workspace` | **全绿**（homeway-core lib **482 passed / 0 failed / 4 ignored**；capi/cli 等各 crate 全绿；总 0 failed）。代码门处置（L1 `debug_assert` / L3 值传参）后**复跑仍全绿** |
| 已知 flake 甄别 | 首轮全量跑出现 `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect` FAILED——**隔离重跑 3/3 通过**，且本轮改动不触 daemon 路径 ⇒ 判为**已知 flake**（派单已列）；其后两轮全量跑（含处置后复跑）未再复现 |
| `cargo clippy --all-targets -- -D warnings` | **无告警**（首轮抓出重写后的 UDP 读循环 `while_let_loop`，已按建议改 `while let`；其后两轮干净） |
| 新增/扩展单测（9 条） | F1：`vecdequelite_matches_vec_reference_and_stays_bounded`（5000 步随机交错 push/consume 与 `Vec` 参照逐字节等价 + `len < 2*remaining` 不变量）、`tx_backlog_watermark_gate_uses_remaining`（含死前缀的满水门：`interests_for` 摘 POLLIN + `read_upstream` 一行不读；消费回门下后恢复读取）；F2：`encap_after_large_packet_no_stale_prefix`（1400B→100B：密文长度 = 明文+32 且内容正确）、`hundred_packet_roundtrip_bytes_identical`（双向各 100 包逐字节一致）；F4：`envflag::tx_dbg_first_read_cached` / `wg_debug_first_read_cached`；F6：`frame_scratch_bytes_match_frame_bytes`（4 种 FrameKind × 5 种载荷逐字节对照）；F7：`udpcap_wait_three_states`（三态） |
| 未覆盖（如实登记，代码门 L2） | F2 的 `wgcore::decapsulate_in` 站点**无 Engine 级直接测试**（既有 `two_tunn_handshake_then_queued_data` 走裸 `Tunn` 不走 Engine）——由「boringtun 契约（返回值恒为已写前缀，评审独立回读 vendored 源码核实）+ 入口 `debug_assert` 长度不变量」双保险承担；批记录登记为测试空档。F5-1 无本地判据（手机核侧，设计已声明）。F1 的端到端字节回归由既有 `intercept` 全套单测（水位/兴趣位/部分写续传/EOF 挂起/DNS 腿/过境 TCP）承担 |

---

## 3. 性能证据

### 3.1 口径（与设计 §4.1/§4.2 对齐）

- **臂**：A = HEAD（`36fd197`，二进制 sha256 `e75a55bf…`）；B = 实现后（`c040c001…`；另 `572bb689…` = 代码门处置后版本，见 §5.3）。臂切换 = 复制二进制 + **每轮**重启本地出口（`tools/local-rust-exit.sh` #9，`--bind-interface none`，端口 42659，state `/tmp/homeway-rs-rustexit-9`）+ 客户端统一进程（`/tmp/qi-client`，serve/relay 双关）。
- **负载**：`speedtest --host qi --json --down 15s --up 15s --streams 4`（设计 §4.4）。
- **交替**：A,B,A,B,A,B（设计 §4.1），各 3 轮；第 3 轮带 `sample <exit_pid> 20`。
- **CPU**：`ps -o time=` 累计差 ÷ 墙钟（**不用** `ps -o %cpu` 短窗值）；RSS：1Hz 轮询取 max。
- **loadavg 纪律**：逐轮记录表头（1/5/15）；判决只在 1min ≤ 4 的轮次作数。**所有数字均带表头与时刻**。
- **旁证集（不同 harness，只作旁证）**：03:22–03:23 的 A 臂 3 轮 = **同一出口进程连续跑**（未每轮重启），与判决集不可混算——**本轮头条数字一律用同窗交替 3+3 中位**。

### 3.2 判决集（交替 A/B 6 轮）

| 时刻 | 臂 | loadavg(1/5/15) | down MB/s | up MB/s | CPU 累计差 | 墙钟 | CPU% | s/GB | RSS max KB |
|---|---|---|---|---|---|---|---|---|---|
| 03:36:44 | **A**-ab1 | 2.65/3.06/2.77 | 89.98 | 125.50 | 44.66s | 36s | 124.1% | 13.82 | 26448 |
| 03:37:26 | **B**-ab1 | 2.97/3.11/2.80 | 101.42 | 123.50 | 44.78s | 36s | 124.4% | 13.27 | 27552 |
| 03:38:09 | **A**-ab2 | 3.17/3.16/2.84 | 96.06 | 125.78 | 44.86s | 36s | 124.6% | 13.48 | 25568 |
| 03:38:51 | **B**-ab2 | 3.64/3.29/2.90 | 96.63 | 121.85 | 44.40s | 37s | 120.0% | 13.55 | 26832 |
| 03:39:34 | **A**-ab3（带 sample） | 3.52/3.32/2.93 | 94.41 | 128.43 | 42.25s | 35s | 120.7% | 12.64 | 24240 |
| 03:40:16 | **B**-ab3（带 sample） | 3.71/3.42/2.99 | 91.50 | 126.96 | 41.54s | 35s | 118.7% | 12.68 | 28480 |

（旁证 A 臂 3 轮〔03:22:14/03:22:54/03:23:33，同 harness 但**同进程连跑**〕：down 89.05/92.30/88.80、up 126.08/127.30/125.41、CPUδ 44.82/42.07/44.48。原始逐轮见 `/tmp/qi-ab/all-results.tsv`。）

**同窗交替 3+3 中位**：A：down **94.41**、up **125.78**、CPUδ **44.66s**、s/GB **13.48**、RSS **25568 KB**；B：down **96.63**、up **123.50**、CPUδ **44.40s**、s/GB **13.55**、RSS **27552 KB**。
⇒ **B/A：down +2.4% / up −1.8% / CPUδ −0.6% / s/GB +0.5% / RSS 1.078×**。
（若错误地把旁证 A 3 轮并入取中位会得 down +6.0%——**该口径已弃用**，代码门 M2 指出并采纳。）

### 3.3 `sample` 叶帧（驱动线程 `homeway-serve-drv`，20s 窗口；A3 = 14533 样本 / B3 = 15175）

| 叶帧 | 归属 | A（HEAD） | B（实现后） | 目标 | 判定 |
|---|---|---|---|---|---|
| `[flush_backlog] → _platform_memmove` | F1 | **2620（18.03%）** | **58（0.38%）** | <3% | **绿**（−97.8%） |
| `[consume_step] → __bzero` | F2 | **393（2.70%）** | **0** | =0 | **绿** |
| `[service_sockets] → __bzero` | F3 | 166（1.14%） | **0** | — | **绿** |
| `[reactor_turn] → __bzero` | F3 | 51（0.35%） | 42（0.28%） | F3 点名帧合计 ≤0.5% | **绿**（1.49% → 0.28%） |
| `getenv`（驱动线程） | F4 | 92（0.63%） | **0** | =0 | **绿** |
| `getenv`（发送线程 `homeway-serve-tx`） | F4 | ≈44（0.30%） | **0** | =0 | **绿** |
| `step_of`（malloc/memmove） | 不做（F2b） | ≈78 | ≈55 | 不变 | 记录（未动） |

**B2 复采（04:01:50，load 2.46，`572bb689`）**：`flush_backlog→memmove` **42（0.28%）**、`consume_step→__bzero` **0**、`service_sockets→__bzero` **0**、`reactor_turn→__bzero` 36（0.24%）、`getenv` **0** ⇒ 处置后版本与 B 一致（叶帧结论可复现）。

**同步观测（解释 CPU 为何持平）**：`[reactor_turn] → poll`（每拍零超时 poll syscall）**1774（12.2%）→ 2835（18.7%）**；`encrypt_in_place_detached→chacha` **1184（8.1%）→ 2079（13.7%）**；`[DnsFaces::service] → __bzero`（`intercept/dnsface.rs:182` 每调用 64KB 零初始化）**67（0.46%）→ 443（2.92%）**（B2 复采 475/3.11%，两轮一致）。三者合计 +7.7 点 ≈ 抵掉 F1 省下的 17.6 点 ⇒ **驱动线程总忙时不变，与 ps 口径 CPU 持平自洽**（机制：驱动循环事件驱动，单拍变快 ⇒ 单位时间拍数变多 ⇒ 每拍固定税放大）。

### 3.4 逐条预期 vs 实测

| 条 | 预期（设计） | 实测 | 判定 |
|---|---|---|---|
| F1 | 叶帧 17.1% → <3% | **18.03% → 0.38%** | **绿**（超预期） |
| F2 | 叶帧 2.7% → 0 | **2.70% → 0** | **绿** |
| F3 | 点名帧合计 1.5% → ≤0.5% | **1.49% → 0.28%**（帧级达标）；**线程总 `__bzero` 4.75% → 3.34%**（含 `dnsface.rs:182` 与分配器内部等靶点外来源，**未达 ≤0.5%**——口径说明见代码门 M3 处置） | **绿（点名帧）**/总口径**登记** |
| F4 | 驱动 101+发送 52 样本 → 0 | **92+44 → 0** | **绿** |
| F5-1 | 结构性（无本地判据） | 代码级 + 单测；alloc 站点消失 | **绿（结构）**，数字为估算 |
| F6 | 中继叶帧 + 吞吐不回归 | **已跑中继臂**（§3.6）：1 流 A down 0.4915 / B 0.4997 MB/s、up 0.1663/0.1661（持平）；帧字节对照单测逐字节相同；relay 全套单测绿。**叶帧判据不可达**（中继 99% 阻塞在 poll，低于采样噪声底） | **绿（不回归 + 结构）**；叶帧面**未取证（登记）** |
| F7 | 三态单测（主判据） | `udpcap_wait_three_states` 绿；`--bind-interface none` 形态（本 harness）修复前后同为 ~300s | **绿（单测）** |
| 总判据① 目标叶帧 | 各条达标 | 上表全达标 | **绿** |
| 总判据② **同吞吐下进程 CPU 相对下降 ≥5%** | ≥5% | **−0.6%（交替 3+3 中位；s/GB +0.5%）** | **红——未达标**，按设计 §4.3 证伪条款记「叶帧消失、收益落在带内噪声」，**不宣称 CPU 收益** |
| 总判据③ 吞吐不回归（≥ −2%） | 不回归 | lo0 down +2.4%、up −1.8%；en0 §3.8 down +2.2%、up −2.1%；中继持平 | **边缘绿**（up 贴 −2% 界；A/B 各 3 轮离散 89.98–101.42 → 噪声 > 效应） |
| 增益判据 down ≥ +2% | +2%（驱动为瓶颈可 +5–8%） | +2.4%（同窗中位） | **边缘达标，不出噪声带** |
| 总判据④ RSS ≤ A×1.1 | ≤1.1× | **1.078×**（lo0）/1.087×（en0，n=1）；F1 容量口径所致，未越界 | **绿（贴界）** |
| 证伪检查「叶帧全消失但 CPU/吞吐无变化」 | 记「收益 < 带内噪声」 | **命中** | 见 §3.5 |

### 3.5 判读（不粉饰）

1. **热点确实是热点、删除也确实删掉了**：F1–F4 四个目标叶帧全部达标/归零（合计 **23.2% → 0.7%**），且 B2 复采可复现；F2 的 65KB memset、F3 的 302KB/迭代级零初始化、F4 的全局锁 getenv 都是**每包/每迭代确定性成本**，消除后对手机核与未来改动是正向基础。
2. **但进程级 CPU 不降**：同一窗口内驱动线程把省下的时间花在了**更多的拍**上（每拍固定税：零超时 `poll`、`dnsface` 每拍 64KB、栈 encap）——**F1 的 18% 忙时收益被同层其它每拍成本吃掉**。本批因此**没有拿到设计期望的 ≥5% CPU 降幅**，吞吐差异（down +2.4%）不出带内噪声。
3. **三个独立口径互相自洽**（`sample` 净忙时不变 + `ps -o time=` 累计差持平 + 吞吐持平）⇒「CPU 未降」的结论站得住，不是仪器问题。
4. **噪声**：A/B 各 3 轮 down 离散 ±6%、up ±3%；交替协议下 B 恒当对第二拍（loadavg 系统性高 0.3–0.5）⇒ 任何 <3% 的差异不可当结论。
5. **本批的真实交付** = ① 三个 memset/memmove 热点的**结构消除**（可复现、口径干净）② 每包/每拍确定性成本下降（`consume_step`/`getenv`/relay 每包 alloc）③ **下一层瓶颈清单**（§6）——而不是吞吐/CPU 数字。

### 3.6 中继臂（F6 专用；已跑）

| 时刻 | 臂 | loadavg | 流 | down MB/s | up MB/s | 结果 |
|---|---|---|---|---|---|---|
| 03:54:05 | A | 2.23/2.71/2.86 | 1 | 0.4915 | 0.1663 | ok（via=relay） |
| 03:54:50 | B | 1.95/2.58/2.80 | 1 | 0.4997 | 0.1661 | ok（via=relay） |
| 03:51:32 | A | 2.57/2.96/2.97 | 2 | 0.4915 | 0.1811 | ok（via=relay） |
| 03:52:36 | B | 2.45/2.84/2.92 | 2 | — | — | **未完成**（客户端「帧协议错误：写通道长时间无进展」watchdog） |
| 03:50:39 | A | 2.29/2.94/2.96 | 2 | — | — | 首轮误用直连 token（via=direct），作废 |

**处置**：2 流形态下 B 臂一次未完成、A 臂同参数完成；降到 1 流后**两臂均完成且吞吐持平（down ±1.7% / up ±0.1%）**。归因 = 中继**每源 200pps 限速**（≈2Mbps 硬顶，RTT 2010ms）下 2 流负载的路径噪声（watchdog 超时），**非 F6 回归**；依据：F6 只改编帧缓冲复用（逐字节对照单测）、`assoc_read` 借用与读缓冲外提，relay 全套端到端单测全绿。**残余不确定如实登记**（未同参数复跑 A 臂 2 流确认）。`sample` 判据（`frame_bytes` alloc 叶帧）**在本机不可达**——中继进程 99% 阻塞在 `poll`（`relay-r3-*.sample.txt`），不虚报。

### 3.7 作废轮（环境退化，仅登记）

03:45:16 A-conf（表头 load 2.58，**跑中 loadavg 峰值 6.29**）down 17.22 / up 21.64 MB/s；03:46:00 B-conf down 54.33 / up 31.23——两轮吞吐塌陷（出口 `reactor` 观测单拍 11.9ms 级停顿、dup ACK 1060）。**两轮不计入判决集、不作任何结论依据**（代码门 L4：跑中 loadavg 未连续落盘，判据不可复现——下一批 harness 补轮末采样）。

### 3.8 en0 产品形态臂（代码门 M4 补跑；n=1/臂，仅旁证）

| 时刻 | 臂 | loadavg | down MB/s | up MB/s | CPUδ | CPU% | RSS max KB |
|---|---|---|---|---|---|---|---|
| 04:02:34 | A | 2.81/2.98/2.89 | 41.60 | 53.88 | 50.15s | 139.3% | 26240 |
| 04:03:17 | B2 | 4.30/3.35/3.03 | 42.53 | 52.75 | 48.45s | 134.6% | 28528 |

**B/A：down +2.2% / up −2.1% / CPUδ −3.4%**。en0 臂 CPU 明显高于 lo0（139% vs 124%，sendto 税），B 侧 CPU −3.4% 是**本轮唯一为正的 CPU 信号**，但 **n=1/臂且两轮 load 不对称（2.81 vs 4.30，B 轮更高）** ⇒ 只作旁证，**不足以推翻 §3.4 总判据②的判红**（且该臂 token 未加 `--loopback-only`，端点由赛跑采纳——两轮 rtt 2-3ms，未逐轮核端点形态）。

---

## 4. 判据行与观测面

- **判据行行文**：diff 中无任何 `logf`/判据行字符串改动（唯一 `eprintln!` 改动是 `HOMEWAY_TX_DBG` 的**守卫表达式**，串本身原样）⇒ **零判据行变更**（代码门独立复核确认）。
- **wire 字节**：F6 编帧逐字节对照单测；F2 出/入 100 包 round-trip + 「大包后小包」长度断言；其余改动不触编码路径 ⇒ **零 wire 变化**（代码门确认）。
- **「计数输入集 / 数值语义变化（行文不变）」登记**（`docs/INTEROP-CRITERIA.md` 同批加一行，代码门 L7 复核成立）：

> | 2026-10-08（Q-I 前段） | **udpcap 探测周期 / caps 新鲜度**（`UDP 默认路径：…` 行 = 本表 udpcap 的 `—` 行，**行文与语义均不变**） | 频次：bindwatch 在位形态 `~600s` → **`~300s`**（对齐 Go `udpcap.go:26` 5min ticker + kick）；`--bind-interface none` 形态本已 ~300s（不变）。喂客户端 C14 的 caps 位新鲜度：`≤10min` → **`≤5min`** | Q-I F7：`recv_timeout` 的 Timeout 与 Disconnected 未区分（多睡一拍） | `UDP 默认路径` 行频次（行文不变）、caps 新鲜度、`server/engine.rs` 纯函数 `udpcap_disconnected_backoff` + 单测 `udpcap_wait_three_states`；**非**编号判据行 |

---

## 5. 评审记录

### 5.1 设计门（r5，第 1 棒产出，见 `docs/reviews/QI-design.md` §5）

**轮次目录** = `/tmp/dsh-review/r5.SjItld/`；**exit code = 0**；意见 14 条主项 + 行号订正 8 条 + 独立复核 4 条；逐条处置：认同 21 / 部分认同 1（举证订正，结论采纳）/ 不认同 0。要点：F5-3 池化降级不做（高危）· F6-2 落地形态写明（E0502）· F7 口径限定 bindwatch 形态 · F1 容量口径 + 摊还 ≤2B/B 订正 · CPU 仪器改累计 `time=` · iovec 漏项登记 · F8 整条不做。全部并入 v2，本批按 v2 实现。

### 5.2 代码门（r6）

- **轮次目录** = `/tmp/dsh-review/r6.TJ3TeI/`（prompt.txt / output.md / stderr.log）；**exit code = 0**。
- 评审对象 = 工作树 diff（未提交）；评审者独立跑了构建（A/B 二进制 sha256 对照 `nm | grep -c envflag` = 0/8）、`cargo test -p homeway-core --lib`（482 passed）、`cargo test --workspace --locked`、`cargo clippy -p homeway-core --all-targets -- -D warnings`，回读 boringtun 0.6 vendored 源码核对 F2 契约，复算采样叶帧与 `s/GB`。
- **意见条数：0 高 / 4 中 / 7 低**；**高危 = 无**（明确「未发现会改变 wire 字节、判据行、或引入别名/陈旧数据泄漏的缺陷」）。
- **总结论原文**：「代码门可通过（无高危、wire/判据行零变化、workspace 测试与 clippy 全绿），但批记录必须改口径后才能收口——按 §4.3 证伪条款记『叶帧消失、CPU/吞吐收益落在带内噪声（② 未达：−0.38% 原始 / −2.76% 归一）』，头条改用同窗 A/B 中位（+2.35%），并登记 F3 的 `dnsface.rs:182` 残留、未跑的中继/en0 臂与 F2 在 wgcore 侧的测试空档。」

### 5.3 逐条处置

| # | 意见（摘要） | 严重度 | 处置 |
|---|---|---|---|
| M1 | 总判据② 未达标（CPU −0.38% / s/GB −2.76%），摘要不得把 down +6% 当整批过门证据；须按 §4.3 记「收益 < 带内噪声」并把每拍固定成本登记为下一批靶点 | 中 | **认同**——本记录 §3.4/§3.5 判红并写明；`docs/PERF-AB.md` §9.18 同口径固化；每拍固定税与同物种残留进 §6 |
| M2 | 头条 down +6.02% 混入不同 harness/时段的 A 3 轮；同窗交替只有 +2.35%；「HEAD e75a55bf」是二进制 sha 不是 commit | 中 | **认同**——头条改用同窗交替 3+3 中位（down **+2.4%**）；旁证 A 3 轮显式标注「同进程连跑、仅旁证」；全文二进制 sha 标注为「二进制 sha256」 |
| M3 | F3 聚合口径未达（线程总 `__bzero` 4.75%→3.34%）；同类残留 `dnsface.rs:182`（每调用 64KB 零初始化）已成驱动线程第一大 `__bzero`（0.47%→2.92%）——要么本批顺手做，要么显式登记 | 中 | **部分认同（登记不动手）**——① 设计 F3 目标是**点名帧**（`service_sockets`/`reactor_turn` 的 `__bzero`，248 样本口径）⇒ 帧级已达标（1.49%→0.28%）；线程总口径按意见补报（§3.4）。② `dnsface.rs:182` **不在设计门审过的改动清单内**（审计 4a 只点了 `intercept/mod.rs` 四处），批内扩围 = 未过设计门的新改动；且本批测量窗口已退化，扩围后无干净复测窗口 ⇒ 按意见给出的第二选项**显式登记为下一批第一靶点**（§6，带本轮数字与修法方向）。**不静默** |
| M4 | en0 产品形态臂与中继臂都没跑，F6 无 A/B 证据 | 中 | **认同（已补跑）**——中继臂 03:50–03:54 已跑（§3.6，含一次 watchdog 未完成轮的归因与残余登记）；en0 臂 04:02–04:03 补跑（§3.8，n=1/臂，仅旁证）。评审时刻（03:49–03:55）与两臂执行有交叠，评审快照可能未含 |
| L1 | `VecDequeLite::consume(n)` 在 `n > remaining` 时静默清空（旧 `drain` 会 panic）；建议 `debug_assert` | 低 | **认同已改**——`consume` 加 `debug_assert!(n <= len - off, …)`（`out_tcp` 同受益）；复跑 test/clippy 全绿 |
| L2 | F2 的 `wgcore` 侧删 memset 无 Engine 级测试覆盖 | 低 | **认同（登记）**——评审同时独立回读 boringtun 源码核实契约安全；本记录 §2「未覆盖」登记为测试空档（论证 + `debug_assert` 双保险） |
| L3 | `udpcap_disconnected_backoff` 对 `Copy` 取引用、以 bool 编码三态；注释没点明「Timeout 不再多睡」这一行为变更 | 低 | **认同已改**——改值传参；注释重写并点明 Timeout 语义变更（旧形态周期翻倍） |
| L4 | 「跑中 loadavg 峰值 6.29」无产物支撑（只在轮首记 uptime） | 低 | **认同（登记）**——作废决定保留（且未挑对 B 有利的一轮留下）；harness 改进（轮末/1Hz loadavg 落盘）入 §6 |
| L5 | 同物种残留：`dnsface.rs:228/333` 的 `Vec+drain`、`server/bind.rs:757` 每包 `Vec`、`wtransport/bind.rs:287` 每包 `Vec` | 低 | **认同（登记）**——进 §6 残余清单（点名，避免下一批重复审计）；本批不改（均超设计清单） |
| L6 | RSS 判据覆盖面弱于它要拦的风险（4 流 × 34s 触不到「慢消费者 + 满 backlog × 1024 流」） | 低 | **认同（登记）**——§6 登记「RSS 判据只覆盖常态形态，需要时用合成多流压测补」 |
| L7 | 判据行登记成立（Go 真源复核过）；nitpick：行内嵌绝对行号会腐烂 | 低 | **认同已改**——登记行去掉 `INTEROP-CRITERIA.md:119` 内嵌行号（该行文本自描述），并把「非编号判据行」措辞保留 |
| — | 其它确认：wire/判据行零变化 ✓；复用缓冲均线程独占 + 单次迭代内拷出，类型层面排除别名 ✓；测量口径一致、`s/GB` 复算无误 ✓ | — | 记录 |

### 5.4 不认同项

**无。**（M3 为「部分认同」：口径与残留事实全部认同，处置选评审给出的第二选项——登记而非批内扩围，理由见上。）

---

## 6. 残余与下一批靶点（防静默漏做）

| 项 | 结论 | 证据/理由 |
|---|---|---|
| **`intercept/dnsface.rs:182` 每调用 64KB 零初始化** | **下一批第一靶点（本批登记不动手）** | 实测驱动线程 `DnsFaces::service → __bzero` **A 0.46% → B 2.92%**（B2 复采 3.11%，两轮一致）；每拍一次，F1 后已成为同线程第一大 `__bzero`。修法方向 = 缓冲提为 `DnsFaces` 字段（按本文件既有 `mem::take` 形态绕开 `route_tag`/`take_route` 的 `&mut self`，同 F3 的 `rx_scratch`）。**注意：A→B 的倍率上升发生在代码未改的站点上**（两二进制该函数反汇编逐指令相同）⇒ 机制 = 驱动循环每单位时间拍数变多（§3.5 判读 2），修它同时能压住「每拍固定税」面。 |
| `reactor_turn → poll(0)` 每拍零超时 poll syscall | 下一批靶点（本批观察项） | `sample` **12.2% → 18.7%**（B 侧放大后）；结构性（reactor 与 UDP 循环共用线程），要动须换统一事件循环（kqueue/epoll） |
| 发送线程 `sendto` 税 / `tx_drain_rounds` 每轮 `msgs` Vec | 观察项（超本批范围） | 发送线程 `sendto` 51.3%→47.7%（叶帧，等待/系统税为主）；`msgs` 分配既有注释已登记「后续」 |
| `dnsface.rs:228/333` `Vec+drain`、`server/bind.rs:757` / `wtransport/bind.rs:287` 每包 `Vec` | 登记（同物种残留，代码门 L5） | 量小或形态合理（跨线程所有权移交）；点名避免下一批重复审计 |
| 审计 4c `flows.keys().collect()`（≈0.09%）、4d 新流全表 count、6d `logf` 无缓冲、哈希 `SipHash` | 登记不改（设计 §6 原判） | 低频/与 Go 同形/全局决策 |
| `udpNoReply` 等 Q-B 观测漂移 | 沿用 Q-B 登记行 | 本批未新增观测面变更 |
| **测量 harness**：轮末 loadavg 落盘（L4）；en0 臂端点形态逐轮核实（§3.8 备注）；RSS 合成多流压测（L6） | 下一批 harness 改进 | 本批 conf 轮判据不可复现即此缺口 |
| **F2 的 `wgcore` 站点 Engine 级测试空档**（L2） | 登记（论证 + `debug_assert` 双保险） | 既有 `two_tunn_handshake_then_queued_data` 走裸 `Tunn`，不经 Engine |
| 中继 2 流形态 B 臂一次 watchdog 未完成 | 登记（残余不确定） | §3.6：200pps 限速路径噪声归因，未同参数复跑 A 臂确认 |
| 生产出口 / tier / homeway 两仓 / baseline | **全程未碰**（隔离纪律） | 只用 `tools/local-rust-exit.sh` #9（42659）、`tools/local-rust-relay.sh` #1（42781）本地私有实例；测毕全停（出口 #9 + 中继 #1 + 客户端进程已清） |
