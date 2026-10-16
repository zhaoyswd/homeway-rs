# term-attach-resilience — 任务

## 0. 评审门（硬规则 5）

- [x] 0.1 技术评审落档：本方案 dsh r1 评审结论与整改决策整理入 `docs/reviews/term-attach-resilience-design.md`（含三条保留意见的理由：周期设施否决/哨兵令牌否决/TCSAFLUSH 否决）；验证：文件存在且覆盖 r1 全部高/中条目的归宿
- [ ] 0.2 完工代码评审任务（占位，实现完成后执行）：checklist 含 AGENTS.md「逐行移植痕迹」项，记录入 `docs/reviews/term-attach-resilience.md`

## 1. 哨兵间隔（服务端）

- [x] 1.1 `TermConfig` 增 `sentinel_gap: Duration`（默认 300ms，注释钉真源）+ `from_env` 读 `HOMEWAY_TERM_SENTINEL_GAP`；验证：单测三态（未设=默认、设值生效、非法值回退），不在调用点直读 env
- [x] 1.2 attach 路径分档：仅「本次 `reg.size_applied == None`」的 attach 走间隔形态，其余三触发点（输入几何/RESIZE/end_leg 重选举）与带几何应用的 attach 保持现状背靠背；`sentinel_count` 口径不变（一次 attach 恰 +1）；验证：单测——几何未变 attach 走间隔、几何应用 attach 与 RESIZE 路径形态不变、计数 +1 不随分档漂移
- [x] 1.3 延迟第二跳：spawn 线程（`self: &Arc<Self>` 签名涟漪一并落）+ 锁内五重验（在册未 done/gen 一致/未 stopped/`rt.size()==base`/恢复）+ spawn 失败锁内立即 `resize(base)` 兜底 + 「第一跳不写 epoch 不改 rt.size()」不变量注释；验证：单测 ×4——间隔期真实 RESIZE 让位、会话换代不跨代误伤、恢复幂等（base 已在位无副作用）、不变量反证（第一跳后 `rt.size()` 仍 == base）
- [x] 1.4 pty.rs:286 测试夹具标注「仅测试用，勿在生产语义引用」；验证：注释落位、`cargo test -p homeway-core` 绿

## 2. 退出排空（CLI）

- [x] 2.1 `drain_terminal_input(timeout_ms)`：`poll(POLLIN, timeout)` → `tcflush(0, TCIFLUSH)`（不读字节、无形态判定；非 tty 静默忽略）；验证：CLI crate 内 `libc::openpty` 单测 ×3——排队应答被清（master 端注入应答后排空、读回无残留）、空队列带 50ms 超时返回、排队击键同被清（不回显）
- [x] 2.2 接入终局段：位置 = 连接拆除后、`tty.restore()` 前（raw 态下、开回显前）；统一 50ms 适用一切退出路径；验证：既有退出路径测试全绿 + 手工 detach/ENDED/信号退出无可感延迟

## 3. 重连循环（CLI）

- [x] 3.1 `DialErr` 类型化：`attach_dial`/`dial_term` 返回 `enum DialErr { Session(code, msg), Transport(..), Proto(..) }`，`proto_error_text` 只在终局打印调用；验证：单测覆盖三档归类（ERROR 帧 code 面、UDS 拒绝/超时、协议错）
- [x] 3.2 触发矩阵纯函数 `link_failure_classify`：含 ENDED/ERROR/信号/分离键/stdout 写失败/stdin EOF/poll 失败 → Finished；读 Io/Disconnected/StreamEnded/send_data 失败/Realign `send_resize` 失败（补返回值检查）→ LinkLost；**本地面一切链路终结 → Finished**；重拨 Session 级 → Finished、Transport 级 → LinkLost；验证：真值表单测覆盖全部分支（含 stdin EOF 先于读腿 Disconnected 的次序）
- [x] 3.3 `attach_run` 拆连接段/终局段：连接段（reader/poke/写半 + 「先 close 后 join」序保持 + `data_rx` 退场不参与后续 poll）；终局段（信号面/标题/排空/termios/文案）；tty raw、信号管道、dm、**标题查询与 `old_title`、`client_id()`（OnceLock）** 进程级一次；验证：`cargo test -p homeway-cli` 全绿 + 手工 attach→detach/Ctrl-C（信号面）退出不回归
- [x] 3.4 `attach_cmd` 外层循环：`new` 首连成功后重拨清 `CREATE|ONLY_IF_ABSENT`；断链横幅一行；退避常量（0.5s×2 封顶 5s，注释注记与既有三套退避的关系）；**重拨失败路径显式 `client.close()`**；验证：本地双实例（`tools/local-rust-exit.sh` 私有实例）——kill 出口→横幅→出口回→自动恢复（含 `new` 创建型会话同验）
- [x] 3.5 等待期交互：`poll([stdin, signal_pipe])` 喂 dm——分离键退出、孤立 0x03 退出、其余丢弃（含孤儿应答）；验证：单测或手工（二选一注明）——等待期 Ctrl-C 即退、断链期击键不注入恢复后会话
- [x] 3.6 fd/线程稳定验证：长重试（连续失败 ≥100 次）下进程 fd 与线程数不增长（lsof/`proc_info` 计数）；验证：脚本断言记入变更目录

## 4. 判据登记

- [x] 4.1 `docs/INTEROP-CRITERIA.md`「判据变更记录」登记 5 行（哨兵间隔 0ms→300ms 可调〔仅几何未变 attach 面〕/重连退避新增/退出排空新增/重连横幅新行为面立册/断链三态文案产出面收窄）；验证：五行齐「日期/条目/从→到/原因/影响面」五列

## 5. 端到端与门

- [x] 5.1 本地端到端（私有实例，CI 可跑面）：vim + 自造差分渲染探针（只按行重绘的假 TUI 脚本）双例验证 attach 画面完整性；kill 隧道/出口 → 横幅 → 恢复 → 画面/输入恢复；退出后 shell 无乱码（在途应答场景）；验证清单逐项打勾记入变更目录
- [ ] 5.2 真机/用户触点档（不进 CI）：opencode attach 画面完整性、真网络中断（关 Wi-Fi/断 VPN）→ 自动恢复、退出无乱码；清单交用户触点
- [x] 5.3 全量门：`cargo test --workspace` + `cargo clippy --workspace`（0 警告）+ `tools/ci-local.sh` 通过；执行 0.2 完工代码评审
