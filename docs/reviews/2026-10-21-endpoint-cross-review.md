# 2026-10-21 端点学习专项交叉复核

## 独立只读复核（QUIC 实现子任务）

范围限定为 `facade/endpoint_candidates.rs`、`daemon/hosts.rs` 及 VPN/HostSession 装配。
复核时未改业务代码，未使用 reviewer skill，未碰生产实例。重点检查锁顺序、同 peer 换 token
的 DNS 晚到、全局八 worker 额度和任务退出，以及异步候选是否实际用于建连。

### 已报告并交给实现方的真实问题

1. **P2：瞬时 worker 饱和可被误判为永久没有端点。** `refresh(true)` 在 ACTIVE=8 时保留
   pending_dns，但置 busy=false 即返回。原 `initial()` 仅当 busy=true 才等待，因此第九个纯
   域名表可立刻得到空候选；`host_session::start` 与 `tun_exec::start_quic` 又在建岛之前将
   空表硬拒。此时没有岛，也没有后续 refresh 入口，短暂容量竞争变成永久构造失败。
   建议在 initial 自身总预算内等待/重试 worker 额度；或允许持合法域名目录的冷表先建岛，
   由首次 race 的动态 snapshot 续接。已即时报告主会话与 endpoint 整改实现方。
2. **P2：首次域名装配丢失关键解析档。** HEAD 的旧 `host_session::resolve_endpoints` 调用
   `ResolveLane::Critical`；新表 `initial()` 统一走 `refresh(true)`，worker 则固定使用
   Background。hostdns 明确分隔四 Critical / 四 Background 配额，系统 resolver 超时脱离后
   仍持额度。四个后台解析挂住时，新服务/隧道即使关键配额空闲，也可能冷启动失败。
   建议仅冷表首次请求带 Critical 优先级，恢复/周期仍 Background，并保持原总预算。
   已即时报告主会话与实现方，和问题 1 分开处理。

本节记录发现时的代码事实；最终修复和回归证据由实现方在下一节补录。

### 通过的专项核查

- **表锁顺序没有发现反向环。** registry 只在 open 时持有，再进入 domains/entries；后台
  不反拿 registry。apply_dns 为 domains→entries，insert/outcome 为 entries→current；
  未发现 current→entries 或 entries→domains 的持锁路径。save 克隆 entries 后释放锁再
  做磁盘 I/O；sinks 内只执行非阻塞命令投递，不等岛回执。
- **DNS 晚到保护有效。** apply_dns 取并持有 domains guard，确认 `(domain, relay)` 仍在
  当前目录才应用。add_token 先替换目录再清未验证旧 token 地址；并发旧结果要么先应用后
  被清理，要么看见域名已撤销而拒绝，不会撤销后又把旧域名结果放回。
- **同 peer 的认证缓存不等于凭据。** 表按 peer/cache 路径共享，旧认证地址可留作候选；新
  会话仍使用当前 token 的 RPK、secret、设备身份完成准入，不能据缓存跳过认证。
- **线程退出大体有界。** worker 的 Done 守卫归还 busy/ACTIVE；spawn 失败亦归还额度。
  hostdns 的底层 getaddrinfo 可能脱离调用方超时，但其自持配额直至真正退出，避免伪造回收
  造成无界 resolver 线程。dirty 在 save 前交换，持久写盘失败不自发无限起线程。
- **模式接线实际存在。** daemon 向 HostSession 传 endpoint_cache_dir；HostSession 与 VPN
  均 open 同表、挂 candidates.hooks、bind 岛，并准备 v4/v6 两族 endpoint。首次显式名单与
  后续空名单重连都由 hooks 补当前快照；race 100ms 纯读新快照可本轮采纳晚到 DNS。
- **令牌刷新装配没有新锁环。** daemon add_record 在 change_mu 内落新记录、锁外停旧会话
  再起新会话；HostTable.inner 不跨候选表的网络等待持有。candidate 回调不反拿 daemon
  change_mu/inner。

本次是源码交叉审阅；上述两项问题按明确控制流和既有 hostdns 配额契约确认，未为了复现
而在共享业务文件加入测试缝。候选实现方负责修复与对应回归。

## 第二路交叉复核与实际反例（中继实现子任务）

本路先按只读角色复核，再由主会话明确授权修复 `endpoint_candidates.rs` 与必要依赖
`probe.rs`；未使用 reviewer skill，未修改其它实现文件。

在 `/tmp/homeway-endpoint-cross.BS6PGV` 隔离副本追加证伪测试，确认原实现三项问题：

- **P2：满 64 条旧已验证缓存会挡住当前 token。** 原 insert 的满表淘汰只接纳未验证弱来源，
  当前 token 的新地址会被静默丢弃；同 peer token 刷新仅追加目录，未验证旧 Token 项也会永久占槽。
  `cross_full_verified_cache_cannot_block_current_token` 实际失败。
- **P2：写盘单飞期间丢失 DNS 请求。** refresh(true) 遇到 busy 直接返回，没有承接标记；
  worker(false) 结束后仅检查 dirty，不会再解析当前恢复轮所需域名。
  `cross_dns_request_during_writer_busy_is_not_lost` 实际失败。
- **P1：垃圾包可无限延长探测，从而占死全局学习 worker。** 学习 worker 新接入的 ping_ex
  只设置单次 recv 超时，收到无效包后重新 recv，不检查绝对 deadline。配置端点持续回垃圾可
  将该 worker 和它持有的 table/sinks 长期保留，八个表即可占满全局额度。隔离回归
  `cross_probe_junk_does_not_extend_absolute_budget` 在 60ms 预算下实测耗时 556ms；原测试失败。

主会话另报告同一域名多轮漂移积累 64 条已验证 Source::Token 记录会再次挡住新 DNS 快照；
此变体与初始缓存容量问题同根，但须同时保护当前 token 的字面量地址，不能一律降低旧地址来源。

## 整改与回归（已冻结）

1. **当前目录优先。** 同 peer 刷新 token 替换域名/字面量目录；旧未验证 Token 项清理，旧
   已验证项降为七天 VerifiedPath 回落缓存。当前 token 可淘汰旧缓存；最近真实采纳路径优先
   保留。旧域名的异步结果在 domains 锁内核对目录后应用，撤销后不能被晚到结果重新加入。
2. **DNS 快照优先。** 不在新 DNS 快照中的旧已验证结果降 VerifiedPath；当前字面量目录独立
   记录，重叠地址恢复为字面量 Token 来源和永久目录寿命。relay/direct 同名域分别处理。
3. **单飞请求承接。** pending_dns 合并 busy 时到达的解析请求，Done 归还槽后至多承接待处理
   请求；DNS/保存失败本身不置 pending，不产生自行重试风暴。持续落盘失败的回归确认 worker
   结束、dirty 消费且最后 table Arc 释放。
4. **冷启动争槽与分档。** 首次无候选在原三秒总预算内短拍争取全局 worker，pending 仅置一次，
   不因瞬时额度竞争立即 NoCandidate。持续额度不足仍按原 deadline 返回。纯域名首次请求走
   Critical 池，恢复/周期刷新保持 Background；四个 Background 名额真实占满时仍能冷启动。
5. **探测严格有界。** ping_ex 每次 recv 前重算同一绝对期限，核对配置目标源，并归一 IPv4
   mapped 地址；随机源失败返回 IO 错误。持续垃圾不能延长预算，已知 nonce 但不同端口的伪造
   应答也不能污染候选列表。

最终定向回归：

- `cargo test -p homeway-core --lib endpoint_candidates --no-default-features`：**20/0**，0.60s。
  在原 11 条上新增九条，覆盖目录替换、满验证缓存重开、DNS busy 承接、撤销域名晚到、
  持续写失败/Arc 释放、暂时与持续 worker 饱和、Critical 分池、64 条旧 DNS 挡新快照变体。
- `cargo test -p homeway-core --lib probe::tests --no-default-features`：**6/0**，0.56s。
  新增持续垃圾绝对期限，以及伪造源拒绝与 mapped 目标合法探测。
- `git diff --check`：通过。core all-targets clippy 已尝试，最后一次被并行 QUIC 改动的
  `exit/socket.rs` origins 字段 type_complexity 阻断；已通知对应实现方。整合后的全仓 clippy
  由主任务统一收口，不能将本条写作最终通过证据。

## 收尾代码复核

复核者重新核对：domains→literals/entries、entries→current，无反向持锁路径；文件 IO 在
克隆快照后执行，0600/create_new 临时文件与 rename 保持；同精确 peer/cache 路径的内存表共享，
registry 只持 Weak。table 保存的 IslandTx 仅持命令通道，不持 Island/Face/hooks，因此不构成
Arc 环；岛 stop 显式终结接收方，后续 worker 的非阻塞 send 失败即摘 sink。DNS worker 总预算
三秒、probe 总预算 250ms，底层 libc resolver 自持分档名额直到退出；修复后垃圾不能延长
上层 worker。失败冷却仍是最多 64s 元数据，跨会话不复活未验证地址；所有候选仍真实 RPK/准入。
代码没有新增常驻线程、无额外 runtime、无 Go 包树/接口直译痕迹。上述明确发现均已整改，
本范围未留已知阻断项；不继续主动扩大审查面。生产实例未操作。

## 消费卫兵与探测覆盖末轮整改

主会话最后复核指出：新 worker 直接使用 `ping_ex().endpoints`，绕过原有 `probe_candidates`
的消费卫兵；仅靠 valid 不足以拒绝明文 Probe/Hint 提供的私网、回环、fake-IP 与 CGNAT。
现已在唯一 learn 消费入口对 Source::Probe/Hint 同时调用 valid 与 probe_addr_acceptable；
Token/DNS 的显式 LAN 地址保留。卫兵补齐 0/8、>=224 的保留/多播/广播、IPv6 unspecified，
映射 IPv4 先归一。通用假线索测试改用公共 8.8.*；真实本机 UDP responder 返回公共端点列表，
不再用“可接受回环 Probe”固定错误预期。新增私网/映射/fake-IP/CGNAT 消费拒绝回归。

原实现在刷新时只探测前两个 Token 地址，与候选全集巡检契约有偏差；本轮改为**所有当前有效
直连候选公平轮转，单轮最多两个，总计 250ms**。每个目标按剩余数量分享预算，首个黑洞不能
持续吃掉第二个目标的预算；中继地址不当出口 probe。新增回归确认非 Token 有效候选也轮到，
三轮覆盖五个有效直连地址；主会话据此同步 spec 的有界轮转口径，不声称一次刷新并发扫全集。

最后源码冻结后的定向验证：候选 **22/0**（0.58s），Probe **6/0**（0.55s），
`git diff --check` 通过。所有新旧候选仍须经过真实 TLS RPK/注册准入；明文 Probe 与 Hint
只提供经过地址卫兵过滤的未验证线索。全仓 clippy/交叉构建与真机包检查仍由主任务统一收口。
