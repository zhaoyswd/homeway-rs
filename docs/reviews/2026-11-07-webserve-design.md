# 独立 WebServe 技术设计评审（2026-11-07）

状态：**技术门通过，允许按最终方案开工**。首轮B1–B8已修订，第二轮dsh实际exit=0通过，2026-11-08资源销账补充经独立核实；尚未实施，不将方案通过记为产品验收通过。

入口：`openspec/changes/independent-webserve/{proposal.md,design.md,tasks.md}` 与 `specs/webserve/spec.md`。

用户明确授权“生成最终方案、完成方案 review、review 通过后开发”。本次先完成技术门，收到通过结论和全部必改项处置后方可写产品代码；不需要再次向用户询问开发许可。仓库两道门由子任务完成，生产实例不碰。

## 已核源码事实

- TUI 依赖统一进程 control/term，本机 TermService 不依赖出口开关；legacy 前台单角色无此宿主。
- 当前 QUIC 无 ALPN；增加服务端 ALPN 会断开无 ALPN 的旧 QUIC 客户端，故采用首 bidi 两字节独立管理分流。
- 管理认证需独立确认中继腿首次 remote+generation，否则未准入腿 TTL 后回收；不得把管理连接注册为联网设备。
- 中继注册表私有，需增加仅本机可读的完整身份快照，并在 stop/drop 清空。
- 实际DEFAULT_CONFIG_TOML和FileServe默认均为出口/中继关闭；unified_cli的true事件串陈旧，实施时纠正，不引入不必要的启动分叉。
- HTTP/WS 将用成熟库，CLI 精确异步控制面白名单需要按判据变更登记及自校准，不放宽 core/capi 或数据面隔离。

## 首轮外评与处置

独立`dsh --profile headless`实际exit=0，完整原文及独立判断见[外评原文](2026-11-07-webserve-design-review.md)，原目录`/tmp/dsh-review/r1-webserve-design.ndsa6qdk/`。结论是“有条件通过，B1–B8阻塞开工”，并非技术门放行；task1.1保持未勾选。

- B1认同：改独立严格webserve.toml，旧config.toml/FileConfig不加新字段，补基线strict兼容验收。
- B2认同：新增protocol.md固定hww1、磁盘key、四帧管理认证/关闭码、全部PTY帧及HTTP/WS消息；夹具生成/全目录SUMS/判据先登记列入任务，未伪造已验向量。
- B3认同：RelayProc接真实就绪SocketAddr，status显示实际地址，验证退让/:0/wildcard，不扩大产品端口0输入。
- B4认同：在途登录+票据+活动界面共享8个RAII名额；未认证WS另8个短命额；票据持连接，过期/失败/目标关闭/轮换/relay epoch变化回收，消费再次验证。
- B5认同：confirm_client类型化四结果；LegMissing仅原generation/原预算内20ms重试，其他失败立即拒，成功后Accept。
- B6认同：HTTP头16KiB/32字段/5秒、体4KiB/5秒、总15秒且禁keep-alive；WS线级帧/消息64KiB，首帧2KiB/5秒，活性与背压期限明确。
- B7认同：统一进程恒装配worker+可空live WebAuth，首次token经RPC热install，不重启出口、不启HTTP。
- B8认同：管理TUI强制--no-spawn，PTY非阻塞poll取消，SIGHUP→500ms→SIGKILL→有界wait，不遗留守护。
- P1认同：本记录关联完整原文、真实退出码及处置，待独立复核后才标通过。
- P2认同：订正默认false的源码事实，任务包含改陈旧日志及真实默认配置验收。
- C1认同问题并采用完整覆盖：Handover/CtlConn保留握手已验pubkey，合法ctl-only可见，不一律排除。
- C2–C12认同：已纳入design对应段、protocol和tasks，覆盖目标最终认证、共享TLS前资源边界、HTTP/agent状态、精确依赖门、PTY取消、秘密擦除、origin归一/HTTP1回源、额外流拒绝、判据夹具、平台/旧守护、Rust锁边界。

## 独立复核与最终放行

第二轮`dsh --profile headless`实际exit=0，原目录`/tmp/dsh-review/r2-webserve-design.r9l79m3u/`，明确结论“技术门通过，可开工”；完整原文见[增量复核](2026-11-07-webserve-design-r2.md)。B1–B8在规划层闭合，无新阻塞；五项非阻塞检查已显式纳入设计和任务。

2026-11-08审后资源销账补充位于design第5.1节和tasks3.4：conn_id lease活位RAII、≤200ms有界sweep、固定代际network_owned/管理owner集合、gateway关闭cutoff+epoch/精确源地址/created门，以及40轮连续登录关闭和普通联网owner保护。独立评审者已逐项核实，与probe接口约定一致，并在上述r2记录追加“最终技术门结论：通过，允许按本方案开工”。

本门据真实评审结果放行开发，task1.1可勾选。OpenSpec strict实际通过；产品代码尚未开始，下一步由主会话先提交规划单元，再按分工实施。最终代码门、真实测试与部署许可不由本门替代。


