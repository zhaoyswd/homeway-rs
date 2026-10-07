# Q-D 终端子系统加固——实现与代码门记录

> 批次：Q 批整改（`docs/REVIEW-ROADMAP.md` §Q-D）。第 2 棒（实现）产出。
> 设计真源 = `docs/reviews/QD-design.md`（v2，设计门 dsh `r7.loQFwv` 已过，§5 记录）；
> 本文件 = 实现销账 + 代码门（dsh `r8`）+ 判据登记索引 + 残余与「不做项」。
> 基线：`git HEAD = bb2f357`（2026-10-08）；本批只动 `crates/homeway-core/src/term/**`、
> `crates/homeway-core/tests/fuzz_replay.rs`、`fuzz/**`、`tools/gen-fuzz-seeds.sh`、
> `tools/ci-local.sh`、`docs/**`（两台生产出口、tier/homeway 只读仓、`baseline/`、pin 全程不碰）。

---

## 1. 交付摘要（F1a/F1b/F1c、F2–F11）

| F | 落地（文件 / 函数） | 关键点 |
|---|---|---|
| **F1a** | 新增 `term/size.rs`（`Size::normalized`/`from_report`/`is_exact`/`MAX_COLS=1000`/`MAX_ROWS=500`/`DEFAULT=80×24`）；`session.rs`（`LegDescriptor.size`/`LegState.size`/`Session.size`/`LegView.size`/`size_applied: Option<Size>`/`leg_resize`/`note_activity`）；`service.rs`（HELLO 入径 `Size::normalized`、`apply_size_locked(rt, size: Size)`、夹取日志）；`vt.rs`（`new`/`resize` 硬拒 `is_exact`）；`ring.rs`（`Epoch.size` + `note_size(size)`） | 尺寸入径归一（0→缺省、>上限→夹取）+ 组件层硬拒 + 类型门三层；`RESIZE/HELLO 65535×65535` 不再发起 ≈96–192 GiB 分配（分配失败 = abort） |
| **F1b** | `service.rs::stream_read_loop` 的 `Op::RESIZE` 分支：`Size::from_report` 为 `None` ⇒ 忽略本次上报（不改腿/不改会话/不 apply），`zero_resize_ignored` 计数 + 节流日志 | 对齐 Go：会话尺寸写点在 0 门之后；修复前 `RESIZE 0×0` 把会话几何污染成 0×0（LIST/ATTACHED 全 0 而格流按 vt 宽编） |
| **F1c** | `service.rs`：`MIRROR_BYTES_BUDGET = 32<<20`、`mirror_rows_budget(cols)`、`mirror_window_rows(cols,rows)`；`flush_surface` 快照材质改用之 | 镜像窗口行数 `rows×10 → min(rows×10, 32MiB/(cols×48))`（1000 列 ⇒ 699 行）；窄屏/golden 形态不变 |
| **F2** | `vt.rs`：`rows()` 去掉指纹提交（保留 `let _ = self.term.damage()` 隐含 Update），新增 `rows_and_commit()`；`dirty_rows()` 全量分支与 `flush_surface` 快照材质改为提交点；`screen_text()` 变纯读 | 不变量：`flushed[y]` 只在第 y 行进入一次下发载荷时推进；1 Hz 采样不再吞差分（证据 B 转正为回归） |
| **F3** | `service.rs`：`focus_nudge_bytes` 纯判据 + `nudge_bytes_for`（三态）；nudge 入每会话 PTY 注入队列（`PtyInject::{Resp,Nudge}`，`resp_tx` 由 `response_writer_loop` 锁外消费）；`end_leg`（focus-out）与 `run_raw_writer`（focus-in）锁内 `try_send` | 队列序 = 状态迁移序；锁内无 PTY 写；写失败行文不变（`focus nudge 写入失败`） |
| **F4** | `legout.rs::stalled_snapshot()`（单次持锁）；`session.rs::register_leg(..., stalled: &[(LegKey, Duration)])` + `evict_victim(s, stalled)`；删除 `LegState.stalled_ms`/`set_stalled`（死面）；`service.rs` 锁内收表 | 淘汰排序真源 = 实时 `LegOut`（Go `evictForSlotLocked` 同序）；消 TOCTOU 与死字段 |
| **F5** | `codec.rs`：`marker` 改 `pub(crate)` + `symbol_prefix()`（借用、UTF-8 边界）；`append_cell` 快路径 + `append_cell_truncated` 副门（`debug_assert!` 可闻）；`vt.rs::cell_of` 主截断 | 单格 symbol ≤127 B（真源 `SYM_LEN_MASK`）；极端字素簇从「错位帧」→「边界截断」 |
| **F6** | `service.rs`：`clip_dropped`/`nudge_dropped` 计数器；`try_send_or_count` + `count_drop`（首 3 次 + 每 100 次节流；消费者退出独立归因）；`pump_loop` 剪贴板写与查询应答接线 | 三计数 + 节流日志；`resp_dropped` 从「只写不读」变为可观测 |
| **F7** | `service.rs`：`ThreadRole`/`PanicAction`/`panic_action`（纯函数）/`guard_thread`/`panic_payload_text`/`panic_inject`（测试注入，服务唯一键）；七处线程包装（conn×2/pump/leg-writer/resp/surface/sample 按会话 + 整拍）；`finalize_panicked_pump`/`surface_panicked`；`legout.rs::lock()` 与 `PtyShared` 7 处毒锁恢复统一 | panic 不静默：日志 `term: {ctx}{role} 线程 panic（已兜住）：{载荷}`；处置表见设计 §2 F7②（+ 代码门 A2 的整拍门） |
| **F8** | `vt.rs`：`plain_text` 备用屏取活动屏；`line_wraps` 抽出（`read_row`/`abs_rows` 共用，回滚行按 WRAPLINE）；`plain_text` **流式**重写（代码门 A1） | 备用屏从「恒空串」→ 实际内容；回滚折行段合并为单条逻辑行；主屏口径与 golden 不变 |
| **F9** | `vt.rs::dirty_rows`：单次 `damage()` + `match`（`force_full` 短路在前；未知形态安全回落 = 全量 + 提交），去掉 `unreachable!` | 语义等义 + 少一次副作用调用 |
| **F10** | `codec.rs`：`MAX_GUNZIP_OUT = 64<<20` + `take(上限+1)` + 超限 `Err(BadGrid("解压超上限"))`；`guard_dims`（`decode_grid`/`decode_rows` 入口）；`service.rs` 重复的 `MIRROR_VIEWPORTS` 已删（统一 `codec::MIRROR_VIEWPORTS`） | 伪造头/炸弹返回 **Err 形状**；合法最大镜像帧不被误伤 |
| **F11** | `fuzz/fuzz_targets/fuzz_term_{frames,vt,codec}.rs` + `fuzz/Cargo.toml` 三个 `[[bin]]`；`tests/fuzz_replay.rs` 三个 `#[ignore]` 回归目标（⑫⑬⑭）+ `term_seeds()`；`tools/gen-fuzz-seeds.sh` term 区；`fuzz/corpus.seeds.sha256`；`ci-local.sh`/`fuzz_replay.rs` 文案「9 目标 → 12」+ 预算 | 两轨 oracle：不 panic；`fuzz_term_vt` 断言每格 `symbol.len() ≤ 127`（F5 运行时哨兵）；codec 目标 decode→再 encode **字节相等**；harness 维度预检按目标分别从输入派生（`-max_len=8192`，注释说明与 files 目标 262144 的差异） |

**代码门后的增补（设计外，见 §4）**：A1（`plain_text` 流式化，消 250 MB 级材质与千万次分配）、
A2（`sample_loop` 整拍 `guard_thread`）、M3（`Disconnected` 独立计数/归因）、L3（`PtyInject::Nudge`
改 `&'static [u8]` 零分配）、L7/L1/M1/M2/H1 注释与口径订正。

---

## 2. 测试与门禁证据

| 门 | 命令 | 结果 |
|---|---|---|
| 单测/集成 | `cargo test --workspace` | **548 passed / 0 failed**（lib 513 + capi 4 + cli 14 + 其它测试二进制 17；`fuzz_replay` 13 ignored 归全量档）。**首轮曾红 1 条已知 flake `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect`**（隔离复跑 2/6 绿；`git stash` 后在 HEAD 复现 **3/8 红** ⇒ 既有 flake，与本批无关；失败点 = 200ms 读窗内 welcome 与 goodbye 未同读到，`daemon/tests.rs:694`） |
| 终端面基线 | `cargo test -p homeway-core --lib term::` | **142 passed / 0 failed**（设计基线 111 → **+31**；含设计点名的 29 条 + A1/A2 两条增补） |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | **clean**（首轮命中 `manual_is_multiple_of` 一处，已改 `is_multiple_of`） |
| OHOS 交叉 | `cargo check --target aarch64-unknown-linux-ohos -p homeway-core -p homeway-cli -p homeway-capi` | 通过（唯一 warning = `go_fmt.rs:69` 既有 `libc::time_t` deprecated，非本批） |
| fuzz 回归轨 | `cargo test -p homeway-core --test fuzz_replay -- --ignored --test-threads=1` | **13 passed**（12 目标 ×100k + 夹具期望；term 三目标 ≈41 s，`ci-local.sh` 注释给的 +2-4 min 预算内） |
| fuzz 深挖轨 | `cargo +nightly fuzz build` + 三个 term 目标实跑 | 12 目标全部构建通过；`fuzz_term_frames` 76.9 万 runs / `fuzz_term_vt` 23.6 万 / `fuzz_term_codec` 33.8 万，**无 crash**（代码门后修订版重跑 vt 23.6 万 + codec 33.8 万） |
| fuzz 种子对账 | `zsh tools/gen-fuzz-seeds.sh` 摘要 vs `fuzz/corpus.seeds.sha256` | **一致**：`07f577d8e2106eed6d3b908159cfa09b92502e28a068fadd9c3cef46eac02bd8`（seeds=112；`ci-local.sh` 第 4.5 步消费） |
| 判据行 | `docs/INTEROP-CRITERIA.md` | 登记表 2 行 + 数值语义表 2 行 + 已知口径注记 3 条（§3） |

---

## 3. 判据行与观测面登记（`docs/INTEROP-CRITERIA.md`，同批 commit）

**判据变更登记表（2 行）**：

1. **E16a/E16b 尺寸字段**（含 LIST JSON / ATTACHED 的 `cols`/`rows`）：任意 u16 尺寸原样接受
   （>上限 ⇒ ≈96–192 GiB 分配 abort）→ **>1000×500 夹取到上限**；`RESIZE 0×0` **忽略本次上报**
   （几何保持）。原因 = P0-3 实测 + F1b 的 Rust 独有移植偏差。影响面点名：`docs/INTEROP-CRITERIA.md`
   E16a/E16b、`service.rs`、`term/size.rs`、四个单测；**正常尺寸逐字节不变**。
2. **surface cell 流 symLen 域**：>127 B 字素簇「错位字节流」→「UTF-8 边界截断」；真源
   `codec::marker::SYM_LEN_MASK`；**无夹具变更**（fixtures/vectors 无该形态）。

**计数输入集 / 数值语义变化（行文不变，2 行）**：

1. **快照镜像窗口行数**：恒 `rows×10` → `min(rows×10, 32MiB/(cols×48))`（宽屏变小，1000 列 ⇒ 699 行；
   客户端 FETCH-ROWS 兜底）；窄屏与既有 golden 形态不变。
2. **新增观测行（additive）**：`查询应答丢弃` / `剪贴板写丢弃` / `PTY 注入丢弃`（首 3 次 + 每 100 次；
   消费者已退出时尾缀 `（消费者已退出）`）/ `{ctx}{role} 线程 panic（已兜住）` /
   `尺寸夹取 {cols}x{rows} → {c}x{r}（上限 1000x500）`（**只在夹取结果真变时打**）/
   `忽略 RESIZE 0×0 上报 {k} 次`；腿断开归因新增 `原因=panicked`。

**已知口径注记（3 条）**：F2 差分下发行集合的竞态窗口（以前被静默丢掉的行现在会补发；快照提交后
增量不重复带）；F8 的 EXPLAIN 文本面（备用屏从空 → 实际内容）与回滚折行合并；F1c 的宽屏镜像行数
指向数值语义表。

---

## 4. 代码门记录（dsh `r8`）

**轮次目录**：`/tmp/dsh-review/r8.kxs9ia/`（`prompt.txt` / `output.md` / `stderr.log`）。
**退出码如实标注**：本轮以 `nohup … &` 后台方式启动，**只捕获到 wrapper 的 `exit=0`**（dsh 自身
退出码未落盘）——判据按「output.md 完整 + 进程正常退出」认定成功；固定姿势（前台 `; echo "exit=$?"`）
在后续轮次恢复。**另注**：本次 dsh 会话自身按 reviewer skill 又起了一轮**内嵌外部评审**
（`/tmp/dsh-review/r1.eZPXd2/`，`output.md` 30 KB 已全文读入）——两层意见均收入下表。

### 4.1 意见摘要（r1.eZPXd2 原文要点 + r8 外层补充）

| # | 严重度 | 意见 |
|---|---|---|
| H1 | 高（外层复核后降低） | `sample_once` 全程持服务锁，而注入点在拿锁前 ⇒ 锁内 panic 路径未测；「可接受的不一致面」注释对 `hygiene`/`state_v2` 半更新不成立（可能永久跳过屏扫） |
| M1 | 中 | `SessionVt::new` 失败的 legacy-only 降级分支归一后不可达（设计论证的风险在实现里没被触发过） |
| M2 | 中 | `mirror_rows_budget` 注释/表达式/下限三处口径不一；「32 MiB 字节预算」把行数预算包装成字节预算；快照一拍峰值 ≈3× 材质未注 |
| M3 | 中（机制被外层纠正） | `resp_tx` 消费者退出后 `try_send` 落 `Disconnected` 分支**静默**（不计丢弃、无日志）⇒ F6 观测面盲区 |
| M4 | 中 | 被夹取的 raw 会话「每次窗口事件重放夹取+哨兵」（设计未点名） |
| L1–L7 | 低 | 注释精度（L1）/ createOnly 0×0→80×24 的登记字段位（L2）/ `Nudge(Vec)` 无谓分配（L3）/ `resp_dropped` 命名（L4）/ 夹取日志「结果未变不报」未登记（L5）/ `decode_rows` 守卫顺序（L6，看过没问题）/ `row_fingerprint` 格式手误（L7） |
| A1 | 中（r8 补充） | `plain_text()`（EXPLAIN）在**全局服务锁内**物化整条回滚：上限处 ≈250 MB 材质 + 千万次堆分配（分配失败 = abort 类） |
| A2 | 中（r8 补充） | `sample_loop` 线程体没套 `guard_thread`（只按会话 catch）⇒ `read_procs`/记账 panic 会让**服务级**检测线程停摆，与设计给 sample 的理由冲突 |
| A3 | 低（r8 补充） | F1c 的 48 B/格估算对病态内容（127 B 字素簇）偏低（合并进 M2 注释） |
| A4 | 低（r8 补充） | `docs/reviews/QD.md` 不存在（本文件）+ R6 文档三处口径销账别漏 |

**六方面结论（评审者原文要点）**：① panic/abort：能兜住（无 `panic="abort"`、处理器只 downcast+logf、
7 个线程池全覆盖、leg-writer 补偿必要非重复）；毒锁恢复「锁安全、状态不安全」（→ H1）；尺寸双门
「不发起分配」是本批最关键一处，正确；② 并发：F2 修好（核过 alacritty `damage()` 不清 Partial 集）、
锁内已无 PTY 写、锁序 state→legout 无环、淘汰快照语义等价 Go、nudge 顺序有真 PTY 断言；
③ 边界七项全部没问题；④ Go 对齐：wire 仅两处登记例外，登记政策合规（除 L2/L5）；⑤ Go 直译痕迹
总体干净（残留三处：L3、`SessionVt` 的 String 错误（既有）、L4）；⑥ F1 夹取真实形态不误伤，
但**纵向余量比横向小**（4K 竖屏 @4px 行高 540 行 > 500）建议两轴分开点名。

### 4.2 逐条处置表

| # | 处置 | 落点 |
|---|---|---|
| H1 | **部分认同（降为低）**：注释口径写实 + 残余细化**采纳**；「加自愈动作」**不采纳**——机制复核：`should_skip_screen_scan` 的短路含 `cur_seq == last_scan_seq`，而 `content_seq` 随每批 PTY 输出自增、`pending_idle` 按墙钟过期、`state_v2/last_scan_seq/prev_cpu/prev_quiet` 每拍全量重算 ⇒ 最坏 = **一拍脏数据**（随下批输出/下一拍自愈），非永久降级 | `service.rs::lock_state` 注释（写实口径 + 自愈链）、`legout.rs::lock` 注释；§6 残余行 |
| M1 | **认同**：`spawn_session_locked` 的 vt 降级分支加「归一后不可达，仅防御未来直连调用点」注释（组件层硬拒由 `vt_size_gate_rejects_oversize_and_zero` 覆盖） | `service.rs:1619-1626` 注释；§6 残余 |
| M2 | **认同（三条全采）**：注释与表达式对齐（`min` 在调用点）、`.max(64)` 注明「纯防御（仅 cols>10922 触发，生产被 MAX_COLS 封住）」、措辞改「行数预算（48 B/格保守折算）」+ 峰值 ≈3× 材质注记 | `service.rs` `MIRROR_BYTES_BUDGET`/`mirror_rows_budget`/`mirror_window_rows` 注释 |
| M3 | **认同（外层已纠正机制：Disconnected 是即时返回，非"前 16 条 Full"）**：`try_send_or_count` 的 `Disconnected` 分支独立归因（`（消费者已退出）`）+ 单测 + 登记行更新 | `service.rs`；`drop_counters_and_throttled_log`；INTEROP additive 行 |
| M4 | **不认同（误报）**：`leg_resize` 写入的是**夹取后**的 `Size`，本地尺寸变化但夹取结果相同时 `note_activity_internal` 返回 `None` ⇒ **不 apply、不哨兵**；只有夹取结果真变才走一次（那本就该走）。「raw CLI 静默」残余已登记（§6，含两轴口径） | §6 残余行（两轴分开点名） |
| L1 | **认同**：`vt.rs::resize` 措辞改「`flushed` 指纹表整体重置」；镜像预算注释同 M2 | `vt.rs`、`service.rs` |
| L2 | **不认同（误报）**：HEAD 的 service 路径 `create_only` 走 `spawn_session_locked(0,0)` + `registry.attach_or_create(name, 80, 24, …)` ⇒ 旧代码的 `Session.cols/rows` **本来就是 80×24**；`SessionRegistry::create_only` 的建会话分支在 service 侧不可达（两调用点都早退）。LIST JSON 无 0×0→80×24 变化，登记无遗漏 | 本表；`session.rs` 注释 |
| L3 | **认同**：`PtyInject::Nudge(&'static [u8])`（消费者直接写借用，零分配） | `service.rs` `PtyInject`/`response_writer_loop`/`enqueue_pty_inject` |
| L4 | **不认同**：`resp_dropped` 现在只计 `PtyInject::Resp`（nudge 独立 `nudge_dropped`），名字与语义仍匹配；设计 §2 F3 风险③担心的「计语义扩展」被拆分计数器解决 | 本表 |
| L5 | **认同**：登记行补「夹取日志只在结果真变时打」口径 | INTEROP additive 行 |
| L6 | **记录**：看过没问题（`guard_dims` 在 `with_capacity` 之前；生产零调用，放大面仅测试/fuzz，守卫已钉上界） | 本表 |
| L7 | **认同**：`row_fingerprint` 换行修正 | `vt.rs` |
| A1 | **认同并实现（本批增补）**：`plain_text` 改**流式**逐格写（`push_cell_text`，含 F5 截断口径），不再物化 `Vec<Row>`——上限处消掉 ≈250 MB 材质 + 千万次 `String` 分配；输出与「`rows_at` + 合并」参考实现**逐字节等价**（新单测 `vt_plain_text_streaming_matches_reference` 三形态钉住）。**未做**：把构建挪出服务锁（需 vt 结构改造）——登记残余 | `vt.rs::plain_text`/`push_cell_text` + 单测；§6 残余 |
| A2 | **认同并实现（本批增补）**：`sample_loop` 每拍整体 `guard_thread`（拆 `sample_tick`），`read_procs`/记账 panic 不再停摆服务级线程；tick 级注入键按服务唯一化（`svc_id`，并发测试不互相消费）+ 新单测 `sample_tick_panic_keeps_loop_alive` | `service.rs` `sample_loop`/`sample_tick`/`tick_inject_key`；单测 |
| A3 | **认同（注释）**：48 B/格只对常态内容保守；病态内容材质可达 ~127 B/格，兜底 = 发送侧 `pending_cap`（不会 abort） | `service.rs` 注释（同 M2） |
| A4 | **认同**：本文件（QD.md）+ §6 的 R6 三处销账 | 本文件 §6 |

**门禁判定**：**无高危必改项**（H1 经机制复核降为低，按「改注释 + 登记」处置）；M3/M2/L3/L7 与
A1/A2 已改码并重跑门禁；M1/M4/L2/L4/L5/L6 处置如上。**未发现需要回退的条目**；§4.2 的 12 条
证伪条件评审逐条核过全部未触发。

### 4.3 改码后的复跑证据

`cargo test -p homeway-core --lib term::` = **142 passed**；`cargo test --workspace` = **513 passed
（lib）/ 0 failed**；`cargo clippy --workspace --all-targets -- -D warnings` clean；
`fuzz_replay --ignored --test-threads=1` = 13 passed；`cargo fuzz run fuzz_term_vt` 23.6 万 runs、
`fuzz_term_codec` 33.8 万 runs 无 crash；种子摘要仍逐字一致（未动 fixtures/脚本语义）。

---

## 5. 与设计的偏差（如实登记）

1. **F1b 的「不改腿」比 Go 多收一层**：设计明写 `from_report(None) ⇒ 不改腿、不改会话、不 apply`。
   Go 对 `RESIZE 0×0` 仍会走 `noteActivityLocked`（活动序/active 切换更新，只是几何不写）。
   Rust 在本批**整条上报忽略**（活动序也不动）——差异面 = 0×0 上报不参与活动选举（极端形态，
   不影响几何/判据行）。属设计选择，如实记录。
2. **`PtyInject::Nudge` 写失败会让 `response_writer_loop` 退出**（与旧实现「只打日志继续」不同）：
   PTY 写失败意味着会话 PTY 已坏，消费者的后续写入同样会失败——退出即降级（与 Resp 路径同处置），
   行文不变。如实记录。
3. **`SessionRegistry::create_only` 的初始几何 0×0 → `Size::DEFAULT`**（类型不可表达 0×0）：
   service 侧不可达分支，语义与 Go `spawnLocked(0,0)` 的落值（80×24）一致（L2 复核）。
4. **代码门增补 A1/A2/M3/L3**：设计外但同面（`plain_text`/sample 线程/观测面/队列类型），已测试 +
   已登记；A1 未做的一半（出锁）进残余。

---

## 6. 残余与「不做项」登记（防静默漏做）

### 6.1 设计 §7「不做项」（全部照做，状态）

| 项 | 结论 |
|---|---|
| `title_stale` 时效语义 | **不做**（Go 同构 + tier 规格明文；TTL = 反规格） |
| `screen_evidence_locked` 早退清 `last_scan_proc` | **剔除（误报）**（四道门/写点与 Go 逐行同构） |
| surface 腿停滞置位 | **不做**（Go 只给 raw；surface 超时即断腿） |
| `is_descendant_of` 每调用建表 | **不做（登记）**（Go 同款，量级不成比例） |
| `read_procs` 空白归一 | **不做**（Go 同款） |
| `encode_frame`/`enc_hello` 静默截断 | **不做（登记残余）**（Go 同款；>255 B 会话名 ⇒ nameLen 模 256 回绕，两侧同病，协议批再议） |
| `manifest::Loader::reload` | **保留不接线**（Go 同款：入口 API = 规格要求） |
| 尺寸上限的 env 覆写 | **不做**（安全边界不由环境放大） |
| `vt.rs:1308` 备用屏内容面（`?47h/?1047h` 不可达） | **不在本批**（R6-gate1 §B6 已登记） |

### 6.2 本批残余（新增/更新）

| 项 | 说明 |
|---|---|
| **raw CLI 对夹取静默**（两轴分开点名） | raw CLI 不回读 ATTACHED 几何、SIGWINCH 继续报本机尺寸 ⇒ 被夹时出口日志可见、客户端无感（surface 客户端可自恢复）。**两轴余量不同**：横向 1000 列在可读字号下余量充足（8K ≈850–960 列）；**纵向 500 行余量更小**——4K 竖屏（2160 高）@4 px 行高 = 540 行会被夹（8 px 行高 270 行在限内）。代码门建议的「每次窗口事件重放夹取+哨兵」经复核**不成立**（M4 误报：夹取结果未变 ⇒ 不 apply、不哨兵） |
| **回滚内存面** | `MAX_COLS × SCROLLBACK_LINES × 24B ≈ 229 MiB/会话`（惰性填充、env 可调）；上限处 ×16 会话 ≈4 GB 最坏（需全灌满）；与 Go 同形，不在本批 |
| **不可读极小字号的全屏终端（>1000 列或 >500 行）被夹** | 无 env 逃逸口（D7 决策） |
| **`PtyShared` 写入超时** | `write_all` 无超时（P0 面已由 F3 移出锁外；超时需 nonblocking+fork 语义改造，风险大于收益） |
| **term 线程 panic 后的数据一致性** | F7 只保证「不静默僵尸 + 有日志 + 保守处置」。口径细化（代码门 H1）：采样锁内 panic 最坏 = **一拍脏数据**（`content_seq` 随输出自增 / `pending_idle` 按墙钟过期 / 检测字段每拍重算 ⇒ 自愈）；不可自愈的是单字段漂移类（例 `LegOut.qbytes`，处置保守：该腿持续「超限入队失败 → needSnapshot」） |
| **EXPLAIN 的 `plain_text` 仍在服务锁内构建** | A1 已消掉材质物化（≈250 MB + 千万次分配），但 ~10 MB 文本的构建仍持服务锁（上限处锁持有时间已大幅下降）；彻底出锁需 vt 结构改造，登记 |
| **`decode_rows` 守卫上界在测试/fuzz 面可达 ≈130–180 MB/次** | `guard_dims` 允许的 `5500 × 1000` 格上界；生产零调用（`decode_*` 族仅测试/fuzz 用），守卫已钉上界，接受 |
| **`SessionVt::new/resize` 返回 `String` 错误** | 既有（仓规第 1 条要求 thiserror）；本批未扩大新类型面，建议下批顺手收敛 |
| **`create_only` 的 registry 不可达分支几何 = `Size::DEFAULT`** | 类型不可表达 0×0；与 Go 落值一致（代码门 L2 复核），service 侧不可达 |

### 6.3 R6 文档三处口径销账（评审 4-R6；本批实现后与代码一致）

1. **`docs/reviews/R6-design.md` §9.7 的 `plain_text` 残余**：原登记未含「备用屏恒空」——F8 已修
   （备用屏取活动屏），残余清单相应作废（R6 文档不再改动，以本批实现为准）。
2. **`docs/reviews/R6-design.md:295-296` 的「停滞最久优先」登记**：原实现读会话侧 `stalled_ms`
   （生产从不写 ⇒ 恒走「最久空闲」兜底），F4 已接回实时 `LegOut` 快照 ⇒ 该登记描述与实现重新一致。
3. **`docs/reviews/R6.md:55` 的锁纪律例外清单**：F3 后「锁内不做阻塞 I/O」的例外清单不再含 PTY 写
   （nudge 改入队，写由 `response_writer_loop` 锁外做）；R6 文档保持只读，口径以本批为准。

---

## 7. 后续指针

- 下一棒（收口）：更新 `docs/REVIEW-ROADMAP.md` §Q-D 状态行 + `AUDIT-2026-10-07.md` 勾选（主会话核对）。
- 建议进下一批的项（非本批范围）：`SessionVt` 错误类型收敛（thiserror）、`plain_text` 出锁、
  `encode_frame`/`enc_hello` 长度域（协议批）、`vt.rs:1308` 备用屏内容面（Q-J/仿真等价批）。
