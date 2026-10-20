# 中继整批集成交叉复核与补修（2026-10-21）

## 范围与方法

按主任务授权独立只读复核 relay/mod、ctlface、relaywire、server/bind/relayleg/engine/pubface、QUIC exit bridge/conn，以及真实 relay_admission_e2e。未调用 reviewer skill。发现后在 `/tmp/homeway-relay-cross.GMOFJe` 的隔离源码副本追加最小证伪测试，主仓业务源码在证伪阶段未改；主任务随后明确授权本子任务实施必要补修。

## 发现（均已处置）

### CR1 / P1：旧连接准入沿按 remote 查当前腿，能提升复用端口的新 cookie

复核版本 Reg4Request 仅带 remote。旧 QUIC 连接的 RREG 已进入异步 bridge 队列后，驱动线程可先处理 LegRemove/LegRegister，使同一中继数据端口映射到新的 sid/cookie；旧 RREG 稍后被接受，confirm_client(remote) 会把新代腿标成 client_admitted。控制重连历史表的 marker 比对并不能保护这条首次准入路径。

证伪：隔离副本 `old_reg4_remote_can_admit_recycled_port_new_generation` 按“登记 A → 保存旧 remote → 摘 A → 同 remote 登记 B → 确认旧 remote”直接复现 B 被提升。与 CR2 的证伪共 2/0。

补修：引擎注入每个腿包时携带 RelayGeneration{sid,cookie}；ExitSock 仅对真实 Initial 形状记录 `(peer,原始DCID) → generation`，首见不覆盖、最多 1024、TTL 20s，满表不淘汰在途证据。Incoming.orig_dst_cid 在异步握手开始前固定代际，随握手结果、连接控制流、Reg4Request 传到引擎。confirm_client 必须同时匹配 remote 与 sid/cookie；没有可靠代际不能提升。不能等 RREG 排队时才查询“当前 remote”。

回归：`stale_connection_proof_cannot_promote_recycled_port_new_cookie` 拒绝旧代、接受当前代；`initial_origin_is_first_seen_bounded_and_expires` 验首见不覆盖/容量/TTL/无效首包。

### CR2 / P1：4 个同 NAT 匿名包即可挤掉已 LEGUP、仍在客户端认证窗口的合法会话

复核版本预池按同 IP 最老项淘汰，不区分是否已收到真实出口 LEGUP。正常客户端尚在 TLS/RREG 网络 RTT 中时，4 个其它源端口的垃圾包就删除其分配 socket。已有正式会话确实不受影响，但“洪水后再建连成功”的旧 e2e 没覆盖“持续洪水中建立新会话”。

证伪：隔离副本 `four_anonymous_packets_evict_legup_authenticated_inflight_client` 精确复现，正常路径已真实通过 LEGUP，再注入 4 个同源 IP 不同端口小包即消失。

补修：新分配首包必须是 kind=5 的合理 QUIC v1/v2 Initial（长头、固定位、版本、CID 长度和至少 1200B）；已有会话不以明文头形状认证。已收 LEGUP 的在途另设保护池（全局 64、每 IP 2），只驻留原预准入绝对 10s，不随噪声续命；原匿名池仍独立保留全局 256、每 IP 4，新来源仍有机会，匿名轮换不能删除受保护在途或正式会话。受保护在途也不再与同 NAT 未知源共享准入 pps 桶。

回归：`four_anonymous_packets_cannot_evict_legup_authenticated_inflight_client` 保留在途且确认新来源可建分配；`malformed_new_client_does_not_allocate_or_instruct_exit` 拦无效首包；真实 e2e `new_client_admits_during_continuous_initial_flood_with_network_delay` 使用下行延迟 100ms 的 UDP 转发器，先确认服务端已回包、再持续发送 100pps 合法 Initial 形状洪水，新身份仍完成真实准入并能开服务流，原身份也存活。

保证限度：这是有界隔离和有限窗口保护，不是任意强度分布式 DoS 的可用性保证。持续足量、有完整 QUIC 形状的攻击仍可能消耗所有有限的匿名/在途名额；共享 NAT 无认证身份前也无法完美区分真实用户。文档不得再声称“匿名洪水永远不阻断新客户端”。预准入回收由已有扫描拍执行，10s 是不续命的资格窗口，物理回收还包含扫描延迟。

### CR3 / P2：TCP-only 注册形态首次 LEGUP 单包丢失后没有可靠重发

只有 TCP 控制注册、尚无 UDP 公共注册地址时，SESSION 仅通告一次，出口 register_leg 仅发一次 LEGUP。若该 UDP 首包丢失，客户端 Initial 永远留在 pend；中继 retry_udp_sessions 因 backend=None 无目标，不能救这条形态，首轮 5s 客户端赛跑会失败。

补修：出口腿保存 LEGUP 首包，在引擎已有 tick 中每秒重发，直到该腿真正收到中继转发的 QUIC 包；绝对最多 10s，重复同代 SESSION 和噪声不能重置起点。CLADM 的独立 ACK/最多十次重发保留。

回归：`lost_legup_retries_without_udp_registration_and_stops_at_absolute_deadline` 丢首包、验证第二次收到相同 LEGUP、重复 SESSION 不续期、到绝对期限不再发。

### CR4 / P1：同 remote 控制重放换 socket，QUIC 发送表仍保留旧 fd

复核时同步仅检查 q.has_leg(remote)。控制重放会关闭并重建同 remote 的出口腿 socket，而 QUIC 表仍持旧 socket 的 dup fd；中继已认可新 auth_src 后，回程从旧源端口发出会被拒绝。代际相同的控制重连也有此问题。

补修：同步同时比较 RelayGeneration 与实际 socket 本地地址；重拨同 remote 必须替换发送句柄。不是每拍无条件克隆所有 fd。

回归：`same_generation_new_socket_requires_send_handle_replacement` 验表差分；引擎集成 `sync_replaces_send_fd_when_control_replay_reopens_same_remote_and_cookie` 真起 ExitQuic，重放同 cookie 后调用生产 sync_quic_legs，证明新 fd 生效、旧 fd 不再匹配。

## 其它复核结论

- UDP 注册现在恒校出口 DH 私钥证明，PSK 只作使用授权，低阶公钥无贡献拒绝。
- LEGUP 与 CLADM 域分离，只有真实 QUIC RPK/TLS exporter 绑定的 RREG 裁决成功能产生客户端提升；sid/cookie 与当前分配匹配。
- CLADM 丢包/ACK 重发有界；控制重连只复用相同 sid/cookie 的认证代际，新 cookie 不继承。
- 握手逐次 read/write 重算剩余绝对期限，EstablishedPermit/HandshakeSlot RAII 覆盖失败、超期、交接丢弃与关闭；停机后短命握手最多等既有期限，唤醒 fd 无复用写入。
- 监听遵守显式 IP/地址族，通配两族明确建立；TCP 不可用能通过实际 UDP SESSION/LEGUP/RREG/CLADM 路径工作。

## 验证证据

- 隔离副本原缺陷证伪 2/0（测试断言“漏洞存在”，没有作为正式回归留主仓）。
- relay 原定向 35/0。
- core 新回归 4/0（代际/可靠 LEGUP/在途保护/坏首包），引擎换 fd 集成 1/0。
- QUIC 代际专项 2/0；完整 exit 定向 104/0。
- core lib 全量 839/0，5 ignored（运行时总计 844；主任务并发还有独立测试新增，最终总量以整批全门为准）。
- 真 relay + 真出口引擎 + 真客户端 e2e 4/0，13.50s（正式会话超预池寿命、UDP-only、坏凭证拒绝、持续洪水中新身份建连）。

## 技术门与代码门

开工前主任务批准上述代际设计与边界；实现遵循 std-only 公面类型、现有引擎 tick/注入队列，不引常驻线程，不变更 relay 线协议字节。完工后以实现者/审查者角色分离复查身份绑定、异步队列跨代、容量/TTL、有限重发、控制重放与 fd 所有权、字面量首包过滤的限度。Rust 形态使用 typed generation/Option、RAII 和现有封装；没有复制 Go 包结构或恢复退役栈。

`cargo clippy -p homeway-core -p homeway-quic --all-targets -- -D warnings` 与 `git diff --check` 通过。

本子任务未提交、未部署、未操作生产。整批全门见主任务收口记录。

## 统一全门后的 fixture 纠偏

统一 ci-local 曾在 `ctl_keepalive_echo` 报 TCP 127.0.0.1:52699 持续拒绝。旧 TestRelay 用 UDP :0 随机选口，却把 on_ready（只保证 UDP 已绑定）误作“同号 TCP 必可用”。UDP 与 TCP 端口占用独立；若随机 UDP 口命中并发 TCP 监听或 TIME_WAIT，生产实现按契约降到 UDP-only，增加 connect 重试时间不能恢复该控制监听。已有固定 TCP 冲突的 `probe_flags_report_ctl_degraded` 正覆盖这个反例。

补修只改测试：TestRelay 的自动端口先实际建立 TCP 监听（通配形态含两族），再确认同号 UDP 未退让，持有成对 guard 到 Relay 线程起跑前才释放，随后显式绑定该端口；固定端口的故障注入保持原样。真实 e2e fixture(false) 同样成对选 TCP/UDP 端口，fixture(true) 故意占 TCP 的 UDP-only 路径不变。没有改生产监听/退让语义，也没有增大重试掩盖问题。

验证：`ctl_keepalive_echo` 定向通过；真实 e2e 4/0（13.62s）；32 线程 core 全量中 relay 全部通过，但另暴露候选 Probe 单次刷新被全局 worker quota 合法拒绝导致测试 responder 超时的独立 fixture 问题（839/1），已提交给该模块 owner，整批全门再验。

Probe fixture 按后续观测事件语义修正后，再跑最终源码 `cargo test -p homeway-core --lib -- --test-threads=32`：relay 与 Probe 均通过；全量 839/1、5 ignored（67.19s），唯一失败为既有 `term::service::tests::attach_size_applies_to_pty` 的环境性 PTY 挂死 60s 硬期限，原文已上报主任务，未改该能力范围。`cargo clippy -p homeway-core --all-targets -- -D warnings` 与 diff 检查通过。本次 relay 测试选口补修已冻结。
