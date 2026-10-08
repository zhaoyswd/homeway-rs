# Q-K 批记录：出口侧有界 IPv4 分片重组 + 分片感知（F1/F2/F4/F5-a~d）

- **批**：Q-K（Q-B F7 遗留 + 设计门带出的两条更早静默失败）
- **棒次**：第 2 棒（实现 + 代码门 + 收口）；第 1 棒设计 = `docs/reviews/QK-design.md`（v2，含 §10 实现注记）
- **基线**：`main` @ `72f1325`（Q-F-B 收口）
- **日期**：2026-10-08
- **改动面**：根 `Cargo.toml`（smoltcp features）、`crates/homeway-core/src/server/intercept/{mod.rs,nat.rs,reasm.rs(新)}`、
  `crates/homeway-core/src/wgcore/mod.rs`（F5-b 门）、`crates/homeway-core/src/server/engine.rs`、
  `crates/homeway-core/src/daemon/proto.rs`、`crates/homeway-cli/src/unified_cli.rs`、`docs/INTEROP-CRITERIA.md`

---

## 1. 实现清单

| # | 内容 | 落点 |
|---|---|---|
| **F1** | 出口 RX 有界分片重组：新模块 `reasm.rs`（`FragKey` newtype / `Ctx` / `Reassembler` / `Dropped` / `PushResult`；上限 64 上下文 / 每源 4 / 64 片 / 4 MiB / 超时 30s 不续期 / 淘汰最老 / 重叠三态一律整条丢弃）；`on_plain` 拆为 `Ipv4FragHdr::parse` 门 + `route_plain`（**唯一投递入口**，原 F7 分支改 `debug_assert` 绊线）；重组完成改 `total_len`/清 flags+off/重算 IP 校验和；**不碰 L4**；`sweep` 在 `pump`/`pump_hold` 每拍开头 + `close()` 清空 | `intercept/reasm.rs`（新，~730 行 + 13 单测）、`intercept/mod.rs`（`on_plain`/`route_plain`/`sweep_reasm`/`note_frag_drop`/`close`）、`intercept/nat.rs`（`Ipv4FragHdr`/`FragSlice`，`fix_ip_checksum` 改 `pub(crate)`） |
| **F2** | `fragDrop` 重定义（RX 丢弃总分片包数）+ 恒等式 `fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`（由 `Stats::note_frag_drop` 单次记账结构保证）；新增 6 计数（`fragReasm`/`fragBad`/`fragOverlap`/`fragTimeout`/`fragLimit`/`txFragDrop`）`snapshot()` 9→15；限频日志两条（键 = 纯 kind、两个封闭具名字段、节流 1/100；重叠/非法不记行）；`EngineInterceptBits`/`ServeInterceptBits`（serde default）/`unified_cli` additive 接线 | `intercept/mod.rs`、`server/engine.rs`、`daemon/proto.rs`、`homeway-cli/src/unified_cli.rs` |
| **F4** | ICMP：`build_icmp(orig, ty, code)` 内核（**去 proto 门** + 抑制集 `0.0.0.0`/组播/`255.255.255.255`）；薄封装 `build_icmp_unreachable` 自留 `proto==17`；只发 type 11 code 1（超时且首片在位），载荷 8B | `intercept/nat.rs`、`intercept/mod.rs`（`note_frag_drop` 的超时分支） |
| **F5-a** | smoltcp `fragmentation-buffer-size-65536`（恢复 R4 前静默丢的 >1472 客户端载荷；出口栈同步抬高 ⇒ 大 UDP 回复真分片） | 根 `Cargo.toml` |
| **F5-b** | `udp_payload_gate`：`20+8+len > 65535` ⇒ `ConnErr::DatagramTooLarge`（新增变体，thiserror）；`(1253, 65507]` 不报错 | `wgcore/mod.rs` |
| **F5-c** | smoltcp `reassembly-buffer-count-8`（客户端并发重组槽 1→8） | 根 `Cargo.toml` |
| **F5-d** | 出口 TX `on_tx` 分片感知：非分片逐字节不变；首片 = `by_rw_port` 命中后 `rewrite_src_first_fragment`（IP 源 + UDP 源端口 + **RFC 1624 增量校验和**，0 保持 0）；非首片 = 只改 IP 源 + IP 校验和（查 TX 分片表）；表 `(dst, ident, proto)` 末片精确回收 + 10s TTL + 上限 64（插入路径留位）；**表未命中 ⇒ 丢片 + `txFragDrop`** | `intercept/mod.rs`（`on_tx` 三分支 + `tx_frag` 表 + `sweep_tx_frag`/`evict_oldest_tx_frag`）、`intercept/nat.rs`（`rfc1624_update`/`rewrite_src_first_fragment`） |

## 2. 测试 / 判据证据

### 2.1 门（本棒实跑）

| 门 | 命令 | 结果 |
|---|---|---|
| 单测全量 | `cargo test --workspace --no-fail-fast` | **757 passed / 0 failed**（首轮 754；代码门整改 +3：`shorter_final_fragment_conflicts`/`ip_options_preserved_and_oversize_dropped`/`tx_frag_table_cap_on_insert_path`）。无 flake 复现（本轮零红） |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | clean（**注**：本机 cargo 1.99 要求 `-D warnings` 走 `--` 之后——与 `tools/ci-local.sh` 的写法一致） |
| 词表 | `tools/check-vocab.sh` | **PASS**（声明 5 单元/26 值；ledger sha256 一致；本批零改动） |
| OHOS 交叉 | `cargo check --target aarch64-unknown-linux-ohos -p homeway-core -p homeway-cli -p homeway-capi` | 通过（仅既有 `libc::time_t` deprecated 警告，与本批无关） |
| 单元级 | `cargo test -p homeway-core --lib 'intercept::'` / `'intercept::reasm::'` | 71 passed（intercept） / 13 passed（reasm） |

### 2.2 本批核心用例（真形态，非自造替代）

| 用例 | 断言要点 | 覆盖设计编号 |
|---|---|---|
| `large_udp_roundtrip_both_directions` | 客户端 UDP 发 **1300B**：客户端 TX 真分片（≥2 片）→ 出口 **RX 重组**（`fragReasm==1`、`fragDrop==0`）→ transit 拨号命中**原目的端口**（上游回环 echo 收到 1300B）→ 出口 TX 真分片（≥2 片，F5-a）→ **F5-d 反重写** → 客户端**重组**收全且逐字节一致（`txFragDrop==0`） | T1/T19/T24/T37 + F5-d 端到端 |
| `tx_frag_incremental_checksum_matches_full_recompute` | 取 **smoltcp 产出的真分片**：全片 IP 源一致 = 原目的；重组后的 UDP 校验和**自洽**（伪头+段折叠 == 0xFFFF）**且与 `rewrite_src` 全量重算逐字节相等** | T28/T29 |
| `client_multi_concurrent_reasm` | 两报文真分片**手工交错**注入同一客户端栈 ⇒ 两条都重组成功。**判别力已实测**：把 feature 换回 `reassembly-buffer-count-1` ⇒ 该用例红（报文 B `None`）；换回 8 ⇒ 绿 | T38（F5-c 的判别面） |
| `tx_non_first_fragment_touches_only_ip_source` | 非首片处理：**只有 IP 校验和 + IP 源**变（其余逐字节不动）；载荷前 2 字节**恰好 = 在册 rw_port** 也不得改写（表路径，R15-2 形态的回归钉）；末片精确回收 | T30/T31 |
| `icmp_*`（4 例） | type 11 code 1 形状/校验和/载荷 8B；无首片不发；抑制集三态；TCP 分片超时也发（内核无 proto 门）而 type3/code3 仍只对 UDP | T22/T23/T24/T25 |
| `reasm::*`（13 例） | 乱序/末片判定/偏移非法三态/重叠三态（部分·内含·同区间不同字节）/末片边界不可变（更短末片 = 冲突）/超时不续期/上限与淘汰最老/每源闸/片数闸/尾随字节不夹带/**IP 选项（ihl=24）保留 + 输出超 u16 ⇒ Bad** | T2–T16 + 门内中-1/低-1 补测 |
| `stats_snapshot_shape_and_frag_identity` / `intercept_bits_new_keys_and_backward_compat` | 快照 9→15 追加末位、既有索引不变、恒等式；`serve.status` 6 新键 camelCase + **旧载荷反序列化取 0** | T26/T27 |
| `udp_payload_gate_boundaries` | 1253/65507 通过、65508 ⇒ `DatagramTooLarge`（调用点为 `udp_send` 首部） | T36 |
| `client_udp_fragments_up_to_64k` | 4096B 与 **65000B** 载荷都真分片发出、线上分片载荷总字节 == len+8 | T35（R4 回归） |

### 2.3 flake 甄别（判据三项：隔离复跑绿 + 与改动面无交集 + 基线可复现）

- 本棒全量两跑（整改前/后）**零红**，无需甄别。
- 代码门评审者独立跑全量 lib 两轮出现 3 条（`server_request_overrun_disconnects` / `per_host_cap_cascade_and_corrupt_file` / `socks_dead_is_recorded_and_rebuildable`），**隔离复跑均绿、两轮失败集合不同**（其中两条是 in-表 flake「固定端口互撞」）——评审者自评为「高置信 flake」并**如实标注第三支（基线复现）未做**（其纪律禁止写仓库）。本棒采纳该标注：三条均不在本批改动路径上。

### 2.4 未覆盖（如实）

- 出口侧跨报文乱序（两报文分片在隧道上真交错）无 E2E 用例：真栈 `Fragmenter` 单缓冲使该形态只能在测试里手工构造；出口 `Ctx` 是 per-key 结构天然隔离，客户端侧交错已由 T38 覆盖。
- T17（全局字节闸）改判为「编译期断言 + 不可达证明」（设计 §8.1 允许口径；见 `QK-design.md` §10.2 低-3）。
- OHOS 只做 `cargo check`（未链接/真机）。

## 3. 判据 / 登记（与代码同批 commit）

`docs/INTEROP-CRITERIA.md` 四处：

1. **判据变更记录 +1 行**：Q-K「IPv4 分片处置」——Q-B F7 由「接受的差异」→「已重组交付」（从/到含重叠、ICMP、TX 分片感知三面）；
2. **计数输入集 +3 行**：`fragDrop` 重定义（含恒等式）、6 个新计数（additive，status 载荷兼容旧值、DC18 人读行不变）、**出口→客户端线上包形态变化**（1 报文变 N 包 + 客户端并发槽 1→8 + `udp_send >65507` 可见失败）；
3. **已知口径注记 +1 条**：Q-K 条目（含 ICMP 客户端不可见、三类重复片处置、片数上限的 MTU=1280 前提假设、ICMP 载荷长度分歧、无低水位滞回、客户端 60s/并发槽残余）；
4. 判据变更记录**尾部批注块**追加 Q-K 段（含 **additive 观测行 2 条**的留痕——代码门低-4 补）。

**Q-B F7 差异注销记录**：`docs/INTEROP-CRITERIA.md` 的 Q-B F7 条目就地加「⚠️【Q-K 批，2026-10-08 收口】本条已注销」指针；Q-B 的 `docs/reviews/QB.md` / `QB-design.md` **不追改**（历史记录）。注销面 = ① 入站分片不再一律丢弃（有界重组交付）；② 重叠/非法/超时/超限四类失败路径的计数语义（`fragDrop` 重定义）；③ 超时回 ICMP 11/1；④ 出口 TX 侧新增分片感知（该面 Q-B 未涉及，属对称缺陷修复）。

## 4. 设计文档实现注记（第 1 棒文档 → 第 2 棒订正）

- 设计与代码的**形态偏差 1 处**（`PushOutcome` 枚举 → `PushResult` 结构）与**代码门检出缺陷 8 条**（改码 5 / 登记 1 / 就地订正 2 / 注释 1）见 `docs/reviews/QK-design.md` §10；§3.1-A10（放大比数字）与 §3.2（字节闸「窄窗口」措辞）已**就地订正**并在 §10.2 点名。
- **无「发现矛盾即静默降级」**：唯一与文档不符的实现取舍（`PushResult` 形态）已在 §10.1 显式登记。

## 5. 代码门（dsh 外部评审）

### 5.1 轮次档案

| 项 | 值 |
|---|---|
| 轮次目录 | `/tmp/dsh-review/r29.3l5A8V`（`prompt.txt` / `output.md` / `stderr.log`） |
| 命令 | `cd /Users/zhaozhe/Documents/projects/homeway-rs && dsh --profile headless "$(cat …/prompt.txt)" > …/output.md 2> …/stderr.log; echo "exit=$?"`（**前台捕获**；本机 harness 将其转后台，但 exit code 由命令自身落盘捕获） |
| **exit code** | **`exit=0`** |
| 结果规模 | `output.md` **145 行**（已用 Read 工具读全，非截断转述） |
| 评审者自查 | `git status --short` 与评审开始**逐行一致**；`git stash list` 空；未在仓库内新增/修改/删除任何文件 |
| 门结论（原文） | 「**可以过代码门（无阻断项）**」——**0 高** / 1 中 / 8 低 |

### 5.2 门结论原文摘要（分级）

- **【中-1】** `try_finish` 的 `(ihl + total) as u16` 静默回绕（首片带 IP 选项 ⇒ `ihl ≤ 60`、`total` 可达 65535 ⇒ 最大 65595）⇒ 产物被 `route_plain` 的 parse 拒而**静默丢**，却已计 `fragReasm`；直接违反已登记的「输出报文上限 65535」。
- **【低-1】** 更短的另一片末片可**改小** `total_len` ⇒ 已收齐却永不完成、白占 30s（gVisor 同形态是 `ErrFragmentConflict` 整条丢弃）。
- **【低-2】** TX 分片表**插入路径**可瞬时到 `TX_FRAG_MAX + 1`（表满且条目全新鲜时）。
- **【低-3】** T17（全局字节闸）无真用例覆盖。
- **【低-4】** 两条新限频日志未按同批惯例在 `INTEROP-CRITERIA.md` 留痕（Q-F-B 批注块写「additive 观测行 10 条」）。
- **【低-5】** 设计 §3.1-A10 的「放大比 < 1」数值不成立（最小诱发 28B ⇒ 回报 56B ≈ 2×）；安全结论仍成立。
- **【低-6】** 死代码/冗余转发：`Ipv4FragHdr::frag_key()` 零调用、`count_src_of()` 纯转发。
- **【低-7】** `PushResult` 注释「与 `dropped` 互斥」与实际不符。
- **【低-8】** `push` 早退路径丢弃已累积的 `dropped`（今天不可达，结构易碎）。
- 评审者另列**「看过，没发现问题」13 组**（重叠三态不可绕、上限逐条给数、**最坏 RSS ≈4.3 MiB 逐项求和复核成立**、无隐式续期、日志不可被对端诱发、ICMP 各态、F5-d 全链正确性、Q-B F7 两洞未复活、`route_plain` 唯一入口（grep 实证）、判据 4 处登记逐条吻合、无越界/panic、热路径开销可接受、改动面纪律）。

### 5.3 逐条处置表

| # | 严重度 | 处置 | 落点 | 证据 |
|---|---|---|---|---|
| 中-1 | 中 | **认同 → 已改码**：`try_finish` 改三态 `Finish{Done,Pending,Oversize}`，`ihl+total > 65535` ⇒ `drop_whole(Bad)`（不静默、不虚计成功） | `reasm.rs`（`try_finish` + `push` ⑥） | 新测 `ip_options_preserved_and_oversize_dropped` ② |
| 低-1 | 低 | **认同 → 已改码**：`total_len` 改**不可变**（`!mf && total != end ⇒ Overlap`）+ `debug_assert`；设计 §4.2-5 已就地补该子形态 | `reasm.rs` ③⑤ | 新测 `shorter_final_fragment_conflicts`（连带 3 包） |
| 低-2 | 低 | **认同 → 已改码**：首片登记路径先 TTL 清扫、再 `while len >= MAX` 淘汰（为 insert 留位） | `intercept/mod.rs::tx_rewrite_first_frag` | 新测 `tx_frag_table_cap_on_insert_path` |
| 低-3 | 低 | **认同 → 改判为设计允许的替代口径**：编译期断言 + 「不可达」证明（`Σbytes ≤ 64×65535 = 4,194,240 < 4 MiB`）；§3.2 措辞已就地订正；实现保留为纯防御面 | `reasm.rs`（断言 + `make_room` 文档）、设计 §3.2/§8.1/§10.2 | 断言在编译期生效；证明写入注释 |
| 低-4 | 低 | **认同 → 已登记**：批注块补 additive 观测行 2 条（键=纯 kind、节流 1/100） | `docs/INTEROP-CRITERIA.md` | 本次 diff |
| 低-5 | 低 | **认同 → 已订正文档**：单次 ≤2×、结构性速率 ≤2.1/s（安全结论不变：只回已认证 peer、不经公网） | 设计 §3.1-A10 | 本次 diff |
| 低-6 | 低 | **认同 → 已删/收敛**：删 `frag_key()`；`count_src` 直接作日志点调用 | `nat.rs`/`reasm.rs`/`mod.rs` | grep 零残留 |
| 低-7 | 低 | **认同 → 注释订正**：「可以同时非空」 | `reasm.rs` | 本次 diff |
| 低-8 | 低 | **认同 → 已改码**：新增 `drop_whole_into(..., &mut Vec<Dropped>)`，全部早退路径合并返回 | `reasm.rs` | 代码 + 既有用例全绿 |

**高危**：无（0 高）。**未采纳/保留意见**：无（9 条全部认同并处置）。

## 6. 残余登记（防「静默漏做」）

1. **客户端入站重组超时仍是 smoltcp 的 60s**（设计 F6：不改——属客户端核内部实现，不在隧道线协议面）；
2. **内存压力淘汰无低水位滞回**（gVisor/Linux 是 4 MiB→3 MiB 滞回；本实现逐条淘汰）——已登记；
3. **`REASM_MAX_FRAGS = 64` 以「对端 IP MTU = 1280」为前提**（合法最坏 53 片；异种 MTU 对端会被 `fragLimit` 拒）——已登记，真机未见异种 MTU 客户端；
4. **ICMP 11/1 对本仓 Rust 客户端不可见**（smoltcp `process_icmpv4` 只处理 Echo；客户端核无 icmp socket）——价值 = 出口侧可观测 + 历史 Go 同形，已登记；
5. **ICMP 载荷 8B ≠ gVisor 的 RFC 1812 ≤548B**（有意分歧，已登记；将来对齐改动点 = 给 `build_icmp` 加 `max_payload` 参数）；
6. **ICMP 只在超时发**（内存压力淘汰不发）——已登记；
7. **`tx_frag` 键完备性依赖「smoltcp `Fragmenter` 单缓冲」**（评审者把握不足项⑤）——有 TTL/上限兜底，不构成无界；真机若出现同 dst 同 ident 碰撞，表现为丢片 + `txFragDrop`（不发坏片）；
8. 出口侧跨报文乱序无 E2E 用例（§2.4）。

## 7. 真机大 UDP 验证待办（**用户触点**，不在本棒）

- 用能发 **>1252B UDP** 的应用（QUIC / DNS-over-UDP 大响应 / TFTP 类）在手机核打到出口侧真实服务，**两个方向都验**：
  1. **出向**：手机 → 出口 >1252B（修前 >1472 静默丢、1253–1472 被 F7 丢）；
  2. **返向**：出口 → 手机 >1252B（修前 1253–1472 被 `on_tx` 写坏校验和 ⇒ 静默丢；≥1473 出口 TX 静默丢）。
- 观测面：出口 `serve status`（或日志）的 `fragReasm` 增长、`fragDrop`/`txFragDrop` 保持 0；`tcpdump` 可见分片与（超时时）ICMP 11/1。
- 建议量级：先 1300B（2 片）→ 4096B（4 片）→ 65000B（53 片，F5-a 上限形态）。

## 8. 收口面（**主会话**执行，本棒不越界）

- `docs/REVIEW-ROADMAP.md` 状态总览加/改 Q-K 行（本棒未动该表——按协议由主会话回填）；
- `AUDIT-2026-10-07.md` 勾选（Q-B F7 条目按本批注销）；
- tier `tools/tailcat/homeway-rs.pin` 前进（**用户触点**，出 App 包才需要）。
