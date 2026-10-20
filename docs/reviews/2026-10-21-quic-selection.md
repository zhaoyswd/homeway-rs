# 2026-10-21 QUIC 赛跑与中继回直连整改

范围：最新 `708c95b` 工作树中重新核查 F5（健康中继不回直连）、F8（TLS 首胜提前取消后备），
以及客户端 socket 双重分配、动态 DNS 候选消费。未调用 reviewer skill，依用户要求由实现者
直接作技术自评和完工自评。生产实例未操作，未提交、发布或部署。

## 技术门：重新核查与设计裁决

重新读了 `client/race.rs`、`client/relay_sock.rs`、`client/mod.rs`、`driver.rs`，并核查本地
quinn-proto 0.11 的 migration 接收条件。两项发现仍成立：TLS 握手首胜即 abort 后备发生于
准入之前；中继 hint 被忽略，健康中继没有升级入口。最近其它修复没有消除此二问题。

| 检查面 | 决策与理由 |
|---|---|
| F8 正确性 | TLS 完成只产出待准入连接。握手后备留在 JoinSet；准入串行，收到 A4 才选胜者并关闭输家，避免同 devTag 并发注册互踢。 |
| 错误边界 | 凭证/非法准入数据的明确拒绝终止整轮，保留 typed 错误；传输中断、准入超时、暂时资源不足可尝试后备。 |
| 时间与资源 | 单候选准入最多 2s，全轮最多原 budget+2s；候选累计最多 64。DirectFirst 仍为 min(配置, budget/2)，直连全部失败可提前解锁中继。 |
| 动态 DNS | Hooks 的 refresh 只触发宿主后台刷新并立即返回；race 每 100ms 纯读 snapshot，将本轮新地址加入，遵守同一原期限与直连窗口，不在岛上解析或落盘。 |
| F5 不互踢 | 复用原 QUIC 连接与 CID，不做设备重新注册。物理路径切换在 socket 层，将直连源映射回现有逻辑 peer。成功仅改 via/ep，原有服务流保持。 |
| 认证证据 | 每候选先以同 RPK TLS-only 握手预筛，成功显式 close、不发 Hello；随后原连接新开 probe 流，写入 32B 随机 nonce 并要求逐字节回显。临时切换时拒旧 relay 回包，裸 UDP 到达不是确认条件。 |
| 健康路径保护 | 黑洞预筛不改变现任物理路由。每候选预筛与终验共享一个 700ms 期限，每轮最多三个，轮间 30s。TLS-only 失败时现任所有包仍走 relay，不能先让用户流量黑洞 2.1s 再声称“服务流保留”。 |
| 跨家庭 | v4 relay→v6 direct 与反向通过一枚辅助 UDP socket 保持原 Endpoint/conn 身份；缺候选族 Endpoint 时，预筛临时创建同族 Endpoint，不注册。 |
| 生命周期 | RAII 在失败、取消、panic 时撤临时路由；换网重建 aux 并唤醒原 Endpoint。新赛跑开始前撤物理 override 并同步旧 live 为 relay，避免同逻辑 peer 的新 TLS 借用旧路由。 |
| 不可信 hint | 仅接受已配置 relay 的精确源地址、完整且有界帧。先归一 IPv4-mapped，再拒本地/私网/链路本地/多播/未指定/零端口；仅加入未验证候选，不能直接采纳。 |
| 简洁性 | 沿用现有 quinn migration、probe 服务与 endpoint；不引入新的注册协议或 CID 解析。包封从双 Vec 分配改为一次。 |

开工设计经主会话审阅后实施。实现中根据主会话复核补充了 TLS-only 预筛、单族 token 后学另一族地址、mapped 地址过滤。

## 代码门：结构化自评

- 对齐完整性：DirectFirst 原有回归全部保留，准入胜者语义与判据断言同时更新；主会话负责跨仓连接策略与判据登记。
- 并发：赛跑/升级互斥，升级与阶梯探测不重叠；准入只有一个在途；辅助 socket 只由原逻辑 peer 所属 Endpoint 接收，避免两个 endpoint 抢包。
- 错误与收尾：已完成的预筛连接显式 close；未完成 Connecting 取消后仍由 quinn/出口期限回收，最多每轮三个，30s 节流。准入失败连接显式 close。辅助发送也经过既有系统禁发/本机错误统计。
- 接收边界：aux EAGAIN 后重新 poll 注册 waker；路由安装/替换/撤销主动 wake 原 endpoint，不能依赖旧 relay 残包。此两问题由跨族真连接测试检出并修复。
- 旧连接保护：新 race 清 override 后，若赛跑失败，旧 relay 上原服务流仍能继续；同 relay 同逻辑地址再次赛跑使用真实信封路径。
- Rust 形态：无 Go 逐行移植；有限共享状态由路由表持有，端点借用由 Arc 表达，任务状态由 Driver 单线程持有，回退用 Drop。没有新增字符串错误协议或无消费者导出。
- 变更影响：新增 Hooks 为纯 std 回调面；测试 stable_id 观测字段用 cfg(test)，正式快照不增加键。

未发现未处置的高危问题。TLS-only 预筛只证明当前候选可达并通过 RPK，不作为路径采纳证据；
准入后的同连接 nonce 回显才是终验。预筛成功后到终验之间发生网络变化时仍可能使用最多
700ms 剩余预算试迁移，失败由原 relay 回退；预筛全黑洞不改变用户数据路径。

## 验证证据

新增可重复运行的真实 QUIC 回归：

- `admission_timeout_keeps_relay_backup`：首个 TLS 路径准入不答，中继后备同轮成功，只有一个准入成功。
- `credential_rejection_does_not_try_relay_backup`：明确凭证拒绝保留归因，中继无包。
- `in_flight_race_picks_up_dynamic_candidate_snapshot`：死址起跑后 200ms 学到新址，本轮约 0.24s 成功。
- `healthy_relay_upgrades_same_family_and_keeps_stream`：先失败候选后成功；同 stable_id；原流继续；Rebind 原流继续；新 race 失败后旧 relay 原流继续；同 logical peer 新赛跑真实包封。
- `healthy_v4_relay_upgrades_v6_direct_and_keeps_stream`、反向 v6→v4：同 stable_id、无新注册、旧流与换绑后读写继续。
- `healthy_single_v4_endpoint_learns_v6_direct_and_keeps_stream`、反向单族 v6→v4：初始缺另一族 Endpoint，仍能后学升级，原流/换绑继续。
- `forged_udp_cannot_confirm_upgrade_and_relay_stream_survives`：候选伪造/回放 UDP 不得确认，回退后原 relay 流继续。
- `blackhole_direct_preflights_do_not_pause_healthy_relay_stream`：三个黑洞预筛 2.1s 期间，原流持续往返，逐次小于 750ms，最终仍 relay，准入成功计数恒为 1。
- `hint_requires_configured_source_and_public_canonical_address`：伪源、私网、本地、多播、mapped 绕过负例；mapped 公网地址归一去重。

执行记录：`cargo test -p homeway-quic --lib` 末轮 **286 passed / 0 failed**（17.01s）；
同族/跨族/单族/旧流/换绑/失败回退/动态 DNS/准入后备均在其中。
测试使用回环独立端口/测试 state，未碰现役出口。

末轮补充 `race_outcomes_demote_timeouts_but_not_cancelled_backups`：整轮超时的在途黑洞候选
必须回报失败并进入宿主退避；认证成功后取消的后备不得被误记路径失败。定向 0.30s 全过。
`cargo clippy -p homeway-quic --all-targets -- -D warnings` 通过；`tools/check-quic-isolation.sh`
十一条断言全绿。


## 整批收口补记（主任务）

实现提交a7faa5ef88a8。统一workspace1216/0、QUIC289/0、core840/0；ci-local quick
通过（矩阵既有WARN/SKIP各1）。定向真实中继4/0、私有恢复4/0，Linux/OHOS check与
真核/HSP完成；最终源码的core32压力轮中继/学习均通过，有一个既有PTY超时，单独
复验通过，未声称压力全绿。见tier同日整改总记录。无生产部署或真机安装。
