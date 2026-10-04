# R6 门一（技术评审）独立评审记录 — 6a 设计 + 6b vt 底座

> 评审对象（用户指定）：`git log 82cf579^..HEAD` 三 commit = **82cf579**（6a 技术设计）、
> **d5d8fba**（6b 依赖接入）、**2aa98bb**（6b vt 底座）。
> 评审执行时工作树 HEAD 已被**并行工作流**推进到 `93c29d7`（264fa70 6c 向量先行 → 573700a 设计回写 →
> 93c29d7 调色板全表）。本文以 `2aa98bb` 为准，**每条发现标注在 HEAD 上是否已消解**。
> 评审方式：原文对读 + 源码核实（alacritty_terminal 0.26.0 / vte 0.15.0 registry 源码、
> libghostty-vt 1.3.2-dev 树内源码）+ **实跑验证**（cargo 测试、临时 Rust probe、Go 真源 cgo probe、
> 复用 264fa70 产出的对照向量）。所有实测证据在 §附录A 复现路径里。

## 0. 结论摘要

**门一结论：6b 底座有 3 个高危实现缺陷 + 1 个高危语义缺口，必须先修再进 6e/6f；6a 设计可作为
6c 起点，但需按本文补 6 处落点。** 6c 已由并行工作流先行并产出了真源向量（质量好），
但**其编码器规格仍缺 ghostty「last-set 单值」语义**（D-17 的结论方向对、理由错），不修会在
鼠标/键编码上产出与 Go 不同的字节。

| 严重度 | 数量 | 条目 |
|---|---|---|
| 高（阻断 6e/6f） | 5 | B1 D-16 拦截丢全量脏标记；B2 绝对行号映射算错（debug 直接 panic）；B3 `Dirty::None` 不可达⇒空闲每拍发帧；B4 D-15 拦截不完整（原点模式/笔态）；C1 脏行契约零测试（B1/B3 的藏身处） |
| 高（6c 规格） | 1 | B5 D-17 编码面：ghostty `flags.mouse_event/format` 是 last-set 单值 |
| 中 | 17 | A1/A2/A4/A5/A6/A7/A8/A10(D-4,D-5,D-10) A11、B6/B7、C2/C3/C4、D1/D2 |
| 低 | 12 | A9、B8/B9/B10、C5..C9、D3..D6 |

---

## 一、设计完整性（必答①：五道必答题对 Go `pkg/term` 行为面的覆盖）

先说结论：**五道必答题的组织是对的**，字节级核心面（帧常量/mode 位/cell 编码/surface 体/错误码词表/
会话常量）逐值对过，无一处漂移（清单见 §六「看过且没问题」）。缺的是**四块整面**与**一族帧体布局**。

### A1 高｜manifest region 层整块无落点
- 位置：`baseline/homeway/pkg/term/manifest/region.go:1-448`（`Input{Screen,OSCTitle,OSCProgress}`
  :21-28；12 个具名 region + 3 个参数化 :31-178；prompt-marker 家族 :248-345；prompt-box 家族
  :348-448）vs `docs/reviews/R6-design.md:22`、`:275`（6a 全文 0 次「region」，`:275` 只写
  「喂 manifest.Input（screen=夹具文本）」）。
- 问题：实测 `grep -rho 'region = "…"' manifests/*.toml`：`osc_title` 19 条、`osc_progress` 6 条、
  `bottom_non_empty_lines(N)` 共 60+ 条、`after_last_prompt_marker` 2、`prompt_box_body` 1 …。
  即**检测引擎的判定范围选择器**（规格里「旧提示残留不误判」的关键）与四路证据里的 OSC 两路
  在设计中没有任何落点；`index.toml` 的 `processes` 选表入口（`load.go:161-183`）、
  `MinEngineVersion` 门（`load.go:134-143`）、`top_non_empty_lines ≥3 行` 约束
  （`manifest.go:205-209`）、Validate 耦合（`skip_state_update ⇒ state=unknown`，`manifest.go:192-201`）
  同样缺失。
- 建议：6f 设计增量补「region 全集 + Input 四路 + `str::lines()` 尾行口径（region.go:169-178）+
  index 选表 + 版本门 + Validate 耦合」。判据向量按 `region.go` 逐函数产。
- 严重度：**高**（§八 判据 3「检测三态同输入同输出」的前提面没被设计覆盖）。

### A2 高｜regex 方言结论错误：规则集不在两方言交集内
- 位置：`baseline/homeway/pkg/term/manifest/dialect.go:29-31`（`"Alphabetic": "L"` 近似映射）+ 包注释
  `:11-19`（自认「少了 Nl 与 Other_Alphabetic」）vs `R6-design.md:345-349`（「Go dialect 垫片保证了
  规则集已限定在两方言交集内，Rust 侧直接 `regex::Regex` 编译即可……**22 份 manifest 在 Rust regex
  下编译通过即证**」）。
- 问题：`\p{Alphabetic}` 真的在用：`manifests/antigravity.toml:25`、`cursor.toml:57`、
  `qodercli.toml:38`。Rust regex 原生 `\p{Alphabetic}` ⊋ `\p{L}`（含 Nl/Other_Alphabetic），
  同一行文本两端判定可不同；「编译通过」只证语法、不证语义。设计自己给出的登记校验因此不成立。
- 建议：Rust 侧复用同款垫片（把 `\p{Alphabetic}`→`\p{L}`；`\uXXXX`/`\u{XXXX}` Rust 原生支持，无需翻译），
  并在 `term_manifest_eval.json` 里做逐规则 `region+matched` 对拍（而不是只对状态）。
- 严重度：**高**（6f 判据核心）。

### A3 中｜ENDED reason 词表缺 `service_stopped`
- 位置：`baseline/homeway/pkg/term/service.go:452`（`sess.finish(termEndServiceStopped, "service_stopped")`）
  vs `R6-design.md:297-299`（code −3 有，reason 硬词表只写 `{replaced, self_reconnect}`）。
- 问题：Go CLI 按词表渲染 ENDED 文案（`term_cli_attach.go:448-457`）；漏词会让出口退出时的
  归因落到「未知 reason」分支。
- 建议：词表补 `service_stopped` 并加一条判据（服务关停 → 全腿 ENDED −3/service_stopped）。
- 严重度：中。

### A4 中｜DIFF 体光标块大小写错
- 位置：`R6-design.md:253`（`[cursor 4B]`）vs `baseline/homeway/pkg/term/term_surface.go:386-389`
  （x:u16 + y:u16 + flags:u8 + shape:u8 = **6B**）。
- 建议：改 6B；SNAPSHOT 行（`:251`）建议同样标尺寸。
- 严重度：中（6d 按 4B 实现会整帧错位）。

### A5 中｜帧表/体布局缺落点（6d 无表可依）
- 位置：`R6-design.md:245-261`、`:319-322` vs `frames.go:29-58,391-464`、`term_surface_leg.go:46`、
  `service.go:1755`、`term_cli.go:744-776`。
- 缺口清单：op→字节表（设计只有「23 个 op」一句）；`ATTACHED`(0x09) `[cols:2][rows:2][modes:4][agent][state][name]`、
  `STATE`(0x07) `[agent][state][titleLen:2][title≤512]`、`ENDED`(0x03) `[code:4LE][reasonLen:1][reason≤200]`、
  `ERROR`(0x06) `[codeLen:1][code][msgLen:2][msg≤4096]`、`REPLAY-DONE`(0x0A) `[replayed:u32][flags:1]`、
  `SNAPSHOT-DONE`(0x0E)、`FETCH-SNAPSHOT`(0x15)、`RESIZE`(0x02)、`OK`(0x0B) 九种体布局一个都没写；
  字段截断上限全缺（`termMaxPayload=65535`、`termDataChunk=16KiB`、name≤64、reason≤200、
  title≤512、errmsg≤4096、clip 65532、notify 4096、clientID≤64）；`surfaceFetchRowsMax=512`；
  LIST 外壳 `{"sessions":[…]}`；EXPLAIN JSON 键表（Go CLI `json.Unmarshal` 缺键静默零值）。
- 建议：6d 设计增量补一张帧总表（`frames.go` 头部注释即可落地），判据用 `fixtures/term/frames.v1.jsonl`
  （注意该 fixture 缺 0x09 案，需补）。
- 严重度：中高（不是"漏一个字段"，是 6d 整步没有规格）。

### A6 中｜env 面 11 项未写 + 「唯一关闭方式」不成立
- 位置：`R6-design.md:307-325` vs `service.go:110-122`、`term_vt.go:30-32`、`service.go:1212`、
  `service.go:335-338`。
- 问题：未写的 exit 侧 env：`HOMEWAY_TERM_VT`（vt 逃生口）、`_DETECT`、`_PORT`、`_REPLAY`、
  `_REPLAY_EPOCH`(all|last)、`_MAX_SESSIONS`、`_MAX_CLIENTS`、`_WRITE_TIMEOUT_MS`、`_STALL_LIMIT_MS`、
  `_PENDING_CAP_BYTES`、`_QUEUE_BYTES`；CLI 侧 `HOMEWAY_TERM_TITLE`、`TERM_SESSION_ID`；
  子进程注入 `SHELL=<登录 shell>` 与 `TERM_PROGRAM_VERSION`。设计 `:324` 写「`HOMEWAY_TERM=off`
  全关（同 Go 唯一关闭方式）」——Go 还有一个**语义不同**的开关 `HOMEWAY_TERM_VT=off`（服务照开、
  surface 能力关闭，`term_vt.go:30-32` → `surfaceCapable()`）。
- 建议：补 env 总表 + 明确 Rust 侧对 `_VT=off` 的处理（Rust 无「无 vt 依赖」构建 ⇒ 需决定：
  等价降级为 legacy-only，还是声明不支持并登记）。
- 严重度：中。

### A7 中｜「全部应答走 ghostty 默认值」的前提不成立（HEAD 已改表、总结句仍在）
- 位置：`R6-design.md:130-131`（6a 原文）vs `third_party/libghostty-vt/include/ghostty/vt/terminal.h:88-105`
  （C API 有 14 个 effect 注入点）+ Go 实际只装 6 个（`vt.go:154`、`response.go:193/203`、
  `helpers.c:73-74`、userdata）。
- 问题：XTVERSION/size(14/16/18t)/title(21t)/ENQ 属**未安装的 effect** ⇒ 生产形态是**不答**，
  不是「答默认值」。6a 表因此有 6 行答了 Go 永不答的字节（其中体积/标题/ANSI-DECRQM 会被
  TUI 探测消费）。
  **状态：573700a 已把 6 行改判为「不答」并换绑 `term_responder.json`；我逐条复核与该向量一致 ✓**
  （详见 §五 消解清单）。但 `:131` 的总结句仍在，建议改述为「未装 effect ⇒ 该族查询不答」。
- 严重度：中（原表已消解，剩描述）。

### A8 中｜SNAPSHOT 镜像窗口尺寸/头语义未写
- 位置：`R6-design.md:252`（只写 `[mirrorLen:u32][mirror]`）vs `term_surface.go:70`
  （`mirrorViewports=10`）、`term_vt.go:179-189`（`above = rows*10`，镜像用**独立** `EncodeGrid(cols, len(rs), rs)`
  封装 ⇒ 头里 rows = 镜像行数）、`surface/surface_scroll.cpp:46-52`（客户端**按位置**重建，行 y 不参与）。
- 建议：补「10 视口 = rows×10 行、独立网格头、行序最旧在前、每行 y 不使用（客户端按序入缓存）、
  镜像 size > offset 时客户端整块丢弃」。
- 严重度：中（wire 载荷与 golden 都依赖）。

### A9 低｜「24 文件内嵌」= 23 个 toml
- 位置：`R6-design.md:22`、`:274` vs `manifest/load.go:29`（`//go:embed manifests/*.toml`）、
  `fixtures/term-manifests/`（24 项，含 `README.md`）。22 = index 的 agent id 集 ✓。
- 建议：改「23 个 toml（22 agent + index）+ README」。
- 严重度：低。

### A10 中｜降级项（恒 false/不可达）定性复核 —— 用户点名的五条
- **D-4（X10/URXVT）定性站不住（中高）**：`vt.go:282` 的 `Modes.MouseX10` 是**独立可读**的真实位；
  `term_vt.go:196-198` 把 `MouseX10||MouseNormal` OR 进 wire 的 M1000 位；**编码器真会上报 X10**
  （我用 Go 真源 probe 实测 `?9h` + press → `ESC[M $#`，release/motion/wheel 不报）。
  Rust 恒 false ⇒ ① `?9h`-only 时 wire M1000 位缺（APP surface 解码层用它决定触摸是否转鼠标事件）；
  ② 该应用完全收不到鼠标。URXVT 同样实测为活路径：`?1000h ?1015h` → `Modes.MouseURXVT=true`
  且编码 `\x1b[32;6;4M`（`?1006h ?1015h` 时 URXVT 胜出 —— 又一次印证 B5 的 last-set 单值）；
  当前鼠标向量未采 1015 形态。
  建议：probe 拦 `PrivateMode::Unknown(9)` 自管 X10 位 + 编码器补 X10/URXVT 支路，
  或在 D-4 里明写「实测偏差 + 影响面：`?9h`-only 应用收不到鼠标上报」并给出判据豁免理由。
- **D-5（overline）成立，但同类 SGR 5 blink 未登记（中）**：Go `render.go:61,273-275` 有
  `AttrBlink=1<<3`；ghostty `sgr.zig:302` 支持 `5 => .blink`；alacritty 0.26 的 cell `Flags` 无 BLINK
  （`term/cell.rs:15-35`）、handler 也不处理 `Attr::BlinkSlow/Fast`（`term/mod.rs:1885-1926`）
  ⇒ attr bit3 在 Rust 侧**不可达**，而 golden 夹具唯一覆盖的属性集是 `1;3;4;7` + `2;9`
  （`term_surface_golden_test.go:419-423`），**无 SGR 5** ⇒ 无判据。建议与 D-5 合并为
  「样式位降级（bit3 blink / bit7 overline）」，补一条「不可达位」显式测试。
- **D-7（password）定性成立**（APP 只拿它做输入框提示），但**当前无判据**：cursor flags 列
  （manifest col10）在 6b 测试里根本没断言（见 C3）。
- **D-8/D-9 已在 HEAD 改判且与向量一致 ✓**（OSC12 = 前景；OSC4 = ghostty 内置表）；
  93c29d7 已补采 256 色全表 ⇒ 该条**已关闭**。
- **D-10（darwin 分支）定性需修正（中）**：ghostty 模式表 `alt_esc_prefix`(1036) 是
  **`.default = true`**（`modes.zig:320`），`setopt_from_terminal` 直接拷它（`key_encode.zig:61`）；
  `legacyAltPrefix` 只在 **darwin** 被 `macos_option_as_alt` 门掉（`key_encode.zig:552-576`）。
  573700a 把 §3.1 回写成「alt 直发文本（实测 alt+a → a）」——**该实测跑在 darwin**
  （向量由本机 darwin-arm64 cgo 库产）。Linux 出口（ROADMAP 里 Go 出口本就跑 aliyun Linux；
  Rust 出口若同形部署）上 alt+text 应产 `ESC+text`。设计 `:364` 又写「linux 出口无此面」。
  建议：按 host OS 保留编译期分支（darwin: option-as-alt 门；linux: 1036 默认 on ⇒ ESC 前缀），
  并在 Linux 上补一条向量（同一 harness 换 host 即可）。
- 另：**D-6（damage 含光标行）定性「安全超集」不完整** —— 见 B3（它让脏行集**永不空**）。

### A11 低｜词表/死分支
- `R6-design.md:177`、`:193` 的 `composing` 分支是**死分支**：wire 的 key 载荷
  （`term_surface.go:632`）无 composing 字段，`term_vt.go:331-339` 也不填 ⇒ 恒 false。
  设计已正确写明 `UnshiftedCodepoint ≡ 0`，composing 建议同样标注或删除。
- `R6-design.md:383`「检测三态」vs `agent.go:103-131` 的 stateV2 四值（unknown/working/blocked/idle）。
- `agentNames` 4 名硬编码（`agent.go:155-163`）vs 22 个 manifest id 的分叉未写（其余 18 个 agent
  落哪个 wire 枚举需明确）。

---

## 二、vt.rs 实现正确性（必答②）

### B1 高｜D-16 拦截丢掉全量脏标记 ⇒ 清屏不下发
- 位置：`crates/homeway-core/src/term/vt.rs:811-818`（`clear_screen` 拦截分支早退）。
- 问题：alacritty 的 `Term::clear_screen` 在**末尾**才 `mark_fully_damaged()`
  （`alacritty_terminal-0.26.0/src/term/mod.rs:1750-1816`，:1815），而 `Grid::reset_region`
  （`src/grid/mod.rs:357-380`）不碰 damage；拦截分支 `self.term.grid_mut().reset_region(..); return;`
  把这句跳过了。**实测**（probe，20×5）：写满 5 行 → clean → `\x1b[H\x1b[2J` →
  `update()=Partial`、`dirty_rows()=[0,4]`（只有光标相关行）。服务端屏已清空
  （`screen_text()` = 全空行），但差分只送 2 行 ⇒ 客户端 1..3 行残留旧内容。
- 建议：`mark_fully_damaged` 是**私有**（`term/mod.rs:494` 无 `pub`），`damage()` 只读、
  `reset_damage()` 只清 ⇒ 适配层需自管 `force_full: bool`（与 `mouse_flags` 同款思路）：
  拦截时置位，`update()`/`dirty_rows()` 见位即返回 `Full`/全视口行，`clean()` 清位。
  同时补一条「ED2 后本拍必须全视口行」的测试（见 C1）。
- 严重度：**高**（6e 起任何「只清屏不写内容」的形态客户端不刷新；`clear`/vim 重绘之外的
  `printf '\033[2J'`、`tput clear` 均命中）。

### B2 高｜绝对行号 ⇔ Line 换算算错（debug 直接 panic）
- 位置：`vt.rs:451`（`let line = Line(abs as i32 - total as i32);`）与设计 `R6-design.md:77` 同式。
- 问题：alacritty 的 `Line(0)` = 视口顶、视口占 `[0, screen_lines)`、回滚是负行
  （`src/grid/storage.rs:220-222` 断言 `requested.0 < visible_lines`；`src/grid/mod.rs:523-535`）。
  正确换算 = `Line(abs - (total - rows))`。**实测**：`session-git-log.bin`（total=44/len=32/offset=12）
  → `rows_at(offset, 2)`（=12）→ `assertion failed: positive < self.len`（`storage.rs:225`）panic；
  `rows_at(total-1,4)` 不 panic 但返回 **1 行错内容**（把回滚行当视口底行）。release 下按环形缓冲
  `zero + positive` 取到错行（无断言）。影响面 = SNAPSHOT 镜像窗口 + FETCH-ROWS 应答（6e/6f 核心），
  当前**零测试覆盖**。
- 建议：改公式 + 补 3 条测试（`rows_at(offset,…)` = 视口前两行；`rows_at(0,…)` = 最旧回滚行；
  `from ≥ total` 截断为空）。修完再进 6e。
- 严重度：**高**。

### B3 高｜`Dirty::None` 不可达 ⇒ 空闲每拍产 1 行差分（Go 是整拍跳过）
- 位置：`vt.rs:313-324`（`update`）/`:328-337`（`dirty_rows`）；设计 `:89-92`（D-6 把它当"多发一行"）。
- 问题：alacritty 的 `Term::damage()` **无条件** `damage_cursor()`（`term/mod.rs:458-486`，
  `damage_cursor` 在 :1021-1026），Partial 迭代器因此恒 ≥1 行 ⇒ `update()` 永不返回 `Dirty::None`，
  `dirty_rows()` 永不空。**实测**：`vt.clean()` 之后立刻 `update()` 仍是 `Partial`（无任何写入）。
  Go 参考行为：空闲 tick `changed=false / count=0 / 整拍跳过`
  （`pkg/term/term_surface_test.go:595-604` `TestSurfaceUpdateSendsCursorAndModeChanges`）。
  照现有适配层直接 port `SurfaceTick` ⇒ 空闲会话每拍给每条 surface 腿入队 1 行 DIFF（
  `term_surface_session.go:143` 的 `count==0 && stateUnchanged` 短路永不命中）——wire 流量与
  Go 不等价（耗电/带宽），且 `term_surface_test.go:695-720` 那条「空闲不得出现空行集差分」的
  同款判据在 Rust 侧会以 `count=1` 形态存在。
- 建议：① 会话层把「本拍是否有新字节 `vt.write()`」作为聚合信号（pump 侧天然可知），与
  `stateUnchanged` 一起构成跳过条件；或 ② 在适配层做行 baseline 比对（`clean()` 时存 行/光标
  快照，`update()` 时比对）。二选一必须在 6e 前定，并登记为与 Go 的差异点（D-6 补一句
  「不只是多发一行，而是脏行集恒非空」）。
- 附：`rows()` 的注释「隐含消费脏状态」（`:339-342`）**与 alacritty 语义不符**：`damage()` 非破坏性，
  只有 `reset_damage()` 清（`term/mod.rs:489-491`）；而 Go 的 `Rows()` 走 `render_state_update`，
  **真的消费** terminal dirty（ghostty `render.zig:341-346` 原文 "This will reset the terminal dirty
  state since it is consumed by this render state update"）。若 6e 照 Go 的「Rows() 之后本拍无脏行」
  逻辑 port，Rust 会多发一拍。
- 严重度：**高**。

### B4 高｜D-15 拦截不完整（原点模式/滚动区/笔态/charset）
- 位置：`vt.rs:751-755`（只 `self.term.goto(0, 0)`）。
- 问题：ghostty 的未配对 `?1049l` 走 `restoreCursor()`（`Terminal.zig:4864-4867`），语义 =
  「有 saved_cursor 则完整恢复，否则默认 `{x:0,y:0,style:.{},protected:false,pending_wrap:false,origin:false,charset:.{}}`」
  （`Terminal.zig:2019-2047`），且**先 `modes.set(.origin, saved.origin)` 再 `cursorAbsolute`**。
  Rust 只做位置：① `Term::goto` 在 ORIGIN 置位时会加滚动区偏移（`term/mod.rs:1156-1172`）；
  ② 不复位 SGR 笔态/origin/charset/pending_wrap/protected。**实测**：
  `\x1b[3;4r\x1b[?6h\x1b[3;5H\x1b[?1049l` → Rust 得 `(0,2)` 且 `origin=true`；
  ghostty 应为 `(0,0)` + `origin=false` + 笔态默认。配对形态（1049h…1049l）两者都对
  （实测 (4,3) 恢复一致），差异只在未配对 + 有滚动区/origin 的形态。
- 建议：拦截分支先 `terminal_attribute(Attr::Reset)`（公开 Handler 方法）、必要时清 origin
  （`unset_private_mode(Named(Origin))` 走一遍），再在 origin=false 下 `goto(0,0)`；
  charset/pending_wrap/protected 若找不到公开 API 就登记残余差异并补实测向量。
- 严重度：**高**（错误坐标是可见语义错；代价低，改起来 5 行）。

### B5 高（6c 规格）｜D-17 的「编码器只读本层快照」结论错：ghostty 是 last-set 单值
- 位置：`R6-design.md:213`（§3.2 首段「模式判定（TermMode 位 → ghostty MouseEvent/Format 二维）」）
  与 `:371`（D-17「自管 mouse_flags 三位……编码器只读本层快照，互斥态不影响行为」）。
- 问题：ghostty 的**上报面/格式是单值、取"最后一次 set/reset"**：`Terminal.zig:111-116`
  原文 "These are set to the last set mode in modes. You can't get the right event/format to use
  based on modes alone because modes don't show you what order this was called"；
  `stream_terminal.zig:1609-1641` 每次 set/unset 覆盖 `flags.mouse_event`/`flags.mouse_format`。
  我用 Go 真源 probe 实测（同一份 bits 两种行为）：

  | setup | Modes 位 | motion | press | release |
  |---|---|---|---|---|
  | `?1000h ?1002h` | N=1,B=1 | 上报（button） | 上报 | 上报 |
  | `?1002h ?1000h` | N=1,B=1 | **不报**（normal） | 上报 | 上报 |
  | `?1000h ?1003h ?1003l` | N=1,B=0 | **不报** | **不报** | **不报** |

- 影响：wire 的 `mouse_flags` 三位（独立位）**是对的**（Go 的 `surfaceModesOf` 读的正是独立位
  `vt.go:282-285`，实测 bits 与 Rust 一致）——D-17 的拦截动作本身没错；错的是**给 6c 的结论**：
  编码器必须另记一对外管单值 `mouse_event ∈ {None,X10,Normal,Button,Any}` /
  `mouse_format ∈ {X10,Utf8,Sgr,Urxvt}`（set/unset 时覆盖，RIS 归零），不能从 `Modes` 推。
  当前 `term_mouseenc.json` 只做单模式 setup，**采不出这个差异**（需补 3 组序列）。
- 建议：`Modes` 增两个自管字段（或 SessionVt 内部字段 + 访问器）；D-17 补一句「编码面单值语义」；
  向量补采上述 3 组 + `?1015h`（URXVT）与 `?9h`（X10，见 A10/D-4）。
- 严重度：**高**（6c 编码器规格错误，直接产出与 Go 不同的字节）。

### B6 中｜`?47h`/`?1047h` 备用屏不可达（D 表未登记）
- 位置：`vt.rs:710-757` 的 private mode 拦截只有 1049 分支。
- 问题：vte 0.15 的 `NamedPrivateMode` 只有 1049（`vte-0.15.0/src/ansi.rs:938-968`），
  47/1047 落 `PrivateMode::Unknown` 被 alacritty 忽略（`term/mod.rs:1937-1940`）；**实测**
  `?47h`/`?1047h` → `screen=Primary`（什么都不发生）。ghostty 两者都实现
  （`modes.zig:302/323` + `Terminal.zig:4800-4869` 的 `switchScreenMode .@"47"/.@"1047"`），
  Go 的旁路扫描器也把 47/1047/1049 都当 AltScreen（`modes.go:167`）⇒ wire Alt 位与屏内容双错，
  且 1047 退出时的「清屏」语义缺失。老 curses/vi 系程序仍在用 47/1047。
- 建议：probe 拦 `PrivateMode::Unknown(47|1047)` → `self.term.swap_alt()`（公开，`term/mod.rs:714`，
  入口会 mark_fully_damaged ✓，1047 退出补 erase）；至少登记为 D-18（影响面：Alt 位 + 屏内容）。
- 严重度：中高。

### B7 中｜属性位 BLINK 不可达（见 A10/D-5）
- 位置：`vt.rs:528-561`（attr 拼装无 BLINK）；`attr::BLINK`（`:131`）恒不置位。
- 严重度：中（与 D-5 同类，需登记 + 判据）。

### B8 低｜`wide_tail` 多一个空格条件
- 位置：`vt.rs:373-384`（`wide_tail && cur_cell.c == ' '`）vs ghostty `render.zig:569-572`
  （只看左格 `.wide == .wide`）。当前形态等价（spacer 恒空格），但语义不精确，且 golden 未断言
  col10（见 C3）。建议去掉多余条件或注释说明。

### B9 低｜`color_of` 的 `*named as u8` 截断
- 位置：`vt.rs:578`。`NamedColor::Cursor(258)`/`Dim*(259+)` 会被截成 2..18。当前不可达
  （那些只用于 underline color），但建议显式 match + 兜底 `Color::None`，别留静默截断。

### B10 低｜`CursorShape` 的 `_ => Block`
- 位置：`vt.rs:385-390`。`HollowBlock`/`Hidden`（`vte/src/ansi.rs:828-844`）被吞成 Block；
  vte 不构造这两个变体（HollowBlock 无生产者；Hidden 只在 vi 模式）⇒ 当前安全，
  建议显式列全以防上游新增。

### 看过且没问题的（§二）
- `Line(0)=视口顶` 的 D-1 结论 ✓（`storage.rs:220-222`）。
- 光标 x/y、`shape` 的 DECSCUSR 映射（vte `ansi.rs:1715-1727`：1|2→Block、3|4→Underline、5|6→Beam，
  blinking=id%2）、默认值（alacritty `default_cursor_style = CursorStyle::default()` = Block 不闪
  ⇒ 与 golden col10 的 flags=1 自洽）✓。
- 模式位到 `TermMode` 的 16 项映射 ✓（含 `SHOW_CURSOR/LINE_WRAP/ALTERNATE_SCROLL` 默认值
  与 ghostty 模式表一致：`term/mod.rs:113-120` vs `modes.zig:295/299/319`）。
- kitty 五位拼装 ✓（`modes()`:267-282 与 `surfaceModesOf` 位值一致）。
- `TermProbe` 的方法覆盖：除 `set_mouse_cursor_icon`（vte 未 re-export `cursor_icon::CursorIcon`，
  且 alacritty `Term` 也未实现 ⇒ 默认 no-op 等价）外，vte 0.15 Handler 的 71 个方法**全部实现**，
  三处拦截之外的委托无遗漏 ✓。
- `update → dirty_rows → clean` 的**调用顺序**本身与 Go 的 `Update→DirtyRows→Clean` 同构 ✓
  （问题只在 B3 的"空集"语义）。
- `rows_in` 的 `Line(y)`/`Column(x)` 视口索引、`scrollbar = total-len`、`at_bottom`、
  `mirror_rows`/`rows_at` 早退条件（备用屏/above==0/越界）✓（除 B2 的换算）。
- `screen_text()` 与 Go `ScreenText`（`render.go:365-388`）等价（跳 skip、空符号补空格、
  `trim_end_matches([' ','\t','\u{a0}'])`）✓。
- `cell_of` 的空格归一化/zerowidth 合并/skip/宽度派生与 Go cellcodec 的**解码侧**口径自洽 ✓
  （见 C4 的覆盖面警告）。

---

## 三、测试判据强度（必答③）

### C1 高｜脏行契约零测试（B1/B3 的藏身处）
- 位置：`vt.rs:914-1016` 四个测试 vs 设计 `:120-126`（§1.4 自检三条）。
- 问题：`update()`/`dirty_rows()`/`clean()` 三者的契约（尤其设计 `:89-92` 定的「Full ⇒ 全视口行」
  与「clean 在载荷下发后」）**没有任何断言**；`mirror_rows`/`rows_at`（B2）同样零覆盖。
  B1（ED2 丢 damage）与 B3（脏行集恒非空）都能在 10 行测试内钉死。
- 建议：新增 `vt_damage_contract`（滚动=Full→全行；ED2 后=全行；空闲 after clean=空；
  clean 后重复 update 幂等）与 `vt_abs_rows`（B2 三态）。
- 严重度：**高**。

### C2 中｜`-diff` 4 行完全未对拍（与设计自定判据不符）
- 位置：`vt.rs:938-944`/`:985`（只 4 个 base 名）vs `R6-design.md:274`（「8 行（4×快照+差分）
  digest 全对」）；`grep -n '\-diff' vt.rs` = 0。
- 影响：DIFF 体、脏行应用、追加后的光标（16,5)/(16,31)/(16,24)/(25,0)、`-diff` 的 modes=86
  全无判据。设计把这一半推给 6e——可以，但 §1.4 的 6b 自检声称的「golden 对拍三步」就应
  明确写"4 行快照"，别让读者以为 8 行已绿。
- 建议：6e 落地时把 `-diff` 纳入；本轮在 §1.4 标注范围。

### C3 中｜光标 flags(col10)/shape(col11) 未断言
- 位置：`vt.rs:997-998`（只查 row[8]/row[9]）vs Go 侧 golden 四列全查
  （`term_surface_golden_test.go:241-245`）。
- 影响：`visible/blinking/wide_tail/password/shape` 的映射没有 golden 支撑；D-7 的
  「登记豁免」目前无任何判据（正好是用户点名的"恒 false 类"降级）。
- 建议：补 `row[10]`/`row[11]` 断言（映射 `surfaceCursorOf` 的 bit 位：0 可见/1 闪/2 宽尾/3 密码）。
- 严重度：中。

### C4 中｜带样式空格（V-1 的 width=0 分支）golden 零覆盖
- 位置：`vt.rs:509-518`（styled ⇒ width=0）与 D-2/V-1 的措辞（`R6-design.md:356`「golden 全绿」）。
- 实测（probe）：四个夹具（cjk/git-log/hexdump/styles）`symbol=="" && !skip && width==0` 的格数
  **全为 0**；`session-styles` 的三个空格都在 `\x1b[0m` 之后（无样式）。即
  「带样式空格 width=0」是**未被任何夹具裁决的假设**——「golden 样式向量 digest 会红/绿直接裁决」
  这句话不成立（digest 里 width 进 flags，只有真的存在带样式空格才可能红）。
- 影响：若 ghostty 对显式带样式空格给的 grapheme 是 `" "`（而非空），Go 的 wire 会编 symLen=1
  ⇒ 解码 Width=1，而 Rust 给 0 —— 两端不一致且测试全绿。
- 建议：加一条 `\x1b[41m \x1b[0m` 内联用例，把 Go 真值（克隆 harness 采一格）写进向量再断言。
- 严重度：中。

### C5 低｜`scrollbar().offset` 断言恒真
- `vt.rs:398` 的 `offset = total - len` 是派生值，`:1000` 对 row[13] 的断言不可能失败。
  建议改为断言独立的 offset 读数（或至少标注该列是派生校验）。
- 严重度：低。

### C6 低｜`style_of(&rows, 100)` 硬编码列数
- `vt.rs:959`/`:988`。建议从 `row[2]` 读 cols。
- 严重度：低。

### C7 低｜session-styles 内联串三处拷贝
- `vt.rs:951` 与 `:988` 两处 + Go `goldenStylesSession()`（`term_surface_golden_test.go:416-430`）。
  评审实测两端**逐字节相同（171B）** ✓，但拷贝维护无锚。建议抽 `fn styles_fixture()` 或落
  `fixtures/term-vt/session-styles.bin`（设计 §4.3 只列三会话流）。

### C8 低｜RIS 测试未钉「记账路径生效」
- `vt.rs:967-979`：`vt.write(b"\x1b[>4;2m")` 后没断言 `modify_other_keys==true`，而是靠显式
  `set_modify_other_keys(true)` —— 即 vte 的 `CSI > 4;N m` 分派（`vte/src/ansi.rs:1685-1693`）
  其实已被验证可用，但测试没把它钉住。建议在 RIS 前加一条断言。

### C9 低｜oracle 可被重生成
- 判据源是 Go 产 `manifest.tsv`（`go test -update` 可重写）。评审实测 `fixtures/SHA256SUMS`
  51/51 OK ✓，且 fixture 与 baseline `surface/test/golden/` 同源。建议在测试头注释写明
  「该文件的权威性靠 SHA256SUMS + 基线门，不靠本测试自证」。

### 看过且没问题的（§三）
- manifest 17 列列序/列宽与 Rust 索引**完全一致** ✓（写入端 `term_surface_golden_test.go:129-133`；
  name0/op1/cols2/rows3/rev4/text5/title6/frames7/x8/y9/flags10/shape11/total12/offset13/len14/style15/modes16）。
- 尺寸/回滚假设：100×32 + scrollback 5000 与 Go `vtNew(100,32,5000)`（`:294`）一致 ✓
  （注：夹具最大 total=44，5000 vs 10000 不可观测）。
- 文本/样式 digest 口径逐函数等价 ✓（`goldenDigest`=FNV-1a64→`%016x`；`goldenStyleText` 每行
  26 hex + 行尾 `\n`；文本行 `TrimRight(" ")` + `\n` 连接、无尾 `\n`；缺格 `Cell{Width:1}` 两侧同款
  ——且实测两侧都不存在缺格）。
- `session-styles` 内联串与 Go 逐字节相同（171B，评审期程序化 diff）✓。
- 判据实际强度：4 行 digest 各异、styles 行覆盖 BOLD/ITALIC/UNDERLINE/INVERSE/FAINT/STRIKE +
  256 色/RGB/16 色 + 宽字符 skip + modes=86（2|4|16|64），是真判据不是空跑 ✓。

---

## 四、Rust 工程原则（必答④）

### D1 中｜`Result<_, String>` 违反仓内工程原则
- 位置：`vt.rs:199-202`、`:240-243`。`AGENTS.md` 明写「错误一律 thiserror 类型 + `Result` 链，
  **不用字符串错误**」。建议 `#[derive(thiserror::Error)] pub enum VtError { #[error("vt: 尺寸非法 {cols}x{rows}")] InvalidSize{cols:u16, rows:u16} }`。

### D2 中｜`Cell::width` 文档与实现互相矛盾
- 位置：`vt.rs:143-145`（结构体注释「1 窄、**2 宽**、0 = 占位格……**编码侧**由「symbol 空」派生」）
  vs `:489-500`（宽字符头给 **1**；width 是**解码语义**）vs Go `cellcodec.go:159-169`（编码器
  **不写** width）、`:284-286`（解码侧 `Symbol=="" ⇒ Width=0`）。
- 问题：①「2 宽」永不产生；② 派生发生在**解码侧**不是编码侧；③ 6d 若按注释实现会多写/错写字节
  （wire 无 Width 字段，与设计 `:85` 的表格把它当 wire 字段是同一处误读）。
- 建议：改成「width = 解码语义宽度（对齐 Go `decodeCell`）：symbol 空 ⇒ 0，占位格 ⇒ 0+skip，
  其余 ⇒ 1；wire 不编该字段」；设计 §1.1 的 Width 行同步修正为「派生字段，不在 wire 上」。

### D3 低｜`Row.dirty` 恒 false 的死字段
- `vt.rs:350-366`、`:440-462` 都写 `dirty: false`。要么删，要么在差分路径填真值；现状是
  「看起来有信息的字段实际恒假」。

### D4 低｜`Sink` 未被使用
- `vt.rs:190-195` 定义了 `Sink`，但 `SessionVt` 用的是 `Term<VoidListener>`（`:172`、`:215`）。
  6c 接剪贴板前建议先删（避免读者以为它已生效——OSC 52 当前实际是**静默丢弃**的）。

### D5 低｜~70 个手写委托方法的样板量
- `vt.rs:615-830`。语义上无漏洞（见 §二"看过且没问题"），但可用 `macro_rules! delegate!`
  压缩到 30 行内，把「只有 3+1 处拦截」的事实凸显出来。若保留手写，建议在文件头列出
  「拦截清单」表（当前注释散在各方法上）。

### D6 低｜`modes_static()` 与 `modes()` 重复判定
- `vt.rs:427-438` vs `:285-289`。建议抽 `fn screen(&self) -> Screen` 复用。

### 看过且没问题的（§四）
- 无多余 `Arc/Mutex`（并发按"会话锁串行"交给上层，`vt.rs:16` 已注明）✓。
- `SessionVt` 的字段形态（`Term<VoidListener>` + `Processor` + 尺寸记账）合理 ✓。
- `Color`/`Modes`/`Cursor`/`Scrollbar` 作为 wire 中间类型的**形态**在 6d 未消费前是合理的
  （6d 的 `codec.rs` 会直接吃它们）——唯一要改的是 `Cell::width` 的语义注释（D2）与
  `Row.dirty`（D3）。

---

## 五、6c–6g 拆步可执行性（必答⑤）

### E1 中｜6c 的"向量先行"路径已实证可行，但规格仍有 4 处要补
- 事实：`264fa70` 已把 harness + 三件向量产出（应答 43 案/键 349 案/鼠标 160 案），
  `93c29d7` 又补了 256 色调色板全表。评审期我独立复核了 DA1/DA2/DA3/DSR/DECRQM/kitty/OSC4/OSC10-12
  与 §3.1/§3.2 的回写结论，**逐字节一致** ✓ —— 这条工程路径没有坑（cgo 克隆可构建、
  gitignore 的 prebuilt `.a` 在基线树内在位、脚本零残留）。
- 仍需补进向量：① `?9h`（X10）与 `?1015h`（URXVT）形态（A10/D-4）；② 鼠标三组
  「last-set 单值」序列（B5）；③ Linux 宿主的键编码（D-10；同一 harness 换 host 跑一次）；
  ④ 带样式空格一格（C4）；⑤ UTF8(1005) 需要**大坐标**（>95）才能与 X10 区分
  —— 实测 (5,3) 时 `?1005h` 与 X10 输出完全相同（`ESC[M &$`），当前向量对 UTF8 分支无判别力。
- 严重度：中。

### E2 中｜harness 头部前提描述与事实不符
- `tools/vector-gen/term/vecgen_term_test.go:12-13`（「Go 绑定只装 write_pty sink、未装任何
  effects 回调 ⇒ 生产形态就是这些默认值」）——与 `helpers.h:40-59` 的 4 个注入点（外加
  userdata/主题）和实测（size/xtversion/title 不答）矛盾。建议改述，免得 6c 按错误前提实现。

### E3 中｜剪贴板/应答抑制的字节级行为只有一行
- 设计 `:155`（OSC 52）与 `:157-158`（capsRawTerminal 抑制）覆盖了方向，但 `helpers.c:99-211`
  的实际语义（MIME 优选 `text/*`、64KiB 回复缓冲、空内容 DENIED、SUCCESS/UNSUPPORTED 应答、
  `remember` 位、location 0/1）没有落点，而这些对客户端可见。建议 6c 设计增量补表。

### E4 中｜surface 双向对拍可落地，但入场前必须修 B1/B2/B3
- 方向 ① 解码侧：Go 产 `.bin`（`fixtures/surface-golden/`，块格式 `[u32 帧数]{op + [u32 片数]{[u32 len][bytes]}}`）
  已在位 ✓；方向 ② 产出侧：Rust 侧目前零 codec（6b 范围外，正常）。但 B1/B2/B3 正好落在
  「产出端脏行+镜像」这条链上，不先修会在 6e 以"帧对不上"的形式返工。另：`-diff` 4 行要纳入判据（C2）。

### E5 低｜退出口（kitty 挂 R6.5）合理，但"基础编码"子集要定义
- kitty 五位已由 alacritty `TermMode` 提供，工作量集中在 `key_encode.zig` 决策树；而它与
  legacy/ctrl/mok 在同一函数里交织。建议明确先行子集 =「legacy 全表（功能键/光标/小键盘/DECCKM/
  DECBKM）+ fixterms CSI u + modifyOtherKeys + kitty DISAMBIGUATE 单档」，其余（REPORT_ALL/
  ASSOCIATED_TEXT/ALTERNATE_KEYS 全组合）挂 R6.5，否则退出时会出现"半套编码"的判据空窗。

### E6 低｜两处最可能返工的空白：A5（帧表）与 A1（region）
- 都不是 6c 的工作，但都必须在 6d/6f 开工前补进设计（增量节即可）。建议在 6c 收尾的同一
  工作单元里补，避免 6d/6f 靠现读 Go 代码推进。

---

## 六、评审期被并行工作流消解/复核的项（HEAD 93c29d7）

| 6a 原文 | HEAD 状态 | 我的独立复核 |
|---|---|---|
| XTVERSION 答 `\x1bP>\|libghostty\x1b\\` | 改判「不答」 | ✓ 与 `term_responder.json.xtversion=""` 一致；根因：trigger 是 `CSI > q`（`stream.zig:2262`）+ Go 未装用户回调 |
| DECRQM ANSI（无 `?`）答 `state$y` | 改判「不答」 | ✓ 向量 4 案全空；ghostty `stream.zig:2169-2203` 的 `'p'` 分支只收 `intermediates.len==2`，`CSI 4 $ p` 落到 warn+ignore |
| OSC 10/11 未上报主题答默认色（V-2） | 改判「不答」 | ✓ 向量 osc10/11/12_default 全空；themed 时答 `\x1b]10;rgb:1111/2222/3333\x07`（**BEL 结尾**，非 ST） |
| CSI 14/16/18t 按名义 cell 自答 | 改判「不答」 | ✓ 向量全空；根因 `stream_terminal.zig:1410-1440` 需 `effects.size`（Go 未设 `GHOSTTY_TERMINAL_OPT_SIZE`） |
| CSI 21t 答标题 | 改判「不答」 | ✓ 向量空；根因 `title_report` 无生产 setter（`stream_terminal.zig:5057` 仅测试） |
| OSC 4 未 set 索引 = xterm 基表（D-9） | 改判「ghostty 内置表」+ 93c29d7 补 256 色全表 | ✓ 向量 idx5=b2b2/9494/bbbb 等；真表在 `color.zig:494-520`（16 色）+ `:11`（256 色表） |
| §3.1 ctrl→C0 / alt 前缀 / mok2 修饰 | 按向量回写 | ✓ mok2 ctrl+a→`\x1b[27;5;1~`、shift+a→`27;2;65~`、ctrl+alt 只叠 ctrl、kitty31 ctrl+a→`\x01`、plain ctrl+a→`\x1b[1;5u`、alt+a→`a`（darwin 宿主，见 D-10 的 Linux 提醒） |
| §3.2 坐标边界 | 按向量回写（≥222 不报、先于格式选择、1000 release btn=3、wheel 64/65） | ✓ 与 mouse 向量一致 |

**未消解**：本文 A1/A2/A3/A4/A5/A6/A8/A10(D-4,D-5,D-10)/A11、B1–B10、C1–C9、D1–D6、E1–E6。

### 依赖接入（d5d8fba）复核 —— 看过，没发现问题
- `alacritty_terminal = "0.26"` 带出 vte 0.15 ✓（`vt.rs:23-25` 的 `vte::ansi` 路径与 0.26.0 源码一致）。
- `flate2 = "1"`：默认 OS 头 = 255、XFL = 0（`flate2-1.1.10/src/gz/mod.rs:451-455`
  `header[9] = operating_system.unwrap_or(255)`；`header[8]` 在默认 level 6 下为 0）⇒ 与 Go
  `gzip.NewWriter` 默认头一致 ✓；「不启 zlib-ng 保纯 rust 后端」的取舍成立。
- `portable-pty = "0.9"`：设计点名的 API 全部在位 —— `PtySystem::openpty`（`lib.rs:267`）、
  `Child::spawn_command`（`:165`）、`MasterPty::process_group_leader`（`:107`，unix 实现
  `unix.rs:374-375` = `tcgetpgrp`）、`CommandBuilder::{env,env_clear,cwd,arg}`（`cmdbuilder.rs:272-342`）、
  `PtySize`（`lib.rs:63`）⇒ §五 的 PTY spawn 设计可落地，「零降级超集」的说法看过的部分成立。
- `fnv = "1"`（golden digest）与 `regex = "1"`（manifest）无版本风险；regex 的语义问题见 A2。

---

## 七、实际核对过的文件清单

**homeway-rs（评审对象）**：`docs/reviews/R6-design.md`（82cf579 版 + HEAD 版 diff）、
`crates/homeway-core/src/term/vt.rs`（全 1017 行，逐函数）、`crates/homeway-core/src/term/mod.rs`、
`Cargo.toml`/`Cargo.lock`（依赖段）、`ROADMAP.md`（R6 节/评审协议/隔离条款）、`AGENTS.md`、
`fixtures/surface-golden/manifest.tsv` + `*.bin`、`fixtures/term-vt/*.bin`、`fixtures/SHA256SUMS`、
`fixtures/term-manifests/`（24 项）、`tools/gen-vectors.sh`、`tools/vector-gen/term/*`、
`fixtures/vectors/term_{responder,keyenc,mouseenc}.json`。

**Go 基线（只读）**：`pkg/term/vt/{vt.go,render.go,cellcodec.go,types.go,response.go,input.go,keys.go,mirror.go,helpers.h,helpers.c}`、
`pkg/term/{frames.go,modes.go,service.go,term_vt.go,term_surface.go,term_surface_session.go,term_surface_leg.go,term_state.go,agent.go,accessors.go,term_leg.go,term_cli.go,term_cli_attach.go,term_remote.go,log.go}`、
`pkg/term/manifest/{region.go,eval.go,load.go,manifest.go,dialect.go}` + `manifests/*.toml`、
`pkg/term/term_surface_golden_test.go` + `term_surface_test.go`（判据部分）、
`surface/{surface_codec.cpp,surface_scroll.cpp,surface_types.h}`、
`third_party/libghostty-vt/src/terminal/{Terminal.zig,stream_terminal.zig,stream.zig,modes.zig,render.zig,color.zig,sgr.zig,c/terminal.zig,page.zig,PageList.zig,Screen.zig}`、
`third_party/libghostty-vt/src/input/{key_encode.zig,mouse_encode.zig}`、
`third_party/libghostty-vt/include/ghostty/vt/terminal.h`。

**上游依赖源码**：`~/.cargo/registry/.../alacritty_terminal-0.26.0/src/{term/mod.rs,term/cell.rs,grid/mod.rs,grid/storage.rs}`、
`vte-0.15.0/src/ansi.rs`。

**明确「看过、没发现问题」的方面**（详见各节末尾）：帧常量值/caps/features/CREATE 极性/surfaceVer=4/
fragChunk/分片器语义；错误码词表 20/20 双向一致；会话常量（1MiB/256KiB/16/8/60s/10s/epoch≤64/
两次 Setsize 哨兵/SIGHUP→500ms→SIGKILL）；LIST JSON 字段序；检测状态机常量 3/700ms/800ms 与五路
融合权威序；cell 归一化与 digest 口径；mode u32 单映射；`TermProbe` 方法覆盖完整性与委托语义；
`session-styles` 串两端逐字节一致；夹具列序/尺寸假设。

---

## 八、附录A：评审期实测复现路径

1. **既有判据复跑**：`cargo test -p homeway-core term`（4 测试，评审期全绿）。
2. **Rust probe（临时，已删）**：
   - `primary ED2 → update()=Partial、dirty_rows=[0,4]`（B1）
   - `git-log 夹具 rows_at(offset,2)` → `assertion failed: positive < self.len`（B2）
   - `clean()` 后立刻 `update()` → 仍 `Partial`（B3）
   - `\x1b[3;4r\x1b[?6h\x1b[3;5H\x1b[?1049l` → `(0,2)` 且 `origin=true`（B4）
   - 四夹具 `styled_blank(width=0)` 全 0（C4）
   - `?47h/?1047h` → `screen=Primary`（B6）
3. **Go 真源 cgo probe（临时拷入 baseline 克隆 `pkg/term/vt/`，跑完即删；克隆 `git status` 复核干净）**：
   鼠标 4 组序列的 Modes 位 + motion/press/release 字节（B5 表格；命令：
   `GOTOOLCHAIN=go1.24.5 go test ./pkg/term/vt/ -run TestVecD17Probe -v`）。
4. **复用并行工作流向量**：`fixtures/vectors/term_{responder,keyenc,mouseenc}.json`
   （应答 43 案/键 349 案/鼠标 160 案），用于复核 HEAD 设计回写。

## 九、附录B：建议的整改顺序（进 6e 前）

1. **B2**（行号换算，5 行代码 + 3 测试）→ 2. **B1**（自管 force_full，~10 行 + 1 测试）→
3. **B3**（空闲语义，需 6e 设计先定方案）→ 4. **B4**（D-15 补笔态/origin）→ 5. **C1/C3/C4**（测试补齐）。
6c 侧并行：**B5**（编码器单值）+ **A10/D-4**（X10/URXVT）+ **D-10**（Linux 分支）+ 向量补采 4 项。
设计侧：**A1/A2/A5/A6**（四块落点）+ **A3/A4/A8/A9/A11** 小修。
