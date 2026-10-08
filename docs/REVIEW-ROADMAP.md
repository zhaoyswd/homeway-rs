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
| **Q-F** | 客户端核与桥（portfwd 诚实性/状态分裂/预算/锁纪律） | **完成**（2026-10-08，提交 `e53f2f6`〔实现 F1–F8 + N1–N7〕+ `273bb88`〔判据登记〕+ `5684d93`/`ba1ffea`〔批记录 + r12/r13 摘要 + flake 证据〕；主会话独立复跑 `cargo test --workspace` **623 passed / 0 failed**；`cargo clippy --workspace --all-targets -D warnings` clean；`tools/check-vocab.sh` PASS）——F1 portfwd 落**诚实语义**（状态恒 `failed` + 明确 err、rc 恒 -1、`target` 文案收敛）/ F2 Ok-but-failed 三分支 + 可重建 + 槽空报 failed / F3 阶梯与闸等待纳入调用方预算（`LadderRc::Deadline`→-1）+ **N1 无界 RPC 收敛**（6s 上界结构可保的前提）/ F4 `dial_port` 克隆出锁拨号 / F6 `syncutil` 锁纪律单源（session 31 + recover 5 + tun_exec 3 + `wgcore::Client` + facade ~40 处）+ **Drop 路径零 panic** + 收工五段共享 6s 预算 / F7 spawn 失败全记行 + 桥 `unavailable` / F8 写退避分级 + 删死分支 + 巡回归因真因 + `Secret` 去 `Copy` + Drop 擦除 + `lookup_host` 分档令牌池。**复验剔除 0 条但订正 7 处**（关键：状态面**并未**谎报 ready——真问题是 rc 面谎报 + 不可重建 + 无自愈）；**新增 N1–N7**；**移出 1 项**（`tun_attach`/`tun_stop` 异步壳落点在 tier napi）。**portfwd 实装监听器挂账 = 独立批「Q-F-B」**（tier `openspec/specs/port-forwarding` 的 SHALL 在 B 落地前处于**已知不达标**；且冻结词表下 A 拿不到「既不违规又不误导」的表达——这两点是 B 批第一件事；交接块 = `docs/reviews/QF.md` §7，实现轮廓 = `QF-design.md` §2.4） | **通过（两轮）**（2026-10-08，dsh `r12.kyMDLA` exit=0：34 条〔3 组高危/14 中/13 低〕**全部认同**整改为 v2；dsh `r13.4ux4qT` exit=0 复校：4 条必改 + 4 条并入已回写 v3，判「不需要第三轮」并给出三条硬约定〔不得带旧写法进代码 / 判据变更同批 commit / 发现矛盾不得静默降级〕） | **通过（两轮）**（2026-10-08，dsh `r14.pE8g8w` exit=0：**无高危**，1 中 + 5 低 + 4 小渍 + 2 记账**全部处置**；dsh `r15.76jP4t` exit=0 复核：8 项整改**全部到位、无高危**，新增 6 条全部处置，结论「本轮整改足以过代码门」） | `docs/reviews/QF.md`（实现+代码门+**portfwd-B 交接**）+ `docs/reviews/QF-design.md`（设计） |
| **Q-G** | 进程卫生与 fd（CLOEXEC/poll revents/STOP_PIPE/权限） | **完成**（2026-10-08，提交 `2e785c3`〔实现 F1–F5 + 代码门 r18 整改〕+ `7512fbe`〔判据登记〕+ `14c25ca`〔批记录 + 设计文档 v3〕；主会话独立复跑 `cargo test --workspace` **639 passed / 0 failed**；`cargo clippy --workspace --all-targets -D warnings` clean；三目标交叉 check 全过）——F1 新增 `sysfd` RAII（linux/OHOS 原子 `SOCK_CLOEXEC`/`pipe2`、darwin fcntl）收口 **11 处生产缺口**（含审计未见的 `server/bind.rs:809`）+ 2 测试态 / F2 `poll_fd → Ready{readable,writable,hup,err,nval}`（判死只看 hup/err/nval、`n==0` 真睡 50ms 消热自旋、写侧 HUP-first + 期限检查写死循环顶）/ F3 relay `StopPipe` 每实例新建 + `WakeHandle` + 幂等 `shutdown` + 前台单通道 + 补 `POLLNVAL`（**修掉 Q-H「relay start 不可恢复」的根因**）/ F4 敏感文件原子 0600 + UDS 目录先 0700 + `SUN_PATH_MAX` 单源（**高危面 `server/state.rs` 私钥 `key.bin` + token 台账**收口）/ F5 5 处 `Client::stop_within(2s)`。**复验订正 2 处归因**（term 继承不成立——portable-pty 会关 fd；真实继承面 = 3 处 std `Command` exec，实跑探针证实）；**新增高危发现**（`state.rs` 静默 chmod）；**fd 扫描**：生产 raw libc 创建点 12 处（11 缺 + 1 正确）；`sun_path` 真实可用 **darwin 103 / linux 107**。手工回归：`relay start→stop→start ⇒ state=running`、broken 行 0、`lsof` PIPE 6→2；真互操作冒烟（`local-rust-exit.sh` + Go 基线客户端 `host add` ⇒「就绪（会话在位）」，C3/C6/C16 同串） | **通过（两轮）**（2026-10-08，dsh `r16.W8cBAM` exit=0：1 高/8 中/25 低 → v2 全改〔33/1/0〕；dsh `r17.lQ201f` exit=0 增量复校：上轮 30/34 真闭合，**新提 2 高**〔N1 判死条件 darwin 不可达 / U1 写侧期限悬空〕→ v3 全改〔21/21 认同，0 不认同〕） | **通过**（2026-10-08，dsh `r18.5G1xcc` exit=0：**0 高**/3 中/7 低 + D1–D4 设计-代码矛盾；3 条中危**全部认同并已改码**〔存量 `key.bin` 归一 / F4 失败策略倒挂改回告警不阻断 / `n==0` 地板睡眠〕；低危逐条改码或登记） | `docs/reviews/QG.md`（实现+代码门）+ `docs/reviews/QG-design.md`（设计 v3） |
| **Q-H** | CLI 与 daemon 控制面（config 双表/--state 形态/daemon 上限） | **完成**（2026-10-08，提交 `44a6e54`〔实现 F1–F17 + 代码门 r20 整改〕+ `3780f04`〔判据登记 14 行 + 计数输入集 3 行〕+ `1defaca`〔批记录 + 设计文档 v2〕；主会话独立复跑 `cargo test --workspace` **675 passed / 0 failed**；`cargo clippy --workspace --all-targets -D warnings` clean；`check-vocab.sh` PASS；linux 交叉 check 通过）——F1 config **单表收敛**（`serve_cli` 类型为唯一 schema + `load_config_strict` 含 Go `validateFile` 全量值域 + 补齐五项 + `assemble→Result` + `process::exit` 只留最前台 + 六 op 零副作用）/ F2 `cli_flags` 唯一取值器（`--state` 全形态 fail-fast + 泛化全体取值 flag）/ F3 relay 恢复**复核勾选**（Q-G F3 已修根因，实测 `start→stop→start ⇒ running`）/ F4 SOCKS `dead` 落账 + 可重建 / F5 daemon 连接上限 64 + 握手期限 10s（读侧）+ 句柄回收 / F6 `short_host` 字符截断 / F7 数值回退 fail-fast + 布尔显式值真生效 / F8 `--help` 短路扩到动词形（`serve stop --help` 不再真停）/ F9 session 锁 IO fail-fast / F10 `resolve_host` 全长 hex 须表内 / F11 host canonical 化（七入口）/ F12 hosts 坏 token 记行 / F13 模板 `burst_kib→burst_kb`（Q-A 遗留）/ F14 launchd KeepAlive 仅默认 state + F15 前台默认 state 对齐 / F16 `wait_stop_pipe` 收口 / **F17 C14「出口能力」行实装**（GAP-AUDIT P1-4 裁定「做」）。**复验剔除/降级 4 条**（均 Go 同构）；**实测回归已固化**（`tests/qh_config_failfast.rs` 5 条 E2E：非法 config 两形态 rc=1 + 可行动文案 + **进程存活** + 文件逐字节未变）。**生产 config 只读预检 = 通过**（Mac 两 state 不会被拒启；阿里云不可达未检）；**行为收窄提示**见 `QH.md` §5.3。**F14 偏差**：实机 plist 用自定义 `--state`（非设计门假设的零参）⇒ 该 state 下 CLI 直接自拉起（最坏 10s 节流抖动）——**裁定维持实现**，plist 精确匹配立 Q-J。**⚠️ 后续回补（Q-I 尾段 F0）**：F2 的**空值全拒过宽**——Go 对 `--stun`/`--stun6`/`--relay`/`--ddns` 空值**接受**（=「关/清空」），`--public-endpoint=` 才拒；已由 **Q-I 尾段 F0** 按逐 flag carve-out **局部回退**（`QIt.md` §1 取证 + 端到端复验） | **通过**（2026-10-08，dsh `r19.eFzARr` exit=0：A 类高 4 + B 12 + C 9 + 14 项「看过没问题」；**25/25 全部认同并入 v2**〔A 四条结构性修订：F1 值域表 + op 顺序、F17 探测目标、§5 登记表补齐〕） | **通过**（2026-10-08，dsh `r20.r0xcC5` exit=0：**1 高**/6 中/8 低 + 5 记账 + 22 项「看过没问题」；**H1 已改**〔EINTR 注入缝不碰 errno——修前 linux `--all-targets` 编译失败，已补跨目标 check 证据〕；M1–M6 全部改码；L1/L2/L3/L7 改码；L4–L8/A1–A5 逐条登记；无豁免未登记项） | `docs/reviews/QH.md`（实现+代码门+**生产 config 预检**）+ `docs/reviews/QH-design.md`（设计 v2） |
| **Q-I** | 性能细节批（memset/env/缓冲复用/DNS 缓存）——**2026-10-08 拆前段/尾段并提前**（前段＝device/wgcore/intercept/relay/facade/udpcap 即时做；尾段＝dnsface/files/工具，**两段均已完成**） | **完成（前段 + 尾段）**（2026-10-08）——**前段**：提交 `74969fa`〔实现 F1–F7 + 9 条单测〕+ `e28bf11`〔判据登记〕+ `d826c3d`〔批记录〕；`cargo test --workspace` 482 passed / 0 failed；clippy clean。F1 `tx_backlog` 换 `VecDequeLite`（叶帧 18.03%→0.38%）/ F2 删两处 65KB `clear+resize`（2.70%→0）/ F3 拦截层读缓冲收敛（1.49%→0.28%）/ F4 `envflag` OnceLock 6 站点（0.93%→0）/ F5-1 `resolve_udp` 缓冲复用 / F6 relay scratch + assoc 借用 + 孤儿扫描 O(L+A) / F7 udpcap 三态。**性能判决：叶帧全达标；总判据②（同吞吐 CPU ≥5% 降）未达（−0.6%）**——按 §4.3 证伪条款记「叶帧消失、收益落带内噪声」，**不宣称 CPU 收益**。**尾段**：提交 `9f44ec7`〔实现〕+ `92d4146`〔工具〕+ `4abfd1c`/`82e51b1`〔判据登记 + 批记录 + PERF-AB §9.19〕；主会话独立复跑 `662 passed / 1 failed`（唯一失败 = 在册 flake，隔离 3/3 绿）；clippy clean。**F0 Q-H 空值回归根因修复**（Go 取证：`--stun`/`--stun6`/`--relay`/`--ddns` 空值 Go 接受=「关/清空」，Rust Q-H 全拒是对 Go 的回归；`--public-endpoint=` Go 也拒 ⇒ 不在回归面；逐 flag carve-out + 端到端复验）/ F1 `DnsFaces.udp_rx`（**叶帧 1.71%→0 判绿**）/ F3 `service(scratch)` 穿参 / F4 `TcpConn.rx/tx`→`VecDequeLite` / F5 上游 `Arc` 快照 + worker 懒分配 / F6.1/F6.2 files 帧缓冲与客户端前缀偏移 / F7 `tools/qi-ab.sh`。**一号优化 F2（reactor 兴趣集并入引擎唯一 poll）实测负收益 ⇒ 按止损闸门整条回退**（poll 样本 +18%、CPUδ +14.5%、up −5.9%、拍频 ×1.69；机制 = 上游 fd 就绪成唤醒源放大每拍固定税）；**最终端到端无收益也无回归**（配对中位 down +1.4%/up +2.4%/CPUδ +0.6%/s/GB −1.0%）。**DNS TTL 缓存移出本批**（tier spec = raw forwarder + 无端到端判据；改防投毒姿态与上游计数语义）⇒ 另起小批；**「每查询新 socket」保持**（防投毒属性）；F6.3 不做（前提与 `wgcore::WriteOut.back` 矛盾） | **通过（两轮）**（前段：dsh `r5.SjItld` exit=0 → 认同 21/部分 1/不认同 0；尾段：dsh `r21.NQMZfM` exit=0，逐条**全部认同并入 v2**，0 不认同——评审独立回核 ≈25 处行号、复算数字、`rustc` 实证借用形态、挖出 F2 与已批 `reactor-design.md` 的反转冲突） | **通过（两轮）**（前段：dsh `r6.TJ3TeI` exit=0，0 高/4 中/7 低 四项已办；尾段：dsh `r22.CMf3ik` exit=0，**0 高**/3 中/6 低，门结论「改后过」——M1 `--ddns=` 同物种残留已扩 carve-out、M2 F4 包络登记订正、M3 poll 采样口径按全量数据订正，L1/L3/L4 已改、L2/L5/L6 登记） | `docs/reviews/QI.md` + `QI-design.md`（前段）；`docs/reviews/QIt.md` + `QIt-design.md`（尾段） |
| **Q-J** | 通用性批（keyenc 平台/DNS 上游配置化/UPnP 协议面） | 等指令 | — | — | — |
| **Q-F-B**（新立，2026-10-08） | **portfwd 实装监听器**（手机核 `127.0.0.1:<listen>` 真监听 + 状态面按真值产） | 等指令（**紧接 Q-F 之后的独立批**；交接真源 = `docs/reviews/QF.md` §7「portfwd-B 交接」，实现轮廓 = `QF-design.md` §2.4）——**落地前 tier `openspec/specs/port-forwarding` 的 SHALL 处于已知不达标**；**第一件事**是解决冻结词表下的表达（`bind_failed` = 假归因「端口被占用」/ 空码违反 MUST） | — | — | `docs/reviews/QF.md` §7 |
| **Q-I-DNS**（新立，2026-10-08） | **DNS TTL 缓存**（原 Q-I 尾段条目，取证后移出） | 等指令（小批）——**移出理由**：tier spec 是 **raw forwarder** + 无端到端判据；缓存会改**防投毒姿态**（「每查询新 socket」是属性）与上游计数语义（E22 `q`/`resp`/`fail`/`fallback` 输入集）。建议口径见 `docs/reviews/QIt.md` §7.1（含若做则须登记的计数语义清单） | — | — | `docs/reviews/QIt.md` §7.1 |

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
| `daemon::tests::handshake_deadline_beats_slow_drip`（2026-10-08 Q-I 尾段复跑观察） | 握手期限 vs 慢滴水的墙钟竞态 | 隔离 3/3 绿；高载轮红 |
| **同一工作树并发跑两个 `cargo test`**（2026-10-08 Q-F 复跑观察） | `daemon::carriers::forward::{add_roundtrip_remove_and_rebuild, per_host_cap_cascade_and_corrupt_file}` 用固定端口（19990/20001）互撞 ⇒ `EADDRINUSE` 红 | 串行/隔离恒绿；两个实例同时跑必红——**不是回归**，判回归前先确认没有第二个 `cargo test` 在跑 |
| `server::bind::tests::dual_stack_listen_and_unmap`（2026-10-08 Q-J 代码门观察） | 200×5ms 轮询窗内收 2 包（高载/并发下窗口内跑不完） | 隔离 5/5 绿、与两 `cargo test` 并发时红；`bind.rs` 未动（判非回归）；判回归前先确认没有第二个 `cargo test` 在跑 |

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
- **（Q-G 批移交，2026-10-08）** `daemon_cli` 的 **launchd 探测与 `--state` 不匹配**——临时 state 会去等
  **生产出口**的 launchd 代理（若 KeepAlive 触发会把生产出口拉起）；属 `--state` 形态族/N1·L7 同族，
  Q-G 代码门判定越界不改，移交本批立条（来源 = `docs/reviews/QG.md` §6 移交项）；
- **（Q-G 批跨批勾选义务）** 「`relay stop` 后 `relay start` 不可恢复」条目**根因已由 Q-G F3 修掉**
  （`relay_cli.rs` 去 `STOP_PIPE` 单例 + 幂等 `shutdown`）——Q-H 收口时**必须复核并勾选**该 AUDIT 条目，
  别当未修项重做；
- **（待用户裁决）** GAP-AUDIT P1-4「客户端『出口能力』打行」代码面无实现（服务端 caps 位在、
  客户端 Session 侧无消费）——做（客户端消费 caps 并打行）或标注不做，二选一由用户拍板。

### Q-I 性能细节批（**2026-10-08 拆前段/尾段**；前段即时做，尾段挂 Q-E 之后）

**前段（不与 Q-E 撞面）**：
- `Device::consume_step` 65KB memset 消除；`encap_peer` 每包 `to_vec`；`HOMEWAY_TX_DBG` OnceLock（每包 getenv 全局锁）；
- 热路径缓冲复用（intercept 每拍分配 + `flush_backlog` 的 `copy_within`/`drain` memmove、`flows.keys().collect()` 每拍全表拷贝、wgcore 每拍 64KB/65KB 分配）；
- `relay` sendmsg iovec；`RelayLog::logf` 无缓冲（QC-design §5 登记）；`udpcap` 周期多睡一个 interval；状态快照分层；
- 全部改动跑 PERF-AB 复测纪律（loadavg 表头 + 干净环境判决）。

**尾段（2026-10-08 已完成）**：
- ✅ 已做：`dnsface.rs` 每调用 64KB 零初始化（前段/ Q-E 双双交接的**第一靶点**）+ `service(scratch)` 穿参；
  `TcpConn.rx/tx` → `VecDequeLite`；DNS 上游 `Arc` 快照 + worker 懒分配读缓冲；files 服务端帧缓冲复用 +
  客户端前缀偏移；`tools/qi-ab.sh` 三臂 harness；**F0 Q-H 空值回归根因修复**（逐 flag carve-out）。
- ⚠️ **一号优化 F2（reactor 兴趣集并入引擎唯一 poll）实测负收益**（poll 样本 +18%、CPUδ +14.5%、拍频 ×1.69）
  ⇒ **按止损闸门整条回退**；端到端无收益也无回归。
- ❌ **DNS TTL 缓存 = 取证后不做**（tier spec 是 raw forwarder + 无端到端判据；缓存会改防投毒姿态与上游计数
  语义）⇒ **移出本批**，另起小批（建议见 `docs/reviews/QIt.md` §7.1）。
- ❌ **「每查询新 socket」保持不动**（防投毒属性，非缺陷）。
- ⏸ F6.3（上传 `write_all_owned` 单拷化）不做——设计前提与 `wgcore::WriteOut.back` 语义矛盾，超边界。

### Q-J 通用性批
- keyenc 平台/布局字段化（HELLO 声明，缺省兼容）；DNS 上游列表/fake-IP 卫兵统一配置化；
- UPnP 协议面（IGD:2/AddAnyPortMapping/ST 兜底/钉卡降级）；`if_nametoindex` 守卫；
- **launchd 探测精确化**（**Q-H 批立条**，2026-10-08）：F14 落地后「KeepAlive 仅默认 state」的假设与实机不符
  ——本机 plist 实为 `--state ~/.config/homeway-rs`（自定义 state）⇒ 该 state 下 CLI 不再等 KeepAlive 而直接
  自拉起（最坏 10s 节流抖动）。Q-J 需做 **plist 精确匹配**（多实例 / 多 label / **内容提及该 state**），
  取代「默认 state」这一粗判（来源 = `docs/reviews/QH.md` §5.2-1）；
- 平台假设盘点（CN 段目标、`/etc/resolv.conf`、`sun_path`）。
