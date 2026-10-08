# Q-I 尾段（性能细节批 · 尾段）— 实现与代码门记录

> 批次：Q 批整改 · Q-I 尾段（`docs/REVIEW-ROADMAP.md` §Q-I 尾段；2026-10-08）。
> 规格真源 = `docs/reviews/QIt-design.md`（v2，设计门已过：dsh `r21.NQMZfM` exit=0，9 组意见全部认同并入）。
> 派单 = 主会话 Q-I 尾段第 2 棒（实现）任务书（含三条裁定：F0 从根上修 / DNS TTL 缓存不做 / F0 归属本批）。
> 本文 = 实现棒记录：**Q-H F0 回归取证与落地** + 实现清单 + 设计门/代码门两轮意见摘要 + 逐条处置 +
> 测试证据 + **性能证据（前后对比表，带 loadavg 与时刻）** + 判据登记 + 不做项/上报项/残余。
>
> 基线 = `git HEAD = 713f963`（Q-H 收口）；A 臂二进制 sha256 `f65b4c52…`；**测量用** B 臂二进制
> sha256 `d03fdf17…`（F2 回退后的形态）；**评审后处置版**（代码门 M1 `--ddns` carve-out + L3
> `debug_assert` + L4 测试订正）二进制 sha256 `a407a973…`——与测量版只差 CLI 空值解析扩面（测量
> 全程未传 `--ddns`）与 debug-only 断言/测试，**出口数据面逐字节等价 ⇒ 性能结论沿用测量版**。
> **F2 曾按要求实现并实测，随后按设计 §4.3 止损闸门整条回退**（中间版本二进制 `4ac9e303…`，
> 产物留档 `/tmp/qit-f2variant/`）。

---

## 0. Q-H F0 回归：取证结论与落地（**跨批问题，放最前**）

### 0.1 现象与影响面（复现）

Q-H 的取值纪律（`cli_flags::take_value_or_exit`）把**全部取值 flag 的空值**判死（唯一 carve-out
`--recover-cause`）。`tools/local-rust-exit.sh`（`:69`）、`tools/perf-ab.sh`（`:89/93`）、
`tools/matrix.sh`（`:192`）的 Rust 出口都带 `--stun= --stun6=` 形态 ⇒ **R5 起的全部 Rust 本地出口
harness 在 Q-H 之后全部起不来**（本轮实现棒复现：`serve … --stun= …` ⇒ exit 2，25s 内不见「serve 就绪」）。

### 0.2 取证：Go 基线（只读 oracle + 实跑）对空值的处理

| flag | Go 源码（`baseline/homeway/internal/server/cli.go`） | `bin/homeway-go` 实跑（2026-10-08 14:07） | 结论 |
|---|---|---|---|
| `--stun` | `:36` 帮助文本「（观测 IPv4 公网映射；**空 = 关**）」；`:119-122` `if explicit["stun"] { stun = *stunServer }` | `serve --stun=` ⇒ **正常起服**（进程存活、token/端点行照打） | **Go 接受空值**（空 = 关） |
| `--stun6` | `:37` 帮助文本同款「空 = 关」；`:123-126` 同形分支 | 同上（与 `--stun=` 一起给） | **Go 接受空值** |
| `--relay` | `:33` 帮助文本「（…；**空 = 不用中继**；未显式给 = 用 config）」；`:107-110` `if explicit["relay"] { relay = *relayServer }` | `serve --relay=` ⇒ **正常起服** | **Go 接受空值** |
| `--public-endpoint` | `:32` 帮助文本「显式公网端点…」；`:98-106` **explicit 即逐项校验** `netip.ParseAddrPort`——空串 `strings.Split("", ",") = [""]` ⇒ 校验失败 | `serve --public-endpoint=` ⇒ `homeway: --public-endpoint "" 非法（not an ip:port；须为逗号分隔的 ip:port）`，**rc=1** | **Go 也拒**（非回归） |
| `--ddns`（**代码门 M1 扩面**） | `:38` 帮助「显式给 = 覆盖 config 的全部条目」；`:127-135` `explicit["ddns"]` ∧ 空 ⇒ `ddnsList = nil`（= 清空） | `serve --ddns=` ⇒ **正常起服** | **Go 接受空值**（空 = 清空）；Rust 侧 `serve_cli.rs` 原本已有该语义分支但**不可达**（parser 先拒）⇒ 一并行 carve-out |
| `--bind-interface`（登记不扩） | `ResolveBind("")` ⇒ `BindAuto`（Go 接受空值 = auto） | 未跑（Rust 现状 rc=2） | 有等价写法（`--bind-interface auto`）⇒ **登记为已知识别差异**，不扩 carve-out |

结论：**Rust 对 `--stun`/`--stun6`/`--relay`/`--ddns` 的空值拒绝 = 对 Go 文档化语义的回归**（产品 CLI 缺陷）；
`--public-endpoint=` 空值两边都拒，不属回归面。设计棒的「工具侧最小修法（删两个 flag）」能解锁测量，
但**没修根**；主会话裁定「从根上修」，本棒按裁定执行。

### 0.3 落地（逐 flag carve-out，只放这四个；`--state` 等仍 fail-fast）

- `cli_flags.rs`：新增 `take_value_empty_ok_or_exit`——`--flag=` / `--flag ""` 的空值返回 `Ok("")`；
  **缺值（末尾 / 下一个是 flag）与其余取值纪律不变**（仍 fail-fast，`--state` 等路径/标识 flag 不受影响）。
- `serve_cli.rs`：`stun`/`stun6`/`relay`/`ddns` 四站点改走该取值器（语义不变——既有映射代码本就按
  `is_empty()` 判「关」，`--relay=` ⇒ `cfg.relay = None`）；用法行补一行说明。
- **`--public-endpoint` 保持拒绝**（Go 同拒；不进 carve-out）；
  **`--ddns` 一并 carve-out**（代码门 M1：Go 实测接受且语义 = 清空 config 条目；本仓 `serve_cli.rs`
  早有该分支但被 parser 判死 = 死代码，现与注释一致）。
- 工具脚本**不改**（`local-rust-exit.sh`/`perf-ab.sh`/`matrix.sh` 的 `--stun=` 形态随之复活）；
  `local-rust-exit.sh` 另按设计 F7 加 `HOMEWAY_BIN` 覆盖（臂切换用）。
- 单测：`cli_flags::tests::empty_value_carveout_forms`、`serve_cli::tests::empty_value_carveout_flags_accept_empty`
  （等号形/空格形空值都收；`assemble_result` 落到 `ServeConfig`：`stun=""`、`relay=None`、`ddns=[]`）。
- **终版冒烟**（评审后二进制 `a407a973…`）：`serve … --stun= --stun6= --relay= --ddns= …` ⇒ 进程存活
  （DDNS-EMPTY-OK）；`local-rust-exit.sh start 9` ⇒ 就绪。
- **端到端复验**（2026-10-08 14:43）：`tools/local-rust-exit.sh wipe 9 && start 9` ⇒ 起得来、
  `status 9` 可采判据行（`serve 就绪：wg=:42659…`）；**互操作冒烟**：Rust 出口 + Go 基线客户端
  `host add` ⇒ 客户端日志 `服务会话: 就绪（会话在位，无桥直通）`（C16 同串）。
- 判据登记：见 §5（「取值 flag 纪律的局部回退」一行，按 Q-H 先例登记）。

---

## 1. 实现清单（逐条，符号定位）

| 条 | 结论 | 改了什么（文件 / 函数） |
|---|---|---|
| **F0** | ✅ 落地（§0） | `cli_flags.rs`（+`take_value_empty_ok_or_exit`）、`serve_cli.rs`（**四站点** stun/stun6/relay/ddns + 用法行 + 单测）、`tools/local-rust-exit.sh`（+`HOMEWAY_BIN`） |
| **F1** | ✅ 落地 | `DnsFaces` 加 `udp_rx: Vec<u8>`（`attach` 一次分配），`service` 的 UDP 读循环改 `recv_slice(&mut self.udp_rx)`（消每拍 64KB 栈零初始化）——`intercept/dnsface.rs` |
| **F2** | ❌ **实现后按止损闸门整条回退**（§3.3） | 曾改 `intercept/mod.rs`（`append_reactor_pollfds`/`reactor_turn(ext)`/`pump_with`）+ `engine.rs`（`build_driver_pollfds` + 腿切片收窄 `[1..leg_end]`）；**当前代码回到 Q-H 形态**，`reactor_turn` 只剩「尝试与回退」文档注释留痕 |
| **F3** | ✅ 落地 | `DnsFaces::service(&mut self, dns, sockets, scratch: &mut [u8])` 穿参 → `service_face` 读循环用 `scratch[..min(len, FACE_READ_CHUNK)]`；调用点 `intercept/mod.rs::service_dns` 传 `&mut self.rx_scratch[..]`（三个不相交字段） |
| **F4** | ✅ 落地（最低优先级） | `TcpConn.rx/tx` 换 `super::VecDequeLite`（`intercept/mod.rs` 的该类型升 `pub(crate)` + `with_capacity`）；13 站点全改（`send_slice(remaining())`/`consume(w)`/`push`/`remaining().len()`）；测试 `conn_with`/两处断言同步。**内存包络（代码门 M2 订正：rx/tx 同形）**：rx backing 最坏 128KiB→≈256KiB（×64 ⇒ ≤+8MiB）；**tx 同形 2×`CONN_TX_CAP`(256KiB) ⇒ 相对旧形态增量 ≤256KiB/连接（×64 ⇒ ≤+16MiB 最坏，实测未现）** |
| **F5** | ✅ 落地 | `UpstreamsState.list` → `Arc<Vec<String>>`，`list()` 返回 `Arc`（变更换新 Arc、`text()` 逐字不变）；worker 自持 `scratch: Vec<u8>`（懒分配）穿参 `respond → forward → exchange`（`conn.recv(&mut scratch[..])`）——`server/dnsproxy.rs` |
| **F6.1** | ✅ 落地 | `files_server.rs`：新 `read_frame_into(r, buf: &mut Vec<u8>) -> io::Result<Option<usize>>`（`None`=终止帧、`Some(n)`=载荷在 `buf[..n]`，只零填增量）；`receive_upload` 循环外持 `frame_buf`（每连接自持）；旧 `read_frame` 降级为 `#[cfg(test)]` 薄壳 |
| **F6.2** | ✅ 落地 | `files.rs`：`Stream.buf` 换 `VecDequeLite`（含 `read_line_opt` 的 `position`+`drain(..=pos)` 与 `read_frame` 的 `drain` 全改前缀偏移 + 摊还压缩） |
| **F6.3** | ⚠️ **不做（API 缺口，升级项）** | `write_all_owned` 单拷化**在父 API 层不可实现**：`wgcore::Engine::write` 的 `WriteOut.back` 只在**零接纳**回带原 Vec（`wgcore/mod.rs` `Cmd::Write` 分支 `back: if n == 0 { Some(data) } else { None }`）——部分接纳（`0 < n < len`，smoltcp 常规形态）的尾部由引擎丢弃 ⇒ 设计 `§2 F6.3` 的「部分接纳 ⇒ `drain(..n)` 原地保尾」与代码现实矛盾；完全落地需改 wgcore 回执语义（**超出本批文件边界**：主会话裁定本批只动 intercept/dnsproxy/files*/tools/CLI-F0）。设计 §5.3 意见 5 本就允许「可只做 F6.1+F6.2」。**上报** |
| **F7** | ✅ 落地（+ 实测跑通三臂） | 新 `tools/qi-ab.sh`：`speedtest`/`files`/`rss` 三模式 + 平衡轮序（A,B,B,A,A,B）+ 1Hz loadavg（带时间戳/轮首轮末标记/逐轮峰值判作废）+ 逐轮 `lsof`+`host list` 端点核实 + 出口 stdout 归档 + `ps -o time=` CPUδ + 1Hz RSS + `sample` 末轮 + `bins.sha256` + `trap` 恢复二进制/清进程；`local-rust-exit.sh` + `HOMEWAY_BIN` |
| 不做（按裁定） | — | **DNS TTL 缓存**（裁定 2：登记不做 + 附录 A 留档 + 建议另起小批）；「每查询新 socket」保持（防投毒属性）；`dnsface::service_face` 两处 `collect()` 小 Vec（F3 同函数，本轮未动，登记） |

---

## 2. 测试与静态检查证据

| 门 | 结果 |
|---|---|
| `cargo test --workspace` | **除墙钟敏感 flake 外全绿**。代码门处置后两轮实测：① 中载轮 = homeway-core lib **618 passed / 1 failed / 4 ignored**（唯一红 = `wgcore::stackb::tests::stack_to_stack_tcp_transfer_fills_window`，**ROADMAP 在册 flake**）；② 高载轮（loadavg 7–10）= **616 passed / 3 failed / 4 ignored**，三个红**全为墙钟/时序断言**：`stackb::stack_to_stack_tcp_transfer_fills_window`（在册）、`daemon::tests::handshake_deadline_beats_slow_drip`（**在册外，代码门 L6 同发现**）、`term::service::tests::attach_size_applies_to_pty`（在册「PTY 时序」）；**隔离复跑 3×3 = 9/9 全绿**（含 loadavg 7.25 条件下），且三者改动面（wgcore 栈对栈 / daemon 握手期限 / term PTY）与批内改动（files/intercept/dnsproxy/CLI）**零交集** ⇒ 按 ROADMAP 三证判 flake（其中 `handshake_deadline` 提请补进 flake 表，见 §7.3）。其余 crate 全绿（cli bin 35、E2E `qh_config_failfast` 5、capi/fuzz/向量族全绿） |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean（无告警）**（本批内两次复跑 + 代码门评审**强制重建 fingerprint 后**再复检 = 0 告警） |
| 新增单测（**5 个新测试函数 + 1 处既有测试扩展**——代码门 L1 订正计数） | F0：`cli_flags::empty_value_carveout_forms`、`serve_cli::empty_value_carveout_flags_accept_empty`；F1：`intercept::dns_faces_end_to_end` **扩展**（同拍两包 alpha/beta 各自应答、question 段逐字节对应）；F4：`dnsface::tcp_conn_buffer_matches_vec_reference`（与 `Vec+drain` 参照实现逐字节等价 + 整帧消费无残留）；F5：`dnsproxy::upstream_list_is_arc_snapshot`（`Arc::ptr_eq` 窗口内同一快照 + 变更换新 Arc + 旧快照内容不变）；F6.1：`files_server::read_frame_into_reuses_buffer`（帧长交替不错位 + 等长帧 ptr/len 不变 = 不重复零填）；F6.2 由既有 `files::frame_parse_boundaries_with_arbitrary_chunking` 等全套覆盖（全绿） |
| 既有回归 | `intercept` 全套（水位/兴趣位/部分写续传/EOF 挂起/DNS 腿/过境 TCP）、`dnsproxy` 全套（fake 上游/兜底/期限/worker 并发/上游跟随）、`files`/`files_server` 全套、`qh_config_failfast` E2E 全绿 |
| flake 甄别 | 按「隔离复跑绿 + 与改动面无交集 + 基线可复现」三证（ROADMAP 判据）——本批三个红**逐条隔离 3/3 绿（合计 9/9）**：`stackb`（在册 + wgcore 面）、`attach_size_applies_to_pty`（在册「PTY 时序」）、`handshake_deadline_beats_slow_drip`（在册外，代码门同发现 ⇒ 登记 + 提请补表） |
| 手工冒烟 | ① F0：`local-rust-exit.sh wipe/start 9` 起得来 + 判据行可采；② F0 互操作：Go 基线客户端 `host add` ⇒ `就绪（会话在位，无桥直通）`（C16）；③ F6：**256MiB 随机文件 put→get 四轮 sha256 全部相同**（`/tmp/qit-after-files/`，4/4 `sha_eq=YES`） |

---

## 3. 性能证据（判据集，全部带 loadavg 与时刻）

### 3.1 口径

- 臂：A = HEAD `713f963`（二进制 sha256 `f65b4c52…`）；B = 实现后（**测量用** sha256 `d03fdf17…`；
  评审后处置版 `a407a973…`，见文首注）；中间态 `B_f2`（F2 版，sha256 `4ac9e303…`）。
- 出口 = 本地私有实例（`/tmp/qit-exit9`、端口 42659、`--bind-interface none`、免 stun flag 形态）；
  客户端统一进程恒定用 **binA** 起（单变量 = 出口臂）；token `--loopback-only`（en0 臂除外）。
- 负载 = `speedtest --down 15s --up 15s --streams 4`；CPU = 出口进程累计 `ps -o time=` 差 ÷ 墙钟；
  RSS = 1Hz max；`sample = 出口进程 20s`（末轮）。
- 判决纪律：1min loadavg ≤4 作数、**任一轮跑中 >6 整轮作废**（本轮 harness 已 1Hz 落盘 + 逐轮峰值判定）。

### 3.2 基线（A 臂 = HEAD，2026-10-08 14:04:52–14:07:14，loadavg 轮首 1.93→轮末 3.65；产物 `/tmp/qit-base-1/`）

| 轮 | 时刻 | loadavg(1/5/15) 轮首 | down MB/s | up MB/s | CPUδ(s) | 墙钟 | CPU% | s/GB | RSS max KB |
|---|---|---|---|---|---|---|---|---|---|
| A-r1 | 14:04:52 | 1.93/1.98/2.00 | 94.72 | 123.58 | 45.28 | 41.2 | 109.9% | 12.59 | 28704 |
| A-r2 | 14:05:40 | 2.22/2.04/2.02 | 94.54 | 123.11 | 45.26 | 41.1 | 110.0% | 12.54 | 26784 |
| A-r3（带 sample） | 14:06:27 | 2.75/2.22/2.08 | 89.35 | 124.03 | 43.14 | 41.3 | 104.5% | 12.24 | 29520 |
| **中位** | — | — | **94.54** | **123.58** | **45.26** | 41.2 | 109.9% | **12.54** | 28704 |

`sample`（驱动线程 `homeway-serve-drv`，总样本 14584）：`reactor_turn→poll` **1481（10.15%）**、
`engine→poll`（阻塞）**4303（29.50%）**、两项合计 **5784**、`DnsFaces::service→__bzero` **250（1.71%）**、
`__bzero` 全线程 280（1.92%）。

### 3.3 F2：实现 → 实测 **负收益** → 按止损闸门整条回退（`B_f2`；2026-10-08 14:44:03–14:48:49，loadavg 轮首 3.59–4.46、无一轮 >4.6；产物 `/tmp/qit-f2variant/`）

| 臂 | down 中位 | up 中位 | CPUδ 中位 | s/GB 中位 | 逐轮 CPUδ |
|---|---|---|---|---|---|
| A | 96.00 | 122.70 | 45.31 | 12.50 | 45.56 / 45.31 / 42.83 |
| **B_f2** | 99.39 | 115.50 | **51.90** | 14.57 | 52.14 / 51.90 / 48.54 |
| **B/A** | +3.5% | **−5.9%** | **+14.5%**（三轮一致 +14.5/+14.5/+13.3%） | +16.6% | — |

`sample` 叶帧（r3）：`reactor_turn→poll` 1514（10.36%）→ **0**（syscall 确实消掉了）；
`engine→poll` 4269（29.22%）→ **6826（48.76%）**；**两项合计 5783 → 6826（+18%，不降反升）**。
拍频（出口 `reactor 观测 pump=/5s` 行，**全量 21 行均值**——代码门 M3 订正）：A **70,635/5s（14.1k 拍/s）**
→ **B_f2 119,320/5s（23.9k 拍/s）**，比 **1.69**；按 35s 载入窗折算多出 ≈**341k 拍**（原稿按末段子集写
66k/107k、比 1.62、287k 拍，口径不准——结论不变）。
`DnsFaces::service→__bzero`：244（1.67%）→ 0。RSS：A 29680 → B_f2 28576 KB。

**机制（与产物自洽）**：设计 §2 F2 的「第二通道」——上游 fd 就绪成为引擎唤醒源——被实测放大成
**拍频 1.6×**：阻塞 poll 从「1/5ms 批处理拍」变成「按上游到达率唤醒」，每拍仍要跑全量 pump
（2× `iface.poll` + drain_tx + service_sockets + service_dns + reap_idle + refresh_waiters + 整形释放）
⇒ 每拍固定成本被放大：三轮 CPUδ 一致 +14.5%，up 吞吐一致 −5.9%（A 122.5/123.2/122.7 vs B 115.1/115.5/116.1）。
「一次 syscall 入口」的节省（设计模型 ≈2.6–4.6 线程点）被多出来的 ~287k 拍（35s 窗）吃光还倒欠。
**止损闸门（设计 §4.3：两项 poll 样本合计降幅 <20% ⇒ 回退）触发 ⇒ 独立回退（代码回到 Q-H 形态）**，
`兜底=N` 观测字段与两条行为变更**随回退未落地、不登记**（见 §5）。

### 3.4 最终 B（F2 回退后）lo0 臂（三次尝试，因外部负载两次整轮/单轮作废；产物 `/tmp/qit-after-lo0/`、`/tmp/qit-after-lo0-r1/`、`/tmp/qit-after-lo0-r2/`（作废）、`/tmp/qit-after-lo0-void/`（作废））

作废记录（纪律执行，不挑数）：尝试 1 起步 loadavg **16.14**（紧接 release 构建，1min 未回落）⇒ 全轮作废
（down 全落到 10–35MB/s）；尝试 2 的 B-r3 峰值 **6.90**、尝试 3 的 B-r3 峰值 **8.73** ⇒ 该单轮作废
（B-r3 相应读数 63.08/32.61MB/s、8.49/14.48MB/s 属作废轮，不入判决）。

判据集 = 两次尝试各自的 **有效 A/B 相邻对**（配对分析，设计 §4.1）：

| 对 | 两轮时刻（A/B 相邻） | 轮首 1min loadavg | A down/up/CPUδ/sGB | B down/up/CPUδ/sGB | 配对比值（down / up / CPUδ / sGB） |
|---|---|---|---|---|---|
| P1 | A-r1 15:07:56 / B-r1 15:08:43 | 2.58 / 3.26 | 92.50/126.90/45.14/12.47 | 97.83/121.90/45.89/12.59 | +5.8% / −3.9% / +1.7% / +1.0% |
| P2 | B-r2 15:09:30 / A-r2 15:10:17 | 3.46 / 3.80 | 95.83/122.84/45.14/12.51 | 98.84/135.23/45.53/11.78 | +3.1% / +10.1% / +0.9% / −5.8% |
| P3 | A-r1 15:22:18 / B-r1 15:23:05 | 2.64 / 3.22 | 97.94/133.20/45.19/11.87 | 97.74/121.18/45.28/12.51 | −0.2% / −9.0% / +0.2% / +5.4% |
| P4 | B-r2 15:23:53 / A-r2 15:24:41 | 4.83 / 4.37 | 96.14/122.55/45.40/12.54 | 92.35/133.16/45.42/12.16 | −3.9% / +8.7% / +0.0% / −3.0% |
| **配对中位** | — | — | — | — | **down +1.4% / up +2.4% / CPUδ +0.6% / s/GB −1.0%** |

（两轮作废判据 = 各自产物 `loadavg.tsv` 的 1Hz 采样区间峰值 >6：尝试 2 的 B-r3 峰值 6.90、
尝试 3 的 B-r3 峰值 8.73；作废轮读数不入判决集。）**判读：端到端无收益也无回归**（CPUδ 配对中位 +0.6%，
落在前段登记的噪声带 ±3% 内；down/up 的离散（−9%…+10%）远超 B/A 差本身）。
A 臂三次独立测量（基线 14:05、尝试 2 15:08、尝试 3 15:22）中位 CPUδ 45.26/45.14/45.19s、s/GB
12.54/12.51/12.46 ⇒ **A 测量复现性良好（±0.2%）**，B 与 A 同量级。
RSS（只取有效轮）：尝试 2：A max 30528 / B max 29552 ⇒ **0.968**；尝试 3：A 30160 / B 29392 ⇒ **0.974**。

### 3.5 `sample` 叶帧（最终 B；A 口径同 §3.2 与 §3.3）

| 叶帧 | 归属 | A（三次采样） | **B（最终）** | 判据 | 判定 |
|---|---|---|---|---|---|
| `DnsFaces::service → __bzero` | F1 | 250（1.71%）/ 244（1.67%）/ 265（1.82%） | **0**（0/13999、0/11800、0/11842 三点） | =0 | **绿** |
| `__bzero` 全线程 | F1 | ≈280（1.9%） | 48–66（0.34–0.56%） | 辅助 | 绿 |
| `reactor_turn→poll` | F2 | 1481–1905（10.2–12.2%） | 1414 / **1536 / 1402 / 1812** / 429†（10.2 / 10.8 / 11.8 / 11.9 / 3.6%） | — | 记录（F2 回退） |
| 两项 poll 合计 | F2 | 5742–6777 | 5917 / **6087 / 6067 / 9777** / 5171† | ≥30% 降幅 | **不适用（F2 已回退）** |

（F3/F4/F5/F6 为冷路径/结构性：F3 以单测为主（设计原判）；F4 参照实现对拍单测；F5 结构性（alloc 站点消失 +
`Arc` 快照单测）；F6 见 §3.6。）
**口径注记（代码门 M3）**：上表 B 侧五个采样点中，`1414` 来自 `/tmp/qit-after-lo0-r1/B-r3`（有效轮）、
`1536/1402/1812` 来自三次尝试的 A/B 末轮、`429†` 来自 `/tmp/qit-after-lo0/B-r3`（**作废轮**，峰值 8.73、
吞吐 8.49/14.48MB/s——未剔除以示全量）；`9777` 为 en0 有效轮、`5171†` 为对应作废轮。**B 侧全部采样
（除 `1414`）落在作废轮或旁证臂 ⇒ 只作「F1 零值 + 形态与 A 同构」的佐证，不作收益/回归判据**
（F2 回退的等价性另由「`reactor_turn` 与 HEAD 逐字节相同、`engine.rs` 零改动」的源码比对证明）。

### 3.6 files 臂（F6 专用；257MiB 随机文件 put→get；产物 `/tmp/qit-after-files/`）

**注意：四轮 loadavg 区间峰值 8.36–10.31 ⇒ 按纪律全部作废**（数字不作判决，仅留档；原因 = 外部负载，
见 §3.4 同源）。可用的**判据外证据**：

- **字节等价回归（正确性，与负载无关）：4/4 轮 `sha256(src) == sha256(dst)`** ⇒ F6.1/F6.2 的缓冲复用/前缀偏移**不破坏帧边界与内容**（设计测试计划②）。
- 形态注记：daemon 托管远程形态（`files --host`）在本机实测「流已终结（gone）」（A 出口 + A 客户端
  同样失败 ⇒ **非本批回归**，pre-existing）；本臂改用 **CLI 直连形态**（`files --token`，= 设计
  §4.2「唯一主侧 = CLI 进程侧」）。
- 统计口径（作废轮仅参考）：clean-ish 的两轮（A-r2 11.21/12.72s、CLI CPU 8.10/8.42s；B-r2
  11.70/12.72s、8.55/8.36s）**差异在 ±4% 内**；`io-control`（`cp` 同尺寸）0.33s vs 传输 11–13s
  ⇒ **非 IO 绑定**（可判），但**测不出收益**（设计已预登记「测不出 ⇒ 按「收益 < 带内噪声」如实记录」）。

### 3.7 RSS 合成多流臂（判据④ 销账；产物 `/tmp/qit-after-rss/`，loadavg 峰值 4.4–4.9 ⇒ 有效轮）

| 臂 | 轮 | 出口 RSS max |
|---|---|---|
| A | r1 / r2 | 12480 / 12592 KB |
| B | r1 / r2 | 12480 / 12512 KB |
| **B/A** | — | **0.994（判据 ≤1.1 ✓）** |

**臂有效性存疑（如实登记）**：16 条慢读合成流在慢读期被对端 reset（16/16 `ConnectionResetError`
于 `s.recv`）⇒ 未形成**持续**慢消费压力（设计意图是「慢消费者 + 满 backlog」压力测试）；本臂只能作
「同负载下 RSS 持平」的旁证，**L6 的合成多流销账仍不完整**（残余登记 §7）。

### 3.8 en0 产品形态臂（旁证口径；产物 `/tmp/qit-after-en0/`，2026-10-08 15:36–15:39）

`--rounds 2 --en0`（4 轮 A,B,B,A）。窗口整体不洁：逐轮 1min 峰值 A-r1 5.04 / B-r1 4.80 /
B-r2 4.33 / **A-r2 15.34（作废）**；吞吐全轮远离 lo0 形态（27–36MB/s，en0 sendto 税 + 本机
代理软件（Surge 在跑）干扰疑点），**有效对仅 1 对**：A-r1 36.07/37.67/CPUδ 44.95/36.15
vs B-r1 27.48/34.79/46.96/44.72 ⇒ down −23.8% / up −7.6% / CPU +4.5%——**单对、无可归因机制、
且该窗口 5min loadavg 5.0–7.8**。**判读：本臂未取得可比判决，不作结论**（设计原列旁证；
判据③ 由 lo0 的 4 个有效配对承载）。RSS（含作废轮）：A max 36016 / B max 34624 ⇒ 0.961。
`sample`（B-r2，有效轮）：`DnsFaces::service→__bzero` **0**（A 侧 0.20% ⇒ 同轮 A 也未复现高值——
en0 窗口采样稀释，历史 A 值 1.5–2%），两项 poll 合计 9580 → 9777（同量级，F2 已回退）。

### 3.9 逐条预期 vs 实测（判绿/判红，不粉饰）

| 条 | 设计预期 | 实测 | 判定 |
|---|---|---|---|
| F0 | 空值 carve-out 恢复 Go 语义、工具复活 | Go 三 flag 实跑接受、Rust 修后 `local-rust-exit.sh` 起得来 + Go 客户端互操作「就绪（会话在位）」 | **绿**（§0） |
| F1 | 叶帧 1.95% → **0** | **0**（三采样点） | **绿** |
| F2 | ≈2.6–4.6 线程点收益；闸门：poll 合计降幅 <20% ⇒ 回退 | 合计 **+18%**、CPUδ **+14.5%**、up **−5.9%**（三轮一致）、拍频 **+62%** | **红 ⇒ 按闸门整条回退**（如实记录） |
| F3 | 非每拍税（冷路径），不计收益 | 单测全绿；无本地叶帧判据（设计原判） | **绿（结构）** |
| F4 | 冷路径结构性，不计收益 | 参照实现对拍单测绿；内存包络 +≤8MiB 最坏（RSS 判据已核） | **绿（结构）** |
| F5 | 结构性（≈0.02–0.05% CPU，不设墙钟判据） | `Arc` 快照 + 懒分配单测绿；alloc 站点消失 | **绿（结构）** |
| F6.1/F6.2 | 估算 1–3% CPU（files 臂）；测不出即如实记 | **256MiB sha256 4/4 一致**；CPU 无可测差异（臂作废，仅 clean-ish 两轮 ±4%） | **绿（正确性）/ 无收益（噪声带）** |
| F6.3 | 单拷化（设计自评可只做 F6.1+F6.2） | **不可行**（wgcore `WriteOut.back` 语义缺口）⇒ 登记 + 上报 | **不做（升级项）** |
| F7 | 三臂 harness + loadavg/端点/日志归档 | 三臂全部实测跑通（speedtest/files/rss）；1Hz loadavg + 逐轮峰值判作废已生效（本轮即据此作废 1 轮 + 2 整轮） | **绿** |
| 总判据① poll 合计降幅 ≥30% | F2 的直接证据 | F2 回退 ⇒ **不适用**（F2 未交付） | 不适用 |
| 总判据③ 吞吐不回归 ≥−2% | 不回归 | lo0 配对中位 down **+1.4%** / up **+2.4%**（有效 4 对） | **绿（噪声带内）** |
| 总判据④ RSS ≤A×1.1 | ≤1.1 | lo0 **0.968/0.974**、files 1.026、rss **0.994** | **绿** |
| 总判据⑤ F1 叶帧 = 0 | =0 | **0** | **绿** |
| 参考量 s/GB（模型 −1~−4%） | 参考 | lo0 配对中位 **−1.0%**；A 自身复现 ±0.2% | 记录（**收益在带内噪声**，不宣称 CPU 收益） |

---

## 4. F7 harness 交付与自检

- `tools/qi-ab.sh`（新）：`speedtest`/`files`/`rss` 三模式；平衡轮序 `A,B,B,A,A,B`；1Hz loadavg
  （带时间戳 + 轮首/轮末标记 + **逐轮峰值判作废**）；逐轮 `lsof -a -p <client> -i UDP` + `host list --json`
  端点核实；出口 stdout 逐臂归档；`ps -o time=` CPUδ；1Hz RSS；末轮 `sample` 叶帧分析（内嵌列位深度解析）；
  `trap` 恢复二进制 + 清进程；`bins.sha256` 留痕；出口启动用**免 stun flag 形态**（对 F0 前后二进制都可跑）。
- 自检（本轮实测即检验）：作废判定生效（本轮作废 1 轮 + 2 整轮）；files 臂形态缺陷被发现并改用直连形态；
  rss 臂「铸 token 的临时出口未停 ⇒ 撞实例锁」缺陷已修（`stop_exit`）；客户端进程恒用 binA（单变量）。

---

## 5. 判据行与观测面影响（同批 commit）

结论：**零编号判据行变更、零 wire 变更**。`docs/INTEROP-CRITERIA.md`「判据变更记录」新增 **3 行**：

1. **取值 flag 纪律的局部回退：空值 carve-out（`--stun`/`--stun6`/`--relay`/`--ddns`）**——对照 Q-H 行登记
   （`--public-endpoint=` 仍拒，Go 同拒；`--bind-interface=` 登记为已知识别差异不扩；其余纪律不变）。
2. **DNS 面缓冲复用（行为注记，非判据行）**——F1/F3/F4/F5 的缓冲形态改动；`E22`/`E4` 行文与计数输入集零变化；
   F4 内存包络**rx/tx 同形**（rx 128KiB→≈256KiB ⇒ ≤+8MiB；tx ≤256KiB/连接增量 ⇒ ≤+16MiB 最坏）登记在内。
3. **F2 尝试后回退——零残留（登记留痕）**——`兜底=N` 字段与两条唤醒时点行为变更**随回退未落地、不登记**。

（另：附录 A 的 DNS TTL 缓存**不做** ⇒ 计数语义零影响，仅设计文档留档。）

---

## 6. 评审记录

### 6.1 设计门（r21，第 1 棒产出，详见 `docs/reviews/QIt-design.md` §5）

轮次目录 `/tmp/dsh-review/r21.NQMZfM/`，**exit=0**；9 组意见（逐条标签：2 高 + 11 中 + 多条低）
**全部认同并入 v2**（0 不认同）。本棒按 v2 实现（F0 按主会话裁定改为 CLI 逐 flag carve-out）。

### 6.2 代码门（r22）

- **轮次目录** = `/tmp/dsh-review/r22.CMf3ik/`（`prompt.txt` / `output.md` 152 行 / `stderr.log`）；
  **exit code = 0**（前台跑、已捕获）。
- **评审规模**：**高危 0** + 中 **3** + 低 **6**；「看过没发现问题」**13 项**。**门结论（评审原文）：
  「改后过」**（M1/M2/M3 建议本批处置或显式登记，均不阻塞）。
- 评审独立做的事：全量读 diff + 回源码逐行核（含 `reactor_turn` 与 HEAD 的 **brace-matching 逐字节比对**）、
  **复跑门禁**（低载全绿 + 强制重建 fingerprint 后 clippy 0 告警 + 高载对照暴露 1 个在册外时序红）、
  **独立复算**全部 /tmp 产物（基线/F2 变体/P1–P4/RSS/files/拍频全量）、**Go 与 Rust 双二进制实跑对拍**
  （四 flag 空值的 STARTED/rc 逐形态）、独立核验 F6.3 的 wgcore API 缺口（确认无边界内出路）。

### 6.3 逐条处置

| # | 意见（摘要） | 严重度 | 处置 | 落点 |
|---|---|---|---|---|
| M1 | `--ddns=` 是 F0 同物种残留（Go 实测接受 = 清空；Rust 拒且已有分支**不可达**=死代码）；建议扩 carve-out 或登记 + 改死注释 | 中 | **认同已改（处置①）**——`--ddns` 纳入 carve-out（parser 站点改走 `take_value_empty_ok_or_exit`，既有「空 = 清空」分支变活）；单测同步（四 flag 形态 + `assemble_result` 的 ddns 清空断言）；判据登记行与冒烟结论同步扩面（§0/§5） | `serve_cli.rs`（ddns 站点 + 测试）、`cli_flags.rs` 测试注释、`INTEROP-CRITERIA.md` F0 行 |
| M2 | F4 内存包络只登记 rx，tx 同形未登记（属"登记口径不实"） | 中 | **认同已改（登记订正）**——判据行与批记录两处补 tx：**backing 同形 2×`CONN_TX_CAP` ⇒ 相对旧形态增量 ≤256KiB/连接（×64 ⇒ ≤+16MiB 最坏，实测未现）** | `INTEROP-CRITERIA.md`（DNS 缓冲行）、本文 §1/§5 |
| M3 | 最终 B 的 poll 采样口径不自洽（主判决产物的作废轮采样被静默剔除；拍频报数取了子集、绝对值不可复现） | 中 | **认同已改（口径订正）**——§3.5 逐点标注轮号/作废状态并明写「只作 F1 零值佐证」；§3.3 拍频改全量 21 行均值（70,635→119,320，比 1.69，多出 ≈341k 拍/35s） | 本文 §3.3/§3.5 |
| L1 | 「新增单测（9 条）」计数失真（实为 5 个新函数 + 1 处扩展） | 低 | **认同已改** | 本文 §2 |
| L2 | `files` 引用 `server::intercept::VecDequeLite`——`pub(crate)` 合规，但通用缓冲放 server 面按 Rust 惯例宜独立小模块 | 低 | **登记（形态，零行为风险）**——本批不做（跨面移动 ~15 处引用，churn 大于收益；若后续再被引用第三次则拆 `crate::bytes`） | 本文 §7.3 |
| L3 | `service_face` 新增 `chunk_len > 0` 门 = 「scratch 空 ⇒ 静默停摆」隐含前提 | 低 | **认同已改**——加 `debug_assert!(!scratch.is_empty(), …)` + 契约注释（唯一生产调用点传恒定 64KiB） | `dnsface.rs` |
| L4 | carve-out 测试对同一调用重复断言；「只许 N flag」边界只在 `take_value` 层钉 | 低 | **部分认同已改**——重复断言已清；边界断言保留在 `take_value` 层（parser 站点走 `exit(2)`，进程内测不了）并补注释指路；「真的只放四站点」由 `grep take_value_empty_ok_or_exit`（定义 1 + 站点 4 + 测试）与手工 E2E 冒烟共同钉住 | `cli_flags.rs`、`serve_cli.rs` 测试注释 |
| L5 | harness：① lsof 腿恒空（端点证据实由 `host list` 承担）；② 设计 §4.2 预登记的「en0 采纳到回环 ⇒ 作废并补跑」未实装 | 低 | **部分认同已改**——① `endpoint_capture` 明确标注 lsof 的 macOS 限制 + 增打「本轮采纳端点（ep）」抽取行；② 实装**作废标记**（`round-void` 行进 loadavg.tsv，en0 采纳到回环即标）；**自动补跑未实装**（登记在 §7.3——en0 臂本轮已成「未取得可比判决」旁证，补跑规则留给后续 harness 改进） | `tools/qi-ab.sh` |
| L6 | 高载全量暴露**在册之外**的第二红：`daemon::tests::handshake_deadline_beats_slow_drip`（墙钟断言；隔离 5/5 绿） | 低 | **认同（登记）**——本批记录在案（§7.3）；ROADMAP「已知 flake 登记」表补行属收口动作，**提请主会话一并补** | 本文 §7.3 |

---

## 7. 不做项 / 上报项 / 残余（防静默漏做）

### 7.1 不做（有据）

| 项 | 结论 | 理由 |
|---|---|---|
| **DNS TTL 缓存**（原尾段头条） | **不做（登记 + 设计文档附录 A 骨架留档）** | 设计 §1 条目 5：tier 规格 = raw forwarder（MUST NOT 名单含域名缓存）；缓存削弱「上游跟随（秒级）」可观测性；本批无可执行端到端判据（无带时延的隧道 DNS 探针、上游查询数无观测面）；风险最高 vs 不可证收益。**建议另起小批**（含 tier 规格面与判据登记；再入条件 = 用户裁决 + 先补带时延探针） |
| DNS「每查询新 socket」 | **不做** | 随机源端口 = 防投毒属性（第二维熵），Go 同形 |
| **F2（reactor 并入引擎 poll）** | **实现后回退（止损闸门）** | §3.3：poll 合计 +18%、CPU +14.5%、up −5.9%、拍频 +62%——负收益 |
| **F6.3 `write_all_owned` 单拷化** | **不做（API 缺口，上报）** | `wgcore::WriteOut.back` 只在零接纳回带（§1 F6.3 行）；完全落地需改 wgcore 回执语义 = 超本批文件边界 |
| `dnsface::service_face` 两处 `collect()` 小 Vec | 登记不改 | 实测 0.19%（前段口径）；与 F3 同函数但本轮未动 |

### 7.2 上报项（主会话裁决）

1. **F6.3 的 API 缺口**（§1/§7.1）：设计 §2 F6.3 的前提与 wgcore 现状矛盾 ⇒ 本棒停手登记。
   若要落地：wgcore `Cmd::Write` 回执改为「部分接纳也回带未接纳尾部」（`back` 语义扩面，两处调用方
   需核：files/term/portfwd），属独立小项。
2. **DNS TTL 缓存不做** ⇒ 收口时须同步改 `REVIEW-ROADMAP.md` §Q-I 尾段文字（设计 §6.3-2 既列）。
3. **F0 carve-out 的产品面影响**：`--stun`/`--stun6`/`--relay` 空值从「exit 2」变「按 Go 语义生效」——
   已按 Q-H 先例登记（§5），主会话据此判断是否需回补 Q-H 记录口径。
4. **daemon 托管远程 files 形态（`files --host`）在本机实测失败**（「流已终结（gone）」；A 出口 + A 客户端
   同样失败 ⇒ pre-existing，非本批回归）——建议单开小项排查（本批 harness 已绕开：files 臂用直连形态）。

### 7.3 残余（防静默）

| 项 | 状态 |
|---|---|
| en0 产品形态臂 | 见 §3.8；若未跑成，按「未取证（环境负载）」登记，判据③ 由 lo0 覆盖 |
| RSS 合成多流压力（L6 销账） | 本臂跑成但压力未持续（16/16 慢读被 reset）⇒ 覆盖不完整（§3.7） |
| files 臂收益判据 | 四轮全作废（loadavg 峰值 >6）⇒ 只有「字节等价 + 无可测差异」；设计已预登记「测不出如实记」 |
| `reactor_turn → poll` 每拍零超时 syscall | **维持未决**（F2 反证：并入引擎 poll 的唤醒语义放大拍频 ⇒ 负收益）；后续若要动须先解决「拍频放大」面（例如合并但保留批处理档），**本批登记** |
| `service_face` 两处 `collect()`、`dnsface.rs` Vec+drain 遗留 | 前段/本批同物种清单（F4 已消 `TcpConn`；`collect` 未动） |
| 新在册外 flake：`daemon::tests::handshake_deadline_beats_slow_drip`（代码门 L6） | 高载全量红、**隔离 5/5 绿**（评审实测）；与改动面零交集 ⇒ flake 性质；**建议主会话补进 ROADMAP「已知 flake 登记」表**（本批记录在案） |
| harness 未实装项（代码门 L5） | ① en0 端点自动补跑（只实装了 `round-void` 标记）；② `lsof` 端点核实腿在 macOS 恒空（已改为以 `host list --json` 的 `ep` 为权威口径） |
| `VecDequeLite` 跨面共享的模块位置（代码门 L2） | `files` 引用 `server::intercept::VecDequeLite`（`pub(crate)` 合规）；若将来被第三处引用则拆独立 `crate::bytes` 模块；纯形态，零行为风险 |
| `--bind-interface=` 空值（代码门 M1 同族） | Go = auto（`ResolveBind("")`）/ Rust 拒；有等价写法（`--bind-interface auto`）⇒ **登记为已知识别差异**，未扩 carve-out |
| 生产出口 / tier / homeway 两仓 / baseline | **全程未碰**；测量只用 `/tmp/qit-*` 本地私有实例（`local-rust-exit.sh` #9 + 私有客户端 state + 私有端口），测毕 `pgrep` 核对 |
