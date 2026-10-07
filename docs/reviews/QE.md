# Q-E 批记录：出口服务修复（files / DNS / UPnP / speedtest）

> 批次：Q 批整改 `docs/REVIEW-ROADMAP.md` §Q-E（顺序 … → Q-D → **Q-E**）。
> 本文 = 第 2 棒（实现）产出：实现清单 + 测试/判据证据 + **两轮外部评审（设计门 r9 / 代码门 r10 + 复审 r11）**
> 的逐条处置 + 不做项与残余登记。
> 设计规格 = `docs/reviews/QE-design.md`（v3，设计门已过）；基线 = `git HEAD 6b7237f`。
> 范围 = **鲁棒性/安全性**修复（沙箱、上限、期限、名额、错误分类）；DNS TTL 缓存 / 缓冲复用 = Q-I 尾段，
> UPnP 协议面扩展 / `if_nametoindex` 语义 = Q-J（见 §6）。

---

## 0. 门命令与证据（本树实测）

| 门 | 命令 | 结果 |
|---|---|---|
| 测试 | `cargo test --workspace` | **全绿**：homeway-core lib **555 passed / 0 failed / 4 ignored**（+ 集成/fuzz 轨：4+14+1+3+3+4+2+3+1 全绿） |
| 静态 | `cargo clippy --workspace --all-targets -- -D warnings` | **clean**（exit 0） |
| 已知 flake 甄别 | `wgcore::stackb` / `daemon::tests::server_bad_frame` 隔离复跑 | 均绿（本批全程 5 次 `cargo test --workspace` 未出现 flake） |

**代码门轮次留档**（`~/.agents/skills/reviewer/SKILL.md` 固定姿势；前台跑、成败只认 exit code）：

- 设计门（第 1 棒）：`/tmp/dsh-review/r9.0mrmgB`（`exit=0`；30 条意见全认同并入 v3——见设计文档 §5）。
- 代码门：**`/tmp/dsh-review/r10.QzJd2P`**（`prompt.txt` / `output.md` / `stderr.log`；`exit=0`；
  **2 高 / 5 中 / 19 低**，结论「不可过代码门」）。
- 处置后复审：**`/tmp/dsh-review/r11.TXvvs6`**（`exit=0`；六条点名项**逐条独立复核后全部成立**，
  高危/中危新问题「**无**」；结论「**本次处置足以过代码门**」）。

---

## 1. 实现清单（F1–F10）

| # | 要点 | 主要落点 | 关键测试 |
|---|---|---|---|
| **F1**（P0-4） | `rel_path` 的 `canonicalize` 快路径整体替换为**逐分量复核** `resolve_in_root`（六动词唯一入口单点收口）：绝对目标符号链接一律 `not_found`（即便根内，对齐 Go `os.Root`）；相对目标 `..` 词法弹栈、弹出根外 ⇒ 拒；`read_link` 失败 ⇒ `not_found`；链接链深度上限 **8**（= Go `rootMaxSymlinks`）；ENOENT ⇒ **绝不返回未复核候选**（剩余队列含 `..` 一律 `not_found`——代码门 H1 订正） | `files_server.rs`（`rel_path`/`resolve_in_root`/`RESOLVE_MAX_DEPTH`） | `rel_path_*`（10 案）+ 端到端 `symlink_escape_no_write_outside` + `rel_path_dotdot_after_missing_component_rejected`（H1 负例，**修前红**） |
| **F2**（P1） | files **客户端**响应行去掉 64KB 误移植门（对齐 `facade/files_op.rs` 的 `read_line_capped(false)`） | `files.rs::read_line_opt` | `response_line_over_64k_accepted`（200KB entries + 12MB text） |
| **F3a**（P1🔎） | 上传**磁盘水位**：起始门（创建 `.tierpart` 前）+ 每 8 MiB 进行中门；保留量 1 GiB；拒绝码 = 既有 `op_failed`（**不新增码**）；`statvfs` 失败 fail-open + 节流告警（首 3/每 100）；注入缝 `DiskWatermark` | `files_server.rs`（`avail_bytes`/`quota_verdict`/`check_upload_quota`/`receive_upload`） | `quota_verdict_three_states` / `avail_bytes_smoke` / `upload_rejected_when_low_disk`（起始 + 中途 + 无 `.tierpart` 残留） |
| **F3b**（N1） | 服务端请求行上限**对齐 Go**：`MAX_REQUEST_LINE` 64KB **累积中判负**（`fill_buf` 逐块）⇒ `invalid_arg`「请求行超过 65536 字节」（Go 同串）+ 收线；busy 路径读出错/超限先回错误响应（超限 `invalid_arg` / IO 类 `op_failed`，对齐 Go `errorResponse`） | `files_server.rs`（`LineErr`/`read_line`/`line_err_response`/`serve_busy`） | `request_line_over_64k_rejected`（msg 逐字 + 收线 + 60KB 仍通）/ `busy_path_over_64k_line_replies_invalid_arg` |
| **F4a**（P1） | DNS 上游读循环改 **per-attempt 绝对期限**（`attempt_deadline = now + budget`，四处 `continue` 回到循环顶按剩余重设；全局 `deadline` 仍只喂 TC→TCP） | `dnsproxy.rs::exchange` | `exchange_absolute_deadline_under_poison_upstream`（生产形态 200ms/2500ms；**修前红/挂死**） |
| **F4b**（N7） | DNS TCP 腿**拨号 + 读写共用同一绝对期限**（`connect_within` 逐地址 `connect_timeout`；`DeadlineIo` 逐 syscall 收敛——代码门 M1 订正） | `dnsproxy.rs`（`exchange_tcp`/`DeadlineIo`/`connect_within`） | `tcp_leg_connect_bounded` / `tcp_leg_read_bounded_under_dribble`（**修前 5.02s ⇒ 红**） |
| **F4c** | DNS worker 2 → **64**（`DnsConfig.workers` 可注入，缺省 64） | `dnsproxy.rs`（`DEFAULT_WORKERS`/`DnsConfig`/`spawn`） | `worker_pool_concurrency`（8 workers 并行 + 单 worker 控制臂 + 缺省 64 断言） |
| **F4d** | 回投容量 rx/tx **双侧** 64 槽/64KB → 256 槽 / 316,624B（`MAX_IN_FLIGHT × 1232 + 1232`） | `dnsface.rs`（`UDP_META`/`UDP_BUF_CAP`） | `udp_tx_capacity_matches_inflight`（200×1232B 全进 tx）/ `udp_rx_capacity_matches_inflight` |
| **F5**（P1） | accept 错误分类（`WouldBlock`→Retry；`EMFILE/ENFILE/ENOBUFS/ENOMEM/ECONNABORTED/EPROTO/EINTR`→Backoff 不退出 + 节流日志；其余 Fatal 记行退工）+ **名额 RAII 回滚**（`ConnReservation` 先建后 spawn）+ 共享可测 accept 循环 `serve_stoppable_accepts`（files/speedtest 复用；engine 两侧退工/起不来记行） | `files_server.rs`（`AcceptAction`/`classify_accept_err`/`serve_stoppable_accepts`/`ConnReservation`）、`speedtest_server.rs`、`engine.rs` | `classify_accept_err_cases` / `serve_stoppable_accepts_survives_transient_error`（**修前红**）/ `serve_stoppable_accepts_honors_stop` / `serve_stoppable_accepts_fatal_returns_err` / `conn_reservation_rolls_back_on_drop` |
| **F6a** | speedtest `Limits`（六字段 + `with_limits`，Go `withDefaults` 同判点 + 溢出夹取）注入；硬超时**绝对化**（`DeadlineIo` 覆盖读/写/flush 全部触达点） | `speedtest_server.rs`（`Limits`/`DeadlineIo`/`arm_io`） | `conn_timeout_is_absolute_under_dribble`（**修前不返回**）/ `normal_session_completes_within_loose_timeout` |
| **F6b** | `release(id)` 按会话收口（`conns: Vec<(u64, UnixStream)>`；`ConnGuard{id}`）；`close_all` 语义保持 | `speedtest_server.rs`（`CountingRegistry`/`ConnGuard`） | `release_by_session_id_and_close_all` / `stop_cuts_remaining_sessions_after_release` |
| **F6c** | busy 路径：帧吞**按声明长度流式吞完**（错位修复）+ 帧吞 `2s` / 吞输入 `1s` **绝对**期限（两腿 `DeadlineIo`——代码门 H2 订正） | `speedtest_server.rs`（`reply_then_close`/`drain_one_frame`/`BUSY_*`） | `busy_path_drains_frame_and_bounded` / `busy_path_dribble_released_by_absolute_window`（**修前 ≈9.9s ⇒ 红**） |
| **F7**（P2/N8） | UPnP `http_call`：分调用体积闸（desc 1 MiB / SOAP 64 KiB = Go 同值，**超限报错**而非 Go 的静默截断）+ `Content-Length` 一致校验（声明超限立拒；实收 ≠ 声明报错；收满声明即返——M5）+ 成环读**绝对期限** + 拨号期限（字面 IP 快路径） | `upnp.rs`（`http_call`/`connect_within`/`UpnpError::{RespTooLarge,ContentLengthMismatch,BudgetExhausted}`） | `http_call_cap_length_and_deadline`（6 案）/ `http_call_returns_when_declared_length_satisfied`（keep-alive） |
| **F8**（P2/N4） | SSDP 应答三条件采纳（私网/环回来源 + `HTTP/1.1 200`/`HTTP/1.0 200` + 非空 `LOCATION`）；不满足继续读 | `upnp.rs::ssdp_response_ok` | `ssdp_response_ok_cases`（9 案） |
| **F9**（P2/N3） | 一轮**只枚举一次**（`MappingTable` 快照）；轮级**先加后删**（`clean_mappings` 跳过 `prefer`）；`allow_evict = verify && Ours` 所有权门（718 才删）；候选去重；缩租「先 add → 718 才 delete + add」 | `upnp.rs`（`list_mappings`/`mapping_round`/`select_external_port`/`add_port_mapping`/`re_add_short_lease`） | `enumerate_once_per_round`（N+1）/ `evict_only_when_verified_ours` / `round_level_add_before_delete` / `candidates_deduped`（**去重前红**）/ `shrink_lease_add_before_delete` |
| **F10**（N5/R2） | 期限**穿透到 SSDP 腿**（每轮 recv/sleep 按剩余夹取）+ **全局预算**（公网端点 40s 单 ctx 贯穿映射与外网 IP 查询、缩租 8s）；`pick_igd_before`（discover 闭包注入）+ `ShrinkOutcome` 五态归因 | `upnp.rs`（`ssdp_location`/`discover_igd`/`pick_igd_before`/`shrink_lease_before`/`ShrinkOutcome`）、`engine.rs`（两处调用点） | `pick_igd_honors_global_deadline` / `discover_igd_deadline_penetrates_ssdp`（修前 1.71s ⇒ 现 0.25s） |

**实现与设计的两处订正**（已在 `QE-design.md` §F1 加实现注记 + 本表）：

1. 链接链深度上限 **40 → 8**（对齐 Go `rootMaxSymlinks = 8`，`os/root.go:70`）。
2. ENOENT 合并分支增「剩余队列含 `..` ⇒ `not_found`」guard（设计 F1 原方案未覆盖；代码门 H1 实测可写出根外）。

---

## 2. 测试证据

**总账**：`cargo test --workspace` = homeway-core lib **555 passed / 0 failed / 4 ignored**（基线 548 → +7 净增，
本批新增 30 条用例、改写 4 条既有用例）；`cargo clippy --workspace --all-targets -- -D warnings` exit 0。

**P0-4 负例清单**（全部**先在修前复现为红**再实现转绿）：

| 用例 | 形态 | 修前表现 |
|---|---|---|
| `rel_path_escape_symlink_with_missing_leaf_rejected` | `link -> <根外>` + 不存在叶子 | `Ok(candidate)` 放行（红） |
| `rel_path_absolute_target_even_inside_root_rejected` | 绝对目标 + 根内 | 放行（红；Go 拒） |
| `rel_path_dangling_symlink_rejected` | 绝对悬空 / 相对越界 / 相对根内悬空 | 前两者放行（红） |
| `rel_path_link_chain_and_cycle_rejected` | `l1 -> l2 -> 根外`；`a ↔ b` 循环 | 放行（红） |
| `rel_path_component_is_regular_file_maps_op_failed` | `file/leaf` ⇒ `op_failed` | 放行（红） |
| `rel_path_dotdot_after_missing_component_rejected`（**H1**） | `l -> gone/../evil/leaf` + `evil -> 根外` | `Ok(root/evil/leaf)` ⇒ 根外落文件（红，真实 crate 复现） |
| `symlink_escape_no_write_outside`（端到端） | 六动词打 `link/<新叶>` | `write` 真写出根外（红）；现全 `not_found` + 根外目录零新增 |

**"修前必红"的其余关键回归**：`exchange_absolute_deadline_under_poison_upstream`（修前不返回/挂死）、
`serve_stoppable_accepts_survives_transient_error`（修前一次 `EMFILE` 即摘服务）、
`conn_timeout_is_absolute_under_dribble`（修前一直活着）、
`stop_cuts_remaining_sessions_after_release`（修前 `release` 清整表 ⇒ 切不到会话）、
`busy_path_dribble_released_by_absolute_window`（修前 ≈9.9s，独立探针复核）、
`tcp_leg_read_bounded_under_dribble`（修前 5.02s）、`candidates_deduped`（去重前 2 次尝试）。

**墙钟断言纪律（T4）**：阈值均留 ≥2× 余量或改事件计数断言（`busy_path_dribble` 5s 最坏 vs 8s 阈值；
`discover_igd_deadline_penetrates_ssdp` 0.25s 实测 vs 1s 阈值；`pick_igd_honors_global_deadline` 1s 无修复形态 vs 500ms 阈值）。

**已知 flake 甄别**：高负载下 `wgcore::stackb`（墙钟 Mbps 断言）与 `daemon::tests::server_bad_frame`
为既有 flake（Q-C/Q-I 前批已记录）；本批 5 次全量跑 + 2 次隔离复跑均绿。

---

## 3. 判据行登记（`docs/INTEROP-CRITERIA.md`，与代码同批 commit）

- **编号判据行行文**：E13/E14/E17/E22/E4 **原串不变**（`git diff -U0` 逐串核过）⇒ 「判据变更记录」表
  **不新增行**（设计 §4.4 已说明理由；本批非判据行变更全部是 additive 观测行）。
- **「计数输入集 / 数值语义变化」表**（+4 行）：E22 `fail`/`fallback` 输入集；`udpDrop` 三输入不变 + 容量
  64 槽/64KB → 256 槽/316,624B；E13 会话时长上界收紧；UPnP 每轮枚举 `3(N+1)` → `N+1`；**新增观测行（additive）**。
- **「已知口径注记」**（+12 条）：F1 沙箱收紧（含 TOCTOU 未收口措辞）、F2 响应行不设界、F3b 与 Go 对齐、
  F3a 上传水位、F7 体积闸/长度一致/期限、F8 SSDP 来源过滤、F9 所有权门与轮级序（**含 M2 口径订正：
  只保证 `prefer`**）、F10 期限穿透与全局预算（**含 M3 订正口径**）、F5 accept 分类、F6c busy 帧吞
  （**含 H2 订正**）、M1 DNS TCP 腿读侧期限、M5 收满声明长度即返、UPnP 缩租五态归因。

---

## 4. 代码门（r10）意见与逐条处置

> 轮次目录 **`/tmp/dsh-review/r10.QzJd2P`**（`exit=0`；`output.md` 122 行 / 19,981 B，**已用 Read 全文读完**）。
> 评审独立做的事（增强可信度）：把 `target/debug/deps/libhomeway_core-*.rlib` 链进仓外探针，走真 UDS/TCP/mock IGD
> 复现关键结论；三条深挖子代理并行读码；`cargo check --target aarch64-unknown-linux-ohos` 复跑 exit 0。

### 4.1 高危（2 条，**必改**——均已改 + 修前红证据）

| # | 意见 | 处置 | 证据 |
|---|---|---|---|
| **H1** | `resolve_in_root` 的 ENOENT 合并分支在 `..` 回升后返回**未复核分量** ⇒ 仍可写出根外（`l -> gone/../evil/leaf` + `evil -> 根外`，真实 crate 复现 `leaf` 落在根外） | **认同并改码**：剩余队列含 `..` ⇒ 一律 `not_found`（POSIX/Go 在缺失分量处即 ENOENT，不会先词法消 `..`）；补负例 `rel_path_dotdot_after_missing_component_rejected`（纯函数 + 六动词 + 根外零新增）；`§4.3.1` 措辞按实登记 | 修前：`Ok(root/evil/leaf)`（红）；修后：全 `not_found` + 根外 `["sentinel"] → ["sentinel"]` |
| **H2** | busy 拒绝路径「帧吞 2s 绝对期限」不是绝对的（只 arm 一次；`read_exact` 块内每 syscall 续命）⇒ 滴流可把线程挂数小时；且登记了一个不存在的行为 | **认同并改码**：`reply_then_close`/`drain_one_frame` 两腿改走 `DeadlineIo`（逐 syscall 按剩余重设）；补滴流回归 `busy_path_dribble_released_by_absolute_window`；`INTEROP-CRITERIA` F6c 行订正为「逐 syscall 收敛的 `DeadlineIo`」 | 修前独立复算 **9.907s**（>8s 阈值 ⇒ 红）；修后实测 **3.75s** 通过 |

### 4.2 中危（5 条）

| # | 意见 | 处置 |
|---|---|---|
| **M1** | DNS TCP 腿读半边仍 per-syscall（注释还断言了不存在的"共用绝对期限"） | **认同并改码**：`exchange_tcp` 读写走 `DeadlineIo`（与拨号共用同一 deadline）；补 `tcp_leg_read_bounded_under_dribble`（修前 5.02s ⇒ 现 0.30s）；登记条目 |
| **M2** | F9 轮级序在 fail-open 下反向：`prefer` 续不上 ⇒ 外口漂移；clean 仍删其余 Ours（真空窗）——「先加后删」只覆盖 `prefer` | **部分认同（不改 D3 门，改登记 + 文案）**：`allow_evict = verify && Ours` 是**用户已定事项**（设计 D3），不在本棒权限内推翻 ⇒ ①fail-open 归因行文案改为「申请时不删既有映射（让位优先）」；②`§4.3.7①` 口径订正为「**只保证 `prefer`**」，并显式登记「fail-open 轮里 `prefer` 续不上会换外部端口（漂移）」与「其余 Ours 仍会被 clean 删除后再加回（一轮真空窗）」两条后果。**→ 见 §6 需上报项（设计侧二选一）** |
| **M3** | F10 的"硬界"不成立：UPnP 期限不覆盖 DNS 解析；SSDP 腿可越过 deadline ≈1.2s+0.5s | **认同并改码**：`connect_within` 加**字面 IP 快路径**（生产 IGD LOCATION 基本是字面 IP）；SSDP 每轮 recv 超时与 sleep 按剩余夹取；登记口径改为「8s + 1 个 recv/sleep 上界」并显式登记**域名解析残余**（`to_socket_addrs` 无可取消面）；用例阈值抬到 ≥2× |
| **M4** | `candidates_deduped` 用例空转（表为空 ⇒ prefer=0 ⇒ 修前也 1 次） | **认同并改码**：构造 `entries=[ours(42641)] + no_tail + refuse_ext=42641`（prefer == internal_port 且被拒）；去重前实测 2 次 ⇒ 必红 |
| **M5** | `http_call` 收满声明长度仍要等 EOF/期限（Go 按 Content-Length 收满即返） | **认同并改码**：声明长度收满即 `break`（长度一致性校验仍在循环后兜住 `got > declared`）；补 keep-alive 回归 `http_call_returns_when_declared_length_satisfied`（0.39ms 返回）；登记条目 |

### 4.3 低危（19 条）处置摘要

**已改码**：`Limits` 溢出夹取（`LIMITS_MAX_TIMEOUT`，防 `Instant + Duration` panic）；
files 请求行 IO 错误码对齐 Go `errorResponse`（`op_failed`，超限仍 `invalid_arg`）；
DNS worker spawn 失败改记行 + 按已起数继续（不再 `expect` 打崩）；`ShrinkOutcome` 增 `TableIncomplete`
（枚举残缺 ≠ 没有映射）且 `Failed{err: UpnpError}` 类型化；UPnP 超限文案改「头段或正文」；`RESOLVE_MAX_DEPTH` 8；
engine 服务线程 spawn 失败记行（不再是静默 `.ok()`）；公网端点 40s 单 ctx 贯穿映射 + 外网 IP 查询；
F4a poison 用例换 question 名（消 1/65536 假红）；F4c 用例兜底指 fake（防真实外联/假绿）；
`DEFAULT_WORKERS` 缺省断言 + Go 行号订正（`server.go:460`）+ `udp_rx_capacity_matches_inflight` 落地；
UDP 元数据槽说明订正；无 `Content-Length` 超大正文 / 头段灌爆两案补测；`pick_igd_before` 降 `pub(crate)`；
`close_all` 竞态残余改注释如实说明。

**登记不改（有据）**：`ECONNRESET → Fatal`（macOS errno 54；UDS 域基本不可达，评审亦判「记录即可」）；
`is_conflict` 串嗅探（SOAP Fault 只有文本体，与既有 `>713<` 表尾判定同一纪律）；
`read_line` 每 chunk 一次小分配（非热路径 nit）；`n > MAX_BLOCK` 死检查（u16 长度场，已注明）；
`dnsproxy` 循环前的初值 `set_read_timeout`（保留为发送腿兜底，已注明）。

**记录项**：busy 滴流用例阈值 6s→**8s**（最坏 5s ⇒ ≥1.6×；修前 9.9s 仍必红）；
M5 keep-alive 用例已补；`files_server` 模块头/测试注释的过时文字（canonicalize / 深度 40）已订正；
上传水位门使上传类用例依赖宿主剩余空间 >1GiB（本机通过；近满盘 runner 会连带变红——如实记录）；
`discover_igd_deadline_penetrates_ssdp` 的 `assert!(r.is_err())` 断言环境属性（本机 SSDP 被 macOS
本地网络隐私拒，与现役出口 launchd 形态同款已知限制；真有 IGD 且组播可用时会偶发红——保留并记录）。

### 4.4 复审（r11）结论

> 轮次目录 **`/tmp/dsh-review/r11.TXvvs6`**（`exit=0`；`output.md` 132 行，**已用 Read 全文读完**）。
> 复审者用仓外探针**独立复算**了四条最关键的"修前必红"（H1 根外零新增 + 正例回归、H2 修前 9.91s vs 阈值 6s、
> M1 修前 5.02s vs 阈值 1s、M5 keep-alive 0.39ms 且长度不符仍被拒），并逐点核对 `DeadlineIo` 覆盖面无绕过点。

**结论原文**：「**本次处置足以过代码门。** 六条被点名的项（H1/H2/M1/M3/M4/M5）逐条独立复核后全部成立……
本轮新增/改写的代码里，我没有找到新的高危或中危缺陷。」三项收尾（均已办）：①本记录（含两处实现订正）；
②M2 登记口径订正（见 §4.2/§6）；③低项随批登记/顺手补。

**代码门判定：通过**（r10 两条高危已改 + 修前红证据；r11 复审无新高危/中危）。

---

## 5. 判据行 / 观测面影响速览

- 编号判据行：**行文零改动**（E13/E14/E17/E22/E4；`git diff -U0` 逐串核过）。
- 数值语义：E22 `fail`/`fallback` 输入集、`udpDrop` 容量、E13 会话时长上界、UPnP 枚举次数（详见 §3）。
- additive 观测行：accept 退避/退工、服务线程起不来、水位 fail-open/中止、UPnP 让位与缩租五态、`http_call`
  三条新错误串（**非编号判据行**）。

---

## 6. 不做项、范围边界与需上报项

### 6.1 不做项（设计 §6 同口径，逐项核对未越界）

| 项 | 归属 | 本批状态 |
|---|---|---|
| `dnsface.rs` 每调用 64KB 零初始化（`DnsFaces::service` 的 `let mut buf = [0u8; UDP_RX]`） | **Q-I 尾段**（QI.md §6 登记的「下一批第一靶点」） | **未做**（本批只改 `attach` 的容量常量；`service` 缓冲复用留给尾段——**明确交接**） |
| DNS TTL 缓存 / `Upstreams::list` 整表 clone | Q-I 尾段 | 未做（grep 未命中新缓存） |
| files 收发拷贝与分配（`receive_upload`/`read`/`download` 的 Vec 分配） | Q-I 尾段 | 未做（本批只加水位门与上限） |
| UPnP 协议面（IGD:2 / `AddAnyPortMapping` / ST 兜底 / 钉卡降级）、`if_nametoindex` 完整语义 | **Q-J** | 未做（唯一 `urn:…WANIPConnection:1` 是既有 mock 夹具） |
| `openat2`/`O_NOFOLLOW` 逐段打开（沙箱 TOCTOU） | 不做（D5） | 未做；残余**如实登记为"未收口"**（无"已消除"措辞） |
| accept Fatal 退出时 unlink socket 文件 | 不做（D6/E3） | 未做 |
| 上传每文件硬上限 | 不做（D1） | 未做（水位优先） |
| 沙箱 `entry.name` 语义与 `write` 穿透语义 | 不做（S3 登记） | 维持既有 |
| 每查询新 socket（防投毒属性） | 不做（非缺陷） | 维持既有 |

### 6.2 本批残余（如实登记）

1. 水位门是**检查点采样**（起始 + 每 8 MiB）+ `statvfs` 失败 fail-open ⇒ 极端形态退化为无水位；
   并发两条上传可越保留量 ≤8 MiB 窗口。
2. DNS 全死上游下稳态吞吐 ≈25.6 qps（worker=64 vs Go 并发 256）——接受的差异。
3. **smoltcp rx 溢出 = 静默丢弃且无观测面**（本批靠容量对齐降低触发概率；不新增计数）。
4. UPnP 静默覆盖型路由器下 `allow_evict=false` 的"先加"仍会顶掉别人的映射（改进仅对回 718 的机型成立）。
5. SSDP/UPnP 三项加固的真机形态**本地不可验证**（本仓既定限制）⇒ 真机验收若发现误杀按登记条目回退
   （尤其：公开地址 LAN 会漏配）。
6. `files` 客户端响应行无上限（D4）；沙箱 TOCTOU 未收口（D5）。
7. 与邻批的同文件摩擦（B1）：`dnsface::attach` 与 Q-I 尾段的 `DnsFaces::service` 是同文件相邻函数；
   `F3a/F3b` 改的 `receive_upload`/`read_line` 正是 Q-I 尾段"files 拷贝与分配"要碰的代码——顺序成本
   （Q-E 在前），非返工。
8. **M2 两条后果**（代码门 r10/r11，登记已订正）：fail-open 轮里 `prefer` 端口回 718 时**不删既有映射
   ⇒ 换外部端口（漂移）**；`clean_mappings` 只跳过 `prefer`，若"在用映射 ≠ prefer"则其余 Ours（含在用
   那条）仍会被删后再加回（一轮真空窗）。
9. UPnP 域名解析（`to_socket_addrs`）无可取消面（字面 IP 已走快路径；生产 LOCATION 基本是字面 IP）。
10. `discover_igd_deadline_penetrates_ssdp` 断言依赖"本机 SSDP 不可用"这一环境属性（记录）。

### 6.3 需上报项（主会话裁决，**不阻塞本批**）

- **M2 二选一（设计侧）**：维持 D3 门（现状，登记已如实说明漂移/真空窗），或按代码门建议
  (a)「门收窄为『快照该 ext 条目正向为 Ours』」/(b)「申请成功后再 best-effort 清非在用 Ours」。
  本棒按「不自行推翻用户已定事项（D3）」处置 = 改登记 + 改文案；如裁决改门，另起小批或并入 Q-J。
