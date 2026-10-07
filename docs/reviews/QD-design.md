# Q-D 终端子系统加固设计文档

> 批次：Q 批整改（`docs/REVIEW-ROADMAP.md` §Q-D）。第 1 棒（设计）产出。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`「Q-D 终端子系统」节 + 总表 **P0-3** + ROADMAP §Q-D。
> 基线：`git HEAD = bb2f357`（2026-10-08）。**行号均为本 HEAD 实测值**，实现以符号定位为准。
> 边界：只动 `crates/homeway-core/src/term/**`（+ `crates/homeway-core/tests/fuzz_replay.rs`、
> `fuzz/**`、`tools/gen-fuzz-seeds.sh`、`docs/**`）；两台生产出口、tier/homeway 两只读仓、
> `baseline/`、pin 全程不碰。
> **状态**：**v2——设计门（dsh 外部评审）已过**，见 §5；v2 已并入评审的全部中等/低意见
> （0 不认同；1.2 的镜像预算按评审建议落地为 F1c）。

---

## 0. 复验方法与本轮实测证据

### 0.1 方法

1. 逐条回源码重定位（审计行号已漂；本批涉及 `term/service.rs` 3763 行、`term/vt.rs` 1831 行）；
2. **两件独立实测**（临时工程 `/tmp/qd-repro`，path 依赖本仓 `homeway-core`；只读本仓、不改仓内文件）：
   巨型尺寸分配量级（RSS 实测 + `rustc -Zprint-type-sizes`）与指纹吞帧 A/B 复现；
3. 行为面回 **Go oracle**（`baseline/homeway/pkg/term/**`）与 **ghostty 上游源码**
   （`baseline/homeway/third_party/libghostty-vt/src/**`）；需求面回 tier `openspec/specs/`（只读）。
4. 🔎 条目按「机制是否成立 + 是否为移植缺陷」双判据复核；不成立的就地剔除并记录。

### 0.2 实测证据

**证据 A（分配量级；P0-3）** —— `cargo +nightly rustc -p alacritty_terminal --lib -- -Zprint-type-sizes`：

- `term::cell::Cell` = **24 B**（char 4 + Color 4 + Color 4 + Flags(u16) 2 + `Option<Arc<CellExtra>>` 8 + 填充）；
- alacritty `Term::new`/`Term::resize` 对**两屏都按 `rows × cols` 即时分配**
  （`term/mod.rs:415-416`；`grid/storage.rs:67-76` `resize_with(visible_lines, || Row::new(columns))`）。

`/tmp/qd-repro`（`src/bin/size.rs`）实测：

```
rss[before] = 2128 KiB
rss[after-65535x8] = 27552 KiB              # SessionVt::resize(65535, 8) 被原样接受
cells(一屏)=524280 两屏≈1048560  实测增量 24.8 MiB ⇒ 每格≈25 B（含两屏）
外推：65535×65535 = 4294836225 格 ⇒ 单屏 96.0 GiB，两屏 192.0 GiB（24B/格）
vt.size() = (65535, 8)                      # 无 MIN/MAX 夹取（实测）
```

⇒ **P0-3 成立且比审计写得更硬**：一次 `RESIZE 65535×65535` = **≈96 GiB 起的分配**（首屏先失败；
两屏 ≈192 GiB）。Rust 分配失败走 `handle_alloc_error` ⇒ **abort（非 unwind）**，`catch_unwind`
救不了；内存/swap 极大的机器上先经历长时间抖动。且 `apply_size_locked` 先 `pty.resize` 再
`vt.resize`（`:2779` → `:2782`）——壳/TUI 会先收到 65535×65535 的 `TIOCSWINSZ`。同进程的
serve/relay 数据面随之殉葬。

**证据 B（指纹吞帧；P0/P1）** —— `/tmp/qd-repro`（`src/main.rs`，A/B 同输入）：

```
B(不采样) dirty_rows.len() = 1  ← 行 0 文本 = "helloX"
A(采样) sampler screen_text = "helloX\n\n\n\n"
A(采样) dirty_rows.len() = 0  ← 现状丢帧
```

A 臂 = 在 `update()` 与 `dirty_rows()` 之间插一次 `screen_text()`（= 1 Hz 采样调用形状，
`service.rs:2824`）⇒ 采样提交指纹后，冲刷按指纹过滤（B3 机制）把该行从差分里静默剔除；
该行在下次变化前**永久不再下发**（光标位仍在推进 ⇒ 客户端字形/光标错位）。

**证据 C（Go/上游对照）** —— 本批行为面条目的 oracle 核对（评审已逐条复验，见 §5）：

| 面 | Go/上游 | 本仓现状 | 判定 |
|---|---|---|---|
| 腿淘汰排序 | `evictForSlotLocked` 读**实时** `l.out.isStalled()/stalledFor()`（`term_leg.go:927-948`）；`noteStall` 仅 raw 路径 4 站点（`:511/535/601/618`） | `evict_victim` 读会话侧 `stalled_ms`，该字段生产路径**从不写** | 移植缺陷（F4） |
| 尺寸 0 的会话几何 | `applySizeLocked` 先 0 门（`term_leg.go:909-916`），**会话尺寸唯一写点** `noteSizeLocked`（`service.go:733-739`）在门之后 ⇒ 0 上报不改会话尺寸 | 会话尺寸写在 `note_activity_internal`（`session.rs:583-597`），**在 0 门之前** ⇒ `RESIZE 0×0` 把会话几何污染成 0×0 | 移植缺陷（F1b） |
| `PlainText` 备用屏 | ghostty formatter **只格式化 active screen**（`src/terminal/formatter.zig:236-239/402/486`） | `plain_text → rows_at` 备用屏早退 ⇒ 恒空串 | 移植缺陷（F8） |
| surface 写超时 | `runSurfaceWriter`（`term_leg.go:272-302`）超时即断腿，**无** `noteStall`（R6-design §9.7 在案） | 同 | 同构；审计「surface 超时置位」建议**不采纳** |
| `encHelloFlags`/`encodeTermFrame` | `frames.go:222-230`（`p[5]=byte(len(name))`，模 256）、`:166-168`（载荷截 `termMaxPayload`） | `enc_hello` `:301`、`encode_frame` `:254` 同款 | **同构**，登记不改 |
| `isDescendantOf` / `readProcsPS` | 每调用建 map（`agent.go:423-441`）/ `strings.Join(f[4:], " ")`（`agent_unix.go:88-108`） | 同款 | **同构**，登记不改 |
| manifest 重载 | `Reload()` 仅被 `NewLoader` 调（`load.go:60`），无生产触发 | `reload()` 仅被 `Loader::new` 调 | **同构**，登记不改（§6 D1） |
| 标题 stale 语义 | `clearOSCEvidence` 置 stale、`setTitle` 清（`modes.go:264-292`）；规格明文要求「切换清证据」（tier `term-agent-state/spec.md:33-34/47-48`） | 逐行同构（`scan.rs:257-287`） | **同构**，审计描述有误（§1.2 订正 2） |

---

## 1. 复验结果表

### 1.1 逐条（AUDIT「Q-D 终端子系统」节 + 总表 P0-3）

| # | 派单条目 | 真伪 | 现行源码位置（HEAD bb2f357） | 结论 |
|---|---|---|---|---|
| 1 | **P0-3** RESIZE 尺寸无夹取 | ✅ **成立（实测加硬）** | 入径三处：`term/service.rs:1030-1048`（RESIZE 帧：`dec_resize` → `leg_resize` → `note_activity` → `apply_size_locked`）、`:854`/`:1334`（HELLO/建会话经 `cols_or_default/rows_or_default` `:2854-2860`）、`:2772-2792`（`apply_size_locked`，唯二守卫 = `cols==0||rows==0`）；`term/frames.rs:424-429`（`dec_resize` 只查 4B）；`term/vt.rs:291-296`（`new`）、`:425-441`（`resize` 只查 0）→ `self.term.resize(SimpleDims{cols,rows})`；alacritty 无上限（`term/mod.rs:655-705`；`MIN_COLUMNS`/`MIN_SCREEN_LINES` 仅定义 `:36/39`，**全库零引用**） | 见证据 A。**修（F1a/F1b/F1c）** |
| 2 | **P0/P1** 检测采样吞掉差分指纹 | ✅ **成立（最小复现转正为回归）** | `term/vt.rs:639-648`（`rows()` 无条件提交 `flushed`）、`:812-814`（`screen_text() = text_of_rows(&self.rows())`）、`:618-636`（`dirty_rows` 按 `flushed` 过滤）；采样侧 `term/service.rs:2602-2625`（`sample_loop`，`SAMPLE_PERIOD=1s` `:80`，`detect` 默认开 `:171`/`:199`）→ `:2645` → `:2824`（`screen_text()`）；冲刷侧 `:1918-1919`（合并窗 16-33 ms `:90-91`） | 见证据 B。**修（F2）** |
| 3 | **P1** `focus_nudge_locked` 锁内阻塞 PTY 写 | ✅ 成立 | `term/service.rs:1557-1565`（`rt.pty.write_input` ⇒ `PtyShared::write_input` `:271-273` ⇒ `pty.rs:279-282` `write_all + flush`，**阻塞 fd、无超时**）；调用点 `:1538`（`end_leg`，持 `:1505` 服务锁）与 `:2146-2152`（`run_raw_writer`，持 `:2148` 锁）；同文件 `:16-20` 自述「锁内不做阻塞 I/O」，例外清单**不含** PTY 写；Go 同形但持**每会话锁**（`service.go:760-771`） | **修（F3）**：判据锁内、注入走每会话单写者队列 |
| 4 | **P1** `stalled_ms` 生产路径从不刷新 | ✅ 成立 | 唯一写点 `term/session.rs:459-465`（`set_stalled`），全仓调用仅 `:718`（`#[cfg(test)]`）；字段 `:132`（init `:410`）；读者 `evict_victim` `:599-619`（`:604` 恒真 ⇒ 恒走「最久空闲」兜底）；Go `term_leg.go:935` 读实时停滞；`LegOut::stalled_for()` `legout.rs:223-228` 存在但**无生产调用** | **修（F4）**：接线实时停滞数据；surface 不置位（Go 同款） |
| 5 | **P1** 单格 symbol >127 字节 ⇒ 格流错位 | ✅ 成立 | 编码 `term/codec.rs:253-264`（`hdr = (sym.len() & 0x7f) as u8` 但全量写）；解码 `:368-392`（`n = b[0] & 0x7f`，只吃 n 字节）；常量 `:165-171`（`SYM_LEN_MASK=0x7f`）；源 `term/vt.rs:1014-1096`（`cell_of`，zerowidth 无上限 `:1044-1051`）；alacritty `push_zerowidth` 无上限（`cell.rs:164-167`） | **修（F5）**：编码前按 UTF-8 边界截断到 ≤127 B |
| 6 | **P1** `resp_dropped` 只写不读 / `clip_dropped` 不存在 | ✅ 成立 | `resp_dropped`：字段 `:470`、init `:1426`、捕获 `:1680/1709`、唯一写点 `:1746`，**全仓零读取点**；剪贴板 `:1759` `let _ = clip_tx.try_send(text)` | **修（F6）** |
| 7 | **P1🔎** `title_stale` 冻结窗口 + 早退不清 `last_scan_proc` | ⚠️ **部分误报**（见 1.2 订正 2） | `scan.rs:257-287`（`set_title` 早退条件含 `!title_stale` ⇒ 任何一次后续 OSC 0/1/2 都清 stale）；触发点 `service.rs:2663-2666`（Go `service.go:1005-1010` 同）；`screen_evidence_locked` `:2795-2841` 的**四道门**（`manifests?`/`vt?`、`proc_name` 空、`should_skip`、`for_process?`）与写点 `:2829` 与 Go `screenEvidenceLocked`（`service.go:1062-1102`）逐行同构 | **不改**（规格 + Go 同构）；「早退不清」子项**剔除** |
| 8 | **P2🔎** 线程无 `catch_unwind` + PTY/LegOut 锁 `expect` | ✅ 成立 | 生产线程：`:589`（term-sample）、`:630`/`:668`（term-conn）、`:957`（term-leg-writer）、`:1442`（term-pump）、`:1449`（一次性收尸）、`:1460`（term-resp）、`:1469`（term-surface）、`:1589`（一次性宽限）；全仓 term 面无 `catch_unwind`（同仓先例 `facade/tun_exec.rs:1243-1258`）；`PtyShared` **7 处** `.expect("pty")`（`service.rs:268/272/276/280/284/288/292`）；`LegOut` 13 处 `.expect("legout")`（`legout.rs:92/112/128/149/158/167/175/181/198/202/207/219/224`）；`state` 锁已有中毒恢复（`:2596-2598`） | **修（F7）** |
| 9 | **P2🔎** `vt.rs:624` `unreachable!` | ✅ 成立（当前安全性可论证） | `term/vt.rs:618-636`（`dirty_rows` 两次调 `damage()`；`damage()` 非纯读——`term/mod.rs:458-487`） | **修（F9）**：单次 `damage()` + 安全回落 |
| 10 | **P2🔎** `plain_text` 备用屏返回空 | ✅ 成立（移植缺陷） | `term/vt.rs:781-809`（`plain_text → rows_at(0,total)`）、`:725-735`（`rows_at` 备用屏早退）；消费点 `service.rs:2554-2560`（`explain_json`）；Go 对照见证据 C | **修（F8）** |
| 11 | **P2🔎** `is_descendant_of` 每调用建表 | ✅ 成立（**Go 同构、量级低**） | `term/agent.rs:340-360`；调用点 `:383`（`foreground_agent_name`）与 `:158`（`classify_agent`） | **不做（登记）**（§7） |
| 12 | **P2🔎** 解码器生产死码 | ✅ 成立 | `codec.rs:289-392`、`:793-798`、`dec_fragment/dec_*_body/FragAssembler`：生产路径零调用（仅测试） | 见 F10/F11；**不删**（线协议参考实现 + fuzz oracle） |
| 13 | **P2🔎** `gunzip_bytes` 无输出上限 | ✅ 成立 | `codec.rs:793-798`（`read_to_end` 无界） | **修（F10）** |
| 14 | **P2🔎** fuzz 缺 term 目标 | ✅ 成立 | `fuzz/fuzz_targets/` 9 目标无 term 面；`tests/fuzz_replay.rs` 同 | **做（F11）**：3 目标 × 2 轨 |
| 15 | **P2🔎** `manifest::Loader::reload` 无触发入口 | ✅ 成立（**Go 同构**） | `manifest/mod.rs:649-696`（仅 `new` 调）；Go `load.go:12-13/60-65`；CLI 每次 `loaderFor` 重建（`term_cli.go:829-835`） | **保留不删不接线**（§6 D1） |
| 16 | **P2🔎** `encode_frame` 静默截断 | ✅ 成立（**Go 同构**） | `term/frames.rs:253-259`（截 `MAX_PAYLOAD=65535`）与 `:301`（`name.len() as u8`，模 256）；Go `encodeTermFrame`（`frames.go:166-168`）、`encHelloFlags` `:227` | **不改**（§7 登记残余） |
| 17 | **P2🔎** `read_procs` 空白归一 | ✅ 成立（**Go 同构**） | `agent.rs:512-539`（`f[4..].join(" ")`）、`:494-508`（NUL→空格） | **不改**（§7） |

### 1.2 误报 / 订正记录

- **订正 1（P0-3 后果链补强）**：审计写「网格分配 GB 级」——实测 **96 GiB 起（两屏 192 GiB）**；
  补两点审计未写：① `apply_size_locked` 先 `pty.resize` 再 `vt.resize`；② alacritty 对**两屏**
  即时分配。
- **订正 2（`title_stale` 条目部分误报）**：审计写「stale 恒空 + **再次 clear 不解除**」——
  后半不成立：`set_title` 早退条件是 `title == self.title && !self.title_stale`（`scan.rs:260`）
  ⇒ **stale 置位后任何一次 OSC 0/1/2（哪怕同串）都会清 stale**（Go `modes.go:266-272` 同款，
  Go 测试 `modes_osc_test.go:144-149` 断言同语义）。「只在启动设一次标题的 TUI 切换后
  `osc_title` 证据失效」这一后果成立，但属 **Go 同款 + 规格明文要求**（tier
  `term-agent-state/spec.md:33-34`「前景 agent 切换时 SHALL 清空保留的 OSC 证据」+ Scenario
  `:47-48`）⇒ **不是移植缺陷，不修**（TTL 反而让旧标题重获判定资格 = 反规格）。**子项
  「`screen_evidence_locked` 早退不清 `last_scan_proc`」剔除**：四道门与写点与 Go 逐行同构
  （Go 也只在成功路径写 `lastScanProc`；`for_process` 的 `?` 两侧都不可达——`proc_name` 来自
  `foreground_agent_name(..., |n| manifests.for_process(n).is_some())`）。
- **订正 3（`stalled_ms` 修法订正）**：审计写「接线 `LegOut::stalled_for()` **+ surface 超时置位**」。
  前半采纳；**后半不采纳**：Go `noteStall` 只在 raw 4 站点，surface 写超时直接断腿
  （R6-design §9.7 在案）⇒ 给 surface 置位是偏离 Go 的新行为。F4 按 Go 语义接线。
- **订正 4（`focus_nudge` 的移植语境）**：Go 同样在持锁下写 PTY，但持的是**每会话锁**
  （`service.go:760-771`）；Rust 收敛成单把服务锁后阻塞半径 = 全服务 ⇒ 修复是**移植特定**的必要
  收紧，不是行为变更。
- **订正 5（PtyShared 计数）**：初稿写 6 处，实为 **7 处**（`service.rs:268/272/276/280/284/288/292`）。
- **订正 6（F10 常量现状）**：初稿写「`MIRROR_VIEWPORTS` 迁到 codec」——现状 `codec.rs:35` **已**有
  `pub const MIRROR_VIEWPORTS`，`service.rs:93` 是重复定义 ⇒ 改为「删 service 侧重复定义，改用
  `codec::MIRROR_VIEWPORTS`」。
- **误报剔除汇总**：条目 7 子项「早退不清 `last_scan_proc`」（剔除）、「再次 clear 不解除」
  （描述不成立）。其余 15 条机制全部复核成立。

---

## 2. 修复清单

> 顺序：**F1a/F1b/F1c → F2 → F3 → F4 → F5 → F6 → F7 → F8 → F9 → F10 → F11**。
> 共同约束：**wire 帧字节零变化**（例外 = F1a 极端尺寸夹取、F1c 宽屏镜像行数、F5 极端字素簇
> 截断——均在 §3 登记）；不加 `unsafe`；不加依赖。

### F1 尺寸入径归一（P0-3；含 0 值面与镜像预算）

**F1a 尺寸值对象 + 上限（正门）**

- **方案**：新增 `term/size.rs`（微型模块，先例 = Q-I 的 `envflag.rs`）：
  ```rust
  pub struct Size { cols: u16, rows: u16 }        // 访问器 cols()/rows()；构造器是唯一入口
  impl Size {
      pub const MAX_COLS: u16 = 1000;
      pub const MAX_ROWS: u16 = 500;
      pub const DEFAULT: Size  = Size { cols: 80, rows: 24 };   // Go spawnLocked 缺省
      /// HELLO/建会话：0 → 缺省；超限 → 夹取（幂等）。
      pub fn normalized(cols: u16, rows: u16) -> Size
      /// RESIZE 上报：0 → None（**忽略本次上报**，= Go applySizeLocked 的 0 门语义）；
      /// 其余 → normalized（幂等）。
      pub fn from_report(cols: u16, rows: u16) -> Option<Size>
  }
  ```
- **接入点**（「未归一尺寸」在类型上不可表达）：`LegDescriptor.size`、`LegState.size`、
  `Session.size`、`RegisterOutcome.size_applied`/`LegEndOutcome.size_applied: Option<Size>`、
  `SessRt::size()`、`apply_size_locked(rt, size: Size)`、`ring.note_size(size)`；
  HELLO 入径（`service.rs:854`/`:1334`）用 `normalized`，RESIZE 入径（`:1031-1039`）用
  `from_report`（None ⇒ 不改腿、不改会话、不 apply，可打一行忽略日志）。
- **组件层硬拒（末道门）**：`SessionVt::new/resize`（`vt.rs:291-296/425-441`）保持
  `(cols: u16, rows: u16)` 签名，内部 `Size::normalized(cols, rows) == (cols, rows)` 不成立即
  返回 `Err`（**不发起分配**）。*形态说明*：不把 vt 签名换成 `Size` 是为避免 31 处调用点
  （多为测试）的机械翻新，安全性由「会话层已归一 + 组件层硬拒」双保；若代码门要求全量传播可
  机械跟改。
- **上限论证（评审 1.2 订正版）**：
  ① 资源面（上限处）：两屏即时 `2 × 1000 × 500 × 24B ≈ 24 MiB`；回滚 `1000 × 10000 × 24B ≈
  229 MiB`（惰性填充，`HOMEWAY_TERM_SCROLLBACK_LINES` 可调）；镜像见 F1c（预算 32 MiB）。
  ② 客户端余量：手机 surface 网格 ≈ 100×40（≥10× 余量）；桌面 CLI attach 用**可读字号**
  （8-9 px 宽 / 16-17 px 行高）时 8K 全屏 ≈ 850-960 列 × 250-270 行——在限内；**只有不可读的
  极小字号（≤6 px）** 才越过 1000/500，这类形态**会被夹取且无 env 逃逸口**（§3 登记 +
  §7 残余）。初稿「1280×540 ⇒ 余量 ≥1.8×」自相矛盾，已删。
  ③ `max_sessions=16` 汇总口径：上限处**最坏驻留** ≈ 16 ×（24 MiB 两屏 + 229 MiB 回滚）≈ 4 GB
  （需 16 个会话全部灌满 10000 行回滚才可达；Go 同形，非本批新增面）——登记为残余（§7）。

**F1b 0 值入径归一（评审 1.1；Rust 独有移植分歧）**

- **根因**：`RESIZE 0×0` → `leg_resize` 写腿尺寸 0 → `note_activity_internal`（`session.rs:583-597`）
  **无条件**把腿尺寸写进会话尺寸 ⇒ 会话几何 = 0×0；`apply_size_locked` 的 0 门随后早退，
  **门形同虚设**。后果：LIST JSON / ATTACHED / surface 体 / FETCH-ROWS 应答几何全 0 而格流仍按
  vt 宽度编 ⇒ 客户端解出空网格/错位（持续到下一次合法 RESIZE，可反复触发）。Go 不会：
  会话尺寸唯一写点 `noteSizeLocked` 在 0 门之后。
- **修法**：`Size::from_report`（0 → None）在入径拦下；`note_activity_internal`/`elect_active`
  只从 `leg.size: Size` 读（类型保证非 0）——**会话几何只能由合法 `Size` 写入**。
- **测试**：集成 `RESIZE 0×0` ⇒ LIST/ATTACHED 几何保持旧值（修复前必红，评审已用 /tmp 程序实测
  现状返回 `Some((0,0))`）。

**F1c 快照镜像窗口字节预算（评审 1.2 的镜像面）**

- **现状**：快照镜像 = `vt.mirror_rows(rows * MIRROR_VIEWPORTS)`（`service.rs:1944`，10 个视口），
  材质先建 `Vec<Row>`（`abs_rows`）再 `encode_grid` + gzip；上限处最坏 `5000 行 × 1000 列 × 40B`
  ≈ 200 MiB/次，且反复 RESIZE 可反复触发。
- **修法**：`MIRROR_BYTES_BUDGET = 32 << 20`（32 MiB）+ `mirror_rows_budget(cols) =
  (MIRROR_BYTES_BUDGET / (cols × 48)).max(64)`，调用点取
  `above = min(rows * MIRROR_VIEWPORTS, mirror_rows_budget(cols))`。
  效果：窄屏（≤200 列）不变；1000 列时镜像 ≈ 699 行（客户端仍可经 FETCH-ROWS 拉更多——
  规格「滚出镜像窗口 ⇒ 按需拉取」Scenario 不变）。
- **登记**：数值语义变化（快照镜像行数在宽屏下变小；无判据行、无夹具）+ 已知口径注记。

- **涉及文件**：新增 `term/size.rs`；`term/session.rs`、`term/vt.rs`、`term/service.rs`、
  `term/codec.rs`（`MIRROR_VIEWPORTS` 去重）。
- **风险**：低-中。① `Size` 传播面（`LegDescriptor/LegState/Session/outcome/apply`）机械但需逐点
  核准（编译期兜底：字段类型变更会让漏改点编译失败）；② 上限取值见 F1a②（不可读字号会被夹，
  已登记）；③ F1c 使宽屏镜像行数变少（登记 + 客户端有 FETCH-ROWS 兜底）。
- **测试计划**：① `Size::normalized/from_report` 边界表（0/1/合法/等于上限/超限/`u16::MAX`）；
  ② `SessionVt::new/resize(65535,65535)` ⇒ `Err` 且 `size()` 不变；③ 集成：HELLO/RESIZE
  `65535×65535` ⇒ LIST/ATTACHED 为 `1000×500`、进程存活、会话可继续输入输出；
  ④ **`RESIZE 0×0` ⇒ 会话几何保持旧值**（F1b 回归，修复前必红）；⑤ 回归：80/24、120/40、0 缺省
  HELLO 不变；⑥ F1c：`mirror_rows_budget(1000) == 699` 层级单测 + 「宽屏快照材质行数 ≤ 预算」。
- **判据行影响**：**有**（极端输入）——E16a/E16b 尺寸字段、LIST JSON `cols/rows`、ATTACHED
  `cols/rows`（夹取）；快照镜像行数（数值语义）。§3 登记。

### F2 rows / commit 拆分（P0/P1）

- **方案**（三处小改，指纹语义收紧为单一不变量：**`flushed[y]` 只在第 y 行内容进入一次下发载荷时推进**）：
  1. `SessionVt::rows()` 去掉 `self.flushed[y] = row_fingerprint(&row);`；**保留** `let _ = self.term.damage();`
     （评审 3.1⑤：`damage()` 非纯读但有既有「隐含 Update」语义；保留 = 最小改动；注释改为
     「隐含 Update 保留；指纹不在此推进」）；
  2. 新增 `rows_and_commit()`（= 现 `rows()` 原体，含提交）；
  3. 提交点改为两处「确实成帧」：`dirty_rows()` 的 Full 分支（`:619-621`）与
     `flush_surface` 的快照材质（`service.rs:1943`）；Partial 分支保持「返回哪些行提交哪些行」。
     `screen_text()` 保持 `text_of_rows(&self.rows())`（自动变纯读）。
- **语义边界**：入队失败（背压/队列溢出）仍走既有乐观消费（`vt.clean()` `:1995` +
  `mark_need_snapshot` 全量兜底），不在本 F 范围。
- **风险（评审 3.1④ 订正）**：语义变化方向 = **以前被静默丢掉的行现在会补发**；快照路径提交全屏
  指纹后，后续增量不会重复带已随快照发过的行（幂等且更省）。`vt.rs:1621` 等测试注释过期
  （断言语义已核过仍成立）。
- **涉及文件**：`term/vt.rs`、`term/service.rs`（`:1943`）。
- **测试计划**：① 新增回归（证据 B 转正）：`screen_text()` 插在 `update()`/`dirty_rows()` 之间 ⇒
  仍返回 1 行且含新字节（修复前必红）；② 快照路径提交断言：全量拍 → 单行改动 ⇒ `dirty_rows().len()==1`；
  ③ 既有 `vt_ed2_forces_full_damage`/`vt_idle_dirty_rows_empty`/codec golden 全绿。
- **判据行影响**：无（§3 加一行口径注记）。

### F3 `focus_nudge` 出锁写（P1；评审 2.1 采纳「每会话单写者」版）

- **方案**：nudge 字节**入每会话 PTY 注入队列**（复用既有 `resp_tx` 与 `response_writer_loop`
  这一每会话唯一 PTY 写者，`service.rs:1774-1791`）：
  ```rust
  /// 判据（调用方持锁）：需要注入时返回字节。入队由调用方在**锁内** try_send
  /// （非阻塞），顺序 = 状态迁移顺序；写入由 response_writer_loop 在锁外做。
  fn focus_nudge_bytes(rt: &SessRt, focus_in: bool) -> Option<&'static [u8]>
  ```
  - 顺序论证：`end_leg` 的 focus-out 与首腿 attach 的 focus-in 都在**服务锁内**入队
    （`try_send` 不阻塞）⇒ 队列序 = 状态迁移序，**消除评审 2.1 的交错**；
  - 丢弃面：队列满（16）时 nudge 可能被丢（计数并入 F6 观测面 + 日志）；focus-in 被丢的降级
    = 依赖尺寸哨兵兜底（少数 TUI 需按键恢复）——写进注释与记录；
  - 失败日志行文不变：`term: 会话 {name} focus nudge 写入失败`（由队列消费者在写失败时打）。
- **涉及文件**：`term/service.rs`（`:1557-1565` 重写 + `:1538`、`:2146-2152` 两调用点 + resp 写者）。
- **风险**：低-中。① nudge 变异步（毫秒级，队列消费者常驻）；② 队列满丢弃（罕见：nudge 仅
  attach/detach 时产生）；③ `resp_tx` 计语文义扩展（应答 + PTY 注入），F6 观测面按新语义命名。
- **测试计划**：① 单测 `focus_nudge_bytes` 三态（`stopped` / `mode_bits::FOCUS` 未置 / 正常）；
  ② 集成：首腿 attach 后 PTY 收到 `\x1b[I`（用壳回显/harness 断言）；末腿摘除后收到 `\x1b[O`，
  且**并发 attach 场景下两字节顺序与状态迁移一致**（评审 2.1 的场景回归）；③ 代码评审确认
  `lock_state()` guard 作用域内无 `write_input`。
- **判据行影响**：无（行文不变）。

### F4 `stalled` 接线实时 `LegOut`（P1；评审 3.3 采纳「数据快照」版）

- **方案**（回归 Go 语义）：
  1. `LegOut` 增 `pub fn stalled_snapshot(&self) -> Option<Duration>`（单次持锁，消 `is_stalled().then(stalled_for)`
     的 TOCTOU）；
  2. `SessionRegistry::register_leg` 增**数据参数** `stalled: &[(LegKey, Duration)]`（只含停滞腿；
   service 侧在锁内从 `rt.legs` 收小表——`session.rs` 保持纯状态机，不做反向回调）；
  3. `evict_victim(s, stalled)`：先选「停滞表里时长最大者」，否则「最久空闲」
     （与 `term_leg.go:929-948` 同序）；
  4. **删除死面**：`LegState.stalled_ms` 与 `set_stalled`（停滞唯一真源 = `LegOut`）；
     `session.rs` 测试改为传数据表（`&[(l2, Duration::from_millis(5000))]`）。
- **锁序（实现注释写明）**：state 锁 → `LegOut` 锁（既有：`end_leg` 持 state 锁调 `finish_ended`
  `:1519-1524`）；`legout.rs` 无任何反向取 state 锁路径 ⇒ 无死锁环。
- **涉及文件**：`term/session.rs`、`term/legout.rs`、`term/service.rs`（`:871` 调用点）。
- **风险**：低-中（签名面 = 1 生产 + 8 测试调用；行为变化只在「腿满 + 有停滞腿」时生效且回归 Go）。
- **测试计划**：① 移植 Go `term_leg_test.go:427-445`：三腿（旧/新/停滞）+ 上限 ⇒ 淘汰停滞腿；
  ② 无停滞 ⇒ 最久空闲（既有断言保留）；③ 集成（`HOMEWAY_TERM_MAX_CLIENTS=2` + 小
  `HOMEWAY_TERM_WRITE_TIMEOUT_MS` + 不回读客户端）⇒ 新腿接入断停滞腿（日志 `原因=evicted_cap`）。
- **判据行影响**：无（行为回归 Go）。

### F5 单格 symbol 收口 ≤127 B（P1；评审 5.3 采纳常量真源）

- **方案**：
  1. `codec::marker` 改 `pub(crate)`；`SYM_LEN_MASK` 成为**代码级单一真源**；
  2. `vt.rs::cell_of` 构造 `symbol` 后按 `SYM_LEN_MASK as usize`（=127）**在 UTF-8 边界**截断
     （`while !symbol.is_char_boundary(n) { n -= 1 }`）；
  3. `codec::append_cell` 副门：快路径判 `sym.len() <= 127`；超限走边界安全截断 +
     `debug_assert!(false, "cell symbol 超上限")`（release 保正确性，debug/测试可闻预期）；
     **注意**：cargo-fuzz release 构建下 `debug_assertions` 关闭 ⇒ 不变量由 F11 的运行时 oracle 兜。
- **涉及文件**：`term/vt.rs`、`term/codec.rs`。
- **风险**：极低（只在 >127 B 字素簇生效；该形态现状即错乱帧；golden/向量无此形态）。
- **测试计划**：① 200×`U+0301` 簇 ⇒ `cell_of` 输出 ≤127 B 且边界合法；② **往返口径（评审 7.2）**：
  含该格的网格 `encode_grid → decode_grid → 再 encode` **字节相等**（decode 会派生 `width`/`wraps`，
  结构体相等不可用作断言）；③ `append_cell` 副门单测（手工 130 B symbol ⇒ 输出 ≤127 + 5 且后续
  颜色/属性字段对齐）。
- **判据行影响**：**有**（极端输入，wire 字节从错位 → 截断）。§3 登记。

### F6 `resp_dropped` / `clip_dropped` / nudge 丢弃观测面（P1）

- **方案**：`SessRt` 增 `clip_dropped`（与 `resp_dropped` 同形）；`pump_loop` 的 `clip_tx.try_send`
  按 Err 计数；F3 的 nudge 入队失败计数并入同一观测面；三计数**节流日志**（首 3 次 + 每 100 次，
  沿用 Q-C F6 形态）：
  `term: 会话 {name} 查询应答丢弃 {n} 条（队列满）` / `剪贴板写丢弃 {n} 条` / `PTY 注入丢弃 {n} 条`
- **涉及文件**：`term/service.rs`。
- **风险**：极低（纯新增计数与日志；投递行为不变）。
- **测试计划**：小队列灌满 ⇒ 计数增长 + 日志行（`svc_with` 的 lines 收集）；既有 FIX-25 测试全绿。
- **判据行影响**：无既有判据行；**新增行**落 §3 指定登记点。

### F7 term 线程 `catch_unwind` + 毒锁恢复统一（P2）

- **方案**：
  1. 统一 helper（参考 `facade/tun_exec.rs:1243-1258` 先例）：
     `catch_thread_panic(role: ThreadRole, logf, body) -> bool`（`AssertUnwindSafe` +
     载荷 downcast `&str/String` 落 `logf`）。**线程角色用 `enum ThreadRole`**（评审 5.2，
     处置表按 enum 匹配）；
  2. 逐线程处置表（panic 处理器**自身不得 panic**——写进实现约束）：

     | 线程 | panic 后动作 | 理由 |
     |---|---|---|
     | `pump`（`:1442`） | 走既有收尾 `wait_bounded(2s)` + `finalize_exit`（**主动收尾**；子进程可能未真退出，`wait_bounded` 超时内含 `kill_force` 兜底） | 无 reader 的会话必死；**不留僵尸**，客户端可重建 |
     | leg writer（`:957`） | `leg_write_failed(name, key, "panicked")`（裸断腿） | 单腿失败不拖垮会话 |
     | `surface`（`:1469`） | 全 surface 腿 `finish_quit` + `end_leg(..., i32::MIN, "", "panicked")`，线程退出 | surface 唯一投递面；裸断让 app 见断连可重连 |
     | `resp`（`:1460`） | 日志 + 线程退出（查询应答降级） | 会话主体不受影响 |
     | `sample`（`:589`） | **按会话** catch（`sample_once` 外包）：日志 + 跳过该会话本拍继续 | 检测是服务级线程，不能全线停摆 |
     | `conn`（`:630`/`:668`） | 日志 + drop conn | 单连接隔离 |
     | 一次性线程（`:1449`/`:1589`） | 不套 | 最小面 |
  3. **毒锁恢复统一**：`PtyShared` **7 处** `.expect("pty")` 与 `LegOut` 13 处 `.expect("legout")`
     （含 `Condvar::wait_timeout` `:202`）改 `unwrap_or_else(|e| e.into_inner())`；注释口径
     （评审 2.3）：**「可接受的不一致面 + 处置保守」，不写「无跨调用不变量」**——例：`enqueue`
     的 `qbytes += len` 与 `push_back` 之间 panic 会让 `qbytes` 永久漂移（现实无 panic 点，
     分配失败是 abort），后果 = 该腿持续「超限入队失败 → needSnapshot」，处置路径保守（断腿/全量）。
- **涉及文件**：`term/service.rs`、`term/legout.rs`。
- **风险**：中。① `catch_unwind` 前提已核（无 `panic="abort"`：根 manifest/`.cargo/config.toml`/
  capi crate/tier `build-core.sh` 全无；capi 自带 FFI 边界 catch_unwind 先例）；② 毒锁恢复后
  半更新状态 ⇒ 处置选保守面；③ 采样按会话 catch 的日志可加「每会话首 3 次」节流。
- **测试计划**：① 毒锁恢复单测（A 线程持锁 panic ⇒ B 线程仍能取锁）；② **panic 注入**
  （`#[cfg(test)]` 静态表按 `(ThreadRole, 会话名)` 键——会话名用既有 `tmp_name` 唯一化；
  单次消费；等待用轮询行为而非固定 sleep；sample 注入类测试串行/独立实例，评审判定 flake 面已采纳）：
  pump panic ⇒ 进程存活 + 会话收尾（ENDED 达）+ 日志含「线程 panic（已兜住）」；leg writer panic
  ⇒ 腿断会话活；sample panic ⇒ 其它会话不受影响；③ 策略函数（panic → 动作）纯函数单测。
- **判据行影响**：无既有判据行；新增日志行（§3 落点）。

### F8 `plain_text` 备用屏 + 回滚折行（P2）

- **方案**：① `plain_text` 备用屏 ⇒ 视口行（`self.rows()` 纯读），主屏 ⇒ `rows_at(0, total)`；
  共用同一「折行合并 + 行尾裁」；② 顺带修 `abs_rows`（`:750-774`）对回滚行恒填 `wraps: false`
  ⇒ 按行读 WRAPLINE 位（与 `read_row`/`row_wraps` 同判据；**wire 不编 `wraps`**，`codec.rs:184-204`，
  ⇒ 无字节影响）；③ `rows_at`/`mirror_rows` 的备用屏语义**不动**（FETCH-ROWS 契约不变）。
- **涉及文件**：`term/vt.rs`。
- **风险**：低（EXPLAIN 输出从空/缺口 → 实际内容，对齐 Go；主屏口径回归由既有测试兜）。
- **测试计划**：① `\x1b[?1049h` + 写字 ⇒ `plain_text` 非空含锚点；退出备用屏恢复主屏口径；
  ② 长行滚入回滚 ⇒ `plain_text` 输出为单条逻辑行（与视口内同文本合并一致）；③ golden digest 全绿。
- **判据行影响**：无。

### F9 `dirty_rows` 的 `unreachable!` 降级（P2；评审 2.5 采纳单次 `damage()`）

- **方案**：`vt.rs:618-636` 改成**单次** `damage()` 取值后 `match`：
  ```rust
  if self.force_full { return self.rows_and_commit(); }   // 短路优先（别再调 damage()）
  match self.term.damage() {
      TermDamage::Full => self.rows_and_commit(),
      TermDamage::Partial(iter) => { /* 收集 damaged 行集 → 指纹过滤 */ }
  }
  ```
  ⇒ 去掉 `unreachable!`、去掉两次 `damage()` 调用（`damage()` 非纯读：替换 `last_cursor` +
  `damage_cursor()`）；Full 分支统一 = 全视口行 + 提交。
- **涉及文件**：`term/vt.rs`。
- **风险**：极低（语义等义 + 少一次副作用调用）。
- **测试计划**：既有测试全绿；补一条 `force_full` 断言（`dirty_rows()` = 全视口行）。
- **判据行影响**：无。

### F10 `gunzip_bytes` 输出上限 + 解码维度守卫（P2；F11 前置）

- **方案**：① `gunzip_bytes` 用 `Read::take(MAX_GUNZIP_OUT + 1)`，超限
  `Err(CodecError::BadGrid("解压超上限"))`；`MAX_GUNZIP_OUT = 64 << 20`，**注释写明与发送侧
  `HOMEWAY_TERM_PENDING_CAP_BYTES` 解耦**（运维可把 pending_cap 调得更小/更大，解码上限独立）。
  ② `decode_grid`/`decode_rows` 入口维度守卫：`cols ≤ Size::MAX_COLS` 且
  `rows/count ≤ Size::MAX_ROWS × (1 + MIRROR_VIEWPORTS)`；③ **删 `service.rs:93` 重复的
  `MIRROR_VIEWPORTS`**，统一用 `codec::MIRROR_VIEWPORTS`（现状两处定义，评审 1.6）。
- **为什么必须**：`BLANK_RUN`/`REPEAT` 的 `min(n, cols-len)` 不挡「5 字节头 + 极短体 ⇒ 4.29e9 格」
  放大（评审 6 已复核）；F11 的 codec 目标在无守卫时秒级撞 OOM。
- **涉及文件**：`term/codec.rs`、`term/service.rs`。
- **风险**：低（守卫 = 编码侧上限 × 镜像倍数，合法帧全过；gunzip 仅测试/参考路径）。
- **测试计划**：① 伪造头 `cols=0xffff` ⇒ **形状断言** `Err`（评审 7.2：不只「不 OOM」）；行数超限同款；
  ② gzip 炸弹（1 MiB → >64 MiB）⇒ `Err`；③ golden 正例（含最大镜像形态）不变。
- **判据行影响**：无。

### F11 term fuzz 目标（P2；§6 D2 裁定「做」）

- **方案**（两轨；本机 `cargo-fuzz 0.13.2` + `nightly 1.101` 实测在，API 全 pub，种子夹具齐全
  ——评审 6 复核）：
  1. **cargo-fuzz 轨**（`fuzz/fuzz_targets/` 3 个 + `fuzz/Cargo.toml` 3 个 `[[bin]]`）：
     `fuzz_term_frames`（`terms::frames` 解码族 + `read_frame`）、`fuzz_term_vt`
     （`SessionVt::new(20,5,100)` + `write_collecting` + 读取面 + `keyenc::encode_*`）、
     `fuzz_term_codec`（`decode_*`/`dec_*_body`/`gunzip_bytes` + `encode_grid → decode_grid →
     再 encode` **字节相等**往返断言——评审 6.5）；
  2. **回归轨**（`tests/fuzz_replay.rs` 同 3 目标，`#[ignore]` 100k，xorshift 种子 + 骨架变异）；
  3. **种子**：`tools/gen-fuzz-seeds.sh` 增 term 区（`fixtures/term/frames.v1.jsonl` payloadHex、
     `fixtures/term-vt/*.bin`、`fixtures/vectors/term_*.json`、`fixtures/surface-golden/*.bin`）；
     **对账链（评审 6.1 订正）**：脚本只把汇总摘要 **print 到 stdout**，由 `tools/ci-local.sh`
     第 4.5 步比对 `fuzz/corpus.seeds.sha256`（**公开 GitHub CI 不查**）⇒ 流程 = 跑脚本 →
     重定向写基准文件 → 同批提交；`fuzz/README` 不存在 ⇒ 目标注释里写明口径；
  4. **harness 上限（评审 6.2 订正，按目标分别从输入派生）**：三目标各自把 `(cols, rows/count)`
     从**对应布局**读出（`decode_grid`：头 1+2+2；`decode_rows`：函数参数由输入前 4B 派生；
     `dec_diff_body`：第 5-8 字节）并限 `cols×rows ≤ 2e5` 才调用（注释写明「harness 预检 ≠ 库守卫；
     库守卫由 F10 + 单测钉住」）；`-max_len` 取 8192 并在注释说明与 files 目标 262160 的差异
     （其 70KB 跨 u16 样本需要）；⑤ **同批改文案**：`tests/fuzz_replay.rs:513` 与
     `tools/ci-local.sh:13/32` 的「9 目标」→ 12；给出时间预算（3 新目标 ×100k，codec 目标带分配
     ⇒ 全量档预计 +2-4 min，写入 `ci-local.sh` 注释）。
- **oracle**：① 不 panic（两轨共享）；② `fuzz_term_vt` 断言 `dirty_rows()` 每格 `symbol.len() ≤ 127`
  （F5 运行时哨兵）；③ codec 往返字节相等。
- **涉及文件**：`fuzz/Cargo.toml`、`fuzz/fuzz_targets/fuzz_term_{frames,vt,codec}.rs`、
  `crates/homeway-core/tests/fuzz_replay.rs`、`tools/gen-fuzz-seeds.sh`、`tools/ci-local.sh`、
  `fuzz/corpus.seeds.sha256`。
- **风险**：低（不触产品路径）；成本 = 种子/摘要/文案同步（机械）。
- **测试计划**：① 回归轨 12 目标 ×100k 全绿（`cargo test --workspace --ignored -- --test-threads=1`）；
  ② 深挖轨 `cargo fuzz run fuzz_term_vt -- -max_total_time=60` 留档；③ 摘要对账一致。
- **判据行影响**：无。

---

## 3. 判据行与观测面影响汇总

| 项 | 判据行行文 | 计数输入集/数值语义 | 登记动作 |
|---|---|---|---|
| F1a/F1b | 无改动（E16a/E16b 行文/字段位不变） | **有**：>1000×500 尺寸由「原样接受（随后进程崩）」→「夹取到上限」；`RESIZE 0×0` 由「会话几何被写成 0×0（客户端错位）」→「忽略本次上报、几何保持」 | **判据变更登记表**（从→到 + 影响面点名验收方） |
| F1c | 无（快照镜像行数不在判据行；无夹具） | **有**：快照镜像窗口 = `rows×10` → `min(rows×10, 32MiB/cols 预算)`（宽屏变小，客户端可用 FETCH-ROWS 补） | **计数输入集/数值语义变化表** + 「已知口径注记」 |
| F2 | 无 | 无（竞态窗口内下发行集合变化） | 「已知口径注记」一行（可选，评审 7.3） |
| F3/F4 | 无（`focus nudge 写入失败`/`原因=evicted_cap` 行文不变） | 无（F4 行为回归 Go） | — |
| F5 | 无 | **有**：>127 B 字素簇 wire 字节由「错位帧」→「UTF-8 边界截断」（无夹具/向量覆盖该形态） | **判据变更登记表**（注明无夹具变更） |
| F6/F7 | 无 | 新增日志行 3 条（应答/剪贴板/PTY 注入丢弃）+ 1 条（线程 panic 兜底） | **计数输入集/数值语义变化表 additive 行**（Q-B「新增独立丢弃计数」先例）+ `QD.md` 批记录 |
| F8 | 无 | `EXPLAIN` 文本面（备用屏从空 → 实际内容） | 「已知口径注记」+ `QD.md`（R6 残余列表修正，评审 4-R6） |
| F9/F10/F11 | 无 | 无 | — |

**登记草案**（实现期逐字落地；「影响面」按政策点名验收方/文档/测试）：

```text
【判据变更记录 · 登记表】
| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
| 2026-10-XX | E16a/E16b 尺寸字段（+LIST JSON cols/rows、ATTACHED cols/rows） | 任意 u16 尺寸原样接受（>上限 ⇒ 96-192GiB 分配 abort） | >1000×500 夹取到上限；`RESIZE 0×0` 忽略上报、会话几何保持 | Q-D F1：P0-3 巨型分配 abort（实测）；与 Go「0 不改会话尺寸」对齐（F1b） | `docs/INTEROP-CRITERIA.md` E16a/E16b 行、`service.rs` LIST/ATTACHED 路径、单测 `oversize_resize_is_clamped`/`zero_resize_keeps_geometry`；正常尺寸逐字节不变 |
| 2026-10-XX | surface cell 流 symLen 域 | >127B 字素簇编码为错位字节流 | 按 UTF-8 边界截断到 ≤127B（真源 = `codec::marker::SYM_LEN_MASK`） | Q-D F5：7 位长度域约定 | `codec.rs`/`vt.rs`、单测 `symbol_cluster_truncated_at_char_boundary`；fixtures/vectors 无该形态 ⇒ 无夹具变更 |

【计数输入集 / 数值语义变化（行文不变）】
| 2026-10-XX | 快照镜像窗口行数 | 恒 `rows×10` → `min(rows×10, 32MiB/cols 保守预算)`（宽屏变小） | F1c：快照材质内存上界（上限处 ≈200MiB/次）+ 反复 RESIZE 放大 | 快照镜像面、`service.rs` 快照材质、单测 `mirror_rows_budget_wide_screen`；客户滚动窗口变小（FETCH-ROWS 兜底） |
| 2026-10-XX | 新增观测行（additive） | 无 → 有：`term: 会话 {n} 查询应答丢弃 {n} 条`/`剪贴板写丢弃`/`PTY 注入丢弃`（节流：首 3 + 每 100）/`{role} 线程 panic（已兜住）` | F6/F7 观测面 | 非 E 族判据行；`docs/reviews/QD.md`、`service.rs` 单测 |
```

---

## 4. 测试与验收计划

### 4.1 门（沿用批协议）

> 基线实测（设计阶段，2026-10-08）：`cargo test -p homeway-core --lib term::` = **111 passed / 0 failed**
> （10.7 s）——本批起点绿，新增测试以此为回归基线。

1. `cargo test --workspace`（本批新增单测/集成；已知 flake `wgcore::stackb` 按既有口径复跑）；
2. `cargo clippy --all-targets -- -D warnings`（零新告警）；
3. `tools/ci-local.sh --full`（含 `fuzz_replay` **12** 目标 ×100k 回归轨 + 本地门；第 4.5 步种子摘要对账）；
4. 判据登记：F1a/F1b/F1c/F5/F6/F7 的条目按 §3 草案写入 `docs/INTEROP-CRITERIA.md`
   （判据变更登记表 / 数值语义表 / 已知口径注记），**同批 commit**；
5. `QD.md` 批记录含：两件实测复现输出（证据 A/B）、R6 文档三处口径销账（§7）。

### 4.2 逐条判绿 / 证伪

| F | 主判据 | 证伪（出现即回退/重定位） |
|---|---|---|
| F1a | `Size` 边界表单测 + `SessionVt` 超限 Err + 集成（LIST/ATTACHED 上限值、进程存活） | 夹取后仍 >上限 进入 `vt.resize`；正常尺寸被改 |
| F1b | `RESIZE 0×0` ⇒ 几何保持旧值（修复前必红） | 会话几何仍被写成 0×0 |
| F1c | 镜像行数 ≤ 预算 + 宽屏单测 | 窄屏镜像行数变化（不该变） |
| F2 | A/B 回归测试 + 快照路径提交断言 | `screen_text` 仍能改变 `dirty_rows` 结果 |
| F3 | 顺序回归（并发 attach/末腿摘除）+ `focus_nudge_bytes` 单测 | 出锁后 nudge 与状态迁移顺序错乱仍可复现 |
| F4 | 停滞腿优先单测 + 集成（evicted_cap 打停滞腿） | 停滞表恒空（键不匹配） |
| F5 | 超长簇单测 + 往返字节相等 | 往返仍错位 |
| F6 | 计数增长单测 + 节流日志 | 计数不增长 |
| F7 | 毒锁恢复单测 + pump panic 注入（进程存活/收尾/日志） | 仍留僵尸会话 |
| F8 | 备用屏 `plain_text` 非空 + 回滚折行合并 | 主屏口径被改（golden 红） |
| F9 | 既有测试（单次 `damage()` 形态） | — |
| F10 | 伪造头 `Err` + 炸弹 `Err` + golden 正例 | 合法最大镜像帧被拒 |
| F11 | 12 目标 ×100k 绿 + 摘要对账 | 目标撞 OOM/超时 |

---

## 5. 设计门记录（dsh 外部评审）

> **轮次目录**：`/tmp/dsh-review/r7.loQFwv/`（`prompt.txt` / `output.md` / `stderr.log`）。
> **exit code = 0**（dsh 成功；评审者未改动仓库任何文件，自跑实测在 `/tmp/qd-zero`、复跑用
> 既有 `/tmp/qd-repro`）。评审方法 = 回源码 spot-check ~40 处行号 + **复跑本设计 §0.2 的两件
> 实测（输出逐字一致）** + Go/ghostty/fuzz 工具链各一路独立核查 + 一处 /tmp 实测反证。

### 5.1 结论

**结论摘要（原文）**：「未发现高危；**2 项中危建议过门前修订**（F1 的 0 值入径、F1 上限论证与
资源上界），另 3 项中等（F3 出锁写引入的 nudge 时序竞态、F11 harness 上限与 F10 现状对不上、
判据登记表口径）。设计整体质量高：证据链可复现、行号可信、误报剔除结论正确、F2/F4/F7 机制核过
成立。」**过门建议**：必须修订 1.1 / 1.2 / 6.2 / 7.1；建议同批处理 2.1 / 1.6 / 2.2 / 4.1-4.3 /
6.1 / 6.3-6.5；R6 文档三处口径冲突本批销账。

**逐条处置（按 §5.3 的行计：30 条 = 0 高 / 5 中 / 21 低 / 4 记录类）：认同 25 / 部分认同 1
（1.2：记账与登记全采纳、镜像预算按建议落地为 F1c；仅拒绝「全部靠上限间接约束」）/ 不认同 0 /
记录类 4（1.7 层选结论、两条误报表态、第 8 条「看过没问题」清单）。** 全部修订已并入 v2
（下表逐条落点）。

### 5.2 评审原文摘要（逐条）

| # | 评审意见（摘要） | 严重度 |
|---|---|---|
| 1.1 | **双门只封上界，0 值入径未归一**：`RESIZE 0×0` ⇒ `leg_resize`→`note_activity_internal` 先写会话尺寸再被 apply 的 0 门放空 ⇒ 注册表几何 `0×0`、surface/LIST/ATTACHED 全 0 而格流仍按 vt 宽编；Go 的会话尺寸写点在 0 门之后（`term_leg.go:909-916`+`service.go:733-739`）⇒ Rust 独有移植偏差；已 /tmp 实测「note_activity 返回 Some((0,0))，会话尺寸 = 0x0」 | 中（偏高） |
| 1.2 | **上限论证自相矛盾 + 资源上界漏算**：设计自给的「8K 最极端 1280×540」> 所选上限 1000×500；资源只算两屏即时 24 MiB，漏快照镜像 `rows×10` 行 ≈ 120-200 MB/次、漏回滚 229 MiB/会话 × 16 会话汇总；建议三项口径 + 镜像独立字节预算 + 重写余量论据 | 中 |
| 1.3 | 「夹取而非拒绝 ⇒ 客户端自描述可恢复」对 **raw CLI 不成立**（`term_cli.rs:1509/1681-1684` 只从 ATTACHED 取 agent/state，不回读几何；SIGWINCH 继续报本机尺寸）⇒ 建议登记残余 | 低 |
| 1.4 | 夹取日志的「天然频率约束」不成立：日志若在门 1（`leg_resize` 前），同一超限值重复上报每次都打；须按 `size_applied.is_some()` 条件化 | 低 |
| 1.5 | 建议 **`Size` newtype**（`from_wire` 归一 + `try_from_wire` 硬拒）替代「每层记得调」，1.1 自动消失，且对齐仓规第 1 条 | 低（建议） |
| 1.6 | F10「`MIRROR_VIEWPORTS` 迁到 codec」与现状不符：`codec.rs:35` **已**有 `pub const`，`service.rs:93` 是重复定义 ⇒ 改「删重复定义」 | 低 |
| 1.7 | 回答「双门必要与充分」：必要；层选正确（**不该放进 `frames::dec_resize`**）；>上限面核过全部入径无绕过（HELLO/RESIZE/create/重选举/CLI attach/FETCH-ROWS/surface 几何）；**0 值面不充分**（见 1.1）；`spawn_session_locked` 直连 `SessionVt::new` 的硬拒 Err 会让会话静默退化 legacy-only（兜底非正常路径，注明） | — |
| 2.1 | **F3 出锁写引入时序竞态**：末腿摘除的 focus-out 与并发 attach 的 focus-in 可乱序（TUI 停在失焦）；Go 每会话锁下天然串行 ⇒ 建议每会话 PTY 注入单写者（复用 `response_writer_loop` 队列或 per-session 小 Mutex），或显式登记为「新行为」并加测试 | 中（偏低） |
| 2.2 | `PtyShared` 的 `expect("pty")` 是 **7 处**不是 6 处（`service.rs:268/272/276/280/284/288/292`；LegOut 13 处核对无误） | 低 |
| 2.3 | 毒锁恢复注释「锁内无跨调用不变量」对 `LegOut.qbytes` 不严格（`enqueue` 的 `+=`/`push_back` 之间 panic 会永久漂移；现实无 panic 点）⇒ 注释改「可接受的不一致面 + 处置保守」 | 低 |
| 2.4 | `#[cfg(test)]` 注入表的 flake 面：按 `(role, 会话名)` 键、单次消费、sample 注入串行/独立实例、轮询等待而非固定 sleep | 低 |
| 2.5 | 明确回答：分配失败=abort/唯一防线=不发起分配（正确，已核 `term/mod.rs:415-416/678`）；`catch_unwind` 有效性核过（全链无 `panic=abort`，capi 有先例）；逐线程处置表成立（提醒：处理器内**不得再 panic**；pump「视同 EOF」实为主动收尾）；**F9 的伪代码与「避免两次 `damage()`」不等价**（缓存 bool 仍要第二次调用；要真正一次需 `match` 携带行集；`force_full` 短路须在前） | — |
| 3.1 | F2 不变量**闭合**（逐调用点核过）；两处行文要改：①「唯一语义变化=多带一次已发过的行」**方向说反**（实际 = 以前被静默丢掉的行现在补发；快照提交全屏指纹后增量不重复带）；②「`rows()` 纯读」没写死是否保留 `let _ = self.term.damage();`（保留=推荐，须改注释；删除须核依赖） | 低×2 |
| 3.2 | F3 除 2.1 外「会话被并发收尾后写已关 PTY」面设计已覆盖，判断正确 | — |
| 3.3 | F4 成立（同序核过、只 raw 照准 Go、锁序无环）；两处可选改进：① `is_stalled().then(stalled_for)` 两次取锁 TOCTOU ⇒ 加单次持锁 `stalled_snapshot()`；② 闭包入参不如**数据快照**（`Vec<(LegKey, Option<Duration>)>`）——纯状态机保持 | 低×2 |
| 4.1 | `screen_evidence_locked` 不是「两处早退」而是**四道门**（Go 同样四道、都在成功路径写 `lastScanProc`）⇒ 结论（剔除）成立，改计数 | 低 |
| 4.2 | surface 写循环在 `term_leg.go:272-302`（`runSurfaceWriter`），**不在** `term_surface_leg.go` | 低 |
| 4.3 | 截断在 `encodeTermFrame`（`frames.go:166-168`），Go 无 `encodeFrame` 符号；`p[5]=byte(len(name))` 是模 256 长度域（>255 B 名字 ⇒ nameLen 回绕 + 余下字节被当尾随块 ⇒ `bad_capability`） | 低 |
| 4-R6 | 与 R6 文档的**登记冲突**要在本批销账：R6-design §9.7 的 `plain_text` 残余未含「备用屏恒空」；R6-design:295-296 的「停滞最久优先」登记与实现（死分支）不符；R6.md:55 锁纪律例外清单 F3 后应同步 | 低 |
| 4-误报 | **两条误报剔除表态**：`title_stale`「再次 clear 不解除」订正正确（逐行同构 + 规格明文，同意不改）；`last_scan_proc` 剔除正确。看过没发现问题 | — |
| 5 | **Go 直译痕迹：看过没发现问题**（无多余 Arc/Mutex、无字符串错误、无无谓拷贝、模块边界 Rust 习惯）；4 条形态建议：`Size` newtype / `ThreadRole` enum / F5 常量代码级引用 `SYM_LEN_MASK`（注释不是真源）/ F4 数据快照 | 低×4 |
| 6.1 | 种子对账链落点说错：脚本**只 print**（`:195`），消费方 = `ci-local.sh` 第 4.5 步（公开 CI 不查）；`fuzz/README` 不存在 | 低 |
| 6.2 | harness「先窥 5 字节头」只适配 `decode_grid`；`decode_rows` 的 cols 是参数、`dec_diff_body` 的 cols/rows 在第 5-8 字节 ⇒ 须按目标分别派生（否则 F10 守卫允许的 5.5M 格 ≈132MB/迭代拖垮 fuzz） | 中 |
| 6.3 | `-max_len=8192` 与仓内既有 262160 口径不一致 ⇒ 统一或说明 | 低 |
| 6.4 | 新增 3 目标 ×100k 拉长 `ci-local --full`（未给预算）；`fuzz_replay.rs:513`/`ci-local.sh:13/32` 的「9 目标」文案须同批改 12 | 低 |
| 6.5 | oracle ③ 应断言 **decode→re-encode 字节相等**（decode 派生 `width`/`wraps`，结构体相等会假红） | 低 |
| 7.1 | 登记草案字段口径不符：政策要求「影响面 = 哪些验收方/文档/测试引用该行需同步」；新增观测行只说「观测面登记」**没写落点** | 中 |
| 7.2 | F5 的「逐格一致」要注意 decode 派生 `width`（按 wire 字段/再编码字节比）；F10 的「不 OOM 即证」是弱断言 ⇒ 加「返回 Err」形状断言 | 低×2 |
| 7.3 | F2 值得在「已知口径注记」加一行（可选） | 低 |
| 8 | 复核过没问题：行号抽查 ~40 处全中；两件实测逐字一致；F2 闭合性；F4 锁序与 Go 语义；F7 catch_unwind 前提；第 5 条 Go 直译；两条误报剔除；F6 方案；F10 守卫必要性与守卫值（唯一小注：解码 64MiB 上限与 `HOMEWAY_TERM_PENDING_CAP_BYTES` 解耦要写明）；不改项裁定全部同意 | — |

### 5.3 逐条处置表

| # | 处置 | 落到 v2 的位置/证据 |
|---|---|---|
| 1.1 | **认同（中偏高）**——采纳 1.5 的 `Size` newtype 作为正解：新增 `term/size.rs`（`normalized`/`from_report`），会话几何只能由 `Size` 写入；RESIZE 0 ⇒ 忽略上报（= Go 0 门语义）；补集成回归「`RESIZE 0×0` 保持旧几何」（修复前必红） | §2 **F1b**、§2 F1a |
| 1.2 | **部分认同**——三项资源口径（两屏/镜像/回滚）+ 多会话汇总 + 余量论据重写（可读字号 8K ≈850-960 列在限内；不可读极小字号会被夹并登记）全采纳；镜像预算按建议**落地为 F1c**（32 MiB 预算 + 按 cols 反推行数 + 数值语义登记）；仅「宽屏镜像行数」落地为登记项而非静默变更 | §2 F1a②/F1c、§3 |
| 1.3 | **认同**——raw CLI 不回读几何 ⇒ 「夹取可自描述恢复」限定为 surface 客户端；raw CLI 不知情登记残余 | §2 F1a、§7 |
| 1.4 | **认同**——日志条件改为「夹取发生且 `size_applied.is_some()`」，并注明刷屏上界 | §2 F1a |
| 1.5 | **认同**——`Size` newtype（范围见 §2 F1a；vt 侧保留 u16 签名 + 硬拒，理由 = 31 处测试调用点，安全性不降） | §2 F1a |
| 1.6 | **认同**——改为「删 `service.rs:93` 重复定义，改用 `codec::MIRROR_VIEWPORTS`」 | §2 F10③、§1.2 订正 6 |
| 1.7 | **记录**——层选结论（策略不进帧层）、入径清单、`spawn_session_locked` 硬拒 Err 的 legacy-only 退化注明（实现注释） | §2 F1a |
| 2.1 | **认同**——采纳「每会话单写者」：nudge 字节入既有 `resp_tx` 队列（锁内 `try_send`，顺序 = 状态迁移序；消费者 `response_writer_loop` 锁外写）；满则计数/日志；focus-in 被丢降级 = 哨兵兜底（写明）；加并发 attach 顺序回归 | §2 **F3** |
| 2.2 | **认同**——改 7 处 | §1.1 行 8、§2 F7③ |
| 2.3 | **认同**——注释口径改「可接受的不一致面 + 处置保守」，举例 `qbytes` | §2 F7③ |
| 2.4 | **认同**——注入表按 `(role, 会话名)` 键、单次消费、轮询等待、sample 注入串行/独立实例 | §2 F7 测试 |
| 2.5 | **认同**——F9 改**单次 `damage()`** 的 `match` 形态（含 `force_full` 短路在前的顺序提醒）；pump「主动收尾」措辞；处理器不得再 panic 写成实现约束 | §2 F9、F7 |
| 3.1 | **认同（两处）**——① 风险段改为「以前被静默丢掉的行现在会补发；快照提交全屏指纹后增量不重复带」；② 明确**保留** `let _ = self.term.damage();` + 改注释 | §2 F2 |
| 3.3 | **认同（两处）**——`LegOut::stalled_snapshot()`（单次持锁）+ `register_leg(..., stalled: &[(LegKey, Duration)])` 数据快照（`session.rs` 保持纯状态机） | §2 **F4** |
| 4.1 | **认同**——改「四道门」 | §1.2 订正 2、§1.1 行 7 |
| 4.2 | **认同**——文件指引改 `term_leg.go:272-302`（`runSurfaceWriter`） | §0.2 证据 C |
| 4.3 | **认同**——符号改 `encodeTermFrame` + 模 256 长度域语义写明 | §0.2 证据 C、§1.1 行 16 |
| 4-R6 | **认同**——`QD.md` 批记录销账三处（R6-design §9.7 plain_text 残余、R6-design:295-296 停滞登记、R6.md:55 锁纪律口径） | §4.1⑤、§7 |
| 4-误报 | **记录**（评审明确同意两条剔除） | §5.3 本行、§1.2 |
| 5 | **认同（四条形态建议）**——`Size` newtype（已采纳）/ `ThreadRole` enum / `SYM_LEN_MASK` 改 `pub(crate)` 代码级引用 / F4 数据快照 | §2 F1a、F7①、F5①、F4② |
| 6.1 | **认同**——对账链改正（脚本 print → ci-local 第 4.5 步比对 → 写基准是机械步骤）；`fuzz/README` 不存在 ⇒ 写进目标注释 | §2 F11③ |
| 6.2 | **认同（中）**——harness 上限按目标分别从输入派生 + 「预检 ≠ 库守卫」注释 | §2 F11④ |
| 6.3 | **认同**——`-max_len` 取 8192 并注明与 files 目标 262160 的差异理由 | §2 F11④ |
| 6.4 | **认同**——`fuzz_replay.rs:513`/`ci-local.sh:13/32` 文案改 12 + 时间预算（+2-4 min）写入注释 | §2 F11⑤、§4.1③ |
| 6.5 | **认同**——oracle ③ 改 decode→re-encode 字节相等 | §2 F11 oracle、F5 测试② |
| 7.1 | **认同（中）**——登记草案按政策字段重写（影响面点名验收方/文档/测试）；观测行落点 = 「计数输入集/数值语义变化」additive 行 + `QD.md`（Q-B/Q-C 先例）；F1c/F8 另落「已知口径注记」 | §3 |
| 7.2 | **认同**——F5 比较口径改「再编码字节相等」；F10 加 `Err` 形状断言 | §2 F5/F10 测试 |
| 7.3 | **认同（可选）**——F2 在「已知口径注记」加一行 | §3 |
| 8 | **记录**——「看过没问题」清单复核成立；`gunzip` 上限与 `PENDING_CAP_BYTES` 解耦写进注释 | §2 F10① |
| 9 | **记录**——过门建议全部执行（必须项 1.1/1.2/6.2/7.1 + 同批项）；无豁免项 | §5.1 |

### 5.4 不认同项

**无。**（1.2 计为「部分认同」：记账/登记/预算全采纳，仅把「全部靠尺寸上限间接约束」这一形态
换成显式镜像预算——已按评审建议落地。）

---

## 6. 「二选一」类决策取证与裁定

| # | 决策 | 取证 | 裁定 |
|---|---|---|---|
| **D1** | `manifest::Loader::reload`：接线 vs 删 vs 保留 | ① Go `Reload()` 无生产触发（仅 `NewLoader` `load.go:60` 内调；`cmd/` 无调用点），CLI 离线 explain 每次新建 loader（`term_cli.go:829-835`）——Rust 同构（`mod.rs:652`、`term_cli.rs:1757`）；② R6 台账（tier `…/term-vt-backend/tasks.md:561`）要求「重载入口」——**方法即入口**（Go 注释 `load.go:12-13` 自认「给运维与 explain 用」）；③ 接线需新触发面（新 op = wire 变更 / env 轮询 = 新行为无需求） | **保留现状**（不删不接线）+ 方法注释写明「无运行时触发是 Go 同款；运维/explain 面每次新建即最新」 |
| **D2** | fuzz 目标本批做还是挂后 | ① ROADMAP §Q-D 明列；② 本机 `cargo-fuzz`+nightly 实测在、骨架/两轨口径现成（评审 6 复核）；③ F10 守卫是目标可用性前置，两者同批才自洽 | **做（F11）**：3 目标 × 2 轨 + 种子/文案同步 |
| **D3** | `title_stale` 加「时效语义」 | Go 同构 + 规格明文（`term-agent-state/spec.md:33-34/47-48`）；TTL 违反污染防线 | **不做**（登记） |
| **D4** | `screen_evidence_locked` 早退清 `last_scan_proc` | 与 Go 逐行同构（§1.2） | **剔除（误报）** |
| **D5** | 停滞置位是否含 surface 腿 | Go `noteStall` 仅 raw 4 站点；R6-design §9.7 在案 | **只接 raw（Go 语义）** |
| **D6** | `is_descendant_of` / `read_procs` 空白 / `encode_frame` 截断 | 三条均与 Go 逐行同构（证据 C） | **不做**（登记 §7） |
| **D7** | F1 上限取值与夹取/拒绝 | 证据 A（24B/格、两屏即时、96-192 GiB）+ 客户端现实余量（可读字号下 8K ≈850-960×250-270 在限内） | **夹取到 1000×500**（拒绝 UX 更差）；**不加 env**（安全边界）；不可读极小字号被夹 = 登记 |
| **D8** | F1c 镜像预算的路径 | 评审 1.2 建议 + 现状 `service.rs:1944` 取 `rows×10`；tier 规格「默认约 10 个视口」+「滚出镜像窗口 ⇒ FETCH-ROWS 按需拉取」（`term-surface-protocol/spec.md:137-158`） | **做**：`min(rows×10, 32MiB/cols 预算)`，登记数值语义；窄屏行为不变 |

---

## 7. 不做与残余登记（防「静默漏做」）

| 项 | 结论 | 证据/理由 |
|---|---|---|
| `title_stale` 时效语义 | **不做** | Go 同构 + tier 规格明文；TTL = 反规格 |
| `screen_evidence_locked` 早退清 `last_scan_proc` | **剔除（误报）** | 四道门/写点与 Go 逐行同构 |
| surface 腿停滞置位 | **不做** | Go 只给 raw；surface 超时即断腿（R6-design §9.7） |
| `is_descendant_of` 每调用建表 | **不做（登记）** | Go 同款；量级 ≈0.1 ms/会话/拍（估算），改面（跨 `AgentProbe` 穿参）不成比例 |
| `read_procs` 空白归一 | **不做** | Go 同款；消费面本就按空白分词 |
| `encode_frame`/`enc_hello` 静默截断 | **不做（登记残余）** | Go 同款（`encodeTermFrame`/`encHelloFlags`）；改 = wire 分叉。残余：>255 B 会话名 ⇒ nameLen 模 256 回绕 + 余下字节被当尾随块（`bad_capability`）；两侧同病，协议批再议 |
| `Loader::reload` | **保留不接线**（§6 D1） | 入口 API = 规格要求；无触发为 Go 同款 |
| 尺寸上限的 env 覆写 | **不做** | 安全边界不应由环境放大；预算类（`SCROLLBACK_LINES`）语义不同 |
| **raw CLI 不知情（评审 1.3）** | **登记残余** | raw CLI 不回读 ATTACHED 几何、SIGWINCH 继续报本机尺寸 ⇒ 被夹时出口日志可见、客户端无感（surface 客户端可从帧几何自恢复） |
| **回滚内存面** | **登记残余** | `MAX_COLS × SCROLLBACK_LINES × 24B ≈ 229 MiB/会话`（惰性填充、env 可调）；上限处 ×16 会话 ≈4 GB 最坏（需全灌满）；与 Go 同形，不在本批 |
| **不可读极小字号的全屏终端（>1000 列或 >500 行）被夹** | **登记残余** | 无 env 逃逸口（D7）；真实可读字号下 8K ≈850-960×250-270 在限内 |
| `PtyShared` 写入超时 | **登记残余** | `write_all` 无超时（P0 面已由 F3 移出锁外；超时需 nonblocking+fork 语义改造，风险大于收益） |
| term 线程 panic 后的数据一致性 | **登记残余** | F7 只保证「不静默僵尸 + 有日志 + 保守处置」；毒锁恢复的不一致面已在注释声明（`qbytes` 例） |
| `vt.rs:1308` 备用屏内容面（`?47h/?1047h` 不可达，B6/D-18） | **不在本批** | R6-gate1 §B6 已登记；仿真等价面（Q-J/协议批） |
| R6 文档三处口径销账 | **本批 `QD.md` 登记** | 评审 4-R6：R6-design §9.7（plain_text 残余未含备用屏恒空）/ R6-design:295-296（停滞登记与死分支不符，F4 修回后销账）/ R6.md:55（锁纪律口径 F3 后与实现一致） |
