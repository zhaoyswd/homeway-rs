# R6 技术设计 — term 服务面（Rust 出口侧）

> 状态：v1（2026-10-03，第 1 会话产出，评审门一用稿）。
> 真源：baseline 克隆 `pkg/term/`（12,128 行非测试）+ `surface/`（C++ 客户端解码层，golden 对拍物）
> + tier `openspec/specs/{term-surface-protocol,term-agent-state,term-host-cli}`（只读）
> + libghostty-vt 源码（应答器/键编码器的行为真源）。
> 本文档按五道必答题组织；六、七节为模块地图与风险表。

## 0. 范围与总体形态

Rust 出口补齐 term 服务面全量：`<state>/term.sock` UDS 服务（7724 经拦截层 LocalServices 转投的
映射已由 R3 装好，`engine.rs:259`），与 Go 出口 `pkg/term` 同协议同判据：

- **协议栈**：`[op:1][len:2 LE][payload]` 帧族（`termProtoVer=1`，23 个 op），GREETING features
  = `list|replay|modes|agent|title|surface|protoVer`（0x7F），HELLO 尾随块（caps/ver/id）+ 服务端
  版本门（FIX-29：caps 带 `capsProtoVer` 且声明版本 ≠ 1 ⇒ `ERROR(term_version)` 拒腿）。
- **双轨**：raw 腿（回放 + DATA 原始字节）与 surface 腿（快照/差分/抽象输入）混合多腿共存。
- **会话**：PTY 由出口持有，腿断只摘泵；历史 = 定长字节环（1MiB 默认）；attach 回放 =
  尾部窗口 + 行边界/ESC 对齐 + 2s 预算；尺寸哨兵两次 Setsize 逼重绘；KILL = SIGHUP→500ms→SIGKILL。
- **vt**：alacritty_terminal 0.26 代替 libghostty-vt，**应答器与键/鼠标/焦点编码器自建**
  （Rust 生态无服务端实现，已知缺口）；旁路扫描器（modes/title/OSC 证据）从 Go `termScan` 平移。
- **检测**：manifest 数据驱动引擎（24 文件内嵌 + `<state>/agent-detection/` 覆盖）+ 五路证据
  融合 + 状态机卫生（working→idle 确认窗 3 拍/700ms、空闲零开销短路、blocked 800ms 重发）。

线程模型遵守 R1 决议：全部 std 线程 + 有界队列，无 tokio。

## 一、alacritty_terminal 0.26 接入面（必答①）

### 1.1 装配方式

每会话一个 `vt::SessionVt`（`term/vt.rs`），持：

```rust
struct SessionVt {
    term: Term<VoidListener>,          // EventListener 空实现（send_event 无操作）
    parser: vte::Parser,               // vte 0.15（alacritty_terminal 重导出）
    cfg: Config,                        // scrolling_history 默认 10000（HOMEWAY_TERM_SCROLLBACK_LINES）
    kitty: KittyState,                  // 见 1.3（模式位查询的兜底记账）
}
```

喂入走**单遍解析**：`parser.advance(&mut Handler 双面分发器, bytes)`。双面分发器
`TermAndProbe<'a>` 实现 `vte::ansi::Handler`，全部方法委托给 `&mut Term`，唯二例外：

- `csi_dispatch`：先按（params/intermediates/action）识别查询（DA1/DA2/DA3/DSR/DECRQM/kitty
  query/尺寸上报/标题上报），命中则把应答字节推进本会话的应答缓冲；再原样委托。
- `osc_dispatch`：识别 OSC 10/11/12 颜色查询与 OSC 4 调色板查询/设置；其余委托。

**为什么单遍**：Go 的应答在 `ghostty_terminal_vt_write` 内部**流中同步**触发（应答反映查询
发生那一刻的模式态）。若用「先扫后喂」两遍解析，跨读批次边界内的模式翻转会让 DECRQM
应答值与 Go 不一致；单遍双面分发器在语义上等价于 ghostty 的流中触发，且省一遍解析。
应答字节在批末由 pump 线程一次性投递到有界 `respChan`（容量 16，满即丢 + 计数），
由独立 response writer 线程写 PTY——对齐 Go FIX-25（毒会话不冻会话锁）。

模式位读取（对齐 `vt.Modes()` 的字段面）：

| Go vt.Modes 字段 | alacritty 来源 |
|---|---|
| CursorKeysApp (DEC 1) | `mode().contains(APP_CURSOR)` |
| KeypadApp (DEC 66) | `APP_KEYPAD` |
| BracketedPaste (2004) | `BRACKETED_PASTE` |
| FocusEvents (1004) | `FOCUS_IN_OUT` |
| MouseX10 (9) / Normal (1000) / Button (1002) / Any (1003) | 无 9 位：`MOUSE_REPORT_CLICK`＝1000；`MOUSE_DRAG`＝1002；`MOUSE_MOTION`＝1003。**X10 无对应位 ⇒ 恒 false**（登记差异 D-4；模式位只影响 Mouse1000 wire 位，Go 侧 `MouseX10||MouseNormal` 同置位，X10-only 形态罕见） |
| MouseSGR (1006) | `SGR_MOUSE` |
| MouseUTF8 (1005) | `UTF8_MOUSE` |
| MouseURXVT (1015) | **无位 ⇒ 恒 false**（D-4；只影响鼠标编码格式选择，见③） |
| AltScreen | `ALT_SCREEN` |
| KittyFlags (u8) | 5 位拼装：DISAMBIGUATE=1 \| REPORT_EVENT_TYPES=2 \| REPORT_ALTERNATE_KEYS=4 \| REPORT_ALL_KEYS=8 \| REPORT_ASSOCIATED_TEXT=16 |
| ModifyOtherKeys | alacritty 不跟踪 ⇒ 自管（应答器在 `CSI > 4;Nm` 处记账，RIS 归零；见②） |
| CursorVisible/Blink、Insert/Origin/Wraparound | `SHOW_CURSOR`；Blink 无位（D-4，快照光标 blinking 恒按 cursor_style）；`INSERT`/`ORIGIN`/`LINE_WRAP` |

网格读取：`term.grid()`（`&Grid<Cell>`）。**坐标系（实现核实，D-1 定案）**：alacritty
`Line(0)` = 视口**顶**行、正数向下到视口底、负数向上进回滚——**与 Go render state 同向**，
无需逐行换算；唯一换算点在「绝对行号 ⇔ Line」（`line_of_abs`）：

```
绝对行 a ∈ [0, total_lines) ⇔ Line(a as i32 - total_lines as i32)   // 0 = 最旧回滚行，total-1 = 视口底行
```

Cell 适配（`vt::cell_of(&Cell) -> wire Cell`）：

| wire 字段 | alacritty 来源 | 归一化 |
|---|---|---|
| Symbol | `c` + `zerowidth` 逐字符 UTF-8 拼接 | `c==' ' && zerowidth 空` → 空串（对齐 ghostty「空白格无字素」；**带样式空格是否落空串 = 6b 实测定调项 V-1**：golden 样式向量 digest 会红/绿直接裁决） |
| Width | `WIDE_CHAR`→2；`WIDE_CHAR_SPACER`/`LEADING_WIDE_CHAR_SPACER`→0+skip；否则 1 | — |
| FG/BG | `Named(Foreground/Background)`→None；`Named(调色板名)`/`Indexed(n)`→Palette(idx)；`Spec(rgb)`→RGB | INVERSE 不展开（wire 传原始，客户端解析，同 Go） |
| Attr u16 | BOLD/ITALIC/DIM(faint)/BLINK?无位见上/INVERSE/HIDDEN(invisible)/STRIKEOUT 位 0..6；下划线样式位 8..11（UNDERLINE=1、DOUBLE=2、UNDERCURL=3、DOTTED=4、DASHED=5） | **无 OVERLINE 位**（D-5 登记：SGR 53 丢失，样式向量 bit7 不可达；现有 golden 夹具不含） |

脏行：`term.damage()` → `Full` ⇒ 全视口行；`Partial` ⇒ `is_damaged()` 行（换算 y_top）。
`reset_damage()` 仅在载荷成功入队后调用（对齐 Go SurfaceClean 的乐观消费 + needSnapshot 兜底）。
**alacritty 的 damage 含光标行**（damage_cursor 会标脏光标所在行）——比 Go 多发一行属安全
超集（差分行多送不破坏一致性），登记 D-6（观测项，不计为漂移）。

### 1.2 resize / reflow 语义差异表

| 维度 | Go ghostty | alacritty | 处置 |
|---|---|---|---|
| resize API | `ghostty_terminal_resize(cols, rows, cellPx 8, cellPx*2)` | `Term::resize(&D)`（仅行列；`grid.resize(!is_alt, …)` 主屏 reflow、备用屏不 reflow） | 等价；像素维只影响 CSI 14/16t 应答（②按名义值 8/16 自答） |
| 回滚重排 | resize 重排回滚 | `reflow=true`（主屏）重排 | 同 |
| 损坏语义 | DirtyFull（滚动常态）→ DirtyRows 全视口行 | `scroll_up/down_relative` 均 `mark_fully_damaged()` → `damage()`=Full | 同；且沿用 Go v4 整改口径：**Full 也走行差分**（全视口行、不带镜像），全量只留给首次/备用屏进出/裁剪/背压/客户端请求 |
| 回滚上限 | `SCROLLBACK_MAX_LINES`（默认 10000） | `Config.scrolling_history` | 同值同 env |
| 滚动读取 | scroll viewport API（滚上去读视口再滚回） | grid 直接按 Line 索引（display_offset 不动） | **更干净**：MirrorRows/RowsAt = 直接索引区间，无「滚回原位」纪律问题 |
| 回滚裁剪 | total 变小（page 粒度） | 历史超限时旧行被丢 | total 单调性同：`total_lines()` 只增或持平、裁剪时变小 ⇒ 腿侧 `noteScrollbar` 全量重建判据照搬 |

回滚条（wire `scrollbar`）：`total = total_lines()`（含视口）、`len = screen_lines()`、
`offset = total - len`（服务端永不滚视口 ⇒ 恒贴底；与 Go 在镜像读取路径外的行为一致——
Go 的 offset 变化来自滚动输出推进 total，同式成立）。`AtBottom` 恒 true。

### 1.3 标题 / OSC 7 / 密码输入等 ghostty 有而 alacritty 无的面

- **标题**：Go 的单一来源本就是 termScan（design D1），不读 vt 标题 ⇒ 无缺口。
- **OSC 7 pwd**（LIST JSON `cwd`）：Go 读 vt 的 PWD 查询；alacritty 无 ⇒ **旁路扫描器补 OSC 7
  解析**（存原始值，消费时走 Go `PwdPath` 同款剥壳：file:// 前缀、host 段丢弃、百分号转义）。
- **密码输入位**（光标 Password）：ghostty 由「前后无输出 + 屏幕无回显」推断；alacritty 无 ⇒
  恒 false（wire cursorFlags bit3 恒 0）。D-7 登记（客户端只拿它做输入框提示）。
- **光标形状**：`cursor_style()`（DECSCUSR）有；**闪烁位**无 ⇒ blinking = `CursorStyle::blinking`
  变体映射（alacritty CursorStyle 带 Blinking 标志，可用）。
- **kitty graphics / 图片协议**：Go 出口未装回调即不支持，无需对齐。

### 1.4 vt 底座自检（6b 验收）

fixtures/term-vt 三个会话夹具喂 `SessionVt`，断言：
1. damage 非空且行数 ≤ 视口行；
2. `screen_text()`（对齐 Go ScreenText：跳 skip 格、空符号补空格、行尾裁空白）非空且含夹具
   语义锚点（cjk 夹具含 CJK、git-log 含 commit 行、hexdump 含 `|`）；
3. golden 对拍三步（见④）：文本 digest / 样式向量 digest / 光标+回滚条+模式位 == manifest.tsv。

## 二、自建应答器（必答②）

行为真源 = libghostty-vt（Go 绑定只装 write_pty sink，**未装任何 effects 回调** ⇒ 全部应答走
ghostty 默认值；helpers.h 仅有 set_write_pty/set_clipboard_*/set_default_colors 四个注入点）。

识别与应答面（`term/responder.rs`，纯函数 + 会话态注入）：

| 查询（应用 → 出口） | 触发解析 | 应答字节面 | 依据 |
|---|---|---|---|
| DA1 `CSI c`（无参/0 参） | csi_dispatch action='c'，无 intermediates/`>` | `\x1b[?62;22c` | **实测向量**（term_responder.json da1/da1_zero） |
| DA2 `CSI > c` | prefix `>` | `\x1b[>1;0;0c` | 实测（da2/da2_args 同答） |
| DA3 `CSI = c` | prefix `=` | `\x1bP!\|00000000\x1b\\` | 实测 |
| XTVERSION `CSI ? 65 c` | `?` + 65 | **不答**（设计期预期答 `libghostty` 系误判；实测向量 xtversion 空） | 实测 |
| DSR-OS `CSI 5 n` | action='n' param 5 | `\x1b[0n` | 实测 |
| DSR-CPR `CSI 6 n` | param 6 无 `?` | `\x1b[<y+1>;<x+1>R`；DECOM 置位时 y 按滚动区顶折算（实测 origin_scrolled：滚动区 5..20 + `\x1b[3;4H` → `[3;4R` = 相对坐标） | 实测（dsr_cpr 三态） |
| DECXCPR `CSI ? 6 n` | `?`+6 | 不答 | 实测 |
| DECRQM `CSI ? Pm $ p` | intermediates 含 `$`，action='p'，带 `?` | `\x1b[?<mode>;<state>$y`；state：已知私有模式 1/2，未知 0，永久态 4 | 实测（1000 开/关、9999、117→4、7→1） |
| DECRQM ANSI `CSI Pm $ p` | 同上无 `?` | **不答**（设计期预期应答系误判；实测 insert/lnm/未知全空） | 实测 |
| kitty 查询 `CSI ? u` | `?` action='u' | `\x1b[?<flags>u`（flags=0 也显式编 `?0u`） | 实测（默认/各档/push-pop 后） |
| OSC 10/11/12 `?` | osc_dispatch 参数 `10;?` 等 | **未上报主题（SetDefaultColors 未设）⇒ 不答**（V-2 实测定案：不是答默认色值）；上报后按上报值答 `\x1b]10;rgb:RRRR/GGGG/BBBB\x07`（8bit×257） | 实测（osc10_default 空 vs osc10_theme） |
| OSC 12 `?`（光标色） | 同上 | 同上格式；上报主题时**答前景色**（实测 osc12_theme = fg 值） | 实测 |
| OSC 4 `idx;?` | osc_dispatch 4 号 | `\x1b]4;<idx>;rgb:…\x07`：set 过的索引回**镜像值**（实测 set rgb:12/34/56 → 答 1212/3434/5656）；未 set 的回 ghostty 内置调色板（实测 idx5=b2b2/9494/bbbb、idx1/2 有值——**真表已在向量里**，D-9 关闭） | 实测 |
| 像素尺寸 `CSI 14 t`/`CSI 16 t`/`CSI 18 t` | action='t' | **不答**（设计期预期按名义 cell 自答系误判） | 实测 |
| 标题上报 `CSI 21 t` | param 21 | **不答**（DEC 21 开后同样不答） | 实测 |
| ENQ 0x05 | print/dispatch 0x05 | 不答 | 实测 |
| kitty 键盘 set/push/pop `CSI > flag u`/`CSI < n u`/`CSI = flag u` | csi_dispatch | 不产生应答，只改 TermMode（alacritty 原生处理）；`CSI > 4;N m` 由 probe 记账 modifyOtherKeys（N==2 置位、其余/复位/RIS 清零） | 实测（query 应答见 kitty 行） |
| XTQMODKEYS `CSI ? 4 m` | csi_dispatch | **不答**（herdr 补丁 0002 的查询面在现产线未暴露） | 实测 |
| OSC 52 剪贴板 | alacritty Handler 原生 `clipboard_store/load` 事件 | 经 EventListener 捕获 → `clipChan`/读缓存（不走 write_pty）；`VoidListener` 换成 `TermSink`（EventListener impl 只收 clipboard 事件） | 对齐 Go 双向语义 |

应答抑制（对齐 Go 任务 5.1 窄规则）：腿集合里存在 `capsRawTerminal` 声明腿 ⇒ probe 停止
产生应答（等价 SetResponseSink(nil)）；该腿全走后恢复。窗口期丢弃不补答（Go 已知窗口，同）。

## 三、自建键/鼠标/焦点编码器（必答③）

行为真源 = libghostty-vt `src/input/key_encode.zig`（2,844 行）+ `mouse_encode.zig`（781 行）+
`function_keys.zig`/`kitty.zig` 表。**Go 侧调用形态**：`EncodeKey` 传
`{Key(W3C code u16), Action, Mods, Text}`——**UnshiftedCodepoint 恒 0**（wire 不带该字段，
term_vt.go 组 KeyEvent 时不填）⇒ 我们实现同样按 0 处理（kitty 表查不到且无 unshifted 的文本键
按纯文本直发）。macOS 专属分支（super 抑制文本、option-as-alt）按宿主平台条件编译对齐
（ghostty 编译期分支；darwin 出口须同形，D-10 登记）。

### 3.1 键（`term/keyenc.rs`，Options 从 TermMode 快照）

决策树（自上而下，任何一步产出即止）：

1. `kitty_flags != 0` → kitty 路径：
   - release 且无 REPORT_EVENT_TYPES → 无输出；release 且无 REPORT_ALL_KEYS 时
     enter/backspace/tab → 无输出；
   - 表项 = kitty_entries 按 W3C key 匹配（功能性/预定义键），否则（unshifted 恒 0）无表项；
   - composing（IME 组段中）且非纯修饰键 → 无输出；
   - utf8 非空且 key∈{enter,backspace}：控制字符 utf8 → 走后续；enter 带文本 → 直发文本；
     backspace 带文本 → 无输出（IME 修正形态）；
   - 无 REPORT_ALL_KEYS 时：无绑定修饰的 enter/tab/backspace → `\r`/`\t`/`\x7f`；
     纯可打印单段 utf8 且无绑定修饰且非 release → 直发文本；
   - 无表项：release → 无输出；utf8 空 → 无输出；否则直发文本（纯文本事件）；
   - 表项为纯修饰键且无 REPORT_ALL_KEYS → 无输出；
   - 序列 `CSI key[:alt1[:alt2]] ; mods[:event] ; text… u`（或 `~` 终止符表项）：
     mods = 1 + bitfield(shift1|alt2|ctrl4|super8|hyper16|meta32|caps64|num128 —— 输入 wire 的
     10 位 mods 收进 shift/alt/ctrl/super/caps/num 六位，hyper/meta 输入面不存在恒 0）；
     event 仅 release=3/repeat=2 且 REPORT_EVENT_TYPES 时编 `:2`/`:3`（press 的 `:1` 不省略——
     对齐 ghostty「Note that Kitty omits :1 … We'll include it」注记：press 也带 `:1`）；
     alternates：REPORT_ALTERNATE_KEYS 时 utf8 首码点为 shifted 形态且 shift 置位 → alt1；
     基座码点（W3C key 的 codepoint）≠ key → alt2；REPORT_ASSOCIATED_TEXT 且非 release 且
     修饰不阻文本（ctrl/super 恒阻、alt 按 D-10 平台口径）→ text 段码点列表（控制码剔除）。
2. legacy 路径（**决策优先级以 term_keyenc.json 实测为准**——下列描述已按 349 案向量校正）：
   - 仅 press/repeat；composing → 无输出；
   - **PC 功能键表**（function_keys.zig：光标键 DECCKM 分 SS3/CSI、F1-F12 修饰族、小键盘
     DECKPAM 分流含 1035/numlock 语义、backarrow DECBKM 0x08/0x7f、modifyOtherKeys 抑制位）
     按「绑定修饰精确匹配 + 光标/小键盘/修饰位过滤」取序列；
   - **ctrl+字母不走 C0**（实测 plain 模式 ctrl+a → `\x1b[1;5u` = fixterms CSI u；C0 直发仅
     控制字符 text 形态在 kitty 全开下出现——kitty31 ctrl+a → `\x01`）；alt 置位且无其它
     修饰 ⇒ alt 修饰被丢弃、直发文本（实测 alt+a → `a`）；
   - **modifyOtherKeys mode 2**（`CSI > 4;2m` 后）：绑定修饰命中 → `CSI 27;<modcode>;<codepoint>~`
     （实测 ctrl+a → `27;5;1~`、shift+a → `27;2;65~`；**ctrl+alt 只叠 ctrl 位**——alt 不进 modcode）；
   - darwin 且 super → 无输出（D-10）；其余直发 utf8。

### 3.2 鼠标（`term/keyenc.rs` 同文件 mouse 段）

模式判定（TermMode 位 → ghostty MouseEvent/Format 二维）：1000/1002/1003 → press/drag/motion
上报面，X10 单独；格式按 1006(SGR)/1005(UTF8)/1015(URXVT)/默认 X10 三字节。
**D-4 影响面**：URXVT 位 alacritty 无 ⇒ 1015 形态不可达（恒走默认/X10），登记。
字节面：

- SGR：`CSI <btn;X+1;Y+1M|m`（release 用 m）；btn = 按钮码 + 修饰(shift4/meta8/ctrl16) + motion 位 32 +
  wheel 位 64（4/5 键；实测 wheel four=64、five=65）；
- X10 三字节：`ESC[M` + `32+btn` + `32+X+1` + `32+Y+1`；**1000 模式 release 报 btn=3（`#`）**（实测）；
  **坐标 X 或 Y ≥ 222 即不报**（实测 press_left_edge 222 → None，对 SGR 格式同样生效——
  坐标检查在格式选择之前）；
- UTF8：同 X10 但坐标按码点编码；
- 上报范围规则（shouldReport，实测钉死）：X10 只报左/中/右 press；normal 只报非 motion
  （motion 不报）；motion 模式按 any_button_pressed 与视口内规则；**同格 motion 去重**：Go 的
  setopt_from_terminal 是否带 last_cell 未传（input.go 未设 OPT）⇒ 按无去重实现（同 Go 实跑），
  D-11 登记复核。

### 3.3 焦点与粘贴

- 焦点：模式位 FOCUS_IN_OUT 开 → `CSI I`/`CSI O`（调用方判模式，编码器恒可产）；
  focusNudge（attach 首 raw 腿回放后注 focus-in、末腿离开注 focus-out）同 Go。
- 粘贴：bracketed（2004 开）时 `\x1b[200~` + 文本 + `\x1b[201~`，**分帧续接语义**（首片开、
  末片闭、中片裸文本——textPasteMore/Cont 位驱动），对齐 EncodePastePart。

### 3.4 单测对齐面（6c 验收；向量**已产出**，见 6c 提交）

- 键编码对照 `fixtures/vectors/term_keyenc.json`（47 键码表 + 14 模式 349 案，克隆 harness
  直调 `vt.Terminal.EncodeKey` 产）：覆盖 legacy 全功能键栏、DECCKM 两态、kitty 五位及组合、
  REPORT_ALL/TEXT 各形态、modifyOtherKeys on/off、ctrl/alt 组合、release/repeat、composing、
  小键盘 DECCKPM、修饰键本身。Rust 对照逐字节。
- 应答器对照 `term_responder.json`（43 案，克隆 harness 喂查询串收 write_pty 字节）：
  DA 族/DSR 族（含 DECOM 两态）/DECRQM（私有开/关/未知/永久态 + ANSI 面不答）/kitty 查询
  各档/OSC 颜色（默认不答 + 主题应答 + 镜像 + 未 set 基表）/尺寸与标题上报不答/ENQ/同批
  多查询分片语义。Rust 对照逐字节。
- 鼠标对照 `term_mouseenc.json`（10 模式 160 案）：SGR/X10/UTF8 × press/release/motion ×
  wheel × 修饰 × 坐标边界（origin/edge/over）。

## 四、surface v4 产出端字节面（必答④）

### 4.1 布局（编码器 `term/codec.rs`，逐字段对齐 term_surface.go）

- 帧：`[op][len:2LE]`，surface 大帧 payload = `[flags:1][gzip]` 分片，片 ≤ 60KiB（fragChunk），
  more=bit0；空数据也发一片。gzip 用 flate2 GzBuilder（mtime=0、OS=255、level 6 默认）——
  与 Go 产物的 **gzip 字节不必逐位同**（deflate 实现差异），消费方解压后比对（golden 的
  digest 断言即解压后口径）；flate2 头域与 Go gzip 默认头同为 OS=255/xfl=0，尽力一致。
- SNAPSHOT 体（v4）：`[ver:1=4][rev:u32][cols:u16][rows:u16][cursor x,y,flags,shape][modes:u32]
  [kitty:u8][misc:u8][scroll total:u64 offset:u64 len:u16][titleLen:u16][title][gridLen:u32][grid]
  [mirrorLen:u32][mirror]`。
- DIFF 体（v4）：`[ver][rev][cols][rows][rowCount:u16][cursor 4B][modes:u32][scroll 18B]
  [rows…]`；**rowCount=0 合法**（光标/模式位/回滚条变化）。
- FETCH-ROWS 请求 `[from:u64][count:u16]` / 应答 `[ver][rev][cols][rows][from:u64][count:u16][rows…]`。
- cell 行编码（cellcodec）：`[ver:1][cols:2][rows:2]` + 每行 `[y:2][格流]`；格流三标记：
  `0x00 空白游程+varint`、`0x01 完整格`（`[symLen|skipBit:0x80][sym][fg kind][…][bg][attr:2LE]`）、
  `0x02 重复游程+varint`；颜色 kind 0/1(palette+idx)/2(rgb+3B)；行序列（差分用）无头版同构。
- INPUT 上行 4 类（key/text/mouse/focus）+ THEME/CLIPBOARD/NOTIFY 载荷、HELLO 尾随
  `[capLen][caps][ver?][idLen][id]`、caps 位（surface=1、rawTerminal=2、protoVer=0x80）。

### 4.2 模式位映射（`surfaceModesOf` 单一映射）

modes u32：DECCKM=1、M1000=2、M1002=4、M1003=8、M1006=16、Focus=32、Bracketed=64、
Alt=128；kitty u8 原样；misc bit0 = modifyOtherKeys（自管位）。快照/差分/golden 样例共用
此一处映射（Go 的「单一映射」纪律照搬）。

### 4.3 golden 夹具消费表（每个 fixture 的用途与判据）

| fixture | 消费方式 | 判据 |
|---|---|---|
| `term/frames.v1.jsonl`（13 案） | Rust 帧解码器单测：逐案解 payloadHex，比对 expect 结构（greeting/hello/resize/data/replay-done/…） | 13/13 全绿 |
| `term-vt/session-{cjk,git-log,hexdump}.bin` | ① vt 底座自检（1.4）；② surface 产出端端到端：喂 SessionVt → 产 SNAPSHOT 帧 → 自解 → 文本 digest/样式向量 digest/光标/回滚条/模式位 对拍 manifest.tsv 同名列；③ 差分：快照后追加「GOLDEN-DIFF-追加」至多 4 次（脏行非空且光标变化止）→ 产 DIFF 帧 → 应用后 digest 对拍 `-diff` 行 | 8 行（4×快照+差分）digest 全对；样式向量逐格口径 = Go goldenStyleText（26 hex/格） |
| `term-manifests/*.toml`（24 文件） | ① 引擎加载：include_str! 直嵌（路径指 fixtures/ 单真源）+ `<state>/agent-detection/` 覆盖优先；② 计数门 = index.toml id 集 = 22；③ 校验上限（规则 128/门深 8/门 512/匹配器 32/1024/长度 512、deny_unknown、正则可编译） | `检测规则已加载 22 份` 判据行同串 |
| `term-manifest/{codex,opencode}-startup.txt` | 引擎求值单测：喂 manifest.Input（screen=夹具文本）→ 命中规则/状态与 Go 同输出（Go 侧对照向量由克隆 harness 产） | 同输入同输出 |
| `surface-golden/manifest.tsv + *.bin` | ① 解码侧：Rust 实现 fragAssembler + 体解码，吃 Go 产的 .bin（块格式 `[u32 帧数]{op + [u32 片数]{[u32 len][bytes]}}`），断言 digest 列——证明我们的解码吃得了 Go 的字节；② 产出侧：4.3 上排的端到端对拍——证明我们的编码产得出同 digest 的字节 | 两向全绿 |
| `surface-golden/surface_input_cases.tsv`（19 案） | INPUT/HELLO-tail/caps/theme/clipboard 载荷解码单测（hex 列 = 期望 wire 字节） | 19/19 |
| `vectors/`（R0 既有） | 不变 | — |

### 4.4 生成向量（tools/vector-gen，都从克隆真源产；term 三件已产出）

- `vectors/surface_codec.json`：固定 cell 输入 → `vt.EncodeGrid/EncodeRows/DecodeGrid`（pkg/term/vt
  导出面）逐字节；SNAPSHOT/DIFF 体（encSnapshotBody/encDiffBody 为未导出 ⇒ 模板测试文件进
  克隆内跑，R0.4 同法）→ 字节 + 字段表。**这是「逐字节」的格式锚**——与仿真器无关。（6d 产）
- `vectors/term_keyenc.json` + `term_responder.json` + `term_mouseenc.json`：**已产出**（见 3.4）。
- `vectors/term_manifest_eval.json`：两份 startup 夹具 × 22 manifest 的求值轨迹（matched 规则/
  状态/可见位），Rust 对拍。（6f 产）

## 五、会话面（必答⑤）

`term/session.rs` + `term/leg.rs` + `term/surface.rs`（对齐 service.go/term_leg.go/
term_surface_session.go/term_surface_leg.go/term_state.go/agent.go）：

- **多腿模型**：`HashMap<String, Session>` + 每会话 `Vec<Leg>`；注册序 = ①同 clientID 替换
  （ENDED self_reconnect）→ ②takeover 全踢（ENDED replaced）→ ③上限腾位（停滞最久/最久
  空闲，裸断无 ENDED）→ ④入表 + 活动选举（单调 activitySeq + attachSeq tie-break）。
  ENDED code：子进程退出码 ≥0 / -1 replaced / -2 killed / -3 service_stopped；
  reason 词表 {replaced, self_reconnect}（硬词表，扩值先扩表）。腿 kind = app/host/legacy。
- **raw 腿**：握手计划（ATTACHED → `\x1b[3J\x1b[2J\x1b[H` → 回放 DATA（16KiB 分片、2s 预算、
  cutByDone 断点语义）→ REPLAY-DONE(flags: truncated|sizeChange) → 首腿 focus-in）+ 实时循环
  （控制帧优先 + 字节环追赶，`off` 只由本腿写者推进）；写停滞语义：超时退避重试同一片
  （部分写续帧 torn 状态）、连续停滞超 60s 断腿（无 ENDED）；ENDED 前排干 [off, written)。
- **surface 腿**：每腿基线/revision/needSnapshot/背压（单帧 4MiB、队列 8MiB）在腿上；flush
  遍历全腿（脏行集每拍一份）；合并窗 16–33ms；`stateUnchanged`（光标/模式/回滚条比对）+
  rowCount=0 合法帧；回滚 total 变小 ⇒ 强制全量（绝对行号滑动）。
- **环形历史**：1MiB 默认（HOMEWAY_TERM_HISTORY），written/start 绝对偏移；replay 窗 256KiB
  尾部优先 + 行边界/ESC 对齐（4096 前看）；epoch 表（≤64）记尺寸变化点。
- **spawn**：登录 shell 解析链（账号库 dscl//etc/passwd → $SHELL → 平台默认；可执行校验）；
  `shell -l` 或 `HOMEWAY_TERM_SHELL` 时 `shell -lc '<cmd>'`；环境白名单（HOME/USER/LOGNAME/
  TMPDIR/SSH_AUTH_SOCK/PATH/LANG/LC_*）+ 强制 TERM=xterm-256color/COLORTERM=truecolor/
  TERM_PROGRAM=Tailcat/TERM_SESSION_ID=tailcat-<name>；cwd=$HOME。**portable-pty 0.9**
  `CommandBuilder`（env_clear + 白名单注入）+ `openpty(PtySize)` + `spawn_command`；
  `master.process_group_leader()`（tcgetpgrp）供检测；resize 用 `PtySize` setsize。
- **采样与检测**：1s tick；进程表（linux 读 /proc；darwin 跑 `ps` 同 Go 口径）；五路证据融合
  权威序（OSC 21337 直报 > blocked 屏幕证据 > 输出腿 > idle 屏幕证据 > 回落）；输出腿 =
  3s 窗 300B 阈值 + 秒桶；CPU 腿仅 shell/other（10 刻度 = 100ms）；磁滞 quiet≤2 维持 working；
  状态机卫生三机制常量同值（3 拍/700ms/800ms）。
- **LIST/NEW/KILL/CREATE/EXPLAIN 命令面**：LIST JSON 字段序 = Go struct 序
  （name,createdMs,lastActiveMs,attached,agent,stateV2,title,cwd?,cols,rows,pid,clients[]；
  clientEntry: kind,cols,rows,sinceMs,active）；CREATE `[flags][nameLen][name]` bit0 =
  reuse-if-exists（**极性与 HELLO bit1 相反**）；EXPLAIN = explainJSON（在线快照跑 runExplain）。
- **UDS 服务装配**：`listen_local_service(serve_dir, "term.sock")`（R3 已有：死活判别 +
  chmod0600）+ 每连接一线程（files 同款）；engine.rs 摘掉「term 不起不建 sock」注记，挂
  TermService::serve_stoppable；`HOMEWAY_TERM=off` 全关（同 Go 唯一关闭方式）。
- **错误码词表**（只增不改）：already_exists/detect_off/marshal/no_agent/no_session/no_vt/
  spawn_failed/too_many/too_many_clients/term_version/bad_op/bad_name/bad_hello/bad_capability/
  bad_create/bad_resize/bad_input/bad_fetch/invalid_name/surface_unavailable。

## 六、模块地图与拆步映射

```
crates/homeway-core/src/term/
  mod.rs          服务门面（TermService::spawn/listen/close）
  frames.rs       帧族编解码（6d）        scan.rs      旁路扫描器+OSC7 pwd（6f）
  vt.rs           SessionVt（1.1）（6b）  responder.rs 应答器（6c）
  keyenc.rs       键/鼠/焦/贴编码（6c）   codec.rs     surface 体+cell 编码（6d/6e）
  session.rs      会话+ring+采样（6f）    leg.rs       腿/队列/写者（6f）
  surface.rs      投递编排（6f）          agent.rs     检测融合（6f）
  manifest/       引擎+数据（6f）         pty.rs       spawn/env/shell 解析（6f）
```

依赖新增（workspace.dependencies）：`alacritty_terminal = 0.26`（带出 vte 0.15、
unicode-width 等）、`portable-pty = 0.9`、`flate2 = 1`（gzip；不启 zlib-ng，纯 rust 后端保
OS=255 头）、`fnv = 1`（golden digest）。regex：manifest 方言垫片（Rust regex vs Go RE2 —— 
**已有先例**：Go 侧 dialect.go 本身就是「Rust regex → Go RE2」的垫片，Rust 侧用 regex crate
天然在「herdr 原方言」上，但与 Go RE2 语义可能有 Unicode 类/lookaround 差异——Go dialect
垫片保证了规则集已限定在两方言交集内，Rust 侧直接 regex::Regex 编译即可，登记校验：全部
22 份 manifest 在 Rust regex 下编译通过即证）。

## 七、风险与登记表（评审重点）

| # | 风险/差异 | 定性 | 处置 |
|---|---|---|---|
| D-1 | alacritty 与 Go 的坐标系关系（设计期误判为反向） | 实现陷阱 | **实现核实（6b）**：alacritty Line(0)=视口顶、负数进回滚，与 Go **同向**，无需换算；唯一换算点 = 绝对行号⇔Line。golden 光标/回滚列对拍全绿钉死 |
| D-2 | 空白格/带样式空格的 Symbol 归一化（ghostty 内部形态未文档化） | 仿真等价 | **V-1 已裁决（6b golden 全绿）**：空格（显式或未写）一律无字素（symbol 空）；带样式空格 width=0、无样式空格 width=1（blankRun 族）；占位格 skip。alacritty 无法区分「显式无样式空格」与「未写格」⇒ 统一走空白格（渲染一致，并入 D-13 登记） |
| D-3 | OSC 10/11 默认色（未上报主题时）ghostty 缺省值未知 | 应答字节 | **V-2 实测定案（6c 向量）**：未设 SetDefaultColors 时 OSC 10/11/12 查询**不应答**——无需默认色值表；Rust 同形（theme 状态三态：未上报/已上报/换帧） |
| D-4 | X10 鼠标位（DEC 9）/URXVT（1015）alacritty 无位 | 模式位/编码不可达 | wire 位恒 false：M1000 位语义等价（Go 也 OR 两位）；URXVT 编码恒不走。真机 TUI 极罕用，登记豁免 |
| D-5 | SGR 53 overline 无位 | 样式位丢失 | attr bit7 不可达；golden 不含；登记，遇真需求补 |
| D-6 | damage 含光标行（比 Go 多发一行） | 安全超集 | 观测项 |
| D-7 | 光标 Password 位恒 false | 提示位降级 | 登记 |
| D-8 | OSC 12 光标色近似 | 应答值 | **实测定案**：ghostty 答前景色（向量 osc12_theme），Rust 同形即精确对齐，登记关闭 |
| D-9 | OSC 4 未 set 索引答 xterm 基表 vs ghostty 自有调色板 | 应答值漂 | **实测定案**：向量已采 ghostty 内置调色板样本（idx1/2/5），Rust 按向量值内嵌同表，登记关闭 |
| D-10 | darwin 平台分支（super 抑制文本、alt 关联文本口径） | 平台条件 | 按 ghostty 编译期分支同形实现；linux 出口无此面 |
| D-11 | 鼠标 motion 同格去重未确认（Go 未显式设 last_cell） | 编码频率 | 按「不去重」实现（同 Go 实跑），克隆 harness 验证 |
| D-12 | gzip 字节不逐位同（deflate 实现差） | 非契约层 | 契约 = 解压后字节 + digest；flate2 尽力同头 |
| D-13 | zerowidth 组合字符 vs ghostty 字素簇 | 仿真等价 | cjk 夹具 digest 对拍覆盖（6b 已绿）；「显式空格 vs 未写格」不可区分并入本条登记 |
| D-14 | 键编码向量 harness 依赖 cgo 克隆构建 | 工程量 | R0.4 模板法复用；只跑一次产 JSON |
| D-15 | 未配对 `CSI ? 1049 l`（收到退出但不在备用屏）：ghostty 按 DECRC 未保存 ⇒ 光标复位 (0,0)；alacritty no-op（**6b 实现中发现**，golden session-styles 光标形态钉住） | 仿真等价 | vt.rs 拦截：unset SwapScreenAndSetRestoreCursor 且不在 ALT_SCREEN ⇒ goto(0,0) |
| D-16 | 主屏 `CSI 2J`（ED2）：alacritty clear_viewport 把视口推入回滚（xterm 形态）；ghostty 只清视口不增回滚（**6b 实现中发现**，golden 回滚 total 列钉住） | 仿真等价 | vt.rs 拦截：主屏 ED2 = reset_region(..) 不进历史；备用屏仍走 alacritty 原生 |
| D-17 | 鼠标上报三模式（1000/1002/1003）：ghostty 独立记账（可叠置）；alacritty 互斥单值（设 1002 清 1000）（**6b 实现中发现**，golden 模式位列钉住） | 模式位 | vt.rs 自管 mouse_flags 三位、不委托 alacritty 的 MOUSE_MODE 位（编码器只读本层快照，互斥态不影响行为） |
| V-2 | （D-3 的实测项） | — | 6c 首批动作 |

**退出口**（承 ROADMAP）：键编码器兼容面（vim/htop/kitty 查询实测）超支 → 「基础编码先行 +
kitty 全量挂 R6.5」，不阻塞其它步。

## 八、验收判据映射（对齐 ROADMAP R6）

1. golden 全对齐：4.3 消费表全绿（含样式向量逐格）。
2. Go term CLI（克隆构建 `homeway term attach/list/new`）消费 Rust term 服务：列表/新建/
   attach/重放/退出/KILL/多腿接管 ENDED 归因全流程判据行同串（6g，`tools/local-rust-exit.sh`
   起本地实例）。
3. 检测三态同输入同输出：manifest 评测向量 + startup 夹具对拍。
4. 应答器/键编码器单测：3.4 向量集逐字节。

---

## 九、门一评审整改的设计增量（2026-10-04 第二会话补，A1/A2/A5 清账）

> 本节补门一评审（`R6-gate1.md`）的「6d/6f 前置三件」+ 顺手修 A3/A4/A9 描述。**全部条款的
> 行为真源 = baseline 克隆源码**（本节引用处已逐条对读）；6d/6f 实现照本节走，不再现读。

### 9.1 A5｜帧总表（6d 规格；`pkg/term/frames.go` 逐条）

帧 = `[op:1][len:2 LE][payload]`，`len ≤ 65535`（**超限截断不是拒帧**：`encodeTermFrame`
直接 `payload[:65535]`）；DATA 由发送方按 ≤16KiB 分片。op 表（两方向共字节空间，
靠连接方向区分）：

| op | 名 | 方向 | 载荷布局 |
|---|---|---|---|
| 0x00 | HELLO | C→S | `[cols:2LE][rows:2LE][flags:1][nameLen:1][name]` + 尾随块（下 9.2） |
| 0x01 | DATA | 双向 | 原始 PTY 字节（≤16KiB/片） |
| 0x02 | RESIZE | C→S | `[cols:2LE][rows:2LE]` |
| 0x03 | ENDED | S→C | `[code:4LE][reasonLen:1][reason≤200]`（code 词表见 9.4） |
| 0x04 | LIST | C→S / S→C | 请求空载荷；应答 = LIST JSON（外壳 `{"sessions":[…]}`，字段序见 §五） |
| 0x05 | KILL | C→S | `[nameLen:1][name≤64]`（encName） |
| 0x06 | ERROR | S→C | `[codeLen:1][code≤255][msgLen:2LE][msg≤4096]` |
| 0x07 | STATE | S→C | `[agent:1][state:1][titleLen:2LE][title≤512]` |
| 0x09 | ATTACHED | S→C | `[cols:2LE][rows:2LE][modes:4LE][agent:1][state:1][name]`（name 无长度前缀，吃尽余量；头部 ≥10B，短了 ok=false） |
| 0x0A | REPLAY-DONE | S→C | `[replayed:4LE][flags:1]`（bit0 头部截断 / bit1 回放跨尺寸变化） |
| 0x0B | OK | S→C | 空载荷（一锤子命令的应答壳） |
| 0x0C | GREETING | S→C | `[ver:1=1][features:4LE]`（features = 0x7F：list1\|replay2\|modes4\|agent8\|title16\|surface32\|protoVer64） |
| 0x0D–0x0F | SNAPSHOT / SNAPSHOT-DONE / SURFACE-DIFF | S→C | §4.1（0x08 历史保留位绝不复用） |
| 0x10 | FETCH-ROWS | C→S / S→C | 请求 `[from:8LE][count:2LE]`（`surfaceFetchRowsMax=512`）；应答体 §4.1 |
| 0x11 | INPUT | C→S | 首字节 kind：0 键 `[key:2][mods:2][action:1][utf8Len:1][utf8]`、1 文本 `[flags:1][len:2LE][text]`（bit0 粘贴/bit1 More/bit2 Cont）、2 鼠标 `[action:1][button:1][mods:2][x:2][y:2]`（网格坐标）、3 焦点 `[gained:1]` |
| 0x12 | THEME | C→S | 默认色上报（§4.1） |
| 0x13 | CLIPBOARD | 双向 | OSC 52 转发（§4.1；65532/4096 截断在载荷层） |
| 0x14 | NOTIFY | S→C | OSC 9 转发 `[len:2LE][payload≤4096]` |
| 0x15 | FETCH-SNAPSHOT | C→S | 空载荷（断档/拒收后请求全量） |
| 0x16 | EXPLAIN | 双向 | 诊断面：请求空；应答 explainJSON |
| 0x17 | CREATE | C→S | `[flags:1][nameLen:1][name]`（bit0 = reuse-if-exists，**极性与 HELLO bit1 相反**：置位=存在则复用、不置位=存在则 already_exists）；应答 OK/ERROR |

截断上限单表：`termMaxPayload=65535`、`termDataChunk=16KiB`、name≤64、reason≤200、
title≤512、errmsg code≤255 + msg≤4096、clientID≤64、clip 65532、notify 4096。

### 9.2 A5 续｜HELLO 尾随块（`encHelloTail`/`decHelloTail`，FIX-29）

形状（顺序固定，三段都可省略）：`[capLen:1][caps:capLen][ver:1?][idLen:1][clientID:idLen]`。
- caps 位：surface=1、rawTerminal=2、**protoVer=0x80（bit 7）**——位只增不改；
- **ver 字节只在 caps 带 protoVer 位时出现**（位即信号，不做位置推断）；声明位在而版本
  字节缺失 = 畸形（拒收，不静默补默认）；
- 解析**必须恰好耗尽**尾随字节（残留/超长声明一律 `bad_capability` 拒腿）；
- 空 caps 块（capLen=0）消费 1 字节、形状合法；
- **同形不可判别约束落在编码侧**：带 ID 必带 caps 块（不产出「无 caps 的 ID」）；
- 服务端版本门（`service.go` ServeConn）：caps 声明 protoVer 且版本 ≠ 1 ⇒ `ERROR(term_version)` 拒腿。

HELLO flags：bit0 create（既有）／bit1 only-if-absent（存在则 `already_exists`）／
bit2 takeover（显式接管，ENDED reason=replaced）。

### 9.3 A3｜ENDED code 与 reason 词表（冻结枚举，term-host-cli design D8）

code（`[code:4LE]`）：**≥0 = 子进程退出码**；-1 replaced；-2 killed（App KILL）；-3
service_stopped。`math.MinInt32` 是「不发 ENDED」哨兵（硬错误/客户端已关——裸 EOF）。

reason 只在 code=-1 时有值且是**受控硬词表**（扩值 MUST 先扩表）：
`replaced`（另一客户端显式接管）、`self_reconnect`（同实例标识重连替换）；
code=-3 的 reason 词面 = `service_stopped`（Go `service.go:452`
`sess.finish(termEndServiceStopped, "service_stopped")`——CLI 按 reason 渲染文案，
漏词会落「未知 reason」分支）。

stateV2 字节枚举（STATE/ATTACHED/LIST 共用）：unknown=0 / working=1 / blocked=2 /
idle=3；agent 枚举：shell=0 / codex=1 / claude=2 / opencode=3 / openclaw=4 / other=5 /
unknown=255（名字词面一一对应，只增不改）。

### 9.4 A4｜SNAPSHOT/DIFF 体的光标块 = 6B

§4.1 原文「DIFF 体 `[cursor 4B]`」有误：光标块 = `x:u16 + y:u16 + flags:u8 + shape:u8`
= **6B**（`term_surface.go:386-389`），SNAPSHOT 与 DIFF 同形。

### 9.5 A2｜manifest regex 方言垫片（Rust 侧同款近似，6f 规格）

规则集**不在**两方言交集内：`\p{Alphabetic}` 在 3 份 manifest 真用（antigravity/cursor/
qodercli）。「编译通过即证」不成立——Rust 原生 `\p{Alphabetic}` ⊋ Go 侧实跑口径。

**Rust 侧方案 = 复用 Go 同款近似映射**：`compilePattern` 单一入口先做方言翻译——
`\p{Alphabetic}`/`\P{Alphabetic}` → `\p{L}`/`\P{L}`（与 Go `propertyAliases` 同表；
`\p{L}` 是 Alphabetic 的子集 ⇒ 与 Go 出口的行为**逐规则一致**，而非「更准」）；
`\uXXXX`/`\u{XXXX}` Rust 原生支持、无需翻译（`\\` 字面反斜杠保护与 Go 同款，防
`\\u2800` 误翻）。`contains` 文本**不过**翻译（`\u` 在那边是两个普通字符）。
判据：`term_manifest_eval.json` 按**逐规则 region+matched** 对拍（不是只对状态）。

### 9.6 A1｜manifest region 层全集（6f 规格；`manifest/region.go` 逐函数）

region = 规则的**判定范围选择器**（旧提示残留不误判的锚定机制）。`Input` 四路：
`Screen`（屏尾纯文本，约一屏）、`OSCTitle`（OSC 0/2，termScan 维护）、`OSCProgress`
（OSC 9;4 原始载荷）。取不到（无提示框/无标记）按 herdr 口径**退回整屏或空**。

全集（12 具名 + 3 参数化；默认 `whole_recent`）：
- `osc_title` / `osc_progress`：走专用字段；
- `whole_recent`（默认）/ `after_last_prompt_marker`（最后一条 codex 提示行 `›`/`› ` 之后；
  无标记退整屏）/ `before_current_prompt_marker`（当前提示行之前；无则整屏）/
  `whole_recent_without_current_prompt_marker`（**有**当前提示行 ⇒ 空——提示输入态不当整屏证据）；
- `current_prompt_block_marker`（当前提示行上方最近的块标记行 `•■✗✓` 前缀，返回该行）/
  `after_current_prompt_block_marker`（该块标记行起至结尾）；
- `prompt_box_body`（提示框顶边框后到框内下一条分隔线前）/ `above_prompt_box`（顶边框
  之前；无框退整屏）/ `last_non_empty_above_prompt_box`（上述范围最后一条非空行）/
  `after_last_horizontal_rule`（最后一条水平分隔线之后；无则从 0 起=整屏）；
- 参数化：`bottom_lines(N)` / `bottom_non_empty_lines(N)` / `top_non_empty_lines(N)`
  （**拒绝前导 0**；N ≤ 65535 对齐 herdr u16::MAX；计数 parse 上限 1<<20 防病态）。

关键实现口径：行迭代对齐 Rust `str::lines()`（结尾换行不产生空元素）；「当前提示行」
= 最后一条提示行且**其后不得再出现块标记**（否则是历史提示）；水平分隔线 = `─` 开头且
（只含横线或横线 ≥3 条）；`top_non_empty_lines` 需 **MinEngineVersion ≥ 3**（加载门）。

加载面耦合：`index.toml` 的 `processes` 是选表入口（按前台进程名选 manifest）；
Validate 耦合 = `skip_state_update ⇒ state=unknown`；`top_non_empty_lines ≥3 行`约束。
判据：region 逐函数产向量（`term_manifest_eval.json` 按 region+matched 对拍，见 9.5）。

### 9.7 已知残余差异登记（编码评审 r1 收口，随实现更新）

- **DECSLRM（DECRQSS 载荷 `s`）恒答 `0$r`**：alacritty 不跟踪左右边距（DECLRMM 的
  模式位 69 有记账、边距值无从取）；ghostty 在 69 开启时答 `1$r{left+1};{right+1}s`。
  真应用不开 69——残余差异只在该形态（代码注记 + 本表双登记）。
- **解码面的非法 UTF-8 → U+FFFD**：Go 一律 `string(bytes)` 原样字节进 string；Rust
  解码面（name/title/clientID/msg 等）用 `from_utf8_lossy` 替换非法字节（长度与字节
  都可能不同）。name 有 `[A-Za-z0-9._-]` 词法校验兜底；title/client_id 非法字节属
  病态输入面。真源对齐优先级低于 panic 安全（见 r1-高① 的字节级截断整改）。
- **OSC 4 多索引的分片粒度**：vte 对 `4;1;?;2;?` 逐参分发（每参一片应答）、ghostty
  合成一条；拼接字节一致（wire 契约不受影响），harness 的 chunks 契约按「piece 只
  允许更细分」放宽对拍（r1-低11）。
- **（6f-3b 新增）explain 的 `rules` 空表序列化**：Go `[]evaluatedRuleOut` nil 切片
  序列化为 `null`、Rust 空 Vec 为 `[]`——只在「manifest 零规则」的病态形态可见
  （内嵌 22 份全非空；覆盖目录可造空表但 Validate 门会拒大部分形态）。wire 兼容
  （消费方按数组迭代）。
- **（6f-3b 新增）D-19 已处置关闭**：信号死退出码按 Go `-1` 语义映射
  （`pty.rs::wait`——`ExitStatus::signal()` 可辨）；残余 = portable-pty 不暴露信号
  数值（只给词面），EXIT 面不需要。正常退出码（0..255）两侧一致（6g 实测 exit 7/3）。
- **（6f-3b 新增）LIST JSON 的 cwd 来源**：Go 取 vt 的 OSC 7 解析、Rust 取旁路扫描器
  的 OSC 7（`scan::pwd_path`，Go `vtPwdPath` 同语义含百分号解码）——单来源等价；
  终端不发 OSC 7 时两侧同为缺省（字段 omitempty）。
- **（6f-3b 新增）surface 腿写超时 = 断腿**：Go 同款（停滞语义只给 raw 腿）——非差异，
  登记为口径注记（Rust 侧 `run_surface_writer` 的 Timeout 与 Hard 都断腿）。
- **（门二 r2 补登）plain_text 的折行展开口径**：Go `PlainText = format(unwrap=true)`
  的软折行展开按 ghostty formatter 实现；Rust `plain_text` 以 alacritty 的行尾
  WRAPLINE 位合并逻辑行（`vt::Row.wraps`）——两侧都是「折行合并 + 行尾裁」，
  差异面只剩极端字形簇拼合的字节级边界（不进判据）。
- **（门二 r2 补登）非 surface 腿的 surface 族上行帧**：Go 静默丢弃（`if
  client.surface` 无 else）；Rust 对齐为静默（无错误回执）——严格对齐，无差异。

### 9.8 A9/A6/A7 顺手修正

- 「24 文件内嵌」改述：**23 个 toml**（22 agent + index）+ README（`load.go` 的
  `//go:embed manifests/*.toml`）；
- env 面（A6）与剪贴板/应答抑制字节面（E3）留 6f 设计增量一并补（本节不展开）；
- §二总结句改述：「未装 effect（XTVERSION/尺寸/标题上报）⇒ 该族查询**不答**」（A7，
  表格已按向量改判）。
