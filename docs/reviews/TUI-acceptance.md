# TUI 面板验收清单（add-tui-panel 组 5）

- 日期：2026-10-11（清单工件化）；执行记录随批次续写
- 环境：`tools/tui-smoke.sh --hands-on`（本地 Rust 出口 = `tools/local-rust-exit.sh`，state = `/tmp/homeway-rs-rustexit-0`）
- 约定：每项「通过/失败 + 现象一句话」；失败项回 openspec tasks 并修复后复测

## 5.1 单实例端到端

| # | 步骤 | 判据（spec 需求） | 结果 |
|---|---|---|---|
| 1.1 | `homeway tui --state /tmp/homeway-rs-rustexit-0` | 四区布局同屏；无选中时右侧引导提示（1） | |
| 1.2 | 主机列表观察 | 本机条目（local 标签）+ 远端主机；确定序（本机优先+名称升序）（2） | |
| 1.3 | 会话列表观察 | 与 App/`term list` 同名同序（最近活跃前）；徽章 working/blocked/idle/unknown/无（3） | |
| 1.4 | 选中会话 Enter → 终端焦点 | 屏幕内容出现；`ls`/vim/htop 正常渲染（宽字符、全屏重绘）（4） | |
| 1.5 | 终端焦点按 Alt 组合 / F1 / Ctrl+C | 原样送达会话（4） | |
| 1.6 | F12 | 返回面板导航焦点（4） | |
| 1.7 | 状态行 | 主机名 + 形态（direct/relay/local）+ 延迟（采样年龄）+ 流量（5） | |
| 1.8 | 粘贴多行文本 | 会话 2004 启用时按括号粘贴转发（4） | |

## 5.2 断线与边界（spec 需求 5/6）

| # | 步骤 | 判据 | 结果 |
|---|---|---|---|
| 2.1 | kill 出口进程数秒后重启 | 自动重附，屏幕经回放恢复；状态行反映链路异常（6） | |
| 2.2 | 远端 kill 会话（`term delete` 或会话内 exit） | 停拨 + 「会话已结束」+ 回落到最近活跃（6） | |
| 2.3 | 占满 8 腿（8 个 `term attach`）再让面板接入 | 面板不接入、显示腿满提示（5）；被让位时 10–30s 退避（5） | |
| 2.4 | 链路 down 时观察 | 主机留守不回落（10） | |

## 5.3 双实例同显（spec 需求 9/10）

| # | 步骤 | 判据 | 结果 |
|---|---|---|---|
| 3.1 | 两个 tui 并行（同 state） | A 切换选中，B ≤1s 镜像（含列表与视图） | |
| 3.2 | 两实例看同一会话 | 内容一致（滚动位置可各异） | |
| 3.3 | A 退出 | B 不受影响；选中持久保留 | |
| 3.4 | 持久化选中指向已删会话后重启 | 回落链：同主机最近活跃 → 第一可用主机 | |
| 3.5 | A 本地回落 | B 的选中不被 A 的回落改写 | |

## 5.4 既有客户端共存（spec 需求 13）

| # | 步骤 | 判据 | 结果 |
|---|---|---|---|
| 4.1 | 面板接入前后 `term list` / `term attach` 输出 | 行为不变 | |
| 4.2 | 面板接入时 App（或 surface 客户端）观察同一会话 | 无全屏重绘风暴（HELLO 尺寸对齐生效，r2 N-4） | |
| 4.3 | 面板腿在场时 App 接入 | 活动腿切换/剪贴板缓存重置外无其他可见影响 | |

## 5.5 轮询成本量化（r2 D-10 / r3 C）

| # | 步骤 | 判据 | 结果 |
|---|---|---|---|
| 5.1 | 空转活动态面板 vs 无面板基线（1 分钟） | daemon CPU 增量 ≤2% 单核；ctx/s 增量 ≤50 | |
| 5.2 | QUIC 新建流计数 | 活动态 ≤35/min；非活动态 ≤8/min | |

## 记录

（执行时逐项回填；失败项附现象与修复 commit）

- 2026-10-12　5.1 手测发现：**切换会话后多会话内容叠加**。根因 = VT 契约「无文本 =
  symbol 空串」在真终端是 Print no-op（空串格不擦旧字符），且会话网格小于右侧 pane
  时带区无人写。修复 = `termview.rs` 渲染前整区 `Cell::reset()` 铺真空格 + 空串格落
  真实空格（宽字符尾占位格保持不打印）；回归钉 `session_switch_clears_previous_content`。
  单会话内 `clear`/全屏应用重绘同路径受益。待复测。
- 2026-10-12　5.1 手测发现（第二轮）：**①滚轮把进面板前的历史滚出来**——备用屏虽已进，
  但没开鼠标捕获，宿主终端在备用屏下仍自己滚 scrollback；修复 = init 加
  `EnableMouseCapture`、三处还原路径（正常退出/panic hook/raw 失败）对称关掉
  （4.4b 的滚轮回看/点选路径就此接活）。**②「光标位置只显示 %」**——pty+pyte 实证
  当前二进制不可复现（提示符整行渲染、内容贴底）；归因 = 上一轮 stale 二进制
  （00:15，缺叠加修复）的残留 + 宿主光标停在无语义位置造成的观感混淆。顺带修复：
  终端焦点内宿主光标定位到会话 VT 光标（回看中隐藏；`cursor_in_pane` 夹取，
  herdr host cursor 同款实践）。复核结论：**RESIZE 链路在当前代码上是通的**
  （探针实证 86×23 → 78×34 应用、PTY 填满 pane、提示符贴底）——此前「PTY 不跟随
  pane」的怀疑被探针证伪（早前盯错了会话：探针接的是 work 不是 demo）。
- 2026-10-12　5.1 手测发现（第五轮）：**「↑↓ 导航后 TUI 直接退出」= bb8f90e 重构滑垒**
  ——`handle_event` 的 Panel 臂把 `apply_panel` 的返回（**选中已变化**）误当 quit 旗：
  任何改选中的按键（第二次 ↓ 选中另一会话）都被当成 `q`，走优雅退出（exit 0、还原
  完整、无 panic——故「像崩溃」实为静默退出）。定位 = 退出路径插桩（HWDBG-EXIT 标记
  进 pty raw 流）+ 探针 ↓×2 复现。修复 = (quit, changed) 双旗恢复原语义；
  全导航 battery（落穿/回穿/Enter/F12/Tab/连打）实证存活。教训：**重构事件分派时
  布尔语义折叠是高危动作，退出路径插桩探针是定位静默退出的最快手段**。
- 2026-10-12　5.1 手测发现（第六轮）：**「终端区域高度不随窗口变化」+「命令后出现带底色
  %」同根**——RESIZE 跟随有两个洞：①窗口尺寸变化只在终端焦点内置 pending_resize（面板
  焦点改窗丢失）；②首交互只发一次、无「尺寸已变即跟随」语义 ⇒ 面板焦点改窗后回同一会话
  （世代未变、interacted 已置位）**永远不再发 RESIZE**，会话几何冻结在旧尺寸；zsh 在
  提示符宽度与 PTY 错位时打印部分行标记（反白 %，pyte 文本 dump 此前看不见——属性感知
  探针补上）。修复 = `TermView.last_sent` 尺寸登记 + 三路跟随（首交互必发〔镜像纪律
  r2 N-4 保持：非交互实例永不发〕/ Passthrough 键尺寸变化即重发 / Enter 进终端焦点补发
  + pending_resize 路径登记）。属性感知探针实证：改窗 100x20 + Enter 回会话 ⇒ work
  78x34→65x19 跟随成功；两轮命令输出反白 % 零出现。
- 2026-10-12　5.1 手测发现（第七轮）：**「敲命令没反应/命令消失」复发——两个确定性缺陷**：
  ①**切会话漏关旧腿**：`leg_closer` 槽只有写入没有关闭——旧腿读循环阻塞在 read_frame
  （空闲会话无帧）世代检查永远跑不到，duplicate 句柄 Drop 不关连接 ⇒ 空闲腿永久泄漏
  （现场实证：单 TUI 进程、work/demo 各挂一条腿）。修复 = spawn_leg 换代前 take+close
  上一条腿的 closer + 世代已变时不注册（防迟到注册覆盖新腿 closer 造成下一轮泄漏）。
  ②**输入写者死得静默**：write_frame 失败仅 break——UI 继续把键入送进死写者（键入凭空
  消失、相位无提示）。修复 = 写者持 closer 副本，写失败主动收口 ⇒ 读循环立即退出进入
  判定重拨（自愈 + 相位可见）。现场时间线：14:39 用户三次切换（work→demo→work），
  14:39:28 的 demo 腿即泄漏腿。
- 2026-10-12　UI 改造批（用户需求：**向 herdr 视觉对齐**，参照 `~/herdr-study/herdr`
  的 sidebar/panes 实现）：①**无边框区块**替换 Block 边框盒——小标题 ` 主机`/` 会话`
  （overlay0+BOLD，herdr ` spaces`/` agents` 同款）+ 标题右侧 ` +` 按钮 +
  区块间 `─` 分隔线（surface_dim）；②**整行底色选中**（selection_bg 49,50,68）替换
  反白高亮；③**会话列表两行化**（`● 会话名` + `徽章 · 标题`，徽章状态色
  working=yellow/blocked=red/idle=green）+ 主机/会话条目均带 gap 空行；④**终端 pane
  标题栏**（聚焦 = accent+BOLD，herdr pane title 同款）显示会话名与标题 + 右侧 ` ✕`
  （点击 = 断开观察当前会话，非破坏——清选中回引导态，不删会话）；⑤**主题 token 模块**
  `theme.rs`（herdr `Palette::catppuccin()` 同源色值钉）；⑥**` +` 真实可用**：点击弹
  输入浮层（selection_bg 行）——主机 + = token 校验（`token::decode` 前置）→ 控制面
  `host.add`；会话 + = `validate_name` → term CREATE op（-d 语义）；Enter 提交 /
  Esc 取消 / 失败红字留在浮层。⑦命中区 `hit_zones()` 纯布局函数（点击与绘制同源）；
  `term_area` 存全屏区域（点击布局基准），RESIZE 位点经 `term_content_of` 派生内容区。
  探针实证：合成 SGR 鼠标点击 + → 浮层 → 键入 → 会话真实创建；非法 token 报错红字；
  Esc 取消。71 用例绿 clippy 零告警。
- 2026-10-12　5.1 手测发现（第八轮，**重炮 TUI 场景**）：**opencode 不清屏重叠 / codex
  内容错乱** 根因 = `apply_cell` 把「带样式空格」与「宽字符尾占位」混判（都命中
  `width==0` → 写空串）：core 口径 style 空格 = width0/skip=false、宽尾 = skip=true；
  空串在真终端是 Print no-op，**而 ratatui 行内游标假设它已打印 ⇒ 该行后续字符的
  MoveTo 全被省略、整行逐字左移**（codex 菜单词粘连 `Backgroundserverhas...`）+ 旧内容
  不擦（opencode 重叠）。修复 = 按 `skip` 位分流（skip→空串不打印 / 空格→真实空格 /
  其余→字形）；回归钉 `styled_space_renders_real_space_not_empty`。对照实验法：
  **同款应用原始 pty（参考）vs 经 TUI，pyte 逐行比**——vim/vim 退出还原/vim 首行空白
  均先证伪（参考组同样形态），避免误修。终验：codex（菜单/logo 盲文/对话/CJK/标题栏
  实时 spinner）与 opencode（ASCII logo/输入框/状态行/标题栏 OSC）全量正确渲染。
  **结论：错误全在自研接缝（渲染映射），alacritty VT 面对重炮 TUI 字节级正确——
  「整体搬 herdr 终端栈」暂不成立（herdr 的 ghostty VT 也修不了这类映射缝错误）；
  若未来出现 VT 级缺口（kitty 图形协议等）再评估借用 herdr 具体件。**
- 2026-10-12　5.1 手测发现（第九轮）：**「ls 无换行 / pwd 尾部怪字符」= 第八轮根因的
  更广影响面**——带样式空格混判同样破坏普通 shell 的列对齐输出（列间空格被吞 ⇒ 视觉
  粘连）与提示符行对齐。归因证据：①修复二进制 22:11 构建、用户 TUI 22:15 启动 ⇒
  症状观察于重启前的 a1a56f9 副本；②镜像只读 dump 用户真实会话网格：三列 ls/pwd/
  提示符全部规范；③当前二进制对同内容（重放 + 同几何）渲染逐行干净。**教训：渲染
  映射类根因的影响面要在「普通 shell 列对齐/提示符/全屏应用」三类场景分别验证，
  不能只看全屏应用一例。**
- 2026-10-12　5.1 手测发现（第十轮，渲染循环重写）：**「clear 后散落字符/ls 无换行」
  的残留类根因**——渲染单元格循环三处确证缺陷：①**空串 skip 格 + ratatui 行内游标
  假设**（后端源码实证：`last_pos` 无条件推进，空串 Print 零输出 ⇒ 后续连贯字符省
  MoveTo、整段落位左移）；②**宽头格双推进**（旧代码头格后 x+=1 + 显式写尾格 + 循环
  再 x+=1 ⇒ 宽字符后整行右移一格留空洞）；③**core 头格 width 恒 1**（「两列」语义
  只由尾格 skip 位表达——按 width==2 判头格是错的）。重写 = 逐列 1:1 推进 +
  「紧跟真字符格的 skip=宽尾写空串 / 其余 skip 与空格=真实空格」二分 +
  `prev_real` 上下文判定；回归钉 `wide_char_column_alignment_pin`（逐列断言，
  左移/右移都会红）+ `styled_space_renders_real_space`。73 用例绿 clippy 零告警。
  **残留未决**：tmux 真终端语义下 ls 流式输出偶发「行首吞 1-2 字符」（pyte 与
  服务端网格均不可见、机制未归因；疑 tmux 解释差异或更深的输出时序问题），
  已如实登记待复现取证（用户侧截图/终端类型为下一步判据）。
- 2026-10-12　5.1 手测发现（第十一轮）：**「进入 opencode 后改窗不变为正确大小/错乱」
  真因 = 客户端本地 VT 从不跟随尺寸**——窗口 resize 只把新尺寸下发给会话侧 PTY
  （RESIZE 帧 → 服务端 setsize），但 TUI 自己的 `SessionVt` 网格仍停在接入时的尺寸
  （`SessionVt::resize` 存在但**全仓无人调用**，实锤）；重炮应用按新尺寸重画 ⇒ 本地
  网格折行错位。修复 = 三处 RESIZE 下发点同拍调用 `tv.leg.resize(w, h)`（Passthrough
  跟随/Enter 补发/pending_resize）。tmux 实证：opencode 启动后改窗 100x28，logo 与
  输入框正确重排到新几何。**「退出 opencode 后背景色残留」本批未能复现验证**——
  tmux 合成按键驱动不了 opencode 的退出（键全进其输入框，进程不退），残留机制
  疑与 df5f713 修复前「缓冲/终端失同步」同族（错位伪影在 ratatui 认为无变化的格
  上存活）；待用户在新二进制上复测，若仍残留以截图定位。
- 2026-10-12　**输出接缝整体搬运（tui-blit-layer，用户拍板）**——经评估确认 herdr 的
  输出接缝 = `render_ansi.rs` BlitEncoder（纯 Rust，不牵 ghostty/zig/OHOS）后整体移植：
  · `tui/frame.rs` = 帧模型（CellData/CursorState/FrameData + `from_ratatui_buffer` +
  颜色/修饰符打包，移植自 herdr wire.rs，Apache-2.0）
  · `tui/blit.rs` = BlitEncoder（整帧/差分双路径、列推进按 unicode 宽度自算、宽尾
  `to_skip`/宽度变化 `invalidated` 失效传播、`CSI ?2026` 同步输出、光标先隐后定、
  DECSCUSR 随帧、每帧 OSC8 复位）+ **BlitBackend**（Buffer-backed 后端：ratatui 仅
  离屏合成，flush = Buffer→帧→编码→stdout——ratatui 的 crossterm 后端彻底退出输出
  路径）
  · 接线：`Terminal<BlitBackend>` + 光标槽（渲染闭包写、flush 读，替代手写 DECSCUSR
  边沿）；termview 宽尾改显式 `skip` 位模型
  · 回归钉 14 个（宽字形列推进/宽尾跳位/差分/半角浊音假名/SGR/光标记忆/尺寸变化整屏/
  同步输出帧序）+ 原 126 用例全绿
  · **tmux 全场景回归（显示列切片——修正探针 CJK 切列 bug）**：ls 逐行完整（含此前
  误报「行首吞字」的 verify_token.py/web.jpeg——**该残留全为探针切片伪影，帧转储与
  字节转储双证渲染一直正确**）、vim 全屏/退出、clear、中文回显全部通过
  · 新发现（独立于输出层）：改窗后 shell 提示符重绘对齐瑕疵（zsh 重绘×VT 重排交互）
  记入下一步
- 2026-10-12　**blit 移植首日回归修复（两处实症）**：①**光标不可见**——ui_loop 自建了
  光标槽 Arc 而 BlitBackend 内部另有其一（渲染写 A、编码读 B 恒 None）；修=槽必须取自
  backend 实例。②**改窗整屏空白**——根因（读 ratatui 源码坐实）：`apply_buffer_with_cursor`
  为 `flush(差分喂送)→swap→backend.flush()` 双冲洗，且 resize 路径 `clear_viewport`
  会复位内部缓冲并自走一轮 flush——我们靠 diff 增量维护的镜像缓冲被清空后，ratatui
  两内部缓冲彼此一致、diff 恒空、**永不复喂**（帧转储实证 nonblank 216→0 不复原）。
  修=**放弃镜像，改快照驱动**：`Terminal::draw` 返回的 `CompletedFrame.buffer` 即当帧
  完整快照——runtime 每帧取它喂 `BlitEncoder`（后端只剩报尺寸/收 clear 旗/光标槽），
  与 herdr 的「编码器吃完整帧」形态一致，diff-mirror 失同步类整体消除。
  tmux 实证：光标 flag=1 且改窗后位置正确、100×28/120×35 双向改窗内容完整、ls 全行
  完整。126 用例绿 clippy 零告警。
- 2026-10-12　5.1 手测发现（第四轮，**高危**）：**「work 会话敲命令回车无反应、命令消失」
  真因 = TUI 进程 tty 死亡后 100% CPU 永久自旋**——crossterm 0.29 `try_read` 内圈在
  `read` 返回 `Ok(0)`（EOF）或非 WouldBlock 错误时既不 break 也不返回（源码实证），
  而 macOS pty master 关闭后 slave **恒「可读」且 read 恒立即 EOF**（py 实验）⇒ 主循环
  困死在 poll 里：输入永远收不到、SIGTERM 旗标永远不被检查（僵尸对 SIGTERM 免疫，
  只能 SIGKILL）。现场证据：**四个 TUI 僵尸各烧满一核数小时**（最长 560 CPU 分钟）、
  work/demo 各残留 2 条腿、`??` 无控制终端仍在跑——用户测试的 TUI 正是其中一个
  tty 已死的僵尸（打字无人消费，画面冻结在最后一帧）。修复 = **输入泵独立线程 +
  有界通道**（主循环 `recv_timeout(100ms)`，永不再困在 crossterm 里）+ 双保险退出：
  ①EOF 看门狗（fd 恒可读而泵 1s 零产出 ⇒ 终端失联）②draw 失败（tty 写死）——
  两条路都走既有还原序列干净退出；副产：SIGHUP/SIGTERM 旗标现在每拍必查（tab 关闭
  可优雅退出）。验证：EOF 探针 master 死后 **0.1s 干净退出**（旧二进制同场景永久
  自旋）；僵尸全清后 work/demo 腿归零。
- 2026-10-12　5.1 手测发现（第三轮，验收期改进批）：**①「命令输出完毕后多一行黑底 %」
  ——隔离实例全 battery（焦点模拟/长命令/vim 开关/多命令）实证不可复现**，standout
  序列零命中。该形态 = zsh 部分行标记（standout %，浅色主题下呈黑底）⇒ 疑点 =
  多腿/多实例干扰（调试期间曾在用户在测的会话上并行接腿——同 client_id 双腿时
  n=2/8 共存而**未见 REPLACED**，与 r2 N-2「同 client_id 走同实例替换」契约存疑，
  待 5.3 双实例测试复核）。**②光标不闪烁** = 宿主光标样式未设——终端焦点沿上
  `SetCursorStyle::BlinkingBlock`、离开/退出还原 `DefaultUserShape`。**③主机条目
  两行化**（用户需求，参考 tier 首页 HostCard）：`● 名称` + 副标题行（已连接 =
  `延迟 42ms(3s) · ↑tx ↓rx`〔采样年龄 spec 需求 8 随迁〕；连接中 = 阶段文案；
  未连接 = 主机 id 掩码〔tier 同款〕；本机 = `local`）；圆点四态 = 直连绿/中继黄/
  连接中闪烁/未连接灰；逐主机速率差分（原单选中主机）；ListState 滚动跟随选中；
  点击命中改两行映射；状态行瘦身为相位/回看/焦点尾巴。调试纪律备注：**探针/调试
  一律用隔离实例（/tmp/homeway-tui-probe，独立端口 41661/41761、关 UPnP/STUN），
  勿接用户在测的会话**。
