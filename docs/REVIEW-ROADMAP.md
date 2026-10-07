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
| **Q-B** | 拦截层与出口数据面加固（P0-1/P0-5 等） | **进行中**（2026-10-07，第 1 棒设计完成） | **通过**（2026-10-07，dsh `r1.jhb9HE` exit=0；27 条意见→21 认同改设计/0 不认同；三处结构性修订 A1〔空帧收线时序〕/A2〔F1 有界性订正+门移进循环〕/B2〔F8 改 per-conn 续写〕已并入） | 待第 2 棒 | `docs/reviews/QB-design.md` |
| **Q-C** | 入口安全与资源上限（P0-2 等：wtransport/中继/设备表） | 等指令 | — | — | — |
| **Q-D** | 终端子系统加固（P0-3 等） | 等指令 | — | — | — |
| **Q-E** | 出口服务修复（P0-4 等：files/DNS/UPnP/speedtest） | 等指令 | — | — | — |
| **Q-F** | 客户端核与桥（portfwd 诚实性/状态分裂/预算/锁纪律） | 等指令 | — | — | — |
| **Q-G** | 进程卫生与 fd（CLOEXEC/poll revents/STOP_PIPE/权限） | 等指令 | — | — | — |
| **Q-H** | CLI 与 daemon 控制面（config 双表/--state 形态/daemon 上限） | 等指令 | — | — | — |
| **Q-I** | 性能细节批（memset/env/缓冲复用/DNS 缓存） | 等指令 | — | — | — |
| **Q-J** | 通用性批（keyenc 平台/DNS 上游配置化/UPnP 协议面） | 等指令 | — | — | — |

## 依赖与顺序建议

- **同文件面串行**：Q-B、Q-C、Q-G 同触 `server/intercept/**`、`wtransport/**`、`udp*` 面 → 按序做；
  Q-I 性能批触 Q-B/Q-E 刚修过的热路径 → 排最后；Q-J 依赖 Q-D（HELLO caps 协商）与 Q-E（DNS/UPnP）。
- **建议顺序**：Q-A → Q-B → Q-C → Q-D → Q-E → Q-F → Q-G → Q-H → Q-I → Q-J（用户可重排）。
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

### Q-I 性能细节批
- `Device::consume_step` 65KB memset 消除；`HOMEWAY_TX_DBG` OnceLock；
- 热路径缓冲复用（intercept/wgcore 每拍分配、relay sendmsg iovec、files 拷贝）；
- DNS TTL 缓存 + socket/缓冲复用；`udpcap` 周期修正；状态快照分层；
- 全部改动跑 PERF-AB 复测纪律（loadavg 表头 + 干净环境判决）。

### Q-J 通用性批
- keyenc 平台/布局字段化（HELLO 声明，缺省兼容）；DNS 上游列表/fake-IP 卫兵统一配置化；
- UPnP 协议面（IGD:2/AddAnyPortMapping/ST 兜底/钉卡降级）；`if_nametoindex` 守卫；
- launchd 探测精确化；平台假设盘点（CN 段目标、`/etc/resolv.conf`、`sun_path`）。
