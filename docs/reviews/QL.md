# Q-L 批记录 — 账实对账 + 挂空小项（第 2 棒：实现 + 落账 + 收口）

> 2026-10-08。开工基线 HEAD = `5da59e8`（Q-K 收口），`git status --short` 空。
> **批定位 = 治理批**：把「说好下一批做、下一批没做也没说不做」的挂空项**逐条判死**
> （闭合 / 转新批 / 明确不做），把**传输耦合项**显式交接给 QUIC 程序（`docs/QUIC-ROADMAP.md`），
> 并就地做掉**传输无关**的小项（L1/L2/L3）。
> 设计真源 = `docs/reviews/QL-design.md`（第 1 棒；设计门 r30 已过）；本文件 = 第 2 棒交付记录。
>
> **边界遵守**：只动 `crates/homeway-core/src/term/**`（L1/L2/L5 注记）、`crates/homeway-cli/src/`
> 的取值纪律（L3）与文档台账；**未动** intercept/wgcore/files/dnsproxy/session/facade/relay；
> 未碰两台生产出口 / `homeway` / `tier` 两仓 / `baseline/` / `tools/tailcat/homeway-rs.pin`；
> 未发 tag / Release / PR。

---

## §1 对账落账（本批主价值）

### 1.1 派单项 A1–A8（判定分布：做 4 / 转 QUIC 1 / 部分闭合 1 / 不做 1 / 文档 1）

| # | 项 | 判定 | 落点 |
|---|---|---|---|
| A1 | 中继 `MAX_ASSOCS_TOTAL=1024` 字节预算化 | **转 QUIC M1**（Q1） | `QUIC-ROADMAP.md` M1 前置表 Q1；理由/量级辨析 = `QL-design.md` §1-A1 |
| A2 | `vt.rs` 备用屏内容面（`?47h/?1047h`） | **做（L2）** ✅ | 本文件 §2-L2；判据登记 = `INTEROP-CRITERIA` Q-L 行 |
| A3 | `SessionVt` 错误类型收敛 thiserror | **做（L1）** ✅ | 本文件 §2-L1 |
| A4 | `encode_frame`/`enc_hello` 长度域 | **不做（闭合）** | 判死注记入代码（`term/frames.rs` `enc_hello` nameLen 域注，L5 零行为加固） |
| A5 | `relay.listen` 接受集放宽（主机名） | **不做（登记）** | `QL-design.md` §1-A5；**不进交接清单**（中继零改动面） |
| A6 | `--bind-interface=` 空值：Go=auto / Rust 拒 | **做（L3）** ✅ | 本文件 §2-L3；判据登记 = `INTEROP-CRITERIA` Q-L 行 |
| A7 | GAP-AUDIT P1-8「公网端点细节分支族」 | **部分闭合 + 剩余转 QUIC M1** | 3 类已实现（Go 同串）+ 1 条审计误判剔除 + 3 条真缺转 M1（Q3/Q4/Q5）；`GAP-AUDIT.md` P1-8 行 |
| A8 | GAP-AUDIT 状态字段刷新 | **做（文档）** ✅ | `GAP-AUDIT.md`（P0-1/P1-4/P1-7/P1-8/P2-4/P2-5/P2-7 + K-15/K-20，追加式） |

### 1.2 新发现 N1–N20（判定分布：闭合 1 / 转 QUIC 5 / 不做登记 14）

| # | 项 | 判定 | 落点 / 备注 |
|---|---|---|---|
| N1 | `files --host` 本机实测失败（QIt §7.2-4） | **闭合（非缺陷）** | §3 L7 复验：默认形态全绿；失败仅 `--rate-limit 0`（文档化盲节流边界，Go 同形）⇒ 不进 M3 |
| N2 | 盘上遗留旧大写条目不迁移 | 不做（登记） | `QL-design.md` §2-N2 |
| N3 | 两套等号形入口并存 | 不做（登记） | 纯形态、行为一致 |
| N4 | `egress.rs` `interfaces()` 告警未进 `Logf` | 不做（登记） | Rust 自加诊断、不影响对齐 |
| N5 | 中继无 JSON 遥测通道 | 不做（登记） | **不进交接清单**（中继零改动） |
| N6 | 中继客户端方向 v6（主监听口 v4-only） | **转 QUIC M1（须显式立条）** | 交接表 Q2 |
| N7 | Q-B F3/F10 UDP 门/`udp_drop` 链 e2e 未做 | **转 QUIC M1** | 交接表 Q6 |
| N8 | 装配错位（`dns=Some` 而 `dns_rx=None`） | 不做（登记） | 生产装配恒成对（结构脆弱面） |
| N9 | Q-I 性能残余（三档） | **转 QUIC（分档）** | ①/③ ⇒ 交接表 Q7（M5 消失）/不做；② `reactor_turn` ⇒ Q8（**不随 M5 消失**） |
| N10 | `plain_text` 出锁 | 不做（登记） | 锁纪律回归风险 > 收益 |
| N11 | `leg_missing_input_drops` 无状态面字段 | 不做（登记） | 中继面、**不进交接清单** |
| N12 | harness 改进（loadavg/en0/RSS） | 不做（登记） | QUIC 程序自带 `tools/quic-ab.sh` 重做 harness |
| N13 | QG 互操作回归脚本小瑕 | 不做（登记） | 当次已手工等价完成回归 |
| N14 | `files` picker 自动化 / UPnP 真网关（K-10/K-5） | 维持 | 用户触点 / 环境不可测 |
| N15 | term/keyenc 热路径分配（唯一明写「归后续批」却无人认领） | 不做（登记） | 无实测驱动 + 热路径语义风险；列为「将来有 profile 靶点再开」候选 |
| N16 | `QFB.md` §6-3 第三份泵拷贝无落点 | 不做（登记） | 补登记（CLI 测试动词，D8 裁决） |
| N17 | `wgcore` 站点 Engine 级测试空档 | **转 QUIC M5（自然消失）** | 交接表 Q9 |
| N18 | 出口侧跨报文乱序无 E2E | 不做（登记） | 交接表 Q12（M1 可选，不立条） |
| N19 | `QC.md` §5 F1 进程级 E2E 未执行 | 不做（登记） | 同一不变量已由 engine 级单测覆盖 |
| N20 | `PERF-AB` §9.15.6 四条 | **转 QUIC M1/M5（分档）** | 交接表 Q10 |

**复核确认已闭合的挂空候选**（防二次挂空）与**分组式残余确认**（10 组）见 `QL-design.md`
§2.1/§2.2；本棒逐组复核未发现新增挂空（Q-H launchd 由 Q-J F6 闭合、Q-C F12 由 Q-J F5 闭合、
Q-B F7 由 Q-K 注销、Q-I 尾段第一靶点由 Q-I 尾段 F1 落地）。

**扫描口径（全称否定类判定的取证方式，代码门 低4 要求落档）**：

1. **挂空扫描面** = `docs/reviews/{QB,QFB,QC,QD,QE,QF,QG,QH,QI,QIt,QJ,QK}.md` 的「残余 / 挂账 /
   移交 / 后续小项 / 不做项」节（定位 = `grep -n "残余\|挂账\|移交\|后续\|不做" docs/reviews/*.md`
   后逐份通读）+ `docs/GAP-AUDIT.md`（P0/P1/P2 表 + K 表）+ `docs/REVIEW-ROADMAP.md` +
   `docs/PERF-AB.md`（§9.15.6 等挂账节）+ `AUDIT-2026-10-07.md`。
2. **「零落地 / 零命中」判定**（A1 的 Q-I/Q-J 零落地、N1/N3/N5/N6/N20 等）= 在该批记录全文搜
   关键标识（`MAX_ASSOCS_TOTAL`/`assoc`/`bind-interface`/`v6`/`sendmmsg`/`reactor_turn`/
   `rate-limit`/`files --host` 等）**零命中** + 回源码核交付物（例：`relay/mod.rs` 无字节预算
   改动；`QIt.md` §7.2-4 只有「建议单开小项」字样）。
3. **「均有接收方」判定**（`QL-design.md` §2.3）= 逐项在其出处文件找到接收方字样（批次名 /
   用户触点 / QUIC 程序），找不到者一律落本批判定表。
4. **已闭合判定**（§2.1 表）= 回源码行 / Go 基线行 / 判据登记行取证，不以批次记录的自述为准。

### 1.3 `GAP-AUDIT.md` 刷新明细（追加式；行内历史注记不改）

- 顶部新增 **2026-10-08 Q-L dated 块**（口径与逐条判定指针）；
- **P0-1**：「归属」格追加「**全清**」（余项 r1-N1/L7 由 Q-H F15 落地 + `INTEROP-CRITERIA`
  「N1/L7 默认 state」行 + 单测 `default_state_matches_unified`；Q-J/Q-K 零残留）；
- **P1-4**：追加「已清（Q-H F17）」（C14 实装 + 4 单测 + 判据登记）；
- **P1-7**：追加「已清」（动词别名 B0-2a；`--host` 远程模式 B0-2b/D-1）+ **QIt §7.2-4 挂点注销**
  （L7 复验 = harness 用法而非产品缺陷）；
- **P1-8**：「Rust 现状」格追加逐子项结论（3 类已实现含行号 + 1 条审计误判剔除 + 3 条真缺转
  QUIC M1 须显式立条）；「影响」/「归属」格同步；
- **P2-4**：已清（标题 OSC 2 `term_cli.rs:1430` + `TERM_SESSION_ID` 回环 `:942-944`）；
  **P2-5**：不适用/等价面（`status --watch` 已交付；`HOMEWAY_LIVE_*` 是 Go 注入缝）；
  **P2-7**：值域表 = Go `validateFile` 全量（Q-H F1）+ 已知残余（`peer_ttl` 窄 / 无行号 / 无 CAS）；
- **K 表**：K-15 状态注（47 备用屏已由 L2 实现，残余 = 4 条登记差异）；K-20 复核注
  （R8 后各批已收官，剩余 = 用户触点）。

---

## §2 B 类实现注记（L1/L2/L3 + L5）

### L1 `SessionVt` 错误类型收敛（thiserror）

- `crates/homeway-core/src/term/vt.rs`：新增 `pub enum VtError { BadSize { cols, rows } }`
  （`#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]`）；`SessionVt::new`/`resize`
  签名 `Result<_, String>` → `Result<_, VtError>`；两构造点改 `Err(VtError::BadSize { .. })`。
- **Display 逐字保文**：`#[error("vt: 尺寸非法 {cols}x{rows}（须为 1..={}×1..={}）", Size::MAX_COLS, Size::MAX_ROWS)]`
  ——与收敛前 `format!` 串**字节相同**（新单测 `vt_error_display_verbatim` 钉 `"vt: 尺寸非法 0x0（须为 1..=1000×1..=500）"` + new/resize 两构造点同变体断言）。
- 调用面：生产 2 处无感（`service.rs` `Err(e)` 走 `{e}` Display；`let _ = vt.resize(...)`）；
  测试面全为 `unwrap/expect`（唯一 `err.contains` 已改 `err.to_string().contains`）。

### L2 `?47h/?1047h` 备用屏内容面

- `vt.rs` `set_private_mode`/`unset_private_mode` 的 `PrivateMode::Unknown` 分支：
  - 进入（`47|1047`，guard `!ALT_SCREEN`）：捕获主屏 `saved_cursor` → 公开 `Term::swap_alt()`
    （alt 光标 ← 主屏光标，含样式；alt 内容由 alacritty 进入复位）；
  - 退出（`47|1047`，guard `ALT_SCREEN`）：克隆 alt 整光标 → `swap_alt()` → 写回主屏光标 →
    `restore_alt47_saved_decsc()`（还原主屏 DECSC 槽，**先按当前尺寸钳制**）。
  - 1049 退出收尾同走 `restore_alt47_saved_decsc()`（混合形态：曾以 47h 进入、未走 47l 就经
    1049 退出时还原；正常 1049 会话捕获为 None ⇒ 零影响）。
- **陈旧注释改写**：原「alacritty 无公开 swap-alt API」删除，换为 ghostty 语义真源引用
  （`Terminal.zig:4795-4870`）与 alacritty API 行号（`swap_alt` `term/mod.rs:714`）。
- `dec_modes` 记账、`scan.rs:159` 的 ALT_SCREEN 位、wire Alt 位（`vt.modes().screen`）**未动**
  ——修前「位对、屏内容错」的分裂转为一致。
- **单测 8 组**：`vt_alt47_switch_and_content`（切屏 + 内容保留）、`vt_alt47_cursor_copy_both_ways`
  （双向整光标含样式）、`vt_alt47_idempotent_guards`（重复 47h/47l 守卫）、
  `vt_alt47_preserves_primary_decsc_slot`（DECSC 槽钉子）、`vt_alt1047_same_and_reentry_clears`
  （1047 同族 + 重入清空差异钉子）、`vt_alt1049_path_unaffected_by_l2`（1049 回归）、
  `vt_alt47_capture_not_stale_across_1049`（混合形态）、
  `vt_alt47_decsc_restore_clamped_after_resize_shrink`（**代码门高1 回归**：修前 panic
  `len 5 / index 23`，修后钳到末格）。
- **四条登记差异**（`INTEROP-CRITERIA` 已知口径注记 Q-L 条 = 验收真源）：① 重入清空
  （alacritty 进入即复位 alt 内容；ghostty 的 47 重入保留旧内容）；② 1047 退出清屏以「进入复位」
  等价覆盖（**依赖 ①**）；③ 混合形态终点 `1049h→47l→1049l` 落 D-15 分支（代码门低3 登记）；
  ④ **grow 位移丢失**（捕获槽跨 resize 放大少 `from_history` 位移；不 panic、位置在界内；
  代码门复核轮 中——补偿需镜像 alacritty 的 grow/shrink 规则，风险 > 收益，列为 term 面候选）。

### L3 `--bind-interface` 取值纪律（空值 carve-out 第五个 + 枚举形态 trim/lower）

- `crates/homeway-cli/src/serve_cli.rs`：flag 站点改 `cli_flags::take_value_empty_ok_or_exit`
  （`--bind-interface=`/`--bind-interface ""` 空值 ⇒ `BindMode::Auto`，Go `ResolveBind("")` 同义）；
  `parse_bind_iface` 重写为 `trim` → `to_ascii_lowercase` 判关键词（`""|"auto"` ⇒ Auto、
  `none|off|no` ⇒ Off）→ `t.parse::<IpAddr>()` → 其余 `Explicit(t)`（**网卡名保原大小写**，只去
  首尾空白——Go `net.InterfaceByName(v)` 吃 trim 后原串）；顺带修 `" AUTO "`/config `bind_interface=""`
  的伪告警路径（修前漂到 `Explicit(" AUTO ")` ⇒ engine 打「找不到 → 退回 auto」）。
- `cli_flags.rs`：carve-out 边界表扩第五站点（`empty_value_carveout_forms`）。
- 未越界：`--public-endpoint=`（`take_value_or_exit(..., false)`）与 `--state`
  （`take_state_or_exit`）仍拒；缺值/吞 flag 纪律不变。
- 单测：`bind_interface_empty_and_enum_forms`（flag/config 两形态 + 枚举/网卡名/IP 五组形态）。

### L5（不做，零行为注记）

- `crates/homeway-core/src/term/frames.rs` `enc_hello` nameLen 域加判死注：仓内不可达
  （双侧名 ≤64）+ Go 同形（`encHelloFlags` 无钳制）⇒ 不做、不加 `debug_assert`。

---

## §3 L7 复验：`files --host`（N1）—— 判定「闭合（非缺陷）」

**时间盒内（≈25 min）完成定因。原始命令与输出：**

1. 起本地私有出口（绝不碰生产）：
   `tools/local-rust-exit.sh start 1` ⇒ `serve 就绪：wg=:42651 … files=7802`
2. 起私有统一进程（state `/tmp/ql-l7/daemon`，serve/relay 显式停用）+ `host add`：
   `homeway-cli --state /tmp/ql-l7/daemon` + `homeway-cli host add --state … --name l7 <token>`
   ⇒ `服务会话: 就绪（会话在位，无桥直通）`
3. **默认形态全绿**（`--rate-limit` 缺省 = 2 MiB/s）：
   - `files --host l7 list /` ⇒ rc=0（列出远端根）
   - `files --host l7 stat /Documents` ⇒ rc=0
   - `files --host l7 put /tmp/ql-l7/put1.bin /ql-l7-put1.bin` ⇒ rc=0；`get … -o get1.bin` ⇒ rc=0，
     `sha256` 两侧一致（`5abd866a…`）
   - 64 MiB：`put`（33.7s ≈2 MiB/s 限速）⇒ rc=0；`get`（1.4s）⇒ rc=0，`sha256` 一致（`b9df0fab…`）
4. **失败仅在 `--rate-limit 0` 复现**：
   `files --host l7 … put /tmp/ql-l7/put32.bin /ql-l7-put32.bin --rate-limit 0`
   ⇒ `files put 失败：files 传输失败：流已终结（gone）`（rc=1，0.126s）；
   daemon 日志同拍：`control: 流 1 不在册（已关？），上行 16384 字节被拒`
5. **定因（双向文档化的既定语义，非缺陷）**：
   - Rust `crates/homeway-core/src/files.rs`：`DEFAULT_RATE_LIMIT = 2 MiB/s` +
     「**发送端速率义务**……**无 ack/credit 下的盲节流**（协议 write 方向无回压信号）……
     `0 = 不限、风险自担（越界被对端收流属可预期边界）`」；
   - daemon `daemon/server.rs`：每流上行工位有界队列 `UP_WORKER_ITEMS = 32` /
     `UP_WORKER_BYTES = 512 KiB`，超界 `finish(STREAM_END_GONE)`；
   - **Go 同形**：`pkg/files/files_cli.go:70-74`（`DefaultRateLimit = 2<<20`）+ `:126-135`
     （`0 = 不限、风险自担`）+ `internal/control/stream.go:44-56`（`upWorkerItems=32` /
     `upWorkerBytes=512<<10`）+ 单测 `l2_upstream_test.go`
     `TestStreamUpstreamOverToleranceStillGone`（「>40 帧仍收流……41 帧起 = gone——发送端仍
     义务分片节流、消费端仍义务及时读」）。
   - QIt 的 harness（`tools/qi-ab.sh` files 臂）正是用 `--rate-limit 0` ⇒ 其「`--host` 形态失败」
     与臂无关（A 出口 + A 客户端同样失败）得到解释。
6. **结论**：`QIt.md` §7.2-4 的运行时缺陷挂点**注销**（harness 用法问题；`files --host` 产品形态
   无缺陷）⇒ **N1 闭合，不进 QUIC M3**（交接表 Q11 已标「已闭合/无需承接」）。
   复验后的清理：出口 stop、daemon kill、`/tmp/ql-l7` 与 home 下测试文件已删。

---

## §4 QUIC 交接落档（12 条，Q1–Q12）

**落档位置**：`docs/QUIC-ROADMAP.md` **M1 节末「M1 开工前置检查项（Q-L 交接）」**（原写 M0
节末，代码门 中2 按「M0 已在 QUIC 程序侧完成」订正为 M1）+ **当前指针第 2/3 条** +
**附录 B**（补 `wgcore/udpbatch.rs`）+ **附录 D 第 8/9 行**；完整理由 = `QL-design.md` §3 与本文件。

- 归属期分布：**M1** = Q1/Q2/Q6/Q8/Q12（+Q5 的 M1/M2 与 Q10 的 M1 半边）；**M5** = Q7/Q9（+Q10
  的删码半边）；**已闭合** = Q11（本批 L7）。
- **Q2/Q3/Q4/Q5 四条「须显式立条」已钉死**：M1/M2 现范围小节无对应 bullet、M5 不会自然删除；
  纪律写明「三种落点（接/不接/上报主会话）」+ 二次兜底（M1 设计门已过 ⇒ 并入 M1 实现任务书或
  顺延 M2 前置）+ **可验收点 = `docs/reviews/M1.md` 须含 Q1–Q12 逐条处置表**。
- **明确不进交接清单**（同节判死，避免误领）：A5 `relay.listen` 接受集、N5 中继遥测通道、
  N11 中继状态面字段（中继零改动面）；N9-③ `dnsface` 两处小 `Vec`（量小，登记不做）。

---

## §5 判据登记索引（`docs/INTEROP-CRITERIA.md`，与代码同批 commit）

| 位置 | 内容 |
|---|---|
| 登记表 **+2 行** | ① **备用屏族 47/1047 内容面**（从「屏不切换」到「真切屏 + 双向整光标 + DECSC 槽保护/钳制」；非编号判据行；含 8 组单测与 fixtures 零变更声明）；② **取值 flag 空值 carve-out 四→五**（`--bind-interface=` ⇒ auto + 枚举形态 trim/lower；`--public-endpoint=` 仍拒） |
| Q-I 尾段原行 | **追加**「后续（2026-10-08 Q-L L3）」——原「已知残留：`--bind-interface=` 空值 Rust 拒」已消除（**未改历史条目**，符合政策 :534-536） |
| 收束段 | 追加 Q-L 段注 + **失效声明**：原句「`--public-endpoint=`/`--bind-interface=` 空值形态**不在** carve-out」的 **`--bind-interface=` 半边自此失效**（`--public-endpoint=` 仍不在） |
| 已知口径注记 | +1 条：备用屏 47/1047 **四条**登记差异 + 钳制整改说明（验收真源） |
| 计数输入集/数值语义 | **不涉及**（本批零编号判据行变更、零 wire 变更） |
| fixtures | **零变更**（`surface_codec.json` 不含 47/1047/1049，已 grep 证） |
| 词表门 | `tools/check-vocab.sh` **PASS**（零改动） |

---

## §6 测试与门证据

| 门 | 结果（终态，2026-10-08） |
|---|---|
| `cargo test --workspace --no-fail-fast` | **767 passed / 0 failed**（exit=0；含新增 9 条：L1 1 + L2 8） |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean**（exit=0；复核轮 `cargo clean -p homeway-core -p homeway-cli` 后强制重检仍 0 告警） |
| `tools/check-vocab.sh` | **PASS**（Rust 声明 5 单元 / 26 值；ledger sha256 一致；缺席表 4 项在册） |
| 定向 | `cargo test -p homeway-core --lib term::` 155/0；`term::frames` 10/0；`serve_cli` 13/0 |
| 修前红证据 | L2 钳制回归 `vt_alt47_decsc_restore_clamped_after_resize_shrink` 修前 panic `index out of bounds: the len is 5 but the index is 23`（alacritty `term/mod.rs:258`），修后绿 |

**flake 甄别（三证口径）**：

1. `daemon::carriers::{forward::tests::add_roundtrip_remove_and_rebuild,
   forward::tests::per_host_cap_cascade_and_corrupt_file, tests::uppercase_host_canonicalized_full_chain}`
   ——一次全量跑 3 红（全为 `EADDRINUSE`，端口 19990/20002/20911）。**直证并发实例**：兄弟
   worktree `/Users/zhaozhe/Documents/projects/homeway-rs-quic` 同时在跑 `cargo test --workspace`
   （其日志 `/tmp/m1-baseline-test.log` EXIT=101，自身红 2 条在册 flake）⇒ 与改动面零交集
   （Q-L 未动 `daemon/**`）+ 串行复跑 767/0 绿 ⇒ 判 flake；该机制与端口族已补入
   `REVIEW-ROADMAP.md` flake 表（含 `uppercase_host_canonicalized_full_chain` 新入册）。
2. `relay::tests::ctl_keepalive_echo`（代码门复核轮观察：`Os{54,ConnectionReset}` @ `relay/mod.rs:2051`）
   ——隔离复跑 3/3 绿、同树第二次全量 767/0 绿、与改动面零交集（未动 `relay/**`）⇒ 判 flake，
   已入册。
3. 本批**无新增 flake**；在册 flake 本轮全量跑均未触发（767/0）。

---

## §7 评审记录

### 7.1 设计门（第 1 棒，r30）

轮次目录 = `/tmp/dsh-review/r30.7NT61o/`；`exit=0`；**1 高 / 10 中 / 13 低**（共 24 条）→
**23 认同已改 + 1 部分认同 + 0 不认同**（全量回填 v2）。摘要与逐条处置表 = `QL-design.md` §8。

### 7.2 代码门（第 2 棒，r31）

轮次目录 = `/tmp/dsh-review/r31.itWvRw/`（`prompt.txt` / `output.md` 151 行 / `stderr.log`）；
命令 = `cd /Users/zhaozhe/Documents/projects/homeway-rs && dsh --profile headless "$(cat …/prompt.txt)" > …/output.md 2> …/stderr.log; echo "exit=$?"`（**前台捕获**）；
**`exit=0`**；**1 高 / 3 中 / 8 低**。评审者自查：`git status` 与开工逐行一致（未改仓库文件），
并独立复现了高危 panic（debug+release 双档 + 改动前对照不炸）。

| 编号 | 严重 | 意见（摘要） | 处置 | 落点 |
|---|---|---|---|---|
| 高1 | 高 | L2 捕获的 `saved_cursor` 跨 resize 缩小后未钳制 ⇒ `ESC 8` 越界 panic（会话被终结；评审独立复现，HEAD 对照不炸） | **认同已改**：新增 `restore_alt47_saved_decsc()`（两还原点统一走它，**先按当前尺寸钳制**，与 ghostty `restoreCursor` 同形）+ 回归测试（行/列缩小各一；修前红→修后绿） | `vt.rs`；`INTEROP-CRITERIA` 口径注记 |
| 中1 | 中 | Q11 判定被改但 `QL.md` 不存在（4 处引用悬空）+ 与 `QL-design` §2-N1/§3-Q11 相反 | **认同已改**：本文件 §3 落 `QL.md`（含原始命令/输出）；`QL-design` §2-N1/§3-Q11 加「后续」说明（追加式） | 本文件 §3；`QL-design.md` |
| 中2 | 中 | 交接收件人过期（M0 已完成、落档进 main 副本） | **认同已改**：小节改名「**M1** 开工前置检查项」并移入 M1 节末；纪律/二次兜底/可验收点写明；补 M0 状态对齐说明 | `QUIC-ROADMAP.md` |
| 中3 | 中 | 「Q-L 已收口/并发批已清」不实（改动未提交） | **认同已改**：改为「已落地并随本批 commit 入库；待主会话补 `REVIEW-ROADMAP` 登记行后视为完全收口」；删「并发批已清」断言 | `QUIC-ROADMAP.md` |
| 低1 | 低 | 「单测 6 组」与 7 个测试名不符 | **认同已改**：改「8 组」并与名单自洽 | `INTEROP-CRITERIA.md` |
| 低2 | 低 | 「1049 进入作废捕获」臂按不变量不可达（死防御） | **认同已改**：删除该臂；测试改钉可达两态；口径注记因果写清 | `vt.rs`；`INTEROP-CRITERIA.md` |
| 低3 | 低 | 混合形态 `1049h→47l→1049l` 终点与 ghostty 分叉 + `dec_modes` 不一致（未登记） | **认同已改**：登记为第 ③ 条差异（登记不改） | `INTEROP-CRITERIA.md` |
| 低4 | 低 | 设计 §2.2 五处行号/条数搬运不实 | **认同已改**：五处全部校正（带 dated 校正标记）；扫描口径写进本文件 §1.2 | `QL-design.md`；本文件 |
| 低5 | 低 | 交接表丢 M5 列 / Q9 措辞 / 前言口径 / `udpbatch.rs` 未在删码清单 | **认同已改**：M5 列恢复、Q9 与 M5 原文对齐、前言改「多数是耦合 + Q8/Q12 保留面」、附录 B 补 `udpbatch.rs` | `QUIC-ROADMAP.md` |
| 低6 | 低 | `QL-design §3-Q5` 漏标「须显式立条」 | **认同已改**：回填（真源内部矛盾已消） | `QL-design.md` |
| 低7 | 低 | 落档范围与 §5.4 授权口径不一致（事后按 §5.4 复核会误判越权） | **认同已改**：§5.4 追加「实际落档范围与理由」 | `QL-design.md` |
| 低8 | 低 | `QL-design` 对 `QUIC-ROADMAP` 的行号引用漂移 | **认同已改**：三处改符号引用（复核轮又补第 4 处 `:422`→符号） | `QL-design.md` |

### 7.3 代码门复核轮（r32）

轮次目录 = `/tmp/dsh-review/r32.28ZnTZ/`（`prompt.txt` / `output.md` 139 行 / `stderr.log`）；
**`exit=0`**。结论原文摘要：**「可过代码门——代码本体无阻断项」**（高1 已闭合且评审独立复现验证：
两还原点统一走 helper、上界钳制与 ghostty 同形、原序列不再 panic；负行与 `cursor` 拷贝面经注入 +
1600 步随机探针判「不可达/不需要额外钳制」；三门复跑 767/0 + clippy 0 + vocab PASS）。

复核轮另提 **1 中（新）+ 4 低**，处置：

| 编号 | 严重 | 意见（摘要） | 处置 | 落点 |
|---|---|---|---|---|
| 中（新） | 中 | **grow 位移丢失**：捕获槽跨 resize **放大**（跨回滚）少 `from_history` 位移 ⇒ 相对 HEAD/ghostty 的窄形态回归（不 panic、位置在界内） | **部分认同**：**登记不改**（补偿需镜像 alacritty grow/shrink 位移规则 + 跟踪主屏 history 跨 resize 增减，风险 > 收益；本批为治理批）——登记为第 ④ 条差异 + 列 term 面候选 | `INTEROP-CRITERIA` 口径注记；`GAP-AUDIT` K-15；本文件 §2-L2 |
| 低1 | 低 | `QL-design` 差异计数 2 与 `INTEROP-CRITERIA` 3 不自洽 | **认同已改**：§4-L2 追加「实登记共 4 条」 | `QL-design.md` |
| 低2 | 低 | `QUIC-ROADMAP` 同文件 M0 状态自相矛盾 | **认同已改**：在 M1 前置节写明「main 副本状态待 QUIC 程序合回更新，以程序侧记录为准」（不代改其状态字段） | `QUIC-ROADMAP.md` |
| 低3 | 低 | `QL-design:157` 的 `:422` 已失效 | **认同已改**：改符号引用 | `QL-design.md` |
| 低4 | 低 | 新在册外 flake `relay::tests::ctl_keepalive_echo` | **认同已改**：入册 flake 表（含隔离 3/3 绿证据） | `REVIEW-ROADMAP.md` |

### 7.4 收口必办项核对（r32 第 123-129 行）

- [x] 落 `docs/reviews/QL.md` §3（N1 原始命令与输出）——本文件
- [x] `QUIC-ROADMAP` 删「commit 见 QL.md」/「并发批已清」不实措辞（改「已落地并随本批 commit 入库」）
- [x] 交接锚点：`QL-design.md` §3 + 本文件 §4（两者随本批入库，悬空已消）
- [x] grow-desync 同批登记（第 ④ 条差异）
- [x] 低项（差异计数 / `:422` / §5.4 落档范围 / flake 表）

---

## §8 不做项与残余（防静默）

### 8.1 本批明确不做（带理由与落点）

| 项 | 理由 | 落点 |
|---|---|---|
| `encode_frame`/`enc_hello` 长度域 | 仓内不可达（双侧名 ≤64 / 三条大帧出口全有界）+ Go 同形 | 代码注记（`frames.rs` `enc_hello`）+ `QL-design.md` §1-A4/§4-L5 |
| `relay.listen` 接受集放宽 | 差异方向 = 更严（不破坏在册部署）；收益≈0 | `QL-design.md` §1-A5 |
| 中继遥测通道 / 中继状态面字段 / `dnsface` 两处小 `Vec` | 中继零改动面 / 量小 | `QUIC-ROADMAP.md` 交接节「不进清单」 |
| 旧大写条目不迁移 / 双等号形入口 / `interfaces()` Logf / `plain_text` 出锁 / 装配错位 / harness 改进 / 互操作脚本小瑕 / term-keyenc 热路径 / 第三份泵拷贝 / 跨报文乱序 E2E / F1 进程级 E2E | 见 `QL-design.md` §2 逐条（形态/工具面/无实测驱动/价值不成比例） | `QL-design.md` §2；本文件 §1.2 |
| 公共端点面 3 条（写失败告警 ×2 / 落盘失败致命性 / flag 值域校验） | 批派单「公共端点不在本批修」（非纯取值纪律，含启停/公布语义） | **转 QUIC M1 须显式立条**（Q3/Q4/Q5） |

### 8.2 残余（如实登记）

1. **L2 四条与 ghostty 的差异**（① 重入清空 ② 1047 等价性依赖 ③ 混合形态终点 ④ grow 位移丢失）
   ——验收真源 = `INTEROP-CRITERIA` 已知口径注记 Q-L 条；④ 的补偿列为 term 面候选（需实测靶点）。
2. **L3 的 `--bind-interface=` 空值**：Go = auto、Rust 同（已对齐）；但**网卡名存在性**仍在引擎
   运行期查（config 期不验，Go 同口径）——非残余，仅口径说明。
3. **L1 的 `VtError` 目前单变体**：将来新增错误种类时按仓规继续 thiserror（不引入字符串错误）。
4. **A1 的 8GB 量级辨析**：理论上界（1024 已认证会话 × 内核不钳制）；内核钳制下 ≈0.4GB 级——
   **不以此判死**，交 M1 用实测裁决（交接表 Q1）。
5. **`GAP-AUDIT` 的 `peer_ttl` 窄于 Go / 报错无行号 / 写回无 CAS**（P2-7 注）——维持登记。

---

## §9 commit 清单与收口

| # | commit | 内容 |
|---|---|---|
| 1/4 | `68a52b1` | 账实对账落账（GAP-AUDIT 追加式刷新 + QL-design 入库与校正） |
| 2/4 | `85a8007` | B 类实现（L1/L2/L3 + L5 注记） |
| 3/4 | `83ab853` | 判据登记（INTEROP-CRITERIA 两行 + Q-I 追加 + 收束段 + 口径注记） |
| 4/4 | `e0b1b25` | QUIC 交接落档（M1 前置检查项）+ flake 表扩证 |
| 批记录 | （本文件入库 commit） | `docs/reviews/QL.md` |

**收口状态**：改动已 push 到 `origin/main`（AGENTS：main 直推）。**待主会话**：在
`docs/REVIEW-ROADMAP.md` 补 **Q-L 状态行**（对账摘要 + 三条 B 类落地 + 交接清单指针；状态/设计门/
代码门/记录四列）——按 `QL-design.md` §5.2 由主会话写；此后 Q-L 视为完全收口（`QUIC-ROADMAP.md`
「下一步」第 2 条据此复核）。
