# 全量对齐缺口审计（GAP-AUDIT）

> 2026-10-05。触发背景：用户发现 v6 双栈钉卡只迁移了 v4 半边（Go `pinToFD` 两族都设、
> Rust 只设 `IP_BOUND_IF`），说明「逐特性对照」存在盲区。本审计 = **系统性找出全部
> 未实现/未对齐能力**，产出分级缺口总账。只审计不实现。
>
> 基线：Go = `baseline/homeway`（d4148f6，212 个非测试 Go 文件）；Rust = 本仓 `crates/`
> （85 个 .rs 文件，R8-3 后 HEAD）。openspec 需求面取自 tier 仓 `openspec/specs/`（37 份，
> 只读；baseline 克隆里无 openspec 目录——AGENTS「45 份」说法已过时，homeway 仓 specs
> 已随契约治理集中到 tier/contracts 面）。

## 一、六路扫描方法与完成度

| # | 路 | 方法 | 完成度 |
|---|---|---|---|
| 1 | **反向日志行扫描** | grep Go 侧全部 `logf/dlogf/ulogf/rllogf` 调用点（296 处）→ 去重得**唯一格式串 257 条**（server/relay/egress/files/speedtest/term 侧 137 + clientcore/hostsession/wgcore/wtransport 侧 120）→ 剥离 `%` 格式占位符取特征短语在 `crates/` 逐条 grep → MISS 项**逐类人工核实**（区分真缺口/措辞差异/结构等价物） | 257/257 扫描 + 74 个初判 MISS 逐类核实 |
| 2 | **CLI 命令面 diff** | baseline 构建产物 `/tmp/homeway-go-audit` 全子命令 `--help` 树（serve/relay/term/files/host/forward/socks/speedtest/status/export/import/reset）vs `target/release/homeway-cli` help + `main.rs` 命令分发表源码 | 全量 15 命令族对照 |
| 3 | **config.toml 全键 diff** | Go `internal/nodeconfig/config.go` 结构体全字段（[serve] 13 键 + [relay] 3 键）vs Rust `serve_cli.rs`/`relay_cli.rs` 的 FileConfig + 消费面追踪 | 全键对照 |
| 4 | **环境变量旁路** | Go 侧 `os.Getenv/LookupEnv` 全集 vs Rust `env::var/option_env` 全集 | 双向对照（含 Rust 独有超集） |
| 5 | **openspec 需求面抽查** | tier 仓 37 份 spec 逐份一行判定（已实现/部分/未实现/不适用+理由） | 37/37 |
| 6 | **在册登记总账** | `docs/reviews/`（R0–R8 全 20 份）「登记/豁免/挂账/遗留」条目 + ROADMAP 当前指针遗留清单 + PERF-AB 挂账，合并去重 | 20 份全扫 |

**路 1 总体覆盖率**：剥占位符后粗匹配 server 103/137（75%）、client 80/120（67%）；
人工核实后其中约 25 条为措辞差异/结构等价（如 Rust `mark_unhealthy_if_current(gen, "fd")`
等价 Go「tun fd 读写失败：标记隧道不健康」；`网卡探测：` 行在 `egress.rs:528`），
**真缺口约 45 条日志行**，聚为下文 12 个能力项。

## 二、未知缺口清单（分级）

> 分级口径：**P0** = 阻塞生产部署（影响手机连接/安全/现役替代）；**P1** = 应补
> （功能面/诊断面完整性，不阻塞部署但用户可感知）；**P2** = 可不补/低价值（带理由）。
> 工作量：S < 半会话日；M = 1–2 会话日；L = 3+ 会话日。归属：B0 前置批 / C 批 / D 批 / 新任务。
>
> **2026-10-08 Q-L 批账实修正（本批）**：P0-1 余项与 P1-4/P1-7 状态按 Q-B…Q-K 交付追加回填
> （Q-A 之后 10 个批次一行未回填）；P1-8 在「Rust 现状」格追加逐子项结论（3 类已实现 +
> 1 条审计误判剔除 + 3 条真缺**转 QUIC M1 须显式立条**）；P2-4/P2-5/P2-7 追加状态；
> K 表 K-15 状态注 + K-20 复核注。**行内正文历史注记不改**（追加式；落笔机制 = 写入
> 「Rust 现状」/「归属」格 + dated 前缀）。逐条判定与一手证据 = `docs/reviews/QL-design.md`
> §1/§1.1 + `docs/reviews/QL.md`。
>
> **2026-10-07 Q-A 批账实修正（本批）**：P0-1 / P1-3 / P1-6 三行的**状态字段**按实际交付修正
> （原行把「B0-1 部署最小面完成」误记成整块已清、把 term CLI 与 export/import/reset 记成
> 「零/全缺」——实际已在 B0-2b 第 2 棒交付）；其中 P0-1 尚有**真实 OPEN 子项**（r1-N1/L7
> 前台默认 `--state` 与统一进程不一致），已转 Q-H 批（`docs/REVIEW-ROADMAP.md`）。
> 行内正文历史注记**不改**。
>
> **2026-10-05 B0-1 批处置**：P0-2/P0-3/P1-2 已修清；P0-1 部署最小面完成（统一进程期望态装配，
> 剩余 daemon 控制面/client 角色/CLI 族归 B0-2）。真网证据 = `INTEROP-CRITERIA.md` B0-1 两节。
>
> **2026-10-05 B 批换装后生产基线变更**：两台生产出口已换装 homeway-rs **v0.2.0**（Mac launchd +
> 阿里云双角色，`docs/DEPLOY-RUST-EXIT.md` 全记录）——本审计的「生产部署」判定基线自此按 Rust
> 形态；P1-1（serve 面文件日志体系）在生产的影响面 = dlogf 无落盘（peer/dns/dialok 判据行生产
> 不可见）+ events 无轮转，优先级维持 C 批不变。
>
> **2026-10-05 B0-2b 第 1 棒处置**：控制面底座（frame/vocab/proto/bus/listen/server/client——43 帧夹具
> 对拍 + 全协议集成测试）+ client 角色（hosts.json 表 + 每主机常驻会话 + reach 探测）+ stream.open 流
> 转发底座（TunnelConn 适配器，EchoConn 集成实证）+ serve/relay 动态角色管理（期望态写 config +
> start/stop/restart/token）+ host/status/serve 组/relay 组 CLI 接线 + daemon-{events,debug}.log 落地；
> 判据 = INTEROP-CRITERIA「daemon/控制面族实采」节（DC1–DC13）。
>
> **2026-10-05 B0-2b 第 2 棒处置（daemon 收尾棒）**：①supervisor 退避重建（r1-M4 清）②term CLI
> 五动词 + `--host` 远程模式（**P1-3 清**——双实例 9/9 判据：回显/marker 真执行/exit 7 直传/KILL/
> 接管 replaced/分离键；顺带修 blocking_push 漏 notify 的 P0 级 bug——recv_wait 消费面 15s 黑洞）
> ③serve.status peers/intercept 观测缝（EngineCmd::StatusQuery + 原子直读）④status --watch（**P2-5 清**）
> ⑤export/import/reset（**P1-6 清**——自管 ustar，Go↔Rust 工件双向互通实测）⑥低-6/S1 小项清。
> 记录 = docs/reviews/B0-2b.md §七–§十二。
>
> **2026-10-05 D-1 处置（承载面清零——P0-1 余量全清，GAP-AUDIT P0 全清）**：承载面 9 op
> 实装（forward：forwards.json 持久化 + 127.0.0.1 监听 + 半关闭透传 + RST 收口 + 端口全局唯一 +
> FIX-05 级联；socks：**SOCKS5 子集服务端从零建**〔no-auth/CONNECT/IPv4+域名、多 A 按序回退 +
> per-candidate 份额预算、off 端口记忆 + RST 在世连接〕+ DNS-over-TCP 解析腿〔A 查询 + NXDOMAIN/
> NoA 哨兵 + 期限壳〕；speedtest：per-host 单飞 runner 状态机〔waiting/busy 相位、link_down 在
> waitMs 预算内重试、refused→not_supported、取消合成 cancelled〕）+ files CLI `--host` 远程模式
> （Stream 承载枚举化，零 wire 改动）+ dialControlSpawn 按需拉起（锁试探/KeepAlive 等待/setsid
> 自 exec/spawn.log；纯读与直改族不拉起）。判据 = INTEROP-CRITERIA「承载面族实采」CA1-CA12 +
> carriers 单测 19 例；dsh 评审高 2 + 中 8 全处置（修 7 登 1，处置表 B0-2b.md §十七）。
> **P0-1 至此全清**（部署最小面 B0-1 + daemon 面 B0-2b×2 + 承载面 D-1）。
>
> **2026-10-06 D-2 批处置（程序最后一批）**：**P0-4 已修清（8o，见 P0-4 行）——P0 至此
> 终极全清**（P0-1〔B0-1+B0-2b×2+D-1〕/ P0-2 / P0-3〔B0-1〕/ P0-4〔D-2〕）。D-1 接棒
> 小项（B0-2b §十七登记段）= D-2 8p 清：files 问候帧看门（中-7）、9 op server 级用例
> （低-11）、lock_held_probe 收敛（补-3）、per-conn 线程 expect 僵尸面（低-1 的外部
> 可触发面）；低-7 余项与低-10 注记收口（重拨预算天然有界〔dial 内部 500ms 节拍〕+
> 孤儿收养无长期僵尸；carriers 公开 API 已类型化〔CarrierErr〕、内部装配面 String
> 不跨接缝）。下行第二瓶颈（K-8/K-9）= 8n 数据链归因 + 两层机制修复（PERF-AB §9.10）。

> **2026-10-05 B0-2a 生产可观测性批处置**：P1-1 已修清（双文件轮转 + 三级接线，判据行落盘
> 本地验收 + 两台滚动升级）；P1-5 登记修正后收口（主检测/停打/提示行 R3-3f 起已在位——审计时
> 误判缺失；真正缺的台账追加吊销分支行已补）；P1-7 动词别名半边收口（get/put 别名在，
> --host 远程模式仍随 B0-2）；P1-6 挂 B0-2b（与 daemon CLI 族同框理由见行内）。
> **B 批登记的 token 重铸时机缺口（DEPLOY §5）已修**（首轮探测后 10×1s 兜底重试——
> 根因非「relay 注册慢」而是探测失败形态下 print_client_token 从未被调）。

### P0（4 项）

| # | 能力 | Go 侧位置 | Rust 现状 | 影响 | 归属 | 量 |
|---|---|---|---|---|---|---|
| P0-1 | **daemon/统一进程整块——✅ 已清（B0-1 + B0-2b×2 + D-1〔承载面 9 op + files --host + dialControlSpawn〕，2026-10-05）；剩余子项 OPEN（2026-10-07 Q-A 账实修正）：r1-N1/L7——前台 `serve`/`relay` 默认 `--state=.` 与统一进程 `~/.config/homeway` 不一致（`serve_cli.rs`/`relay_cli.rs` 默认值未对齐）⇒ 默认取值下「全形态共用锁」不成立；已转 Q-H 批**（`docs/REVIEW-ROADMAP.md` Q-H 节「N1/L7」条）。原部署最小面（B0-1）：零参统一进程（config 期望态装配 serve/relay、单实例锁 `<state>/lock` 全形态共用、三层布局 + config 原子生成、events 最小集、SIGTERM/SIGINT 按序收尾；client/control 留桩如实标注）；两生产形态真跑验收 + 手机式客户端数据面通（INTEROP-CRITERIA「B0-1 统一进程两生产形态验收」节）。**剩余面归 B0-2**：control.sock 控制面（帧复用/hello/req/evt/stream.open）、client 角色（hosts.json/多主机会话）、CLI 消费面（host/forward/socks/status --watch/term --host/files --host/serve start-stop-restart）、~~events 完整轮转体系~~（**serve 面 ✅ B0-2a**：events 2MB×3 + debug 8MB×2；daemon 侧 daemon-events/daemon-debug 仍归 B0-2）、**角色装配失败的 supervisor 退避重建**（r1-M4：现 Rust exit(1)——launchd KeepAlive 能拉回，nohup 形态直接退出）、前台 serve/relay 的 OpenNodeState 三步序与默认 --state 对齐（r1-N1/L7：前台默认 --state=. 与统一进程 ~/.config/homeway 不一致 ⇒ 默认取值下「全形态共用锁」不成立） | `internal/daemon/` + `internal/control/` + `clientcore/facade/` | 部署最小面在（`homeway-cli` 零参 + `nodestate.rs` + `unified_cli.rs`）；daemon 面 B0-2 | 已对齐（承载面 9 op + files --host + 拉起形态 = D-1 清）**（2026-10-08 Q-L：余项已清——见「归属」格 dated 注）** | **已清（B0-1 + B0-2b×2 + D-1）；余项 OPEN：r1-N1/L7 前台默认 `--state` 未对齐 → Q-H 批**（2026-10-07 Q-A 账实修正）。**（2026-10-08 Q-L）全清**：余项 r1-N1/L7 已由 **Q-H F15** 落地（前台 serve/relay 默认 `--state` = `~/.config/homeway`，判据登记 = `INTEROP-CRITERIA`「N1/L7 默认 state」行 + 单测 `default_state_matches_unified`）；Q-J/Q-K 两批对本行无残留（逐份扫「残余/挂账」节零命中） | — |
| P0-2 | ~~v6 双栈钉卡半边~~ **✅ 已修（B0-1，2026-10-05）**：`pin_socket_to_iface` 两族都设 + 各自单栈容错（两族都失败才报错、文案 Go 同串）——`egress.rs`；配套单测钉 v4/v6/双栈三类真 socket + 非法 index 错误面 | `pkg/ifaceutil/ifacebind_darwin.go:31-36` | 已对齐 | — | 已清 | S |
| P0-3 | ~~v6 STUN 观测与 v6 公网端点公布族~~ **✅ 已修（B0-1，2026-10-05）**：`--stun6`/config 接线（默认 cloudflare 对齐 Go）；**出口主 socket 双栈化**（[::] V6ONLY=0，v4 回退；IP 字面量单栈 udp4/udp6 **且按地址所属网卡附带钉卡**——Go IfaceForAddr 分支，r1-M3 整改）+ recv unmap + 发送族 map；v6 同 socket 观测（8s 预算）+ 三形态判据行 + `[v6]:port` 条目 + token 携带；客户端 wtransport socket 同步双栈（真机 v6 直连前提）。真网全链实采见 INTEROP-CRITERIA「B0-1 手机 v6 直连真机实采」节 | `internal/server/publicendpoint.go` + `pkg/egress/stun.go` | 已对齐 | — | 已清 | M |
| P0-4 | ~~DDNS 双半边~~ **✅ 已修（D-2 8o，2026-10-06）**：server 侧 `serve ddns add/delete/list`（一次性直跑写 config，幂等语义/提示行同串）+ `[[serve.ddns]]`/`--ddns` 配置消费（裸域名校验）+ token 叠加域名条目（端口 = 公布端点外部口，`ddnsEntryPort` 同义）+ 域名自检 `server/ddnscheck.rs`（raw UDP :53 双公共解析器回退、fake-IP 段/非全球单播两档卫兵、连续 3 拍滞后阈值、缺 AAAA 告警+恢复行、解析失败首拍节流+恢复行；随公网端点探测同拍、kick 轮同跑、探测全关跳过告示行）+ serve.status ddns 段（DdnsBrief/lagStreak/warnedLag/warnedAAAA）+ 顺手补 `serve relay set/clear`；client 侧 `wtransport/domain_eps.rs`：token 域名端点展开（建会话解析一次 A+AAAA v4 前、失败跳过记行「token 端点 %q 域名解析失败（跳过）」/成功「解析为 %d 个地址」）+ 域名重解析编排（DomainRefresher：Rearm/RearmSoft 并发另跑 5s 单飞、失败退回、变化记行「域名重解析：%d 条候选已刷新」、中继采纳节流 15s 软赛跑补投「域名重解析晚于赛跑结算…」、旁路探测拍同步 3s）+ hosts reach 域名解析。本地验证：CLI 往返/幂等/--json、config 写出、token 3 端点含「home.example.com:42677（域名）」、自检真跑失败行（公共解析器全失败形态）；判据行入 INTEROP-CRITERIA DDNS 节 | `internal/server/ddnscheck.go`/`ddnsresolve.go`/`servegroup_cli.go`（ddns）+ `clientcore/hostsession/session_endpoints.go`（域名解析编排） | 已对齐 | — | **已清**（D-2 8o） | — |

### P1（8 项）

| # | 能力 | Go 侧位置 | Rust 现状 | 影响 | 归属 | 量 |
|---|---|---|---|---|---|---|
| P1-1 | ~~serve 面文件日志体系~~ **✅ 已修（B0-2a，2026-10-05）**：共享 `logfile::RotatingWriter`（Go `internal/logfile` 语义全量：O_APPEND/超限轮转最旧删/失败丢段重试自愈）+ events.log 2MB×3 + debug.log 8MB×2 双文件 + 统一进程/前台 serve 三级接线（logf=stdout+events 双写、dlogf=debug 恒落盘+verbose 回显）；relay::logfile 私有轮转器收拢为同源消费方。判据行落盘验收（本地统一进程）：`serve 就绪`/token 行在 events.log，`peer: +`/`intercept: tcp transit …（dialok）`/`dns: q=` 在 debug.log；两台生产滚动升级随批（DEPLOY §9） | `internal/server/logging.go` + `internal/logfile/` | 已对齐 | — | 已清 | M |
| P1-2 | ~~绑卡看护循环~~ **✅ 已修（B0-1 升级入批，2026-10-05）**：`server/bindwatch.rs`（5s 指纹看护 + 60s 健康探针 + 连续 2 次防抖重挑 + 重钉经 EngineCmd::Repin 在驱动线程两族执行 + 换卡踢公网端点/udpcap）；六分支单测 + 真网注入实采（`HOMEWAY_BINDWATCH_PROBE` 死地址——探针失败 1/2→2/2→连续探不通链全行） | `internal/server/bindwatch.go` | 已对齐 | — | 已清 | M |
| P1-3 | ~~term CLI 五动词~~（list/new/attach/delete/explain 离线+在线）：attach 交互（detach 键/尺寸重对齐/标题 OSC 2/TERM_SESSION_ID 回环检测） | `pkg/term/term_cli*.go`（~1,800 行） | **已实现（2026-10-05，B0-2b 第 2 棒 commit `40668c9`）**：`homeway-cli term` 五动词在位（`crates/homeway-cli/src/term_cli.rs`），含 `--host` 远程模式（DC14/DC15 双实例 9/9 判据——回显/marker 真执行/exit 7 直传/KILL/接管 replaced/分离键）。**原判「Rust 无 term CLI（CLI 面零）」为审计时点快照，已被交付超越**（2026-10-07 Q-A 账实修正） | 原影响面已消（运维可在主机上 `homeway-cli term list/attach`） | 已清（B0-2b） | M |
| P1-4 | **出口能力客户端打行**：参照点探测取回能力位后「出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v」+ 失败归因行 | `clientcore/hostsession/session.go:252-280` | 服务端半边在（udpcap_loop 喂探测应答 caps，engine.rs:1279+）；**客户端 Session 侧无消费/打行**。**（2026-10-08 Q-L）已清（Q-H F17）**：C14 行实装（服务会话启动序列 C13 之后；成功行逐字 Go 同串 + 失败归因行 `出口能力：参照点探测失败（%v）—— 本机网络到出口的 UDP 不通或出口未应答`；探测目标 = token 序首个已解析候选）；证据 = `QH.md` F17 行 + `format_outbound_caps_line`/`format_outbound_caps_fail` 纯函数 + 4 单测 + 判据登记（`INTEROP-CRITERIA`「C14 首次实装」行） | 手机侧诊断面缺一行关键证据（这台出口 UDP 转发到底行不行——TUN 代理污染场景排障依赖它）；tunStatusJSON 若含该面同步缺。**（2026-10-08 Q-L）原影响面已消** | C 批 | S |
| P1-5 | ~~token 吊销运行期告警族~~ **✅ 收口（B0-2a，2026-10-05；含登记修正）**：主检测/停打/提示行（「⚠️ 在用凭证已被吊销——不再打印 token…」+ 吊销 1s 跟随读）**R3-3f（f8157dd）起已在位**——本行审计时误判缺失；真正缺的是台账追加的 `ErrSecretRevoked` 分支专用行（「…本轮 token 未入台账…」），已在 4b344fa 补齐 | `internal/server/serve.go`（revoked 运行期检测） | 已对齐 | — | 已清 | S |
| P1-6 | ~~CLI 状态/工件面（daemon 无关半边）~~：`export`（不变量四件打包 tar）/`import`（布局校验+回滚落位）/`reset cache`（清可弃层，在跑拒绝）——纯文件操作，不依赖控制面。**挂 B0-2b**（B0-2a 拍板）：与 daemon CLI 族（status --watch/host/forward 等消费面）同属一框——spec 的「进程必须在停」校验与单实例锁/控制面生命周期强耦合，独立先做会与 B0-2 的 CLI 框架重复设计 | `internal/daemon/artifact_cli.go` + `internal/nodestate/artifact.go` | **已实现（2026-10-05，B0-2b 第 2 棒 commit `57156d8`）**：`main.rs`/`daemon_cli.rs` 实装 export/import/reset（自管 ustar，Go↔Rust 工件双向互通实测；在跑拒绝 `import: 目标 state 的统一进程在跑（…/lock 被持有）——先停进程再导入`）。**原判「全缺」为审计时点快照，已被交付超越**（2026-10-07 Q-A 账实修正） | 原影响面已消（换机/备份/复位运维路径可用） | 已清（B0-2b） | S-M |
| P1-7 | **files CLI 动词名 + 远程模式**：Go 六动词 `list/stat/mkdir/read/get/put` + `--host <ref>` 经控制面转发 + `--timeout` 三段预算。**动词别名 ✅ 已修（B0-2a，de6b2e9）**：`get`/`put` 与 `download`/`upload` 双名等价（两侧脚本契约都不破）；`--host` 远程模式维持随 B0-2（控制面依赖） | `pkg/files/files_cli.go` | 动词别名已对齐；`--rate-limit`/tierpart 原子落盘在。**（2026-10-08 Q-L）已清**：`--host` 远程模式已由 **B0-2b 第 2 棒 + D-1** 交付（Stream 承载枚举化，零 wire 改动）。**加注（QIt.md §7.2-4 的运行时缺陷挂点已注销）**：`files --host` 本机实测全绿（list/stat/put/get × 1 MiB/64 MiB，sha256 一致，默认限速 2 MiB/s）；`QIt.md` 所记失败**只在 `--rate-limit 0` 下复现**，且是双向文档化的既定语义——Rust `files.rs`「发送端速率义务：无 ack/credit 下的盲节流；0 = 不限、风险自担（越界被对端收流属可预期边界）」+ daemon 上行工位有界队列（32 帧/512 KiB，超界收流 `gone`）；Go 同形（`files_cli.go` DefaultRateLimit=2MiB/s / `--rate-limit 0` 同文案 + `internal/control/stream.go` upWorkerItems=32/upWorkerBytes=512KiB + 单测 `TestStreamUpstreamOverToleranceStillGone`）。⇒ **N1 判闭合（harness 用法问题，非承载面/控制面缺陷）**，判据 = `QL.md` §N1 / `QL-design.md` §2-N1 | 远程模式随 daemon。**（2026-10-08 Q-L）已清（B0-2b/D-1）** | B0-2（--host 半边）。**（2026-10-08 Q-L）已清** | S |
| P1-8 | **公网端点细节分支族**：「外口 %d ≠ 监听口 %d（沿用历史端口/回退），用 STUN 的 IP + UPnP 的外口公布」「已按 --public-endpoint 配置公布（在）」「写 %s 失败」「监听端口落盘失败」「--bind-interface 找不到→退回 auto」「--public-endpoint 非法→按未配置」「--relay 解析失败→跳过中继注册」 | `internal/server/publicendpoint.go`/`cli.go`（resolveBind/校验告警族） | 主公布路径/`--public-endpoint` 覆盖/端口退让在（bind.rs:181 同串）；**端点不一致仲裁分支、两处落盘失败告警、三个 flag 校验告警缺**（Rust Explicit 网卡名不验存在性）。**（2026-10-08 Q-L 逐子项结论，追加式）**——**3 类已实现（Go 同串）**：① 端点不一致仲裁 `engine.rs:1550-1558`（含 `pinned` 运行期判据 + 逐字行文）；② `--public-endpoint` 公布分支 `:1472-1488`；③ `--relay 解析失败跳过注册` `:554-560`（Go `serve.go:425`）+ `--bind-interface 找不到→退回 auto` `:309-338`（Go `cli.go:201-207`）。**❌ 审计误判（Q-L 剔除）**：「Rust Explicit 网卡名不验存在性」**不成立**——`engine.rs:309` 即 `egress::interfaces().find(name)`，找不到才告警回退，与 Go 同口径（Go 也不在 config 期验存在性）。**真缺 3 条（转 QUIC M1，须显式立条）**：`public_endpoint.txt` 写失败静默 ×2（`engine.rs:1476/1630`；Go `publicendpoint.go:126/226` 有告警）/ `listen_port.txt` 写失败 **Rust 致命（`?` 上抛）vs Go 非致命**（`engine.rs:418`；Go `role.go:95` 告警继续）/ `--public-endpoint` **flag 值域零校验**（`serve_cli.rs:694-695`；Go 前端报错 `cli.go:98-105`、守护期告警清空 `serve.go:113-121`）——三者**均不会被 QUIC M5 删除/重写**（公共端点面在 QUIC 下保留），本批按批派单「公共端点不在本批修」不动，**交接 = QUIC M1 显式立条**（`docs/QUIC-ROADMAP.md` **M1 开工前置检查项（Q-L 交接）** Q3/Q4/Q5——2026-10-08 代码门 中2 订正：M0 已完成，收件人 = M1） | 观测面边角：异常形态下行为等价性未证实（如外口/监听口不一致时公布什么）——**（2026-10-08 Q-L）已证伪：仲裁分支已实现且 Go 同串**；告警缺失让配置 typo 静默——**（Q-L）剩余 3 条转 QUIC M1** | C/D 批。**（2026-10-08 Q-L）3 类已清 + 1 条误判剔除 + 3 条转 QUIC M1** | S |

### P2（9 项，可不补/低价值）

| # | 能力 | 不补理由 |
|---|---|---|
| P2-1 | `peer: ! dev=… 写入/移除 peer 失败` 4 条 + `peer: ⚠️ 设备配置操作排队超时` 3 条 | Go 侧是 opCh 异步队列的错误路径；Rust table.rs 同步执行返回 DevOp 序列（R3 设计刻意消队列），结构上无「排队超时」形态；device 写失败罕见路径错误面已有 Result 链兜底 |
| P2-2 | Go goroutine panic recovery 5 条（待发包下推器/旁路探测/统计/巡检 panic 已恢复） | Rust 线程模型不同：阶梯线程 panic 兜底在（recover.rs:375 catch_unwind）、tun 面世代守卫在（tun_exec.rs:1187 panic→mark_unhealthy）；关键面已有等价保护，逐线程 catch_unwind 属 Go 直译 |
| P2-3 | `tun fd=%d TUNGETIFF/fstat/mode` 4 条 Linux 诊断行 | Linux 调试面（TUN fd 形态核对）；Rust 核跑 OHOS/macOS，TUNGETIFF 是 Linux UAPI——不适用面 |
| P2-4 | `HOMEWAY_TERM_TITLE`/`TERM_SESSION_ID` | term CLI attach 的标题 OSC 2 与回环检测——随 P1-3 term CLI 一并，单独无价值。**（2026-10-08 Q-L）已清**：随 P1-3/B0-2b 落地（标题 OSC 2 `term_cli.rs:1430` + `TERM_SESSION_ID` 回环检测 `:942-944`） |
| P2-5 | `HOMEWAY_LIVE_SOCK`/`HOMEWAY_LIVE_TOKENS` | status --watch 的 live 渲染注入缝——随 P0-1 daemon 面。**（2026-10-08 Q-L）不适用/等价面**：`status --watch` 已交付（B0-2b）；`HOMEWAY_LIVE_*` 是 Go 侧测试注入缝，Rust 自有测试体系（不补） |
| P2-6 | `HOMEWAY_IT_TOKEN`/`HOMEWAY_DAEMON_LOCK_CHILD`/`CLI_TEST_*` | 测试注入缝，Rust 有自己的测试体系（HER_SEED 等） |
| P2-7 | config 值域校验细节：Go fail-fast 带文件行号（bind_interface 枚举细校/ddns 裸域名/relay 取值）vs Rust serde deny_unknown + 端口/时长手动校验（无行号、网卡名不验存在） | 键表 typo 保护两侧同强度；差异仅在报错精细度，P1-8 的告警族补齐时顺带即可。**（2026-10-08 Q-L）**：**值域表 = Go `validateFile` 全量**（Q-H F1：`serve.listen`/`bind_interface`/`public_endpoint`/`serve.relay`/`relay.listen` 五项补齐）；**已知残余**：`peer_ttl` 接受集窄于 Go（`QH.md` §5.2-10）+ 报错无行号（TOML 语法/类型错误除外——toml crate 原文含行号与源码片段）+ 写回无 CAS（`QH.md` §5.2-14） |
| P2-8 | 服务收工判据行措辞（`files/speedtest 服务收工：%v`/`speedtest 服务未启用`）与 UPnP 让位/清旧映射 2 条、dns 代答「未能建立任何监听/监听失败」2 条、`中继端点与直连端点相同按直连处理`、`中继：挑战 DH 计算失败`、`pipe %v→%v` | 收工序本身在（D5 有序收工 + UPnP 退出缩租）；这些是观测行/边角错误路径，主判据行已对齐（INTEROP-CRITERIA 在册），按需在 C 批判据补采时顺手 |
| P2-9 | `⚠️ 主 socket 读错误原地重试（防读 goroutine 死亡失聪）` | wireguard-go 专有结构（读 goroutine 交回机制）；Rust 自管驱动线程 poll 循环无此形态 |

### 扫描中确认**无缺口**的重点面（防复查重劳动）

- **NAPI 20 导出面**：Go `//export` vs `homeway-capi` 符号集 **diff 为空**（R7 交付完整）。
- **RECOVER 恢复阶梯全族**：`session/recover.rs` 判据行逐串对齐（R2 实测四档时间窗）。
- **udpcap 服务端半边**：udpcap_loop + 探测应答 caps 位在（缺的只是客户端打行，P1-4）。
- **网卡探测/自动挑卡**：`egress.rs:528` 同串；select_best 探针判据同源。
- **DDNS 之外的 token/协议/帧面**：fixtures 向量 + golden 全绿在册（ci-local 门）。
- **fuzz**：Rust 九目标 cargo-fuzz 是 Go 侧没有的超集，无缺口。
- **信号面**：serve SIGTERM/SIGINT 有序收工（D5 序）在；daemon 面的 stop 异步应答随 P0-1。
- **term env 面**：HOMEWAY_TERM* 17 个中 15 个已消费（缺 TITLE 随 P1-3）；Rust 另有 TX_*/UDP_NO_BATCH/WG_DEBUG 独有调试缝（合理超集）。

## 三、已知登记总账（与上节未知缺口分开）

> 来源：docs/reviews/ 全 20 份 + ROADMAP 当前指针 + PERF-AB。去重合并后 20 项。

| # | 项 | 出处 | 状态 |
|---|---|---|---|
| K-1 | v6/双栈族（R3 评审 M10「v4-only 登记 R5」+ R3-design §0「双栈/v6 面整族登记 R5 补」） | R3-design/R3.md | **登记未兑现**（R5 未做）→ 本审计升级 **P0-2/P0-3** |
| K-2 | DDNS 自检（R3-design §0 不做清单） | R3-design | 登记未做 → 本审计 **P0-4** |
| K-3 | 绑卡看护循环（R3-design §0 裁剪「保留启动期一次性」） | R3-design | 登记未做 → 本审计 **P1-2** |
| K-4 | daemon 控制面（R3-design §0「本期只有前台 serve 与纯读 serve token」） | R3-design | 登记未做，R4–R8 未接 → 本审计 **P0-1** |
| K-5 | UPnP 真实网关实测（SSDP 组播被 macOS 本地网络隐私拒——不可本地测） | R3.md M8 残余 | **豁免维持**（代码面 R5 已补三件套 upnp.rs:155；与现役 Mac 出口 launchd 形态同款环境问题） |
| K-6 | tier `homeway-rs.pin` 期望 SHA | R8 §五③ | 挂账 R8-2（与「仓归属」用户触点一并定） |
| K-7 | echo RTT +6.3ms 恒定粒度（poll 拍密） | PERF-AB §6.2 | 挂账（优化候选归 R8+） |
| K-8 | 吞吐 >45MB/s 下一档（手机侧每字节成本 1.8× + ACK 时钟面） | PERF-AB §9.7-bis | 登记（第二瓶颈，非本批） |
| K-9 | B 热态绝对值 25-30 vs 40 预期（消融证明与突发正交） | ROADMAP R8-3 | 登记在册 |
| K-10 | files 上传 picker 自动化不稳定 | R7 E2E P2-4 | App 侧真手指清单维持 |
| K-11 | cmd 通道无界（vs Go hubQueue=512） | R7 r3 我-4 残余 | 维持登记（换 bounded 引入新阻塞面，不换） |
| K-12 | speedtest 取消生效点延到拨号预算边界（Go ctx 即刻打断） | R7 M-8 残余 | 维持登记 |
| K-13 | KNOWN-GAP speedtest 经中继并发（200pps 防放大闸） | R5 5-a | **转正为两侧共有形态限制**（Go 同值） |
| K-14 | rekey stall（>30s 跨 rekey 断流） | R6 前置批① | **根因收口**（双会话互踢形态，产品形态无此问题） |
| K-15 | term §9.7 口径注记 6 条（DECSLRM 恒 invalid/lossy 替换/47 备用屏等） | R6.md/R6-design §九 | 在册豁免。**（2026-10-08 Q-L）状态注**：其中「47 备用屏」一条已由 **Q-L L2** 实现（`?47h/?1047h` 真切屏 + 进出双向整光标拷贝 + 主屏 DECSC 槽保护；见 `INTEROP-CRITERIA`「判据变更记录」Q-L 行）；**残余** = 重入清空差异 + 混合形态终点 + grow 位移丢失（alacritty 进入即复位 alt 内容 / `1049h→47l→1049l` 落 D-15 分支 / 捕获槽跨 resize 放大少位移——**共 4 条**已登记「已知口径注记」Q-L 条） |
| K-16 | `i32::MIN` 哨兵（ENDED code 词表契约） | R6 L7 | 保留（词面契约） |
| K-17 | D-19 portable-pty 信号死 -1 | R6 6f-3a | 已按 Go 逐值对齐处置（6f-3b） |
| K-18 | R2 登记项（tunStatusJSON 键面→R7 已兑现；CLI 并发 foot-gun→7e 会话锁已兑现） | R2.md 豁免表 | 已清 |
| K-19 | R5 第二道门登记留档 4 中 + 2 低 | R5.md | 已随 R6 前置批清账 |
| K-20 | R8 收官批剩余触点项（共存定案/终测报告定稿/tier 文档指针/AGENTS-README 定稿/遗留清账移交） | ROADMAP 当前指针 | 待做（收官批，多为用户触点）。**（2026-10-08 Q-L）复核注**：R8 之后各批已收官（Q-A…Q-K + Q-F-B + Q-L 全清），本表剩余 = **用户触点**（tier `homeway-rs.pin` 前进 / 终测定稿 / tier 文档指针）——见 `ROADMAP.md`「下一步（当前指针）」 |

## 四、汇总统计

| 级 | 项数 | 归属建议分布 |
|---|---|---|
| **P0** | 4 | B0 前置批 3（v6 钉卡 S + v6 STUN/公布 M + DDNS M）；B0/C 1（daemon 整块 L，可四段拆） |
| **P1** | 8 | C 批 7（日志体系 M/绑卡看护 M/term CLI M/出口能力 S/吊销告警 S/工件面 S-M/files 动词 S）；C/D 1（端点细节族 S） |
| **P2** | 9 | 登记豁免/随上级项/不适用面，均带理由 |
| 已知登记 | 20 | 4 项被本审计升级为缺口（K-1~K-4）；豁免维持 6；已清/收口 5；挂账在册 5 |
| 工作量合计 | — | P0 ≈ 9–14 会话日；P1 ≈ 6–9 会话日 |

## 五、一句话总评

**遗漏面大但高度集中**：数据面（wire/协议/恢复阶梯/拦截/term 服务/fuzz）经逐行对照基本无缺口（257 条日志行覆盖率 75%+，残余多为措辞差异），真正的洞集中在**两簇**——①「v6/端点公布族」（钉卡半边 + stun6 空消费 + DDNS，三项同族，且 Mac 现役出口正靠 v6 直连，是 B 批阻塞级）；②「daemon/统一进程整块」（Go 侧 19k 行的控制面 + client 角色 + 期望态装配，R3 明确裁剪后一直没人接，导致生产部署形态与 9 个 CLI 命令族整体缺失）。修完这两簇，Rust 版才谈得上「现役替代」。
