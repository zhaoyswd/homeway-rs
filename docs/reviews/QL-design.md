# Q-L 设计文档 — 账实对账 + 挂空小项（第 1 棒）

> 2026-10-08 立稿。开工基线：HEAD = `5da59e8`（Q-K 收口），`git status --short` 空。
> 本批定位 = **治理批**：把「说好下一批做、下一批没做也没说不做」的挂空项**逐条判死**
> （闭合 / 转新批 / 明确不做），把**传输耦合项**显式交接给新立的 QUIC 程序
> （`docs/QUIC-ROADMAP.md`，2026-10-08 立稿），并就地做掉**传输无关**的小项。
>
> 本棒边界：只读代码 + 只改文档；**不碰产品代码**（改动留给第 2 棒）；不碰两台生产出口 /
> `homeway` / `tier` 两仓 / `baseline/`；不发 tag / Release / PR。
>
> 行号说明：本文件所引行号 = 本棒**当日实测**（HEAD `5da59e8`）并经设计门评审（§8，dsh
> `r30.7NT61o`，exit=0）逐条复核订正；后续实现时按符号重定位。

---

## 0. 方法与复验口径

1. **扫描面**：`docs/reviews/AUDIT-2026-10-07.md` 全量 + `docs/GAP-AUDIT.md`（P0/P1/P2 表
   与「三、已知登记总账」K 表）+ Q 批各批记录（`QB/QFB/QC/QD/QE/QF/QG/QH/QI/QIt/QJ/QK`，
   逐份扫「残余 / 挂账 / 移交 / 后续小项 / 不做项」节）+ `docs/REVIEW-ROADMAP.md`
   + `docs/PERF-AB.md`（§9.15.6 等挂账节）。
2. **判定口径**（本批新增，替代「挂下一批」的模糊表述）：
   - **闭合**：一手证据（代码行 / Go 基线行 / 实测）证明已交付或不存在；
   - **转新批**：判定为真缺口且有明确接收方（`转 QUIC M<n>` 或「Q-M 候选」）；
   - **明确不做**：给理由 + 落点台账，**不留「后续再看」**。
3. **传输耦合识别**：凡会被 QUIC M5 删除 / 重写，或属 WG·relay·公共端点·wire 面者，
   **不在本批修**，进 §3 交接清单；反之为 B 类（§4）。**注意**：「公共端点」面的三条剩余
   （§1.1）**不会**被 M5 删除/重写——本批仍不修是遵从批派单的「公共端点不在本批修」条款，
   因此 §3 对它们的要求是「QUIC 程序须**显式立条**（M1 现范围小节无对应 bullet），否则退回
   主会话裁决」，绝不允许再次静默。
4. 证据里凡称「Go」= `baseline/homeway`（只读 oracle，d4148f6 冻结）；称「ghostty」=
   `baseline/homeway/third_party/libghostty-vt`（Go 侧终端引擎真源）；称「alacritty」=
   `alacritty_terminal 0.26.0`（cargo registry 源码，本棒实测 API）。

---

## 1. 对账总表（本批主价值）

> 判定列：**闭合** / **转 QUIC M<n>** / **转新批** / **不做（登记）** / **做（B 类）**。

| # | 项 | 出处（挂空原文） | 判定 | 一手证据 | 落点台账 |
|---|---|---|---|---|---|
| A1 | `MAX_ASSOCS_TOTAL=1024` × 4MB+4MB ≈8GB 内核缓冲的字节预算化 | `QC.md` §5「字节预算化留 **Q-I/Q-J**」；`QC.md` §4 补充 1 同 | **转 QUIC M1** | `relay/mod.rs:61`（常量）、`:247`（`bump_sock_bufs` 4MB×2 尽力而为）、`:939-944`（全局闸）、`:1109-1140`（认证后才抬）；Q-I/Q-J 记录逐份扫得**零落地**；内核钳制（注释自陈「尽力而为——内核钳制」，Linux 默认 `net.core.wmem_max=212992` ⇒ 实际量级 ≈0.4GB 而非 8GB，且需 1024 条**已认证**会话——产品设备量级远低于此） | `QL-design.md` §3-Q1 / `QL.md`（收口时） |
| A2 | `vt.rs` 备用屏内容面（`?47h/?1047h` 不可达） | `QD.md` §6.1「**不在本批**」+ §7「（**Q-J/仿真等价批**）」；R6-gate1 §B6 原登记 | **做（B 类 L2）** | vte 0.15 只把 1049 收进 `NamedPrivateMode`（`vte-0.15.0/src/ansi.rs:897-919`），47/1047 落 `PrivateMode::Unknown` 被 alacritty 忽略（`term/mod.rs:1936-1940`）；**但** `alacritty_terminal 0.26.0` 有**公开** `Term::swap_alt()`（`term/mod.rs:714`）与公开 `grid()`（`:645`）/`grid_mut()`（`:650`）、公开 `Grid::cursor`/`saved_cursor`（`grid/mod.rs:113/117`）——`vt.rs:1405` 注释「alacritty 无公开 swap-alt API」**不成立**；ghostty 两者都实现（`Terminal.zig:4800-4869`：47 = 切换+光标拷贝不清屏；1047 = 退出时清屏；1049 = 保存光标+进入清屏）；Go 旁路扫描器已认 47/1047（`scan.rs:159`）——**wire 位对、屏内容错** | `QL-design.md` §4-L2 / `QL.md` |
| A3 | `SessionVt` 错误类型收敛 thiserror | `QD.md` §6.2 末尾行（「建议下批顺手收敛」）+ §7 建议项 | **做（B 类 L1）** | `vt.rs:300`（`new -> Result<Self, String>`）、`:437`（`resize -> Result<(), String>`）、错误构造与文本 `:301-307`/`:438-444`；仓规第 1 条禁字符串错误；生产调用面 2 处（`service.rs:1673` `SessionVt::new`、`service.rs:3239` `let _ = vt.resize(...)`）+ 测试面 ≥40 处（`vt.rs` 内测 36 + `codec.rs`/`keyenc.rs`/`responder.rs`/`tests/fuzz_replay.rs` 8）全为 `unwrap/expect` ⇒ 改名后照样编译；Display 文本被拼进日志行 ⇒ 收敛时**必须逐字保文** | `QL-design.md` §4-L1 / `QL.md` |
| A4 | `encode_frame`/`enc_hello` 长度域（静默截断/回绕） | `QD.md` §6.1「不做（登记残余）…**协议批再议**」+ §7「（协议批）」 | **不做（闭合）** | 仓内不可达：① 会话名双侧 ≤64（CLI `term_cli.rs:614-624` `validate_name`；服务端 `service.rs:118` `valid_name`，`952/1017` 拒 `invalid_name`）；② 大帧三条出口全有界——DATA 恒 ≤16KiB（`frames.rs:24` `DATA_CHUNK`）、快照/差分走 `codec::fragment_payload`（`FRAG_CHUNK=60KiB ≤ MAX_PAYLOAD`）、CLIPBOARD 侧钉 `MAX_PAYLOAD-3`（`codec.rs:900`）⇒ `encode_frame` 的 `MAX_PAYLOAD=65535` 永不触发；③ Go 同形（`pkg/term/frames.go:222-230` `encHelloFlags` 的 `p[5]=byte(len(name))`，无钳制；`termMaxNameLen=64` 只作用于 `encName`，`frames.go:466-475`）。**无「协议批」存在**，本条就此判死（可选：加注释指路不可达） | `QL.md` §不做项（本文件 §4-L5） |
| A5 | `relay.listen` 接受集放宽（主机名） | `QH.md` §5.2-2「跨语言差异登记在此（**如需放宽可随 Q-J 通用性批**处理）」；Q-J 记录零落地 | **不做（登记）** | Rust `relay_cli.rs:101-107` `parse_listen` 只收 `:port` / IP:port（`:port` ⇒ `Ipv4Addr::UNSPECIFIED`）；Go `net.ListenUDP` 走 `net.ResolveUDPAddr` 可收主机名。差异方向 = **更严**（不破坏在册部署：Mac 生产出口与本机默认 state 已核 `QH.md` §0.1/§0.2 皆 `:41741`；**阿里云出口不可达未检**）；放宽需在启动期引入阻塞 DNS 解析与失败语义，收益≈0。中继在 QUIC 程序里**零改动** ⇒ 不属交接面 | `QL.md` §不做项 |
| A6 | `--bind-interface=` 空值：Go=auto / Rust 拒 | `QIt.md` §7.3 末行「**登记为已知识别差异**，未扩 carve-out」；`QIt.md` §7.2-3 同 | **做（B 类 L3）** | Go `internal/server/cli.go:190-198`：`ResolveBind("")` 先 `TrimSpace` 再判 ⇒ `BindAuto`；大小写不敏感（`strings.ToLower`）⇒ `" AUTO "` 亦 `BindAuto`。Rust：flag 站点用 `take_value_or_exit(..., false)`（`serve_cli.rs:457-458`）⇒ 空值 exit 2；config `bind_interface=""` 过校验（`serve_cli.rs:335-345` `validate_bind_interface` 收空串）后走 `parse_bind_iface("")`（`serve_cli.rs:738-749`）落 `Explicit("")` ⇒ engine 打伪告警「找不到（no such interface）→ 退回 auto」（`engine.rs:309-338`）。属纯取值纪律（传输无关），改动只在解析层 | `QL-design.md` §4-L3 / `INTEROP-CRITERIA` 登记（§5.3） |
| A7 | `GAP-AUDIT.md` P1-8「公网端点细节分支族」（标「C/D 批」但**无任何批次认领**） | `GAP-AUDIT.md:107`；`AUDIT-2026-10-07.md` 无对应条目 | **部分闭合 + 剩余转 QUIC M1（须显式立条）**（见 §1.1） | 见 §1.1 逐子项 | `GAP-AUDIT.md` P1-8 行（§5 改法）+ §3-Q3/Q4/Q5 |
| A8 | `GAP-AUDIT.md` 状态字段刷新（落后 Q 批 10 个批次） | 本批派单所列 | **做（文档，第 2 棒执行）** | 见 §5 逐行改法 | `GAP-AUDIT.md` + `QL.md` |

### 1.1 A7 逐子项（P1-8）取证

| 子项（GAP-AUDIT 原文） | 现状 | 证据 |
|---|---|---|
| 「外口 ≠ 监听口（沿用历史端口/回退），用 STUN 的 IP + UPnP 的外口公布」 | **✅ 已实现（Go 同串）** | `engine.rs:1550-1558`（含 `pinned` 运行期判据 + 逐字行文） |
| 「已按 --public-endpoint 配置公布（在）」 | **✅ 已实现** | `engine.rs:1472-1488` |
| 「--relay 解析失败 → 跳过中继注册」 | **✅ 已实现（Go 同串）** | `engine.rs:554-560`；Go `serve.go:425` |
| 「--bind-interface 找不到 → 退回 auto」 | **✅ 已实现（Go 同串 + 模式随之变 auto）** | `engine.rs:309-338`（`find` 在 `:309`、回退块 `315-338`）；Go `cli.go:201-207`（告警行 `:205`） |
| GAP-AUDIT 备注「（Rust Explicit 网卡名不验存在性）」 | **❌ 审计误判（应剔除）** | `engine.rs:309` 即 `egress::interfaces().find(name)`，找不到才告警回退；与 Go 同口径（Go 也不在 config 期验存在性——`validateBindInterface` 只拒 `:`/`/`/空白`，存在性在 `ResolveBind` 运行期查） |
| 「写 %s 失败」（`public_endpoint.txt`） | **❌ 真缺（Rust 静默）** | Rust `engine.rs:1476` / `:1630` 均 `let _ = std::fs::write(...)`；Go `publicendpoint.go:126` / `:226` 有 `logf("公网端点：写 %s 失败（%v）")` |
| 「监听端口落盘失败」 | **❌ 真差异（Rust 致命 / Go 非致命）** | Rust `engine.rs:418` `std::fs::write(cache_dir.join("listen_port.txt"), …)?` 用 `?` 上抛；Go `role.go:95` 是 `logf("监听端口落盘失败（%v）—— 只是少了给人看的记录，不影响隧道")` |
| 「--public-endpoint 非法 → 按未配置」 | **❌ 真缺（方向不一致）** | Go 前端 CLI **报错**（`cli.go:98-105`）；Go 守护/装配期 **告警+清空**（`serve.go:113-121`）。Rust：config 文件面已由 Q-H F1 改**拒启**（更严，已登记 `QH.md` §5.3）；**flag 面零校验**——`serve_cli.rs:694-695` 直赋 `cfg.public_endpoint`，非法值会原样写进 `public_endpoint.txt` 并进 token |

**判定**：A7 记的 4 类子项中 **3 类已闭合**（审计行落后于交付）、1 条备注为误判需剔除；
**真正剩余 3 条**（两处落盘告警 + 1 处 flag 校验）——**均不会被 M5 删除/重写**（公共端点面
在 QUIC 下保留），本批按派单条款不动，**必须由 QUIC M1 显式立条接收**（§3-Q3/Q4/Q5）。

### 1.2 A8（GAP-AUDIT 状态刷新）——改法见 §5

Q-A 只修了 P0-1/P1-3/P1-6 三行；此后 10 个批次（Q-B…Q-K）的交付与裁定**一行都没回填**，
且 P1 里有两行（P1-4/P1-7）其实早已交付、一行（P1-8）该按 Q-A 先例做「追加式状态修正」。

---

## 2. 新发现的挂空项（扫描各批「残余/挂账/移交」节所得）

> 扫描范围：`QB/QFB/QC/QD/QE/QF/QG/QH/QI/QIt/QJ/QK` 记录 + `AUDIT-2026-10-07.md`
> + `PERF-AB.md`（§9.15.6）+ `REVIEW-ROADMAP.md`。设计门（§8）独立复扫又补了 N15–N19。

| # | 项（挂空原文） | 判定 | 证据 / 理由 |
|---|---|---|---|
| N1 | **daemon 托管远程 files（`files --host`）本机实测失败**（`QIt.md` §7.2-4「建议单开小项排查」，**零认领**） | **转新批**（第 2 棒时间盒复验 ≤1h；根因若在承载面 ⇒ 转 QUIC M3）。**后续（2026-10-08 第 2 棒 L7 复验，代码门 中1）：判「闭合（非缺陷）」**——`files --host` 默认限速形态实测全绿（list/stat/put/get × 1 MiB/64 MiB，sha256 一致）；失败只在 `--rate-limit 0`（不限、风险自担）下复现，属双向文档化的盲节流边界（Rust `files.rs` 发送端速率义务 + daemon 上行工位 32 帧/512 KiB；Go 同形）⇒ **不进 QUIC M3**；证据 = `docs/reviews/QL.md` §N1（原始命令与输出） | `QIt.md:336`：「流已终结（gone）」、A 出口 + A 客户端同样失败、判 pre-existing；错误模板 = `daemon/client.rs:42`（`#[error("流已终结（{0}）")]`），字面 `gone` = `daemon/vocab.rs:238`（`STREAM_END_GONE`，由 `client.rs` 流终结路径填入）。该形态 = P1-7 交付面（§5.1 台账须加注） |
| N2 | F11「盘上遗留旧大写条目不迁移、`remove_host` 级联摘不掉」（`QH.md` §5.2-8「迁移面留后续」） | **不做（登记）** | canonical 只施加在入口（`carriers/mod.rs:149/163/175/193/204/208/212`），`hosts.rs:548 load_hosts` 与规则装载不归一 ⇒ 旧大写条目在 CLI 面不可达。影响面 = **存量 state 不可枚举**（本批不碰生产 state，按最坏影响登记）；自愈 = 重加/手改 JSON；归一化装载要定义同名冲突合并语义（收益小、语义风险 >0） |
| N3 | 两套等号形入口并存（`carriers_cli` 的 `expand_flag_eq` vs `cli_flags::split_flag`，`QH.md` §5.2-13「去重留后续小项」） | **不做（登记）** | 纯形态、行为一致已由 Q-H 代码门核过；跨 ~10 站点重构的回归面 > 收益 |
| N4 | `egress.rs` `interfaces()` 的 `if_nametoindex` 告警未进 `Logf`（`QC.md` §5 / L3：F12 完整语义「告警进 Logf」**挂 Q-J**，Q-J 只做了平台化文案） | **不做（登记）** | `egress.rs:200` 仍 `eprintln!`；`interfaces()` 无日志入参且调用面含无日志上下文（token 生成/候选枚举），接入属形态改造；告警本体是 Rust 自加诊断（Go 无此告警），不影响行为对齐；exit 进程形态下 stderr 可见 |
| N5 | 中继无 JSON 遥测通道（`QC.md` §5「新建通道属后续批次」） | **不做（登记）** | 需求面无驱动（用户裁决 F7.4 = 只做日志/单测）；中继在 QUIC 程序里**零改动**，不属交接面 |
| N6 | 中继**客户端方向 v6**（主监听口 v4-only，`QC.md` §5「挂账」） | **转新批（QUIC M1，须显式确认）** | 真链 = `relay_cli.rs:103-105`（`:port` ⇒ `Ipv4Addr::UNSPECIFIED`）+ `relay/mod.rs:388-403` `listen_with_fallback` 绑它；Go `relay.go:289-319`（`mustResolve` + `net.ListenUDP`）为**双栈** ⇒ v6 客户端可达差异。**冲突声明**：修它 = 改 relay 代码，与 `QUIC-ROADMAP.md`「中继零改动/不引 QUIC 依赖」冲突，且 M1 中继条（`QUIC-ROADMAP.md` M1 中继条——2026-10-08 落档后行号漂移，按符号引用：M1「中继：控制面零改动…200pps/句柄预算与隧道包尺寸须实测复核」条）**没有** v6 字样 ⇒ 须 QUIC 程序显式扩范围；若不接 ⇒ 退回「登记差异（不做）」并上报主会话 |
| N7 | Q-B F3 主路径 / F10 链 **e2e 未做**（`QB.md` §6-1「挂后续小项」） | **转 QUIC M1** | UDP 上行门/上限/计数与 `deliver_udp53→udp_drop` 链只有谓词/直测级覆盖；M1 的**风险 #5**（DATAGRAM 丢包与全局代理 TCP 的背压信号路径变更）正是该测试面 ⇒ 在 M1 一并补测或登记 |
| N8 | Q-B F2-5②「装配错位（`cfg.dns=Some` 而 `dns_rx=None`）⇒ 回投通道无人 drain、tag 无界增长」（`QB.md` §6-2②） | **不做（登记）**（可选：第 2 棒加 `debug_assert`/注释） | 生产装配恒成对（`engine.rs:344-386` 同一分支产出 `dns`/`dns_events`，设计门已实证）；属结构脆弱面而非可触发缺陷；正常路径回执回收已由 Q-B F2 覆盖 |
| N9 | Q-I 性能残余（`QI.md` §6） | **转 QUIC（分档）** | ① `wtransport/bind.rs:287`、`server/bind.rs:757` 每包 `Vec`、`server/bind.rs:1020` `tx_drain_rounds` 每轮 `msgs` Vec（`QI.md:189`）⇒ **M5 删码即消失**；② `reactor_turn`（`server/intercept/mod.rs:2211`，调用 `:1737/:1779`）每拍零超时 `poll` 税 ⇒ **不随 M5 消失**（intercept 保留，`QUIC-ROADMAP.md` 附录 E/保留清单——按符号引用：目标架构「server/intercept 保留、收窄」）⇒ 移交 M1 复测后重定动作；③ `dnsface.rs:228/333` 两处小 `Vec` ⇒ intercept 保留、量小，**不做（登记）** |
| N10 | `plain_text` 出锁（`QD.md` §6.2/§7「建议进下一批」，**零认领**） | **不做（登记）** | A1 已消掉 ≈250MB 材质物化与千万次分配，残余 = 上限态 ~10MB 文本构建仍持服务锁；彻底出锁需 vt 结构改造（锁纪律回归风险 > 收益） |
| N11 | `leg_missing_input_drops` 无状态面字段（`QJ.md` §5.4「并入 term 状态面留后续批」） | **不做（登记）** | 中继面（QUIC 零改动）；节流日志 + 测试可读已足够归因 |
| N12 | harness 改进：轮末 loadavg 落盘 / en0 端点自动补跑 / RSS 合成多流（`QIt.md` §7.3、`QI.md` §6） | **不做（登记）** | 工具面（`tools/qi-ab.sh`），只服务性能判决；QUIC 程序自带 `tools/quic-ab.sh`（M0 转正）会重做 harness ⇒ 不重复投资 |
| N13 | QG 互操作回归脚本小瑕（`QG.md` **§6.3**（`:265-267`）「手工等价命令完成互操作回归…脚本修正属工具面，留给后续批」） | **不做（登记）** | 工具面；当次已以手工等价命令完成回归（结论不受影响） |
| N14 | `files` 上传 picker 自动化不稳定（GAP-AUDIT K-10）/ UPnP 真网关实测（K-5）等已知登记 | **维持** | 用户触点 / 环境不可测，无需动作 |
| **N15** | **term/keyenc 热路径分配**：`vt.rs` 每帧分配（`write_collecting` 的 `responses: Vec<Vec<u8>>` + 每批 `carry_prefix()` 的 `Vec<u8>`，`:1004-1025`；**2026-10-08 第 2 棒校正，代码门 低4⑤**——原引 `:1014-1051` 实为 `carry_prefix` 尾 + `is_dcs_prefix`，系逐字继承 AUDIT 未复核）、`keyenc.rs:351-542` 键表线性扫描（`AUDIT-2026-10-07.md` §Q-I 的 `[P2 🔎]` 状态快照行末句「**term/keyenc 面不在前段**（终端栈/通用性面，**归后续批**）」——**唯一一条明写「归后续批」却无人认领**；Q-D 只借用了 `vt.rs` 同区做 symLen 截断、Q-J F1 只做平台口径，性能面两批都没接） | **不做（登记）** | 终端引擎在 QUIC 下保留（M5 不删）、传输无关；但**无实测驱动**——R6/Q-D/QI 的性能判决未测出该面瓶颈，且改动落在终端输出热路径（golden/surface 语义风险）；列为「若将来有实测靶点（term 面 profile）再开」的候选 |
| N16 | `QFB.md` §6-3（`:198`）「残留第三份泵拷贝**另行登记**」——**至今无落点** | **不做（登记）** | CLI `cmd_portfwd` 是测试动词（Q-F-B D8 裁决不切核心实现）；`QF.md:242` 只收了「目标文案第三份拷贝」（已随 Q-F-B 复用 `pf_target_text` 落账），泵拷贝本体本批补登记 |
| N17 | `QI.md:194`「F2 的 `wgcore` 站点 Engine 级测试空档」 | **转 QUIC M5（自然消失）** | `wgcore` 在 M5 删除（`QUIC-ROADMAP.md` M5 范围条——按符号引用；**注意 M5 原文措辞 =「`wgcore`（除 QUIC 岛共用类型）」**，共用类型保留）⇒ 无动作，登记即交接 |
| N18 | `QK.md:129`（§6-8）「出口侧跨报文乱序无 E2E 用例」 | **不做（登记）** | `reasm.rs` 在 intercept **保留面**，QUIC 后仍缺该用例（真栈 `Fragmenter` 单缓冲使该形态只能手工构造）；判「登记维持」（低价值高构造成本），M1 若重测分片可顺带 |
| N19 | `QC.md` §5「F1 进程级 E2E 未执行（表满注入需 grace 注入面）」 | **不做（登记）** | 同一不变量已由 engine 级单测（真实 `Device` + `apply_dev_ops`）覆盖；进程级复现需给 `grace` 加注入面（CLI/config 面），价值不成比例 |
| N20 | `PERF-AB.md` §9.15.6（`:827-839`）四条「登记不实装——后续决策」：E13 `--loopback-only` token / 发送线程默认 on 的重开条件 / 8s 不动 / Linux `sendmmsg`+五跳管线合一 | **转 QUIC M1/M5（分档）** | 四条全部是 **WG 路径性能项**（发送线程/管线/端点竞速）；M1 换承载后：① 端点竞速 = QUIC 岛原生网络（重定）；② 发送线程/五跳管线/`sendmmsg` ⇒ **M5 删码即消失**；③ 「8s 不动」是矩阵口径不是缺口 ⇒ 本批不逐条判，按 §3 分档交接 |

### 2.1 复核确认「已闭合」的挂空候选（防二次挂空）

| 项 | 原挂点 | 闭合证据 |
|---|---|---|
| Q-C F12「完整语义挂 Q-J」 | `QC.md` §5 | Q-J F5 已落地：`index_ok` 消费（`egress.rs:227` 候选过滤）、三处 index 键面 name 优先（`iface_same`）、告警平台化（`egress.rs:195-207`）；仅剩 N4（Logf） |
| Q-B F7 出口侧分片重组 / F9 ICMP 不可达 | `QB.md` §6-3 | **Q-K 已实现并注销**（`INTEROP-CRITERIA` Q-B F7 条已加注销指针） |
| Q-H F14 launchd（移交 Q-J） | `QH.md` §5.1 | Q-J F6 plist **内容**精确匹配（四态）已落地 |
| Q-H「relay stop→start 不可恢复」跨批勾选义务 | `REVIEW-ROADMAP` Q-H 节 | Q-H F3 已复核勾选（Q-G F3 修根因） |
| Q-I 尾段「`dnsface` 64KB 零初始化」第一靶点（Q-E 明确交接） | `QE.md` §6.1 | Q-I 尾段 F1 已做（叶帧 1.71%→0 判绿） |
| QF §7 portfwd-B 交接块 | `QF.md` §7 / `QF-design.md` §2.4 | **Q-F-B 已交付**（tier SHALL 转达标；残余 14 条逐条有处置） |
| Q-D 提出的 `SessionVt`/`encode_frame`/备用屏三项 | `QD.md` §7 | 本批 A2/A3/A4 判死 |
| `reactor.md:60` 登记（`udp_seq_of` 从未赋值 / `dial_accept` decr_flow 下溢） | `reactor.md` §存量 | Q-B F5 已修（判据行已登记） |

### 2.2 分组式残余确认（防「漏扫」误读）

本批对下列**各组**做过逐条或整组判死；组内已另立条目的按括号指针，其余组维持原批记录
（原批已带理由、无「下批做」字样）：

| 组 | 规模 | 处置 |
|---|---|---|
| `QFB.md` §6（`:192-215`） | 14 条 | 13 条维持（Go 同形/用户触点/登记）；**#3 补落点 = N16** |
| `QD.md` §6.2 | 9 条 | 维持；**SessionVt 行 = A3/L1**、`plain_text` 出锁 = N10 |
| `QH.md` §5.2 | 15 条 | 维持（含 `peer_ttl` 窄于 Go = §5.1-P2-7 注、F11 迁移面 = N2、双入口 = N3）；launchd 已由 Q-J F6 闭合 |
| `QJ.md` §5.4 | 6 条 | 维持；`leg_missing_input_drops` = N11 |
| `QK.md` §6 | 8 条 | 维持；跨报文乱序 = N18 |
| `QF.md` §6.3（`:251-262`；**2026-10-08 第 2 棒校正，代码门 低4**——原引 `:236-248` 不实） | 7 条 | 维持（portfwd 残余，Q-F-B 已逐条处置/用户触点） |
| `QG.md` §6.2（`:244-258`；**2026-10-08 第 2 棒校正，代码门 低4**——原引 `:207-267` 不实） | 9 条 | 维持；互操作脚本小瑕 = N13 |
| `QE.md` §6.2 | **10 条**（**2026-10-08 第 2 棒校正，代码门 低4**——原写「2 条」不实） | 维持（沙箱 TOCTOU 未收口，如实登记） |
| `QI.md` §6 / `QIt.md` §7.3 | **各 10 条**（**2026-10-08 第 2 棒校正，代码门 低4**——原写「6+11 条」不实） | 性能/harness 面 = N9/N12/N20；其余维持 |
| `QC.md` §5 | 8 条 | F12 = N4、遥测 = N5、v6 = N6、assoc 预算 = A1；其余维持 |

### 2.3 明确「接受残余」复核

`PERF-AB` 挂账节已按 N20 triage（不再笼统称「均有接收方」）；`ROADMAP.md`「当前指针」遗留清单、
`QI-DNS` 小批（等指令）均有明确接收方（用户触点 / Q-I-DNS 小批 / QUIC 程序），不属挂空。

---

## 3. QUIC 交接清单（给 QUIC 程序主会话，可直接消费）

> 口径：本批判定「传输耦合 ⇒ 不在 Q-L 修」的全部条目。**归属期**按 `docs/QUIC-ROADMAP.md`。
> 凡标注「**须显式立条**」者 = M1/M3 现范围小节**没有**对应 bullet，QUIC 程序须在开期时
> 显式认领；不接则退回主会话裁决（绝不允许再次静默）。

| # | 项 | 为什么属传输耦合 | 建议归属期 | M5 删码是否自然消失 |
|---|---|---|---|---|
| Q1 | 中继全局 assoc 上限 `MAX_ASSOCS_TOTAL=1024` 的字节预算化（`relay/mod.rs:61/247/939`） | 中继数据面资源治理；M1 明文「200pps/句柄预算与隧道包尺寸**须实测复核**」 | **M1**（预算复核点；复核时裁决「计数上限 vs 字节预算」）。**若裁决 = 字节预算化 ⇒ 属 relay 代码改动（与「中继零改动」冲突），须 M1 显式扩范围或另立期** | 否——中继**零改动**，assoc/套接字面在 QUIC 下原样存在 |
| Q2 | 中继**客户端方向 v6**（主监听口 v4-only；`relay_cli.rs:103-105` + `relay/mod.rs:388-403`） | 中继主 socket 族；QUIC 客户端经中继（v6 候选） | **M1（须显式立条 + 范围确认）**——M1 中继条无 v6 字样、修它违反「中继零改动」⇒ 由 QUIC 程序/主会话二选一：①扩范围承接双栈；②退回「登记差异（不做）」 | 否（同 Q1） |
| Q3 | `public_endpoint.txt` 两处**写失败静默**（`engine.rs:1476/1630`；Go `publicendpoint.go:126/226` 有告警） | 公共端点公布面（本批派单「公共端点不在本批修」；**注意：M5 不会删/重写它**） | **M1（须显式立条）**；退路 = 主会话裁决把 2 行告警拉回任一批次 | 否 |
| Q4 | `listen_port.txt` 写失败 **Rust 致命 / Go 非致命**（`engine.rs:418` `?`；Go `role.go:95` 告警继续） | 出口实际端口落盘 = 端点/token 面（同上「不在本批修」条款） | **M1（须显式立条）** | 否 |
| Q5 | `--public-endpoint` **CLI flag 值域零校验**（`serve_cli.rs:694-695`；Go 前端报错 `cli.go:98-105`、守护期告警清空 `serve.go:113-121`） | 公共端点取值面（非法值会原样进 token） | **M1/M2（须显式立条）**（端点面复核；token 口径随 M2 一并定稿；**2026-10-08 第 2 棒回填「须显式立条」，代码门 低6**——原漏标） | 否 |
| Q6 | Q-B F3/F10 的 UDP 门/`udp_drop` 链 **e2e 未做**（`QB.md` §6-1） | M1 风险 #5 = DATAGRAM 丢包与全局代理 TCP 的背压路径变更，正是该测试面 | **M1**（背压专项测试一并） | 部分——intercept 保留，但入口换成 DATAGRAM |
| Q7 | 性能残余（M5 删除面）：`wtransport/bind.rs:287`、`server/bind.rs:757`、`server/bind.rs:1020`（`tx_drain_rounds` 每轮 `msgs` Vec） | 三处全在 M5 删除清单内 | **M5**（无动作，随删码消失） | **是** |
| Q8 | `reactor_turn→poll(0)` 每拍固定税（`server/intercept/mod.rs:2211`） | **不是** WG 专属：intercept **保留**（`QUIC-ROADMAP.md` 附录 B「保留」行——按符号引用；2026-10-08 第 2 棒校正，代码门复核轮 低3：原引 `:422` 在落档后已成空行），该税不会随 M5 消失 | **M1（复测后重定动作；不得按「M5 消失」处理）** | **否**（设计门 Q-1 订正） |
| Q9 | `wgcore` 站点 Engine 级测试空档（`QI.md:194`） | `wgcore` 整块删除 | **M5**（无动作） | **是** |
| Q10 | `PERF-AB` §9.15.6 四条（`PERF-AB.md:827-839`）：E13 `--loopback-only` token / 发送线程默认 on 重开条件 / 8s 口径 / Linux `sendmmsg`+五跳管线合一 | 全部是旧 WG 路径的发送管线/端点竞速性能项 | **M1/M5**：端点竞速 ⇒ M1 重定；发送线程/管线/`sendmmsg` ⇒ M5 删码即消失；「8s」为矩阵口径（非缺口） | **是**（除端点竞速条） |
| Q11 | `files --host` 失败（N1）：**仅当**第 2 棒复验判定根因在承载面（`wgcore`/`session`/`TunnelConn`） | 客户端承载 = M3 重写对象（`bridge_host` DialFn 改 STREAM） | **M3**（服务流迁移期复核；M3 判据含「App 真机 files 全绿」）。**后续（2026-10-08 第 2 棒 L7 复验，代码门 中1）：条件不成立 ⇒ 闭合（非缺陷），M3 无需承接**（`files --host` 默认形态全绿；失败仅 `--rate-limit 0` 的盲节流边界，Go 同形；证据 = `docs/reviews/QL.md` §N1 / `QUIC-ROADMAP.md` M1 前置 Q11 行） | 是——旧承载路径删除；若根因在**控制面**（daemon carriers/stream.open）则不消失，需另立批次。**后续：根因判定为「无缺陷（harness 用法）」⇒ 本格条件化结论作废，登记知悉即可** |
| Q12 | `tx_frag` / 分片感知的跨报文乱序 E2E（N18） | intercept 保留面（非耦合），仅因 M1 若重测分片可顺带 | **M1（可选，不作为立条）** | 否 |

> **明确不进交接清单**（同样判死，避免 QUIC 程序误领）：A5 `relay.listen` 接受集、
> N5 中继遥测通道、N11 中继状态面字段、N9-③ `dnsface` 两处小 `Vec`——前者们都在「中继零改动」
> 的不动面上，属**登记差异/不做**；`dnsface` 属 intercept 保留面的**量小**项，登记不做。

---

## 4. B 类小项修复清单（第 2 棒实现规格）

### L1（做）：`SessionVt` 错误类型 thiserror 收敛

- **文件**：`crates/homeway-core/src/term/vt.rs`。
- **方案**：
  ```rust
  /// 会话 vt 构造/改尺寸错误（仓规第 1 条：thiserror 类型；Display 文本与收敛前**逐字相同**
  /// ——`service.rs` 把 `{e}` 拼进会话日志行）。
  #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
  pub enum VtError {
      #[error("vt: 尺寸非法 {cols}x{rows}（须为 1..={}×1..={}）", Size::MAX_COLS, Size::MAX_ROWS)]
      BadSize { cols: u16, rows: u16 },
  }
  ```
  `new -> Result<Self, VtError>`（`vt.rs:300`）、`resize -> Result<(), VtError>`（`vt.rs:437`）；
  构造点 `Err(VtError::BadSize { cols, rows })`。**不改** Display 文本（现串 `vt.rs:303` / `:440`）。
- **调用面（实测）**：生产 2 处——`service.rs:1673`（`Match Err(e)` → `{e}` 不变）、
  `service.rs:3239`（`let _ = vt.resize(...)` 结果丢弃，类型变更无感）；测试面 ≥40 处
  （`vt.rs` 内测 36 + `codec.rs`/`keyenc.rs`/`responder.rs`/`tests/fuzz_replay.rs` 8），
  全为 `unwrap/expect` ⇒ 改名后照样编译。`thiserror` 已在 `homeway-core/Cargo.toml:8`。
- **测试**：既有 `vt_size_gate_rejects_oversize_and_zero`（`vt.rs:1904-1907`）覆盖行为；
  **新增**一条 Display 逐字断言（`VtError::BadSize{cols:0,rows:0}.to_string()` == 旧串），钉住日志行不变。
- **风险**：低（类型收敛，行为零变化）。
- **判据行影响**：**无**（错误文本不变、无 wire/CLI 变化）；`INTEROP-CRITERIA` 不登记。

### L2（做）：`?47h/?1047h` 备用屏内容面

- **文件**：`crates/homeway-core/src/term/vt.rs`（`set_private_mode` 的 `Unknown` 分支
  `:1400-1411`；`unset_private_mode` 的 `Unknown` 分支 `:1463-1469`）。
  **交付物含**：改写 `:1404-1407` 的陈旧注释（现文「alacritty 无公开 swap-alt API」不成立）。
- **语义真源**（ghostty `Terminal.zig:4800-4869`，逐条）：`47` = 切屏、**不清屏**、进出双向拷光标；
  `1047` = 同 47 + **退出时（在 alt 上）先 `eraseDisplay(complete)`** 再切回；`1049` = 进入保存光标
  + 清屏、退出恢复（现状已由 alacritty `Named` 分支覆盖，不动）。
- **实现（按设计门 B-1/B-2 订正后的形态）**：
  ```rust
  // set_private_mode（Unknown 分支）：进入
  47 | 1047 => {
      if !self.term.mode().contains(TermMode::ALT_SCREEN) {
          // ① 保主屏 DECSC 槽（alacritty swap_alt 会覆盖 grid.saved_cursor——
          //    那是 1049 的实现痕迹；ghostty 的 47/1047 从不 saveCursor）
          let saved = self.term.grid().saved_cursor.clone();
          self.term.swap_alt();                       // 公开 API（term/mod.rs:714）
          self.term.grid_mut().saved_cursor = saved;  // 还原，对齐 ghostty
      }
  }
  // unset_private_mode（Unknown 分支）：退出
  47 | 1047 => {
      if self.term.mode().contains(TermMode::ALT_SCREEN) {
          // ② ghostty：退出时把 alt 的**整个光标**（含 SGR 样式；ghostty cursorCopy 除 hyperlink）
          //    拷回 primary（两个方向都拷）——不能只拷 point
          let c = self.term.grid().cursor.clone();
          self.term.swap_alt();
          self.term.grid_mut().cursor = c;
      }
  }
  ```
  `dec_modes.set(n, true/false)` 既有记账**不动**（DECRQM 报告面）；`scan.rs:159` 的
  `ALT_SCREEN` 位**不动**；surface `screen` 字段（`vt.rs:587/774`）自动跟随 `TermMode::ALT_SCREEN`。
  幂等：连续 `47h` 不二次切换（guard）；不在 alt 时 `47l` no-op（guard）。
- **已知差异（如实登记，共 2 条）**：
  1. alacritty `swap_alt` 在**进入时**复位 alt 内容（`term/mod.rs:723` `reset_region`）⇒「47 重入
     （47h→47l→47h）时 ghostty 保留旧内容、本实现清空」；1047 退出清屏以「下次进入复位」等价覆盖
     可观察面——**该等价性依赖本条差异**（若上游复位行为变化则等价失效；ghostty 的 ED2 另清
     kitty 图像，本仓无对应物）；
  2. 其余光标细节按 B-1/B-2 订正后已对齐（`saved_cursor` 还原、整光标拷贝）；若实现时发现
     `Cursor` 含 alacritty 专有态无法整体赋值 ⇒ 记录实做形态并补登记。

  **后续（2026-10-08 第 2 棒实现 + 代码门，追加式）**：实登记共 **4 条**——① 保留（重入清空）；
  ② 订正为「已对齐，无该形态」（整 `Cursor` 可整体赋值）；新增 **③ 混合形态终点**
  （`1049h→47l→1049l` 落 D-15 分支，代码门低3）与 **④ grow 位移丢失**（捕获槽跨 resize 放大
  少 `from_history` 位移；不 panic、位置在界内；代码门复核轮 中）——③④ 均落
  `INTEROP-CRITERIA`「已知口径注记」Q-L 条（**以该处为验收真源**）。另：还原捕获槽前**按当前
  尺寸钳制**（代码门高1 整改，`restore_alt47_saved_decsc`；ghostty `restoreCursor` 同为钳制口径）。
- **测试（新增）**：
  1. `47h` → `mode` 含 `ALT_SCREEN` + 写入落 alt + primary 内容原样；
  2. `47l` → 回 primary + primary 内容保留 + **光标（含样式）** = alt 末位（ghostty 口径）；
  3. 幂等：连续 `47h` 不二次切换；不在 alt 时 `47l` no-op；
  4. `47h` **不覆盖主屏 DECSC 槽**（`saved_cursor` 还原的钉子测试：先 `ESC 7` 定位主屏 saved_cursor，
     再 `47h`/`47l`，`ESC 8` 应回到原位置）；
  5. `1047h/l` 同 1/2 + 重入清空（登记差异的钉子）；
  6. 回归：`vt_plain_text_alternate_screen`、`session-styles` golden（1049 路径）不变。
- **风险**：中低（只影响发 47/1047 的遗留 curses/vi 系程序；1049 不受影响）。
- **判据行影响**：**有**（行为面）⇒ `INTEROP-CRITERIA`「判据变更记录」**新增一行**（Q-L，追加式）：
  备用屏族 47/1047 从「屏不切换（内容留主屏）」→「真切换 + 光标双向整体拷贝 + 主屏 DECSC 槽不动；
  1047 退出清屏以进入复位等价；2 条与 ghostty 的登记差异」；fixtures **不动**
  （`surface_codec.json` 不含 47/1047/1049，已 grep 证）；「计数输入集/数值语义变化」表**不涉及**。

### L3（做）：`--bind-interface=` 空值 carve-out（第五个）+ 枚举形态 trim/lowercase

- **文件**：`crates/homeway-cli/src/serve_cli.rs`
  - flag 站点 `:457-458`：`take_value_or_exit(...)` → `cli_flags::take_value_empty_ok_or_exit("bind-interface", inline, next)`；
  - `parse_bind_iface`（`:738-749`）改为：
    ```rust
    fn parse_bind_iface(v: &str) -> BindMode {
        let t = v.trim();
        match t.to_ascii_lowercase().as_str() {
            "" | "auto" => BindMode::Auto,          // Go ResolveBind("")⇒BindAuto（TrimSpace+ToLower）
            "none" | "off" | "no" => BindMode::Off,
            _ => match t.parse::<std::net::IpAddr>() {
                Ok(ip) => BindMode::Addr(ip),
                Err(_) => BindMode::Explicit(t.to_owned()),
            },
        }
    }
    ```
    （设计门 B-3：`validate_bind_interface` 本就 trim+lowercase 判关键词，而 `parse_bind_iface`
    原为精确匹配 ⇒ `" AUTO "`/`--bind-interface AUTO` 会漂到 `Explicit(" AUTO ")` 打伪告警。
    关键词判定用 lower，**网卡名保原大小写**（`t` 仅去首尾空白）——与 Go `ResolveBind` 的
    `InterfaceByName(v)` 同形。）
- **为什么做**：① 纯取值纪律（与 Q-I 尾段 F0 同族）；② 顺带修掉 config `bind_interface=""`
  与大小写/空白形态的伪告警路径；③ Go 语义明确、无歧义。
- **测试**：`parse_serve_flags(["--bind-interface="])` ⇒ `BindMode::Auto`；
  `serve_config_of` 对 `bind_interface = ""` 同断言；`parse_bind_iface(" AUTO ")`/`"NONE"` ⇒
  `Auto`/`Off`；`parse_bind_iface("en0")`/`"en0 "` ⇒ `Explicit("en0")`（去空白保大小写）；
  扩 `take_value_empty_ok_or_exit` 边界测试表。
- **风险**：低（此前空值 = exit 2，现按 Go 落 auto；枚举形态此前打伪告警，现不告警）。
- **判据行影响**：**有（登记，追加式）**⇒ 按 `INTEROP-CRITERIA.md:534-536` 政策「登记条目一经
  写入不修改（可追加「后续」说明）」：**新增一行**（2026-10-08 Q-L：取值 flag 空值 carve-out 四→五）
  + 在 Q-I 尾段原行**追加**「后续：Q-L 已扩 `--bind-interface=`」；并按既有先例在
  「判据变更记录」收束段（`:608-615`）**追加**一段 Q-L 段注，宣告 `--bind-interface=` 已进 carve-out
  （该段现有「`--bind-interface=` 空值形态**不在** carve-out」句由此**追加式**失效声明）。

### L4（做）：`public_endpoint.txt` 写失败告警 —— **不做（转 QUIC M1）**

> 属「公共端点」面，按批派单不修；已进 §3-Q3（须显式立条）。此处仅记录「为何不顺手做」：
> 它不是纯取值纪律——含出口启停/公布面语义（`listen_port.txt` 那条更是启停行为差异），
> 归 QUIC M1 端点面一次性收口。

### L5（不做，登记）：`encode_frame`/`enc_hello` 长度域

- **不做理由**：仓内生产者双侧 ≤64（§1-A4 证据）；三条大帧出口全有界（DATA 16KiB / 快照 60KiB /
  剪贴板钉上限）；Go 同形（`encHelloFlags`，`frames.go:222-230`）；无协议批存在。
- **可选零行为加固**（第 2 棒裁量）：在 `enc_hello`（`frames.rs:306-313`，nameLen 写点在 `:311`）
  加一行注释指路不可达。不加 `debug_assert`（收益为零、测试面误炸风险）。

### L6（不做，登记）：`relay.listen` / 遗留大写条目 / 双解析器 / `interfaces()` Logf / `plain_text` 出锁 等

- 见 §1-A5、§2-N2/N3/N4/N10；逐条理由与落点已在表内。

### L7（第 2 棒时间盒复验）：`files --host`

- **动作**：`tools/local-rust-exit.sh` 起私有出口（或统一进程 + daemon）→ `homeway-cli host add`
  → `files --host <ref> ls`；复现「流已终结（gone）」即取证成功。
- **分档**：根因在 `wgcore/session/TunnelConn`（承载面）⇒ 写进 §3-Q11（转 M3）；
  根因在 `daemon`（carriers/`stream.open` 适配面）⇒ **本批不修**，登记 `QL.md` 移交 + 上报主会话定批。
- **时间盒**：1 小时；到点未定因 ⇒ 只落「复现成立 + 未定因」并上报（不许静默）。

---

## 5. 台账刷新方案（第 2 棒落笔，主会话收口核对）

> 落笔机制沿用 Q-A 先例：**在「Rust 现状」格内追加 dated 状态**（P0/P1 表无独立「状态」列），
> **行内正文历史注记不改**（★设计门 P-4/P-6 订正：不整行重写，误判以「❌ 审计误判（Q-L 剔除）」标注）。

### 5.1 `docs/GAP-AUDIT.md`

1. 顶部**新增**一段 dated 块（仿 Q-A 先例）：
   > **2026-10-08 Q-L 批账实修正（本批）**：P0-1 余项与 P1-4/P1-7 状态按 Q-B…Q-K 交付追加回填；
   > P1-8 在「Rust 现状」格追加逐子项结论（3 已实现 + 1 条审计误判剔除 + 3 条真缺转 QUIC M1）；
   > P2-4/P2-5/P2-7 追加状态；K 表 K-15 状态注。行内正文历史注记不改。
2. **P0-1 行**（「Rust 现状」/「归属」格追加）：**全清**——余项 r1-N1/L7 已由 Q-H F15
   （前台 serve/relay 默认 state 对齐）落地；Q-J/Q-K 无残留。
3. **P1-4 行**：追加 **已清（Q-H F17）**：C14「出口能力」行实装（证据 = `QH.md` F17 行 +
   `c14_probe_target` 纯函数 + 4 单测）。
4. **P1-7 行**：追加 **已清**（动词别名 B0-2a；`--host` 远程模式 B0-2b/D-1）；**加注**：
   Q-I 尾段实测 daemon 托管形态失败（`QIt.md` §7.2-4）⇒ 运行时缺陷由 Q-L N1 接管
   （见 `QL-design.md` §2-N1）。
5. **P1-8 行**：「Rust 现状」格**追加**：
   > （2026-10-08 Q-L）**3 类已实现（Go 同串）**：端点不一致仲裁 `engine.rs:1550-1558`、
   > `--public-endpoint` 公布分支 `:1472-1488`、`--relay 解析失败跳过注册` `:554-560`、
   > `--bind-interface 找不到→退回 auto` `:309-338`；**❌ 审计误判（Q-L 剔除）**：
   > 「Rust Explicit 网卡名不验存在性」不成立（engine 验存在性 + 告警回退，与 Go 同口径）。
   > 真缺 3 条（`public_endpoint.txt` 写失败告警 ×2 / `listen_port.txt` 写失败致命 vs Go 非致命 /
   > `--public-endpoint` flag 值域校验）⇒ **转 QUIC M1（须显式立条）**。
6. **P2-4/P2-5 行**：P2-4 → 追加已清（term CLI 标题 OSC 2 `term_cli.rs:1430` +
   `TERM_SESSION_ID` 回环检测 `:942-944`，随 P1-3/B0-2b）；P2-5 → 追加不适用/等价面
   （`status --watch` 已交付 B0-2b；`HOMEWAY_LIVE_*` 是测试注入缝，Rust 自有测试体系）。
7. **P2-7 行**：追加「值域表 = Go `validateFile` 全量（Q-H F1）；**已知残余**：`peer_ttl`
   接受集窄于 Go（`QH.md` §5.2-10）+ 报错无行号 + 写回无 CAS（§5.2-14）」
   （★设计门 P-3 订正：不得写成「只剩报错无行号」）。
8. **K 表**（★设计门 P-5 新增）：K-15 追加状态注「47 备用屏内容面已由 Q-L L2 实现，残余 =
   重入清空差异（alacritty 语义）」；K-20 复核注「R8 后各批已收官，剩余 = 用户触点（pin/终测定稿）」。
9. P2-2/P2-9 等：不动（Go 结构差异理由仍成立）。

### 5.2 `docs/REVIEW-ROADMAP.md`

- 新增 **Q-L 行**（主会话收口时写）：对账结论摘要 + 三条 B 类落地 + 交接清单指针；
  状态/设计门/代码门/记录四列按既有格式。
- 「已知 flake 登记」表：本批不动（无新 flake；若第 2 棒复跑见新红按三证口径甄别）。

### 5.3 `docs/INTEROP-CRITERIA.md`

- 「判据变更记录」：**+2 新增行**（L2 备用屏族 47/1047；L3 carve-out 四→五 flag）+
  Q-I 尾段原行**追加「后续」说明**（**不修改原条目**——政策 `INTEROP-CRITERIA.md:534-536`）+
  在收束段（`:608-615`）**追加** Q-L 段注（宣告 `--bind-interface=` 已进 carve-out）。
- 「计数输入集/数值语义变化」：无。
- 「已知口径注记」：+1 条（47 重入清空的 alacritty 差异 + 1047 等价的依赖前提），随 L2 一并写。

### 5.4 `docs/QUIC-ROADMAP.md`（可选，**需谨慎**）

- 只建议在**附录 D（风险与未决）**加一行指针：「Q-L 交接清单 = `docs/reviews/QL-design.md` §3」
  **并点名 Q2/Q3/Q4/Q5 四条「须显式立条」**；**不改** M0–M7 正文（QUIC 程序自有协议）。
- **后续（2026-10-08 第 2 棒实际落档 + 代码门 中2/低7，追加式）**：实际落档**超出本节的「只加附录 D
  指针」**——按批派单 ③ 的明文要求（「必须让 QUIC 程序在开期时必然看到」）写进 QUIC-ROADMAP 的
  **期前置**：M0 节末新小节 → 按代码门 中2 订正为「**M1 开工前置检查项（Q-L 交接）**」并**移入 M1
  节末**（收件人 = M1：M0 已在 QUIC 程序侧完成，main 副本状态待其合回更新）+ 当前指针第 2/3 条 +
  附录 B 补 `udpbatch.rs`（代码门 低5）+ 附录 D 第 8/9 行。理由 = 派单要求「必然看到」且设计门
  Q-3 要求把 Q2–Q5 钉死；锚点 = `QL-design.md` §3 + `QL.md` §QUIC 交接（后者随本批 commit 落库）。

### 5.5 收口记录

- `docs/reviews/QL.md`（第 2 棒写）：对账表落地结果 + B 类实现注记 + 判据登记索引 +
  本批**不做项清单**（含理由）+ N1 复验结果 + 设计门/代码门记录。

---

## 6. 测试与验证计划（第 2 棒）

| 项 | 验证 |
|---|---|
| L1 | `cargo test -p homeway-core term` 全绿；新增 Display 逐字断言 |
| L2 | 新增 6 组 vt 用例（§4-L2）+ `term::` 面全绿 + `session-styles`/`surface_codec` 既有向量不回归 |
| L3 | `homeway-cli` 单测（flag/config 两形态 + 枚举形态 trim/lower）+ `take_value_empty_ok_or_exit` 边界表 |
| 全局 | `cargo test --workspace`（隔离复跑在册 flake）+ `cargo clippy --workspace --all-targets -D warnings` clean + `tools/check-vocab.sh` PASS + OHOS 交叉 `cargo check` |
| 判据 | `INTEROP-CRITERIA` 两处登记**与代码同批 commit**（AGENTS 硬规则 4） |
| 隔离 | 全程只用本地私有实例；`git status` 复核；dsh 副作用文件转 `/tmp` |

---

## 7. 风险与边界

1. **L2 语义近似**：alacritty 公开 API 无法完全复刻 ghostty 的 47「不清屏」（进入即复位）；
   已登记 2 条差异（重入清空 / 等价性依赖），不宣称字节级等价（fixtures 无覆盖，故不改向量）；
2. **A1/Q1 量级证据边界**：8GB 是「1024 条已认证会话 × 内核不钳制」的理论上界；本棒给出
   内核钳制与前置条件的量级辨析（≈0.4GB 级），**不以此判死**，交 M1 用实测裁决；
3. **N1 未定因风险**：`files --host` 复验可能超时间盒 ⇒ 只落取证与分层结论，不猜根因；
4. **Q2/Q3/Q4/Q5 的「须显式立条」**：若 QUIC 程序不显式认领，主会话须在 **M1** 开工前置里裁决（**2026-10-08 第 2 棒订正，代码门 中2**：M0 已由 QUIC 程序完成 ⇒ 收件人 = M1；落档见 `QUIC-ROADMAP.md`「M1 开工前置检查项（Q-L 交接）」）
   （否则本批的交接会退化成新一轮挂空——这正是本批要消灭的形态）；
5. **不越界**：本棒不改产品代码、不碰生产出口 / 只读两仓 / `baseline/` / `tools/tailcat/homeway-rs.pin`；
   不发 tag / Release / PR。

---

## 8. 设计门（dsh 外部评审）记录

### 8.1 轮次档案

| 项 | 值 |
|---|---|
| 轮次目录 | `/tmp/dsh-review/r30.7NT61o/`（`prompt.txt` / `output.md` 99 行 / `stderr.log`） |
| 命令 | `cd /Users/zhaozhe/Documents/projects/homeway-rs && dsh --profile headless "$(cat …/prompt.txt)" > …/output.md 2> …/stderr.log; echo "exit=$?"`（**前台捕获**） |
| **exit code** | **`exit=0`** |
| 评审者自查 | `git status` 与本批开始时逐行一致（仅未跟踪的 `docs/reviews/QL-design.md`）；未修改仓库任何文件 |
| 结果规模 | **1 高 / 10 中 / 13 低**（共 24 条：①8 ②7 ③6 ④5 ⑤6，含跨节重复点名的同源条目） |
| 门结论（原文摘要） | 「证据密度与自我纠错（A2 翻案、A7 剔除审计误判）是这份设计文档的强项，**but**：§2 漏收了 AUDIT 里唯一一条明写『归后续批』的 term/keyenc 性能项（高）…上述**均可低成本就地修正，不改变本批『治理批』的总体方向**」 |

### 8.2 逐条处置表

| 编号 | 严重 | 意见（摘要） | 处置 | 落点 |
|---|---|---|---|---|
| E1 | 中 | N6/§3-Q2 引 `relay/mod.rs:379` 错误（那是 `frame_scratch`）；真链 = `relay_cli.rs:103-105` + `listen_with_fallback`；结论（v4-only）仍成立 | **认同已改**：引 `relay_cli.rs:103-105` + `relay/mod.rs:388-403`；补 Go `relay.go:289-319` | §2-N6 / §3-Q2 |
| E2 | 低 | A4/L5 引 Go `frames.go:230-243`（实为 decHello）与 `frames.rs:306`（签名非写点） | **认同已改**：Go `frames.go:222-230`（`encHelloFlags`）、Rust `frames.rs:306-313`（写点 `:311`） | §1-A4 / §4-L5 |
| E3 | 低 | L1 调用面清单不全（漏 `service.rs:3239`；测试面 ≥15 处含 `tests/fuzz_replay.rs`） | **认同已改**：补生产 2 处 + 测试面 ≥40 处（vt.rs 36 + 跨文件 8） | §1-A3 / §4-L1 |
| E4 | 低 | `engine.rs:316-345`→`:309/315-338`；Go `role.go:95`；Go 告警在 `cli.go:201-207` | **认同已改**（逐处校正） | §1-A6 / §1.1 |
| E5 | 低 | `daemon/client.rs:42` 只是模板；`gone` 真源 = `vocab.rs:238` | **认同已改** | §2-N1 |
| E6 | 低 | 「两台生产出口…」阿里云未检 | **认同已改**：改「Mac 生产出口与本机默认 state 已核；阿里云未检」 | §1-A5 |
| E7 | 低 | A4 ② 论据面偏窄（只举 DATA） | **认同已改**：扩三条大帧出口（DATA 16KiB / 快照 60KiB / 剪贴板钉上限 `codec.rs:900`） | §1-A4 |
| E8 | 低 | N2 的「影响面 = 只有…state」是推断，无一手取证 | **认同已改**：改为「存量 state 不可枚举（本批不碰生产 state），按最坏影响登记」 | §2-N2 |
| **M1** | **高** | **漏收**：`AUDIT-2026-10-07.md` §Q-I 的 `[P2 🔎]` 行末「term/keyenc 面…**归后续批**」——唯一明写「归后续批」却无人认领 | **认同已改**：新增 **N15**，判「不做（登记）+ 理由（无实测驱动/热路径语义风险）」，并列入「将来有 profile 靶点再开」候选 | §2-N15 |
| M2 | 中 | `QI.md:189` `tx_drain_rounds` 每轮 `msgs` Vec 漏收 | **认同已改**：并入 N9-①/Q7（`server/bind.rs:1020` ⇒ M5 删码即消失） | §2-N9 / §3-Q7 |
| M3 | 中 | `QFB.md:198` §6-3「残留第三份泵拷贝另行登记」至今无落点 | **认同已改**：新增 **N16**，判「不做（登记）」（CLI 测试动词，D8 裁决） | §2-N16 |
| M4 | 中 | §2.3 判据句与自身成员矛盾、组清单不全 | **认同已改**：§2.2 改分组式（10 组逐组给处置指针），§2.3 只留 PERF-AB/ROADMAP/QI-DNS | §2.2 / §2.3 |
| M5 | 低 | 其余挂空候选：`QI.md:194` wgcore 测试空档 / `QK.md:129` 乱序 E2E / `QC.md` §5 F1 进程级 E2E | **认同已改**：分别落 **N17 / N18 / N19**（转 M5 消失 / 不做登记 / 不做登记） | §2 |
| M6 | 低 | N13 出处应为 `QG.md` §6.3 | **认同已改** | §2-N13 |
| M7 | 低 | §2.3 汇总名单缺 QH §5.2 / QF §6.3 / QG §6.2 / QI §6 / QIt §7.3 | **认同已改**：并入 §2.2 分组表（10 组） | §2.2 |
| Q-1 | 中 | §3-Q8「M5 自然消失」判错——`reactor_turn` 在 intercept（保留面） | **认同已改**：Q8 改「**否**（不得按 M5 消失处理），M1 复测后重定动作」 | §3-Q8 |
| Q-2 | 中 | §3-Q2 过度引用 M1（无 v6 字样）+ 与「中继零改动」冲突未声明（与 A5/N5/N11 的排除标准不一致） | **认同已改**：Q2 标「须显式立条 + 范围确认」，二选一（扩范围承接双栈 / 退回登记差异）写明；与 A5/N5/N11 的差别（数据面 vs 配置面）写明 | §0.3 / §3-Q2 |
| Q-3 | 中 | Q3/Q4/Q5 属「硬转」（M1 范围无 bullet；三者都不会被 M5 删） | **部分认同**：**不把 Q3/Q4/Q5 拉回本批修**（批派单明文「公共端点…不要在本批修」是上级条款，且 Q4 触出口启停语义——超出「日志文本」）——但按意见把交接**钉死**：§0.3 说明「不会被 M5 删除 ⇒ 交接必须显式立条」+ §3 三条标「**须显式立条**」+ 退路 = 主会话裁决拉回；§7-4 把「不接则退化挂空」列为风险 | §0.3 / §3-Q3/Q4/Q5 / §7-4 |
| Q-4 | 中低 | Q1 裁决结果可能要改 relay 代码，冲突未声明 | **认同已改**：Q1 补「若裁决=字节预算化 ⇒ 须 M1 显式扩范围或另立期」 | §3-Q1 |
| Q-5 | 中低 | §2.2 对 `PERF-AB` 挂账的断言无证据（§9.15.6 四条未 triage） | **认同已改**：新增 **N20** + §2.3 改写 | §2-N20 / §2.3 |
| Q-6 | 低 | §3-Q9 是自我指涉空行 | **认同已改**：删除，原 Q9 顺延为 wgcore 测试空档（N17），PERF-AB 条目为 Q10 | §3 |
| B-1 | 中 | L2 只拷 `cursor.point`，ghostty 拷整个 Cursor（含样式） | **认同已改**：改 `let c = grid().cursor.clone()` → 整体赋值；测试 2 补「光标样式随行」 | §4-L2 |
| B-2 | 中 | 漏登记：alacritty `swap_alt` 进入时覆盖主屏 `saved_cursor`（ghostty 47/1047 从不 saveCursor） | **认同已改**：实现改为「进入前捕获 `saved_cursor`、退出后还原」+ 钉子测试；登记差异条数订正为 2 条 | §4-L2（测试 4）/ §7-1 |
| B-3 | 中 | L3 只修空值；`" AUTO "`/`AUTO` 的伪告警路径仍在（Go trim+lower） | **认同已改**：L3 扩为「trim+lowercase 判关键词、网卡名保原样」+ 测试 | §4-L3 |
| B-4 | 低 | 未把「改写 `vt.rs:1405` 陈旧注释」列为交付物；K-15 需状态注 | **认同已改**：L2 交付物补注释改写；§5.1 补 K-15 | §4-L2 / §5.1-8 |
| B-5 | 低 | 1047 等价性依赖「alacritty 进入即复位」这条差异；ghostty ED2 另清 kitty 图像 | **认同已改**：等价性论证写明依赖；差异条 1 补 kitty 图像面 | §4-L2 |
| P-1 | 中 | L3 的登记方式违反 `INTEROP-CRITERIA.md:534-536`「一经写入不修改」；且 §4-L3「更新那行」与 §5.3「+2 行」自相矛盾 | **认同已改**：统一为「新增一行 + 原行追加『后续』说明 + 收束段追加段注」 | §4-L3 / §5.3 |
| P-2 | 中 | 收束段（`:608-615`）「`--bind-interface=` 不在 carve-out」将变假陈述，未列入改动面 | **认同已改**：§5.3 明确追加 Q-L 段注 | §5.3 |
| P-3 | 中 | §5.1-7 的 P2-7 措辞与 `QH.md` §5.2-10（`peer_ttl` 窄于 Go）矛盾 | **认同已改**：改写为「值域表全量 + 已知残余（peer_ttl 接受集/无行号/无 CAS）」 | §5.1-7 |
| P-4 | 低 | §5.1-1「历史注记不改」vs §5.1-5「整行重写」自相矛盾 | **认同已改**：P1-8 改「在『Rust 现状』格追加」，误判标剔除不抹历史 | §5 前言 / §5.1-5 |
| P-5 | 中低 | §5.1 未触及 K 表（K-15 失义、K-20 陈旧） | **认同已改**：§5.1-8 补 K-15/K-20 | §5.1-8 |
| P-6 | 低 | P0/P1 表无「状态」列，落笔机制未定义 | **认同已改**：§5 前言明确「写入『Rust 现状』格 + dated」 | §5 前言 |

**处置汇总**：**23 条认同并已改**（含 M1 高危、Q-1/Q-2/B-1/B-2/B-3/P-1/P-2/P-3 等）；
**1 条部分认同**（Q-3：保留转 QUIC 不拉回本批，但按其意见把「显式立条 + 退路 + 风险」钉死）；
**0 条不认同**；评审者「看过，没发现问题」的五组（①A1/A2/A7/§2.1；③Q6/Q7/Q10/排除清单；
④L1/L2 可行性/L3 前提；⑤L1 判据无影响/L2·L3 判据有影响/§5.2·§5.4 分寸/边界纪律）**全部保留**。

### 8.3 过门结论

**通过**（exit=0；唯一高危 = M1 漏项，已就地补为 N15 并判死；无结构性反对，无「需重做设计」
意见；评审结论原文亦为「均可低成本就地修正，不改变本批『治理批』的总体方向」）。
本设计文档已按处置表全量回填（= 本节所在版本）。
