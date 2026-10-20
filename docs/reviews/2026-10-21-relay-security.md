# 中继安全与控制面整改（2026-10-21）

## 技术自评（开工门）

重新核对当前 HEAD：UDP PROOF token 模式仍只校 PSK；read_exact/write_all 内部未逐次收紧期限；TCP 恒绑 IPv4 通配；匿名 assoc 与正式配额混用；200pps 位于全数据面入口。这五组问题仍成立，前批 relay worker 所有权与 join 修复必须保留。

设计：DH 两模式恒校且拒绝非贡献密钥；握手逐次系统读写按同一绝对期限收紧，established 令牌由 RAII 从握手交给连接；TCP 显式地址按配置族绑定、通配分别绑定 v4 与 v6only 两枚 listener（macOS dual socket 可与既有 v4 listener 同号共存，不能据 dual bind 成功宣称两族覆盖）；匿名会话独立短 TTL 有界预池，只有出口完成客户端 QUIC 准入后发域分隔认证标记才提升正式池；数据面按会话字节预算，200pps 仅用于新来源/控制面准入。

评审者检查：边界包括共享 NAT、预池洪水、未知 marker、过期标记、清理与控制重连；并发态仍由单驱动线程独占，握手只交接令牌；不改既有 wire 字节，新增可忽略的 marker 子协议；错误面随机源失败/读写失败均回收；不引入额外锁或逐行移植形态。提升证据必须来自客户端 token/RREG 成功后的出口事件，LEGUP 与正常 QUIC header 均不足以提升。通过，按上述设计实施。

## 代码自评与验证

F1/F2/F3/F4/F9 均重新复核后实施。主要实现：

- UDP/TCP DH 恒校，拒绝 non-contributory X25519；token PSK 只授予中继使用权，不能代替出口私钥。
- 预准入：中继全局 256 / 每源 IP 4，10s 绝对寿命，满池仅淘汰最旧预准入；正式每出口 32 / 全局 1024 另计。出口侧 64 条独立预准入腿池，满时不驱逐已准入腿。
- 仅 QUIC RREG 成功后 `confirm_client` 发 CLADM（sid/cookie/HMAC 域分隔），ACK 前每秒重试、最多 10 次。LEGUP 只是出口身份，不提升正式配额。UDP-only 通过带 MAC 的 UDP_SESSION→LEGUP 同一闭环，不再有原始转发先于准入腿登记的竞态。
- 控制重连仅在完整 admission marker（绑定 sid/cookie）相同时保留真准入；最多 32 条 / 3min，RELEASE 和腿死亡删除，remote 端口复用不继承旧认证。
- 200pps 限制仅控制/未准入流量；正式上行按会话 16MiB/s 字节预算，与下行同量级，不与共享 NAT 的其它客户端共用数据 pps 桶。
- read/write 每次系统调用重算同一个 10s deadline；established RAII 令牌随 Handover/CtlConn 移动，失败、消息丢弃及关闭均归还。
- 出口从中继主口接 hint；公共 socket 按目标 30s 限频、64 目标有界，只发送 3 个 8B 打洞包。hint 不能当认证；映射 IPv4 先归一再过滤 loopback/unspecified/multicast。
- 自评额外修正：`reap_assoc` 删除 fd 必须置 `poll_dirty`，避免空闲后 poll 持已关闭 fd 造成 POLLNVAL 忙转。TCP/UDP 统一会话状态后删除不可达 sid=0/raw fallback 降级分支。

按评审者 checklist 逐项检查：wire 原有黄金字节未改（新增前向兼容消息）；预池/正式池边界分离；同源 NAT 不争正式数据桶；MAC 域不互替、旧 cookie 不可提升新会话；所有 mutable 状态仍归单驱动；线程 stop/join 原有回归保留；没有新增常驻线程/多余锁/Go 式接口仿写。实现者整改与评审者复查结论：通过，未留已知阻断项。

已实跑：`cargo test -p homeway-core --lib relay --no-default-features` 52/0；`cargo test -p homeway-core server::bind --no-default-features` 17/0（含预池洪水保正式、marker ACK/retry）；随后 `cargo test -p homeway-core --lib server::bind::tests --no-default-features` 9/0（另含同代重放/异代不继承）；`cargo clippy -p homeway-core --all-targets --no-default-features -- -D warnings` 通过。relay 全目标筛选同时跑到 `relay_admission_e2e` 1/0（13.4s，跨 bootstrap 寿命）与 `relay_vectors` 2/0。slow-drip 新回归使用有效帧头/正文、180ms 总预算，不能被旧的非法长度门误判为通过。新增吞吐回归在同一窗口递送 500×1200B，确认不是 200pps 封顶。

未碰生产、未提交/发版；工程整合/全仓/交叉目标与 App 换装验证由根任务统一收口。

兼容边界：新中继要求 CLADM，新出口可对接旧中继；旧出口无法为新中继提供真实客户端准入证据，停留短命预池，因此升级应先出口再中继。本批不部署，不声称新中继兼容未实现 CLADM 的旧出口。
