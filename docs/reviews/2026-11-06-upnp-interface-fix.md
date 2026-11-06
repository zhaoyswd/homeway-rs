# 双网卡 UPnP 来源与 QUIC 绑定修复

范围：仅本地产品修复、测试与评审；基线 cdff8f1。本子任务不操作现役 .84、不使用SSH/SCP，不正式部署或发布；主会话可独立进行 /tmp 私有IGD补验，不替换正式安装。
诊断依据见 `2026-11-06-lan84-upnp.md`，不改正常 LaunchAgent 启动链路。

## 技术设计（第一道门输入）

- `UpnpContext` 保存本地 IPv4 与该地址所属接口；发现时设置，描述 GET 和全部 SOAP
  从同一地址/接口发起，保留 SSDP 来源对主机名的钉定、原 HTTP Host、期限/取消/体积闸。
  TCP 绑卡失败终止该候选，避免控制面继续跨卡。
- 扩展现有 publication 锁的状态，保存运行期 QUIC 接口、世代、成功映射来源/外口；
  初始化仅使用 socket 成功钉卡事实及显式 bind 地址。自动或显式接口仅在当前接口
  的实时 IPv4 集合里选择；显式 IPv4 仅用它本身；none/unbound/钉卡失败或纯 IPv6
  没有安全 IPv4 候选，失败可见，不能退回枚举首项。
- 驱动重钉与世代更新在同一 publication 门内串行。每轮观测捕获世代；UPnP 内存结果、
  cache/探针/token 发布均在门内核对世代和停止位。成功换卡即使返回旧卡也增加世代；
  失败不谎报新卡。保留已有停止隔离。
- 退出保留所有世代成功或可能成功请求的原IGD/来源/内外口收据，不重新发现；完整表中
  精确核验后缩租，不能认领同机另一接口或另一实例的映射。所有记录共享8s；40s等预算及wire/token不变。

## 风险与验证计划

- 内核 pin 跨平台差异：沿用 egress 的统一 socket pin，并运行可用交叉 check。
- 回环不等于真实IGD：主会话已独立用修订前及最终私有件完成真实.84/en7及auto各两轮完整QUIC补验；
  正式GitHub Release仍未发布/部署/验收。nonce回答本身不等于完整QUIC成功。
- 回环 mock IGD 检查 GET/List/Add/Delete/短租的对端源与 InternalClient；主机名 Host
  保持和 DNS 边界；候选选择、初始失败、换卡及旧轮不发布；运行 core tests/clippy。

## 评审与验证结果

技术门、实现、专项终验及代码门原文/整改分别记录如下。

## 第一道门原始意见（dsh exit 0，完整原文）

## 评审结论

**方向通过，设计门未闭合。** 根因判断成立：当前 `discover_igd_context` 的 SSDP 绑了候选本地地址，但 `connect_within` 的 TCP 走系统默认源，确实会造成 SSDP 源、SOAP 源、`NewInternalClient` 三者不一致。设计把 SSDP、描述 GET、全部 SOAP 固定到同一本地 IPv4/接口，并让候选受 QUIC 运行期接口约束，主线是对的。

但设计仍有 **2 高、9 中、3 低**。建议先补 H1/H2，以及 M1/M2/M5 的状态与验证闭环，再开工；方向不需要推翻。

范围核对：基线 `cdff8f1`；四个产品文件无改动；`git status` 仅显示设计文档未跟踪；无 openspec change。本轮只读，未跑 cargo、未写文件。

## 逐条问题

| 级别 | 位置 | 问题 | 建议 |
|---|---|---|---|
| **高** | 设计 L15-17；`upnp.rs:766-789`、`upnp.rs:1156-1184`、`engine.rs:2141-2148` | **世代门只挡发布，不挡 UPnP 副作用。** 换卡/停止若发生在 `clean_mappings`、`AddPortMapping`、`DeletePortMapping` 之间，旧卡映射已经写进路由器；后续按新 `local_ip` 清理时，该条目会被 `classify_mapping` 判为 `Foreign`，不会删除。0 租期回退时还会永久残留。停止路径同样可能留下未记录的刚建映射。 | 把 generation/stop 检查下沉到每次 Add/Delete/re-add 之前；Add 成功后立即记录精确元组；若发现世代已变，按“ext+UDP+internal client+internal port+desc”精确删除本轮刚建的条目；停止时至少保留记录供缩租。补“延迟 Add 响应期间换卡”的 mock 测试。 |
| **高** | 设计 L9-10；`upnp.rs:220-225`、`egress.rs:371-443`、`doh.rs:495-498` | **“TCP 绑卡失败终止候选”没有区分源地址 bind 与接口 pin。** `upnp.rs:225` 自己写明 Linux 无 `CAP_NET_RAW` 时 `SO_BINDTODEVICE` 会 EPERM；SSDP 和 DoH 都选择降级继续。若 TCP pin 失败就终止候选，普通用户 Linux 上 UPnP 会全灭，而真正修复 402 的其实是源地址 bind + `NewInternalClient` 一致。 | 拆成两个动作：`bind(local_ip)` 失败 = 候选失败；`pin_iface` 失败 = 记可见告警后继续（源地址已固定），或明确选择 fail-closed 并把 Linux 权限要求写进启动日志/文档/测试。不要用一个“绑卡失败”覆盖两种语义。 |
| **中** | 设计 L8-9、L26；`upnp.rs:590-613`、`upnp.rs:766-789`、`upnp.rs:1725-1739` | **`source == NewInternalClient` 不变量没有类型化，测试计划也不足以证伪。** `add_with_lease` 仍单独接收 `internal_ip`，与 `UpnpContext` 的本地源是两个口子；若只绑 TCP 源却传错 `internal_ip`，仍会 402。回环 mock 若只绑 `127.0.0.1`，默认源也是 `127.0.0.1`，删掉源绑定测试仍绿。 | 让 `UpnpContext` 持有唯一的 `local_ip`，`Igd::add_*` 从 context 取 `NewInternalClient` 或 `debug_assert_eq!`；测试注入 connector，或构造“默认源 ≠ 选定源”的双本地地址用例，并同时断言 GET/SOAP 的 peer source 与 `NewInternalClient`。 |
| **中** | 设计 L11-17；`engine.rs:841`、`engine.rs:2314`、`bindwatch.rs:211-224`、`pubface.rs:136-149` | **运行期状态分散，世代与候选快照可能不原子。** `pinned_flag`、`pin_iface`、`TokenCtx.quic_ep`、新 generation/映射记录是四份状态；`repin_to` 先改 `pin_iface`，watcher 之后才置 `pinned_flag`，pubep 可能读到“新接口+旧 pinned”或“旧候选+新世代”。 | 收敛为单一 `PublicationState { generation, iface, pinned, mapping, published }`，全部在 publication 门内更新；每轮开始时一次性快照 `(generation, iface)`，再从该 iface 枚举地址；`pinned` 由状态派生，不再单独维护。明确锁序 `publication -> pin_iface`，且锁内不做网络 IO。 |
| **中** | 设计 L17、L19；`bindwatch.rs:136-161`、`bindwatch.rs:211-231`、`engine.rs:1200-1226`、`engine.rs:1237-1252` | **失败、超时、停止三类在途竞态没有闭合。** 地址漂移后 resolve/repin 失败时，旧映射可能已失效但仍会公布，且不触发 `on_change`；`repin` 10s 超时后命令仍可能被驱动线程迟到执行；watcher 未 join，CLI 的 `stop_public_observation → shrink → shutdown` 顺序下，shrink 可能与 repin 并发。 | 地址已从旧 iface 消失时立即失效旧映射并 kick；`Repin` 带 epoch、幂等，迟到成功也要收敛到共享状态；停止序先停并 join watcher，再 shrink；补“地址删除 + repin 失败/超时”测试。 |
| **中** | 设计 L11-14；`engine.rs:363-425`、`engine.rs:2133`、`upnp.rs:1296-1310`、`bindwatch.rs:149-160` | **候选集生成规则不完整。** 同一接口多个 IPv4 时没有确定性顺序；`BindMode::Addr(0.0.0.0)`/公网/非单播地址会被“显式 IPv4 仅用它本身”直接当 `NewInternalClient`；`--bind-interface none` 从“仍可枚举映射”变为“不映射”，这是用户可见行为变化但没有专门日志/测试；接口身份同时有 index/name 两套。 | 定义 `CandidateScope::{BoundAddr, Iface, None}`；接口按 name 优先实时回查；地址去重、排序；拒绝 unspecified/multicast/broadcast；none/unbound/pin 失败/纯 IPv6 打不同原因行；补多地址、none、显式非法地址测试。 |
| **中** | 设计 L11、L18-19；`engine.rs:1342-1347`、`upnp.rs:947-961`、`upnp.rs:1094-1111` | **缩租拿不到“本世代成功映射”的记录，且当前查找口径会误认领。** `shrink_upnp_lease` 只拿 `engine`，重新枚举全部候选；`find_our_mapping` 是“同号优先，否则第一条我们的”回退，不是精确 tuple。`local_port=0` 时还可能把同 IP/前缀、未被监听的历史映射认成自己的。换卡成功后旧接口映射也没有回收规则，0 租期会永久残留。 | 在共享状态里保存 `MappingRecord { generation, local_ip, ext, internal, proto, desc }`；shrink 只用该记录、只沿该 local_ip 发现；新增精确查找（ext+UDP+client+port+desc），表不完整或无记录就跳过；换卡成功时对旧记录做精确删除或明确登记“留到过期”。 |
| **中** | `upnp.rs:1005-1016`、`upnp.rs:793-807`、`engine.rs:1359-1361`、`upnp.rs:2206-2240` | **缩租的 718 路径可能删掉唯一可用映射，日志还说“原映射仍在”。** `re_add_short_lease` 先 `DeletePortMapping` 再 Add；若 Delete 成功、Add 失败，映射已消失。现有测试只用 `delete_noop=true` 模拟“加不回去”，没有覆盖 Delete 成功 + Add 失败。 | 失败时尝试按原租期恢复；无法恢复则返回独立 `Lost` outcome，日志如实说“映射可能已删除”，不要复用“原映射仍在”。补 Delete 成功/Add 失败用例；`add_port_mapping` 的 718 分支同型，建议同批处理或登记。 |
| **中** | 设计 L8-9、L19；`engine.rs:2159-2166`、`upnp.rs:1141-1150` | **WAN IP 的第二次发现可能打到另一个 IGD。** `ensure_port_mapping` 只返回 `(ext, local_ip)`，`refresh_quic_public` 随后重新 SSDP 发现并取 `external_ip()`；同一本地源仍可能收到另一台 IGD 的应答，且重复消耗共享预算。 | 让 mapping round 返回 `Igd`（或 `wan_ip`）并复用同一 `UpnpContext`；不要为了 `external_ip` 再发现一次。补“同 LAN 两个 IGD 应答”测试。 |
| **中** | 设计 L8-9、L23-27；`upnp.rs:590-613`、`doh.rs:138-156`、`deadline_io.rs:14-67`、`INTEROP-CRITERIA.md:1284` | **本地 IPv4 与解析出的 IPv6 控制目标不兼容。** `connect_within` 会遍历 `resolve_system` 返回的所有地址；若先拿到 `::1`/AAAA，后续源地址 bind 无法成立。`connect_timeout` 也不轮询 stop，最长约 3s；这与现有判据一致，属于可接受上界，但设计应写明。 | 有 `local_ip` 时只接受 IPv4 目标；无 IPv4 地址则明确失败，不要回退为未绑定连接。用 socket2 在 connect 前 bind+pin；若要求严格 50ms 停止，则改非阻塞 connect + poll，否则保留并登记 3s 上界。补 `localhost`/IPv6 LOCATION 测试。 |
| **中** | 设计 L15-16；`engine.rs:2117-2185`、`engine.rs:2405-2435`、`engine.rs:2630-2651`、`engine.rs:2717-2719` | **`quic_pub` 与 `last_published` 不是一个原子提交。** UPnP 成功会先写 `quic_pub`；若后续 STUN 证据合流拒绝公布，`publish_endpoints` 不执行，但 token 兜底线仍可能把 `quic_pub` 带进 token，形成“融合说暂不公布、token 却公布”的矛盾。 | 把“映射结果、`quic_pub`、`last_published`、探针端点”放在同一次 publication 提交里；只有融合成功才一起写入，token 只读一个快照。补“UPnP 成功但融合拒绝”的 token 断言。 |
| **低** | 设计 L8-10；`upnp.rs:50-58`、`upnp.rs:90-134`、`upnp.rs:590-613` | **命名和错误面不够类型化。** `hostname_addr` 是远端 SSDP 来源，新字段是本地源，二者都叫 source 容易在实现时接错；源 bind 失败、pin 失败目前都会被压进 `Io`/字符串，日志缺 local_ip/iface。 | 用 `local_ip`/`local_iface` 与 `hostname_addr` 明确区分；新增 `SourceBind`/`IfacePin` 类型化错误，日志带候选地址与接口名。 |
| **低** | 设计 L17；`bindwatch.rs:104-165` | **“成功换卡即使返回旧卡也增加世代”语义不清。** 同接口地址未变、健康探针重挑回同卡、失败后保留旧 pin，三种情况是否增代没有定死；实现容易变成每拍增代或该增不增。 | 明确定义：只有“成功改变 socket 绑定或观测到本地地址集变化”才增代；单调 `u64`；补同卡/旧卡/失败三态测试。 |
| **低** | 设计 L19；`INTEROP-CRITERIA.md:276-300`、`:837`、`:1284-1285` | **判据行登记没有写进设计。** 设计只承诺 wire/token 不变；新增“未绑卡/接口无 IPv4/源绑定失败”等失败原因，或改变 `ShrinkOutcome`/E-q4 行文时，按仓库硬规则必须登记到“判据变更记录”。 | 列出新增/变化的 additive 行；实施同批登记。无需 openspec change，但判据登记不能省。 |

## 专项结论

| 专项 | 结论 |
|---|---|
| **来源错配是否解决** | **主线能解决，但不是设计门全闭合。** SSDP/GET/SOAP 同源 + `NewInternalClient` 同源是正确修复；H1 的副作用门和 M1 的类型化/可证伪测试必须补。 |
| **候选选择** | 方向正确：不再枚举首项，受运行期 QUIC 接口约束。规则需补 M4：多 IPv4 顺序、none、显式非法地址、接口身份。 |
| **换卡在途竞态** | 世代门方向正确，但只挡发布不挡副作用（H1），状态快照不原子（M2），失败/超时/停止未闭环（M3）。 |
| **绑定失败与 none** | fail-closed 方向正确；但 H2 把源 bind 与 iface pin 混在一起，Linux 非 root 会误伤。`none` 的行为变化要专门可见化。 |
| **缩租所有权** | “本世代、精确元组、完整表”原则正确；M5 的共享记录/精确查找、M6 的 Delete 后 Add 失败语义必须补。 |
| **超时取消/DNS 边界** | 基础架构通过：`UpnpContext` + `DeadlineStream` 50ms 轮询 + 单系统解析 worker，已避免 pubep 线程无界 `getaddrinfo`；H1/M8 补的是副作用门和 IPv4/IPv6 目标边界。 |
| **平台差异** | socket2 钉卡 TCP 的模式已有 DoH 先例，macOS/Linux/Windows 可复用；真正风险是 Linux `SO_BINDTODEVICE` 权限和 Windows 仅交叉 check、无运行面证据。 |
| **Rust 形态/Go 直译** | **没有明显过度设计，也没有逐行移植痕迹。** 用 `UpnpContext` 承载 deadline/cancel、用 generation 解决世代竞态，是合理的 Rust 形态；问题在状态分散（M2/M9），不是抽象太多。 |

## 没有问题的方面

- **根因定位与主线修复方向正确。** 设计直接覆盖根因文档中的 `.84→.71 402`、`.84→.84 200` 对照，不再让 SSDP 与 SOAP 各自选路。
- **“不退回枚举首项”的原则正确。** none/unbound/钉卡失败/纯 IPv6 没有安全候选时失败可见，比继续猜一个地址安全。
- **期限/取消骨架可保留。** 现有 `UpnpContext::check/pause`、`DeadlineStream`、`resolve_system` 唯一 worker、40s/8s 全局预算，已经避免无界 DNS 和滴流读；无需重写。
- **缩租的“精确核验、表不完整不认领”原则正确。** 这是防止误缩同机另一实例/另一接口映射的关键，问题只在状态和 API 没接上。
- **wire/token 格式不变。** 不涉及协议字节、token 编码或客户端恢复阶梯；`socket2` 已经在 workspace 依赖中，无需新增依赖。
- **范围控制合理。** 不改 LaunchAgent，不碰 .84 部署/发布；真实 IGD 验收留后续授权，符合本轮边界。

## 开工前建议的最小闭环

1. **H1**：把 generation/stop 变成 `UpnpContext` 的 mutation guard，覆盖 Add/Delete；成功后立即记录映射，过期则精确补偿。
2. **H2**：拆开 `bind(local_ip)` 与 `pin_iface`；源 bind 失败才终止候选，pin 失败默认降级并可见，或明确 fail-closed 的 Linux 权限前提。
3. **M1/M2/M5**：`local_ip` 与 `NewInternalClient` 单源；`PublicationState` 单源；每轮原子快照 `(generation, iface)`；shrink 只吃本世代精确 `MappingRecord`。
4. **测试**：双地址/注入 connector 证明源绑定；延迟 Add + 换卡证明旧轮不残留；none/pin 失败；多 IPv4；另一 IGD；Delete 成功 + Add 失败；token 不发布被融合拒绝的 `quic_pub`。
5. **L3**：实施同批登记 `INTEROP-CRITERIA.md` 的新增/变化行。

**总评：方向通过，设计门需修订后再开工。** 当前不建议直接进入产品代码实现。

## 第一道门逐项处置与修订设计

1. H1 接受：上下文带绑定世代检查，每次HTTP/SOAP前检查；发出可能建规则的操作前记录原IGD和目标元组，响应迟到/取消也保留到退出精确核验，旧轮不能发布。成功或可能成功的旧卡规则不被新卡覆盖；退出全部记录共用8s。AddAny外口未知时仅允许完整表中同源/同内口/精确描述匹配，不能跨实例认领。
2. H2 部分接受：源bind与interface pin独立报错；选择明确fail-closed。自动/显式卡只有QUIC实际pin成功才产生候选，UPnP TCP若pin失败也拒绝跨卡。Linux普通用户无权限pin时自动UPnP不可用，明确日志；静态端点/relay不受此候选门影响。
3. M1 接受：context本地源是唯一真源，SOAP Add验证InternalClient与其一致；真实socket回归使用默认来源不同的地址，不能只用默认127.0.0.1自证。
4. M2 接受：复用publication锁保存运行期绑定与原子世代；驱动在同门内repin及更新，原子快照绑定+世代；pinned_flag仅兼容既有观测判据，由驱动更新，watcher不再独立写。门内没有网络IO/driver RPC。
5. M3 接受范围内部分：驱动持有实际绑定事实，迟到成功仍更新共享状态；停止位门拒绝停止后的Repin，watcher不能在缩租期间再换卡。实时地址选择及发布前源仍存在校验防地址删除后发布。watcher join/通用RPC取消不扩展为本批框架；失败仍保持真实旧socket绑定，不能谎报新卡。
6. M4 接受：接口按name及当前index核对，IPv4去重排序，只收既有RFC1918候选；显式IPv4只用自身，拒0/组播/广播/非私网，纯IPv6和none/unbound失败可见。
7. M5 接受并加强：保存成功及可能成功映射的原IGD/context，退出不重新发现、不扫描其它接口。精确外口+UDP+client+internal+描述且表完整才缩租，旧记录保留；所有记录共用一个8s。
8. M6 接受：缩租只尝试Add300，718失败保留原映射，不再delete+add，避免删唯一可用规则；一般映射既有718所有权行为不扩大范围，旧原租期保证的误述修正。
9. M7 接受：ensure返回原IGD，在同上下文取WAN，不二次SSDP。
10. M8 接受：有IPv4本地源时过滤AAAA，保留3s总connect上界、DNS来源钉定及50ms读写取消。
11. M9 接受：UPnP仅返回证据，不提前写quic_pub；融合成功时与published/token同门提交。失败不留下新quic_pub。
12. L1 接受：LocalSource含ip/iface，区分远端hostname_addr，新增SourceBind/IfacePin/SourceMismatch错误。
13. L2 接受：仅驱动成功repin（含同卡地址变化触发）增加单调世代；稳定无操作、repin失败不增。
14. L3 接受：同批更新INTEROP-CRITERIA变更记录；wire/token/预算不改。

修订后结构化复核：来源/候选/旧轮发布/永久规则记录/缩租所有权已有明确闭环，允许实施。代码门须复核上述14项及测试证据，不能把技术门意见当成实测。

## 实施结果与专项验证（代码门输入）

- `upnp.rs`：LocalSource贯穿SSDP/GET/所有SOAP；TCP先bind再pin，类型化来源/钉卡/不一致错误；IPv4来源过滤AAAA；context校验世代；WAN复用原IGD。建请求前保存收据，已知外口并集合并，不覆盖旧来源；AddAny回应未知仍可按完整表精确client/internal/desc核验；退出按原上下文共享8s缩租。
- `engine.rs`：复用publication门保存实际绑定/原子世代/收据；driver成功repin才原子更新，停止后拒绝repin，迟到成功不依赖watcher回执。观测原子快照候选+世代，最终提交复核实时地址；probe命令携带世代。quic_pub在证据融合后与cache/token提交，fallback忽略旧世代端点。
- `bindwatch.rs`：不再自行写pinned_flag，实际事实由driver拥有。稳定卡与失败仍沿用既有看护语义。
- 缩租仅Add300，不再delete+add；无收据不枚举认领。原同机另一接口/实例、同前缀不同描述均不触碰。其他映射生命周期/候选外口预算常量保持。
- 真实socket回归覆盖GET/List/Add3600→0/Delete/Add300/AddAny，HTTP peer必须等于所选InternalClient；gateway.invalid始终保留Host，来源钉定不跑DNS。Mac无127.0.0.2，实核使用已分配物理IPv4连回环；测试无skip成功路径。
- 反证：临时仅移除TCP源bind、其余逻辑保持，`http_and_every_soap_stage_use_selected_source` exit101，GET实际源127.0.0.1、预期192.168.3.12，立即失败；已恢复源码。日志 `/tmp/homeway-upnp-source-negative.log`，不入仓凭证。
- 延迟Add响应时换世代：旧轮返回Err，但建请求前收据仍可沿原源把映射缩到300；候选实际绑定/多IPv4/显式IP/none/失败/IPv6、成功与失败repin、旧轮cache/probe/token不发布及fallback不泄漏、none仍可显式public-endpoint发布均有回归。
- 最新专项 `cargo test --locked -p homeway-core --lib server:: --no-fail-fast`：289 PASS、0失败、5既有ignore（真实网等）、661 filtered，5.56s。首轮旧缩租测试预期冲突后delete+add，与修订行为不符而失败，已改为Delete允许成功但不得发出的保护断言；新精确缩租测试首轮误依赖mock条目顺序而失败，已按外口查找修正，非产品缺陷。
- clippy首轮仅新测试切片clone触发cloned_ref_to_slice_refs，已用from_ref修复；终验与交叉结果见下表。

## 专项终验

| 命令/门 | 结果 |
|---|---|
| `cargo test --locked -p homeway-core --lib server:: --no-fail-fast` | 289 PASS，0失败，5既有ignored，661 filtered，5.56s；包含30个UPnP测试与绑定/发布新回归 |
| `cargo clippy --locked -p homeway-core --all-targets -- -D warnings` | PASS，7.81s，0 warning；首轮测试clone已修复 |
| `CC_aarch64_unknown_linux_ohos=<DevEco NDK>/aarch64-unknown-linux-ohos-clang cargo check --locked --target aarch64-unknown-linux-ohos -p homeway-core -p homeway-cli -p homeway-capi -p homeway-quic` | PASS，真实NDK，14.47s |
| `CC_x86_64_unknown_linux_musl=<DevEco NDK>/clang CFLAGS_x86_64_unknown_linux_musl='-nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem <repo>/tools/cc-check-shim' cargo check --locked --target x86_64-unknown-linux-musl -p homeway-core -p homeway-cli -p homeway-capi -p homeway-quic` | PASS，12.36s；capi既有 `cdylib` 不支持musl目标warning，非本批源码warning |
| `git diff --check` | PASS |

musl第一次用系统Apple clang exit101（其resource stddef.h include_next失败）；保留 `/tmp/homeway-upnp-musl-check.log`，换成已安装NDK clang后通过，未改仓库构建配置。其余日志 `/tmp/homeway-upnp-{server-final,clippy,ohos-check,musl-ndk-check}.log`。
本子任务没有发布GitHub Release或正式部署、没有SSH/SCP、没有改现役服务；此前全仓ci-local/release档构建及私有真实IGD补验由主会话独立执行，不能把mock结果当成真实.84或发布件验收。

## 第二道门原始意见（dsh exit 0，完整原文）

## 结论

未发现新的高危项。根因闭环、H2 fail-closed、世代/发布门、建规则前收据与“只 Add300 不 delete”方向均成立。仍有 **3 个中、3 个低**；建议第 1/2 项修后再提交，第 3/4 项同批修或明确登记为残余，第 5/6 项清理即可。

范围核对：基线 `cdff8f1`，产品代码实际变更 4 个文件；review 文档未跟踪；无 openspec change。本次只读，未跑 cargo、未改文件。评审期间工作区被并发刷新：review 文档从 123 行增到 136 行（追加 §专项终验，musl PASS），`docs/matrix-latest.md` 也被 `matrix.sh` 刷新；后者不属于本修复，提交前应确认丢弃或独立提交。

## 仍存在的问题

| 级别 | 位置 | 问题 | 建议 |
|---|---|---|---|
| **中** | `upnp.rs:1437-1441`、`1108-1112`、`1125`；`INTEROP-CRITERIA.md:618-619` | **AddAny 成功路径也会留下 `any_port=true`，缩租时忽略已知外口。** AddAny 前 `record(None)` 置 `any_port=true`；成功后的 `record(Some(ext))` 经 `merge` 的 `self.any_port \|= other.any_port` 仍保持 true；`shrink_before` 在 `any_port` 下直接跳过 `external_ports` 过滤，按“同源/同内口/同描述”扫全表。正常 AddAny 成功也会对同 tuple 的所有外口做 Add300；AddAny 失败或请求前被停止同样留下 broad 收据。现有 `shrink_receipts...` 只测了直接构造的 unknown 收据（`upnp.rs:2299`），未覆盖“`record(None)` → 成功 `record(Some)`”路径。 | 保留“请求前先记收据”的原则，但把收据分成 **Pending / Known(ext) / Unknown** 三态：成功时把本请求的 Pending 收敛为 Known(ext)，确定失败删除 Pending，只有真正无回应才保留 Unknown；不要再用 `Option<u16>` + 布尔 OR 同时表达三种语义。补“AddAny 成功只缩返回外口”的测试。 |
| **中** | `bindwatch.rs:89-95`、`107-160`、`267`；`engine.rs:1609-1611`、`2061-2067`、`2080-2090`、`2510` | **看护首拍没有初始卡状态，稳定启动也会重钉同卡。** `WatchState::default()` 的 `cur=None`，首拍（5s）不做“同卡同指纹”比较，直接 `Repin` 到当前已钉的卡；`PublicationState::repin` 随即 generation+1，`EngineCmd::Repin` 还清空 probe 端点。若首轮公网观测在 5s 时尚未完成，`publish_endpoints` 的 `round.current` 会拒绝该轮；kick 虽会触发第二轮，但若第二轮 UPnP/DDNS 较慢，15s token 兜底可能先打出不含公网 QUIC 端点的 token，probe 端点也会空到下一次成功发布。首轮若已完成，probe 端点仍会被无谓清空。 | 当初始 QUIC pin 成功时，把 `resolved` + 实时指纹作为 `WatchState` 初始值（仅初始 pin 失败时保持空状态让首拍重试）；首拍走正常“同卡指纹未变”分支，不重钉、不增代。另可让 `repin` 返回“绑定/来源是否真变化”，仅在真变化时清 probe 端点。 |
| **中** | `upnp.rs:1127` 与 `211-215` | **缩租的 InternalClient 精确匹配没有复用 v4-mapped 归一化。** `classify_mapping` 明确支持 `::ffff:a.b.c.d`，但 `MappingLease::shrink_before` 只做 `parse::<Ipv4Addr>()`；路由器若把 `NewInternalClient` 报成 v4-mapped 形态，本进程的映射在 `mapping_round` 可被认作 Ours，退出缩租却匹配不到，Add300 不会发出。若之前走过 lease 0 回退，该映射就成为永久规则。 | 抽出与 `classify_mapping` 共用的 `parse_internal_client`（纯 IPv4 + v4-mapped IPv6），`shrink_before` 与分类逻辑同源；补 v4-mapped 表项测试。 |
| **低** | `upnp.rs:538-552`（尤其 `540`）、`783-785`、`810-823`；`engine.rs:2224-2266` | **世代只挡结果发布，不在写请求前检查，也不能中断在途读写。** 世代变化可以发生在 `connect_within` 返回之后、`stream.write_all` 之前，旧轮仍会发出 Add/Delete；Add 有收据兜底，Delete/clean 没有收据。更实际的是 `DeadlineStream` 只绑定 `stop`，不检查 generation：换卡后旧轮若卡在 SOAP 读，单线程 pubep 最长约 40s 不能开始新卡的映射，公网端点恢复被拖慢。不是旧结果被发布，而是旧轮副作用/恢复时延。 | 在 `write_all` 前再做一次 `context.check()`；把 stop+generation 合成 `DeadlineStream`/读写的取消条件，世代变化时快速返回 `BindingChanged` 并让已排队的 kick 起新一轮。补“延迟响应不返回 + repin”的测试，而不只是 2s 后恢复。 |
| **低** | `upnp.rs:783-785`、`810-823`；`UpnpError:148-160` | **新类型化错误在 GET/SOAP 包装处被字符串化。** `igd_from_location_context` 把 `http_call_context` 的 `SourceBind`/`IfacePin`/`BindingChanged` 统一变成 `DescFetch(String)`；`Igd::soap` 又把底层错误变成 `Soap { msg }`。类型变体只对直接 `context.check()` 路径有效，描述 GET 和 Add 超时/换卡错误到 engine 时已无法按类型分支，只剩文案。 | `DescFetch` 至少保存 `#[source] Box<UpnpError>`（或 `thiserror` source）而不是 `to_string()`；`Soap` 增加可选 typed cause 或保留关键错误分类。若暂不做，至少在日志里带 local_ip/iface，避免只看到字符串。 |
| **低** | `INTEROP-CRITERIA.md:290-291`；`reviews/2026-11-06-upnp-interface-fix.md:31`、`:123` 对 `:125-136` | **文档残留已过时。** 旧 Q-E 条目仍写“718 才 delete+add(300)”，与本次“只 Add300”相反；新条目虽已登记，但旧条目未标“已被 2026-11-06 条取代”。review 文档顶部仍写“待实施后补全”，第 123 行仍写“终验与交叉结果待补”，而第 125 行起已补 musl/clippy/OHOS 终验。 | 在旧 Q-E 条目加“已被 2026-11-06 条取代”或更新主判据行；删除/更新 review 文档两处占位，避免最终记录自相矛盾。 |

## 看过、没发现问题

- **来源根因闭环**：`LocalSource` 贯穿 SSDP、描述 GET、所有 SOAP，`check_internal_client` 保证 `NewInternalClient` 与源 IP 一致；WAN 查询复用 `ensure` 返回的原 IGD，不再二次 SSDP。没有看到默认路由重选或 InternalClient 分叉。
- **H2 fail-closed**：`SourceBind`/`IfacePin`/`SourceMismatch` 独立；TCP 先 bind 再 pin，pin 失败在 connect 前返回；自动候选只来自实际 pin 成功的物理接口，none/unbound/纯 IPv6/公网地址均可见失败，没有跨卡降级。显式 IPv4 仅用自身，符合设计。
- **世代与发布门**：`PublicationState` 单锁保存绑定、世代和收据；`ObservationRound::snapshot/current` 原子快照并在最终发布前复核实时来源；probe 命令带世代；token fallback 用世代隔离旧 `quic_pub`/`last_published`。除上面第 2/4 项的启动时序和在途读写外，没有看到旧结果被发布。
- **收据与缩租主体**：建请求前记录、同 scope 合并已知外口、旧来源保留、所有收据共享 8s、只 Add300、不 delete、不重新枚举其它接口、完整表 + 精确 description/client/internal 才缩租——这些方向正确。问题集中在第 1 项的 `any_port` 与第 3 项的 v4-mapped。
- **停止/换卡**：停止位在 `pubep_stop`，`state.repin` 拒绝停止后的重钉，`SetProbeEndpoints` 复核世代，`stop_public_observation` 在缩租前等待发布门和观测线程；迟到成功由 driver 状态收敛。除前面提到的首拍和 check-then-act 残余外，没有看到 stopping 期间重新换卡或旧轮写 token。
- **平台与形态**：`socket2` 源绑定 + Unix/Windows raw socket pin 路径完整；OHOS NDK、musl、clippy 结果按 review 文档 §专项终验均 PASS；没有新增依赖、wire/token 字节和 8s/40s/3s 预算未变，也没有明显 Go 逐行移植/多余 Arc/Mutex。`PublicationState` 和 `ObservationRound` 的抽象是合理的；真正的类型薄弱点是收据用 `Option<u16>` 表达三态。
- **测试**：289 PASS/5 ignore、source 反证、延迟 Add、精确缩租、token fallback 覆盖到位；但缺 AddAny 成功路径、watcher 首拍、v4-mapped、以及“读不返回 + repin”的测试，正好对应上面的中/低项。

## 优先级

1. **先修第 1、2 项**：前者是所有权口径与文档/测试不一致，后者是稳定启动也可能触发的世代/探测端点回归。
2. 第 3、4 项同批修；至少第 3 项应补测试，第 4 项若换卡恢复 SLA 要求秒级，则按中等问题处理。
3. 第 5、6 项清理；`docs/matrix-latest.md` 属于生成产物，不要混进本修复 commit。

## 第二道门逐项判断与整改计划

1. 接受：AddAny每次SOAP请求分别记Pending，只有该请求成功回报ReservedPort才收敛为Known；没有成功回报保留Unknown，不能用OR永远扩大匹配。用类型化事件和未解决请求计数，保护先前未知回应，不丢永久规则。
2. 接受：初始pin成功才给watcher传入已绑定卡及初始指纹；pin失败保持空状态首拍重试。稳定首拍不增代、不清probe，不改变健康探针节拍。
3. 接受：抽取parse_internal_client供分类与缩租共用，支持v4-mapped；补永久租期表项回归。
4. 接受：HTTP写前再查context；DeadlineStream可选世代取消，与现有50ms stop轮询同层组合，保留原总期限与connect<=3s。补服务端一直不回应时换世代迅速返回的回归。
5. 接受：DescFetch/Soap保留Box<UpnpError> source链；缺ReservedPort独立类型错误，发现候选汇总也保留最后typed cause。
6. 接受文档清理；matrix-latest为主会话明确要求保留的同批ci证据，不采其独立提交建议。修前验证/真机证据留存，最终件另行复验。

主会话明确允许上述整改后做结构化复核关闭，无高危时不重复20分钟dsh；后续验证须区分修前/最终件。

## 代码门整改终验与结构化复核

| 意见 | 最终实现/回归 | 复核结论 |
|---|---|---|
| 中1 AddAny未知收据不收窄 | `MappingAttempt::{Requested,AnyPending,AnyKnown,AnyRejected}`；逐SOAP请求计未解决数，成功仅解当前Pending，明确Fault解Pending；较早缺Reserved的未知回应不被后来成功抹掉。正常成功/3600 Fault后0成功只缩已知返回外口，另有200缺Reserved再成功保留两次Unknown回归 | 已修；无额外锁，scope按原IGD/source/internal聚合，known外口并集不丢旧规则 |
| 中2 首拍无谓repin | `WatchState::from_bound`；仅实际初始QUIC pin成功seed初始卡/指纹，失败仍None；稳定首拍零repin/零change，失败首拍重试回归 | 已修；健康探针节拍不变 |
| 中3 v4-mapped缩租 | 分类和缩租共用`parse_internal_client`，`::ffff:127.0.0.1`永久映射缩300回归 | 已修；原源/内口/外口/精确描述所有权门保持 |
| 低4 世代取消 | HTTP写前context复核；DeadlineStream可选generation，原50ms读写取消轮询同时看stop与generation；服务端不回应且不关闭，换世代<=350ms返回BindingChanged回归 | 已修；原3s connect、40s/8s总期限不变，默认DoH消费者无generation仍原语义 |
| 低5 字符串错误链 | DescFetch/Soap/NoIgd保存Box typed cause，缺ReservedPort独立错误；真实pin错误穿GET/SOAP及发现汇总仍可按变体核对回归 | 已修；不再靠字符串辨识来源/取消错误，SOAP业务Fault仍保留Display正文供既有713/718判定 |
| 低6 文档残留 | 主摘要写全部世代原IGD收据、不重新发现；旧Q-E缩租口径标后继条；删除待补占位；区分私有真机和正式Release证据 | 已修；历史两门原文完整保留，matrix证据按主会话要求同批保留 |

新回归准确名称：
- `server::upnp::tests::add_any_receipt_resolves_success_but_keeps_earlier_unknown_request`
- `server::bindwatch::tests::initial_success_seeds_watch_state_but_failed_pin_retries_first_tick`
- `server::upnp::tests::shrink_recognizes_v4_mapped_permanent_mapping`
- `server::upnp::tests::repin_interrupts_silent_http_without_waiting_for_server_or_total_budget`
- `server::upnp::tests::description_soap_and_discovery_preserve_typed_causes`

另由主会话真实auto补验证实旧E20句将UPnP43553与监听43553比较却写“不相等”；本批更正日志为实际STUN24584 vs UPnP43553，融合行为不改，已登记判据。所有权scope额外核对原hostname_addr，防同hostname已换IGD来源时误合并旧控制上下文。

最终 `cargo test --locked --workspace` exit0：CAPI4 PASS；CLI231 PASS/2既有ignore；core954 PASS/6既有ignore（另有child测试1，不重复计唯一用例）；QUIC294 PASS，全部集成及doc tests通过。日志 `/tmp/homeway-upnp-r3-workspace-test.log`。随后只纠正上述STUN诊断打印的参数，最终全仓clippy/交叉/release包含此文案修正。
`cargo clippy --locked --workspace --all-targets -- -D warnings` exit0（7.10s）；OHOS真实NDK与x86_64-musl四crate check均exit0，musl仅既有capi cdylib目标warning。日志 `/tmp/homeway-upnp-r3-{clippy,ohos,musl}.log`。

结构化代码复核（按主会话授权关闭，不重复外部dsh）：来源/Host/DNS边界、绑定候选/初始失败/首拍/换卡、停止与世代取消、迟到映射收据、AddAny已知/拒绝/未知、跨实例/接口所有权、缩租/v4-mapped、typed错误链、预算/体积限制、无Go逐行移植/多余Arc或Mutex均复核通过；未发现新增高危或未处置阻塞项。旧接口彻底下线时退租仍可能失败，共享8s下为明确可见best effort；永久规则可能需要人工精确清理，不能承诺必然过期。

最终私有release档构建 `cargo build --locked --release -p homeway-cli` exit0，1m36s；产物
`target/release/homeway` 为10,612,480 B，SHA256
`0e7cd0c0ffa4bc2317608ed9edf742e7473104984afe488fe0e348fa3b11c44f`。
此为测试构建，不是GitHub Release，不替换系统PATH内正式二进制。最终件en7/auto两轮真实IGD/外网QUIC补验交由主会话继续；修订前d711d69…两轮证据完整保留，不能冒充最终件证据。
最终 `git diff --check` PASS；产品源码冻结。未commit、未push、未正式部署；ROADMAP当前指针留主会话同批更新。

## 主会话整体验证（区分整改前与最终件）

完整 `tools/ci-local.sh` quick 于2026-11-06 23:36:44 exit0，日志
`/tmp/homeway-upnp-fix-validation/ci-local.log`。该轮为代码门整改前版本：core949/6ignored、
CLI231/2ignored、QUIC294、CAPI4及全部集成/doc tests通过；全仓clippy、OHOS真NDK
check和真实链接（`.so` 2,943,808 B）、隔离门、向量/SHA确定性、fuzz种子摘要、词表、
WG残留门通过。RRR私有冒烟316s，真实注册/直连、双向测速1176/1268Mbps、100MB
双侧SHA一致、隧道往返及文件list通过；既有E11 WARN、DNS形态不适用SKIP按矩阵
在册口径保留。生成结果 `docs/matrix-latest.md` 同批入库。

代码门整改涉及共享DeadlineStream，故最终件重新跑完整workspace tests/clippy与
OHOS/musl check，数字见前节；不把整改前整轮CI冒充最终整轮CI，也未重复无关矩阵。
最终私有二进制重建后另行两轮真实IGD/公网QUIC验收，下节记录同件证据。

整改前私有件 `d711d69daa2836f59948feb9b917db01a53a3edbd92e462f65355b1cc6b40262`
（10,612,656 B）在真实Q7完成R1显式en7和auto两轮：UDP43552/43553分别映射到
`.84`同内口，云官方v0.3.26仅用唯一公网IPv4候选，ready/direct/实际ep匹配、暖机、
各3次path probe通过。固定轮准入22ms、暖机8ms、probe8/12/8ms；auto轮准入19ms、
暖机8ms、probe10/9/8ms。auto原STUN外口24584与UPnP43553不同仍正确融合并连接；
旧日志误将融合后43553与UPnP43553打印成不等，已据实修正，不能引用作原端口相同证据。
两轮IGD15→15条，排除租期倒计时后七列多重集相等；正式PID7025/官方二进制及7个
普通state文件逐字节和权限owner均保全、台账追加0B。全部私有进程、映射、state精确
回收；原始四份tar及回执保留本机私有目录
`/var/folders/_b/vk9nj0z93rb2msqs0pz5c9gm0000gn/T/homeway-upnp-private-20261106-lqojqk_n`。

两轮客户端均校验token结构/校验和，仅替换端点段为kind2、全局公网IPv4、实测映射
外口且去重唯一的候选，peer/secret/RPK字节原样保留；云端全新identity-dir，最终
status须ready/direct/目标ep且明确3条probe成功（CLI probe失败不保证非零退出，
不能只看exit0）。因此无LAN/IPv6/中继混胜，只声称完整QUIC准入/暖机与path probe，
不将其扩称文件、终端或bulk吞吐验收。token与原始凭据不入本仓。

## 最终件 R2 真实IGD与外网QUIC验收

同一最终私有件SHA256
`0e7cd0c0ffa4bc2317608ed9edf742e7473104984afe488fe0e348fa3b11c44f`，10,612,480 B，
由本机scp至`.84`独立`/tmp`目录并复核，不替换`/usr/local/bin/homeway`。基础启动复用
`tools/local-rust-exit.sh`的临时副本，仅移除默认显式回环public-endpoint参数，追加
测试bind-interface/upnp/stun选项；state/文件根/端口/身份均隔离，SSH责任链持续驻留。
启动前完整表保护既有条目，测试结束先优雅停，再按完整表精确核验并删除本轮规则。

| 2026-11-06轮次 | 实际绑定/私有PID | IGD与STUN/公布端点 | 云官方客户端验收 |
|---|---|---|---|
| 23:51:42 fixed | en7/index27，192.168.3.84；13191 | UDP43552→.84:43552；监听socket STUN公网43552；公布114.242.60.128:43552 | ready/direct/ep精确匹配，准入23ms，warmup12ms，3/3 probe9/9/8ms，1.118s，exit0 |
| 23:52:01 auto | en0探测126ms、en7 119ms后选en7/.84；13476 | UDP43553→.84:43553；监听socket STUN公网43553；公布114.242.60.128:43553 | ready/direct/ep精确匹配，准入23ms，warmup10ms，3/3 probe9/9/9ms，1.077s，exit0 |

两轮均全新state/token/log/云identity和映射表快照，沿前节唯一公网IPv4候选约束，排除
LAN/IPv6/relay混胜。两轮约8s观测内未见首拍无谓同卡repin/重测。最终R2未出现
STUN/UPnP外口不同，不能将其声称为新mismatch日志分支实测；旧R1的24584/43553
证据及最终源码/编译检查支持该诊断参数更正，实际融合行为保持不变。

最终raw及脱敏回执存本机私有目录
`/var/folders/_b/vk9nj0z93rb2msqs0pz5c9gm0000gn/T/homeway-upnp-private-r2-20261106-4ndpvg52`。

最终回收守卫：两轮IGD全表15→15，RemoteHost/external/protocol/internal/client/enabled/
description七列排序多重集严格相同，仅租期自然减少（fixed全-9s、auto全-8s）。正式
PID7025、官方件SHA/inode、7个非cache普通state文件字节/权限owner全部不变，tokens
追加0B。两轮SIGTERM正常，强杀0；最后独立复核Mac/云私有PID均0、UDP43552/43553
空闲、测试映射0、全表仍15。901/902临时state经owner marker、锁、进程、端口、表
全门核实后精确回收，states_absent=true。未启停正式服务、未改正式LaunchAgent。
`verification-report.json`为脱敏数字报告；私有`raw-manifest.json` SHA256为
`ee8a62925f4137cca96ff5a61159d25c62e24b4676e52158794d27cb91fa66dc`，
记录四份server/cloud原始tar及脚本/守卫回执哈希；原始凭据不入库。

最终结论：代码修复、两门整改、最终全仓测试/交叉检查与同件双轮真网关验收完成。
本地中文提交包含ROADMAP/判据/本轮矩阵证据；无push/tag/GitHub Release/正式部署。
现役v0.3.26仍需后续正式Release升级才获得代码修复，不能把私有验证当成正式换装。
