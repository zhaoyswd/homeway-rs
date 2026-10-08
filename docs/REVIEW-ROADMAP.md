# homeway-rs 审计整改 Roadmap（Q 批 · 跨批进度真源）

> **立项**：2026-10-07 全面评审（11 个模块级子代理纵深审查 + 主会话对 P0/P1 逐条回源码复核）。
> 发现清单单一真源 = `docs/reviews/AUDIT-2026-10-07.md`（条目带 ✅=已复核 / 🔎=批次启动时须复验）。
> **用户拍板（2026-10-07）**：每批两道评审门（**设计评审 + 代码评审**，均走 `reviewer` skill /
> dsh 外部评审）；**全流程（含评审）由子代理执行，主会话只做进度管理**（读指针→派发→收摘要→
> 更新本表→收尾核对）。**开工节奏**：Q-A 立即执行（无评审）；**Q-B…Q-J 等用户指令逐批开工**。
>
> **部署边界**：批内只动本仓代码与文档并提交（main 直推按 AGENTS）；**两台生产出口滚动升级、
> tier pin 前进属用户触点**，等指令。不建 tag / 不发 Release / 不建 PR。

## 状态总览（手维护）

| 批 | 内容 | 状态（更新时间） | 设计门 | 代码门 | 记录 |
|---|---|---|---|---|---|
| **Q-A** | 文档与治理对齐（**无评审**） | **完成**（2026-10-07，提交 `5d47087` + `37b4fe5`）——PERF-AB 删除批注记（pacing/MTU/SENDTHREAD；§10 推荐键改现存面）+ AGENTS 退役事实修订（Go 退役/R7 完成/smoltcp 0.14/openspec 指向/锚点/硬规则 2·4 重述）+ BASELINE 冻结声明 + make-baseline 锚点对齐 d4148f6 + INTEROP-CRITERIA 出处修正与「判据变更记录」政策（含 Q-B 两条预登记占位）+ ROADMAP 去重（R2 段）+ 「下一步（当前指针）」节 + 隔离条款状态注记 + R8 既成事实注记 + GAP-AUDIT 账实修正（P0-1 余项 OPEN→Q-H / P1-3 / P1-6）+ ROADMAP P1 7/8 修正 + fixtures SUMS 全量重生成（45/52→59 项）+ MANIFEST 口径 + README 建仓首页 + workspace version 0.1.0→0.2.3 + cli 描述/unified_cli 头注/ci-local 头注口径 | — | — | 本表 |
| **Q-B** | 拦截层与出口数据面加固（P0-1/P0-5 等） | **完成**（2026-10-07，提交 `9e37445`〔实现 F1–F10 + 单测〕+ `0b37246`〔观测接线 + 判据登记〕+ `8a51c2a`〔代码门记录 + 状态回填〕；`cargo test --workspace` 452 passed / 1 已知 flake〔`wgcore::stackb` 墙钟断言，主会话复跑 5/5 绿〕；`cargo clippy --all-targets -D warnings` clean）——F1 VecDequeLite 死前缀压缩 + 水位门移进读循环 / F2 DNS 待答表丢弃回执回收 + 空帧收线（循环外）/ F3 UDP 上行门 + `out_udp` 双上限 + 回投失败计数 / F4 `tx_deferred` 字节上限（非 TCP 丢新）/ F5 `udp_seq_of` 赋值 + `decr_flow` 配对守卫 / F6 DNS 腿会话内每包应答 / F7 IPv4 分片丢弃 + 计数 / F8 DNS-over-TCP per-conn 续写 + 硬上限 / F9 非 TCP 不建会话 / F10 `deliver_udp53` 失败计数；P2 性能面移交 Q-I | **通过**（2026-10-07，dsh `r1.jhb9HE` exit=0；27 条意见→21 认同改设计/0 不认同；三处结构性修订 A1〔空帧收线时序〕/A2〔F1 有界性订正+门移进循环〕/B2〔F8 改 per-conn 续写〕已并入） | **通过**（2026-10-07，dsh `r2.jl9GH2` exit=0；11 条意见→9 认同改/1 部分认同/0 不认同；高危 H1〔F8 待写队列无上限〕已双管齐下修〔硬上限 + 读侧软背压〕；M1 status 观测接线/M2 F6 测试/M3 门阈值/L1-L4 已改） | `docs/reviews/QB.md`（实现+代码门）+ `docs/reviews/QB-design.md`（设计） |
| **Q-C** | 入口安全与资源上限（P0-2 等：wtransport/中继/设备表） | **完成**（2026-10-07，提交 `07e32ca`〔实现 F1–F4 + F11 bind 面〕+ `7f81285`〔实现 F5–F8/F10 中继面〕+ `50ababa`〔实现 F9/F11/F12〕+ `181923e`〔判据登记 + 批记录 + 状态回填〕+ `072e8e2`〔daemon flake 补记〕+ `7ed957f`〔F1 E2E 口径补记〕；`cargo test --workspace` 473 passed / 1 已知 flake〔`wgcore::stackb` 墙钟断言，基线 HEAD `git stash` 复现同样红 42Mbps〕；`cargo clippy --all-targets -D warnings` clean）——F1 设备表淘汰产 `Remove`（先选后落/排除视图，ops=[Remove,Add] 与 Go 同序）/ F2 仅 Data 帧采纳 + hint 卫兵 / F3 未采纳期 2s reg 补投（每次未采纳期 ≤15 次/≤60s，rearm 归零）/ F4 端点缓存 cap 64 + 尾部淘汰 + 投喂配额 / F5 中继腿表 `pending` 分表（无状态 cookie 版经复验不可实现——PROOF wire 无 pubkey；用户改裁备选）/ F6 拒绝日志节流（enum 桶）/ F7 v1 源绑定 `a.backend` + 全局 assoc 上限 1024 + 未认证不 bump + 下行字节桶 16MiB/s / F8 转发只计成功 + 双栈 assoc / F9 probe 字节截断 + 中继 build 注入 / F10 中继 flags bit5 降级位 / F11 `same_candidates` 共享 + `tunnel_addr` 撞车守卫 + `direct_first` None 语义 + frame 断言 / F12 `if_nametoindex` 最小守卫；P2 性能面移交 Q-I；`mark_failed` 按设计门 Q13 剔除（无调用点） | **通过**（2026-10-07，dsh `r3.e5GAso` exit=0；约 47 条意见→44 认同/3 部分认同/0 不认同；三阻塞项〔F1 删 B 版、F2 判据收紧、F5 改方案〕已并入 v2） | **通过**（2026-10-07，dsh `r4.tywkTq` exit=0；M1–M4 + L1–L8 + 2 补充观察；**无高危**；已改 M2〔load/merge 真断言〕/M3①③〔rearm 归零文案 + 窗口测试〕/M4〔登记补 pend 入队计数〕/L4〔hw-app 双向研磨〕/L5〔节拍参数注入免墙钟〕/L7〔RejectLog enum〕；记录/给证据 M1① ②/L1/L2/L3/L6/L8/补充 1·2） | `docs/reviews/QC.md`（实现+代码门）+ `docs/reviews/QC-design.md`（设计） |
| **Q-D** | 终端子系统加固（P0-3 等） | **完成**（2026-10-08，提交 `1b12191`〔实现〕+ `17afbf3`〔判据登记〕+ `77ccd37`〔批记录〕；主会话独立复跑 `cargo test --workspace` 548 passed / 0 failed；`cargo clippy --workspace --all-targets -D warnings` clean；`term::` 面 142 passed〔基线 111，+31〕；fuzz 回归轨 13 目标 ×100k 全绿 + 深挖轨三 term 目标实跑无 crash）——F1a `Size` newtype + 1000×500 上限（入径归一 + `apply_size_locked` 双门 + vt 硬拒）/ F1b `RESIZE 0×0` 忽略上报保几何（对齐 Go 门后写点）/ F1c 快照镜像 32MiB 行数预算 / F2 `rows()`-`rows_and_commit()` 拆分（指纹只随下发推进）/ F3 nudge 入 PTY 注入队列（锁外写）/ F4 `stalled_snapshot()` 实时停滞接线 + 删死字段 / F5 symbol UTF-8 边界截 127B / F6 `clip_dropped`/`nudge_dropped` 观测面 / F7 逐线程 `catch_unwind` + 毒锁恢复统一 / F8 `plain_text` 备用屏 + 流式重写 / F9 单次 `damage()` 去 `unreachable!` / F10 `gunzip` 64MiB 上限 + 维度守卫 / F11 3 个 term fuzz 目标。**P0-3 复验比审计更严重**（`65535×65535` ⇒ 192 GiB 分配 ⇒ abort，`catch_unwind` 无效）；**剔除 2 条误报**；**新增 2 项**（`0×0` 几何污染、`abs_rows` 回滚行 `wraps` 缺口） | **通过**（2026-10-08，dsh `r7.loQFwv` exit=0；30 条意见〔0 高/5 中/21 低/4 记录〕→25 认同/1 部分认同/0 不认同；评审独立复跑两件实测、抽查 ~40 处行号全中） | **通过（附偏差标注）**（2026-10-08，dsh `r8.kxs9ia`；12 条〔1 高/4 中/7 低〕+ 外层补充 4 条；无高危必改〔H1 机制复核降为低：锁内 panic 最坏 = 一拍脏数据，自愈链已核〕；已改 M2/M3/A1/A2/A3/L1/L3/L7，M4/L2/L4 判误报给证据，M1/L5/L6 处置在案。**偏差**：本轮 nohup 启动只捕获 wrapper `exit=0`，dsh 自身退出码未落盘，按「output.md 完整 + 进程正常退出」认定，已在 `QD.md` §4 如实标注） | `docs/reviews/QD.md`（实现+代码门）+ `docs/reviews/QD-design.md`（设计） |
| **Q-E** | 出口服务修复（P0-4 等：files/DNS/UPnP/speedtest） | **完成**（2026-10-08，提交 `f48a30e`〔实现 F1–F10〕+ `243e731`〔判据登记 + 设计实现注记〕+ `f58eda6`〔批记录〕；主会话独立复跑 `cargo test --workspace` **590 passed / 0 failed**（两轮，首轮 2 条时序敏感测试红——隔离 3/3 绿已甄别并登记进「已知 flake」表）；`cargo clippy --workspace --all-targets -D warnings` clean）——F1 `rel_path` 改 `resolve_in_root` 逐分量 walk（P0-4，链深上限 8 = Go `rootMaxSymlinks`）/ F2 客户端响应行去 64KB 误移植门 / F3a 上传磁盘水位（1 GiB 保留，`op_failed`）+ F3b 服务端请求行上限对齐 Go（文案「请求行超过 65536 字节」）/ F4 DNS per-attempt 绝对期限 + worker 2→64 + TCP 腿拨号期限 + 回投 rx/tx 256 / F5 accept 错误分类退避 + RAII 名额回滚 + 共享可测 accept 循环 / F6 speedtest `Limits` 注入 + 硬超时绝对化 + `release(id)` 收口 + busy 绝对窗 / F7 UPnP 分调用体积闸（1 MiB/64 KiB）+ Content-Length 一致 + 期限 / F8 SSDP 来源过滤 + 非空 LOCATION / F9 轮级先加后删 + `allow_evict` 所有权门 + 枚举一次 + 候选去重 / F10 缩租期限穿透 SSDP + 全局预算。**8 项新增发现**；**剔除/改判 2 处**（DNS「每查询新 socket」非缺陷、「上传无上限」Go 同形属加固超出）。**D1–D6 六项自裁决策**（水位优先/worker=64/`allow_evict`=verify&&Ours/响应行不设界/不做 openat2/不做 socket unlink）；**M2 二选一裁定：维持 D3 门**（漂移/真空窗已如实登记，不重开安全语义） | **通过**（2026-10-08，dsh `r9.0mrmgB` **exit=0**〔前台捕获，已修 Q-D 偏差〕；30 条〔3 高/10 中/17 低〕**全部认同并入 v3**；评审揪出 3 条阻塞：S1 悬空符号链接分支、G1「Go 无服务端请求行上限」定性错误（Go 实有 `MaxRequestLine`+`readLineLimited`）、R1 期限改错层） | **通过（含一轮高危整改 + 复核轮）**（2026-10-08，dsh `r10.QzJd2P` exit=0：**2 高**/5 中/19 低——**H1 = 沙箱 `..` 逃逸**〔评审用真实 crate 复现根外落文件〕、**H2 = busy 帧吞非绝对期限**，两高危**必改已改**并附修前红证据；M1/M3/M4/M5 改码，M2 按「不推翻已定 D3」= 改登记口径 + 改文案。复校轮 dsh `r11.TXvvs6` exit=0：六条点名项逐条成立、**无新高危/中危**，结论「本次处置足以过代码门」） | `docs/reviews/QE.md`（实现+代码门）+ `docs/reviews/QE-design.md`（设计） |
| **Q-F** | 客户端核与桥（portfwd 诚实性/状态分裂/预算/锁纪律） | **实现棒已交付**（2026-10-08，见 `docs/reviews/QF.md`：F1–F8 + N1–N7、判据登记、代码门 r14；**状态由主会话核对后回填**）。**portfwd 实装监听器挂账 = 独立批「Q-F-B」**（设计 §7-1 的「本批必办登记」；tier `openspec/specs/port-forwarding` 的 SHALL 在 B 落地前处于**已知不达标**——交接块与实现轮廓见 `docs/reviews/QF.md`「portfwd-B 交接」） | — | — | `docs/reviews/QF.md`（实现+代码门）+ `QF-design.md`（设计） |
| **Q-G** | 进程卫生与 fd（CLOEXEC/poll revents/STOP_PIPE/权限） | 等指令 | — | — | — |
| **Q-H** | CLI 与 daemon 控制面（config 双表/--state 形态/daemon 上限） | 等指令 | — | — | — |
| **Q-I** | 性能细节批（memset/env/缓冲复用/DNS 缓存）——**2026-10-08 拆前段/尾段并提前**（前段＝device/wgcore/intercept/relay/facade/udpcap 即时做；尾段＝DNS TTL/files 挂 Q-E 之后） | **前段完成**（2026-10-08，提交 `74969fa`〔实现 F1–F7 + 9 条单测〕+ `e28bf11`〔判据登记〕+ 批记录提交 `d826c3d`〔QI.md/PERF-AB §9.18/AUDIT 勾选/状态回填〕；`cargo test --workspace` 482 passed / 0 failed〔首轮 1 条已知 flake `daemon::server_bad_frame`，隔离 3/3 绿〕；`cargo clippy --all-targets -- -D warnings` clean）——F1 `tx_backlog` 换 `VecDequeLite`（叶帧 18.03%→0.38%）/ F2 删两处 65KB `clear+resize`（2.70%→0）/ F3 拦截层读缓冲收敛（1.49%→0.28%）/ F4 `envflag` OnceLock 缓存 6 站点（0.93%→0）/ F5-1 `resolve_udp` 缓冲复用 / F6 relay scratch 编帧 + assoc 借用 + 孤儿扫描 O(L+A) / F7 udpcap 三态（Timeout 不再多睡）。**性能判决：F1–F4 目标叶帧全部达标；总判据②（同吞吐 CPU 相对下降 ≥5%）未达（−0.6%，s/GB +0.5%）——按设计 §4.3 证伪条款记「叶帧消失、收益落在带内噪声」，不宣称 CPU 收益**（机制：驱动循环每拍固定税随拍数放大；`docs/PERF-AB.md` §9.18）。**尾段**（DNS TTL 缓存 / files 拷贝 / socket 缓冲）挂 Q-E 之后 | **通过**（2026-10-08，dsh `r5.SjItld` exit=0；14 主项 + 8 行号订正 + 4 复核 → 认同 21 / 部分认同 1 / 不认同 0；F5-3 降级不做〔高危〕/ F8 整条不做 / F1 摊还常数 ≤2B/B 与容量口径订正 / CPU 仪器改累计 `time=` / iovec 漏项登记，全部并入 v2） | **通过**（2026-10-08，dsh `r6.TJ3TeI` exit=0；**0 高 / 4 中 / 7 低**；M1 判据② 未达须按 §4.3 记「< 噪声」+ M2 头条改用同窗交替中位 + M3 `dnsface.rs:182` 同类残留登记为下一批第一靶点 + M4 补跑中继/en0 臂——**四项已办**；L1 `debug_assert` / L3 值传参 / L7 去内嵌行号已改；L2/L4/L5/L6 登记） | `docs/reviews/QI.md`（实现+代码门）+ `docs/reviews/QI-design.md`（设计） |
| **Q-J** | 通用性批（keyenc 平台/DNS 上游配置化/UPnP 协议面） | 等指令 | — | — | — |

## 依赖与顺序建议

- **同文件面串行**：Q-B、Q-C、Q-G 同触 `server/intercept/**`、`wtransport/**`、`udp*` 面 → 按序做；
  Q-J 依赖 Q-D（HELLO caps 协商）与 Q-E（DNS/UPnP）。
- **建议顺序（2026-10-08 重排；用户授权「按建议定序、连续推进、不必逐批问」）**：
  Q-A → Q-B → Q-C → **Q-I 前段** → Q-D → Q-E → **Q-I 尾** → Q-F → Q-G → Q-H → Q-J。
  - **重排理由**：2026-10-08 实测暴露性能面（Mac 出口 30–50% CPU @ <10MB/s；全管线 ≈38µs/包，
    而 lo0 消融最佳臂仅 12.6µs/包、组件天花板 boringtun ~1µs / smoltcp ~0.05µs；`sample` 热点
    前两名 = `flush_backlog` 的 memmove 与 `sendto`）⇒ Q-I 从「排最后」提到 Q-D 之前。
    原顾虑「Q-I 触 Q-B/Q-E 刚修过的热路径」中 **Q-B 已完成**，只剩 Q-E 面重叠。
  - **Q-I 拆两段**：**前段**（即时做）只做不与 Q-E 撞面的热路径项——`server/device.rs`、
    `wgcore/**`、`server/intercept/**`、`relay/**`、`facade/**`、`udpcap`；**尾段**（Q-E 之后）
    做 DNS TTL 缓存 + files 拷贝——避免与 Q-E 的 `dnsproxy.rs`/`files*` 改动互相返工。
  - **PERF-AB 复测纪律（本次排查再次实证）**：本机负载对吞吐/CPU 判决影响极大（同二进制
    load≈4 → 203–341Mbps；load≈30 → 9–85Mbps）⇒ 性能判决必须在**安静环境**跑，报数带 loadavg 表头。
- **pin**：tier `tools/tailcat/homeway-rs.pin` 落后本仓 HEAD 属正常（出包时前进）；Q 批不主动动 pin。

## 每批执行协议（子代理按此跑，主会话按此核对）

1. **复验**：读本批 AUDIT 条目 + 回源码重定位（行号会漂）；🔎 项须先复核真伪，**误报就地剔除并
   在报告里记录**（不硬做审计条目）。
2. **设计门**：产出 `docs/reviews/Q<X>-design.md`（修复清单/方案/风险/测试计划/**判据行影响**）→
   调 `reviewer` skill 走 dsh 外部评审（prompt 指路 = 设计文档 + 代码路径，不喂结论；结果落
   `/tmp/dsh-review/r<N>.XXXXXX/`，见该 skill 的固定姿势）→ 逐条处置（认同改 / 不认同给证据）→
   过门记录（评审原文摘要 + 处置表）写入设计文档。
3. **实现**：按设计实施 + 新增/修改测试；`cargo test --workspace` 全绿 + `cargo clippy --all-targets
   -D warnings` 无新告警；涉判据行变更的**必须**同步 `docs/INTEROP-CRITERIA.md` 变更附录
   （Q-A 起的政策，见该文件「判据变更记录」节）。
4. **代码门**：调 `reviewer` skill 对 commit 范围做第二轮 dsh 评审 → **高危必改或显式豁免登记** →
   记录 `docs/reviews/Q<X>.md`（两轮意见原文摘要 + 逐条处置表 + 测试/判据证据）。
5. **收口**：更新本表状态 + `AUDIT-2026-10-07.md` 勾选 + 中文 commit（每工作单元一 commit）；
   主会话核对「评审记录在 + 测试证据在」后勾选。push 遵循 AGENTS（main 直推）。
6. **失败/升级**：dsh 不可用 → 结构化双角色自评并注明；复验发现条目与代码现状不符 → 报告
   主会话裁决。

### 已知 flake 登记（高负载/并发下会红；判回归前先隔离复跑 + 看 loadavg）

| 测试 | 形态 | 甄别口径 |
|---|---|---|
| `wgcore::stackb::tests::stack_to_stack_tcp_transfer_fills_window` | 墙钟吞吐断言（`mbps > 100`） | 全量并发跑常红、隔离跑恒绿；基线 `git stash` 亦复现 |
| `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect` | 读日志行时序 | 同一二进制重复单跑红绿翻转 |
| `speedtest_server::tests::serve_send_end_to_end`（2026-10-08 Q-E 复跑观察） | 50ms 预热/200ms 窗口内按精确字节数断言 | 隔离 3/3 绿、全量并发跑可红（窗口内跑不完 5 次迭代） |
| `term::service::tests::attach_size_applies_to_pty`（2026-10-08 复跑观察） | PTY 时序（0.87s 隔离） | 隔离恒绿、并发/高负载下超时红 |
| **同一工作树并发跑两个 `cargo test`**（2026-10-08 Q-F 复跑观察） | `daemon::carriers::forward::{add_roundtrip_remove_and_rebuild, per_host_cap_cascade_and_corrupt_file}` 用固定端口（19990/20001）互撞 ⇒ `EADDRINUSE` 红 | 串行/隔离恒绿；两个实例同时跑必红——**不是回归**，判回归前先确认没有第二个 `cargo test` 在跑 |

> 判据：**隔离复跑绿 + 与改动面无交集 + 基线可复现** ⇒ 判 flake，不算回归；三者缺一就不许当 flake
> 放过（Q-E 批 `serve_send_end_to_end` 即按此口径甄别：隔离 3/3 绿 + 全量两轮 590 passed/0 failed）。

## 各批范围（条目细节以 AUDIT-2026-10-07.md 为准）

### Q-A 文档与治理对齐（无评审；即时执行）
- PERF-AB 删除批注记（pacing/MTU 机器已删，§10 推荐键改现存面）；
- AGENTS 退役事实修订（smoltcp 0.14、R7 前条款、openspec 指向、锚点、硬规则 2/4 重述）；
- BASELINE 冻结声明 + `tools/make-baseline.sh` 锚点修正；
- INTEROP-CRITERIA 出处修正 + **判据变更政策**（版本化附录；“字节级冻结”解冻通道）；
- ROADMAP 去重（R2 段）+ 缺失的「下一步（当前指针）」节 + 隔离条款状态注记 + 审计程序指针；
- GAP-AUDIT 账实修正（P0-1 余项 OPEN / P1-3 / P1-6 行）；
- fixtures SUMS 补全 + MANIFEST 口径；README 建仓首页；workspace version 0.1.0→0.2.3；
  CLI 描述与 `unified_cli.rs` 头注释过期面；ci-local 头注释口径。

### Q-B 拦截层与出口数据面加固
- **P0-1** `VecDequeLite` 死前缀无界（压缩/换 `VecDeque<u8>` + `out_tcp` 字节上限）；
- **P0-5** DNS 待答表丢弃路径泄漏（回执回收或容量+TTL；`mlen==0` 收线）；
- UDP 上行门接线 + `out_udp` 上限 + `udp_send_to_client` 失败计数；
- `tx_deferred` 字节上限（无窗流丢新、TCP 不丢）；
- `udp_seq_of` 赋值 + `decr_flow` 下溢（**判据行变更**，走新政策登记）；
- DNS 腿会话内重传应答（`service_sockets` 读到客户端包走 `submit_leg`）；
- IPv4 分片识别丢弃+计数；DNS-over-TCP 写回原子化（拼帧单写 + 短写收口 + 长度钳制）。

### Q-C 入口安全与资源上限
- **P0-2** 设备表淘汰发 `Remove`（表-设备一致单测）；
- 路径采纳时序（帧解码/来源校验先于 `adopt`）；
- reg 未采纳期补投（首包丢不再等分钟级阶梯）；
- 端点缓存/候选/`race_seen`/`send_err_log_at` 上界 + `mark_failed` 降权 + 探测注入配额；
- 中继：腿表挑战/接纳分表或 cookie 前置、拒绝日志限流、v1 fallback 源绑定、assoc 预算、
  未认证不 bump 缓冲、200pps 上限入状态面；
- `probe::respond_ex` build 串 char-boundary 截断；`relay` build 注入；
- `same_candidates` 多重集、`tunnel_addr` 撞车守卫、`direct_first` 哨兵、`frame` 长度域断言。

### Q-D 终端子系统加固
- **P0-3** RESIZE 夹取（解析边界 + `apply_size_locked` 双门 + 回归测试）；
- vt 指纹提交面拆分（`screen_text` 不提交 `flushed`）；
- `focus_nudge` 出锁写；`stalled_ms` 接线 `LegOut::stalled_for()` + surface 超时置位；
- symbol >127B 字素簇截断（编码前收口）；`title_stale` 时效语义 + 早退清 `last_scan_proc`；
- `resp_dropped`/`clip_dropped` 观测面；term 线程 `catch_unwind` + 毒锁恢复统一；
- `plain_text` 备用屏路径；`gunzip_bytes` 输出上限 + term/cellcodec/keyenc fuzz 目标；
- `Loader::reload` 接线或删（二选一，设计门定）；`unreachable!` 降级。

### Q-E 出口服务修复
- **P0-4** files 沙箱祖先复核（不存在的叶子不得直返 candidate + 负例测试）；
- files 客户端响应行上限拆分（对齐 `files_op` 的 `read_line_capped(false)` 口径）；
- DNS 上游绝对期限（deadline 重设/非阻塞）+ worker 数/背压评估；
- files/speedtest accept 错误分类（EMFILE 退避不退出）+ spawn 名额回滚；
- UPnP：`read_to_end` 上限 + Content-Length/滴流防护、先加后删（718 回退）、枚举一次缓存、
  SSDP 源过滤；speedtest 硬超时收敛 + `release(id)` 按会话收口；
- files 上传配额/磁盘水位（设计门评估口径）。

### Q-F 客户端核与桥
- portfwd：实装监听器或恢复诚实语义（不报假 listening），`targetPort=0` 文案；
- 服务会话暖机硬失败状态机分裂（Ok but failed ⇒ 不发布 Ready + 可重建）；
- `healing_dial` 预算纳入阶梯等待（或 Wait 带 deadline）；`dial_port` 锁出拨号；
- `tun_attach`/`tun_stop` 异步壳（对齐 TunRecover 先例）；
- session 域锁纪律统一 `lock_unpoison`（Drop 路径禁 panic）；`ServiceStop` 三 join 有界；
- spawn 失败落判据行；`Secret` zeroize；巡检失败真实归因；状态快照分层缓存（可留 Q-I）。

### Q-G 进程卫生与 fd
- 全仓自建 fd 补 `FD_CLOEXEC`（拦截层 + `udpbatch` + 扫描确认全部 socket 创建点）；
- `wgcore::poll_fd` revents 处理（HUP/ERR → `TunFdDead` / 立即返错）；
- relay 唤醒 pipe 生命周期（去 `OnceLock` 单例，stop/start 可重建；裸 fd 写守卫）；
- daemon 权限创建即 0600、`sun_path` 平台上限、fd 复用写防护落地。

### Q-H CLI 与 daemon 控制面
- config 双表收敛（统一进程启动期严格 schema 试装配；`assemble` 改 `Result`，`process::exit`
  只留前台层）；**非法配置不再打崩统一进程**（含实测回归）；
- **（Q-A 批遗留登记）** `nodestate.rs:257` 默认 config 模板注释把键写成 `burst_kib`，实际
  `TxShapeCfg` 字段 = `burst_kb`（`intercept/mod.rs:363`，`deny_unknown_fields`）——用户照模板
  填键会拒启；随本批 config 面一并修正；
- `--state` 形态族（`--state=` 等号/缺值/空值全量 fail-fast；`serve token` 支线补齐）；
- daemon SOCKS `dead` 落账 + `socks on` 重建;hosts 会话重建环;连接/握手上限;
- `short_host` char-boundary；数字回退 fail-fast；`serve --help` 拦截；幂等口径统一；
- N1/L7：前台 serve/relay 默认 state 与统一进程对齐（GAP-AUDIT OPEN 项）；
- **（待用户裁决）** GAP-AUDIT P1-4「客户端『出口能力』打行」代码面无实现（服务端 caps 位在、
  客户端 Session 侧无消费）——做（客户端消费 caps 并打行）或标注不做，二选一由用户拍板。

### Q-I 性能细节批（**2026-10-08 拆前段/尾段**；前段即时做，尾段挂 Q-E 之后）

**前段（不与 Q-E 撞面）**：
- `Device::consume_step` 65KB memset 消除；`encap_peer` 每包 `to_vec`；`HOMEWAY_TX_DBG` OnceLock（每包 getenv 全局锁）；
- 热路径缓冲复用（intercept 每拍分配 + `flush_backlog` 的 `copy_within`/`drain` memmove、`flows.keys().collect()` 每拍全表拷贝、wgcore 每拍 64KB/65KB 分配）；
- `relay` sendmsg iovec；`RelayLog::logf` 无缓冲（QC-design §5 登记）；`udpcap` 周期多睡一个 interval；状态快照分层；
- 全部改动跑 PERF-AB 复测纪律（loadavg 表头 + 干净环境判决）。

**尾段（Q-E 之后做，避免与 Q-E 的 `dnsproxy.rs`/`files*` 返工）**：
- DNS TTL 缓存 + socket/缓冲复用；files 客户端/服务端拷贝与分配。

### Q-J 通用性批
- keyenc 平台/布局字段化（HELLO 声明，缺省兼容）；DNS 上游列表/fake-IP 卫兵统一配置化；
- UPnP 协议面（IGD:2/AddAnyPortMapping/ST 兜底/钉卡降级）；`if_nametoindex` 守卫；
- launchd 探测精确化；平台假设盘点（CN 段目标、`/etc/resolv.conf`、`sun_path`）。
