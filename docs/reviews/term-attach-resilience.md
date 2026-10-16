# term-attach-resilience · 完工代码评审记录（第 2 道门，2026-10-16）

> **来源**：openspec 变更 `term-attach-resilience` 实现批（commit 范围 `c82335c..HEAD`，
> 4 commits：fe61279 间隔哨兵 / c545f87 CLI 排空+重连重构 / 02b1261 判据登记 /
> 58ce2e6 任务收口），外部评审轮 r2（dsh，代码评审门；r1 = 方案评审，记录见
> `docs/reviews/term-attach-resilience-design.md`）。
>
> **评审方式**：`git diff c82335c..HEAD` 全量逐行；与 `c82335c` 的旧 `attach_run` 逐段
> 对照等价性；判据/工件与 `INTEROP-CRITERIA.md`/design/tasks/VERIFICATION 交叉核对；
> 独立实跑门：`cargo test --workspace`（0 failed；`--list` 1066 项 − ignored 36 =
> 1030，与自述一致）、`cargo clippy --workspace --all-targets`（0 警告）、
> `cargo test -p homeway-core --lib term::service::tests::sentinel*`（6/6）、
> `cargo test -p homeway-cli term_cli::tests`（20/20）。e2e 未重跑（需私有出口+daemon）。

## 结论摘要

- **贴合度**：D1–D4 的主要决定逐条落地属实——分档条件、五重验、不变量、真变路径保持
  背靠背、DialErr 三档、`new` 重拨降级、本地面终局分档、标题/client_id 进程级一次、
  退避常量注记、Realign 返回值检查、poll+tcflush 位置与超时、判据五行登记，全部对上。
- **重构等价性**：拆段后 detach / ENDED / 信号 / 查询窗回投 / 标题 / 收尾顺序与旧实现
  逐条一致，文案零漂移（唯一差异 = 非自动命名路径的 `already_exists` 从裸码面改走
  `proto_error_text`，该路径不可达，可忽略）。
- **实质缺口**：① 重连循环的 GREETING 读失败路径**未 close**（r1 3.3 的 EMFILE 面没收
  干净，半死主机形态每次重拨泄漏 3 fd + 1 线程）；② 重连拨号段读 ATTACHED **无预算**，
  且此时信号面已装 ⇒ 可挂死且 SIGTERM/SIGHUP 被吞（D2「拨号自带 --timeout 预算」前提
  被破坏）；③ 判定矩阵真值表测的是生产不构造的输入；④ 3.6/5.1 验证证据不成链。
- **判定**：**有条件通过**——H1/M1 修完（各 1 行级改动）并补 fd 证据后即可收口；
  其余中低项可随批或登记后置。

## 发现清单

| # | 严重度 | 位置 | 问题 | 建议 |
|---|---|---|---|---|
| H1 | 高 | `crates/homeway-cli/src/term_cli.rs:463-466` | `read_greeting` 首读失败/超时路径未 `conn.close()`（同函数 468/473/477 三处都 close）。`TermConn::Remote` 的 `Arc<ControlClient>` 无 Drop、`hw-cctl-reader` 线程持强 Arc（`daemon/client.rs:216-220`），只有 `ControlClient::close()`（`:412-424`）能收口 ⇒ 重连循环每次 GREETING 超时泄漏 3 fd + 1 线程。仓内 `term_cli.rs:216-218` 记录的「服务流能开但一个字节不流」恰走此路 | 首读 `map_err` 前补 `conn.close()`；更彻底 = 给 `TermConn`/`ControlClient` 加 Drop 消灭整类漏网。修后按半死主机形态复测 fd/线程 |
| H1b | 中 | `openspec/changes/term-attach-resilience/tasks.md:27`、`VERIFICATION.md` | 3.6「脚本断言记入变更目录」勾选但无产物（变更目录只有 e2e_term.py/diff_probe.py，VERIFICATION 无 fd 行）。自述「SIGSTOP 出口下 80s fd 恒 11」很可能走 open_stream 失败档（该档确实 close，`term_cli.rs:404-410`），未覆盖 H1 的 GREETING 超时档 | 补可复跑的 fd/线程计数记录，或改回未勾并注明手工证据边界 |
| M1 | 中 | `term_cli.rs:1079-1084`（+1782 重连调用点、`daemon/client.rs:145-158`、`term_cli.rs:1462-1487`） | 重连拨号读 ATTACHED 用无限等待的 `read_frame()`（`recv_wait` 等条件变量）。重连段信号面已装（`AttachEnv::new` 装于首连成功之后），SIGTERM/SIGINT/SIGHUP/SIGQUIT 只写管道、主线程不读 ⇒ 拨号挂死时进程不可正常终止（只能 SIGKILL）。破坏 D2 的「拨号自带 --timeout 预算」前提与 spec「用户主动退出仍然有效」 | ATTACHED 读改 `read_frame_deadline(t.timeout)`（或单列拨号预算），使「一次拨号 ≤ 预算」成为硬约束 |
| M2 | 中 | `term_cli.rs:1569-1607`、`2423-2450`、`1914-1917` | `LinkEvent` 11 档中 7 档（Ended/ErrorFrame/StdoutWriteFail/StdinEof/PollFail/ExitSignal/Detach）生产从不构造（`#[cfg_attr(not(test), allow(dead_code))]` 自证），真值表测的是非生产输入；真实判定散在内联分支（1982/1995-2006/2015-2026/2050-2053/2075-2085），矩阵对其无回归力 | 内联分支也构造事件走同一函数；或删不产出变体，把「本地面链路断→终局」「stdin EOF→终局」等提为可测小函数 |
| M3 | 中 | `tasks.md:35`、`scripts/e2e_term.py:12-16,80,104` | 5.1 标「CI 可跑面」但脚本硬编码 `REPO=/Users/zhaozhe/...`、`/tmp/diff_probe.py`、固定 state/host；`:80` 的 `b'tt1'` 与 `SESS=f"tt{pid}"` 不匹配（预清理不可靠）。VERIFICATION 缺 tasks 承诺的 vim 对照例；B→C 组合段异常未归因 | 脚本参数化（repo 根用 git 探测、探针走仓内路径、SESS 用实际名），补齐 vim 例与 B→C 异常归因 |
| L1 | 低 | `service.rs:1336`（+1253-1257、1338-1343） | `spaced` 分档对 surface 腿同样生效：`wake_surface()` 紧跟第一跳，快照基本**必**落在 300ms 窗内（design 措辞「恰在窗内」低估），窗内 PTY=sentinel 宽而 vt=base 宽，会污染 App 首帧快照（随后自愈）。surface 腿本不需「逼 TUI 重绘」（vt 是快照真源） | 分档加 `&& !surface`，或 surface 腿把哨兵推迟到快照之后；同步 Risks 措辞 |
| L2 | 低 | `service.rs:1985-2001` | 注释「线程起不来 = 锁内兜底立即恢复」与实现不符（实为出锁后调 `sentinel_restore`，重取锁并重跑五重验）；design D1 亦写「锁内」 | 注释/design 改述为「锁外兜底（重跑五重验）」——实现本身更优，只差措辞 |
| L3 | 低 | `term_cli.rs:1763`、`375-380`、`1755`、`1661` | 多余拷贝/借用面：`attached.clone()`（`attach_run_once` 只在开头读，传 `&attached` 即可）；`DialErr::text()` 返回 `String`；`let mut conn = conn;`；`AttachEnv::new` 收 `&mut TermConn` 仅为失败 close | 按「借用优先」收敛，与本批行为无耦合 |
| L5 | 低 | `pty.rs:280` vs `service.rs:436-440` | `PtySession::tty_size` 是 `pub`，实际只有 `#[cfg(test)]` 的 `PtyShared::tty_size` 用 | 降 `pub(crate)` 或 `#[cfg(test)]` |
| L6 | 低 | `crates/homeway-cli/Cargo.toml:28-29` | 新增纯注释「留空占位说明」节（manifest 里解释「无依赖」） | 删除；信息落单测注释或本记录 |
| L7 | 低 | `term_cli.rs:1662` | `detach_key_spec` 失败路径未 `conn.close()`（同函数 1667/1686 都 close）。该路径随即进程退出 ⇒ 实际零泄漏，但与 r1 3.3 立的原则不一致 | 补 close 或给 `TermConn` 加 Drop（一并消 H1） |
| L8 | 低 | `term_cli.rs:1809-1845` / `2056-2085` | 「断链期输入隔离」只覆盖退避窗：拨号窗（≤`--timeout`）内 stdin 无人读，重连后原样注入会话 | 明确口径（spec/design 注记「仅退避窗」）或重连后进主循环前做一次有界丢弃 |
| L9 | 低 | `term_cli.rs:1815-1817` | `wait_during_backoff` 把 poll 的 EINTR（含 SIGWINCH）当 Timeout ⇒ 退避窗可被信号截短；登记项未注记该例外（行为无害：HELLO 自带当前尺寸） | 注释注记，或 EINTR 时重算剩余预算 |
| L10 | 低 | commit 58ce2e6（`docs/matrix-latest.md`） | 生成物噪声（时间戳/IP/speedtest 数字，`C-via` 127.0.0.1→192.168.3.12），非本变更交付物 | 生成物单独 commit 或 commit 信息注明「矩阵重跑」 |

## 保留意见复核（r1 明确不采纳/部分采纳的 3 条）

- **第二跳交周期面执行（不采纳）**：**仍成立**——实现只用一个 300ms 短命线程（`service.rs:1988-1995`），未立周期设施；term 服务无通用定时轮的前提未变。
- **哨兵令牌防叠加（部分采纳）**：**仍成立**——核对同 base 叠加（第二次第一跳同值 TIOCSWINSZ 不产生信号 + 两跳恢复收敛）与异 base 叠加（重验④让位）两条路径，终值均收敛，令牌机制不成比例。
- **TCSAFLUSH 替代（不采纳）**：**仍成立**——`drain_terminal_input` 只作用于退出路径且显式 `TCSANOW` 还原语义未变，面未扩大。

## 看过、未发现问题的方面

1. **D1 分档与五重验**：`spaced_sentinel = reg.size_applied.is_none()` 与 `session.rs:579`「真变才 Some」语义一致；三处真变触发点全 `false`（`service.rs:1418/1471/2097`）；五重验顺序与条件齐（`:2006-2024`）；不变量（第一跳只动 PTY + `sentinel_count += 1`，不写 epoch/不改 `rt.size()`）成立且有反证单测；让位/跨代/幂等/三态 env 五组单测在；`pty.rs` 夹具标注在。
2. **并发/竞态**：延迟线程只走 state→pty 锁序（与既有 `apply_size_locked`/`sentinel_repaint_locked` 同序，无新反转）；所有 `teardown()` 调用点均在 `write_half` 锁外（无自锁死）；poke 管道跨连接复用先 `poll(0)` 再 drain 正确，且旧 reader 在 teardown 中 join 后才复用 fd；`finish` 关 poke 前 reader 必已 join。
3. **teardown 完备性**：`attach_run_once` 15 个 return 点中，除 `conn.split()` 失败（无 reader，无需 teardown）外全部先 teardown 再返回；「停 reader→close→join」序保持原逃生口语义；poll EINTR 分支 `continue`（不误 teardown）。
4. **重构等价性**：与 `c82335c` 旧 `attach_run` 逐段比对——detach/Realign/首键回投/ENDED/ERROR/信号/stdin EOF/未知帧/标题/收尾打印与还原顺序一致；split 失败路径由「就地还原」改经 `env.finish`（等价且更收敛）；RESIZE 写失败从「忽略」改为链路归因（r1 3.4 要求，矩阵已登记）。
5. **文案漂移**：无（`收到信号退出…`/`已分离…`/`写本地终端失败`/`忽略未知帧…` 逐字一致）。
6. **Rust 工程质量**：DialErr/LinkDecision/AttachOutcome/WaitEvent/LinkBackoff 均为类型承担语义；无新增字符串错误；**无逐行移植痕迹**（Go stream 循环形态未照搬）；可见性除 L4/L5 外合规。
7. **判据登记**：5 行齐，日期/条目（含从→到）/原因/影响面齐全，列形与最近先例（v3/v3.1/v4 条）同形。
8. **两处门**：`cargo test --workspace` 实跑 0 failed（非 ignored 1030 与自述一致）；`cargo clippy --workspace --all-targets` 0 警告——自述属实。

## 后续

- H1/M1/H1b 修完 → 更新本记录「整改归宿」列 → 勾 `tasks.md` 0.2。
- L1/L2 建议同批（注释/分档一行级）；L3–L10 可随批或登记后置。


## 整改归宿（r2 意见 → 本批处置，2026-10-16）

| r2 条目 | 归宿 |
|---|---|
| H1 GREETING 读失败档泄漏（高） | **已修**：`read_greeting` 首读失败补 `conn.close()`；GREETING 超时档 fd 复测恒 14（VERIFICATION fd-1）。`TermConn`/`ControlClient` 加 Drop 记为后续项（动 homeway-core 公共面，本批不打） |
| M1 重拨读 ATTACHED 无预算（中） | **已修**：改 `read_frame_deadline(t.timeout)`——「一次拨号 ≤ 预算」成为硬约束，挂死窗消除 |
| M2 真值表只测死变体（中） | **已修**：删 4 个生产不构造变体（Ended/ErrorFrame/ExitSignal/Detach 内联终局），stdout/EOF/poll 三档收编走 `link_failure_classify`（生产构造点齐） |
| M3 证据链（中） | **已修**：脚本 SESS 错配修复 + 参数化（git 探测 repo 根、探针走仓内路径）；vim 对照例补测 PASS；fd 行入 VERIFICATION（fd-1/fd-2/B→C 归因） |
| L1 surface 腿间隔窗（低） | **已修**：分档加 `&& !surface`——surface 快照真源是 vt 不需要逼重绘，回到现状背靠背（保守方向） |
| L2 注释「锁内」实为锁外（低） | **已修**：注释与 design 措辞如实化（锁外兜底、重跑五重验） |
| L3/L5/L6/L7 卫生项（低） | L6 已删 manifest 注释节；L5 `tty_size` 收窄 `#[cfg(test)]`；L7 `detach_key_spec` 失败路径补 close；L3 借用面随 M2 重构顺手收（`attached` 不再 clone 传递为首连/重拨两处赋值） |
| L8 拨号窗输入隔离（低→提级） | **已修**：重拨成功、进主循环前 `drain_terminal_input(0, 0)` 排空断链期积压（spec MUST 面补齐） |
| L9 EINTR 截短退避窗（低） | **已修**：`wait_during_backoff` 按 deadline 循环重算剩余预算 |
| L10 matrix-latest 生成物混入（低） | 登记：收口 commit 已混入无法拆分，本条记入教训（生成物单独 commit） |
| 补1 退避不重置 | **已修**：`LinkBackoff::reset()`，重连成功即复位（单测补档） |
| 补2 同拍「读腿断 vs stdin EOF」次序 | **不采纳代码改、注记入 design**：等待循环已天然兜底（EOF 在重连等待窗 poll 立即命中 → Quit 干净退出；最坏代价 = 一次横幅 + 一次预算内拨号），改判读序的复杂度/风险大于收益 |
| 补3 哨兵线程无 panic 防护 | **已修**：`catch_unwind` 包裹 + panic 兜底再恢复 + 记行 |
| 补4 env 无法表达 gap=0 | **已修**：`HOMEWAY_TERM_SENTINEL_GAP` 解析放行 0（显式退回背靠背的测试/回退档） |
| 编号漏洞 L4 缺失 | 评审自身笔误，不影响结论 |
