# WIN 批代码评审记录（dsh r1 + 整改，2026-10-13）

评审对象 = `cbe8c10^..0fc5b19`（WIN-1/2/3 三 commit）；载体/材料入口见
`WIN-design.md`。评审人独立复跑了 windows 交叉 check、linux-gnu 目标 clippy、
musl check 与两例真进程信号测试。结论 **3 高 / 10 中 / 9 低**，全部处置如下。

## r1 意见与处置表

| # | 级 | 摘要 | 处置 | 验证 |
|---|---|---|---|---|
| H1 | 高 | `stopsig::on_stop` 用 `sysock::send` 写 unix 管道 → ENOTSOCK 被吞，serve/统一进程收不到 SIGTERM（脚本「TERM→等→KILL」回退掩盖成全绿） | `on_stop` 改走平台分派 `write1`（unix=write(2)）；注记钉死 | 真进程实测：前台 serve SIGTERM→**0.0s** 优雅收工（`出口收线…35µs` 行在）；统一进程见 H2 行 |
| H2 | 高 | 共享 `ARMED` + relay 无条件 `disarm_and_restore` → 统一进程 relay 收线把宿主 handler 打回 SIG_DFL | `RelayProc` 增 `armed_here` 属主位；只在本面曾 arm 时 disarm+restore；**未 arm 时完全不动共享槽**（第一版 else 写 DISARMED 又弄瞎宿主——实测统一进程挂死复现后删掉 else） | 统一进程 `relay start→stop→SIGTERM` 全链 PASS（`homeway: 收到信号，收工` 判据行原文） |
| H3 | 高 | egress linux 臂丢 `let _ = index` → linux CI clippy 必红（mac 本地盲） | 恢复；linux-gnu 目标的 clippy 面登记进 §4-8（本地复验配方） | 本地 `cargo clippy --target x86_64-unknown-linux-gnu -p homeway-core -- -D warnings` 过 |
| M1 | 中 | portfwd 双 cfg 死臂（`not(windows)`+doc+`windows` 同置一 item，两端都不编译） | 删误加行；复核后**windows pf_bind 臂也删**（运行时半边整体 not(windows)，无调用点——设计 §4-6 改「未接线」 | windows check 零警告（死码清除） |
| M2 | 中 | windows 上 files/speedtest 本地监听 bind 后即丢（端口文件残留/就绪行误导）+ term 块内死 `own=(0,0)` 臂 | 本地调试腿整体 unix 化；windows 走 `service_intake(cap,...)`（QUIC-only）+ windows 形态就绪行；死臂清 | windows check；就绪行形如 `（STREAM tag=1…；本机调试腿在 windows 不装配）` |
| M3 | 中 | windows 门 warning-blind（42 条无人看，含死码） | 全量清零（imports/常量/助手随消费半边门控；TunExecutor 族门控；sysio 死助手删）+ win-check 上 `RUSTFLAGS: -D warnings` | 三 crate 交叉 check **零警告** |
| M4 | 中 | windows 无 daemon 级优雅停止（控制面只有 serve.\*/relay.\*；taskkill 硬杀） | **登记**（README/WIN-design §4-9）：`daemon.stop` 控制面动词 = 后续批；README 已明写「无优雅停止路径，taskkill /f 形态」 | 文档 |
| M5 | 中 | `PollEnt` 缺 `#[repr(C)]`（靠字段巧合布局） | repr(C) + size/align/offset 编译期断言（core+quic 两份） | 编译期断言在位 |
| M6 | 中 | quic `sysio` 复制 ≈250 行（设计称 ≤50）且 `send` 语义分叉 | send 对齐（unix 补 MSG_NOSIGNAL）；设计 §4-10 改按实登记（抽第三叶子 crate = 后续批） | 代码 + 文档 |
| M7 | 中 | relay poll 错误路径：EINTR 新记行 + 真错误双记 + 重读 errno（unix 行为回归） | Err 臂恢复 `kind != Interrupted` 抑制 + 单次记行；删遗留块 | unix 全量测试绿 |
| M8 | 中 | systime 丢 `#[allow(deprecated)]`（musl/OHOS 交叉 check 噪声回归） | allow+原注释随迁 | musl check 无警告 |
| M9 | 中 | quic `pair()` 缺 nodelay；每服务流一对 loopback TCP 的成本未登记 | 补 `set_nodelay(true)`；TIME_WAIT/端口耗尽面登记 §3 风险表 | 代码 + 文档 |
| M10 | 中 | pump 头注「AF_UNIX 无 RST/错误码不跨 socketpair」在 windows 臂为假 | 注释改口径 + §4-11 登记（close 带未读数据 ⇒ RST；错误码面差异） | 文档 |
| L1 | 低 | 四处 unix 失败路径可见差（timestamp 回退形态/TUN 日志细节/V6ONLY 上抛） | bind_v6_only 恢复忽略语义；其余三处登记为「失败路径改善」（§4-12） | 代码 + 文档 |
| L2 | 低 | `LockFileEx(0,0)` 区段口径存疑 | 改 `1,0`（锁字节 0——互斥最小共识面）；真机两进程互斥断言 = 后续批 | 代码 |
| L3 | 低 | 适配器缓冲对齐；IP_UNICAST_IF 字节序未真机钉 | 对齐维持（实践对齐面）+ 登记真机清单 | 文档 |
| L4 | 低 | `pub mod sysio` 暴露面过宽 + 死助手 + 多余 cast | `mod sysio` + `pub use SvcStream`；死助手删；cast 清 | windows check 零警告 |
| L5 | 低 | `-11` 哨兵 / quic_only 手抄 / 平台分支泄漏 | 前两件重构（四态直排 + `build()` 收口）；`from_std` 两臂维持（登记） | 代码 |
| L6 | 低 | 注释/文案漂移 + windows 帮助列 term | 逐处订正；帮助行平台分档 | unix check 零警告 |
| L7 | 低 | 设计文档失真（pf_bind 完成面/bridge_auth 未落地/≤50 行/§4-3 stopsig） | §4 全面按实订正（见下） | 文档 |
| L8 | 低 | 脚本 pattern 放宽误配 homeway-go；stop 无优雅断言；attach_size 残留进程毒化 | `our_pid` 收紧为精确/词首匹配；`stop_pid` 大声记账再 KILL 兜底 | 脚本语法过 + 本次收口全链重验 |
| L9 | 低 | stopsig windows 注释口径（事件集/TRUE 语义/arm(DISARMED) 共享槽） | 注释订正；`arm(DISARMED)` 面随 H2 属主化一并消失 | 代码 |

评审人复核确认面（「看过没发现问题」的独立背书）：sysock/sysio unix 臂逐调用
同形（intercept 三态/egress 三平台臂/engine poll/ctlface/锁语义/files accept
重写等价）；windows 臂 sockopt 常量与宽度/`WSAGetLastError` 纪律/GetAdaptersAddresses
联合取值/Hinnant civil 算法/60s 刷新节律/句柄收窄；判据行零改动（评审人做了
全 diff 字面量归一比对 + 判据关键词扫描，无命中）。

## 待真机/后续批清单（评审人列 + 处置）

1. `IP_UNICAST_IF/IPV6_UNICAST_IF` 网络序口径真机钉（错 = 钉卡失败 + 告警，不静默）。
2. `LockFileEx(1,0)` 两进程互斥真机断言。
3. **WSAPoll 对失败 connect 不报 POLLERR/POLLHUP 的社区报告**——reactor 已有 10s
   `dial_deadline` 兜底（最坏 = 失败拨号延迟到死线）；真机断言一次。
4. 每服务流 loopback TCP 的 TIME_WAIT/动态端口耗尽量级（高流速率场景压测）。
5. windows `cargo test`/`clippy --all-targets`（用例面 unix 化）。
6. `daemon.stop` 控制面动词（M4）。
7. sysio/sysock 抽第三叶子 crate（M6 记载）。
