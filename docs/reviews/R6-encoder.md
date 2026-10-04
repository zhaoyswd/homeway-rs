# R6 编码器/帧族代码评审记录（第二道门，实装批）

> 评审对象：`git log e06c8d2..HEAD`（cb7ebe8 键/鼠编码器+应答器补全面、5c9d4fc 帧族+
> 设计增量；eeaec00 会话注册表在评审期间由并行工作落库，评审者已注明不在被评范围）。
> 评审方式：dsh 外部评审（`r1.NpLX0n` 轮）——原文对读 + 独立 crate 行为探针实测 +
> 向量/fixture 复跑。记录 = 逐条发现 → 整改状态；高危全整改。

## 结论摘要

| 严重度 | 数量 | 处置 |
|---|---|---|
| 高 | 1（#1 截断 panic） | **已修**（字节级截断 + CJK 回归） |
| 中 | 3（#2 dec_ended 越界 / #3 B5 reset 半套 / #4 热路径拷贝） | **全修**（守卫+回归 / 无条件覆盖+回归 / tail 空零拷贝） |
| 低 | 11（#5..#15） | 9 修 + 2 项以「设计登记」收口（#7 DECSLRM / #9 lossy）+ #14 的 OnceLock 表保留（Box::leak 与死变量已清） |

评审者另确认的「看过没问题」面（摘）：kitty 表 81/81 与 kitty.zig 逐条全等、
function_keys 四道门判定序一致、CSIu 两套位序无混用、扫描器字节不重不漏+应答流内
序正确、op/词表/截断上限逐值一致、thiserror 纪律、三 parity 逐字节全量。

## 逐条发现与整改

### #1 高｜frames.rs 四处 `&str` 按字节截断 panic（UTF-8 边界）
- `enc_ended/enc_state/enc_error/enc_name` 的 `&s[..min(N)]` 在 CJK 截点直接 panic；
  评审者实测复现（513B 中文标题 / 66B 中文 name 等）。可达性：title 来自 OSC 0/2
  （任意程序 `printf '\033]0;…'`）⇒ 服务端崩溃。
- **整改**：`truncate_bytes()`（Go 按字节切同语义；载荷是 `Vec<u8>` 无 UTF-8 约束，
  绝不 panic 且长度与 Go 逐字节一致）+ `truncation_limits` 补四条 CJK 用例。

### #2 中｜`dec_ended` 声明越界切片 panic、丢长度守卫
- `&p[5..5+p[4]]` 只查了 `p.len() >= 5`；Go `decEndedParts` 有 `len >= 5+n` 守卫。
- **整改**：`n.min(p.len()-5)` 钳制（同 dec_state 风格）+ 越界用例（答空 reason）。

### #3 中｜B5 的 reset 语义只做对一半（与 ghostty 实测不符）
- 088f71f 的 reset 写成「当前单值 == 该模式才清」；ghostty 是**无条件覆盖**。
  评审者探针实测：`?1000h ?1002h ?1000l` → press 仍报（应不报）；`?1000h ?1006h
  ?1005l` → 仍 SGR（应回落 x10）。向量抓不到（每 mode 只单独 set）。
- **整改**：unset 一律 `mouse_tracking = None`（9/1000/1002/1003）与
  `mouse_format = Default`（1005/1006/1015）+ 交错回归测试三条。

### #4 中｜`write_collecting` 每批全量拷贝
- tail 空（绝大多数批）也 `p.to_vec()`。**整改**：tail 空 = 直接扫 `p`（零分配），
  跨块罕见路径才拼接。

### #5 低｜DCS 参数段 > 32B 的跨块续接静默丢应答
- `SCAN_CARRY_MAX=32` 与「参数不设限」矛盾；实测 params=33 split@34 无应答。
- **整改**：DCS 候选回找最后一个 `\x1bP`（4KiB 病态上限）+ params=40 跨块回归。

### #6 低｜DECSCUSR 应答双来源 OR
- `dec_modes[12] || cursor_style().blinking`：`?12h` + `CSI 2 q` ⇒ 误答 `1 q`。
  ghostty 的 mode 12 是唯一真源（setCursorStyle 写模式位）。
- **整改**：`set_cursor_style` 拦截镜像写 `dec_modes[12]`（None=默认不闪），
  `decscusr_value` 只读模式位 + 两个交错回归。

### #7 低｜DECSLRM 恒答 `0$r` 未进设计登记 → **登记收口**
- R6-design §9.7 残余差异表新增（alacritty 无左右边距面；真应用不开 69）。

### #8 低｜`dec_input` FOCUS 载荷过宽 → **已修**（`len < 2` 报错，Go 同款）。

### #9 低｜`from_utf8_lossy` 与 Go 原样字节的差异 → **登记收口**
- §9.7 登记（name 有词法校验兜底；病态输入面的字节替换；优先级低于 panic 安全）。

### #10 低｜fixture 三条负例被静默跳过 → **已修**
- `short_header/truncated` 走 `read_frame` 断 Err；`bad_payload` 走 `dec_hello` 断
  `HelloLen`；计数改为「正例 10 + 负例 3」全数消费。

### #11 低｜parity 只比拼接字节、丢 chunks 分片契约 → **已修**
- 逐片收集 + `pieces_cover_chunks`（piece 只允许更细分、禁跨界/乱序——vte 对多索引
  OSC 4 逐参分发 vs ghostty 合成一条，拼接字节才是 wire 契约）+ 每案四个切点的跨块
  拆喂轮。

### #12 低｜案数下限 51 ≠ 实际 57 → **已修**（`>= 57`）。

### #13 低｜`pc_style_function_key` 每键多一次分配 → **已修**（`Cow<'static, [u8]>`，
静态项借用零分配）。

### #14 低｜`Box::leak` 静态串 + 死变量 → **已修**（kpd 直接收完整字面量、
`leak_concat` 删除、scan 元组去 `start`）。OnceLock 表保留（建一次、换 const 表的
收益边际）。

### #15 低｜mod.rs 文档仍列 frames 未就位 → **已修**。

## 整改后状态

`cargo test -p homeway-core --lib`：**193 全绿**（+3 回归测试）；clippy all-targets
**0 warning**。三 parity（键 387 / 鼠标 160 / 应答 57×〔含跨块 4 切点〕）全量逐字节绿。

## 评审者建议采纳情况

- 「CI 补 `--target x86_64-unknown-linux-gnu` 的 cargo check 让 linux 分支有编译面
  判据」——**采纳、归 6g**（D-10 的 linux 向量同批；本机只跑 darwin 测试）。
- 「向量生成器补交错 set/unset 与 >32B DCS 参数案」——采纳，归下一次基线重跑批
  （`tools/gen-vectors.sh` 确定性 diff 门内）；Rust 侧回归测试已先行钉死行为。
