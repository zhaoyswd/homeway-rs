# Q-I 前段（性能细节批）设计文档

> 批次：Q 批整改（`docs/REVIEW-ROADMAP.md` §Q-I 前段；2026-10-08 拆前段/尾段并提前）。第 1 棒（设计）产出。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`「Q-I 性能细节批」节 + ROADMAP「依赖与顺序建议」（重排说明）。
> 基线：`git HEAD = 36fd197`。**行号均为复验时（本 HEAD）实测值**，实现以符号定位为准。
> 实测证据：2026-10-08 03:01 本地私有实例 `sample`（**HEAD release 构建**，见 §0.2）。
> **范围边界**：前段只做 `server/device.rs`、`wgcore/**`、`server/intercept/**`、`relay/**`、
> `facade/**`、`udpcap`（engine）。**DNS TTL 缓存 / socket 缓冲复用 / files 拷贝 = 尾段**
> （Q-E 之后），本文不设计。两台生产出口与 tier/homeway 两仓全程不碰。
> **状态**：v2——设计门（dsh 外部评审）已过，见 §5；v2 已并入评审与二次复核的全部阻塞/中等意见。

---

## 0. 复验方法与现状实测（证据先行）

### 0.1 复验方法

逐条回源码重定位（审计行号已漂，且 Q-B/Q-C 改过 `intercept/**`、`wtransport/**`、
`relay/**`、`server/engine.rs`），并用一次**只读性能测量**（本地私有实例 + `sample` +
`speedtest`）核对「热点是否真在审计所指位置、量级多大」。🔎 项按性能问题复核真伪。
引用 Go 语义处一并核对 `baseline/homeway`（只读 oracle）。

### 0.2 本轮实测（HEAD 构建；2026-10-08 03:01）

**口径**（照 PERF-AB §9.15 既有消融形态）：

- 出口 = `tools/local-rust-exit.sh start 9`（端口 42659；state `/tmp/homeway-rs-rustexit-9`）——
  **`cargo build --release -p homeway-cli` 03:00:56 现场重建**（仓内旧产物建于 10-07 16:17，
  早于 Q-B/Q-C，已弃用）。
- 客户端 = 同二进制统一进程（state `/tmp/qi-client`，serve/relay 双关），token 铸
  `--loopback-only` 变体 ⇒ 采纳 `127.0.0.1:42659`（**lo0 路径**）。
- 负载 = `speedtest --down 15s --up 15s --streams 4`；测量期 `sample <exit_pid> 22`。
- **loadavg 表头**：跑前 `3.52/2.64/3.43`，跑后 `3.85/2.89/3.49`（1/5/15 分钟）。
- 结果：`down 87.07MB/s（≈696Mbps，rtt 4ms）/ up 127.58MB/s（≈1021Mbps）`，`via=direct`。
- 原始产物：`/tmp/qi-sample2.txt`（= `/tmp/qi-sample-head16b.txt`，22s 窗口每线程采得
  **16070 样本**；采样器在负载下未满 1kHz，故有效 ≈16s s。**采样窗口跨 down 尾 + up 头**：
  `flush_backlog` 只在 down 相位活跃 ⇒ 其占比是**被 up 相位稀释后**的值，down 相位真实占比更高；
  A/B 两臂同窗口 ⇒ 不影响判决，只影响绝对数的解读）。
  另有一份同日 02:55 的 pre-Q-B 旧产物样本 `/tmp/qi-sample.txt`（结论一致，见判读 1 注）。

**出口驱动线程（`homeway-serve-drv`，16070 样本；引擎循环内 8640 样本 = 忙时 54%）**：

| 叶帧（sample 调用链） | 样本 | 占线程 | 占引擎忙时 | 归属 |
|---|---|---|---|---|
| `service_sockets → flush_backlog → _platform_memmove`（**`tx_backlog.drain(..w)` 尾搬移**） | **2756** | **17.1%** | **31.9%** | **F1** |
| `reactor_turn → poll(0)`（每拍零超时 poll syscall） | 2048 | 12.7% | 23.7% | 观察（§6） |
| `Device::encapsulate → Tunn::encapsulate`（boringtun 组件） | 1990 | 12.4% | 23.0% | 组件（非本批） |
| `iface.poll`（smoltcp 收发/ACK；各帧合计口径，挑帧粒度 ±10%） | ≈1800 | ≈11% | ≈21% | 组件（非本批） |
| `Device::encapsulate → consume_step → __bzero`（**65KB `clear+resize`**） | **432** | **2.7%** | **5.0%** | **F2** |
| `service_sockets +1112 → __bzero`（`[0u8; 64*1024]` 栈缓冲） | **194** | **1.2%** | **2.2%** | **F3** |
| `flush_backlog → send_slice → enqueue_slice`（进栈环拷贝，必要） | 109 | 0.7% | 1.3% | 保留（F1 后仍在） |
| `Device::encapsulate → getenv`（`HOMEWAY_TX_DBG`） | **101** | **0.63%** | **1.2%** | **F4** |
| `Device::encapsulate → step_of`（`to_vec` malloc+memmove） | 75 | 0.47% | 0.9% | F2b（不做，§6） |
| `reactor_turn +3068 → __bzero`（`[0u8; READ_CHUNK]`） | **54** | **0.34%** | **0.6%** | **F3** |
| `service_sockets → flush_out → __sendto`（upstream 写） | 277 | 1.7% | 3.2% | 系统税 |

**发送线程（`homeway-serve-tx`，16070 样本）**：`tx_drain_rounds → UdpSocket::send_to → __sendto`
7849（48.8%，系统税）；`tx_drain_rounds → getenv` **≈52（0.32%）**；`tx_drain_rounds` 内
malloc/free 约 70（0.4%，`msgs` 每轮局建——**既有注释已登记「后续」**，超出本批范围）。

**判读**：

1. **审计「flush_backlog 是第一位热点」成立且比预想更集中**：该叶帧 = 引擎忙时的 32%，全部来自
   `Vec<u8>::drain(..w)` 的尾部 memmove（`flush_backlog+0x158` 即 `bl memmove`；指令序列
   `subs len-w` / `memmove(ptr, ptr+w, len-w)` / `str len-w` 与 `Vec::drain` 的 drop 逐字对应；
   评审与二次复核均独立反汇编确认）。机制：栈内 TCP 发送环每次只腾出少量空间（窗口/ACK 节奏），
   `send_slice` 只接纳少量字节，随后把**几乎整条 backlog（≤256KB+）**搬到队首——搬移量/消费量
   放大数十倍（评审独立算量级：2756 样本 ≈2.7s CPU ⇒ 按 15GB/s 计 ≈40GB 搬移 / 同窗实载 ≈1.3GB
   ⇒ **放大 ≈30×**）。**pre-Q-B 旧样本同叶帧 2582/14566（17.7%）**（驱动线程总样本 14566）——
   两版一致 ⇒ 与 Q-B/Q-C 改动无关、仍是当前代码的一号热点。
2. `consume_step` 的 65KB memset 实测 432 样本（2.7%，≈0.73µs/包 × 26kpps 量级），审计成立；
   `wgcore::decapsulate_in` 内层循环的同类 clear+resize 仍在（注释自称已删，**注释-实现不符**，
   见 §1.2）。
3. `getenv` 实际是**5 个站点**（审计列 2 个）：新增 `server/bind.rs:792/999/1045` 三处
   （P1 发送线程批引入），其中 `:1045` 在**每排空轮**、`:999` 在**每 poll 唤醒**执行。
   （进程内其它合法 getenv 读者——主线程装配期等——不在本批口径内，证伪条件按站点限定，见 §4.3。）
4. intercept 的 64KB 栈缓冲零初始化（`__bzero`）三处合计 248 样本（1.5%），审计成立。
5. **本机 lo0 臂当前 ≈700Mbps（down 87MB/s）**：与 §9.15/§9.16 的 lo0 形态（默认 413、消融
   最佳 749）相比不低；但**749 出自 loadavg 5-10 的带且用了已随 v0.2.2 删除的
   `HOMEWAY_TX_SENDTHREAD` 消融缝**，只作「量级可行、环境可用」的旁证，**不做跨带对比**
   （§4.1 自己的纪律）。本次 loadavg 3.5-3.9。

---

## 1. 复验结果表

| 派单条目 | 真伪 | 现行源码位置（HEAD 36fd197） | 结论 |
|---|---|---|---|
| **1** `consume_step` 每出站密文包 65KB memset | ✅ 成立（且实测 2.7%） | `server/device.rs:261-275`（`Wire` 分支 `clear()` `:268` + `resize(WG_BUF,0)` `:269`）；`WG_BUF = 65536+148` `:40`；缓冲构造 `:117`；同类残留 `wgcore/mod.rs:537-538`（`decapsulate_in` 空数据报重调循环内），注释自称已删 `:528-529/:607-608/:620-621` | 每个出站密文包（`encap_peer → consume_step(Wire)`）付一次 65684B memset；sample 实测 `consume_step+568 → __bzero` **432 样本 = 2.7%**（≈0.73µs/包）。**成立**；另发现 `wgcore` 内层注释与实现不符。 |
| **2** `encap_peer` 每包 `to_vec` | ✅ 成立（低值） | `server/device.rs:397-405`（`step_of`：`WriteToNetwork(w) => w.to_vec()` `:399`、`WriteToTunnelV4 => to_vec()` `:400`）；调用 `:343`/`:354`/`:356` | 每包 1 次 alloc+copy（出站 ~1.3KB ≈ 41ns；入站明文同形）。sample 实测 `step_of` **75 样本 = 0.47%**。**成立但可见收益 ≈ 0.2%**，处置见 §6（不做）。 |
| **3** 每包 `std::env::var_os("HOMEWAY_TX_DBG")` | ✅ 成立（**站点 5 处，非 2 处**） | `server/device.rs:344`（每出站包）；`wtransport/bind.rs:542`（每收包）；**`server/bind.rs:792`（每次唤醒推送）/ `:999`（每 poll 唤醒）/ `:1045`（每排空轮）** | getenv 走全局锁（macOS `_os_unfair_lock`），实测单次 ≈55ns；sample：驱动线程 101 样本（0.63%）+ 发送线程 ≈52（0.32%）。**成立**；`wgcore/mod.rs:731` 的 `HOMEWAY_WG_DEBUG`（每次写失败）同族，顺手一并缓存。装配期一次性 env 读取（如 `tx_shape_resolve`）不是热路径，不在本批。 |
| **4a** 每数据报 64KB 栈缓冲零初始化 | ✅ 成立 | `intercept/mod.rs:1994`（`read_upstream` UDP 循环内 `[0u8; 65536]`）、`:2029`（TCP 循环内 `[0u8; READ_CHUNK]`）、`:2496`（`service_sockets` TCP 读循环 `[0u8; 64*1024]`）、`:2570`（UDP 读循环 `[0u8; 65536]`） | sample：`service_sockets → __bzero` 194 + `reactor_turn → __bzero` 54 = **248 样本（1.5%）**。**成立**（每循环迭代一次 64KB 零初始化；`read_upstream` 已内联进 `reactor_turn`，故叶帧名不显）。 |
| **4b** 双拷贝 | ✅ 成立（其一可省） | `read_upstream` 读栈缓冲→`tx_backlog.extend`（`:2033`）；`service_sockets` 读栈缓冲→`out_tcp.push`（`:2510`）/`out_udp.push_back(to_vec())`（`:2611`） | 内核→栈缓冲 与 栈缓冲→自有缓冲 两拷。第二拷是设计（字节流/数据报队列），**保留**；本批消除的是**栈缓冲的每迭代零初始化**（F3）与 **`tx_backlog→栈环` 的放大搬移**（F1），不是「消除双拷贝」本身。 |
| **4c** 每拍 `flows.keys().collect()` 全表拷贝 | ✅ 成立（低值） | `intercept/mod.rs:2453`（`service_sockets` 开头）；同形 `:2833`（`close()`，罕用）；`reactor_turn` 每拍另有两处全表遍历：cc/obs 取最忙流 `:1785-1789`、**建 pollfds `:1912-1919`** | sample：`spec_from_iter` ≈14 样本（0.09%）。**成立但极低**；处置见 §6（可选微项）。 |
| **4d** 新流判定全表 `count` | ✅ 成立（低频） | `:1019-1021`（`tcp_new` TCP 在册数）、`:1044-1047`（DNS-TCP 腿数）、`:1383-1386`（`udp_new` UDP 会话数） | 三处都只在**新流**路径（SYN/首包）执行，非每包、非每拍。**成立但可忽略**（并发上限 1024/4096 上界内 O(N) 扫描，1 次/SYN）。处置：登记不改（§6）。 |
| **5** `wgcore` 每拍 64KB/65KB 分配 + `Arc<Mutex>` 队列 | ✅ 成立 | `wgcore/mod.rs:685`（`resolve_udp`：**每个待决 id 每拍** `vec![0u8; 65535]`，已核只在 `wait_recv.is_some()` 后执行）、`:982`（`resolve_pending`：每次读结算 `vec![0u8; 64*1024]`，已核只在 `can_recv` 时执行）；`stackb.rs:51`（`Arc<Mutex<VecDeque<Vec<u8>>>>`）、`:134-148`（`DevTxToken::consume` 每包 `vec![0u8; len]` + 每包加锁；token 只持 `out: SharedQueue`） | 成立。① 的浪费在「有 `UdpRecv` 挂起但无包」时也每拍发生（本次只做 ①）；② 的净收益 ≈0（见 §2 F5）；③ 池化方案经设计门判定不可行/收益不可达（§2 F5-3、§5 意见 1.1/2.2）。 |
| **6a** relay `frame_bytes` 每包堆分配 + memcpy | ✅ 成立 | `relay/mod.rs:958`（`forward_up`：`let frame_bytes = frame::frame_bytes(kind, payload);`）；`wtransport/frame.rs:62-66`（alloc+2 次 extend） | 每上行包 1 次 alloc + 全量拷贝。**成立**（中继形态每包税；直连形态不经过）。 |
| **6b** assoc 读每事件 64KB 分配且单包 | ✅ 成立 | `relay/mod.rs:500-502`（`PollSource::Assoc` 分支：每事件 `vec![0u8; 65535]` + `b[..n].to_vec()`，且**只读一包**不排空）；`:480`（主 UDP 读缓冲亦每事件分配，但循环排空） | **成立**。另外 `assoc_read` `:985` 全程只读 `pkt`（`legup_cookie`/`verify_legup`/`send_to`/`len`），签名却收 `Vec<u8>` ⇒ 多一次拷贝。 |
| **6c** per-peer 全表 count / 孤儿扫描 | ✅ 成立（低频） | `:846`（`forward_up` 建会话时 `assocs.keys().filter(...).count()`）；`:750`（腿换址，罕见）；`:1264-1273`（`close_ctl_quiet` 孤儿清理：`legs` 过滤 × 每条候选 `assocs.keys().any` = **O(legs×assocs)**） | **成立**。前者仅新会话路径（低频），后者每次控制连接关闭执行 O(L×A)。处置：孤儿扫描改一遍 Hash 聚合（低风险）；count 登记不改。 |
| **6d** `RelayLog::logf` 无缓冲 | ✅ 成立（**建议不改**） | `relay/logfile.rs:93-98` `logf` → `RotatingWriter::write_line`（`logfile.rs:65-86` 单次 `write_all`，无缓冲） | Q-C F6 已把逐包可达日志全部节流；当前 `logf` 调用点均为「每会话/每次腿变更/每分钟统计」量级。Go 基线同为单次无缓冲写（`baseline/.../logfile/logfile.go`）⇒ 加缓冲 = 落盘时序分叉（崩溃丢行、需 flush 生命周期）。**登记不改**（§6）。 |
| **6e（批次真源补项）** relay sendmsg iovec | — 未列（ROADMAP 前段有） | — | ROADMAP「Q-I 前段」列有「`relay` sendmsg iovec」，本设计不实装（理由见 §6），**显式登记不静默**（本轮评审 7.8 抓的正是这条漏项）。 |
| **7** `udpcap` 周期多睡一个 interval | ✅ 成立（**限定形态**） | `server/engine.rs:1800-1801`（`if kick_rx.recv_timeout(UDPCAP_INTERVAL).is_err() { sleep(UDPCAP_INTERVAL) }`）；`UDPCAP_INTERVAL=300s` `:44`；kick sender 只在 `resolved.is_some() && bind_addr.is_none()`（`:726`）时移交 bindwatch（`:745`），否则 `start()` 返回即 drop | **bindwatch 在位形态（auto 挑卡/显式绑卡 = 生产形态）**：Timeout 已耗 300s 再睡 300s ⇒ 实际 **~600s**。**无看护形态（`--bind-interface none`，含本设计的测量 harness）**：sender 已 drop ⇒ `Disconnected` 立即返回 + `sleep(300)` ⇒ 周期本就 **~300s**、无 bug。审计与 Go 真源（`udpcap.go:26/140-152`，5min ticker + kick）一致 ⇒ 修复只对 bindwatch 形态生效。 |
| **8** 状态快照每 250ms 全量重建 | ✅ 成立（量级不可测；**裁决不做**） | `facade/mod.rs:374-390`（`tun_status`）；`tun_status.rs:102-198`（每次全量建 JSON）；`tun_exec.rs:594-600`（`runner`/`transport`）/`:632-662`（`runner_of`） | tier 侧 `STATUS_POLL_MS=250`（`tier:entry/src/main/ets/vpnextension/TierVpnExtensionAbility.ets:120`）⇒ 4Hz。每次调用：多处锁 + 字符串格式化 + serde_json 全量序列化，**估 ≤40µs/次 ⇒ ≤0.02% CPU（估算，未实测）**。另：`tun_status()` 每调用两次 `stage.snapshot()`（`:376` 与 `tun_running_inner` `:397`；后者另有调用点 `facade/mod.rs:447`）——**与 Go 同形**（Go `tunRunningValue()` 自己也再读一次 `tunStageSnapshot()`，`tunmode.go:545-556`，且注释明确接受「不是一致快照」）⇒ 改成单次读是**朝 Go 反方向收紧**、零收益。**裁决：整条不做**（§2 F8/§6）。 |

### 1.2 误报 / 订正记录

- **无整条误报剔除**：8 条派单条目全部回源码复验成立（含 🔎→实测确认的样本量级）。
- **订正 1（站点数）**：条目 3「每包 getenv」审计列 2 处，实际 **5 处**——`server/bind.rs:792/999/1045`
  为 P1 发送线程批新增（`tx_drain_rounds` 每轮、`tx_thread_loop` 每 poll 唤醒、入队唤醒推送），
  实测占比与 `device.rs:344` 同量级。修法须覆盖全部 5 处（另附 `HOMEWAY_WG_DEBUG`）。
- **订正 2（注释-实现不符）**：`wgcore/mod.rs:528-529/607-608/620-621` 注释自称「不 clear/resize
  （每包 65KB memset 纯浪费，中-10①）」，但 `decapsulate_in` 的空数据报重调循环内 `:537-538`
  **仍有一对 `clear()+resize(WG_BUF, 0)`**（握手应答路径，非数据面每包）。实现须一并删除，
  注释与代码对齐。
- **订正 3（措辞）**：条目 4「`read_upstream`/`service_sockets` 双拷贝」成立，但其中「第一拷」
  （内核→栈缓冲）无法消除（`read(2)` 语义），第二拷是设计（队列）；本批消除的是**栈缓冲的
  每迭代零初始化**（F3）与 **`tx_backlog→栈环` 的放大搬移**（F1），不是「消除双拷贝」本身。
- **订正 4（低频 vs 每拍）**：条目 4c/4d——4c 确为每拍（≈0.09%），4d 只在**新流**路径，
  量级可忽略。措辞按此收紧。
- **订正 5（udpcap 形态限定）**：条目 7「实际 ~10min」只在 **bindwatch 在位**形态成立；
  `--bind-interface none` 形态本就 ~300s（见 §1 表条目 7；设计门意见 3.1，本轮复验确认）。
- **订正 6（批次真源漏项补录）**：ROADMAP 前段列有「`relay` sendmsg iovec」，本设计未实装——
  在 §6 显式登记「不做 + 理由」（设计门意见 7.8）。
- **本轮新增观察（登记，不改）**：`reactor_turn → poll(0)` 占驱动线程 12.7%（每拍一次零超时
  poll syscall，23.7% 忙碌时）；`tx_drain_rounds` 每轮 `msgs` Vec 分配（既有注释已登记「后续」）。
  两者都超出本批派单范围，见 §6。

---

## 2. 修复清单

> **实现顺序按实测收益排序：F1 → F2 → F3 → F4 →（手机核）F5-1 →（中继）F6 → F7。**
> F8 经设计门裁决为「不做」（见下）。每条含：方案 / 涉及文件 / 风险 / 测试计划 /
> **预期收益量级与测量臂** / 判据行影响。所有条目的共同约束：**wire 字节零变化**、
> **判据行零变化**（§3）、**不引入新的 `unsafe`**（本轮评审确认本批不需要 unsafe——这正是
> 比 iovec 形态安全的点）。

### F1 `tx_backlog` 前缀偏移化（消 `drain` 放大搬移）——**本批一号**

- **条目**：4 的性能核心（2026-10-08 `sample` 第一位热点；本轮 HEAD 复测 17.1% 线程 / 31.9% 忙时）。
- **现状**：`Flow.tx_backlog: Vec<u8>`（`intercept/mod.rs:713`）承载 upstream→栈方向字节流：
  - 写入：`read_upstream` `:2033`（`extend_from_slice`）、DNS-TCP 腿 `:1493`（`extend_from_slice`）；
  - 消费：`flush_backlog` `:2339-2358`：`send_slice(&f.tx_backlog)` `:2350` → 成功 `w` 字节后
    `f.tx_backlog.drain(..w)` `:2353`（**尾部整体前移 = memmove**）；
  - 长度语义：水位门 `:2024` / 兴趣位 `:1914`（`interests_for(io, f.tx_backlog.len())`）/
    cc 观测最忙流 `:1750` / EOF 判空 `:2344/:2383` / FIN 推进 `:2355`。
- **方案**：把 `tx_backlog` 的类型换成同文件既有 `VecDequeLite`（`:151-189`，Q-B F1 已具备
  「消费阈值 `off*2 >= len` 触发原地压缩」的摊还实现，并有单测 `:4570-4595`）：
  - `extend_from_slice(x)` → `push(x)`；`drain(..w)` → `consume(w)`；
    `send_slice(&f.tx_backlog)` → `send_slice(f.tx_backlog.remaining())`；
    一切 `len()` 读语义 → `remaining().len()`（`is_empty()` 保持）；
  - `VecDequeLite` **不提供 `len()`** ⇒ 「忘了改某处」在编译期暴露（类型承担不变量）；
  - **不变量与内存口径**：`len = off + remaining`，压缩触发条件 `off*2 >= len` ⇒
    任意时刻 `dead ≤ remaining`、`len < 2 × remaining`；**背面口径 = 容量**（`Vec` 只增不减、
    倍增扩容）：最坏 `capacity ≈ 2 × len < 4 × remaining`。旧实现 `len ≤ WATERMARK + FLOW_BUF`
  （320KB）/容量 ≤ ~512KB；改后 `len < 640KB`/容量最坏 ≈ **1MiB/流**（`MAX_CONNS=1024` 上界内
  理论最坏 +~0.5MiB/流，只在「慢消费者 + 满 backlog」形态出现，与现状同条件）。
    **RSS 判据**（§4.3）拦这一面。
- **为什么摊还是 ≤2 字节/字节消费（订正：初稿写 ≤1，错）**：一次压缩拷 `R = 压缩后 len`
  字节，下次触发需再消费 `R/2` ⇒ 摊还 ≤2 字节搬移/字节消费；对照现状实测放大 ≈30×（评审独立
  算量级自洽）。修正后仍在 **~15×** 的下降量级上，目标不改。
- **为什么不换 `VecDeque<u8>`**：消费端 `send_slice` 需要**连续切片**，`VecDeque` 要么
  `as_slices` 两次 send（多一次 syscall、部分写语义变复杂），要么 `make_contiguous`（O(n)，
  与 `copy_within` 同价但改动面更大）。
- **为什么不做「直读进栈环」**（`TcpSocket::send(closure)` + `read(2)` 直写）作为本批默认：
  它会取消 `tx_backlog` 的 `WATERMARK` 背压门语义（内存上界从 256KB/流变成栈环 1MB/流），
  并改动 FIN 挂起时序——属**行为面**改动。**登记为可选后续**（§6）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`（**11 个代码站点 + 1 处测试**：
  `:713`（字段）/`:1207`（初始化）/`:1493`（DNS 腿写）/`:1750`（cc 观测 `len`）/`:1914`
  （兴趣位 `len`）/`:2024`（读门 `len`）/`:2033`（读入）/`:2350`（`send_slice`）/`:2353`
  （`drain`）/`:2344`+`:2355`+`:2383`（判空/挂起）/ **`:4493`（测试内 `len()`——换类型后会编译失败）**；
  以符号定位为准）。
- **风险**：低-中。① 语义点漏改 → 编译期兜住；② `push` 的死前缀清理仅当 `off == buf.len()`
  （已空）触发，交错消费下靠 `consume` 的阈值压缩——与 Q-B 对 `out_tcp` 的既有行为同构，
  已被单测覆盖；③ 容量口径见上（RSS 判据兜）；④ `consume(n)` 在 `n > remaining` 时静默清空
  （不 panic），但 `w` 来自 `send_slice` 恒 ≤ remaining ⇒ 不可达。
- **测试计划**：① 复用/扩展 `vecdequelite_compacts_dead_prefix`（`:4570`）：随机交错
  `push/consume` 序列与 `Vec<u8>` 参照实现逐字节对照 + `len < 2*remaining` 断言；
  ② 新增「水门槛按 remaining 计」单测：灌到 `WATERMARK` 断言 `interests_for` 摘 POLLIN、
  `read_upstream` 的门停读；③ 回归全量 `intercept` 单测（水位/兴趣位/部分写续传/EOF 挂起）
  + `cargo test --workspace`。
- **预期收益**：`flush_backlog→memmove` 叶帧 **2756 样本（17.1% 线程 / 31.9% 忙时）→
  模型预测 ≈1%（≤2 字节/字节 vs 现状 ≈30×），目标 <3%**。折算：驱动线程忙时少 ~1/4。
- **测量臂**：§4 的 lo0 判别臂（`sample` 叶帧 + speedtest 中位）+ en0 产品形态臂（CPU）。
- **判据行影响**：无（wire 不变；`tx_backlog` 非观测面）。

### F2 `consume_step` 65KB memset 消除（含 `wgcore` 同类残留）

- **条目**：1。
- **方案**：删除两处「清缓冲再重调」的 memset，保持 `wg_buf.len() == WG_BUF` 恒定不变量：
  - `server/device.rs:268-269`（`Wire` 分支的 `clear()+resize(WG_BUF,0)`）：删除。理由：缓冲
    自 `Device::new` `:117` 起恒为 `WG_BUF` 长，`Tunn::decapsulate/encapsulate` 只**写前缀**
    并返回子切片（不改 dst 长度）；紧接的空数据报重调（`:272`）写自己的前缀，无需清零。
  - `wgcore/mod.rs:537-538`（`decapsulate_in` 重调循环内，同款）：删除；顺带修正
    `:528-529/:607-608/:620-621` 注释与代码的一致性（订正 2）。
  - **不变量守卫**：`debug_assert_eq!(self.wg_buf.len(), WG_BUF)`（放 `consume_step` 入口/
    `decapsulate_in` 入口，debug 构建兜「未来有人把缓冲改短」）。
- **为什么安全（分面）**：boringtun 0.6 对 dst 太小的处置**分两个面**——数据面
  `session.rs:196-199/243-244` 是 **panic**（release 也生效）；握手应答面
  `handshake.rs:715/795` 返回 `WireGuardError::DestinationBufferTooSmall`。两面都只在
  **dst 长度不足**时触发；我们从不缩短缓冲，长度契约不变。返回值是「已写满的精确前缀」
  （`format_packet_data` 返回 `&mut dst[..DATA_OFFSET+n]`、`handle_data` 返回
  `&mut packet[..computed_len]`），空数据报重调走 `send_queued_packet → encapsulate`，
  同样只写前缀 ⇒ **删了不会泄漏陈旧前缀**（评审已逐条核）。
- **涉及文件**：`crates/homeway-core/src/server/device.rs`、`crates/homeway-core/src/wgcore/mod.rs`。
- **风险**：低（两行删除 + 断言）。
- **测试计划**：① 现有 `device.rs` 单测（握手/rekey/漫游/双 peer/服务端主动握手）+ `wgcore`
  `two_tunn_handshake_then_queued_data` 全绿；② **新增**「大包后小包」单测：先 encapsulate
  ≥1400B 再 encapsulate 100B，断言第二个密文长度 = 100+32 且对端解密内容正确（防陈旧前缀
  泄漏）；③ 新增「连续 N 包 round-trip」测试（出/入各 100 包字节一致）。
- **预期收益**：`consume_step→__bzero` **432 样本（2.7% 线程 / 5.0% 忙时）→ 0**（≈0.73µs/包，
  26kpps 量级 ≈ 19ms/s）。
- **测量臂**：lo0/en0 两臂 `sample`（叶帧消失）+ speedtest 中位。
- **判据行影响**：无。

### F3 intercept 64KB 栈缓冲零初始化收敛

- **条目**：4a。
- **方案**（**按臂分别落地**——设计门 1.3 订正）：
  1. **循环外提（`read_upstream` 两臂都走这条；零风险，必做）**：`:1994`（UDP）、`:2029`（TCP）
     的缓冲提到各自 `loop` 外，每次调用一次 memset（而不是每数据报/每块）。
     理由：UDP 臂把缓冲传给 `&mut self` 方法 `self.udp_send_to_client(flow, &buf[..n])`
     （`:2002`）⇒ 结构体字段形态会撞 E0502；`self.on_upstream_eof(flow)`（`:2040`）不接收缓冲，
     TCP 臂本身可用字段形态，但统一走外提更简单。
  2. **结构体字段复用（仅 `service_sockets`）**：`Interceptor` 加 `rx_scratch: Box<[u8; 64*1024]>`
     字段，TCP 读循环 `:2496`、UDP 读循环 `:2570` 改用 `&mut self.rx_scratch[..]`。
     字段级分借（`self.sockets.get_mut(...)` 与 `&mut self.rx_scratch`）合法；两处传给
     `&mut self` 方法的调用点（`self.flush_out(flow)`/`udp_send_to_client` 等）都在缓冲
     **借用结束之后**，不冲突（实现时以编译器为准；若撞借用则退回第 1 级）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`。
- **风险**：低（缓冲生命周期/复用不改读写语义；`read(2)` 只写 `0..n`，`&buf[..n]` 用法不变）。
- **测试计划**：现有 `intercept` 单测全绿（UDP 回投/过境 TCP/DNS 腿）；新增一条「同一 pump 内
  多流多块读」回归（断言内容与块序不变）。
- **预期收益**：`__bzero` 叶帧合计 **248 样本（1.5%）→ 目标 ≤0.5%**（降幅 = 每调用迭代数
  ≈4-8×；设计门 6.3 建议放宽：循环外提只到「每调用一次」）。
- **测量臂**：lo0 臂 `sample`（`service_sockets`/`reactor_turn` 的 `__bzero` 叶帧计数）。
- **判据行影响**：无。

### F4 `HOMEWAY_TX_DBG`/`HOMEWAY_WG_DEBUG` 缓存（OnceLock）

- **条目**：3（含订正 1 的 5 站点）。
- **方案**：新增 `crates/homeway-core/src/envflag.rs`（`pub(crate)` 小模块，~25 行）：
  `pub fn tx_dbg() -> bool { static V: OnceLock<bool> = ...; *V.get_or_init(|| std::env::var_os("HOMEWAY_TX_DBG").is_some()) }`
  + `pub fn wg_debug() -> bool`（同形）。替换 5 + 1 个站点：
  `server/device.rs:344`、`wtransport/bind.rs:542`、`server/bind.rs:792/999/1045`、
  `wgcore/mod.rs:731`。
  - 语义登记：env 一旦被首次读取即缓存（既有先例 `wgcore/mod.rs:72-81` `ack_drain_bytes` 同款）；
    手动排障开关**须在进程启动前设置**（写入函数注释）。
  - 形态说明（设计门 ⑤）：两个全局自由函数 = 同一开关 5 站点共用一处缓存；放进 config/装配面
    需穿参到 `bind`/`device`/`wgcore` 三层，成本不成比例；与仓内 `ack_drain_bytes` 的文件内
    OnceLock 先例同族，可接受。
- **涉及文件**：新增 `envflag.rs`；`server/device.rs`、`server/bind.rs`、`wtransport/bind.rs`、
  `wgcore/mod.rs`、`lib.rs`（模块声明）。
- **风险**：低。风险点 = 测试/工具在**进程运行中**设置 env 的用法会失效——全仓已核
  `grep set_var` 为零个用例。
- **测试计划**：① 单测：未设置时 `tx_dbg()==false`；② 实测：`HOMEWAY_TX_DBG=1` 起出口 →
  `[TXDBG]`/`[ENCDBG]` 行仍可打（启动前设置路径有效）；③ lo0 样本中 6 个目标站点的 `getenv`
  叶帧消失（**证伪条件按站点限定**：进程内其它合法 env 读者不计）。
- **预期收益**：`getenv` 叶帧 驱动 101（0.63%）+ 发送 ≈52（0.32%）**→ 0**。
- **测量臂**：lo0 臂 `sample`（两线程 `getenv` 叶帧）。
- **判据行影响**：无（调试开关非判据面）。

### F5 `wgcore` 分配（**只做 F5-1**；F5-3 降级登记）

- **条目**：5。
- **F5-1（做）**：`resolve_udp` `:685`：`vec![0u8; 65535]` 改 `Engine` 字段
  `udp_rx_buf: Vec<u8>`（构造期一次分配）；`recv_slice(&mut self.udp_rx_buf)` 后
  `self.udp_rx_buf[..n].to_vec()` 交付（UDP 内层包小，多一次 n 字节拷贝 ≪ 省下的
  64KB alloc+memset）。**涉及文件**：`wgcore/mod.rs`。**测试**：既有 `udp_*` 单测 + 新增一条
  「复用缓冲后连续两包内容独立」断言（防别名）。**收益**：每待决 id 每拍的 64KB
  alloc+memset 消失（挂起无包时也发生；估算，未实测——手机核侧无本地判据）。
- **F5-2（不做）**：`resolve_pending` `:982` 64KB 复用——交付需 owned Vec，bulk 读（n≈64KB）
  会「省一次 memset、多一次同量拷贝」⇒ 净收益 ≈0（评审 2.3 已核账）。登记 §6。
- **F5-3（**降级为不做**，设计门 1.1 高危）**：`stackb.rs:143` `DevTxToken::consume` 的
  `vec![0u8; len]`（每包 malloc+零初始化）。
  - **降级理由（评审三条，逐条核实）**：① **池拿不到**——`DevTxToken` 只持 `out: SharedQueue`
    （`stackb.rs:134-148`），没有 `&mut TunDevice` 通路，「`consume` 从池取」类型上不成立；
    ② **省不掉 memset**——若归还时清空 `len`，`resize(len,0)` 仍整段零填充（净收益只剩
    malloc/free，「省 malloc+free+memset」是夸大；真省需 `set_len`/`MaybeUninit` = 新
    unsafe）；③ **别名（最要命）**——`stackb::TunDevice` **同时**是 `Interceptor` 的设备
    （`intercept/mod.rs:38/762/822`）：`drain_tx` 产物不是马上丢弃——出口侧 `pump` 里
    `for pkt in raw { self.on_tx(pkt) }` 把同一 `Vec` **移动**进 `self.tx_out`
    （`:1868-1887`）/整形路径 `tx_deferred` 跨拍持有（上限 4MiB）⇒ 在 `drain_tx`/`on_tx`
    处回收 = 同一分配两个可变属主 = 串包。
  - **登记**（§6）：若未来手机核 `sample` 让该分配升为头部热点，可选**受限形态** = 池只服务
    wgcore 驱动环（归还点硬约束 = `pump_once` 的 `drain_tx` 消费循环结束处，`Interceptor`
    明确排除且永不调用 `recycle`），并把收益口径写成「省 malloc/free」。本批不做。
- **判据行影响**：无。

### F6 relay 热路径分配与全表扫描

- **条目**：6a/6b/6c（6d 登记不改）。
- **方案**：
  1. **`frame_bytes` → scratch**（`:958`）：`Relay` 加 `frame_scratch: Vec<u8>`；直发路径
     （`:969-980`）改 `self.frame_scratch.clear(); frame::encode_frame(kind, payload, &mut self.frame_scratch); send_to(&self.frame_scratch)`
     （`encode_frame` 已有 append 形态 `frame.rs:54-59`）。**等腿窗 `pend` 路径保留 owned**
     （`:962`，慢路径，需跨拍持有）。
     **收益口径订正（设计门 2.2）**：scratch 消掉的是**每包 alloc/free**；payload→帧缓冲的
     **memcpy 仍在且必要**（`encode_frame` = reserve + push + extend_from_slice）。
  2. **assoc 读**（`:500-502`）：缓冲改**主循环外的局部量**（`let mut assoc_buf = [0u8; 65535]` 提到
     `loop {` 之前，或 `Vec` 一次分配）——**不用结构体字段**（`self.assoc_read(&mut self, ...)`
     与 `&self.assoc_rx_buf[..]` 撞 E0502，评审 1.2 已复现）；`assoc_read` 签名
     `pkt: Vec<u8>` → `pkt: &[u8]`（全程只读已核），去掉 `b[..n].to_vec()`。
     备选落地形态（若局部量方案不便）：`let b = std::mem::take(&mut self.assoc_rx_buf); …;
     self.assoc_read(…, &b[..n]); self.assoc_rx_buf = b;`。
  3. **主 UDP 读缓冲**（`:480`）：`vec![0u8; 65535]` 提到主循环外（一次分配）。
  4. **孤儿扫描**（`:1264-1273`）：先 `let active: HashSet<[u8;8]> = self.assocs.keys().map(|k| k.label).collect();`
     再过滤 —— O(L×A) → O(L+A)。
  5. **per-peer count**（`:846`）与 `:750`：**不改**（仅建会话/腿换址路径，MAX_ASSOCS_TOTAL=1024
     上界内，登记 §6）。
- **涉及文件**：`crates/homeway-core/src/relay/mod.rs`。
- **风险**：低。scratch 复用需保证「同一调用内不再重入」——`forward_up` 内 send 后即返回，
  无重入；`assoc_read` 只读借用不改变逻辑。
- **测试计划**：① 单测：scratch 编出的帧与 `frame::frame_bytes` **逐字节相同**（对照断言）；
  ② 回归 relay 既有单测（转发/等腿窗回放/双栈/伪源拒绝）；③ 经中继 E2E（`tools/local-rust-relay.sh`
  形态）走一轮 speedtest 不回归。
- **预期收益**：中继形态每上行包 1 次 alloc/free 消失（**memcpy 保留**）；每事件 64KB 分配
  消失；控制连接关闭 O(L×A)→O(L+A)。本地中继臂判据 = 不回归 + `sample` 叶帧。
- **测量臂**：§4 的中继臂。
- **判据行影响**：无（wire 不变）。

### F7 `udpcap` 周期订正（bindwatch 形态 ~600s → ~300s）

- **条目**：7（形态限定见 §1 表）。
- **方案**（`server/engine.rs:1800-1801`）：
  ```rust
  match kick_rx.recv_timeout(UDPCAP_INTERVAL) {
      Ok(()) => {}                     // kick：立即重探
      Err(mpsc::RecvTimeoutError::Timeout) => {}   // 到期：直接重探（不再多睡一拍）
      Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(UDPCAP_INTERVAL), // 看护未起：防自旋，按节拍继续
  }
  ```
  为可测性把分支抽成纯函数（入参 = `recv_timeout` 三态，返回「是否 sleep」），单测覆盖三态。
- **涉及文件**：`crates/homeway-core/src/server/engine.rs`。
- **风险**：极低（分支语义显式化，`Disconnected` 的「不空转」保留；`--bind-interface none`
  形态修复前后同为 ~300s，零行为变化）。
- **测试计划**：① 单测三态（**主判据**）；② 实测（可选，仅 **auto/显式绑卡形态**——本设计的
  `local-rust-exit.sh` harness 固定 `--bind-interface none`，**跑不出 300 vs 600 的差异**）：
  起 auto 形态出口观察两轮「UDP 默认路径：…」行时间戳差 ≈300s。
- **预期收益**：非 CPU 热点；价值 = 行为对齐 Go 真源（`udpcap.go:26` 5min）+ caps 结论新鲜度。
- **测量臂**：单测为主；auto 形态日志时间戳对账为辅。
- **判据行影响**：无（`UDP 默认路径` 行文/语义不变，仅频次；该行在 `INTEROP-CRITERIA.md:119`
  为 `—` 行）。**另注**（§3）：caps 位（喂客户端 C14）新鲜度从 ≤10min 变 ≤5min，属
  「行文不变、数值语义变化」——按该表惯例在 §3 加注记（不占判据行登记）。

### F8 状态快照——**裁决：整条不做**（含初稿的「单快照微项」）

- **条目**：8。
- **裁决与证据**：
  1. 分层缓存无收益：4Hz × 单次 ≤40µs（**估算，未实测**）⇒ ≤0.02% CPU；且 `elapsedMs` 每调用
     必变，任何缓存都要保留每调用拼装路径；`tun_status_json` 是**字节合同**
     （`fixtures/vectors/tun_status.jsonl` 10 案 + 键序断言），改动风险与收益不成比例。
  2. **连「单快照微项」也不做**（设计门第二段复核，采纳）：初稿想把 `tun_status()` 里两次
     `stage.snapshot()`（`:376` 与 `tun_running_inner` `:397`）合成一次——但 Go
     `tunRunningValue()`（`tunmode.go:545-556`）**自己就再读一次** `tunStageSnapshot()`，
     且注释明确接受「三个信号合起来不是一致快照」。合成一次读 = **朝 Go 反方向收紧**：
     JSON 字节虽不变，`state`/`running` 在转换瞬间的取值组合不再与 Go 逐位同序，零收益
     却动字节合同函数的调用序。
  - 结论：**不改代码**；若未来要重构状态面，归 Q-F「状态分裂」批按需处理。
- **测试/判据行影响**：无（不触代码）。

---

## 3. 判据行与观测面影响汇总

| 项 | 判据行行文 | 计数输入集/数值语义 | 说明 |
|---|---|---|---|
| F1 | 无 | 无 | `tx_backlog` 非观测面；水位门/兴趣位/cc 观测读 remaining，口径不变 |
| F2 | 无 | 无 | 缓冲管理内部 |
| F3 | 无 | 无 | 同上 |
| F4 | 无 | 无 | 调试开关非判据面（语义：启动期读取一次） |
| F5-1 | 无 | 无 | 内部缓冲 |
| F6 | 无 | 无 | wire 帧字节不变（scratch 对照单测保证） |
| F7 | 无 | **注记**：`UDP 默认路径` 行频次 ~10min→~5min（bindwatch 形态）；caps 位（喂客户端 C14）新鲜度 ≤10min→≤5min | 该行为 `INTEROP-CRITERIA.md:119` 的 `—` 行（非编号判据行），行文/语义不变、只变频次 ⇒ 按「行文不变、数值语义变化」惯例在此登记，不占判据行变更附录 |
| F8 | 无 | 无 | 不改代码 |

**结论：本批预期零判据行变更；唯一"数值语义变化"面 = F7 的 udpcap 频次/caps 新鲜度（上表注记）。**
若实现期出现任何偏差（例如 F6 scratch 复用引入字节差异、F1 水位口径漂移），必须在该批 commit
内按 `docs/INTEROP-CRITERIA.md`「判据变更记录」节登记，不得静默。

---

## 4. PERF-AB 测量与验收计划

### 4.1 纪律

- **安静环境**：跑前 `uptime` 记录 loadavg（1/5/15）；判决只在 **1min loadavg ≤ 4** 时作数
  （阈值出处 = ROADMAP 2026-10-08 实证：「同二进制 load≈4 → 203-341Mbps；load≈30 → 9-85Mbps」；
  §9.15.1 只规定**记录** loadavg，不规定阈值——初稿出处引用已订正）。
- **每轮记录 loadavg 表头**（含采样时刻）；判决用**同小时段、同带内**的臂间中位差；跨带
  绝对值不作比较（含历史上的 749/568 等消融臂——它们出自 loadavg 5-10 且部分用已删除的
  `HOMEWAY_TX_SENDTHREAD` 缝）。
- **交替轮**：A,B,A,B,A,B（A = 实现前 commit 构建，B = 实现后），各 3 轮取中位；臂切换 =
  重建二进制 + 重启本地出口（state/token 保持同一）。
- **隔离**：只用本地私有实例（端口 4265x/4266x 段、`tools/local-rust-exit.sh`）；两台生产
  出口（launchd 41641 / 阿里云）、tier/homeway 两仓、`baseline/` 全程不碰；测毕停实例清进程。
- **产物**：`/tmp/qi-ab/{speedtest-*.json,sample-*.txt,cpu.tsv,rss.tsv,loadavg.tsv}`（不入库；
  判据数字与结论固化进 `docs/PERF-AB.md` 新节 + `docs/reviews/QI.md`）。

### 4.2 臂与口径

| 臂 | 形态 | 采什么 | 用途 |
|---|---|---|---|
| **lo0 判别臂** | `local-rust-exit.sh` + token `--loopback-only` + Rust 客户端统一进程 + `speedtest --down 15s --up 15s --streams 4` | down/up MB/s（3 轮中位）、`sample <exit_pid> 20` 叶帧计数、**进程累计 CPU**、RSS | 主判据（本轮现状：down 87MB/s / up 128MB/s，loadavg 3.5-3.9） |
| **en0 产品形态臂** | 同上但不加 `--loopback-only`（采纳 `192.168.3.12:42659`） | 同上 | 产品形态 CPU 收益（吞吐受 sendto 税支配，预期变化小） |
| **中继臂** | `tools/local-rust-relay.sh` + 出口挂 relay + speedtest | 吞吐不回归 + relay 进程 `sample`（malloc/memcpy 叶帧） | F6 专用（量级小，判「不回归 + 叶帧下降」） |
| （手机核） | 本地无设备 | 结构性证据 | F5-1：代码级 + 单测；真机另议 |

**CPU 口径（设计门 6.1 订正）**：**不用 `ps -o %cpu`**（macOS 是 ≈1s 短窗衰减值——实测烧核
5s 后 1s 内回落 2.0%，采样时点决定读数）。改用**累计 CPU 时间差**：
`ps -o time= ` 测前/测后差 ÷ 墙钟 = 归一核数（`time=` 为累计值，已实测验证）；
分母写明「出口进程全部线程合计」（驱动/发送/dns/files/term…），需要机制归因时附
`sample` 的驱动线程忙时占比。单位写「相对值 %」。

**RSS 判据（设计门 3.3/6.1）**：1Hz 轮询取 max（PERF-AB §3 口径）；**B 臂 ≤ A 臂 ×1.1**
（拦 F1 的容量口径最坏 +~0.5MiB/流），超阈按告警复测，不得无声通过。

### 4.3 逐条判绿 / 证伪

**总判据（必达）**：同负载带内 B 相对 A —— ① 各条目标叶帧消失或占比降到目标值（下表）；
② **同吞吐下出口进程 CPU（累计 time= 口径）相对下降 ≥5%**（F1-F4 合计的模型预期：驱动线程
忙时少 ~1/4 ⇒ 单核约 7-9 个百分点，但摊到整进程可能不足 5%，故取 ≥5% 相对值 + 机制归因
双重确认；单位与分母见 §4.2）；③ 吞吐**不回归**（lo0 与 en0 两臂 down/up 中位相对 A ≥ -2%）；
④ **RSS ≤ A×1.1**。
**增益判据（期望达标）**：lo0 臂 down 中位 **≥ +2%**（若驱动线程为瓶颈可到 +5-8%）。
**证伪（出现即该 F 无效/回退）**：
- F1：叶帧未降（<50% 降幅）或吞吐回归 >3% 且复现 ⇒ 回退该 F；
- F2：`consume_step→__bzero` 叶帧仍 >0 ⇒ 定位残留 memset（可能有第二来源）；
- F3：`__bzero` 叶帧未降 ⇒ 零初始化不在所指位置，重定位；
- F4：驱动/发送线程内、且落在 6 个目标站点调用链的 `getenv` 叶帧仍出现 ⇒ 有漏改站点
  （进程内其它合法 env 读者不计——设计门 6.3）；
- 全体：若「叶帧全消失但 CPU/吞吐无变化」⇒ 该热点与瓶颈无关，按 §9.7-bis 记「收益 < 带内
  噪声」，不虚报。

| 项 | 目标叶帧（现状 → 目标） | 臂 | 附带断言 |
|---|---|---|---|
| F1 | `flush_backlog→memmove` 2756 样本（17.1%）→ **<3%**（模型 ≈1%） | lo0 + en0 | 吞吐不回归；`send_slice` 叶帧仍在（必要拷贝）；RSS ≤A×1.1 |
| F2 | `consume_step→__bzero` 432（2.7%）→ **0** | lo0 + en0 | `step_of` 叶帧不变（F2b 不做） |
| F3 | `service_sockets/reactor_turn → __bzero` 248（1.5%）→ **≤0.5%** | lo0 | 读写内容回归测试全绿 |
| F4 | 目标站点 `getenv`（驱动 101 + 发送 ≈52）→ **0** | lo0 | `HOMEWAY_TX_DBG=1` 启动仍能打 `[TXDBG]` |
| F5-1 | 无叶帧判据（非本地路径） | — | 单测全绿 + alloc 站点消失（**结构性判据**；数字标估算） |
| F6 | relay 进程 `sample` 中 `frame_bytes` alloc 叶帧消失；吞吐不回归 | 中继臂 | 帧字节对照单测 |
| F7 | 三态单测（主）；auto 形态日志时间戳 ≈300s（辅） | 单测 | — |
| F8 | 不设（不做） | — | 既有 tun_status 单测不受影响 |

### 4.4 复现命令（可直接跑；2026-10-08 实测有效）

```bash
# 0. 构建（A 臂 = 实现前 commit 检出后同法构建；B 臂 = 实现后）
cargo build --release -p homeway-cli
uptime                                   # loadavg 表头（每轮记录）

# 1. 本地出口（私有实例，#9 = 42659；先 wipe 保证干净）
tools/local-rust-exit.sh wipe 9; tools/local-rust-exit.sh start 9

# 2. 客户端统一进程（serve/relay 双关）+ loopback-only token
rm -rf /tmp/qi-client && mkdir -p /tmp/qi-client
printf '[serve]\nenabled = false\n[relay]\nenabled = false\n' > /tmp/qi-client/config.toml
nohup target/release/homeway-cli --state /tmp/qi-client --verbose > /tmp/qi-client/stdout.log 2>&1 &
RAW=$(target/release/homeway-cli serve token --state /tmp/homeway-rs-rustexit-9 | grep -o 'hmw1[A-Za-z0-9+/=_-]*' | head -1)
LOOP=$(target/release/homeway-cli token "$RAW" --loopback-only)
target/release/homeway-cli host add --state /tmp/qi-client --name qi "$LOOP"

# 3. 测速 + 同步采样出口进程（sample 窗口跨 down 尾/up 头——A/B 同窗口即可）
EXIT_PID=$(cat /tmp/homeway-rs-rustexit-9/pid)
CPU0=$(ps -o time= -p $EXIT_PID); T0=$(date +%s)
(target/release/homeway-cli speedtest --host qi --state /tmp/qi-client --json --down 15s --up 15s --streams 4 > /tmp/qi-speedtest.json 2>&1 &)
sleep 3
sample $EXIT_PID 20 -file /tmp/qi-sample.txt
sleep 16                                   # 等 speedtest 走完
# 4. CPU（累计口径）：time= 差 ÷ 墙钟
CPU1=$(ps -o time= -p $EXIT_PID); T1=$(date +%s); echo "cpu_delta=$CPU0 -> $CPU1 wall=$((T1-T0))s"
# 5. 收工
pkill -f "homeway-cli --state /tmp/qi-client"; tools/local-rust-exit.sh stop 9
# 6. 读叶帧（关键行）：
grep -E "flush_backlog|consume_step|__bzero|getenv|step_of|send_slice" /tmp/qi-sample.txt
```

（en0 臂：第 2 步去掉 `--loopback-only`；中继臂：`tools/local-rust-relay.sh` 起本地中继 +
出口 `--relay <token>`，形态见 `tools/perf-ab.sh`。RSS：`while :; do ps -o rss= -p $EXIT_PID; sleep 1; done` 取 max。）

---

## 5. 设计门记录

> 第 1 棒设计门 = `reviewer` skill（dsh headless 外部评审）。
> **轮次目录**：`/tmp/dsh-review/r5.SjItld/`（prompt.txt / output.md / stderr.log）；
> 评审者在其内部另起了一轮子评审（其输出自称 `r1.koDI2G`），本轮 output.md 含
> 「外部评审原文摘要 + 其独立复核」两部分。

### 5.1 结论

**exit code = 0（dsh 成功）**；评审意见 **14 条主项**（含 1 高 / 7 中 / 6 低）+ 行号订正 8 条，
另由评审者的独立复核追加 4 条。**逐条处置：认同 21 / 部分认同 1（举证订正，结论采纳）/
不认同 0**。评审独立反汇编复核了 F1/F2/F3 三个叶帧、编译复现了 F6-2 的 E0502、实测了
`ps -o %cpu` 的短窗语义、回读 boringtun 源码与 Go 基线——**未发现误报**。
**过门结论：通过（v2）**。全部高危/中等意见已在 v2 消化：F5-3 降级不做（登记）、F6-2 落地
形态写明、F7 口径限定、F1 容量口径 + 摊还常数订正、CPU 仪器更换、iovec 漏项登记、行号订正；
F8 由「微项」改为**整条不做**（采纳复核意见）。

### 5.2 评审原文摘要（逐条）

| # | 评审意见（摘要） | 严重度 |
|---|---|---|
| 1.1 | **F5-3 `stackb` 池化不可实现 + 别名**：token 只持 `SharedQueue` 拿不到池；清空 len 后 `resize` 必整段 memset（省不掉）；`TunDevice` 同时是 Interceptor 的设备，`drain_tx` 产物会移动进 `tx_out`/`tx_deferred` 跨拍持有 ⇒ 回收 = 双可变属主 | 高 |
| 1.2 | **F6-2 借用编译不过**：`&self.assoc_rx_buf[..n]` 传进 `&mut self` 方法 = E0502（已写最小 repro 复现）；须写明 `mem::take` 往返或拆两段 | 中 |
| 1.3 | **F3 方案二借用理由不完整**：真阻塞点是 UDP 臂 `self.udp_send_to_client(flow, &buf[..n])`（`:2002`）；TCP 臂字段形态可行；方案二仅适用 `service_sockets`（评审原写的 `:2006`/`:2040` 举证不准） | 中 |
| 1.4 | F1/F2/F3 其余清零与借用语义、`VecDequeLite` 等价性、**本批不需要 unsafe** —— 看过，没发现问题（反汇编独立复核三叶帧） | — |
| 2.1 | F1/F2/F4/F7 是实测真热点、推导成立；F1 机制指令级坐实（放大 ≈30×） | — |
| 2.2 | **F6-1 收益夸大**（scratch 只省 alloc，copy 必留）；**F7 现状 ~600s 需限定形态**；F8「不做分层」站得住但依据是估算；F5-3 收益不可达 | 中 |
| 2.3 | F5-1/F5-2 的裁决（做/不做）与账 —— 看过，没问题 | — |
| 3.1 | **F7 取证口径不成立**：`--bind-interface none`（= 本文 harness）走 Disconnected ⇒ 现状本就 ~300s，「300 vs 600」不可复现；须限定 bindwatch 在位形态 | 中 |
| 3.2 | F1 换类型后的水位门/FIN/EOF 站点集 —— 看过，没发现问题 | — |
| 3.3 | **`VecDequeLite` 内存上界推导漏容量**：`len<2×remaining` 只约束 len；容量可到 ~1MiB/流（现状 ~512KiB）⇒ 改口径 + RSS 判据 | 中 |
| 3.4 | 其它边界（FIN 挂起、DNS 腿 push、`n=0`、pend owned、孤儿 HashSet 类型）—— 看过，没发现问题 | — |
| 4 | 零判据行变更成立；建议给 F7 的 caps 新鲜度补一行注记 | 低 |
| 5 | Go 直译：除 F5-3 的 `sync.Pool` 味（+ `envflag` 包级函数）外无新痕迹 | 低-中 |
| 6.1 | **CPU 仪器不可用**：`ps -o %cpu` 是短窗衰减值（实测 92.8→2.0/1s）；须改累计 `time=` 差 + 写明分母/单位；RSS 需显式判据 | 中 |
| 6.2 | **§0.2「749 同带」不成立**（749 出自 loadavg 5-10 且用已删除的 `HOMEWAY_TX_SENDTHREAD` 缝）；「≤4 阈值」出处应标 Roadmap 实证而非 §9.15.1 | 中 |
| 6.3 | F4 证伪条件须限定站点范围；F3 目标 `<0.3%` 偏紧（改 ≤0.5%）；F5/F8 的数字应标「估算」 | 低 |
| 7.1–7.7 | 行号/计数订正：F1 站点「9」实为 11（+测试 `:4493`）；`:1786` 非建 pollfds（建在 `:1912-1919`）；`server/bind.rs` collect 在 `:1044`；`iface.poll` 占比低估；boringtun 契约分面写；`tun_running_inner` 第二调用点；旧样本分母笔误 | 低 |
| 7.8 | **批次真源漏项**：ROADMAP 前段列有「`relay` sendmsg iovec」，设计既未实装也未登记 | 中 |
| 复核-1 | **F8 的微项与 Go 相反**：Go `tunRunningValue()` 本就再读一次快照（`tunmode.go:545-556`）⇒ 单快照是朝 Go 反向收紧；建议 F8 整条不做 | 低-中 |
| 复核-2 | F1「涉及文件」还漏 `:2350`（`send_slice` 关键转换点） | 低 |
| 复核-3 | 样本时长口径：产物 16070 样本 ≈16s，与命令写的 22/20 对不上 | 低 |
| 复核-4 | 采样窗口跨 down/up 相位（17.1% 被稀释）；「百分点 vs %」单位混用 | 低 |

### 5.3 逐条处置表

| # | 处置 | 落到 v2 的位置/证据 |
|---|---|---|
| 1.1 | **认同**（高危）——F5-3 降级为「不做/登记」，只做 F5-1；受限形态（池只服务 wgcore、归还点硬约束、Interceptor 排除）写入 §6 作为将来选项。三条理由逐条核实：token 无池通路（`stackb.rs:134-148`）、清空 len 后 resize 必 memset、Intercept 侧 `on_tx`/`tx_deferred` 跨拍持有（`intercept/mod.rs:38/762/822/1868-1887`） | §2 F5、§6 |
| 1.2 | **认同**——F6-2 写明落地形态：主循环外局部缓冲 + `pkt: &[u8]`；`mem::take` 往返列为备选 | §2 F6-2 |
| 1.3 | **部分认同（举证订正）**——机制对：采纳「方案二仅适用 `service_sockets`」；订正评审举的 `:2006`（应为 `:2002` 的 `udp_send_to_client`）与「TCP 臂同阻塞」的表述（`on_upstream_eof` 不接收缓冲）。统一策略：`read_upstream` 两臂走循环外提 | §2 F3 |
| 1.4 | 记录（无处置） | §5.1 |
| 2.1 | 记录；其中「F1 改后摊还 ≤1B/B」由复核-5 订正为 **≤2**，一并采纳 | §2 F1 |
| 2.2 | **认同**——F6-1 改成「省 alloc/free，memcpy 保留」；F7 加形态限定；F8 依据标「估算」（并整条不做，见复核-1） | §2 F6/F7/F8、§6 |
| 2.3 | 记录 | §2 F5 |
| 3.1 | **认同**——§1 表条目 7 限定「bindwatch 在位 = ~600s；`--bind-interface none` 本就 ~300s」；测试计划②改 auto 形态或删（主判据 = 三态单测） | §1、§2 F7 |
| 3.2 | 记录 | — |
| 3.3 | **认同**——F1 内存改「len/容量」双口径（最坏容量 ≈1MiB/流）；§4.3 增 RSS ≤A×1.1 判据 | §2 F1、§4.2/4.3 |
| 3.4 | 记录 | — |
| 4 | **认同（低优先）**——§3 表 F7 行加 caps 新鲜度注记（不占判据行登记） | §3 |
| 5 | **认同（F5-3 消解）**；`envflag` 包级函数保留并写明理由（同开关 6 站点共用、与 `ack_drain_bytes` 先例同族；进 config 装配面需穿三层） | §2 F4 |
| 6.1 | **认同**——§4.2 CPU 改累计 `ps -o time=` 差 ÷ 墙钟；写明分母 = 进程全部线程、单位为相对 %；RSS 判据进 §4.3 | §4.2/4.3、§4.4 |
| 6.2 | **认同**——§0.2 判读 5 删「同带 749」措辞，改为「与 lo0 形态 413-568 同量级、环境可用」的旁证；§4.1 阈值出处改 ROADMAP 实证 | §0.2、§4.1 |
| 6.3 | **认同**——F4 证伪限定到「驱动/发送线程 + 6 个目标站点调用链」；F3 目标 ≤0.5%；F5/F8 相关数字标「估算，未实测」 | §4.3、§2 F3/F5/F8 |
| 7.1–7.7 | **认同**——全部订正：F1 站点清单改「11 代码站点 + 1 测试（含 `:2350`/`:4493`）」；`:1786`→`:1912-1919`（建 pollfds）；`server/bind.rs` collect `:1044`；`iface.poll` ≈1800/≈11%（口径注明）；boringtun 契约分面（数据面 panic / 握手面 Err）；F8 不再涉及 `tun_running_inner` 改签名；旧样本分母 14566 订正 | §0.2、§1、§2 |
| 7.8 | **认同**——`relay sendmsg iovec` 显式登记「不做 + 理由」（设计已选安全形态；中继不在本机判据面；iovec 需 `sendmsg`+unsafe），不静默消失 | §1 表 6e、§6 |
| 复核-1 | **认同**——F8 整条不做（含原「单快照微项」）：Go 双读已核（`tunmode.go:545-556` + `tunStatusJSON` `:565`），单快照 = 反 Go 收紧 + 零收益 + 动字节合同函数 | §2 F8、§6 |
| 复核-2 | **认同**——F1 涉及文件补 `:2350`（并补 `:4493`） | §2 F1 |
| 复核-3 | **认同**——§0.2 写明产物每线程 16070 样本（采样器未满 1kHz ≈16s 有效）；§4.4 命令与产物口径对齐 | §0.2、§4.4 |
| 复核-4 | **认同**——§0.2 注明采样跨相位（17.1% 为稀释值，不影响 A/B 判决）；F1 收益的单位统一为「相对值」 | §0.2、§2 F1 |

### 5.4 不认同项

**无。**（唯一立场差：评审 2.1 沿用了初稿的「摊还 ≤1B/B」常数，其自身复核在第二段已订正为
≤2——本设计按订正后的 ≤2 落地，见 §2 F1。）

---

## 6. 不做与残余登记（防「静默漏做」）

| 项 | 结论 | 证据/理由 |
|---|---|---|
| 条目 2 `encap_peer`/`step_of` 的 `to_vec` | **不做** | 实测 `step_of` 合计 75 样本（0.47%，malloc/memmove 各半）。替代形态「右尺寸直写」仍要 alloc+memset（实测 `vec![0u8;1300]` 33.8ns vs `to_vec` 40.8ns，省 ≈7ns/包 ⇒ <0.05%）；真正归零需跨线程缓冲池（复杂度/风险 ≫ 收益）。**若代码门坚持处置，最低风险形态 = 设备侧右尺寸直写**。 |
| 批次真源「`relay` sendmsg iovec」 | **不做（登记）** | 本批中继走「scratch 复用 + `assoc_read` 借用」安全形态；iovec（`sendmsg` + `msghdr`）要 unsafe 且改发送面，而中继不在本机判据面（Mac 生产走直连）；若将来中继 CPU 成为目标再评估（可连 memcpy 一起省）。 |
| F5-3 `stackb` `DevTxToken` 池化 | **不做（登记）** | 设计门 1.1 三条（池不可达/省不掉 memset/Intercept 别名）。将来若要：池只服务 wgcore 驱动环 + 归还点硬约束（`pump_once` 消费循环后）+ Interceptor 永不 recycle + 收益口径「省 malloc/free」。 |
| F5-2 `resolve_pending` 64KB 复用 | **不做** | 交付需 owned Vec；bulk 读净收益 ≈0（省 memset、多拷贝）。 |
| F8 状态快照（分层缓存 **与** 单快照微项） | **不做** | §2 F8：4Hz×≤40µs（估算）≤0.02% CPU；字节合同风险；单快照与 Go 双读相反。归 Q-F 视需要。 |
| 条目 4c `flows.keys().collect()` | 可选微项 | 实测 ≈14 样本（0.09%）。实现可顺手做「成员缓冲复用」；不做也不影响判据。 |
| 条目 4d 新流全表 `count` | **不做** | 仅新流路径，非每拍；维护增量计数引入双计数一致性负担，收益 <0.01%。 |
| 条目 6d `RelayLog::logf` 加缓冲 | **不做** | Go 基线同为无缓冲单写；加缓冲 ⇒ 崩溃丢行、需 flush 生命周期；Q-C F6 后无逐包日志。 |
| `reactor_turn → poll(0)` 12.7% | 观察项（超本批范围） | 每拍一次零超时 poll syscall；结构性问题（reactor 与 UDP 循环共用线程），要动须换统一事件循环（kqueue/epoll），登记后续批。 |
| 发送线程 `msgs` 每轮 Vec 分配 | 观察项（超本批范围） | `server/bind.rs:1043-1044` 每轮 `map().collect()`（既有注释已登记「后续」）；实测 ≈0.4%。 |
| `tx_backlog` 直读进栈环（免中间缓冲） | 可选后续 | 会改背压上界（256KB/流 → 栈环 1MB/流）与 FIN 挂起时序 = 行为面改动；F1 后若 `send_slice` 拷贝成为新热点再评估。 |
| 哈希表 `SipHash`（热路径 HashMap 查询） | 观察项 | sample 中 `Hasher::write` 散布各分支（合计 ≤0.5%）；换 hasher 属全局决策，登记后续。 |
