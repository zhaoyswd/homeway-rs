# Q-C 设计文档 — 入口安全与资源上限（wtransport / 中继 / 设备表）

> 批次：Q 批整改第三批（`docs/REVIEW-ROADMAP.md` §Q-C）。第 1 棒（设计）产出。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`（Q-C 节）+ `docs/INTEROP-CRITERIA.md`（判据行政策）。
> 基线：`git HEAD = 938007d`。行号均为复验时（本 HEAD）实测值，实现时以符号定位为准。
> **边界**：本文档只设计，不含产品代码改动。
> **状态**：设计门（dsh 外部评审）**已过**——见 §4；本文档已按评审意见修订（**v2**）。

---

## 0. 复验方法

逐条回源码重定位：设备表淘汰/落位序列、`wtransport::bind` 采纳与 reg 搭车路径、
端点学习缓存、中继腿表/转发/fallback 分支、probe 应答与 build、`domain_eps`/`tunnel_addr`/
`frame` 边界。凡涉及 `baseline/homeway`（Go 只读 oracle，锚 `d4148f6`）处一并逐行核对。

**复验的核心结论（贯穿全批）**：本批多数条目的「根因」在 **Go 基线上同样存在**——它们不是
Rust 直译偏差，而是**两端共享的入口健壮性缺口**。故本批条目分两类：

- **A 类 · 直译偏差**（Rust 与 Go 行为不一致，修 = 回归对齐）：P0-2 的 ops 序列、中继 assoc
  socket 家族。
- **B 类 · 共享硬化**（两端同缺口，修 = **有意分歧**，须登记）：采纳时序收紧、reg 补投、
  端点缓存上界、中继腿表、拒绝日志限流、v1 fallback 源绑定、转发失败计数、撞车守卫、
  `same_candidates`。
- **C 类 · 不可观测/需另立通道**（v2 新增，见 §4 处置）：中继侧无 JSON 遥测通道 ⇒ 部分「入状态面」
  的修法在本批**无法交付**，只能降级为「日志 + 单测观测」或另立通道。

**A 类**修完即回归对齐，判据行照旧同串；**B 类**须按 `docs/INTEROP-CRITERIA.md`「判据变更记录」
节登记（§3 逐条列出）。这是本批最重要的设计判断——把「修 bug」与「有意分歧」分清楚。

---

## 1. 复验结果表

| 条目 | 真伪 | 现行源码位置（HEAD 938007d） | Go 基线口径 | 结论 |
|---|---|---|---|---|
| **P0-2** 设备表 `evict_stale` 删表项不产 `Remove` | ✅ 成立 | `table.rs:406-428`（`evict_stale` 返 `bool`，`remove` 表项后**不产 op**）；调用侧 `table.rs:330`（仅当门用）；成功路径只返 `vec![DevOp::Add{..}]` `:369`；`engine.rs:1274-1276` `apply_dev_ops` 只吃成功返回 | `peers.go:508` `removeLocked(victim)` → `:579-585` **同步** `applyDeviceOp(RemovePeer)`；Register `:426-432` 淘汰后落 Add | **成立**。`Device.peers`（boringtun）不屈从淘汰：被淘汰设备仍可路由（`device.rs:86/171-176`）并过源校验（`device.rs:316`）。`device.remove_peer` 全仓生产唯一调用点 = `engine.rs:1296`（`apply_dev_ops`）⇒ ops 丢失即孤儿 peer **永久驻留**。**A 类**。 |
| **P1** 路径采纳先于帧解码 | ✅ 成立（措辞订正） | `bind.rs:503-504`：`self.adopt(src)` 在 `frame::decode_frame` **之前**；任意来源（含 1 字节垃圾）触发 `adopt`（写 `adopted`/`race_seen` + 打 C5/C6 + 登记 handover） | Go `bind.go:499-501` **同样**先 `b.adopted = src`；Go 在 `bind.go:511-515` 自注为**已知限制**并写明后续收紧方向「收紧到『数据帧/候选来源』」 | **成立**。Rust 与 Go 一致（**非直译偏差**）；本批修 = 落实 Go 自己注记的收紧方向。**B 类**。 |
| **P1** reg 一次性消费 | ✅ 成立 | `bind.rs:310`（`peek_reg`）、`:360-361`（`sent>0` 即 `reg_armed=false`）、`:456-458`（门控）、`:630-632`（`refresh_reg` 需 `adopted`）；rearm 才重 arm `:766`/`:785` | Go `bind.go:637-640` **同样**在首次 `Send` 即消费 `regArmed`（更早——写出前就消费） | **成立**（Rust 消费点比 Go 更晚/更严，是 R1 评审的收紧）。**B 类**。 |
| **P1** 端点缓存/候选/`race_seen`/`send_err_log_at` 无上界 | ✅ 成立 | `endpoint_cache.rs:68-76`（无 cap）、`:219-231`（`observe`）、`:234-246`（`mark_verified`）、`:249-264`（`entries()` 只按 TTL 过滤）；`bind.rs:127`（`race_seen`）、`:145`（`send_err_log_at`）只增（`race_seen` 仅 rearm 清 `:770/:789`，`send_err_log_at` 全仓无清理点）；注入面 `probe.rs:108`（`take(MAX_ENDPOINTS=8)`）、`:235-261`（`probe_candidates` 逐条 `on_endpoint`） | Go `endpointcache.go` **同样无 cap** | **成立**。**B 类**。 |
| **P1** 中继未认证占表 | ✅ 成立 | `relay/mod.rs:573-598`：HELLO 分支 `:560` 只校验 `relay_id(pub)==label`（**label 由 pubkey 派生，自洽即可**），随即 `:584-597` **插入未验证腿**占 `legs` 槽；PROOF 校验在 `:612-664`；`max_legs=256`（`:52`）、`LEG_BOOTSTRAP=30s`（`:48`） | Go `leg.go:62-78` **同样** HELLO 即插腿并受 `MaxLegs` 闸 | **成立**。**B 类**。 |
| **P1** 中继热路径无节流逐包同步写盘日志 | ✅ 成立（措辞订正 + 清单补全） | 拒绝类日志：`:576-583`（max-legs 拒绝）、`:766-772`（per-peer 上限）、`:621-628`（PROOF 版本不符）、`:655-661`（token 校验不过）、`:985-988`（并发握手上限）；`logfile.rs:93-98` `logf` → `RotatingWriter::write_line`（**同步、无缓冲、驱动线程内**）；`stamp()` 在 `logfile.rs:22-56`（`localtime_r` `:33`）；`MAX_BYTES=2MB×3`（`:17-18`） | Go `assoc.go:65`/`leg.go:67` **同样**逐包无节流；`logfile.go:55-72` 同样 `w.f.Write` 无缓冲 | **成立**。审计「`:765-772`（per-peer 上限，**每包一行**）」措辞**不精确**（该行只在超限丢弃时打，洪水下才退化为每包一行）。**评审补全**：`handle_control` 另有 `:621-628`/`:655-661` 两处每包可达、未认证可达。**B 类**。 |
| **P1** fallback v1 会话无源认证 | ✅ 成立（描述订正） | `:921-925`（`sid==0`：任意 `from` 续命）、`:951-956`（`:955` 原样回投）、`:435-447`（assoc 读**无限流**、单包/事件、每事件 64KB 分配）、`:774-782`（建 assoc 即 `bump_sock_bufs` 4MB×2，**未认证来源也建**） | Go `assoc.go:205-208` 同 v1 分支、同无源绑定；`assoc.go:68` `net.ListenUDP("udp", nil)`（双栈） | **成立**。**订正（评审 D3）**：`rate_limit=200` **不是**「不入任何状态/日志面」——`handle_udp_packet` `:521` 的 `rate_ok` 是**所有入站 UDP 的第一步**（probe/HELLO·PROOF/数据面），超限 `stats.dropped += 1`（`:522`），而 `dropped` **就打在 60s 的 R10 判据行里**（`:489-493`）。真实缺口 = 无**独立细分计数**。**B 类**。 |
| **P2** 转发静默丢包计成功 | ✅ 成立 | `:866-869`（`let _ = a.sock.send_to` 后无条件 `forwarded_up += 1`）、`:944`（pend 回放 `let _ =`）、`:955-956`（`forwarded_down += 1`）；assoc socket 恒 `UdpSocket::bind("0.0.0.0:0")` `:774`（v4-only） | Go `assoc.go:68` `net.ListenUDP("udp", nil)`（nil 地址 → 平台双栈，通常 `[::]`） | **成立**。**A 类（家族偏差）+ B 类（失败计数）**。 |
| **P2** 每包分配 / 全表扫描 | ✅ 成立 | `:855`（`frame_bytes` 堆分配/包）、`:437-439`（assoc 读每事件 64KB + 单包）、`:765`（per-peer 全表 `count`）、`:1093-1102`（孤儿扫描） | Go 同形 | **成立**。**性能面**，归 Q-I。 |
| **P2** 控制面降级不可见 | ✅ 成立（口径订正） | `:293-310`（TCP 同号口 bind 失败仅一行，无状态位）；分钟统计 `:490`（不含控制面状态）；**中继**探针 `:526` `respond_ex(pkt, self.build_str(), 0, &[])` **flags 恒 0** | Go `relay.go:262` 同样仅一行日志 | **成立**（**仅对中继**）。**订正（评审 D4）**：serve 侧 flags **不是 0**（`server/bind.rs:370` 传 `self.caps`，位定义 `engine.rs:150-154`：`UDPCAP_DNS=1<<0`…`SEEN=1<<4`，与 Go `udpcap.go:36-48` 同值），且 flags **已被 C14 判据行消费**（`INTEROP-CRITERIA.md:60` 三个动态字段 = bits 0/1/2/4 派生；`:119` 已记录实采 `flags=0x0b`；serve 侧 `engine.rs:1790` 打印该字节）。bit0–4 **已全占**。**B 类**。 |
| **P2** `probe::respond_ex` build 字节截断 | ✅ 成立（分类订正） | `probe.rs:95`：`if build.len() > 32 { &build[..32] }`——**按字节**切，非 ASCII 恰跨界即 panic（入站探测应答路径） | **订正（评审 D2）**：Go `probe.go:173-174` `build = build[:maxBuild]`（`maxBuild=32`）**同样按字节截断、且 Go 不 panic**（至多产生非法 UTF-8） | **成立**，但**归 Rust 特有缺陷**（与已修 `upnp.rs` H4 同族），**非 A 类直译偏差**。**生产不可达**（两个调用点 build 恒 ASCII 且 <32B，`relay/mod.rs:547-550` 恒 `"relay-dev"`、serve `engine.rs:109` 恒 `"homeway-rs-dev"`）。另：`relay` 的 `Config.build` 全仓**无赋值**（声明 `relay/mod.rs:73`、默认 `:95`）。 |
| **P2** `egress.rs` `if_nametoindex` 失败静默 index=0 | ✅ 成立 | `server/egress.rs:165-168`：`e.index = libc::if_nametoindex(...)` **不查返回值**；macOS 上 0 = 解绑且 `setsockopt` 成功 ⇒「已钉卡」判据反向 | Go 侧同族 | **成立**。AUDIT 在 Q-C 与 Q-J **两节都列**——本批 vs Q-J 边界见 F12。 |
| **P2 🔎** `domain_eps::same_candidates` 非多重集比较 | ✅ 成立 | `domain_eps.rs:361-374`：`y` 不消费，`a=[A,A,B]` vs `b=[A,B,B]` 判等 | — | **成立**。**评审补充**：仓库已有**正确实现** `bind.rs:827-836`（sort+比较）⇒ 存在第二份拷贝。误判后果比原判重：`refresh_async` 里 `apply_static(&fresh)` `:306` **无条件执行**、`if !changed { return }` 在其后，误判「未变」会**跳过中继档的节流软赛跑补投**（`:317-331`）并污染 `last_cands`。 |
| **P2 🔎** 派生隧道地址与 `SERVER_TUNNEL_IP` 无撞车守卫 | ✅ 成立 | `tunnel_addr.rs:36-44`（`derive_tunnel_ip` → `100.64.(v>>8).(v&0xff)`，`v∈[1,65534]`）；`wgcore/mod.rs:49` `SERVER_TUNNEL_IP = 100.64.255.1`（v=65281，**落在值域内**）；`table.rs:431-446`（`assign_ip` 只查表内条目） | Go `proto/tunneladdr.go:22` **同样**取值域含 `.255.1`、**同样无守卫** | **成立**（两端同缺口）。**评审补充**：`derive_tun_ip`（应用面）**同样**落 100.64/16，且服务端 `ip_taken` 是**双地址并集**判定（`table.rs:465-467`）⇒ 守卫须**两个派生函数共用**；碰撞**可故意研磨**（设备自选公钥；评审实测 ~5.3 万次得 `hw-tun` 撞车、~10.6 万次 `hw-app`）。**B 类**。 |
| **P2 🔎** `direct_first` `Some(0)` 哨兵 | ✅ 成立（含订正） | `bind.rs:183-189`：`direct_first.map_or(Some(DEFAULT), |d| if d.is_zero() { Some(DEFAULT) } else { Some(d) })`——`None` 与 `Some(0)` **都**映射成 `Some(DEFAULT)`；文档 `:110-111/:162` 却称「`None` = 显式关」 | Go `wgcore/core.go:117-124`：`0 → 2s`、**`<0 → 0`（显式关）**；`bind.go:664` `DirectFirst <= 0` 即解锁中继 | **成立**。真实缺陷 = **「`None` = 关」这条文档语义无实现**；`Some(0)→默认 2s` 与 Go `0→2s` **一致**。生产唯一装配点传 `Some(ZERO)`（`wgcore/mod.rs:1078`）⇒ **当前生产行为与 Go 一致、无实际危害**；缺陷在 API 表达力与文档。**A 类（文档/语义）**。 |
| **P2 🔎** `frame` 长度域静默截断 | ✅ 成立 | `frame.rs:110`（`encode_batch`：`let len = payload.len() as u16`；`:104` 是容量求和）、`:150`（`hint_bytes`：`(addr.len() as u16)`）——> 65535 静默回绕 | Go `proto.BatchMsg` 同族 | **成立**。现状不可达（数据面 MTU 与 hint 串远小于 65535）；防御性断言缺口。 |

### 误报 / 措辞订正记录

- **P1 采纳时序**：审计「1 字节垃圾可达」成立；但隐含「Rust 特有」的框架**不准确**（Go 行为相同，
  且 Go 自己注记此为实现层已知限制与后续收紧方向）。本批修 = 有意分歧（硬化），须登记。
- **P1 中继日志限流**：审计「每包一行」措辞不精确（超限丢弃时才打）；核心结论成立。
  **评审补全两处漏网点**（`:621-628`/`:655-661`）。
- **P2 `direct_first` 哨兵**：`Some(0)→默认 2s` 与 Go `0→2s` 一致（非缺陷）；真缺陷是「`None` = 显式关」
  无实现；危害面（生产无调用者）已写明。
- **P2 `probe` build 截断（评审 D2）**：**不是** A 类直译偏差——Go `probe.go:173-174` 同样按字节截断
  且**不 panic**；Rust 的 panic 是 **Rust 特有缺陷**（生产不可达）。
- **P1 v1 fallback 限流描述（评审 D3）**：`rate_limit=200` **是**「所有入站 UDP 的第一步」且超限进
  `stats.dropped`（**就打在 R10 里**）——原文「不入任何状态/日志面」**错误**；真实缺口 = 无独立细分计数。
- **P2 控制面降级（评审 D4）**：flags「恒 0」**仅对中继成立**；serve 侧 flags = `caps` 且已被 C14 消费。
- **行号订正（评审 D5）**：`relay/logfile.rs:65-86` 非 `stamp()`（`stamp()` 在 `:22-56`）；`frame.rs` 回绕
  点在 `:110`（`:104` 是容量求和）。
- **无整条误报剔除**：Q-C 全部条目复验**成立**（无一条整条剔除）；措辞/分类订正见上（含评审 D1 的
  §5 挂账错误，已删）。

---

## 2. 修复清单（v2，已并入设计门意见）

> 每条：方案 / 涉及文件 / 风险 / 测试计划 / 判据行影响。评审意见标注 `[门-Dx]`/`[门-Qn]`/`[门-Nn]`。

### F1（P0-2）设备表淘汰产出 `DevOp::Remove`（表-设备一致）——**只保留 A 版**

- **方案（评审 Q1 认同并订正：B 版必须删除）**：
  - **只做 A 版「先选后落」**：拆 `select_stale_victim(&self, now) -> Option<([u8;8], Entry)>`（**纯 `&self`**，
    选择逻辑与现状同：`idle > grace` 中最旧、严格 `>`）→ `assign_ip`/`assign_tun_ip` 加
    `exclude: Option<&[u8;8]>` 参数，冲突检查对**「表内条目 − victim」** → 校验通过后再 `remove` victim +
    打 E9 行 + push `DevOp::Remove`，最后插入新设备。
  - **删除原 B 版（先摘后查、接受残余不一致）**：`device.remove_peer` 全仓生产唯一调用点是
    `engine.rs:1296`（`apply_dev_ops`），而 `gc()`（`table.rs:396-399`）只遍历 `entries` ⇒ B 版错误路径
    产生的孤儿 peer **永久驻留** `Device.peers`、仍可路由/过源校验——**与 P0-2 同一 bug、只是概率低**，
    不是「稀有残余」。写进文档会诱导实现选错。
  - **禁用「回插补偿」**（评审 Q2）：回插会让**已打的 E9 行说谎**（行已发、表未变）。
  - 落位序列 = `[Remove(victim_pub), Add(new)]`，与 Go `removeLocked`(同步 RemovePeer) → `AddPeer` **同序**。
- **涉及文件**：`crates/homeway-core/src/server/table.rs`（`select_stale_victim`/`register`/`assign_ip`/
  `assign_tun_ip`/`ip_taken`）；`crates/homeway-core/src/server/engine.rs`（一致断言）。
- **风险**：低—中。**评审 N4 提示**：`table.rs:352-359` 自己记录「同公钥挂两个 devTag」的合法克隆形态——
  此时按 pubkey 发 `Remove` 会把**另一仍在表的 devTag 的 peer** 一并摘掉（`apply_dev_ops` 是
  `device.remove_peer(&pubkey)`）。**这与 Go 行为一致**（`peers.go:579-585` 同样按 pub 摘），属共享缺口；
  **测试断言必须写成弱不变量「同一 pubkey 只保留一条 peer」**，并在文档注明克隆场景已知差异，
  否则实现者会照测试改错方向。
- **测试计划**（评审 Q4/N4 补格）：
  1. 单测（`table.rs` 扩 `table_full_and_stale_eviction`）：淘汰时 `ops` **恰为**
     `[Remove(victim_pub), Add(new_pub)]`（现有断言用 `let (a, _) = ...` 丢弃 ops，正是漏网原因）。
  2. 单测：淘汰后 `t.len()` 与 ops 序；用**真实 `Device`**（评审 Q30：`Device` 无 trait 缝，假 device 会
     引入 Go 式接口仿写）断言 `peer_count()/has_peer()` 语义（**弱不变量**：同一 pubkey 单 peer）。
  3. 单测（A 版视图守卫，**拆两格**）：① 新设备派生 IP **只与 victim 撞车** → **应成功**（A 版视图的核心
     回归守卫）；② 与**在表另一设备**撞车 → 拒绝且**表内条目不变**（victim 仍在）。
  4. 端到端：`tools/local-rust-exit.sh` + 表满注入（`--max-peers 2` + 三设备）观察 E9 `reason=stale` 行 +
     出口 device peers 数回落。
- **判据行影响**：**E9 `reason=stale` 的触发集变化**（评审 Q3）——A 版把 Remove **推迟到冲突校验之后**：
  Go 在 `assignIPLocked` 失败时「已摘 peer + 已打 E9」，A 版「表/设备都不动 + **不打 E9**」⇒ 触发集变，
  **须登记「计数输入集 / 数值语义变化」表**（§3）。行文本身不变。

### F2（P1）采纳时序：**只在解出 Data 帧时采纳**（评审 Q5/Q7 订正，判据更严）

- **方案（订正：比原「解码成功」严）**：
  - `decode_frame` 只判 `len>=2 && buf[0]==0xBB`、**未知 kind 原样返回**（`frame.rs:70-75`）⇒
    「解码成功」几乎无门槛（`[0xBB, 任意 1 字节]` 即通过）。**评审 Q5 成立**。
  - **新判据**：`adopt` 仅当 **解码出 `kind == Data`**（`src == 当前 adopted` 视为空操作、不重打行）。
    `Control`(hint)/未知 kind/垃圾一律**不 adopt**。
  - **hint 仍被处理**：`Control` 帧照旧走 `on_hint`→`observe`→`set_candidates`（学习候选），
    **只是不立即接管路径**——真正的漫游/自愈靠「未知来源 + 合法 **Data** 帧」照样成立（Go 注记方向即
    「数据帧/候选来源」，本判据与之同向）。
  - **hint 来源过滤（评审 Q7 采纳）**：未知来源的 hint 至少过 `probe_addr_acceptable`，封掉
    「谁能发一个 Control 帧就让客户端向任意地址打洞」的注入面。
  - **审计后半句「伪造源为中继地址时误报 `via=relay`」处置（评审 Q6）**：去掉「已知来源」豁免后，
    **垃圾包路径闭合**；**合法帧伪造不可行**（需 WG 密钥）⇒ 登记为已知限制，**无需服务端改动**。
    **不采纳**评审 Q6 建议的「中继源只接受带 `0xAA` 路由头」——**该建议错误**：中继下行是把后端腿帧
    **原样转发**（`relay/mod.rs:955` `udp.send_to(&pkt, key.client)`，不带 `0xAA`），按此改会让**经中继
    的会话全部失效**。
  - **实现顺序（评审 Q8）**：文档原顺序**编译不过**（`payload` 借用 `self`，随后 `adopt(&mut self)`
    E0502）。写法：`let shape = frame::decode_frame(&self.recv_buf[..n]).map(|(k,_)| k);`（借用即刻结束）
    → 判定 → `adopt` → 再解码一次取 payload（或抽 `fn frame_kind(buf)->Option<u8>`）。
- **涉及文件**：`crates/homeway-core/src/wtransport/bind.rs`（`recv_from` `:475-534`）。
- **风险**：中。须保证 hint 学习路径、`relay_only` 测试缝、U1 升级条纹（`--no-hints` + 端口搬移）不回归。
- **测试计划**：① 未知来源 + 1 字节垃圾 → 不 adopt、`race_seen` 空、无 C5/C6；② 未知来源 + `0xBB+1B`
  （未知 kind）→ 不 adopt；③ 未知来源 + 合法 **Data** 帧 → 仍采纳（漫游）；④ 未知来源 + hint → 不 adopt
  但 `on_hint` 被调用且地址过 `probe_addr_acceptable`；⑤ 回归赛跑/handover 单测 + U1。
- **判据行影响**：**C5/C6 输入集变化**（非 Data 帧不再触发「赛跑结算/路径确立」）——行文不变，
  登记「计数输入集」表（§3）。

### F3（P1）未采纳态 reg 补投（**定时 2s**，上界写成速率口径）

- **方案**：未采纳期在 `send_wg` 未采纳分支，**距上次搭 reg 超过 `REG_RESEND_INTERVAL`（2s）即重新搭**；
  采纳后停止。保留「真正写出才消费」（`sent>0` 才更新 `last_reg_sent`）。新增字段 `last_reg_sent: Option<Instant>`。
  - **上界口径（评审 Q9 订正）**：**不是「量级个位数」**——未采纳期 boringtun 在 `REKEY_ATTEMPT_TIME` 内
    每 ~5s 重传握手（每次都 `send_wg`），到点 `ConnectionExpired → rebuild_tunn_once`（`wgcore/mod.rs:381-397`）
    **rearm 再武装 + 主动 encap**，循环持续 ⇒ 真实上界 = **速率 ≤1/2s（实际 ≈1/5s）、无总量上界**。
  - **不选「每包都搭」（评审 Q10）**：上界依赖 boringtun 调度这个仓外不变量；且出口 `register()` 每次 reg
    打一行 E8（**同步 stdout 日志**），按数据包率搭车会把出口日志放大到数据包率量级。
  - **新增 N1 处置（评审新增，重要）**：F3 与 **F1 淘汰窗口交互**——每次 reg 到达出口都刷新 `last_reg`
    （`table.rs:285`），而 F1 淘汰前提是 `idle > grace(600s)`（`DEFAULT_GRACE=600s`）⇒ 隧道坏掉但出站可达
    的客户端持续补投会让条目**永不 stale**、永不淘汰，在 `max_devices=32` 表里长期占位、**反噬 F1**。
    **F3 必须加次数/时长上限**（如「未采纳期最多补投 N 次或 T 秒，之后交恢复阶梯」），并写明与 F1 的交互。
- **涉及文件**：`crates/homeway-core/src/wtransport/bind.rs`（`send_wg`/`peek_reg`/新增字段与常量）。
- **风险**：低—中。出口对重复 reg 幂等（`table.rs:282-295` 同 devTag 同公钥 → `Refreshed`）。
- **测试计划**：① 未采纳态连续多次 `send_wg` → 按间隔仍搭 reg（现有 `direct_first_unlock_resends_with_reg` 可扩）；
  ② 采纳后不再搭；③ 补投次数/时长上限生效；④ 出口侧重复 reg → `Refreshed`（已存在）。
- **判据行影响**：**E8 频次上升 + `idle=` 数值语义变化**（评审 Q10：`idle` 从「距上次注册」变成「距上次
  补投」，通常 0s/5s）——行文不变，**两条都登记**「计数输入集」表（§3）。

### F4（P1）端点缓存上界 + 降权 + 投喂配额

- **方案（评审 Q11–Q15/N3 订正）**：
  - **cap 落三处 + 出口截断（评审 Q12）**：`observe`/`mark_verified`（写入）+ `load`（`:136-148`）
    + `merge_disk`（`:199-216`，每次 `save()` 前合并也能绕过）+ `entries()` 出口截断（否则 C13 条数不确定）。
    收敛到一个 `fn admit(&mut self, e)`。
  - **淘汰键 = `entries()` 排序的尾部（评审 Q11 关键订正）**：`entries()` 排序 = `verified` 降序 →
    `verified_at` 降序 → `learned_at` 降序（`endpoint_cache.rs:249-263`）⇒ 从尾部淘汰等价于「**未验证优先、
    其中最旧 learned 先出；全 verified 时淘汰最久未验证的**」——**正好保护**「长连但仅靠 `verified_at` 保活」
    的端点。**禁止**按字面「最旧 `learned_at`」实现（会误删它们）。
  - **「新条目永不被拒」（评审 Q11）**：满则立刻淘汰尾部腾位——避免「全 verified 且表满时 `observe` 被丢 ⇒
    漫游/换网新 hint 存不进」的死结（比内存问题更严重）。**不采纳**评审 Q11 的「cache 拿 `Bind::adopted` 做
    pin」——会引入反向依赖/回调（AGENTS 原则 5）；adopted 地址必然是 `verified_at` 最新档，天然被保护。
  - **`mark_failed`（评审 Q13）**：**无调用点即死代码**。处置：**有真实调用点（拨号/探测失败路径）才做**，
    否则**从本批剔除**；若做，**只内存降权、不进落盘格式**（评审 N3：`CacheEntry` 以「键序 = Go 结构体声明序、
    同数据与 Go 字节相同」为契约且有落盘格式单测；新增 `fail_count` 须 `#[serde(skip)]`，否则破坏逐字节同形）。
  - **投喂配额（评审 Q15）**：**cap 是唯一必须的硬界**；投喂速率界 = **每 peer 缓存每窗口（如 60s）新增
    **未验证**地址 ≤ N**，**hint/probe 共用同一计数器**、只计**新增地址**、rearm 归零；`probe_addr_acceptable`
    接到 hint 路径（与 F2 协同）。**放弃**「每轮 ≤24」这类与线程数/会话数耦合的口径。
  - **`send_err_log_at`/`race_seen`（评审 Q14）**：cap + F2 落地后**基本冗余**——文档写明「由候选 cap + F2
    传递钳制」+ 规模上界单测即可，不单列独立上限。
- **涉及文件**：`crates/homeway-core/src/wtransport/endpoint_cache.rs`、`bind.rs`、`crates/homeway-core/src/probe.rs`、
  调用方（`session/mod.rs`/`facade/tun_exec.rs` 的 `on_hint`/探测接线）。
- **风险**：中（淘汰策略误删已讨论；N3 落盘契约）。
- **测试计划**：① 超 cap 插入 → 条数受限、未验证/最旧先出、已验证保留；② 全 verified + 表满 → 新条目仍
  **被接纳**（淘汰尾部）；③ `load`/`merge_disk` 超 cap 也受限；④ 投喂配额（hint/probe 共享）生效；⑤ 回归
  `ttl_freshness_and_ordering`；⑥ 落盘逐字节同形回归（若做 `mark_failed`）。
- **判据行影响**：C13 条数受上限影响——登记「计数输入集」表（§3）。

### F5（P1）中继腿表：**无状态 cookie 挑战**（评审 Q16 改方案，首选）

- **方案（评审 Q16：无状态 cookie 更简、更能抗洪水、**不改 wire**）**：
  - 依据：token 模式（生产形态）下 PROOF 校验**只用 `secret`**——`relay/mod.rs:643-654` 的
    `(Some(sec), Some(_eph))` 分支里 `eph_priv` **完全没参与**（DH 只用于开放模式）⇒ 挑战态真正需要的
    只有 `(nonce, 时间戳)`。
  - **HELLO 完全无状态**：`nonce = HMAC(secret, src_ip ‖ pubkey ‖ time_bucket)`（开放模式用启动期随机
    relay key 派生 `eph_priv = HMAC(key, nonce)` 保持 DH 可验）；回 `eph_pub ‖ nonce`（**wire 字节不变**）。
    不写任何表。
  - **PROOF**：由 `src`+`pubkey`+当前/前一 15s 桶**重算 nonce**（`CHALLENGE_TTL=15s` ⇒ 接受两桶等价），
    比对 MAC；通过后才建/更新 `legs`（受 `max_legs` 闸）。
  - **收益**：无新表、无新 cap、无淘汰策略、**没有「pending 被灌满」的次级 DoS**、wire 不变、§3 那条 F5
    登记可撤。
  - **备选（若设计门/用户否决无状态版）**：`pending_legs` 小表（≤64）+ PROOF 通过才进 `legs`；此时三条
    语义**必须写死（评审 Q17）**：① **已 verified 的 label 再 HELLO 不得迁移出 `legs`**（否则在用数据腿在
    挑战窗口 `admitted()` 变 false、数据面闸关）；② `pending_legs` 满**按最旧淘汰**而非拒绝（否则 200pps×1 源
    在 **0.32s** 内让合法后端也进不来）；③ PROOF 提升时 `legs` 满应拒绝 + 让后端重试。
  - **订正（评审 Q18）**：「HELLO 加每源限速（复用 `rates`）」**已存在**（`:521`+`:1314-1327`，200pps/源、
    主监听口全量）⇒ 删掉该子项；「8.5pps 打满」改为「**64 槽只需 0.32s@200pps**」——限速挡不住单源洪水，
    真正起作用的是**无状态化/淘汰语义**。
- **涉及文件**：`crates/homeway-core/src/relay/mod.rs`（`Leg`/`handle_control` HELLO/PROOF/KEEPALIVE、
  `reap`、状态行 `:490`）。
- **风险**：中。须保证合法后端 HELLO→PROOF 两跳内成功、控制面建腿（`handshake_done` `:999`）不受影响、
  地址变更重注册（`moved` `:665-701`）正确。
- **测试计划**：① 伪造 label 的 256 次 HELLO **不占 `legs`**（合法后端仍可注册）；② HELLO→PROOF 正常提升；
  ③ 时间桶边界（前一桶仍可验、更早桶拒绝）；④ 开放模式 DH 可验；⑤ 回归 R3/R4/R5/R6/R11/R12。
- **判据行影响**：**R1 就绪行「腿总数上限 256」的数值语义变化**（评审 Q19：分表/无状态后 256 只数**已接纳**
  腿、pending/未验证不计 ⇒ 行文不变但含义变）——**须登记**「计数输入集」表（§3）。若采纳无状态版，
  原「拒绝行触发语义」差异登记可撤。
  - **评审 Q20 提示**：`stats.denied`/`leg_rejected` **完全不可观测**（`:104-114` 有字段，`:489-493` 分钟行
    只打 registered/forged/assigned/reclaimed/forwarded/dropped，`:217-223` `stats` 私有无访问器）⇒
    「合法后端注册不上（`stats.denied` 抬高）」后果论证落空，F5 测试①只能靠单测。**处置**：本批**只做单测观测**
    （接进 60s 行 = R10 行文变更，见 F7 的通道选择，不在本批）。

### F6（P1）中继拒绝日志限流（**清单补全**）

- **方案**：给拒绝类日志加**每原因节流**（复用 `leg_reject` 的「首 3 条 + 每 100 条一条」或时间窗「每 N 秒
  至多一行」）。**点位（评审 Q24 补全）**：`:576-583`（max-legs）、`:766-772`（per-peer）、**`:621-628`
  （PROOF 版本不符，未认证可达、≤200pps 可刷）**、**`:655-661`（token 校验不过，未认证可达）**、`:985-988`
  （并发握手上限）。**本批只做节流**（`RelayLog::logf` 走缓冲写属 Q-I 性能面，且改落盘时序有风险）。
- **涉及文件**：`crates/homeway-core/src/relay/mod.rs`。
- **风险**：低（只改日志频次）。
- **测试计划**：单测：连续超限/伪造注入 → 各拒绝行数受限（≤ 阈值）。
- **判据行影响**：**无**（拒绝行非 R1–R13 判据行）；日志频次下降属行为差异（登记 §3）。

### F7（P1）v1 fallback 源绑定 + assoc 预算 + 未认证不 bump + 限流入观测面

- **方案（评审 Q21/Q22/Q23/N5 订正）**：
  1. **v1 fallback 源绑定**：`sid==0` 分支（`:921-925`）只接受 `from == a.backend` 的包（**取 `a.backend`
     而非 `lg.addr`**——评审 Q21：`replay_sessions` 回滚会把 `a.sid=0/dial_up=false/dialed=false`
     （`:1150-1160`）而**保留 `a.backend`**，此时若腿是纯控制面腿（`lg.addr=None`），用 `lg.addr` 会**当场
     丢光下行**）；否则**显式丢弃 + `leg_rejected` 计数、不续命**（现在是排他 if/else，无第三出口，会穿透到
     `:955` 原样回投）。**兼容性结论（评审 Q21）：不误杀**——上行方向本就钉在 `a.backend`（`forward_up:866`，
    且 `:748-753` 腿地址变化时直接拆会话重建）⇒ 重映射时上行早已黑洞，绑定只让下行与上行一致；地址真变有
     自愈链（KEEPALIVE `lg.addr != src` → AGAIN `:720-724` → 重注册 → 拆会话重建 `:750-753`）。
  2. **assoc 预算（评审 N5 订正）**：**禁止直接复用 `leg_rate_ok`**——它是 `rate_limit*10 = 2000/s`
     （`:1330-1337`）且只用于拒绝日志节流，照抄 ⇒ 下行 ≈2000pps×~1400B ≈ **22Mbit/s 硬顶**，而当前 assoc 读
     路径**完全无准入限流**（今天下行不设速率上限，R2 实测只提上行 2.7Mbps 被 200pps 限住）。**先写清目标吞吐**
     （如 ≥100Mbit/s ⇒ ≥9kpps 或按字节桶），再设桶；**登记为行为差异**（否则是静默下载性能回归）。
  3. **未认证不 bump（评审 Q31 升为必做）**：`:782` 的 `bump_sock_bufs(&sock)` 延迟到首次成功下行转发后
     （或仅 `has_ctl`/已认证路径）；**另加全局 assoc 数上限**——`max_per_peer=32` 是**每腿**上限，全局最坏
     `32 × 256 = 8192` socket（每个 4MB+4MB 内核缓冲）⇒ fd/内存双耗尽面。建议把 `max_per_peer` 判定改成
     全局闸（或 `max_legs × max_per_peer` 乘积上限），`max_ctl_conns` 已有全局闸先例。
  4. **限流入观测面（评审 Q22 改口径）**：**中继侧无 JSON 遥测通道**——`relay status` 载荷只有
     `{enabled,state,listen,tokenMask}`（`daemon_cli.rs:916-925`），`Relay::Stats` 私有无访问器；§3 类比 Q-B 的
     `udpDrop`（那条通道只在 serve 侧存在）**不成立**。**三选一**：① 本批新建中继遥测通道；② 并进 60s 行 =
     **R10 行文变更 → 登记表**；③ **本批只做日志 + 单测观测并明说**。**本批选 ③**（新建通道远超范围；改 R10
     会连带 Q-B 的 R10 验收口径、破坏本批「行文不变」承诺）。
- **涉及文件**：`crates/homeway-core/src/relay/mod.rs`（`assoc_read`/`forward_up`/`reap`/`Stats`）。
- **风险**：中—高。v1 源绑定是本批最需确认的兼容性点（结论：安全，见上）。
- **测试计划**：① v1 会话下「非 `a.backend` 来源」包被拒且不续命；② assoc 读超目标速率丢弃；③ 未认证
  assoc 不调 `bump_sock_bufs` + 全局 assoc 数上限生效；④ 限流丢弃计数在日志/单测可见。
- **判据行影响**：**R10「丢弃」数值语义**（评审 D3：限流丢弃**今天已计入** `dropped`）——F7.4 若新增独立
  计数属 additive；登记「计数输入集」表（§3）。

### F8（P2）转发失败不再计成功 + assoc socket **双栈**

- **方案（评审 Q23 订正）**：
  - `forward_up`/`forward_down`：`let _ = send_to` 改 `match`——`Ok` 才 `forwarded_* += 1`，`Err` 进**新计数**
    `send_fail_up`/`send_fail_down`（additive；但见 F7.4 通道限制）。`:944`（pend 回放）同口径。
    **注意（评审 Q23）**：pend 回放的包入队时已 `forwarded_up += 1`（`:860`），回放失败**不要再动
    `forwarded_down`**（否则出现「同包 up 成功/down 失败」的混面）。
  - **assoc socket 家族（评审 Q23 订正）**：**不用**「按 `a.backend` 家族 bind」——拨腿会话建 socket 时
    `a.backend` 还是 `leg_addr`（纯控制腿为 `None`），后端一族要等 LEGUP 认证后才知道。**最简且与 Go 逐字
    对齐**：用**双栈 socket**（`udpbatch::open_client_socket()` + `xmit_addr()`，同 crate `pub(crate)` 可用，
    客户端 `Bind` 已是这套）。**客户端方向 v6 仍缺**（`relay_cli.rs:99-103` 主监听口 v4-only）——写明挂账。
- **涉及文件**：`crates/homeway-core/src/relay/mod.rs`（`forward_up`/`assoc_read`/建 assoc）。
- **风险**：低—中。
- **测试计划**：① `send_to` 失败注入 → `forwarded_*` 不增、`send_fail_*` 增；② v6 后端 → 双栈 assoc socket
  能发；③ pend 回放失败不动 `forwarded_down`。
- **判据行影响**：**R10「转发 上/下」数值语义变化**（只计成功）——登记「计数输入集」表（§3）。

### F9（P2）`probe` build **字节截断**（对齐 Go）+ relay build 注入

- **方案（评审 N2 订正）**：
  - `probe.rs:95` 改**字节截断**（与 Go `probe.go:173-174` 逐字同形、且不 panic）：
    `let b = build.as_bytes(); let n = b.len().min(32); base.push(n as u8); base.extend_from_slice(&b[..n]);`
    ——**不用** `floor_char_boundary(32)`（多字节跨界时会**少发 1–3 字节**、长度字节也跟着变 ⇒ 引入未登记的
    wire 差异）。
  - **relay build 注入**：新增共享常量（如 `crate::BUILD_STR`）在 `relay_cli.rs:186` 赋 `cfg.build`。
    **评审 Q25**：这会**改 C14 的「构建」字段**（Go C14 由 `tr.Probe(cands[0].Addr)` 的 `build` 渲染，
    `cands[0]` 可以是 relay 端点）⇒ §3 增登记。**serve 侧 `build` 不动**（避免牵动 C14）。
- **涉及文件**：`crates/homeway-core/src/probe.rs`、`crates/homeway-cli/src/relay_cli.rs`（+ 常量）。
- **风险**：低。
- **测试计划**：① `respond_ex` 传 >32B 且含多字节字符的 build → **不 panic**、输出与 Go 字节同形；
  ② relay 装配后 `build_str()` ≠ `"relay-dev"`。
- **判据行影响**：**C14「构建」字段**（中继为首个候选场景）——登记「计数输入集」表（§3）。

### F10（P2）控制面降级可见（**中继侧口径**）

- **方案（评审 D4 订正）**：relay 记录控制面状态（`ctl_ok: bool`，TCP 同号口 bind 成功与否）；暴露到
  **中继探针 flags**（新位，如 `FLAG_CTL_DEGRADED`）——中继 flags 当前字面量 0（`relay/mod.rs:526`），
  **serve 侧 flags = caps 不动**。**评审 Q33**：bit0–4 已被 serve 的 udpcap 占用 ⇒ 新位落**中继命名空间**并
  登记；追加位对按位掩码消费方安全（`probe.rs:132-160` 客户端解析只按位取用）。**若不做新位**，则本批降级为
  「status 行外日志 + 单测」。
- **涉及文件**：`crates/homeway-core/src/relay/mod.rs`（`:293-310`、`:490`、`:526`）、`crates/homeway-core/src/probe.rs`。
- **风险**：低—中（wire 增位）。
- **测试计划**：控制面 bind 失败注入 → 探针 flags 降级位置位 + 单测可见。
- **判据行影响**：**无行文变化**（serve 侧 flags/C14 不受影响）；中继 flags 语义扩展登记 §3。

### F11（P2 🔎）小边界族

- **方案（评审 Q26–Q29/N2 订正）**：
  - `same_candidates`（`domain_eps.rs:361-374`）：**抽共享实现**（仓库已有正确版 `bind.rs:827-836` sort+比较），
    两处共用，消除第二份拷贝；**测试**覆盖多重集反例 + `refresh_async` 的 `apply_static`/软赛跑补投路径。
  - `tunnel_addr` 撞车：**两函数共用守卫**（`derive_tunnel_ip` + `derive_tun_ip` 都落 100.64/16，服务端
    `ip_taken` 是双地址并集）；结果 == `SERVER_TUNNEL_IP` → `hw-tun.N`/`hw-app.N` 再散列；**考虑把
    `SERVER_TUNNEL_IP` 挪到 `tunnel_addr`**（地址空间单一真源）。**写明**：碰撞**可故意研磨**（非「自然稀有」）、
    且**旧对端/fixtures 向量会对该设备地址不一致 ⇒ 直接连不上**。**B 类分歧**。
  - `direct_first`：`bind.rs:183-189` 改 `direct_first.map(|d| if d.is_zero() { DEFAULT } else { d })`
    （`None` 保持 `None` = 显式关）。**评审 Q28**：`rearm()` 日志 `:776` `unwrap_or(DIRECT_FIRST_DEFAULT)` 在
    `None` 时会打「中继在 2s 后才解锁」（与语义相反，仅日志）⇒ 一并改文案。
  - `frame` 长度域：`encode_batch`/`hint_bytes` 对 `payload.len() > u16::MAX` 加 `debug_assert!`（现状不可达，
    热路径不宜返 `Result`）；**评审 Q32** 可在 `tun_exec.rs:525` 顺手补上界收口。
- **涉及文件**：`crates/homeway-core/src/wtransport/domain_eps.rs`、`bind.rs`、`tunnel_addr.rs`、
  `crates/homeway-core/src/wgcore/mod.rs`、`wtransport/frame.rs`。
- **风险**：低（`tunnel_addr` 中，牵动 wire 派生地址，须双侧同步）。
- **测试计划**：① 多重集反例单测；② 撞车注入（构造 v=65281）→ `hw-tun.2`/`hw-app.2` 且双侧一致；
  ③ `direct_first(None)` → 立即解锁中继 + 日志文案正确；④ `frame` 长度域断言。
- **判据行影响**：撞车分支改变极稀有设备的隧道地址（wire 差异）——登记 §3。

### F12（P2）`egress.rs` `if_nametoindex` 失败静默 index=0

- **方案（评审 Q29/N6 订正）**：本批只做**最小守卫**——返 0 时**不静默**：记告警（可带 errno ENXIO）
  并把该网卡标为「无 index / 不可钉」，**不进入「已钉卡」成功路径**；完整语义（钉卡失败降级策略、
  `AddAnyMapping`、平台假设盘点）**挂 Q-J**。
  - **评审 N6 提示**：`index` 还被 `engine.rs:917-923` 的 `stateOf` 指纹渲染成 **E21 判据行**的
    `index=%d up addrs=[…]`（`bindwatch.rs:54/390/409/472` 逐字钉死）⇒ 改 `Option` 后失败路径该字段渲染必然变。
    处置：**让 `Option::None` 在该行保持既有形态**（如沿用 0 但**另起一条告警行**），否则补登记。
- **涉及文件**：`crates/homeway-core/src/server/egress.rs:165-168`（+ 消费方 `upnp.rs:192`、
  `bind.rs:227-236`、`engine.rs:387/1475-1482` 的 `pinned` 链路）。
- **风险**：低—中。
- **测试计划**：单测：`if_nametoindex` 返 0（注入不存在网卡名）→ 不标「已钉」、记告警、E21 行形态不变。
- **判据行影响**：E21 `index=` **取值路径**须保持既有形态（见上）——否则登记（§3）。

### P2 性能面（🔎）——**移交 Q-I**

- `relay/mod.rs:855`（`frame_bytes` 堆分配/包）、`:437-439`（assoc 读每事件 64KB + 单包）、
  `:765`/`:1093`（全表扫描）——性能面，按 Q-B 先例**移交 Q-I**。

---

## 3. 判据行影响清单（v2，评审补 6 条登记）

> 政策 = `docs/INTEROP-CRITERIA.md`「判据变更记录」节。**行文变更**走「登记表」；**行文不变但输入集/
> 数值语义变**走「计数输入集 / 数值语义变化」表。**全部须与代码同批 commit**。
> **评审提示**：criteria 现只有两张表（登记表 + 计数输入集表），本批**不新增判据行** ⇒ 无需第三张表。

| 类别 | 条目 | 从 → 到 | 触发自 | 处置 |
|---|---|---|---|---|
| **登记表** | 中继「注册腿总数已达上限…拒绝新的」拒绝行 | 现状「HELLO 即占 `legs` 槽」→ 无状态化/分表后「HELLO 不占 `legs`，PROOF 通过才占」——**非 R1–R13 判据行** | F5 | 登记（行为差异） |
| **登记表** | `tunnel_addr` 撞车分支 | 极稀有/可研磨设备隧道地址 `100.64.255.1` → 再散列值 | F11 | 登记（wire 差异，双侧对称） |
| **计数输入集表** | **E9** `reason=stale`（评审 Q3） | 触发集：Go「先摘后查、撞车也摘」→ A 版「校验通过才摘」（撞车拒绝时**不打** stale 行） | F1 | 登记（行文不变） |
| **计数输入集表** | **C5/C6**（赛跑结算 / 路径确立·切换） | 输入集：任意来源包 → **仅 Data 帧** | F2 | 登记（行文不变） |
| **计数输入集表** | **E8**（`peer: ~ … refresh`） | ① 频次：未采纳期 2s 补投后可能多打；② **`idle=` 数值语义**（评审 Q10）：从「距上次注册」→「距上次补投」（常 0s/5s） | F3 | 登记（行文不变，**两条**） |
| **计数输入集表** | **C13**（候选端点条数） | 学习候选无上界 → **上限 + 尾部淘汰** | F4 | 登记（行文不变） |
| **计数输入集表** | **R1**（就绪行「腿总数上限 256」）（评审 Q19） | 数值语义：过去含未验证腿 → 无状态/分表后只数**已接纳**腿 | F5 | 登记（行文不变） |
| **计数输入集表** | **C14**（「出口能力：构建 %s」）（评审 Q25） | 中继 build：`relay-dev` → 注入值（中继为首个候选时） | F9 | 登记（行文不变） |
| **计数输入集表** | **R10**（`中继统计`） | ①「转发 上/下」只计**成功**；②「丢弃」的细分（限流丢弃**今天已计入** `dropped`，F7.4 若新增独立计数属 additive） | F7/F8 | 登记（行文不变） |
| **计数输入集表** | 中继 assoc 下行速率（评审 N5） | 无上限 → 目标吞吐桶（≥100Mbit/s 口径） | F7.2 | 登记（**性能行为差异**） |
| **观测面（additive，不新增判据行）** | 探针 flags 中继降级位 / `send_fail_*` / 限流细分计数 | 无 → 有 | F7/F8/F10 | 登记（**中继无 JSON 通道，本批多为日志/单测可见**，见 F7.4） |

**明确无判据行影响者**：F6（拒绝日志限流）、F12（`if_nametoindex` 守卫，**前提**是 E21 取值路径形态不变）、
`direct_first` 语义（F11，生产无调用者，评审 Q28 确认不登记）。

---

## 4. 设计门记录（dsh 外部评审）

### 4.1 轮次信息

- **dsh 轮次目录**：`/tmp/dsh-review/r3.e5GAso/`（`prompt.txt` / `output.md` / `stderr.log`）
- **exit code**：`0`（成功；`output.md` 316 行已完整读毕）
- **评审者自述**：逐条回源码复验 + Go 基线对照 + 内部子代理交叉核验（其内部另跑一轮 `r1.JSQRDb`）。
- **意见条数**：**约 47 条**——D1–D5（事实性冲突 5）/ Q1–Q29（设计取舍 29）/ Q30–Q36（边界 case 7）/
  N1–N6（评审新增发现 6）。总体判断：§1 表行号抽查 30 余处除 2 处归属错误外全命中，Go 引用逐条成立，
  A/B 两分方法论方向正确；但有 **4 处结论与源码冲突、6 处判据行影响漏登记、3 处方案本身选错**。

### 4.2 评审原文摘要（逐条）

**D. 事实性冲突**
- **D1【中】** §5「Go `assignIPLocked` 退到池分配」**不存在**：Go `peers.go:529` 是
  `return netip.Addr{}, ErrTunnelIPConflict`（硬拒），`peers_test.go:375-388` 明确断言「不再退池」。
  → 删该挂账 / 改写为「Go 注释腐化，两端行为一致（都拒）」。
- **D2【中】** `probe` build 截断**非** A 类直译偏差：Go `probe.go:173-174` 同样**字节截断且不 panic**；
  Rust panic 是 **Rust 特有缺陷**，且**生产不可达**。→ 改分类；F9 测试改为直接调 API 注入 >32B 多字节串。
- **D3【中】** `rate_limit=200` **不是**「不入任何状态/日志面」：`relay/mod.rs:521` 的 `rate_ok` 是
  **所有入站 UDP 第一步**，超限进 `stats.dropped`（`:522`），而 `dropped` **就打在 R10 判据行里**（`:489-493`）。
  真实缺口 = 无独立细分计数。
- **D4【中】** F10「探针 flags 当前恒 0 / flags 未被任何判据行消费」**不成立**：serve 侧 flags = `caps`
  （`server/bind.rs:370`，位定义 `engine.rs:150-154`），且**已被 C14 消费**（`INTEROP-CRITERIA.md:60/119`）；
  「恒 0」**仅对中继**成立（`relay/mod.rs:526`）；bit0–4 已全占。→ F10 改口径。
- **D5【低】** 行号归属：`relay/logfile.rs:65-86` 非 `stamp()`（`stamp()` 在 `:22-56`）；`frame.rs` 回绕点
  在 `:110`。

**Q. 设计取舍裁决**
- **Q1【高】** F1 的 B 版**没消除根因，必须删掉只留 A 版**（`device.remove_peer` 唯一生产调用点 =
  `engine.rs:1296`；`gc()` 只遍历 `entries` ⇒ B 版错误路径孤儿 peer **永久驻留**，与 P0-2 同一 bug）。
- **Q2【中】** A 版应拆「纯选择 + 排除视图」；**不要回插补偿**（会让已打的 E9 行说谎）。
- **Q3【中】** A 版把 Remove 推迟到校验后 ⇒ **E9 触发集变**，必须进登记表；文档「无判据行影响」自相矛盾。
- **Q4【低】** 测试缺「新设备派生 IP **只与 victim 撞车**」一格（A 版视图的回归守卫）。
- **Q5【高】** 「解码成功」判据**过弱**（`decode_frame` 只判 `len>=2 && buf[0]==0xBB`、未知 kind 原样返回）。
- **Q6【高】** 审计后半句「伪造源为中继地址时误报 `via=relay`」**完全没被设计覆盖**；须修或登记「不修+理由」。
- **Q7【中】** 未知来源的 Control(hint) 帧**不该构成路径证据**；判据应为「`src ∈ 已知` **或**（解码成功
  **且 kind==Data**）」。
- **Q8【低】** 文档给的实现顺序**编译不过**（借用 E0502）。
- **Q9【中】** F3 上界论证错：**无总量上界**（`rebuild_tunn_once` rearm 循环）。
- **Q10【低】** 支持 2s 版；补：**E8 `idle=` 语义会变**（文档漏登记）。
- **Q11【中】** F4「只在未验证里淘汰」有死结（全 verified + 表满 ⇒ 新 hint 存不进）；建议「新条目永不被拒」
  + 绝不淘汰当前 adopted。
- **Q12【中】** cap 须同时落 `load()`/`merge_disk()`/`entries()` 出口。
- **Q13【中】** `mark_failed` **无调用点** ⇒ 死代码/投机接口。
- **Q14【低】** `send_err_log_at`/`race_seen` 独立上限基本冗余。
- **Q15【低—中】** 配额口径：cap 是唯一必须硬界；hint 入口同样无配额且无地址卫兵。
- **Q16【高—中】** **无状态 cookie 更简也更能抗洪水、不改 wire，建议改方案**（token 模式 PROOF 不用 `eph_priv`）。
- **Q17【中】** 若坚持分表，三条语义必须写死（verified label 不迁移 / pending 满按最旧淘汰 / 提升时满则拒）。
- **Q18【中】** 「HELLO 加每源限速」**已存在**；「8.5pps 打满」应改「64 槽 0.32s@200pps」。
- **Q19【中】** §3 漏登记 **R1「腿总数上限 256」数值语义**。
- **Q20【中】** `stats.denied`/`leg_rejected` **完全不可观测** ⇒ 后果论证落空。
- **Q21【中】** F7 v1 源绑定**不误杀**；**绑定键应取 `a.backend` 而非 `lg.addr`**（replay 回滚会造
  `sid==0` + `backend` 保留 + `lg.addr=None`）；「无 `lg.addr` 不进 v1」须写成**显式丢弃分支**。
- **Q22【中—高】** F7.4/F8/F10 共同前提「走 status 载荷」**在中继角色不存在**（无 JSON 遥测通道）；
  三选一并写明。
- **Q23【低】** `leg_rate_ok` 是 `rate_limit*10=2000/s`；F8 assoc 家族应取 `a.backend` 的族；pend 回放失败
  不要再动 `forwarded_down`。
- **Q24【中】** F6 日志点清单**不全**（`:621-628`/`:655-661` 未认证可达、每包一行）。
- **Q25【中】** F9 relay build 注入会**改 C14 的「构建」字段** ⇒ §3 增登记。
- **Q26【中】** F11 `tunnel_addr` 守卫**只守了一半**（`derive_tun_ip` 同域、双地址并集、**可故意研磨**、
  旧对端不一致）。
- **Q27【低】** `same_candidates` 仓库**已有正确实现**（`bind.rs:827-836`）；误判后果比原判重（跳过软赛跑补投）。
- **Q28【低】** F11 `direct_first`：修后 `rearm()` 日志文案会与语义相反。
- **Q29【低】** F12 建议正确；补：`if_nametoindex` 返 0 会置 errno（ENXIO）；改 `Option` 时同步 `pinned` 链路。

**Q30–Q36（边界）**：Q30 `Device` 无 trait 缝（假 device 会引入接口仿写）；Q31 assoc 全局最坏 8192 socket
（「全局上限」应**升为必做**）；Q32 `frame` 用 `debug_assert!` 合适；Q33 探针 flags bit0–4 已占；Q34 F5 放大面
（请求 33B/响应 ~60B ≈1.8×）；Q35 配额口径与线程数解耦；Q36 §1 F3 行「Rust 消费点更严」已核实。

**N1–N6（评审新增）**：**N1【中】** F3×F1 交互——2s 补投刷新 `last_reg` ⇒ 条目永不 stale、反噬 F1 淘汰；
**N2【中】** F9 修法会与 Go 的 wire 字节分叉（应**字节截断**而非 `floor_char_boundary`）；
**N3【中】** `mark_failed` 若持久化会破坏与 Go 逐字节同形的端点缓存文件；
**N4【中】** F1「表-设备一致」不变量在克隆场景不成立（同公钥两 devTag ⇒ Remove 摘两条），测试须写**弱不变量**；
**N5【中】** F7.2「复用 `leg_rate_ok`」会变成下行 22Mbit/s 硬顶（静默性能回归）；
**N6【低】** F12 守卫会牵动 **E21 判据行** `index=` 取值路径。

### 4.3 逐条处置表

| 意见 | 处置 | 依据/落地 |
|---|---|---|
| D1 | **认同** | 已独立验证 `peers.go:529` 硬拒 + `peers_test.go` 断言；§5 挂账**已删**，改写为「Go 注释腐化、两端一致（都拒）」。 |
| D2 | **认同** | 已验证 `probe.go:173-174` 字节截断；§1 已改分类为「Rust 特有缺陷（生产不可达）」。 |
| D3 | **认同** | 已验证 `:521-522` + `:489-493`；§1 v1 条目描述**已订正**。 |
| D4 | **认同** | 已验证 serve flags = caps（`server/bind.rs:370`/`engine.rs:150-154`）+ C14 消费；F10 **已改口径**为「仅中继 flags 报 0」。 |
| D5 | **认同** | 行号**已订正**（`stamp()` `:22-56`；`frame.rs:110`）。 |
| Q1 | **认同** | B 版**已删**，F1 只留 A 版；补「B 版孤儿 peer 永久驻留」论证。 |
| Q2 | **认同** | F1 改「纯 `select_stale_victim` + `exclude` 视图」；**禁用回插补偿**已写入。 |
| Q3 | **认同** | §3 **已增 E9 触发集登记**；§1 结论订正。 |
| Q4 | **认同** | F1 测试**已拆两格**（只与 victim 撞 / 与在表另一设备撞）。 |
| Q5 | **认同** | F2 判据**已改「kind==Data」**（非「解码成功」）。 |
| Q6 | **部分认同** | **认同**「审计后半句须处置」——处置 = 去掉 known 豁免使垃圾包路径闭合、合法帧伪造需密钥 ⇒ 登记已知限制。**不认同**其建议方向「中继源只接受 `0xAA` 路由头」：中继下行**原样转发**后端腿帧（`relay/mod.rs:955` 不带 `0xAA`），按此改会让经中继会话全部失效（**评审自身第二部分亦已推翻该建议**）。 |
| Q7 | **认同** | F2 **已采纳**：hint 不构成路径证据 + 未知来源 hint 过 `probe_addr_acceptable`。 |
| Q8 | **认同** | F2 **已写明**借用安全的实现顺序（先 `map(|(k,_)|k)` 结束借用）。 |
| Q9 | **认同** | F3 上界**已改速率口径**（≤1/2s、无总量上界）+ `rebuild_tunn_once` 循环依据。 |
| Q10 | **认同** | 采 2s 版；§3 **已增 E8 `idle=` 数值语义登记**。 |
| Q11 | **部分认同** | **认同**「新条目永不被拒」+ 淘汰键 = `entries()` 排序尾部（保护 verified）。**不认同**「cache 拿 `Bind::adopted` 做 pin」——会引入反向依赖/回调（AGENTS 原则 5）；改用「rank 淘汰天然保护 adopted」+ 说明。 |
| Q12 | **认同** | F4 **已写** cap 落 `observe`/`mark_verified`/`load`/`merge_disk`/`entries()`。 |
| Q13 | **认同** | F4 **已改**：`mark_failed` 有真实调用点才做，否则**剔除**；做则 `#[serde(skip)]`。 |
| Q14 | **认同** | F4 **已改**为「由候选 cap + F2 传递钳制 + 规模上界单测」。 |
| Q15 | **认同** | F4 **已改**配额口径（cap 为唯一硬界；hint/probe 共享每窗新增未验证计数；`probe_addr_acceptable` 接 hint）。 |
| Q16 | **认同** | F5 **已改首选方案为无状态 cookie**；`pending_legs` 降为备选（附 Q17 三条语义）。 |
| Q17 | **认同（作为备选约束）** | 已写入 F5 备选分支三条语义。 |
| Q18 | **认同** | F5 **已删**「加每源限速」子项、改「64 槽 0.32s@200pps」。 |
| Q19 | **认同** | §3 **已增 R1 数值语义登记**。 |
| Q20 | **认同** | F5 **已写**「本批只做单测观测」；`denied`/`leg_rejected` 接 60s 行（=R10 行文变更）不在本批。 |
| Q21 | **认同** | F7.1 **已改绑定键为 `a.backend`** + 显式丢弃分支 + 兼容性论证。 |
| Q22 | **认同** | F7.4 **已改**：中继无 JSON 通道 ⇒ **本批选 ③ 只做日志/单测观测**（明说）；①/② 列为备选。 |
| Q23 | **认同** | F8 **已改**为双栈 socket（非 `a.backend` 家族）；pend 回放不动 `forwarded_down`；F7.2 不再复用 `leg_rate_ok`。 |
| Q24 | **认同** | F6 点位**已补全** `:621-628`/`:655-661`。 |
| Q25 | **认同** | §3 **已增 C14 构建字段登记**。 |
| Q26 | **认同** | F11 **已改**：两函数共用守卫 + `SERVER_TUNNEL_IP` 迁址 + 可研磨性 + 旧对端不一致。 |
| Q27 | **认同** | F11 **已改**为抽共享实现（`bind.rs:827-836`）+ 误判后果（软赛跑补投）写入。 |
| Q28 | **认同** | F11 **已补** `rearm()` 日志文案修正。 |
| Q29 | **认同** | F12 **已补** errno + `pinned` 链路同步。 |
| Q30 | **认同** | F1 测试**已改**用真实 `Device` + 弱不变量。 |
| Q31 | **认同** | F7.3 **已升为必做**（全局 assoc 上限）。 |
| Q32 | **认同** | F11 **已确认** `debug_assert!`（+ 可选 `tun_exec.rs:525` 收口）。 |
| Q33 | **认同** | F10 **已改**新位落中继命名空间并登记。 |
| Q34 | **认同** | F5 **已写**放大倍数（~1.8×）与「不新增反射面」（无状态版）。 |
| Q35 | **认同** | F4 **已改**配额口径与线程数解耦。 |
| Q36 | **认同** | §1 已确认。 |
| N1 | **认同（新增设计）** | F3 **已加**补投次数/时长上限 + 与 F1 淘汰窗口交互说明。 |
| N2 | **认同** | F9 **已改**为**字节截断**（与 Go 同形），弃 `floor_char_boundary`。 |
| N3 | **认同** | F4 **已写** `mark_failed` 落盘须 `#[serde(skip)]`（保逐字节同形）。 |
| N4 | **认同** | F1 **已写**克隆场景弱不变量 + 已知差异。 |
| N5 | **认同** | F7.2 **已改**：先定目标吞吐（≥100Mbit/s）再设桶 + 登记行为差异。 |
| N6 | **认同** | F12 **已写** E21 `index=` 取值路径保持既有形态。 |

**处置汇总**：**认同 44 条 / 部分认同 3 条（Q6/Q11/Q23 的一半）/ 不认同 0 条**（Q6 的「0xAA 建议」与 Q11 的
「pin 接口」两项**具体建议**被否，但其**问题识别**均被采纳并以替代方案落地——故不列为「不认同」）。
**无「不认同整条」项。**

### 4.4 过门结论

**通过（exit=0）**。评审 47 条意见**全部处置**：44 条认同并已并入本文档 v2；3 条部分认同——Q6 的
「中继源要求 `0xAA` 路由头」建议**经独立验证为错误**（中继原样转发后端腿帧，会打挂经中继会话；评审自身
第二部分亦已推翻），改以「去掉 known 豁免 + 登记已知限制」落地；Q11 的「cache 持 `Bind::adopted` pin」建议
**经 AGENTS 原则 5 判断会引入反向依赖**，改以「rank 尾部淘汰天然保护 adopted + 新条目永不拒」落地；Q23 的
「assoc 家族取 `a.backend`」经查**拨腿会话建 socket 时 backend 未知**，改以**双栈 socket**落地。

**三项阻塞项已在 v2 修订**：① F1 删除 B 版（Q1）；② F2 判据改「Data 帧才采纳」+ 处置审计后半句（Q5/Q6/Q7/Q8）；
③ F5 改无状态 cookie（Q16）+ F7.4 明确「中继无遥测通道，本批只做日志/单测」（Q22）+ 全局 assoc 上限升为必做
（Q31）+ F3 加补投上限（N1）。

**判据行登记已由 6 条补至 11 条**（补 E9 触发集 / E8 `idle=` / R1 256 语义 / C14 构建 / 探针 flags 命名空间 /
R10 + N5 吞吐 + F9 字节口径 + F12 E21 取值路径）。

**未决/待用户裁决**：见 §6。

---

## 5. 本批范围外挂账（复验 + 评审订正后）

- ~~`assign_ip` 无池分配兜底~~（**已删，评审 D1**）：Go `assignIPLocked` **同样硬拒**
  （`peers.go:529` `ErrTunnelIPConflict`，`peers_test.go:375-388` 明确断言不再退池）——**两端行为一致（都拒）**，
  Go 的「退池」注释（`peers.go:514`/`tunneladdr.go:23`）**已腐化**。非偏差，无需挂账。
- **中继无 JSON 遥测通道**（评审 Q22）：`relay status` 载荷仅 `{enabled,state,listen,tokenMask}`，
  `Relay::Stats` 无访问器 ⇒ 本批「入状态面」类修法（F7.4/F8/F10 计数）**只能落日志 + 单测**；
  新建通道属后续批次。
- **中继下行 v6 客户端方向**（评审 Q23）：`relay_cli.rs:99-103` 主监听口 v4-only ⇒ 仅修 assoc（后端）方向，
  客户端方向 v6 挂账。
- **`RelayLog::logf` 无缓冲**（`logfile.rs:93-98`）：F6 只做节流；缓冲写属 Q-I。

---

## 6. 待用户裁决项（**已裁决 2026-10-07**）

> **状态：已裁决（2026-10-07）**——下列四项已由用户拍板，第 2 棒（实现）按此执行。

1. **F5 方案选择**：无状态 cookie 挑战（评审推荐、wire 不变、实现面较大）vs `pending_legs` 分表（改动小、
   但保留 pending 灌注面与 Q17 三条语义约束）。**设计文档推荐无状态 cookie**。
   - **裁决结果（2026-10-07）**：**实现期复验发现「无状态 cookie」不可实现**——PROOF 校验必须知道
     HELLO 的 pubkey（token 模式 `HMAC(secret,"relay-psk"‖nonce‖pubkey)`、开放模式 DH 均需），而
     PROOF wire 固定 50B **不含 pubkey**、`label=sha256(pubkey)[:8]` 不可逆 ⇒ wire 不变前提下无解
     （已核 Go `leg.go` 与 Rust `relayleg.rs`：HELLO→CHALLENGE→PROOF 是三段独立数据报）。
     用户据此**改采本项「备选」= pending 小表**（≤64、满按最旧淘汰**不拒绝**、PROOF 通过才进
     `legs`，含 Q17 三条语义）。已落 `relay/mod.rs`（`PendingLeg`），并登记判据行影响。
2. **F7.4 观测通道**：本批「只做日志 + 单测观测」（推荐，保「行文不变」承诺）vs 新建中继遥测通道 vs
   并入 R10（**R10 行文变更**，连带 Q-B 验收口径）。
   - **裁决结果（2026-10-07）**：**只做日志 + 单测观测**（不新建遥测通道、不改 R10 行文）。
3. **F11 `tunnel_addr` 撞车守卫**：是否本批做（改变极稀有/可研磨设备的 wire 派生地址，双侧须同步）vs 挂 Q-J。
   - **裁决结果（2026-10-07）**：**本批做**（roadmap 口径）。
4. **F12 范围**：本批最小守卫（推荐）vs 完整语义挂 Q-J（已在文档中按「本批最小 + Q-J 完整」拆分）。
   - **裁决结果（2026-10-07）**：**本批最小守卫**，完整语义挂 Q-J（roadmap 口径）。

---

## 7. 第 2 棒（实现）定稿留痕

> 实现与代码门全记录 = `docs/reviews/QC.md`（含 dsh 代码门意见原文摘要 + 逐条处置）。
> 本文档正文不再改动；以下为代码门（`/tmp/dsh-review/r4.tywkTq/`，exit=0）确认的**正文措辞订正**：

1. **§2 F2「垃圾包路径闭合；合法帧伪造不可行（需 WG 密钥）」**：表述过强（代码门 L1）——
   收紧后「意外/非 Data 来源」不再触发采纳，但 **Data 帧头（2 字节）可伪造仍会采纳**（内容才需要
   WG 密钥）；与 Go（任意包即采纳）相比是严格收紧，属**已知限制**（处置见 QC.md §4.2/§5）。
2. **§3 E8 行「≤15 次 / ≤60s」**：为**每次未采纳期**口径（`rearm` 归零；恢复阶梯重开预算）——
   已按此改写 `INTEROP-CRITERIA.md`（代码门 M3）。
3. **§2 F5 残余（代码门 M1）**：单源洪水刷空 `pending` 的极端时序下，合法 PROOF 会静默计
   `forged`（后端 5s 重发 HELLO 自愈）——已登记（登记表 F5 行边缘注记）。
4. **§2 F4 之 Q14 论证**：`race_seen` 实际不受「候选 cap + F2」钳制（代码门 L2），只受 rearm
   周期清空约束——残余记入 QC.md §5。
5. **§3 R10 行**：补「等腿窗 `pend` 入队即计 `forwarded_up`（既有语义）」+「`dropped` 输入集
   含全局 assoc 上限拒绝」（代码门 M4）。
