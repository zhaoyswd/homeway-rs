# TUI 实现批代码评审（add-tui-panel 任务 5.8 代码评审门）

- 日期：2026-10-11
- 评审对象：实施批 22715a9..69adefd（12 commit）——tui 模块 13 文件（~4.5k 行）+ term_cli 可见性缝 + core vt.rs 滚动面 + smoke 工件
- 评审形态：**双层对抗**——dsh headless 外部子评审（原存档 /tmp/dsh-review/r1.3n5SLW/output.md）+ 评审者逐模块复核（3 项实测取证），本文 = 复核版全文（原始落盘 /tmp/dsh-review/r4.IUdkGT/output.md）
- 总判定：模型层/谓词层扎实（61 用例、clippy 0、三目标绿、依赖单版本），**装配层（runtime.rs，零单测覆盖）有必修缺陷**；放行判定 = 手测前必修 3 条、同批 12 条、登记若干
- 处置：**全部采纳**（外部条目经评审者纠错 3 处后全收），下表为本仓处置对照；修复 commit = 96831f1（A 组）+ 后续 B 组 commit

## A 组（手测前必修，已修——commit 96831f1）

| 条 | 摘要 | 修复 |
|---|---|---|
| F1（高，评审者自身发现） | 本机会话**永不上线**：leg 线程为本机拼 serve、`list_once` 本机分支再拼 ⇒ `<state>/serve/serve/term.sock`；本机是默认选中项 | `list_once` 契约统一「恒传 state 根」；dial 目录仅在拨号点 join serve；judge/vacancy 全部改传根 |
| 高2 | **RESIZE 被包成 DATA 帧**写进 PTY（输入写者对通道一切字节 `write_frame(Op::DATA)`）⇒ 首键注垃圾、服务端永收不到尺寸 | 输入通道类型化 `LegInput{Data,Resize}`；写者经 `input_frame` 分派；测试钉「RESIZE 必发 Op::RESIZE」 |
| 高1（家族） | 冷启动（无选中文件）/回落链换目标/spawn 当刻 LIST 失败 ⇒ 腿永不挂 | 挂腿改**状态收敛**：UI 每拍对比「期望目标 ≠ 已挂目标」即重挂——覆盖冷启动/恢复/回落/镜像全路径 |

## B 组（同批必修，已修——B 组 commit）

| 条 | 摘要 | 处置 |
|---|---|---|
| 高3 | 面板焦点切换未按会话 1004 注入失焦/复得 | `push_focus`（Enter=复得/F12=失焦），与外层事件经 `session_focus` 态去重 |
| F2 | 腿满提示**连文案都不存在**（相位显示靠手写五处字符串） | 相位由监督决策点驱动：`set_phase`（不推世代）+ `vacancy_text`（数量/类型构成/活跃标记，spec 需求 5 两场景）；`SessionView` 保留 clients 明细 |
| 高4 | SIGTERM/SIGHUP 无 handler ⇒ 终端残留 | 信号旗标 + UI 心跳还原退出（libc::signal，经函数指针 usize 转换过 clippy） |
| 低2 | 活动态判据缺「外层焦点在面板」 | `outer_focus`（1004）入 `active()`；终端不上报焦点时按活动保守处理 |
| F3 | 需求 8「显示采样年龄」SHALL 未实现 | 状态行恒显年龄：`延迟 42ms(60s)`；>120s → `(Ns 陈旧)`；测试对齐 |
| 中1 | 粘贴 >64KiB 被 encode_frame 截断且无闭合序列 | `encode_paste_fragments`：48KiB 分片、首片起始/末片结束、中间裸文本；测试断言片界与正文无损 |
| F5 | 旧腿无主动收口（依赖对端 REPLACED） | `Shared.leg_closer` 注册 + spawn/退出主动 close；`TermCloser::Noop` 变体 |
| 高5 | `TermCloser::Local` 失败回落 `/dev/null` connect unwrap 必 panic | 改 `Noop`（/dev/null 非 UDS 必 ENOTSOCK） |
| F4 | 同世代重接后写者线程双消费者 | 输入通道**随接入换代**：每接入建新通道，shared 槽替换 ⇒ UI 旧 Sender 立刻 Err、旧写者收尾 |
| 中6 | 状态拍每秒新建控制面连接 | 连接复用（失败重拨） |
| 中4 | Event::Resize 丢弃 | pending_resize 旗标 + 画后（新 area 落位）下发 RESIZE |
| 中3 | `Gone.reason` 十处写零消费 | UI drain 消费 reason → 相位显示 |
| 低4 | 文档漂移（core 零改动互斥/计数 stale/3.2 未勾） | 本批订正（proposal/design 口径 + 3.2 勾选） |
| 低5（部分） | 泛豁免掩盖死代码 | runtime 装配层最小用例补强（input_frame 契约钉）；其余豁免收窄随归档批 |

## C 组（登记，不阻塞）

- 中7：attached_size 校验（存而未读）——随 4.8 后续消费
- 低1：cursor/selected 失配（导航 clamp 已兜底）
- 低3：`#![allow(dead_code)]` 收窄（归档批清理）
- 低2 补充：外层焦点不可知终端的保守策略已内置

## 评审者「看过没问题」声明（复核版）

锁面与死锁；通道背压；wire/契约面（core diff 仅 vt.rs、词表零改动）；4.8 滚动面语义 + golden parity；三目标编译面（零 cfg 分支、依赖单版本）；模型层测试有效性；需求 11（面板不发终结语义帧）。

## 遗留

- runtime 装配层零单测是本轮全部高项的根因——A/B 修复已补 input_frame/挂腿收敛两类纯函数钉，**集成层覆盖由组 5 手测清单（TUI-acceptance.md）承担**
- 手测 5.1–5.5 按清单执行；5.9 归档前复跑 smoke + 三目标
