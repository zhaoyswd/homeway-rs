## 1. 技术方案与评审门

- [x] 1.1 完成独立技术评审并处置全部阻塞意见；验证 `docs/reviews/2026-11-07-webserve-design.md` 有实际 reviewer 退出码、完整意见与通过结论，且 OpenSpec strict 校验通过。

## 2. 实例凭证与本机管理

- [ ] 2.0 产品代码前先在INTEROP-CRITERIA登记W-web新增协议/行为常量及CLI精确异步控制面；验证登记完整且无把规划写成已验，随后才进入2.1–4.3。

- [ ] 2.1 实现独立持久 Web 凭证及 hww1 编解码、离线身份初始化与轮换；以稳定/损坏/权限/并发/不启监听、可擦除敏感缓冲、日志不含秘密、凭证类型隔离测试验证；control未就绪但实例锁占用时禁止离线写入，验证在线缓存/磁盘不分叉。
- [ ] 2.2 实现固定当前二进制 argv 的 TUI PTY 管理、OPEN/INPUT/RESIZE/OUTPUT/CLOSE 帧和有界取消；以真实 TUI 私有实例验证尺寸、输入、PTY背压取消/SIGHUP→500ms→SIGKILL收尸、逐字节顺序/输出延迟、--no-spawn覆盖全部后台拨号、控制断开/父进程退出无孤儿及工作会话保留。

## 3. QUIC 管理承载与中继准入

- [ ] 3.1 在首 bidi 增独立管理魔数、nonce/exporter 绑定认证及可撤销 grant，不改 ALPN/联网 reg4；通过正错 secret、错 RPK、重放/超时、轮换及原联网准入回归验证。
- [ ] 3.2 实现独立管理客户端和单流 socketpair/pump 同步边界，复用中继封装；验证管理流不创建设备、普通 tag/DATAGRAM 不可达、连接取消释放资源。
- [ ] 3.3 实现ManagementRequest固定remote/generation与Confirmed/LegMissing/GenerationMismatch/CapacityFull回执，LegMissing限时重试不切新代；Handover/CtlConn保留已验完整公钥、RelayProc暴露实际SocketAddr和epoch、快照收合法UDP/TCP认证；验证注册/Hello交错、配额满/错代、pending不可见、ctl-only可见、过期/stop清空、端口退让/:0/wildcard及超过10秒真实交互；验证CapacityFull映射0x75、worker/入口满映射0x73以及8管理与formal32交互。
- [ ] 3.4 实现conn_id ManagementLease alive RAII及≤200ms失活sweep，固定代际network_owned/管理owner集合销账；实现gateway关闭cutoff时刻+relay epoch/精确AssocKey/created门的本机release队列，不改线上RELEASE方向；验证40轮连续登录关闭、其它管理/普通联网owner保护、同代移交/跨代晚清理、端口复用和满队列的精确回收。

## 4. Web 入口与统一进程

- [ ] 4.1 实现独立webserve.toml/命令/控制词表及HTTP/agent分列状态；订正unified_cli陈旧default=true事件串，验证生成默认配置及普通启动均不装配出口/中继、已有角色保持、同配置start already/不同配置start无副作用拒绝、restart重绑/端口冲突、HTTP停用不影响agent、先启出口后首次token热安装不重启，以及基线旧严格config读取兼容。
- [ ] 4.2 实现仅127.0.0.1的 HTTP/WS、Host/Origin、限流、限时首帧和一次票据；验证默认端口归一/HTTP1.1回源、异站/坏Host、slowloris/头体总期限、WS线级首帧前后64KiB上限、重复/过期票据、在途+票据+活跃总8与第9/32次拒绝、失败/OPEN/目标关闭/rotate/relay epoch清理、未认证WS不建PTY及多标签目标隔离。
- [ ] 4.3 内嵌固定版本 xterm.js/fit 和许可证、登录/终端/断线页面；用真实浏览器验证中文输入、宽字符、鼠标、复制粘贴、resize、关闭重连和工作会话保留，不以静态页面截图冒充交互通过。
- [ ] 4.4 在2.0前置判据登记后，按protocol.md生成fixtures/webserve token/握手/PTY正反向量并更新MANIFEST及全目录SHA256SUMS；添加反代说明，运行词表/隔离正反自校准及CLI features无rt-multi-thread、CAPI闭包无axum/hyper-Web依赖验证。

## 5. 集成验收与代码评审

- [ ] 5.1 使用显式/tmp state的统一进程完成本机Web-only、出口无HTTP经中继、多目标拒绝/隔离、token轮换与中继重启烟囱；包含正确pubkey错误secret/目标轮换拒绝、先启出口后首取token无HTTP、注册/HELLO交错、票据/首帧失败/过期/目标关闭/relay重启、父进程退出无孤儿、40轮登录关闭的assoc/正式腿/PTY回归基线、v4/v6 wildcard实际地址选择；以真实HTTPS/WSS反代与目标TUI进程/工作会话证据验证。
- [ ] 5.2 运行相关单测、workspace测试/clippy、ci-local适用门和OHOS/musl/Windows交叉检查，覆盖协议错magic/版本/长度/重放、管理额外bidi/DATAGRAM与共享握手压力回收后普通准入、Windows全部webserve unsupported/旧daemon unknown_op升级错误不拉第二守护，记录实际结果与已知基线失败；不以未运行项标通过。
- [ ] 5.3 完成独立代码评审（含Rust惯用形态、逐行移植痕迹、窄句柄/锁内零IO、错误类型/借用解析、资源/取消/认证边界）并整改复核；验证docs/reviews有完整门记录，更新任务与ROADMAP后按主会话安排本地中文提交。
