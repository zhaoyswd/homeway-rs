# term-attach-resilience · 技术评审记录（设计阶段，2026-10-16）

> **来源**：openspec 变更 `term-attach-resilience`（term attach 劣化路径三连修：
> TUI attach 画面不完整 / 断链退出不自愈 / 退出漏应答乱码）方案工件（proposal/specs/
> design/tasks）完成后，外部评审轮 r1（dsh headless，`/tmp/dsh-review/r1.oHgm3j/`）。
> 评审对象 = 方案（代码零改动）；评审者独立核对全部代码锚点。

## r1 结论摘要

- **贴合度**：诊断与代码现状全部对上（锚点逐条核对属实）；「attach 几何未变 ⇒ 哨兵是
  唯一重绘机制」「focus-in 有 `?1004` 门」「同 client_id 重拨即 self_reconnect」均成立。
- **主要缺口**：① 哨兵改动面被写成 attach 专用，实际 4 个触发点，真变路径会**新引入**
  300ms 窄列回归（apply+hop1 合并落中间值 C'-1）；② 重连的「会话级 vs 网络类」二分缺
  错误类型，`new` 重拨必被 already_exists 打死；③ 无限重试下拨号失败路径泄漏
  ControlClient（无 Drop）→ 约 20 分钟 EMFILE；④ 排空规范自相矛盾且分类器无行为效果；
  ⑤ Ctrl-C 在 raw 模式下不是信号（banner 空承诺）；另有矩阵漏项、本地面未分档、
  等待期 stdin 归属、标题跨重连还原错、判据登记缺用户可见面等中档意见十余条。
- **判定**：五重验让位语义完备（「第一跳不写 epoch」「rt.size() 只由 apply 更新」两个
  隐式不变量成立）；拆分方向自洽；否决项判定全部合理。

## 整改决策（已并入工件）

| r1 意见 | 归宿 |
|---|---|
| 2.1 真变路径窄列回归（中） | **采纳**：间隔哨兵收窄到「attach 且 `reg.size_applied == None`」单点；真变路径保持背靠背现状（真 SIGWINCH 已足够逼重绘）；`sentinel_count` 口径不变 |
| 3.1 new 重拨 already_exists（高） | **采纳**：首连收到 ATTACHED 后，本进程内重拨一律清 `CREATE\|ONLY_IF_ABSENT`（纯 attach 语义） |
| 3.3 ControlClient 泄漏（高） | **采纳**：重拨失败路径显式 `client.close()`；加「长重试 fd/线程稳定」验证任务 |
| 4.1/4.4 排空矛盾与简化（中） | **采纳①**：`poll(50ms) + tcflush(TCIFLUSH)` 不区分形态；**分类器迁移取消**（`pure_terminal_reply` 保持 service.rs 私有） |
| 3.2/3.4/3.5/3.6/3.7/3.8/3.9/3.10（中低） | **采纳**：DialErr 类型化（矩阵前置）；矩阵补 stdout 失败/stdin EOF/poll 失败/Realign 检查；本地面链路终结局；等待期 poll([stdin,signal]) 喂 dm、孤立 0x03 退出、丢弃其余；标题查询与 client_id 进程级一次（OnceLock）；退避常量注记 |
| 2.2/2.3/2.4/2.6/2.7/2.8/4.2/4.3/4.5/3.11/5.1/5.2/5.3（中低） | **采纳**：spawn 失败锁内兜底恢复；「第一跳不写 epoch」不变量注释+反证单测；vt 窗口错位入 Risks；pty.rs 测试夹具标注；Arc 签名涟漪入任务；config 字段替代 env 直读；排空统一 50ms 全退出路径；8KiB/截断面随 tcflush 消失；30s 写超时登记不修；判据登记扩到 5 行；两道评审门入任务；openpty 单测 + opencode 归真机档 |
| ⑥ 第二跳可交既有周期面执行 | **不采纳**（保留意见）：term 服务无通用定时轮（出口守护驱动拍属另一进程），为单一 300ms 延迟建周期设施是过度设计；KILL_GRACE 式短命线程成本可忽略，取舍已写入 design D1 |
| 2.5 哨兵令牌防叠加（低-中） | **部分采纳**：叠加瞬态如实入 Risks；不设令牌（仅竞态重连可触发、终值收敛，机制不成比例） |
| 4.4 附带 TCSAFLUSH 替代 | **不采纳**（保留意见）：TCSAFLUSH 改变一切退出路径的输入语义（用户向 shell 的 type-ahead 也会被冲），面过大；保 TCSANOW + 显式 poll+tcflush，语义只作用于本退出 |

## 后续

实现批完成后走完工代码评审（0.2，记录入 `docs/reviews/term-attach-resilience.md`）。
