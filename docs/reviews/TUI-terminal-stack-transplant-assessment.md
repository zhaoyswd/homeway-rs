# 评估：「整体搬 herdr 终端栈」的可行性（2026-10-12）

- 背景：TUI 验收期连续暴露渲染/终端交互缺陷（共 11 轮），用户提出评估把 herdr 终端
  相关部分整体移植过来。本文 = 尽调结论与路线建议。
- 尽调对象：`~/herdr-study/herdr`（Rust + ratatui 0.30 + crossterm 0.29，同栈；
  Apache-2.0，许可上允许借用）。

## 1. 「herdr 终端相关部分」的构成与规模（实测）

| 组成 | 内容 | 规模/约束 |
|---|---|---|
| VT 引擎 | vendored **libghostty-vt**（C/Zig） | `vendor/` **495MB**；`build.rs` 需 **zig 工具链**，target 白名单**不含 `aarch64-unknown-linux-ohos`**（列出即 panic） |
| 终端状态 + 服务端渲染 | `src/pane/terminal.rs`（ghostty 绑定 + 离屏 TestBackend 渲染 + 帧/补丁协议） | **7,366 行** |
| 渲染流协议 | `src/server/render_stream.rs` + `protocol/render_ansi.rs`（SemanticFrame / TerminalAnsi 双编码 + BlitEncoder） | 765 行 + 协议类型 |
| 客户端 shell | `src/client/shell/*`（布局/键位/渲染/滚动/弹出浮层/图形） | 其 client 段合计（含 ghostty bindings 5,288 + mod 4,602）**终端相关总计 41,810 行** |
| 周边 | pty 管理 / 输入编码 / 检测 / kitty 图形 / 主题同步 | 与上述耦合 |
| **我们整个 TUI** | `crates/homeway-cli/src/tui/*` | **5,406 行** |

「整体搬」= 引入 4.2 万行适配 + 495MB vendor + zig 构建链，**替换掉我们 5.4k 行的 TUI**，
并且要以 herdr 的 server-client 协议/App 模型为骨架重建我们的面板——这不是「换掉一个
模块」，是**把产品重写成 herdr 的客户端**。

## 2. 归因对照：已发现的缺陷分布（11 轮全量）

| # | 缺陷 | 所在层 | 是否 VT 仿真 |
|---|---|---|---|
| 1 | 切会话内容叠加 | 自研渲染桥（空格契约） | 否 |
| 2 | 滚轮漏历史 | 自研终端初始化（缺 mouse capture） | 否 |
| 3 | 光标不闪/位置无意义 | 自研（DECSCUSR/光标定位） | 否 |
| 4 | ↑↓ 导航即退出 | 自研事件分派（重构滑垒） | 否 |
| 5 | 腿泄漏 / 写者静默死 | 自研腿生命周期 | 否 |
| 6 | tty 死亡 100% CPU 自旋 | crossterm 0.29 缺陷 + 自研主循环结构 | 否 |
| 7 | codex 词粘连 / opencode 重叠 | 自研渲染桥（空格 vs 宽尾混判） | 否 |
| 8 | clear 散落 / ls 无换行 | 自研渲染桥（空串游标错位/宽头双推进） | 否 |
| 9 | 改窗后错乱 | 自研（本地 VT 从不跟随尺寸） | 否 |
| 10 | RESIZE 不跟随窗口 | 自研（跟随链两洞） | 否 |
| 11 | tmux 行首吞字（未归因） | 疑自研渲染桥残余 | 未定 |

**VT 仿真（alacritty）层面：0 例**。反证：参考实验（原始 pty vs 经 TUI）中 vim 的
「首行空白/退出还原」均被参考组证伪；codex/opencode 的字节流经服务端网格核对完整；
服务端（同 alacritty 系）的网格始终正确。

**herdr 一侧的强证据**：其渲染桥的空白格映射
（`pane/terminal.rs:3109`）：

```rust
Wide          => "  ",   // 宽带格
SpacerTail    => "",     // 宽字形尾格 → 空串（不打印）
Narrow|SpacerHead => " " // 其余 → 真实空格
```

**与我们三轮真终端调试后收敛的最终规则逐字一致**——即：这类「VT→ratatui 桥」问题
在 herdr 同样存在、同样必须在该层解决。**搬栈不会消灭该类缺陷；我们的桥已独立
收敛到与 herdr 相同的答案。**

## 3. 硬约束（若走整体搬）

1. **CI 三目标门**：本仓 `ci.yml` 对 homeway-cli 做 OHOS/musl 交叉 check；ghostty 的
   build.rs 不支持 OHOS（白名单 panic），需把整个 VT 依赖改 optional + cfg 门控 +
   CI 装 zig——与「对齐三件套」无关的纯基础设施债。
2. **双 VT 语义漂移**：服务端 term 服务（alacritty 系）负责会话、快照、检测、哨兵
   重绘（按**它的**网格重编码字节流）；客户端换 ghostty 后，每个边界行为都要双端
   核对（该成本长期化）。
3. **与既有对齐资产冲突**：wire Cell 契约 / golden 夹具 / 判据行体系建立在现服务端
   之上；客户端 VT 换型虽不直接碰 wire，但会把「一处语义两套实现」的面扩大。
4. **刚完成的 herdr 风格面板 UI** 会被 herdr client shell 覆盖——那是产品模型置换，
   不是终端栈修复。

## 4. 路线对比与建议

| 路线 | 成本 | 对已观测缺陷的预期收益 | 建议 |
|---|---|---|---|
| A. 整体搬 | 4.2 万行适配 + zig/OHOS 基建 + 重建面板 | **低**（已观测缺陷全在自研接缝，herdr 桥同样要在该层解题——其空白格表即为证明） | **不推荐** |
| B. 只借 ghostty VT（feature 门控的客户端可选 VT） | vendor C 构建链 + 双 VT 语义核对 | 低（无 VT 级缺陷在册） | 暂缓；**触发条件见下** |
| C. 定向借 + 继续收口自研桥 | 继续现状（每修一类加回归钉） | 高（11 轮每轮覆盖一整类；当前未决仅 1 项） | **推荐** |

**C 路线的具体动作**：
- 未决项（tmux 行首吞字）继续用 tmux 参照终端法收口——herdr 的桥代码（`pane/
  terminal.rs` 附近）可直接对照阅读其窄/宽/占位处理；
- 把 herdr 桥的**映射表**与本仓 `termview.rs` 的规则做一次逐条对账（写进测试钉）；
- **B 路线的触发条件**（届时再评估）：出现明确的 VT 级缺口——kitty 图形协议、
  undercurl/下划线色、复杂字素簇宽度、或 tmux 残留被确证为 alacritty 仿真差异时。

## 5. 结论

**整体搬不可行（成本/收益倒挂），但 herdr 作为「同栈参照实现」的价值应继续榨取**：
11 轮 bug 全在自研接缝、且我们最终收敛的规则与 herdr 逐字相同——这说明修的是正确
的层，方向不需要推翻；需要的是把「VT→ratatui 桥」这一层的规则补全并钉死。**当且仅当
出现 VT 级缺口（B 路线触发条件）时，才评估以 feature 门控方式引入 ghostty VT。**
