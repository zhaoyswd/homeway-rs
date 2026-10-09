# M4「portfwd 收口（拨号缝换轨 + spec 不回退核验）」设计

> 本文件 = M4 期的**设计真源**（设计门第一道）。体例照 M3/M2/M1：
> ① §0 复验（回源码重定位 + 绿基线 + 与路线/前批记数的差异登记）；
> ② §1–§5 方案定稿（dial 协议与地址接受集 / 拨号缝换轨 / 出口 dial 腿 / 失败归因 / NAPI 分档）；
> ③ §6 spec 不回退核验清单（tier `port-forwarding` 逐条）；④ §7 Q-F-B 残余 14 条逐条对照；
> ⑤ §7A 预算（体积/性能/内存）；⑥ §8 判据行影响 + **登记条目草案**（五字段）；
> ⑦ §9 真机验证计划；⑧ §10 风险与未决；⑨ §11 实施清单（S1–S6 + 依赖顺序 + 每项完成判据）；
> ⑩ **§12 实测锚点**（`/tmp/m4lab` 原始行，已落盘）；⑪ **§13 设计门记录（dsh r19 13 条 + r20 复审 3 条，逐条处置）**；
> ⑫ §14 待主会话裁决。
> **本棒不写产品代码**（只写本文件 + `/tmp` 验证台）。
> 真源：`docs/QUIC-ROADMAP.md` M4 节 + 「口径重申（2026-10-09）」+ 每期执行协议/评审协议；
> `docs/reviews/M3-design.md` §1/§5/§12-4/§14/§15/§16；`docs/reviews/M3.md` §5/§7；`docs/reviews/QFB.md`；
> `docs/INTEROP-CRITERIA.md`（CA 族 + E-q5/C19 行族）；只读需求真源 = tier `openspec/specs/port-forwarding/spec.md`。

---

## 0. 复验（回源码重定位；行号以 **`dd9b8c2`** 工作树为准）

### 0.1 工作树与隔离面

- 检出 = `/Users/zhaozhe/Documents/projects/homeway-rs-quic`（分支 `quic`）。本棒开工 HEAD = **`dd9b8c2`**
  （「政策：用户重申『不用考虑和之前的兼容性』——常设口径登记」），工作树**干净**（`git status --short` 空）。
  与前一批（`09b8b9f` M3 收口）之间只有 `dd9b8c2` 一个**纯文档** commit（政策登记）⇒ 本设计的行号以 `dd9b8c2` 为准。
- **隔离面**：M4 的主战场 = `crates/homeway-core/src/facade/{portfwd.rs,tun_exec.rs,quic_stream.rs}` +
  `crates/homeway-quic/src/exit/{serve.rs}`（+ 新增 `exit/dial.rs`）+ `tools/check-quic-isolation.sh`。
  **不碰**：主检出、`homeway`/`tier` 两仓、`baseline/`、现役出口、`docs/QUIC-ROADMAP.md`、`docs/INTEROP-CRITERIA.md`。

### 0.2 承重行号重定位（逐条回源码；路线/M3 记的行号已漂）

| 项 | 本树实测（`dd9b8c2`） | 说明 |
|---|---|---|
| `PfDialTarget` 与「出口自己」映射 | `facade/portfwd.rs:136-151`（`ExitPort(port) ⇒ SERVER_TUNNEL_IP:port` 在 **:147**）；派生 `dial_target()` **:157-171**（环回/未指定 ⇒ `ExitPort` 在 **:166-168**） | M3-design §1.1/§5.3-A1③ 点名的缺口落点 |
| 四形态 target 文案 | `facade/portfwd.rs:123-132`（`pf_target_text`） | NAPI 面，本期**不改**（§5） |
| 拨号缝（全注入） | `facade/portfwd.rs:344-345`（`PfDialFn`）+ `PfSetup.dial` **:368** + `PfContext.dial` **:380** + 调用点 **:985**（`pf_conn_thread` 首行 `(ctx.dial)(dst.resolve(), ctx.budget)`） | **签名本期不变**（§2.3） |
| 阀 / 准入 / FlowGuard | `admit_conn` **:907-944**（阀判定 **:914-926**）、`FlowGuard` **:949-974**（`admit` :956、`Drop` :966）、`conn_spawns` 测试缝 **:932-933** | 逐字保留（§2.4） |
| 两阶段 install / 换入 | `install` **:498-569**（阶段 1 take 旧 lns :502-505、阶段 2 等 ack :519-544、阶段 3 逐条 bind+spawn :545-559、阶段 4 单次换入 :560-568）、`install_one` **:573-640**、`StagedLns` **:408-421**、`stop_all` **:647-681** | 逐字保留 |
| 裸拨（WG 档） | `facade/tun_exec.rs:330-341`（`session_connect_target`，`connect_deadline` 在 **:339**）；pf 生产闭包 **:345-357**；闭包装配 **:1267**；`conn_err_to_io` **:361-367** | M4 换轨点 |
| 桥拨号的承载分派先例 | `tun_exec.rs:1758-1779`（`BridgeHost::new` → `set_dial_timeout`：`if run2.l3_on_island()` **:1765**） | M4 的 pf 分派**照此形**（§2.3） |
| pf 装表 / 收工顺序 | attach 段 `run.pf.install(&cfg.port_forwards)` **:1740** + stale 复查 **:1741-1746**；收工 `run.pf.stop_all()` **:1878**（桥停之前） | 逐字保留 |
| QUIC 档 dial 现状 | `crates/homeway-quic/src/exit/serve.rs:99`（`StreamTag::Dial => dial_refuse(..)`）、`dial_refuse` **:164-180**（一律 `reset(0x22)` + 行）、tag 白名单自检 **:253-285**（`:278-284` 明写 0x25/0x26 本期不写） | M4 把这条臂换成真拨号 |
| dial 帧（M3 定稿，**本期不改**） | `crates/homeway-quic/src/stream.rs:247-266`（`dial_target` 组帧 / `dial_parse` 反解析；6B）+ 单测 **:369-380** | 「帧不再改」（M3 §12-4） |
| 复位码表 / StreamErr | `stream.rs:96-117`（0x21–0x27 + 白名单闭区间）、`StreamErr` **:135-203**、`from_reset_code` **:167-177**、`is_ladder_exempt` **:197-202** | 0x25/0x26 已在码表（M3 备而未用） |
| 客户端服务流 seam | `facade/quic_stream.rs`：`tag_for_port` **:40-51**、`open_stream` **:81-87**、`dial_with`（重试策略）**:96-124**、`dial` **:127-141**、`stream_err_to_io` **:151-163**（`NotSupported|Refused ⇒ ConnectionRefused` 在 **:153**）、`QuicStream::into_halves` **:199-214**、读半 **:224-246**、写半 **:255-298**、`close_write` **:300-310** | M4 的 `dial_target()` 加在本文件（**该文件的「零 WG 引用」纪律**是隔离门 ⑪(c) 的判据面） |
| NAPI 下推链 | `homeway-capi/src/lib.rs:146-149` → `facade/mod.rs:457-461`（`tun_recover`，`Level::clamp`）→ `facade/tun_exec.rs:903-915`（executor `recover`，恒 `run.recover`）→ `tun_exec.rs:644-660`（`recover`/`recover_until`）→ `run_round` **:708-731** | 全链**无承载分档**（§5） |
| 判活（两承载同接口） | `tun_exec.rs:2297-2315`（`l3_probe`：岛 `Cmd::Probe` / WG `path_probe`） | §5 的 QUIC 档下推动作面复用它 |
| QUIC 档「不重复动作」先例 | `tun_exec.rs:2738-2742`（待发包下推：quic 档「岛内快探阶梯为准…本拍不重复动作」）、`:2553-2557`（巡检失败同款）、`:2572-2577`（3 连败兜底同款） | §5 的分档**与既有三处内部触发点同形**（r20 订正行号：待发包下推在 **:2738**） |
| 出口 UDS 豁免臂（WG 档） | `server/intercept/mod.rs:1186-1213`（`route_upstream`：`dst == tunnel_ip` ⇒ `Exempt` ⇒ `local_services` 命中 UDS / 否则 `loopback(port)`）；映射装配 `server/engine.rs:396-399`；非阻塞 connect 死线 **:75**（`dialTimeout` 10s） | 「出口自己」在 WG 档的实现路径（§1.3 对照用） |
| 隔离门 ⑪ 条 | `tools/check-quic-isolation.sh:426-427`（块标记 `BridgeHost::new(` → `set_dial_timeout`）、`:451-472`（零命中 + 自校准：`B_QUIC/B_WG ≥1`） | M4 需补 **pf 拨号缝块**（§2.5） |
| 出口 async 泵先例 | `crates/homeway-quic/src/exit/pump.rs:79-117`（`run` + `tokio::join!`）、`upstream` **:131-160**、`downstream` **:162-190**、`COPY_BUF = 8 KiB` **:193** | 出口 dial 腿**复用同一份泵**（泛型化，§3.3；不写第四份拷贝） |
| 流面定值（**S9 整改后**） | `crates/homeway-quic/src/tuning.rs:21-58`（`MAX_BIDI=64` / `RECV_WINDOW=**4 MiB**`（:37）/ `CONN_RECV_WINDOW=**8 MiB**`（:45）/ `SEND_WINDOW=2 MiB`（:56）/ `PENDING_BYTES=64 KiB`（:58））、`:217`（`FAST_BUDGET=700ms`）、`:222`（`REPROBE_FACTOR=2`） | 设计门 r19 M1 点名的**口径修正**（§7 预算、§8 行 11、§10-W1 全部按此；r20 订正行号） |
| 出口连接上限 | `crates/homeway-quic/src/exit/mod.rs:187-189`（`conn_cap = 2 × max_devices` ⇒ 缺省 **64**）；`intercept` 侧全局最坏登记 `server/intercept/mod.rs:399`（`MAX_CONNS(1024) × ~1MB ≈ 1GB`） | 资源上界两口径（§7/§8 行 11；设计门 r19 M2） |
| 「虚拟端口」测试面 | `crates/homeway-quic/src/exit/tests.rs:3055`（`service_stream_dial_reads_target_then_refuses_0x22`，**必改**）、`stream.rs` 的 `serve_only_emits_the_designed_codes` 自检 `serve.rs:253-285`（含 `:278-284` 的「M4 两码本文件不得写出」） | M4 的 S1 判据点名 |
| e2e 先例（可复用） | `crates/homeway-core/tests/quic_island_e2e.rs`：`connect_local_exit` **:673-713**、真世代 `generation_l3_rides_quic_datagram_against_local_exit` **:507-670**（`TunnelExec` + `ClientCore::tun_prepare/tun_attach` + `UnixDatagram` 假 TUN fd **:606-608**）、App 核桥面 **:908+** | M4 的真世代 pf e2e 照此形（§11-S5） |

### 0.3 绿基线（`cargo test --workspace`；两次全量，如实登）

- **第 1 次全量**：`homeway-core --lib` = **708 passed / 1 failed / 4 ignored**（80.97s）+ 其余 10 个目标全 ok；
  `exit=101`。唯一红例 = **`daemon::tests::server_bad_frame_gets_goodbye_and_disconnect`**
  ⇒ 在册 flake 族（M2 登记「隔离复跑同二进制内翻转」、M3 S6 复跑读数含它）。隔离复跑两次：
  **一次红（0.00s）/ 一次绿（0.06s）** ⇒ 与在册签名一致（不是本设计引入的红）。
- **第 2 次全量**：**全绿 `exit=0`**：`homeway-core --lib` **709 passed / 0 failed / 4 ignored**（86.96s）
  + 其余目标全 ok。
- 结论：**基线绿**（在册 flake 1 例，隔离复跑翻转；两次全量里一次全绿）。

### 0.4 与前批记数的差异登记（**两条**，均以本树实测为准）

1. **M3 收口记的 `.so` / 测试计数不含 M4 面**：本树 `homeway-core --lib` = **713 个测试**（`running 713 tests`），
   路线/前批记的是各切片当刻的数（M3 收口期 691 量级）。**不是漂移**，是本树新增切片后的实数——M4 实现棒
   以此数为基线（判回归只看「新增/改动面之外的差分」）。
2. **本机开发环境有 fake-IP 代理（对 e2e 设计有硬影响，必须登记）**：本机 `utun4` 持有 `198.18.0.0/15`
   且是**默认路由**（`netstat -rn` 实测 `default → link#24 (utun4)`）⇒ 一切「非直连网段」的目的地址
   （如 `10.255.255.1:9`、`100.64.255.1:1`）会被该代理**接住并回成功**（§12-P1 原始行）。
   **设计含义**：①本地 e2e 的「出口可达其它 IP」形态**只能用回环 + 直连网段**（本机 = `192.168.3.0/24`），
   否则测的是代理不是我方拨号腿；②P1 的「非直连目标」读数**不可作为出口网络行为的证据**（只作触发面坐标）。

---

## 1. dial 协议定稿（帧 / 回执 / 「出口本机」语义位 / 地址接受集 A8）

### 1.1 目标帧：**不改**（M3 定稿沿用）

- 客户端→出口：`tag(0x04) ‖ [4B IPv4 大端][2B 端口大端]`（6B）。**本期零改动**
  （M3-design §1.1 + §12-4 的「M4 换轨时不再改帧」；组帧/解帧单源 = `stream.rs:247-266`，单测 `:369-380`）。
- **域名不可表达**（帧限 IPv4；整表校验期 `TableErr::BadTarget` 已拒，tier spec「目标地址须为空或 IPv4 字面量」一致）。
- **IPv6 目标仍不支持**（M3 §1.1 显式登记；本期不扩族——扩族是新协议面，归后续期）。
- `targetPort == 0` 的**旁路形态**走 `dial_target()` 的折叠语义（IP 分支折 `listen`；空 IP 分支 `ExitPort(0)`）。
  **可达性（精确到向量，设计门 r19 L3 订正）**：NAPI 热替换路径**不可达**（`tun_set_port_forwards` 恒过
  `validate_table`，其 `ZeroPort` 分支先拒，`facade/mod.rs:500-506` + `portfwd.rs:100-102`）；
  **可达向量 = `tunConfig.portForwards`**——`TunCfg.port_forwards` 是 **serde 直读**字段
  （`facade/mod.rs:99` `#[serde(default)]`），**不过** `validate_table`，由 `install` 的逐条
  `defensive_err` 兜（`portfwd.rs:712-732`）；真实来源 = 手改/损坏的持久化记录（Q-F-B 残余①）。
  新承载下 wire = `127.0.0.1:0`（§4.4 的差异登记）。

### 1.2 **新增：1B 拨号回执**（出口→客户端；M4 的唯一 wire 增量）

**为什么必须有**：`STREAM[dial]` 的拨号发生在**出口**，客户端 `open_bi + 写 6B` 之后**无从知道**拨号成败。
若无回执，`PfDialFn` 只能在「还没拨通」时返回 `Ok` ⇒ Q-F-B 钉住的语义（**拨号失败 ⇒ 对端 `read` =
`ConnectionReset` 而非静默 EOF**、`fails` 计数、`拨号失败 #n` 行）全部退化 ⇒ 违反 M4 判据
「`STREAM[dial]` 与 Q-F-B 的阀 / 计数 / 热替换语义逐条对照」。

**形态**（线面，写死）：

| 方向 | 字节 | 含义 |
|---|---|---|
| 客户端→出口 | `tag(0x04) ‖ 6B 目标 ‖ 原始字节流` | 帧后即裸字节（M3 §1.2 不变） |
| 出口→客户端 | **`0x01`（`DIAL_OK`）** ‖ 目标侧原始字节流 | 首字节 = 拨号成功回执（**写于 `TcpStream::connect` 成功之后、泵启动之前**）；失败路径**不写回执**，改为 `reset(0x25/0x26)` |

- `DIAL_OK: u8 = 0x01` 常量落 `crates/homeway-quic/src/stream.rs`（**协议面单源**，两侧共用；不另写字面量）。
- **对 M3 文档的订正（登记）**：M3-design §1.2 表里 dial 行「后 = 裸字节管」只对**客户端→出口**方向成立；
  出口→客户端方向为「**1B 回执 + 裸字节**」。**不是**帧格式变更（客户端发的字节一个没变），是**新增方向面**。
- 客户端 seam 必须处理「**首块 > 1B**」：`Cmd::StreamRead` 返回的是块（≤16 KiB），出口写完回执可能立刻
  跟目标数据 ⇒ 首块可能是 `[0x01, payload…]`。**余量必须预置进读半缓冲**（`QuicStream::with_pending`），
  否则**丢一个字节 = 应用层帧错位**（浏览器侧表现为「响应缺首字节」，极难定位）。此点列入 S2 完成判据。

### 1.3 「出口本机」语义位（A1③ 的落点）——**wire = 出口回环**

| 形态（tier/NAPI 面） | 今天（WG 档实测路径） | M4（新承载） |
|---|---|---|
| `targetIp` 空（=「主机自己」） | `ExitPort(p)` → 客户端拨 `SERVER_TUNNEL_IP:p`（100.64.255.1）→ 出口 intercept 的**豁免臂**（`route_upstream`：`dst == tunnel_ip`）→ 出口 `127.0.0.1:p` | `ExitPort(p)` → wire `127.0.0.1:p` → 出口 dial 腿**直接拨本机回环** |
| `targetIp = 任意 127/8`（含 127.0.0.5） | `dial_target()` 归一为 `ExitPort(port)`（丢弃具体环回地址） | 同左 ⇒ `127.0.0.1:port`（**行为等价**：今天也丢弃了具体环回地址） |
| `targetIp = 0.0.0.0`（未指定） | `dial_target()` 归一为 `ExitPort(port)` | 同左 ⇒ `127.0.0.1:port`（**出口侧的 0.0.0.0 拒入仅作防御面**，正常客户端不发它） |
| `targetIp = 出口的其它地址`（LAN/公网 IP） | 客户端经隧道拨该地址 → 出口 transit 腿拨之 | wire 原样 ⇒ 出口 OS 拨之（**「出口 IP」语义保留**：本机地址由内核回送，§12-C4 实测） |

**裁决（裁决 D-1）**：`PfDialTarget::resolve()` 的 `ExitPort(p)` 产物由 `SERVER_TUNNEL_IP:p` 改为
**`127.0.0.1:p`**；WG 腿（`session_connect_target`）在拨号前把 **环回目标替换为 `SERVER_TUNNEL_IP:p`**
（A1① 的「客户端不得把 127/8 当隧道内目标」约束**在 WG 腿内**继承，wire 语义不外泄）。

- **理由**：①「出口本机」在 QUIC 档**没有隧道 IP 可指**（100.64.255.1 是 WG 的隧道地址常量，QUIC 档出口
  不持有该地址语义）——继续用它当哨兵就是「为旧承载留隐含依赖」；②回环即「出口本机」是**平台无关的真话**
  （tier spec 已把「出口侧回环目标不经代理」写进需求，见 §6-R5）；③映射收敛在**各承载的拨号腿**内 = 单一职责，
  QUIC 腿零 WG 常量引用（隔离门可判）；④验收口径简单：wire 上出现 `127.0.0.1:p` 就是「出口本机」。
- **登记**：`targetIp = 100.64.255.1`（手填出口隧道 IP 常量）在 QUIC 档 = 「字面拨 100.64.255.1」（通常拒），
  与 WG 档的「豁免臂 ⇒ 出口回环」**不同**（§8 登记行 6）。**不引入别名**（无兼容包袱常设口径；手填该常量的
  真实用户不存在——它是内部常量，App 表单不会产生它）。
- **不做**：出口侧**不得**对环回目标加任何拒绝（M3 §5.3-A1② 明写：加了会打死合法目标）。

### 1.4 地址接受集判定表（**A8 的定稿**；逐类 accept/reject + 理由 + 证据）

**判定点**：出口 dial 腿（`exit/dial.rs`），**在 `TcpStream::connect` 之前**。客户端侧不做地址类检查
（单源在出口；客户端的 config 期校验仍是 Q-F-B 的面：字面量/值域/重复/条数）。

| # | 类 | 例 | 判定 | 码/归因 | 理由与证据 |
|---|---|---|---|---|---|
| 1 | 回环 | `127.0.0.0/8` | **accept** | — | = 「出口本机」（§1.3）；spec 场景①；**出口侧不得拒**（A1②） |
| 2 | 出口可达的任意单播（含出口自己的 LAN/公网 IP） | `192.168.3.12`、`192.168.3.5` | **accept** | — | spec 场景②③；§12-C4 实测（出口自己的 LAN IP ⇒ 真拨通） |
| 3 | 私网 / CGNAT / 链路本地 | `10/8`、`172.16/12`、`192.168/16`、`100.64/10`、`169.254/16` | **accept** | — | 同上（出口出站视野 = 语义全部）。**安全面论证见下** |
| 4 | 公网单播 | 任意 | **accept** | — | 同上 |
| 5 | 未指定 | `0.0.0.0` | **reject** | `0x25` + 行 | **平台陷阱**：macOS 实测 `connect(0.0.0.0:port)` = **连本机**（§12-P1）⇒ 语义歧义；客户端已归一为出口回环，出口侧拒 = 防御面（防手写/异常对端把它当「随机本机端口」用） |
| 6 | 「本网络」 | `0.0.0.0/8`（`0.1.2.3` 等） | **reject** | `0x25` | RFC 1122 保留；平台归一不一（实测 `HostUnreachable`）⇒ 归因不可靠，直拒更诚实 |
| 7 | 受限广播 | `255.255.255.255` | **reject** | `0x25` | TCP connect 无意义（实测 `EAFNOSUPPORT(47)`）；拒 ⇒ 归因可读 |
| 8 | 组播 | `224.0.0.0/4` | **reject** | `0x25` | 同上（`224.0.0.1`/`239.1.1.1` 实测同码） |
| 9 | 子网广播 | `192.168.1.255` | **不单独判**（透传 OS） | `0x25`（拨号失败） | 判定需出口各接口掩码 + 平台差异；归因不失真（`EHOSTUNREACH`/`EHOSTDOWN`/超时） |
| 10 | 出口隧道 IP 常量 | `100.64.255.1` | **accept**（字面） | `0x25`（实际不可达时） | §1.3 裁决 D-1；**登记差异**（§8 行 6） |
| 11 | **端口 0** | `127.0.0.1:0` 等 | **reject** | `0x25`（why 区分「端口 0 不是可拨端口」） | 端口 0 无「连接」语义（`EINVAL`/`EADDRNOTAVAIL` 平台不一）；旁路配置形态见 §1.1（r19 L2） |
| 12 | 保留 / 未来用 | `240.0.0.0/4`（除 `255.255.255.255`） | **不单独判**（透传 OS） | `0x25`（拨号失败） | 与公网单播同面；拒列表只收「平台语义歧义或不可读」的类，不加宽（r19 L2） |
| 13 | 基准测试段 | `198.18.0.0/15` | **不单独判**（透传 OS） | `0x25` | 同上；本机开发环境的 fake-IP 代理恰好用该段（§0.4-2）是**本机现象**，不是产品语义 |
| 14 | IPv6 / 域名 | — | **不可表达** | — | 帧限 IPv4（§1.1）；域名在整表校验期拒 |

**实现形态（设计门 r19 L2 采纳）**：地址类判定与 why 串**落 typed enum + `text()`**（照 `TableErr`
（`portfwd.rs:78-93`）/`StreamErr`（`stream.rs:135-203`）的**单源**先例）——`DialAddrClass::{Unspecified,
ThisNetwork, Broadcast, Multicast, PortZero}` 各带 `text()`；拒行 `%s` 由 `text()` 产出，**不得**散在
`format!` 里（防「行文与码各写一份」的漂移）。

**SSRF 面的正面回答（本期待点）**：

1. **dial 腿不构成新的权限面**：能发 `STREAM[dial]` 的前提 = **已准入的绑定连接**（设备身份成立，M3
   `serve.rs:89-94` 的 `dev_of_conn` 前置），而同一设备**早已**拥有经出口的**任意目的地址 L3 全局代理**
   （M1 的产品核心功能：它的 TCP 在出口 intercept 终结后由**出口 OS** 拨出，`intercept/mod.rs:228`
   `dial_nonblocking`）⇒ `dial` 腿能到的，L3 腿本来就能到（含 `169.254.169.254`、私网、出口回环）。
   **收紧 dial 的接受集不增加任何安全性，只会打死合法用法。**
2. **与既有拦截口径的关系**（**禁止混用**，逐条写死）：
   - `wgcore/stackb.rs` 的**环回拒绝** = 客户端内部栈的**能力缺失**（栈 B 没有 lo），不是安全策略
     ⇒ **只在 WG 腿继承**（§1.3 的替换），**不得搬到出口**；
   - `probe.rs::probe_addr_acceptable`（拒 private/回环/链路本地/fake-IP/CGNAT）= **候选地址卫兵**
     （客户端要连的**出口/中继端点**，防「被 token 指向内网」）——那是**准入前的信任面**，与 dial 的
     「已认证设备的出口出站」**不是同一面**。**不得复用**（复用即打死 spec 场景②）。
3. **真正的边界**：①准入（token/设备表/绑定连接）；②出口主机自己的网络位置（它的路由表与防火墙）——
   后者本就与「出口是一台被信任的转发节点」一致。
4. **有意的收紧项**（第 5–8 类）：目标不是「缩小 SSRF 面」而是**消灭平台歧义与不可读归因**
   （0.0.0.0 实测连本机 = 最危险的一类歧义）。登记为**有意选择**。

---

## 2. 拨号缝换轨（客户端侧）

### 2.1 换轨点与形态

| 面 | 今天 | M4 |
|---|---|---|
| 生产闭包 | `tun_exec.rs:1267` 的 `dial: Arc::new(move \|dst, budget\| pf_dial_via_run(&w, dst, budget))` | **不变**（同一闭包、同一 `Weak<GenRun>`） |
| `pf_dial_via_run`（**新分派**） | `tun_exec.rs:345-357` 直入 `session_connect_target` | `let Some(r) = run.upgrade()…`；**`if r.l3_on_island()` ⇒ `quic_stream::dial_target(&r, dst, budget)`（QUIC 档）**；否则 `session_connect_target(&r, dst, budget)`（WG 档） |
| `session_connect_target`（WG 腿） | `tun_exec.rs:330-341` 原样 | **保留 + 一行归一**：`let dst = wg_dial_addr(dst);`（环回 ⇒ `SERVER_TUNNEL_IP:port`）；其余逐字不变（`connect_deadline` 在 :339） |
| 新 seam（QUIC 腿） | — | **`facade/quic_stream.rs::dial_target(run, dst, budget)`**（**放本文件**：该文件受隔离门 ⑪(c) 「零 WG 引用」约束） |
| `PfDialFn` 签名 | `Arc<dyn Fn(SocketAddrV4, Duration) -> io::Result<Box<dyn BridgeStream>> + Send + Sync>` | **逐字不变**（`portfwd.rs:344-345`）——这是「阀/泵/计数/热替换逐字保留」的结构前提 |

分派判据用 `l3_on_island()`（M3 S3 桥拨号**同一判据**，`tun_exec.rs:1765`）：岛在世且 L3 在岛上 ⇒ QUIC 档；
其余（WG 档 / 岛未就 / 岛已收回 / 单测合成世代）⇒ WG 腿。**与桥拨号的分派同源同形**（评审时可对照两处）。

### 2.2 QUIC seam 步骤（`quic_stream::dial_target`；预算语义写死）

```text
deadline = now + budget                      // budget = pf 的 cfg.dial_ms（缺省 15s）
① island = run.current_island()              // None ⇒ io::ErrorKind::NotConnected（"世代装配中（数据面未就绪）"）
② id = Cmd::StreamOpen{tag:Dial}，**自有有界等待 remain()**   // 岛内 open_bi；额度耗尽 ⇒ StreamErr::Busy（快速失败）
   shared = Arc<StreamShared{island, id}>    // ★ RAII 守卫：构造即接管「失败也要关流」（见下）
③ write_frame([4B IPv4][2B 端口])            // 6B；走既有 QuicWriteHalf 的 Ok(0) 分级退避环（§1.4 的 n=0 语义）
④ ack = StreamRead（**有界**：remain()）      // 值空间穷举见下
⑤ return Box::new(QuicStream::from_shared(shared).with_pending(rest))   // rest 预置读半缓冲（§1.2 铁律）
```

**★ RAII 关流（设计门 r19 H1，高危，必须写死）**：岛内的流**只**由 `Cmd::StreamClose`
（`driver.rs:1218-1222` → `streams.rs:360-374`）或连接死（`streams.rs:381-400` 的
`clear_on_connection_loss`）从在册表摘除；**对端 reset 不清表**（`read_task` 只回 `Err`，
`streams.rs:502-557`）。而 `has_capacity()` 判 `map.len() < service_capacity()`（缺省 62，
`streams.rs:225-228`）⇒ **每条「回执 0x25/0x26」的失败拨号（= 常态：浏览器探测、目标没起）若不关流，
就在长活连接上永久漏一个槽**，62 条之后同一连接上的 files/term/speedtest/dial **全部** `Busy`。
故：**②之后立即构造 `Arc<StreamShared>`**，任何失败路径**靠 drop 关流**（`Drop for StreamShared`
的 `Cmd::StreamClose` 有界 2s，`quic_stream.rs:172-184`）；成功路径把同一枚 `Arc` 交给
`QuicStream`（客户端半收线时的既有路径不变）。**S2 完成判据**：新增用例「连续 N（>62）次
`0x25` 失败拨号后，岛内在册数回到基线、且随后一条 files 流仍能开」（**在册读数的注入面** = 岛快照
或在册计数；不可读则用「第 63 次失败拨号仍返回 `Refused` 而非 `Busy`」等价断言）。

**④ 回执值空间穷举（设计门 r19 H2，中高）**：

| 读到 | 判定 | 归因 |
|---|---|---|
| `[0x01, rest…]` | **成功**；`rest` 预置进读半缓冲 | — |
| **空块**（`Ok(vec![])`，`read_task` 存在该形态：`pump.rs:136-137` 自己就有 `Ok(Some(0)) => continue` 先例） | **预算内续读**（不得 `[0]` 索引 ⇒ 否则 panic = conn 线程死 + FIN 收口，不是设计要的 RST/计数） | — |
| `[b, …]` 且 `b != 0x01` | `Err(InvalidData)`（协议面错） | pf 链 = 拨号失败 |
| `Err(Busy)` | 立即失败（额度耗尽，岛内快速失败） | `Other` ⇒ pf 链 |
| `Err(Refused)` / `Err(Timeout)` | 类型化归因（出口 `0x25`/`0x26`） | `ConnectionRefused` / `TimedOut` ⇒ pf 链 |
| `Err(Closed)`（= 对端 FIN / 白名单外复位码 / `ConnectionLost`，`streams.rs:546-557`） | **必须显式判为「拨号失败」**——**不得**沿 `QuicReadHalf::read` 的 `Closed ⇒ Ok(0)`（那是 EOF，读成「成功无回执」） | pf 链 |
| `Err(NotSupported/Unbound/BadTag)` | 防御面（出口无该服务/未绑定/对端异常） | 归因照 `stream_err_to_io` |

**③ 的 6B 帧写入前提（设计门 r19 L6，写死）**：开流瞬间待发队列**必空**且容量 64 KiB ≫ 6B
⇒ 单次写入 `n == 6` 是**结构性事实**；实现按「`n == 6` 则成，否则 `Err(WriteZero)`」判（保留 Ok(0)
退避环作防御，但**不承担**「按 `&data[n..]` 重试」的隐含要求）。
**措辞订正（r20）**：写入走**既有 `QuicWriteHalf`**，其 Ok(0) 环带**固定的 10s 无进展上界**
（`quic_stream.rs:276-281`）⇒ 「seam 总时长 ≤ `dialMs`」的**严格上界**是「②的等待 ≤ `remain()`；
③在病态形态至多 +10s」——因 ② 的结构性事实（队列空）**实际不触发**，但**不得**据此声称数学上 ≤ `dialMs`。

**岛侧防御项（r20 建议，采纳）**：`Cmd::StreamOpen` 的 `reply.send` 失败（调用方已放弃/岛正在收工）时，
岛侧**就地关流**（`driver.rs` 的 `StreamOpen` 臂加一条「回执投不出去 ⇒ `streams.close(id)`」）
——否则该流在岛内无句柄可关（seam 拿不到 id，RAII 也就无从构造），是 H1 的**残余窄窗**。

**⑤ 成功路径的余量保真**：`Cmd::StreamRead` 返回块（≤16 KiB）——出口写完回执可能立刻跟目标数据
⇒ 首块可能是 `[0x01, payload…]`；`payload` **必须**预置进读半缓冲（`QuicStream::with_pending` /
`from_shared`），否则**丢一字节 = 应用层帧错位**。S2 判据含专门用例（构造 `0x01+3B` 同块 ⇒ 读半先给 3B）。

- **单次尝试（不重试）**：**不复用** `quic_stream::dial_with`（那是服务流策略：连接面失败重试一次）。
  理由：Q-F-B D11 已定「pf 的常态拒绝不许触发恢复阶梯」；重试只会把「出口不在/目标拒绝」放大成延时，
  且 pf 侧失败立刻 RST 给本机应用（浏览器自己会重连）。**登记为策略面**（§8 行 5）。
- **预算**：各步耗 `remain()`（`deadline.saturating_duration_since(now)`；`≤0 ⇒ Err(TimedOut)`）。
  **②的自有有界等待（设计门 r19 L1 采纳）**：**不复用** `quic_stream::open_stream`（它的等回执预算是
  `budget + OPEN_BUDGET(5s)`，`quic_stream.rs:81-87` ⇒ 会给出 `dialMs+5s` 的假上界）；seam 侧自带
  `island_stream_cmd(.., Some(remain()))`。⇒ **seam 总时长 ≤ `dialMs`**（与今天的
  `connect_deadline(dst, budget)` 同一上界口径；岛内 `open_bi` 自带的 `OPEN_BUDGET` 仍是最后一层兜底，
  属「岛卡死」错配形态，不计入常规预算）。
- **超期/放弃时**：失败路径**靠 §2.2 的 RAII 守卫**发 `Cmd::StreamClose`（有界 2s）
  ⇒ 出口侧流被 reset/STOP_SENDING，`dial` 腿随流收（在途 connect 见 §3.2/§10-W3）。
- **io::Error 映射**：**复用 `stream_err_to_io`（`quic_stream.rs:151-163`）**——`Refused ⇒ ConnectionRefused`、
  `Timeout ⇒ TimedOut`、`Busy ⇒ Other`、`NotSupported ⇒ ConnectionRefused`。0x25/0x26 的映射**已在 M3 备好**
  （该函数 :153 已含 `Refused`、:154 `Timeout`），M4 只需让它们**真的可达**。

### 2.3 「阀 / 两阶段 install / FlowGuard / 计数」逐字保留的**保证方式**

结构论证（**构造性**，不是承诺）：`PfRuntime` 的整条流水线（`pf_bind` → `pf_accept_thread` → `accept_serve`
→ `admit_conn`[阀 + FlowGuard + spawn] → `pf_conn_thread`[dial + 双向泵 + 失败 RST]）**只经 `PfDialFn` 与
外界接触**（`portfwd.rs:985`）；`PfDialFn` 签名与语义不变 ⇒ **承载差异止于该闭包内**。

钉法（三层，缺一不可）：

1. **Q-F-B 批新增的真 socket/状态面用例原样留用**（`portfwd.rs` 的 `mod tests` 内，全部注入桩 `PfDialFn`
   ⇒ 与承载无关；Q-F-B 记「新增单测 22 条 = portfwd 21 + tun_exec/门测试改写」，**本设计点名其中与承载无关的
   18 条**）：`install_reports_per_entry_states`、`inbound_roundtrip_through_pump`、
   `hot_replace_frees_reused_port`/`hot_replace_keeps_established_conns`、`dial_failure_rst_and_counters`、
   `flow_valve_rejects_over_limit`（含 `conn_thread_spawns == 2`）、`stop_all_*`/`generation_stop_*`、
   `install_ack_timeout_retries_then_marks`、`bind_uses_reuseaddr_and_single_attempt`、
   `bypass_config_entries_are_failed_not_bound`、`accept_fatal_*`/`accept_transient_*`/
   `listener_exit_marks_late_fail_on_panic`、`install_atomic_single_swap`、`poisoned_lock_still_stops_all`、
   `pump_bridge_label_and_eof_line`/`pump_pf_label_eof_silent_errors_logged`。
   **判据（可操作；r20 订正计数）**：`git diff` 对 `facade/portfwd.rs` 的 `mod tests` **只允许一处改动**——
   `dial_target_semantics` 的 `resolve()` 断言（`:1208-1211`）；其余用例**零改动**且全绿。
2. **新增「结构不变量」断言**（防后续重构把承载塞进阀/准入）：
   - `PfDialFn` 签名 + `pf_conn_thread` 首行的调用形态（:985）用**编译期**保证（签名不改即编译不过）；
   - `FlowGuard` 的 RAII 语义已有双向用例（`flow_valve_rejects_over_limit` + `conns/flows` 归零断言）。
3. **判据/计数对照**（M4 判据原文「`STREAM[dial]` 与 Q-F-B 的阀 / 计数 / 热替换语义逐条对照」）：
   见 §6 表尾的「逐条对照」小节。

### 2.4 允许改动 / 不允许改动（**白名单**，评审据此核）

**允许改动**（仅这些）：
- `portfwd.rs`：`PfDialTarget::resolve()` 的 `ExitPort` 产物（`:145-150`）⇒ `127.0.0.1:p`；
  **同批允许改**：`resolve()`/`dial_target()`/`PfDialTarget` 的 **doc 注释**（r19 L4——注释里仍写着
  「`ExitPort` = 出口隧道 IP」的话就是假话）；对应单测断言（`:1208-1211`，`assert_eq!` :1208 / `resolve()` :1209）同步改；新增 `resolve()` 类单测。
- `portfwd.rs`：**不新增/不改**任何阀、install、FlowGuard、泵、RST、计数逻辑（**这些**的注释也不动）。
- `tun_exec.rs`：`pf_dial_via_run` 分派 + `session_connect_target` 的环回归一（新小函数 `wg_dial_addr`）+ 单测。
- `quic_stream.rs`：新增 `dial_target` / `QuicStream::from_shared`/`with_pending` + 单测；
  `open_stream` 保持原样（seam 自带有界等待，见 §2.2）。
- `homeway-quic`：`stream.rs` 加 `DIAL_OK`；`exit/serve.rs` 的 dial 臂改调 `exit/dial.rs`；新增 `exit/dial.rs`；
  **`exit/pump.rs` 泛型化**（r19 M7：`upstream`/`downstream` 由 `WriteHalf<UnixStream>`/`ReadHalf<UnixStream>`
  放宽为泛型参数，≈4 行 + 调用侧 turbofish）——**不为 dial 腿写第四份泵拷贝**（残③同族：三份泵拷贝已是
  Q-F-B 残余 3 的对象）；泵的半关语义单测（`pump.rs` 既有）必须全绿；
  **`driver.rs` 的 `StreamOpen` 臂加「回执投不出去 ⇒ 就地关流」**（r20 的 H1 残余窄窗，见 §2.2）。

**不允许**（违反即设计走偏）：改 `PfDialFn` 签名；把承载判据塞进 `admit_conn`/`accept_serve`/`install`；
把 QUIC 档的流账并进 `PfCounters`；为兼容留 WG 哨兵（`SERVER_TUNNEL_IP`）在 wire 上。

### 2.5 隔离门扩展（`tools/check-quic-isolation.sh`；**与实现同批**）

- 现有 ⑪(c) 覆盖：`quic_stream.rs` 整文件零 `wgcore|stackb|connect_deadline|healing_dial|Session`；
  桥拨号闭包块（`BridgeHost::new(` → `set_dial_timeout`）零 WG 引用 + 自校准（`quic_stream::dial` ≥1 且
  `session_connect` ≥1）。
- **M4 需补**：**pf 拨号缝块**（标记 `fn pf_dial_via_run(` → 下一个行首 `}`）——
  零 `stackb::|wgcore::|connect_deadline|healing_dial`；自校准 = 块内 `quic_stream::dial_target` ≥1
  **且** `session_connect_target` ≥1（防「QUIC 分支不在」的空过）；块行数下界断言（防标记漂移，
  照 :465 先例）。`quic_stream.rs` 的零 WG 引用已由既有条款覆盖（`dial_target` 加在该文件即自动受检）。
- **负例自检**（纪律照 M3 S6）：注入一处可见 `stackb::` 到该块 ⇒ **确定红**；写进 S3 完成判据。

---

## 3. 出口侧 dial 腿（`crates/homeway-quic/src/exit/dial.rs`，新增）

### 3.1 为什么直接拨 OS（不走 intercept）

- `STREAM[dial]` 的语义 = 「**出口**去连这个地址」；出口的 OS 网络栈就是「出口可达」的权威定义
  （spec 场景②③）。
- **不走 intercept**：intercept 是**客户端 TCP 的用户态终结/NAT**（它的流来自 DATAGRAM 重放，
  `route_upstream` 才需要 `tunnel_ip`/`local_services` 判定）；把 dial 塞进 intercept 要**伪造客户端
  源地址 + 建 NAT 表项**（复杂且无收益）。**WG 档**之所以「经出口过境重拨回环」是因为客户端的 TCP 终结点
  在 intercept 里（`pkg` 时代的形态）；QUIC 档的 dial 流不经过那层。
- 附带好处：M5 收窄 intercept（DATAGRAM 过境 + DNS 两径）时 **dial 腿零改动**。
- **同源不变量（设计门 r19 M-[中] 采纳，写进代码注释与登记）**：dial 腿与 intercept 的 transit 腿
  **当前都是「无目的地策略的直拨」**——这是两者等价的前提。**若将来（含 M5）给 transit 腿引入目的地策略
  （拦 metadata / 私网 / ACL），dial 腿必须同批同步**（策略单源或同批登记），**不得**让 dial 腿静默成为
  该策略的旁路。M4 只写这条不变量，不实现策略。

### 3.2 步骤（`dial_serve(send, recv, conn_id, ctx)`）

```text
① 读 tag（既有 handle_stream 已读，dispatch 到本函数）
② 读 6B 目标帧（预算 TAG_READ_BUDGET=5s，沿用 M3 现值；读不出/畸形 ⇒ reset(0x25) + 拒行，why 串区分）
③ 地址类判定（§1.4 表；reject ⇒ reset(0x25) + 拒行「目标地址类不可拨（%v：%s）」）
④ tokio::time::timeout(DIAL_BUDGET=10s, TcpStream::connect(dst))
     Err(超期) ⇒ reset(0x26) + 拒行「目标拨号超时（%v；预算 %v）」
     Err(e)    ⇒ reset(0x25) + 拒行「目标拨号失败（%v：%e）」（含 errno 原文）
     Ok(tcp)   ⇒ ⑤
⑤ send.write_all(&[DIAL_OK])  // **回执先于任何目标字节**；失败（对端已 reset/连接死）⇒ **立即返回**
                              // （TCP socket 随作用域 drop；**不得**继续起泵——否则目标连接成活僵尸）
⑥ 受理计数 + 行（E-q5 族：`quic: 服务流已受理（tag=dial dev=%s 第 %d 次）`——**语义 = 拨号成功**）
⑦ 泵（§3.3）⇒ 结束计数 + 行（复用 `quic: 服务流结束（tag=dial，↑%dB ↓%dB，耗时 %v）`）
```

- **出口拨号预算 10s（常量）**：与 WG 档出口侧 `intercept` 的 `dialTimeout`（`intercept/mod.rs:75`）
  **同值**（不是新发明）；缺省客户端预算 15s > 10s ⇒ **正常形态下 `0x26` 由出口先给**（精确归因）。
  客户端预算更短（脏配置 `dialMs<10000`）时客户端先收线，出口的在途 dial 最多再活到自身期限（§10-W3）。
- **拒绝行不加新码族**：`0x25`/`0x26` 已在 M3 的复位码表（`stream.rs:106-109`）与客户端白名单（:173-174）里
  （M3 声明「备而未用」）——M4 只是**让它们可达**，**码表零改动**。

### 3.3 泵（**复用** `exit/pump.rs` 泛型化后的同一份实现；半关逐向传播）

- **复用（r19 M7 采纳）**：`exit/pump.rs` 的 `upstream`/`downstream` 泛型化后，dial 腿以
  `TcpStream` 的读写半为参数调用**同一份实现**（不写第四份泵拷贝）；dial 腿只在**拨号那一段**有专属代码。
- 上行（客户端 → 目标）：`copy(recv → tcp_w)`；客户端 FIN ⇒ 目标 `shutdown(WRITE)`（半关传播；
  `pump.rs:145` 同款）；对端 reset ⇒ 收本方向（不跨层错误码）。
- 下行（目标 → 客户端）：`copy(tcp_r → send)`；目标 EOF ⇒ `send.finish()`（**不用 reset 做正常收口**，
  M3 §1.3）。
- **目标侧异常结束的可观测面（r19 L5 采纳）**：目标 RST / 读错误以 `finish()` 收口（与 M3 现装一致），
  而客户端对「白名单外复位码 / FIN」都归 **EOF**（`streams.rs:550-556` + `quic_stream.rs:242`）
  ⇒ **客户端侧的 `port-forward[down] 读错误` 行在该形态下会消失**（今天 WG 档在某些 `ConnErr` 变体下会记）。
  处置：**出口侧补一行**（`quic: 服务流目标侧中断（tag=dial，%s 方向：%e）`，additive），把可观测面回到出口；
  客户端侧的行为差异**登记**（§8 行 12）。
- 缓冲 8 KiB（`COPY_BUF` 同值）；无新线程（跑在出口 `current_thread` runtime 的任务里，`JoinSet` 托底）。
- **字节计数口径写死（r19 M7 采纳）**：`stream_bytes_in/out` = **泵搬运的字节**（**不含 1B 回执**）
  ——回执写在泵之前、不计入（否则 `↑%dB ↓%dB` 与目标真实字节差 1）。
- **不接受 intake 队列**：dial 腿不占服务资源（与 probe 同档），并发上界见 §7 的两口径。

### 3.4 生命周期

- 流收 / 连接收 / 出口收工：`serve_streams` 的 `JoinSet` drop = abort（`serve.rs:64-65` 既有）；
  TCP socket 随任务 drop 关闭；在途 connect 随 future drop 取消（tokio 语义）。
- **客户端预算先到期**的在途窗口：见 §10-W3（实测锚点 §12-C2）。

---

## 4. 失败归因（四形态 target 文案 × 三失败类；逐条「从 → 到」或「不变」）

`pf_target_text`（`portfwd.rs:123-132`）**零改动**（NAPI 面；四形态 = 「主机（同端口）」/「主机:{port}」/
「{ip}:{listen}」/「{ip}:{port}」）。失败归因的**对外两条链**逐条对照：

**链 A（本机应用可见）**：`pf_conn_thread` 的 `Err(e)` 支路 ⇒ `fails += 1` + 节流行
`port-forward: {listen} -> {target} 拨号失败 #{n}: {e}`（**Go 逐字行文不变**）+ `rst_close_tcp(conn)`
⇒ 本机应用 `read` = `ConnectionReset`。

**链 B（出口侧可观测）**：`quic: 服务流拒（dev=%s tag=dial；%s（0x%02x）；第 %d 次）`（E-q5 族，**新增取值**）。

| 失败类 | 今天（WG 档，Q-F-B 实测） | M4（QUIC 档） | 变/不变 |
|---|---|---|---|
| **回环拒绝**（历史形态：127/8 被客户端栈拒 ⇒「监听中但连不上」） | **不存在**（已由 `ExitPort` 映射绕开；客户端栈的环回拒绝是 A1① 的能力缺失） | **仍然不存在**，且**出口侧明确不拒环回**（§1.4 第 1 类）；wire 即 `127.0.0.1:p` ⇒ 出口回环直拨 | **不变**（语义保留）+ **订正**：哨兵由 `SERVER_TUNNEL_IP` 改为 `127.0.0.1`（§1.3；登记） |
| **未监听**（目标端口无服务） | 出口 intercept 拨 → `ECONNREFUSED` → 客户端 `ConnErr::Refused` → `ConnectionRefused` | 出口 dial 腿 `ECONNREFUSED`（实测 201µs）⇒ `reset(0x25)` ⇒ `StreamErr::Refused` ⇒ `stream_err_to_io ⇒ ConnectionRefused` | **等价**（`fails` + RST + 行文全同；行内 `{e}` 文本变为「QUIC 服务流：目标拒绝（目标拒绝）」） |
| **拨号失败**（不可达/超期/网络错） | `ConnErr::Timeout` ⇒ `TimedOut`（`conn_err_to_io:364`） | 超期 ⇒ `reset(0x26)`（出口先到，缺省）⇒ `StreamErr::Timeout` ⇒ `TimedOut`（实测 10.002s）；不可达 ⇒ `0x25` ⇒ `ConnectionRefused`（**实测 §12-P1 的 errno 族**） | **等价**（kind 面：`TimedOut` 保留；「不可达」从 `Other`（WG 的 `ConnErr::EngineGone/Closed` 兜底）收敛到 `ConnectionRefused`——**归类更准**，登记为「kind 面变化」） |
| **额度耗尽**（**新增可达形态**） | 无（栈 B 无并发上限；`TooManyConns` 是死变体，M3 已删） | 岛内自记账（容量 = `max_bidi − 2` = 62）⇒ `StreamErr::Busy`（快速失败，不等 5s）⇒ `Other` ⇒ `fails+1` + RST | **新增**（登记；§10-W1） |
| **回执面异常**（**新增可达形态**，r19 H2） | 无对等物 | 回执读得 `Closed`（对端 FIN / 白名单外复位码 / `ConnectionLost`，`streams.rs:546-557`）⇒ **显式归「拨号失败」**（**不得**被读成 EOF/成功）⇒ `fails+1` + RST；空块 ⇒ 预算内续读（不计失败） | **新增**（§8 行 5 的输入集含此例） |
| **出口无该服务**（防御：对端不是本产品） | — | `0x22` ⇒ `NotSupported` ⇒ `ConnectionRefused`（与今天「出口活着、端口没服务」同链） | 保持 `stream_err_to_io:153` 的既有映射（零改动） |
| **端口 0 旁路**（r20 三处机制统一：**以 §1.4 行 11 为准**） | `SERVER_TUNNEL_IP:0` ⇒ 出口回环拨 0 ⇒ 失败 | `127.0.0.1:0` **在拨号前的地址类判定被拒**（§1.4 行 11）⇒ `0x25` + why「端口 0 不是可拨端口」 | **等价（都失败）**；归因串从「拨号 errno」改为**判定期拒绝**（§8 行 8 已同步） |

**四形态 × 三失败**（逐条给判据，S5 的 e2e 用它做表）：

| `pf_target_text` 形态 | 未监听 | 拨号失败 | 归因行（本机/出口） |
|---|---|---|---|
| 主机（同端口） | `127.0.0.1:listen` 无服务 ⇒ `0x25` | 出口回环不可达（极端）⇒ `0x26` | `…拨号失败 #n: QUIC 服务流：…` / `目标拨号失败（127.0.0.1:listen：Connection refused…）` |
| 主机:{port} | 同上（换端口） | 同上 | 同上 |
| `{ip}:{listen}` | 出口 dial `{ip}:listen` 拒 ⇒ `0x25` | 超期 ⇒ `0x26` | 同上（`{ip}` 回环时 = 形态 1） |
| `{ip}:{port}` | 同上 | 同上 | 同上 |

---

## 5. NAPI `ClientCoreTunRecover` 未分档——**本期修（最小分档）**

### 5.1 事实链（回源码；M3.md §5 的行号已漂）

`homeway-capi/src/lib.rs:148`（`ClientCoreTunRecover`）→ `facade/mod.rs:457`（`tun_recover`）
→ `tun_exec.rs:903`（executor `recover`：只做 gen/stale 检查，恒 `run.recover(lvl, cause)`）
→ `tun_exec.rs:644`（`recover`）→ `run_round`（:708）⇒ **隧道域 WG 阶梯**（`TunnelTransport` 用 `Client` 的
`rebind/rearm/reset_peer_session`）。**全链无承载分档**（对照面 = **四处既有内部触发点**均以
`l3_on_island()` 分档：挂起唤醒 `:2370`（**否定式** `if !run.l3_on_island()` ⇒ 回落 WG 阶梯）、
巡检失败 `:2555`、3 连败兜底 `:2574`、待发包下推 `:2738`（**三处均为正守卫** `if run.l3_on_island()`
⇒ QUIC 档只留痕「不重复动作」）——**r20 订正**：不是「四处都写否定式」）。

### 5.2 后果（QUIC 档）

①产 `RECOVER R1/R2/R3` 族行（与「QUIC 档不再产生 C11 族行」的 M3 登记相抵）；
②在同步 NAPI 调用里跑 WG 动作（最长 ≈13s/档；tier 侧是 `clientCoreTunRecoverAsync` 异步导出，
所以不占 JS 线程——但**动作是错的**）；③与岛内快探阶梯**两条真相面**。

### 5.3 方案（推荐；**rc 值域不变**，但**可达集与归因面变化须登记**）

**最强理由（r19 L8，先说）**：**今天 QUIC 档的下推 rc 与 QUIC 数据面无关**——`from=3`（tier 的换网语义）
跑的是**一条已不是 L3 承载**的 WG 连接的 rebind/赛跑（`:708` 的 `TunnelTransport` 持 WG `Client`），
它返回的 `0/-1` 既不能证明也不能证伪岛上的连通性 ⇒ 这是**错误证据源**，不是「可用但不够好」。

`tun_exec.rs:903` 的 executor `recover` 按**有效承载**分档（**r19 M3 采纳：用 `l3_on_island()`，
不用 `run.bearer`**——`bearer=Quic` 但岛未就时本世代按 WG 跑，`:1425-1445` 的 `set_l3_on_island(false)` +
「本世代回落 WG 承载」行；用 `bearer` 分档会（a）对一条**确实 attached** 的 WG 隧道谎报「无 attached」，
（b）在「QUIC 分支」里经 `l3_probe` 回落 `path_probe` ⇒ 分支里跑 WG 动作）：

| 判据 | 行为 | rc |
|---|---|---|
| 世代未 attach / stale（**既有前置，不动**） | 不做动作 | **`-2`**（既有语义） |
| `!l3_on_island()`（WG 档 **或** QUIC 档岛未就/已收回 = 实际承载是 WG） | **原路**：`run.recover(lvl, cause)`（WG 阶梯逐字保留） | 既有值域（含 `-3/-4`） |
| `l3_on_island()` | **快探 + 一次复探**（**r19 M5 采纳**）：`l3_probe(&run, FAST_BUDGET)` 失败 ⇒ 立即用 `FAST_BUDGET × reprobe_factor(=2)` 再探一次（与岛内阶梯**同一判负粒度**，`tuning.rs:217/222`） | 通过 ⇒ **`0`**；两次都失败 ⇒ **`-1`**（「走完未恢复」⇒ tier 整套重建） |

- **`l3_on_island()==true` 的世代：不跑 WG 阶梯、不做 WG 动作、不产 C11 族行**（回落世代见上表第 2 行）；新增一行 additive 归因（**新行，非改写既有行**）：
  `quic: 恢复下推（%s）——按承载分档（岛快探%s：%s）`（`%s` = cause / 「+复探」或空 / `通过|失败（%s）`）。
- **真实时间上界（r19 L7）**：探段 = 700ms + 1.4s = **2.1s**；`l3_probe` 的 RPC 等待是 `budget + QUIC_RPC_BUDGET(5s)`
  ⇒ **最坏 ≈2.1s + 5s = 7.1s**（该 5s 只在「岛 RPC 卡死」的错配形态吃满）；tier 侧是
  `clientCoreTunRecoverAsync`（`tailcat_napi.cpp:95-150` 的异步导出）⇒ **不占 JS 线程**。
- **rc 可达集与归因面变化（r19 M4 采纳、r20 精确化，单列登记 = §8 行 13）**：tier 的消费链
  （`TierVpnExtensionAbility.ets:1228-1242`）**只有 `rc===0` 跳过整套重建**，`-1/-2/-3/-4` 全落重建；
  `attribute()`（`:1094-1104`）把 `-3/-4` 当「本机网络栈」归因。**可达集要说全（r20 点名的自相矛盾）**：
  ①**`l3_on_island()==true` 的世代**（= QUIC 档且岛真承载 L3）只产 **`0/-1/-2`** ⇒ `-3/-4` 在该世代**不可达**；
  ②**`bearer=Quic` 但岛未就的回落世代**（`:1444` 置 false，`bearer` 仍是 Quic）走**表第 2 行 = WG 原路**
  ⇒ `-3/-4` **仍可达**（该世代的 L3 实际在 WG）。③`-2` 的构成从「无 attached/陈旧」扩到「岛不在/未 attach」
  ⇒ **tier 的 `-2` 日志文案（「当前无 attached 隧道」）在该形态下不实**。故：**删掉「零 rc 语义变更」的措辞**
  （值域不变、**可达集与归因面变**，如实登记）；tier 决策（`0` 跳过重建 / 其余重建）**不变**。
  **凡本文他处出现「QUIC 档只产 `0/-1/-2`」「零 C11 行」的断言，一律限定为「`l3_on_island()==true` 的世代」**
  （r20 点名的 §8 行 9 / §11-S4③ / §9⑦ 已按此限定）。
- **为什么不是「什么都不做返 0」**：返 0 而零证据 = 谎报「某档通过」（本仓纪律：不谎报）。
  一次 700ms 快探（+ 复探）是**最便宜的真话**，且与 M3 §3.2 的「快探 = 唯一能在 3.5s 内定音的判据」同源。
- **为什么在 M4 做（而不是转交 M5）**：
  ①它是路线文件点名的「M4 设计门首条」（M3.md §5/§7 与路线「下一步」第 2 条）；
  ②代价小：1 个函数 + ~30 行 + 2 条用例，rc 值域与 tier 契约**零变更**；
  ③M4 的真机验证要跑换网/恢复场景，未修则真机证据里会混入 RECOVER 族行与 WG 动作（**证据被污染**）；
  ④M5 删 `wgcore`/`session/recover` 后**必然**要改成 QUIC 形态——**本设计给出定稿语义，M5 只删不设计**
  （否则 M5 的「大删码期」要临时补一次设计决策，风险更高）。
- **若主会话裁定转交 M5**（备选）：本 §5 的表格即 M5 的输入；M4 只登记「未分档」为**已知差异**并在
  §8 的登记行里写明承接期 = M5。两条路都不阻塞 M4 的拨号缝换轨。
- **可 falsify 的真机判据（r19 M5 采纳，预登记）**：真机换网（WiFi↔蜂窝）N 次采集
  ①`rc=-1` 的**误判率** = 「返 -1 后 10s 内岛内自愈/迁移成功」的次数占比（预登记阈值 **≤ 1/5 次**）；
  ②QUIC 档日志**零** `RECOVER R1/R2/R3` 行；③下推返回耗时 ≤ 8s（上界 7.1s + 余量）。
  **超阈值 ⇒ 触发备选**：预算提到 `PROBE_TIMEOUT`(10s) 或改「岛内阶梯下推入口」（需新 `Cmd`，本期不做）。

---

## 6. spec 不回退核验清单（tier `port-forwarding` spec **全条**）

spec 结构（只读真源，`~/Documents/projects/tier/openspec/specs/port-forwarding/spec.md`）：
**5 条 requirement / 10 个 scenario**（+ 结尾的「实现现状（归档时补记）」段）。
**覆盖口径（r19 M6 采纳）**：下表**逐 requirement** 给三类证据——①**正文 SHALL 条款**（无 scenario 的那些）
②**scenario** ③**结构性/零触碰**声明；凡只能靠「Q-F-B 已验 + M4 零触碰」成立的，**明写**，不冒充 M4 验证面。

| # | requirement（含 scenario） | 本机怎么验（命令/断言） | 真机怎么验 | 预期结果 | 必须真机？ |
|---|---|---|---|---|---|
| **R1** | **端口映射的建立与访问**（S①出口主机自己的服务 / S②出口可达的其它 IP / S③与分流模式无关） | **正文 SHALL**：①「在手机上监听 `127.0.0.1:<listen>`（**仅回环，不暴露局域网**）」= 结构性（`pf_bind` 硬编码回环，`portfwd.rs:737-780`；**M4 零触碰**）②「**不依赖 TUN 路由**」= 结构性（pf 拨号是 QUIC 流面/裸拨，**不查路由表**；M4 后更强——QUIC 档连 stackb 都不经）③「**不受应用绕过名单影响**」= 结构性（核侧 `facade/**` 无 bypass/split 配置面；分流的开关只在 App/出口侧不参与 pf 路径）——**三条都请 S5 用「配置面不存在 + 代码路径无消费点」的 grep 断言钉住（而非只靠叙述）**。<br>**Scenario**：`tools/quic-pf-e2e.sh` 起本地私有出口 + `cargo test -p homeway-core --test quic_pf_e2e -- --ignored`：真世代（`TunExec`+`tun_attach` 假 TUN fd，照 `quic_island_e2e.rs:606-608`）装表 `{"listen":L,"targetIp":"","targetPort":T}` ⇒ 断言 ①`127.0.0.1:L` 真可连 ②出口回环目标上跑 echo ⇒ 字节往返逐字节一致 ③`targetIp=192.168.3.12`（本机直连网段）⇒ 往返一致（**不得用非直连网段**，§0.4-2）④杀进程/空表 ⇒ 端口真释放 | 装机 + 免点屏 token（`DEVICE-TEST-OHOS.md` §3）⇒ App 端口转发页加映射 ⇒ 手机浏览器 `http://127.0.0.1:L` | ① 加载成功；② 目标服务有响应；③ IP 分流开关两态下都通 | **S①/S③ 必须**（App 侧浏览器与分流开关）；S② 本机可（+真机复核更佳） |
| **R2** | **映射配置随主机持久化与级联删除**（S①删除主机后映射消失 / S②改动后重连生效） | **正文 SHALL**（持久化/仅当前主机生效/重连后生效/配置页提供重连入口）：**核侧无此面**（持久化在 tier `HostStore`）；核侧只验「装表」接口与 rc：`tun_set_port_forwards` 的 `0`/`-1`/`-2` 三态（`facade/mod.rs:500-516`）+ 单测 `port_forwards_gate`（:805+）——**M4 零触碰** | ①删主机 ⇒ 重进页面映射为空（不复活）②页内新增一条 + 「立即重连」⇒ 新端口开始监听、浏览器可访问 | 与 spec 场景逐字一致 | **必须**（持久化 + 级联删除 + 重连生效都在 App/扩展侧） |
| **R3** | **映射状态可见**（S①端口被占用单条失败、其余正常 / S②未连接时状态展示 / S③未知错误码兜底；MUST：失败映射带稳定 `code`，App 只按 code 分派） | **正文 SHALL**（未连接显「未连接」/已连接显「监听中或失败及原因」/失败不影响隧道与其余映射/经现有状态通道推送）：单测面 = `bind_failed` 真值（`install_reports_per_entry_states`）、空码面（`bypass_config_entries_are_failed_not_bound`、`failed_with(err,None)`）、「不影响其余」= `install` 逐条独立性 + `lns_len==4` 断言；**MUST（code 稳定枚举）** = 词表门 `tools/check-vocab.sh`（`portfwd/err` 单元）+ `portForwards[].code` 值域零改动。**状态 JSON**：`tun_status()` 的 `portForwards[]` 逐键断言（`state/err/code/conns`）+ 真世代 e2e 断言 `state=="listening"` 与 `conns` 随连接 ±1 + 热替换 rc=0/-1/-2 | ①占用端口 ⇒ 页面「失败 · 端口被占用」②未连接 ⇒ 「未连接」③状态经 5s 事件泵/回推刷新（无新轮询） | 页面渲染与 `code` 分派一致；**核侧零新键**（additive 之外不动） | **必须**（渲染面在 App；核侧只保证 JSON/code 真值） |
| **R4** | **配置校验**（S①非法输入被拒） | **正文 SHALL**（listen 1024–65535 / 目标空或 IPv4 字面量 / 同主机不重复 / 上限 8 条 / 失败就地显示不保存）：核侧值域与规则 = `validate_table`（`portfwd.rs:96-119`）逐条单测 + NAPI 门 rc=`-2`（`port_forwards_gate`）；**「就地显示不保存」= App 表单面**（核侧只能保证拒绝值不落表） | 表单填 80 或域名 ⇒ 就地报错、不保存 | 与 spec 场景一致 | **必须**（表单面）；核侧本机可验 |
| **R5** | **出口侧回环目标不经代理**（S①跨机代理的出口映射本机服务） | **vacuous 达标（r19 M6 采纳，不冒充验证）**：该条的前提「出口配置了转发代理」在 Rust 出口**不存在**——全仓 `forward-via-proxy` **零命中**（`crates/`+`tools/`+`docs/` 实测 0；spec 自己的「实现现状（归档时补记）」段已写「**已随旧栈退役**，转发出站一律走系统默认路由，回环目标天然在出口本机直连」）。M4 的可验部分 = 「回环目标确实在出口本机直连」：本机 e2e 断言「目标空 ⇒ wire `127.0.0.1` ⇒ 出口本机 echo 命中」（§12-C1-A） | 出口起一个 8080 服务 + 映射目标空 ⇒ 手机访问 `127.0.0.1:L` 命中出口本机服务 | 命中出口主机自己的服务 | 本机可（真机复核） |

**spec 原文两条「已达标/有意偏离 → 达标」的复核（Q-F-B 收口态，M4 不得回退）**：
- 「映射的建立与访问」SHALL（真监听 + 经隧道转发 + 与分流模式无关）⇒ M4 后由**新承载**满足（§R1 表）；
- 「映射状态可见」的失败码 MUST（`bind_failed` 真值 + 空码面）⇒ **核侧零改动**（`PfState`/`code`/JSON 不动）。

**逐条对照：`STREAM[dial]` 与 Q-F-B 的「阀 / 计数 / 热替换」**（M4 判据原文）：

| 面 | Q-F-B（`72f1325`/`129f1c4`） | M4 | 判定 |
|---|---|---|---|
| 规则上限 | `MAX_PF_RULES=8`（装配期 `TooMany` ⇒ rc=-2） | 不动 | **不变** |
| 并发流阀 | `MAX_PF_FLOWS=256`，**在 accept 线程判定**、拒绝型、`flow_rejected` 计数、行 `并发流已达上限` | 不动；**但实际生效上界 = 岛 bidi 额度 − 2（缺省 62）** ⇒ 阀通常先于它不可达 | **阀逐字保留 / 生效上界变化登记**（§10-W1） |
| 计数 | `accepted`（准入 +1，阀拒绝不计）/`fails`（拨号失败）/`flows`（RAII）/`flow_rejected` | 不动；`fails` 的**输入集新增**「承载额度耗尽/流面拒绝」 | **不变 + 输入集登记** |
| 热替换 | 两阶段 install（只 take 旧 lns + 单次换入）+ 退出 ack 400ms + 3×50ms 重试 + 双记行；**已建立连接不被打断** | 不动（与承载无关） | **不变** |
| 拨号预算 | `cfg.dial_ms`（缺省 15s）在**客户端**侧；出口侧无独立预算 | 客户端 ≤`dialMs`；**出口 ≤10s**（新常量，= intercept `dialTimeout` 同值） | **新增（出口侧预算）登记** |
| 失败收口 | 拨号失败 ⇒ `fails+1` + **RST**（`SO_LINGER(0)`）+ 节流行（`<=5 ∥ %20`） | 不动（1B 回执保证「拨号成败」在 seam 返回前已知） | **不变**（这是 1B 回执的存在理由） |

---

## 7. Q-F-B 残余 14 条逐条对照（`docs/reviews/QFB.md` §6；**承载相关**项重点）

| # | 残余项 | 承载相关？ | M4 处置 |
|---|---|---|---|
| 1 | 真机浏览器 E2E 四场景（用户触点） | **是** | M4 以**新承载**重跑（§9；四场景 + 状态可见），Q-F-B 的待办由 M4 的验证收口（记录进 `M4.md`） |
| 2 | tier 两处文案（dirty 兜底 / rc 日志） | 否 | 已随 Q-F-B 自愈，不动 |
| 3 | CLI `cmd_portfwd` 未切核心实现（第三份泵拷贝） | 否（CLI 是 WG-only 测试动词） | 不动（M5 随 WG 面处理） |
| 4 | 已建立连接**无空闲回收、无速率整形** | **是**（新载体的资源账） | 语义不变（Go 同形）；新承载下每流占用 = **4 MiB 流接收窗（不止是上界——它是流控信用）+ 8 MiB 连接级接收面聚合闸（与流数无关，`tuning.rs:37/45` 的 S9 定值）+ 64 KiB 待发 + 出口 OS socket + 2×8 KiB 泵缓冲**；§7 的预算表按此重述（**r19 M1 订正：旧文写的 256 KiB/流是吞吐整改前值，已作废**） |
| 5 | 跨世代换代同端口瞬时 `EADDRINUSE` 窄窗 | 否（pf 结构层） | 逐字保留（3×50ms 重试 + 双记行） |
| 6 | 收工 join/ack 到点 detach | 否 | 逐字保留；新增面 = dial 流随连接收（`JoinSet` drop） |
| 7 | `TunExecutor` 默认 `request_port_forwards = -1` | 否（NAPI 面） | 不动（无承载 = 真话） |
| 8 | 破损配置空码路径（F8） | 否 | 不动（`failed_with(err,None)`） |
| 9 | 引擎侧连接表无独立上限（阀是唯一界） | **是** | **口径变化（登记，r19 M2 订正）**：QUIC 档的 dial 腿**不进** `intercept::MAX_CONNS=1024` 账。**两个口径**：①**持续态** = 每设备 ≤62 条（= `max_bidi − 2`）× 在用设备数（≤设备表上限 32）≈ **1984**；②**强制态**（全表打满 + 连接上限）= `conn_cap = 2 × max_devices`（`exit/mod.rs:187-189`，缺省 **64** 连接）× 62 ≈ **3968（≈4096）**——**登记取后者**（与 `exit/tests.rs:490` 钉的 64 一致）。**不新增全局阀**的理由（补强，r19 M2）：对照组 = intercept 的**已登记**全局最坏 `MAX_CONNS(1024) × ~1MB ≈ 1GB`（`intercept/mod.rs:397-400`，Q-B 口径）——dial 腿每条 <64 KiB 缓冲、无 NAT 表项，比 L3 面低一个量级；且上界由「bidi 额度 × 连接上限」**构造性**给出 |
| 10 | 引擎缓冲按连接固定 2×1 MiB（阀 256 按此定） | **是** | **口径变化（登记）**：WG 档仍 2 MiB/连接（stackb）；QUIC 档接收面 = **每流窗 4 MiB + 连接级 8 MiB 聚合闸**（S9 定值）+ 待发 64 KiB/流 ⇒ 阀 256 的「最坏 512 MiB」论证**在 QUIC 档不成立**（真正的墙 = **8 MiB/连接聚合 + 256 流 × 64 KiB 待发**）；且 QUIC 档实际生效并发上界 = 62（bidi）而非 256（§8 行 11） |
| 11 | `stats_loop` tick 下限 5s | 否 | 不动（`stats_line` 纯函数已解决可测性） |
| 12 | accept 致命错误「转 failed」偏离 Go | 否 | 不动 |
| 13 | 「macOS accepted socket 无 `SO_NOSIGPIPE`」 | 否 | 已判不成立（宿主 ABI = linux），不重查 |
| 14 | `install` 的按需语义（旧表在途连接不计入新表 `conns`） | 否 | 不动（Go 同形） |

**与承载相关的 4 条（1/4/9/10）全部在 §8/§10 有登记或处置**；其余 10 条与承载无关（M4 零触碰）。

---

## 7A. 预算（体积 / 性能 / 内存；路线「每期执行协议」第 2 条要求；**r19 M1 采纳后按 S9 定值记账**）

| 维度 | 增量（估;实施期实测入册） | 判据/方法 |
|---|---|---|
| **体积** | 新增 = `exit/dial.rs`（≈150 行：判定表 + 拨号 + 回执 + 复用泵）+ `serve.rs` 派发（~10）+ 客户端 seam（~60）+ 分派/归一（~30）⇒ **估 +6…12 KB**（`.so`）；删除 = 0 | `tools/build-app-core.sh` 的体积门 + `tools/quic-ab.sh size`（**实测入册**；3.8MB 阈值按设计属 M5 判） |
| **每包 CPU** | **不涉 L3 每包路径**（服务流面） | 无需新臂；如触发回归用 `tools/quic-ab.sh cpu`（独占机器） |
| **内存（每连接）** | **接收面最坏 = 8 MiB/连接**（连接级聚合闸，`tuning.rs:45`，**与流数无关** —— S9 定值；「64×每流窗」的旧账已作废）。**每 dial 腿增量** ≈ 2×8 KiB 泵缓冲 + 1 个出口 socket（+ 内核 socket 缓冲）；**出口侧 fd 上界** = `conn_cap(64) × 62 ≈ 3968`（强制态；持续态 ≈ 每连接 62 × 在用设备数） | `tools/quic-ab.sh mem`（vmmap 三轮下中位；稳态/负载态两档）+ 本机 e2e 的 fd/在册读数 |
| **每流窗与吞吐** | 流窗 4 MiB（S9 定值）/ 连接级聚合 8 MiB / 待发 64 KiB；**pf 大文件下载吞吐**不劣于现状 | `tools/quic-ab.sh` + S5 的 e2e 往返（如需相对门槛 = 「同刻同承载同操作 ≥0.95×」（M3 口径） |
| **预算可测性** | 上述每项都给了命令与读数落点；**唯一不设门槛**的是「pf 并发 62 的观测」（W1，只登记 + 真机打点） | — |

---

## 8. 判据行影响 + 登记条目草案（五字段；日期 = **实施批当日**，此处占位）

### 8.1 分类总表

| 类 | 条目 | 动作 |
|---|---|---|
| 判据行（编号族） | E-q5（出口服务流行族）、C19（客户端服务流行族） | **扩展取值集 / 可达性变化**：`tag=dial` 真产出；`%s` 取值新增（见下） |
| 判据行（编号族） | C11（RECOVER 族） | **触发集订正**：**`l3_on_island()==true` 的世代**里 NAPI 下推不再产 C11 族行（回落世代仍走 WG 原路，§5/§8 行 9） |
| 新增观测行（additive） | 出口 dial 归因三条 + 客户端 `quic: 恢复下推` 一条 | 登记 |
| **wire（非判据行）** | dial 流的 1B 回执（出口→客户端） | 登记（M3-design §1.2 的「裸字节管」句订正） |
| 计数输入集（行文不变） | `stats.streams_open/stream_refused` 的 tag=dial 时点；`pfFails` 的输入集 | 登记 |
| 行为差异（非判据行） | `targetIp=100.64.255.1`、虚拟端口 7802/7724/7803、`target_port=0` 旁路、`io::ErrorKind` 归类 | 登记 |
| fixtures / 词表 / tier 文档 | 无（`fixtures/` 零改动；词表门零改动；tier `port-forwarding` **零改动**） | 零改动复核 |

### 8.2 登记条目草案（逐条五字段）

> **行 1（判据行扩展——E-q5）**｜日期：**实施批当日**｜条目：`E-q5 服务流出口行族`（`INTEROP-CRITERIA.md`
> 的 M3 S7 行；tag=dial 真产出）
> **从**：`tag=dial` 的拒行 why 恒为 `服务不可用（dial 目标 %v；M3 只定协议，M4 换轨）`（一律 `0x22`）
> **→ 到**：why 取值集新增 ①`目标地址类不可拨（%v：%s）`（0x25；`%s` ∈ {未指定 0.0.0.0 / 本网络 0/8 /
> 受限广播 255.255.255.255 / 组播 224/4 / **端口 0**（§1.4 行 11）}）②`目标拨号失败（%v：%e）`（0x25）
> ③`目标拨号超时（%v；预算 %v）`（0x26）④`目标帧未读出（%v）`/`目标帧畸形`（0x25）；**受理行**
> `quic: 服务流已受理（tag=dial dev=%s 第 %d 次）` 的语义 = **拨号成功之后**（其余 tag = 入队即受理）；
> 结束行沿用 `quic: 服务流结束（tag=dial，↑%dB ↓%dB，耗时 %v）`
> **原因**：M4 把 `exit/serve.rs::dial_refuse` 换成真拨号腿（`exit/dial.rs`）
> **影响面**：出口排障读者；`crates/homeway-quic/src/exit/{serve,dial}.rs` 单测；任何按「dial 一律 0x22」
> 写断言的测试须改（现存自检 `exit/serve.rs:278-284` 的语义随之调整）

> **行 2（判据行可达性——C19/复位码）**｜日期：**实施批当日**｜条目：`C19 客户端服务流行族` + 复位码
> `0x25/0x26`
> **从**：`0x25/0x26` 在码表与白名单里但**恒不产出**（M3 声明「M4 的码」）；`StreamErr::{Refused,Timeout}`
> 的 `text()` = `目标拒绝`/`服务流超时`（不可达）
> **→ 到**：同一取值**真可达**（由 dial 腿产出）；`%s` 取值集**不变**（仅可达性变化）
> **原因**：同上｜**影响面**：`stream.rs` 的码表单测（`:298-346`）语义注记；`quic_stream.rs:151-163` 的映射
> 用例（Refused⇒ConnectionRefused 已有断言，M4 起成为**主路径**）

> **行 3（**新增 wire 元素**——非编号判据行）**｜日期：**实施批当日**｜条目：`dial 流的出口→客户端 1B 回执`
> **从**：M3-design §1.2「`tag=4` 后 = 裸字节管」（双向）
> **→ 到**：客户端→出口方向**不变**（6B 目标帧后裸字节）；**出口→客户端方向首字节 = `0x01`（`DIAL_OK`）**，
> 之后才是目标侧字节；失败路径不写回执（改 `reset(0x25/0x26)`）
> **原因**：客户端 seam 必须在返回前知道拨号成败（否则 Q-F-B 的「失败 ⇒ RST + `fails` + 行」退化）
> **影响面**：M3-design 该句（**订正指针**）；`crates/homeway-quic/src/stream.rs`（`DIAL_OK` 常量）；
> 两侧 seam/腿；`docs/INTEROP-CRITERIA.md` 的 E-q5/C19 说明段

> **行 4（新增观测行 additive）**｜日期：**实施批当日**｜条目：新增行族
> **从**：无 → 有：①（出口）`quic: 服务流拒（dev=%s tag=dial；…（0x%02x）；第 %d 次）` 的四条 why（见行 1）
> ②（客户端）`quic: 恢复下推（%s）——按承载分档（岛快探：%s）`
> **原因**：M4 的 dial 腿与 NAPI 分档需要各自的归因面
> **影响面**：出口/核日志读者；排障脚本（`docs/DEVICE-TEST-OHOS.md` §5 的速查串可增补）

> **行 5（策略/计数输入集；行文不变）**｜日期：**实施批当日**｜条目：`pfFails`（`stats.pfFails` + `stats:` 行
> 两位 + `port-forward… 拨号失败 #n` 行的触发集）
> **从**：输入集 = 「WG 裸拨失败（`ConnErr` 归因）」
> **→ 到**：输入集 = 「**承载拨号失败**」——含 `0x25/0x26`（出口拨号失败/超期）、**额度耗尽**
> （**本地** `StreamErr::Busy`：`driver.rs:1167-1175` 的 `has_capacity()` 分支，**无复位码**——
> 错：`0x23=INTAKE_FULL` 只由 intake 路径写出，**dial 腿无 intake ⇒ 对 dial 不可达**，r20 订正）、
> **回执面异常**（读得 `Closed`/`ConnectionLost`——r19 H2）、`NotSupported/Unbound/BadTag`（防御面）；
> **行文形态与节流窗（`<=5 ∥ %20`）逐字不变**；`{e}` 的字面取值变为「QUIC 服务流：…（…）」
> **原因**：M4 换轨｜**影响面**：`facade/portfwd.rs` 的计数单测（`dial_failure_rst_and_counters`）；
> 「`{e}` 逐字」类断言须改按「前缀 + 非空」口径

> **行 6（行为差异；非判据行）**｜日期：**实施批当日**｜条目：`portForwards[].targetIp = 100.64.255.1`
> （出口隧道 IP 常量）
> **从**：WG 档 = 经出口豁免臂 ⇒ **出口回环**该端口（`intercept/mod.rs:1196-1206`）
> **→ 到**：QUIC 档 = **字面拨 `100.64.255.1:port`**（出口侧通常不可达 ⇒ `0x25`）；**不引入别名**
> **原因**：无兼容包袱常设口径 + 该常量是 WG 隧道地址（QUIC 档无此语义）；避免在出口腿里留旧承载隐含依赖
> **影响面**：spec 无该条（用户面不可达形态）；登记以便排障口径统一

> **行 7（行为差异；非判据行）**｜日期：**实施批当日**｜条目：出口**虚拟端口**（7802/7724/7803）作为
> portfwd 目标
> **从**：WG 档 = 豁免臂命中 `local_services` ⇒ **UDS 服务**（Q-F-B 实测可达）
> **→ 到**：QUIC 档 = `ECONNREFUSED` ⇒ `0x25`（服务已改为**流 tag**，不再是 TCP 端口）——实测 §12-C5
> **原因**：M3 的服务承载切换（A13 的「虚拟端口退役」）｜**影响面**：spec 无该条；排障口径

> **行 8（行为差异；非判据行）**｜日期：**实施批当日**｜条目：`targetPort == 0` 的旁路形态 wire 目标
> **从**：`SERVER_TUNNEL_IP:0` → 出口豁免臂 ⇒ 回环拨 0（失败于 connect 的 errno） | **→ 到**：
> `127.0.0.1:0` ⇒ **在拨号前被地址类判定拒**（§1.4 行 11）⇒ `0x25` + why「端口 0 不是可拨端口」
> **原因**：§1.3 的哨兵更换 + §1.4 行 11 的判定（r20 统一三处机制）｜**影响面**：
> `portfwd.rs::dial_target_semantics` 单测（:1173+）的解析断言

> **行 9（判据行触发集订正——C11）**｜日期：**实施批当日**｜条目：`C11（RECOVER 族）触发集`
> **从**：M3 登记「QUIC 档零 C11 入口（内部触发点）」+ **同日订正「NAPI 下推入口未分档」**（M3.md §5）
> **→ 到**：**NAPI 下推入口已分档**——**`l3_on_island()==true` 的世代**不产 C11 族行（改产行 4 ②的 addtive 行）；
> **`bearer=Quic` 但岛未就的回落世代仍走 WG 原路（C11 族行照旧）**；WG 档逐字不变
> **原因**：M4 §5 的最小分档｜**影响面**：`facade/{mod,tun_exec}.rs`；tier 文档 `connection-lifecycle.md`
> 的修订稿（M3 附录 A §11 第二条由「未分档」改为「已分档」，**tier 触点**）

> **行 10（计数输入集；行文不变）**｜日期：**实施批当日**｜条目：出口 `stats.streams_open/stream_refused/
> stream_bytes_*/streams_closed` 的 **tag=dial** 输入集
> **从**：无（dial 只走拒行计数 `stream_refused`）｜**→ 到**：`streams_open` = 拨号成功数（**时点 = 回执写成功**）；
> `stream_refused` = 四类拒；`stream_bytes_*` = 泵字节；`streams_closed` = 泵收
> **原因**：§3.2｜**影响面**：`serve status --json` 的 quic 段数值语义；出口排障

> **行 11（残余/口径变化；登记）**｜日期：**实施批当日**｜条目：**并发与内存口径**（Q-F-B 残余 9/10）
> **从**：「阀 256 = 唯一界；引擎每连接 2×1 MiB ⇒ 最坏 512 MiB」
> **→ 到**：QUIC 档 ①**实际生效并发上界 = 岛 bidi 额度 − 2（缺省 62）/连接**（阀 256 保留但其行文与计数
> 仍在 —— 只是通常不可达）；②dial 腿**不进** `intercept::MAX_CONNS` 账，fd 上界 = `conn_cap(64) × 62 ≈ 3968`
> （**强制态口径**；持续态 ≈ 每连接 62 × 在用设备数）；③接收面 = **每流窗 4 MiB + 连接级聚合闸 8 MiB**
> （`tuning.rs:37/45` 的 **S9 定值**；「64 × 256 KiB = 16 MiB」与「2 MiB/连接」两条旧账**均作废**）
> **原因**：承载换代（M3 §1.7 + S9 整改 + §7A）｜**影响面**：Q-F-B 残余 9/10 的注记；M5 的容量复核

> **行 12（可观测面差异；非判据行）**｜日期：**实施批当日**｜条目：**目标侧异常结束的行**
> **从**：WG 档在某些 `ConnErr` 变体下，客户端 pf 泵记 `port-forward[down] 读错误（累计 …）`
> **→ 到**：QUIC 档目标 RST 以 `finish()` 收口 ⇒ 客户端把它读成 **EOF**（白名单外复位码/FIN
> 都归 `Closed`，`streams.rs:550-556`）⇒ **该行不再出现**；**出口侧新增** `quic: 服务流目标侧中断
> （tag=dial，%s 方向：%e）`
> **原因**：M3 §1.3 的「正常收工不用 reset」沿用（r19 L5）｜**影响面**：客户端日志读法；
> `bridge_host::pump` 的 pf 无条件记行门槛（**行为不变**，只是触发面变小）

> **行 13（rc 可达集与归因面；非判据行但驱动 tier 行为面）**｜日期：**实施批当日**｜条目：
> `ClientCoreTunRecover` 的 **rc 可达集**
> **从**：QUIC 档可达 `0/-1/-2/-3/-4`（跑 WG 阶梯 ⇒ 与 QUIC 数据面无关的值）
> **→ 到**：**`l3_on_island()==true` 的世代**可达 **`0/-1/-2`**（岛快探 + 复探；`-3/-4` 在该世代**不可达**）；
> **`bearer=Quic` 但岛未就的回落世代仍走 WG 原路**（`-3/-4` 照旧可达）；`-2` 的构成
> = 「未 attach/陈旧」∪「岛不在」；tier 决策不变（只有 `0` 跳过整套重建），但**`-2` 的 tier 日志文案
> （「当前无 attached 隧道」）在该形态下不实**（tier 触点，M3 附录 A §11 的第二条同步改）
> **原因**：§5 的最小分档（**rc 值域不变、可达集与归因面变**；本文不再声称「零 rc 语义变更」）
> **影响面**：`facade/{mod,tun_exec}.rs`；tier `TierVpnExtensionAbility.ets:1228-1242` 的日志与
> `connection-lifecycle.md` 修订稿

### 8.3 零改动面（本设计核实）

`fixtures/`（零改动）、`tools/check-vocab.sh` 四处（声明集/缺席表/manifest/tier 码表；`portfwd/err` 值域不变）、
NAPI 符号面（零新增/改名）、`portForwards[]` 的 JSON 键与 `code` 值域、tier `port-forwarding` spec（零改动）、
`PfDialFn` 签名、`pf_target_text` 四形态。

---

## 9. 真机验证计划（设备 `FMR0224116011480` 已授权；手册 = `docs/DEVICE-TEST-OHOS.md`）

**形态**（照手册 §6 的最小闭环 + M4 追加项）：

1. 本机侧：`tools/local-rust-exit.sh start 1`（本地私有出口，**绝不碰现役出口**）+ 在出口侧准备
   ①一个回环服务（如 `python3 -m http.server 18080 --bind 127.0.0.1`）②一个出口 LAN 地址可达的服务
   ③一条**被占用**的监听端口（用于 R3 场景①）。
2. 出包：`HOMEWAY_RS=<本 worktree> tier/tools/tailcat/build-core.sh` → `assembleHsp`×2 + `assembleHap`
   （手册 §1 的已知拦点：tier `log-index.md` 陈旧 ⇒ 门在 `cp` 前，逃生口 = 手拷 `.so` 两处 + 记哈希）。
3. 装 + 免点屏注入 token（手册 §2/§3）⇒ `uitest` 点 VPN 开关（§4）。
4. **M4 专属用例**（每项给判据行）：
   - ①App 端口转发页加「监听 L → 目标（空）:T」，出口回环有服务 ⇒ 手机浏览器 `http://127.0.0.1:L` 加载成功
     （**R1-S①**；出口行 L1 `DIAL_OK` 类的可观测位 = `quic: 服务流已受理（tag=dial …）`）；
   - ②改「目标 = <出口 LAN IP>:T」⇒ 仍可访问（**R1-S②**）；
   - ③IP 分流开关两态都通（**R1-S③**）；
   - ④占用端口那条 ⇒ 页面「失败 · 端口被占用」（`bind_failed`，**R3-S①**）；未连接 ⇒ 「未连接」（**R3-S②**）；
   - ⑤改动 + 「立即重连」⇒ 新映射生效（**R2-S②**）；删主机 ⇒ 映射消失（**R2-S①**）；
   - ⑥负例：目标端口**无服务** ⇒ 浏览器立刻「连接被重置」（`0x25` 链）；出口日志出现
     `目标拨号失败（…：Connection refused…）`（**§4 归因面**）；
   - ⑦换网（WiFi↔蜂窝，**人工**，手册 §4 已记 Wi-Fi 主开关不吃模拟点击）：QUIC 档迁移后 pf 仍可访问；
     且**在 `l3_on_island()==true` 的世代**里**不再出现 `RECOVER R1/R2/R3` 族行**（若本世代是
     「岛未就的回落世代」（`bearer=Quic` 但岛起不来），则**允许**出现——那是 WG 原路）；
     并采 §5.3 的三个预登记指标（`-1` 误判率 ≤1/5、零 RECOVER 行、下推耗时 ≤8s）。
     若分档被裁定转交 M5，则本项改为「如实记录 RECOVER 行 + 标注承载」。
5. 日志面：核日志 `tailcat-tun.log`（`grep -a`）+ App 日志 + 出口 `stdout.log`（手册 §5 的速查串增补
   `tag=dial` 与本设计的新行）。
6. 收工：关 VPN 开关、停本地出口、`git -C tier status --porcelain` 对照（零新增脏文件）。

**真机前的本机门**（必须全绿才出包）：`cargo test --workspace` + `clippy -D warnings` +
`tools/check-quic-isolation.sh`（含新条款）+ `tools/check-vocab.sh` + OHOS 交叉 `check` +
`tools/quic-pf-e2e.sh`（新）。

---

## 10. 风险与未决

| # | 风险 | 影响 | 处置 |
|---|---|---|---|
| **W1** | **pf 并发上界被承载收紧**：阀 256 vs 岛 bidi 额度 62 | 多标签/多应用的 pf 并发在 62 处开始 `Busy`（`fails+1` + RST，浏览器表现 = 连接被重置） | **本期只登记**（§8 行 11）。备选（**r19 M1 订正后的正确账**）：①提高 `max_concurrent_bidi_streams`（64→256）：**接收面不会爆**（连接级聚合闸 8 MiB 与流数无关，`tuning.rs:47`）——真正变差的是 **fd/任务数（×4）**、**每流 4 MiB 窗的分摊**与出口侧并发拨号数；②给 dial 保留额度（新机制，本期不做）；③不改（接受 62）。**真机打点**：R1 用例并发压 60+ 观察是否触发；**M5 容量复核**时一并定 |
| **W2** | dial 腿无出口侧独立全局阀 | 强制态上界 ≈ 3968 条 leg（`conn_cap 64 × 62`），每条约 <64 KiB 缓冲 | 登记（残余 9，两口径写明）；对照 intercept 的已登记 1GB 最坏 ⇒ 低一个量级；不加新阀 |
| **W3** | 客户端预算先到期的在途 dial | 出口侧拨号任务最多再存活到自身 10s 期限（实测 §12-C2：客户端 600ms 放弃、出口 10.0017s 才收） | 登记；缺省预算 15s > 10s ⇒ 正常形态不触发；`dialMs` 脏配置才可达 |
| **W4** | 1B 回执与「首块 > 1B」 | 余量丢失 = 应用层帧错位（难定位） | S2 完成判据含专门用例（构造 `0x01+payload` 同块） |
| **W5** | 本机 fake-IP 代理劫持非直连目标（§0.4-2） | 本地 e2e 可能测到代理而非我方腿 | 测试只用回环 + 直连网段；e2e 断言「出口行出现 `DIAL_OK` 类受理行 + 泵字节」而非只断言「通了」 |
| **W6** | 出口侧「不可达」归因的 kind 面变化（`Other` ⇒ `ConnectionRefused`） | 桥宿主的 `is_refused_like` 类判定在 pf 面更常命中（pf 不用该谓词；只影响日志读法） | 登记（§8 行 5） |
| **W7** | NAPI 分档的快探误判 | 若真机「网络尚未就绪 ⇒ 快探+复探双失 ⇒ `-1` ⇒ 整套重建」偏多 | **已按 r19 M5 取「快探 + 一次复探」**（与岛内阶梯同粒度）+ 预登记可 falsify 指标（§5.3）；超阈值再升级 |
| **W8** | pf 泵仍是「阻塞线程 + 无限期读」形态（`QuicReadHalf` 读挂起） | 每连接 3 线程不变；收工依赖 `FlowGuard` 的 Arc 生命周期 | 与 Q-F-B 同形（不新增风险）；登记 |
| **W9** | 拨号失败到本机 RST 的**时点后移**（多一个 RTT：出口 reset 到达才 RST） | 浏览器表现 = 「被拒」的时序差异（同为 RST） | 登记；本地量级 = RTT（实测 200µs–1ms） |
| **W10** | WG 腿的归一函数（`wg_dial_addr`）是**双栈期临时物** | M5 删 `session_connect_target` 时需一并删 | 设计标注（§11-S6 登记指针给 M5） |
| **W11** | **岛内流槽泄漏**（r19 H1 的高危原形） | 若失败路径不关流 ⇒ 常态失败累积到 62 后**同连接全部服务流 `Busy`**（且连接长活） | **已闭合于设计**（§2.2 的 RAII 守卫 + S2 判据）；风险表保留条目以便实现期对照 |
| **W12** | 内存/并发**数字口径**曾用 S9 前的旧值（256 KiB） | 会给出错误的取舍结论（W1 备选①） | **已按 S9 定值订正**（§7A/§8 行 11/残余 4·10）；实现期以 `tuning.rs` 现值为唯一真源 |

**未决（需主会话/用户裁决）**：①§5 的 NAPI 分档**本期修 vs 转交 M5**（本设计**建议本期修**，见 §5.3）；
②W1 是否需要调整流额度（本期建议只登记）；③§1.3 的「不引入 `SERVER_TUNNEL_IP` 别名」是否接受（本设计
**建议不引入**）。

---

## 11. 实施清单（S1–S6 + 依赖顺序 + 每项完成判据；供主会话切棒）

> 顺序：`S1 → S2 → S3 → {S4, S5} → S6`（S2 依赖 S1 的出口面协议；S3 可与 S4/S5 并行；S6 收口）。
> 每项 = 「落点 + 测试 + 判据行 + 登记」四件套（M2/M3 体例）。
> **硬约定（照 QFB 设计门先例，r19 L9 采纳）**：①**不带旧写法进代码**（不留 `SERVER_TUNNEL_IP` 哨兵/旧 why 串
> 的兼容分支）；②**判据变更同批 commit**；③**实现期发现设计与代码矛盾（或设计不可实现）不得静默降级**——
> 必须在本文件追加 §15 起「实施期订正」并上报主会话。

- **S1 出口 dial 腿 + 协议常量**：`stream.rs` 加 `DIAL_OK`（+ 两侧同源断言）；`exit/serve.rs` 的
  `StreamTag::Dial` 臂改调 `exit/dial.rs::dial_serve`；新文件 `exit/dial.rs`（**地址类 typed enum + `text()`** +
  拨号 + 回执 + 复用泛型化泵）；`exit/pump.rs` 泛型化；`tools/check-quic-isolation.sh` 的 `ASYNC_FILES`
  **同批**加 `exit/dial.rs`（该门双向 fail-closed）。
  **完成判据**：单测（**七类地址的判定表逐条**——含 `port==0`；`0x25/0x26` 双码；回执先于字节；
  半关双向传播；目标侧异常有一行；**`stream_bytes_*` 不含回执字节**）**+ 出口侧真 socket e2e**（照
  `exit/tests.rs:2920` 的 `exit_with_intakes` 形态自建 loopback echo）**+ 既有两处必改测试**
  （`exit/tests.rs:3055` 的 `service_stream_dial_reads_target_then_refuses_0x22` 改为「真拨成功/失败两态」；
  `serve.rs:253-285` 的 `serve_only_emits_the_designed_codes` 自检按新码集改，`:278-284` 的注记删除）
  **+ 拒行/受理行逐字断言**（E-q5 扩展取值）**+ 泵半关单测（`pump.rs` 既有）全绿 + 隔离门绿**。
- **S2 客户端 seam + 拨号缝分派**：`quic_stream.rs::dial_target`（开流/**RAII 守卫**/写帧/等回执/余量预置）；
  `portfwd.rs::PfDialTarget::resolve()` ⇒ `127.0.0.1`（+ doc 注释 + 单测改写）；`tun_exec.rs`：
  `pf_dial_via_run` 分派 + `wg_dial_addr` 归一 + 单测。
  **完成判据**：单测（`dial_target` 的**单次尝试**语义；**回执值空间穷举**——`[0x01,rest]`/空块续读/
  `ب≠0x01`/`Busy`/`Refused`/`Timeout`/`Closed`/`ConnectionLost`；**首块 >1B 的余量保真**；`resolve()` 三形态；
  `wg_dial_addr` 的 127/8 ⇒ tunnel IP）**+ 泄漏判据**（连续 N>62 次失败拨号后在册数回基线，或第 63 次仍
  返回 `Refused` 而非 `Busy`）**+ Q-F-B 的 22 条真 socket 用例零改动静绿**（唯一允许改 =
  `dial_target_semantics` 的 `resolve()` 断言 + doc 注释）。
- **S3 隔离门扩展 + 负例自检**：`check-quic-isolation.sh` 加「pf 拨号缝块」（§2.5）。
  **完成判据**：真文件注入 `stackb::` ⇒ 确定红；块行数下界与自校准（`quic_stream::dial_target` ≥1 且
  `session_connect_target` ≥1）双断言在场；正常树绿。
- **S4 NAPI 分档（若裁决「本期修」）**：`tun_exec.rs:903` 的 `recover` 分档（**判据 = `l3_on_island()`**）
  + 快探 + 一次复探 + additive 行 + 用例。
  **完成判据**：①WG 档 `recover` 行为逐字不变（对照用例）②QUIC 档三态（岛在世探通 ⇒ `0`；两次探失败 ⇒ `-1`；
  未 attach/stale ⇒ `-2`）＋**`l3_on_island()==false` 的 QUIC 档走 WG 原路**（回落形态用例）③QUIC 档
  **零 `RECOVER` 族行**（日志断言，**限定于 `l3_on_island()==true` 的世代**）④行文逐字 + 耗时上界断言
  （≤7.1s 写进注释与用例上界）⑤§8 行 13 登记同批。
- **S5 e2e + 真机 + spec 全条核验**：`tools/quic-pf-e2e.sh` + `crates/homeway-core/tests/quic_pf_e2e.rs`
  （真世代 + 真出口；照 `quic_island_e2e.rs:606-608/673-713`）；§6 的 5 条 requirement（**含正文 SHALL 条款**：
  R1 的三条无 scenario 的 SHALL 用「配置面不存在 + 代码路径无消费点」的 grep 断言钉住）本机面逐条断言；
  §9 的真机四场景。
  **完成判据**：e2e 全绿（含**四形态 × 三失败**的归因断言、阀/计数/热替换对照、spec R1–R5 本机面）+
  R1①「仅回环」/R1③「绕过名单」的结构断言（grep 零消费点）+ 真机记录（含换网/无 RECOVER 行/下推耗时）+
  读数落 `docs/reviews/M4.md`。
- **S6 登记 + 代码门 + 收口**：`INTEROP-CRITERIA.md` 的 §8.2 十三条**同批 commit**；`docs/reviews/M4.md`
  （代码门 dsh + 逐条处置 + 证据）；Q-F-B 残余相关 4 条的收口指针；M5 输入清单（W10/§1.3 的临时物）。
  **完成判据**：登记表条目齐全（五字段）+ 词表门 PASS + 代码门无高危（或显式豁免登记）+ `M4.md` 入库。

---

## 12. 实测锚点（`/tmp/m4lab` 验证台；**本棒不写产品代码**，只做 `/tmp` 实验）

**台架**：`/tmp/m4lab/`（`Cargo.toml` + `src/main.rs` + `cert.pem/key.pem`，由 `/tmp/m3lab` 克隆；
TransportConfig 逐值照抄 `crates/homeway-quic/src/exit/transport.rs`，并加 `max_concurrent_bidi_streams=64` /
`stream_receive_window=**4 MiB**` / `receive_window=8 MiB` / `send_window=2 MiB`——**逐值照
`crates/homeway-quic/src/tuning.rs` 的 S9 现值**，与产品面同值）。子命令：`echo`（真 TCP 回显目标）、`dialserve`
（出口 dial 腿：tag=4 + 6B → 真拨 → 1B 回执 → 双向泵）、`dialcli`（客户端：开流 + 6B + 等回执 + 收发）、
`addrprobe`/`asyncdialprobe`（地址类拨号行为）。
**原始产物**：`/tmp/m4lab/raw/{p1-addrprobe-std,c1-client-cases,c2-client-timeout,c3-client-exit-timeout,
c4-exit-ip,c5-virtual-ports,echo,echo-any,echo-any2,serve1}.txt`（`echo-any.txt` = 首个 0.0.0.0 回显台；
`echo-any2.txt` = §12-C4 用的 python 台）。

### 12.1 P1——地址类拨号行为（**A8 判定表的证据面**）

```
ADDRPROBE 回环-开   dst=127.0.0.1:54291  => OK（本机可达！）elapsed_us=139
ADDRPROBE 回环-关   dst=127.0.0.1:1      => kind=ConnectionRefused raw=Some(61) elapsed_us=64
ADDRPROBE 未指定    dst=0.0.0.0:54291    => OK（本机可达！）elapsed_us=196      ← ★ 连到本机（平台陷阱）
ADDRPROBE 本网络    dst=0.1.2.3:54291    => kind=HostUnreachable raw=Some(65) elapsed_us=104
ADDRPROBE 受限广播  dst=255.255.255.255  => kind=Uncategorized raw=Some(47) elapsed_us=23
ADDRPROBE 组播      dst=224.0.0.1 / 239.1.1.1 => raw=Some(47) elapsed_us=17/15
ADDRPROBE 链路本地  dst=169.254.169.254:80 => kind=TimedOut elapsed_us=1502320   （本机可当黑洞用）
ADDRPROBE CGNAT     dst=100.64.255.1:1   => OK elapsed_us=1538   ※本机 utun4 fake-IP 代理劫持（§0.4-2）
ADDRPROBE 私网      dst=10.255.255.1:9   => OK elapsed_us=3090   ※同上，勿作网络行为证据
```
**结论**：①`0.0.0.0` 必须显式拒（实测 = 连本机，静默错位）；②`0/8`、受限广播、组播各自有平台 errno
（归因不可读）⇒ 直拒更诚实；③回环拒绝码 61 = 与 WG 档同族归因；④`169.254.169.254` 在本机是**黑洞**
（1.5s 超时）⇒ 可作超期用例的目标。

### 12.2 C1——端到端 dial（出口预算 10s；原始 `raw/c1-client-cases.txt` + `raw/serve1.txt`）

```
[A] 127.0.0.1:54291（echo）  CLI_ACK ok=0x01 us=351 → CLI_ECHO n=14 got="ping-forward-1" us=598
     出口：DIAL_OK elapsed_us=156 / DIAL_PUMP_UP_EOF n=14 / DIAL_PUMP_DOWN_EOF n=14 / DIAL_STREAM_DONE total_us=425
     echo 侧：ECHO_ACCEPT → ECHO_RX n=14 → ECHO_EOF（客户端 FIN ⇒ 目标见 EOF，半关传播成立）
[B] 127.0.0.1:1（未监听）     CLI_ACK_ERR us=558 err=ReadError(Reset(37))     ← 0x25 ⇒ StreamErr::Refused
     出口：DIAL_REFUSED kind=ConnectionRefused raw=61 elapsed_us=201
[C] 0.0.0.0:54291（未指定）   CLI_ACK_ERR us=306 err=ReadError(Reset(37))；出口 DIAL_CLASS_REJECT why=未指定 0.0.0.0
[D] 224.0.0.1:80（组播）      Reset(37)；出口 DIAL_CLASS_REJECT why=组播 224/4
[E] 255.255.255.255:80（广播）Reset(37)；出口 DIAL_CLASS_REJECT why=受限广播 255.255.255.255
```
**结论**：①1B 回执机制成立且廉价（回执 +351µs、往返 598µs）；②`0x25` 在客户端成为**类型化**
`ReadError(Reset(37))`（⇒ `StreamErr::Refused` ⇒ `ConnectionRefused`）；③拒码即刻可见（160–558µs）。

### 12.3 C2/C3——超期归因的两条路（原始 `raw/c2-*.txt`/`raw/c3-*.txt`）

```
[F] 客户端预算 600ms ⊕ 出口预算 10s（dst=169.254.169.254:80，黑洞）
     客户端：CLI_ACK_TIMEOUT budget_ms=600（不产 typed 码 ⇒ Err(TimedOut)）
     出口：  DIAL_TIMEOUT budget_ms=10000 elapsed_us=10001700   ← ★在途窗口实测（§10-W3）
[G] 客户端预算 12s（同目标）
     客户端：CLI_ACK_ERR us=10002239 err=ReadError(Reset(38))   ← 0x26 ⇒ StreamErr::Timeout ⇒ TimedOut
```
**结论**：缺省（15s > 10s）下**出口先超期** ⇒ `0x26` 精确归因（10.002s 到达客户端）；
客户端预算更短时先收线，出口的在途 dial 至多再活到自身期限（**登记**）。

### 12.4 C4——「出口 IP」语义（原始 `raw/c4-exit-ip.txt`）

```
目标 = 192.168.3.12:54396（本机 LAN 地址；目标侧 = python 0.0.0.0 echo）
客户端：CLI_ACK ok=0x01 us=581 ；出口：DIAL_OK elapsed_us=376 + DIAL_PUMP_UP_EOF n=13
目标侧：ECHO_ANY_ACCEPT ('192.168.3.12', 54485) / ECHO_ANY_RX b'exit-ip-probe' / ECHO_ANY_EOF
```
**结论**：出口**自己的其它地址**作为目标是普通拨号并真通（「出口 IP 语义保留」的证据面）；
**下行 EOF 未在 3s 内到达 = 台架瑕疵**（python 的 accept 循环帧仍持有 socket ⇒ 不 FIN；上行半关已证）。

### 12.5 C5——虚拟端口作为目标（原始 `raw/c5-virtual-ports.txt`）

```
dst=127.0.0.1:7802 / :7724 / :7803  ⇒ 一律 ReadError(Reset(37))（0x25）
出口：DIAL_REFUSED kind=ConnectionRefused raw=61 elapsed_us=112/173
```
**结论**：服务已改为流 tag ⇒ 这些**端口不再是 TCP 目标**（§8 行 7 的证据）。

### 12.6 复现

```bash
cd /tmp/m4lab && cargo build --release
./target/release/m4lab echo 0 &                  # 记下端口
./target/release/m4lab dialserve 0 10000 &       # 记下端口
./target/release/m4lab dialcli <sp> 127.0.0.1:<echo> 3000 ping-forward-1
./target/release/m4lab dialcli <sp> 127.0.0.1:1 3000 -
M4LAB_OPEN_PORT=<echo> M4LAB_CLOSED_PORT=1 ./target/release/m4lab addrprobe
```

---

## 13. 设计门记录（dsh）

### 13.0 轮次事实

| 项 | 值 |
|---|---|
| 目录 | **`/tmp/dsh-review/r19.ZNsUWS`**（`prompt.txt` / `output.md` 145 行 / `stderr.log` 1008 行 = 推理流） |
| 命令 | `dsh --profile headless "$(cat prompt.txt)" > output.md 2> stderr.log; echo "exit=$?"` |
| **exit code** | **`0`**（前台捕获；**成败只认 exit code**） |
| 意见条数 | **13 条**：**高 1**（H1）· **中 10**（H2 / M1 / M2 / M-[中] / M3 / M4 / M5 / M6 / M7 + H2 并列记中高）· **低 11**（L1–L11，其中 L2/L3 为一条多要素） |
| 门结论（原文摘要） | 「**有条件通过。** 骨架成立（`PfDialFn` 不变换轨 / 1B 回执 / SSRF 面论证 / 出口直拨 / 登记体例），多数论证经回源核对为真；以下 **9 项必须在实现棒开工前闭合**……不阻塞项 L1–L11 建议同批吸收」 |
| 评审独立做的事 | `git status --short` 复核（评审**未改任何跟踪文件**）；回源码抽查 20 处行号；全文读设计主件与 6 份真源；逐值对照 `/tmp/m4lab/raw/` 的 11 个原始文件；读 tier `TierVpnExtensionAbility.ets:1080-1410` / `HealPolicy.ets:150-240`；**独立复核了 SSRF 论证**（查 `exit/conn.rs:744-746` 的源校验接受集、`intercept/mod.rs:1186-1213` 的目的地路由、设备表无 per-device 能力位） |
| 仓内副作用 | `git status --short` = 仅 `?? docs/reviews/M4-design.md`（本棒产物）；评审的分析产物落 `/tmp/m4-review/review.md`（**仓外**） |

> **注**：r19 结论段自述「9 项」实为其正文的 10 条中危中的 9 条（未含 M-[中]）；r20 已订正该计数——
> 本表按 **13 条**（1 高 + 10 中 + 11 低）登记全部意见。

### 13.1 逐条处置表（认同改 / 不认同给证据）

| 编号 | 严重度 | 意见（摘要） | 处置 | 落到 |
|---|---|---|---|---|
| **H1** | **高** | 客户端 seam 的失败路径（写帧失败 / 回执 0x25·0x26）**不关流** ⇒ 岛内槽只由 `StreamClose`/连接死摘除（对端 reset 不清表）⇒ 常态失败累积 62 条后**同连接全部服务流 `Busy`**（连接长活） | **认同（高危必改）**：回源复核成立（`driver.rs:1218-1222`、`streams.rs:360-374/381-400/502-557`、`has_capacity` `:225-228`）⇒ §2.2 写死 **RAII 守卫**（②之后即建 `Arc<StreamShared>`，失败靠 drop）+ S2 加泄漏判据 + §10-W11 | §2.2 / §11-S2 / §10-W11 |
| **H2** | 中高 | 回执读的边角：**空块踩 `[0]`**（quinn 存在 0 长块，`pump.rs:136-137` 有先例）；`Closed/ConnectionLost` 必须归「拨号失败」（不得被读成 EOF/成功） | **认同**：§2.2 加「回执值空间穷举」表（空块 ⇒ 预算内续读；`Closed` ⇒ 显式失败）+ §4 新增「回执面异常」行 + §8 行 5 补输入集 | §2.2 / §4 / §8 行 5 |
| **M1** | 中高 | **内存口径用了 S9 前旧值**：现为每流窗 **4 MiB** + 连接级 **8 MiB** 聚合闸（`tuning.rs:37/45`）⇒ W1 备选①的「16→64 MiB」论证不成立；且**缺体积/内存预算行** | **认同**（回源复核：`tuning.rs:21-58` + `M3.md:722/830-871`）⇒ §7 残余 4/10 重写、§8 行 11 改数、§10-W1 改论证、**新增 §7A 预算表**；§12 台架参数改「逐值照 S9 现值」 | §7 / §7A / §8 行 11 / §10-W1 / §12 |
| **M2** | 中 | 资源上界与代码不符：出口强制连接上限 `conn_cap = 2 × max_devices`（缺省 **64**）⇒ 强制上界 ≈ **3968/4096**（非 2048）；且未引 intercept 的 1GB 对照 | **认同**（`exit/mod.rs:187-189` + `intercept/mod.rs:397-400` 复核）⇒ 残余 9/§8 行 11/§10-W2 改「持续态 ≤32 连接 / 强制态 ≤64 连接」两口径 + 补 1GB 对照 | §7 残余 9 / §8 行 11 / §10-W2 |
| **M-[中]** | 中 | dial 腿与 transit 腿是**两个策略执行点**：M5 若给 transit 加目的地策略，dial 会静默成为旁路 | **认同**：§3.1 加**同源不变量**（将来加策略必须同批同步/单源） | §3.1 |
| **M3** | 中 | 分档判据必须用 **`l3_on_island()`**，不能用 `run.bearer`（`bearer=Quic` 但岛未就 ⇒ 本世代按 WG 跑，`:1425-1445`） | **认同**（回源复核同结论）⇒ §5.3 表改判据并写明两种误判形态 | §5.3 |
| **M4** | 中 | 「零 rc 语义变更」不成立：QUIC 档 `-3/-4` 变**不可达**、`-2` 语义扩张（tier `:1228-1242` 只对 `0` 跳过重建；`attribute()` 用 `-3/-4` 归因「本机网络栈」） | **认同**：删「零 rc 语义变更」措辞；**单列登记行 13**（值域不变 / 可达集与归因面变；tier 决策不变、`-2` 文案不实） | §5.3 / §8 行 13 |
| **M5** | 中 | `-1` 判负粒度比岛内阶梯粗（岛 = 首探 + 复探 1.4s）；缺可 falsify 的真机指标 | **认同**：§5.3 改「**快探 + 一次复探**」（`REPROBE_FACTOR=2`）+ **预登记三个真机指标**（误判率 ≤1/5、零 RECOVER 行、耗时 ≤8s） | §5.3 / §10-W7 |
| **M6** | 中 | §6 只覆盖 scenario，未覆盖 requirement 正文的 SHALL 从句；R5 应写明 **vacuous**（代理面不存在） | **认同**：§6 改为「逐 requirement × 三类证据」（正文 SHALL / scenario / 结构性零触碰）；R5 标 vacuous + 证据 = 全仓 `forward-via-proxy` 零命中（**本棒实测已核**）+ spec 实现现状段；R1 的三条无 scenario SHALL 请 S5 用 grep 断言钉 | §6 / §11-S5 |
| **M7** | 中 | 泵会成**第四份**拷贝（`exit/pump.rs` 同形）；`stream_bytes_*` 必须**排除回执字节** | **认同**：§2.4 白名单**放行 `exit/pump.rs` 泛型化**（≈4 行 + 调用侧）；§3.3 复用同一份 + 计数口径写死 | §2.4 / §3.3 / §11-S1 |
| **L1** | 低 | `open_stream` 的等回执 = `budget + OPEN_BUDGET` ⇒「总时长 ≤ dialMs」不成立 | **认同**：§2.2 改「seam 自带有界等待（`island_stream_cmd(.., Some(remain()))`），不复用 `open_stream`」 | §2.2 |
| **L2** | 低 | 接受集缺 `port==0`、`240/4`、`198.18/15`；判定宜落 typed enum + `text()` | **认同**：§1.4 加三行（11/12/13）+ 实现形态段（typed enum + `text()` 单源） | §1.4 |
| **L3** | 低 | `target_port==0` 的「仅手改/损坏配置可达」表述偏强，应点名向量 | **认同**：§1.1 精确到向量（NAPI 恒过 `validate_table` ⇒ 不可达；可达 = `tunConfig.portForwards` 的 serde 直读） | §1.1 |
| **L4** | 低 | §2.4「连注释都不动」与 `resolve()`/`dial_target()` 的 doc 必改冲突 | **认同**：白名单显式放行这两处 doc 注释 | §2.4 |
| **L5** | 低 | 目标 RST 取 FIN ⇒ 客户端 pf「读错误」行消失（可观测面变化，登记缺该条） | **认同**：§3.3 补**出口侧一行**（`服务流目标侧中断`）+ §8 **新增行 12** 登记客户端面的行消失 | §3.3 / §8 行 12 |
| **L6** | 低 | 6B 帧的部分接纳在开流瞬间结构性不可达，应写明前提 | **认同**：§2.2 写死 `n == 6` 的结构性事实（否则 `WriteZero`） | §2.2 |
| **L7** | 低 | 「一次 700ms 快探」真实上界 = `FAST_BUDGET + QUIC_RPC_BUDGET` = 5.7s | **认同**：§5.3 写真实上界（复探后 ≈2.1s + 5s = **7.1s**） | §5.3 |
| **L8** | 低 | §5 应补「现状 rc 与 QUIC 数据面无关 = 错误证据源」；`-2` 的 tier 日志不实需登记 | **认同**：§5.3 首段加「最强理由」；§8 行 13 含 `-2` 文案不实项 | §5.3 / §8 行 13 |
| **L9** | 低 | 缺 QBF 设计门那条硬约定（矛盾不得静默降级） | **认同**：§11 头部照抄三条硬约定 | §11 |
| **L10** | 低 | 应点名 `exit/tests.rs` 的 `..._refuses_0x22` 与 `serve.rs` 自检必改 | **认同**：§0.2 行 + §11-S1 判据点名（实测行号 = `exit/tests.rs:3055`、`serve.rs:253-285/278-284`） | §0.2 / §11-S1 |
| **L11** | 低 | 行号小漂（`l3_probe` 2297；`dial_target_semantics` 断言位置） | **认同**：§0.2 订正 `l3_probe` = **:2297**；`resolve()` 断言**先采 :1209-1212，再经 r20 复核订正为 `:1208-1211`**（r19 的 1205-1209 亦差一行；见 §13.2） | §0.2 |

### 13.2 不认同项（**附证据**；共 1 条，**已被 r20 订正为「认同」**）

1. **L11 的一半（`dial_target_semantics` 的断言行号）——本条本棒原判「部分不认同」，r20 复核后改判「认同」**：
   r19 写「resolve 断言在 `portfwd.rs:1205-1209`」；本棒当时的实测转录为「`:1209` 是 `assert_eq!`、`:1210-1211` 是实参」⇒ 采 `1209-1212`。**r20 逐行复核的正确值是：注释 `:1207`、`assert_eq!` `:1208`、
   `resolve()` `:1209`、期望值 `:1210` ⇒ 断言块 = `:1208-1211`**；r19 的 `1205-1209` 与本节原写的
   `1209-1212` **都差一行**（r19 说的「另一断言在 :1205-1207」实为 `:1203-1206` 的 `example.com` 断言）。
   ⇒ **以源码为准改 :1208-1211**（已在 §0.2/§2.4 同步）；留本条记录以证「行号引用须每次回源复核」。

### 13.3 复审轮（r20）

| 项 | 值 |
|---|---|
| 目录 | **`/tmp/dsh-review/r20.Qh4Tvq`**（`output.md` 39 行） |
| **exit code** | **`0`** |
| 结论 | 「**有条件通过**」；**上一轮 13 条**里 **12 条已闭合**（H1/H2/M1/M2/M-[中]/M3/M5/M6/M7 + L1–L11 的实质面），**1 条部分闭合**（M4）+ **新 2 条中危** + 行号类低危若干 |
| 评审独立做的事 | 读 r19 原文 → 回源码逐条复核（不采信 §13.1 自述）；实测 `tuning.rs:37/45`、`exit/mod.rs:187-189`+`table.rs:27`+`exit/tests.rs:490`、`l3_probe:2297`/`:2303`、`FAST_BUDGET:217`/`REPROBE_FACTOR:222`、`pump.rs:131-159/166-189`；独立重读 tier spec（5 req / 10 scenario = 3+2+3+1+1）；`git status` 复核零跟踪文件改动 |

**r20 剩余项处置（3 条中危，全部已改）**：

| # | 意见（摘要） | 处置 |
|---|---|---|
| **R2-a** | **M4 部分闭合**：§5.3 表第 2 行自写「岛未就 ⇒ WG 原路（含 `-3/-4`）」，与末段/§8 行 13 的「只产 `0/-1/-2`」相抵；§8 行 9 / §11-S4③ / §9⑦ 同病 | **认同并改**：全文限定为「**`l3_on_island()==true` 的世代**」+ 补「`bearer=Quic` 但岛未就（`:1444` 回落）的世代 = WG 原路」——改 §5.3 / §8 行 9 / 行 13 / §11-S4③ / §9⑦ |
| **R2-b** | **【新】§8 行 5 的 `0x23` 归因错**：额度耗尽是**本地** `StreamErr::Busy`（`driver.rs:1167-1175`，**无复位码**）；`0x23=INTAKE_FULL` 只在 intake 路径写出，dial 腿**无 intake** ⇒ 不可达；与 §4 同行自相矛盾 | **认同并改**（回源复核成立）：§8 行 5 改为「本地 `StreamErr::Busy`（**无复位码**）；`0x23` 对 dial 不可达」+ §4「额度耗尽」行同步 |
| **R2-c** | **【新】端口 0 三处机制互相矛盾**：§1.4 行 11（拨号前拒）vs §4 末行/§8 行 8（`EINVAL/EADDRNOTAVAIL` errno） | **认同并改**：**以 §1.4 行 11 为准**（编入地址类判定 ⇒ `0x25` + why「端口 0 不是可拨端口」）；§4 末行 + §8 行 8 + §8 行 1 的 why 清单同步 |
| **R2-d** | 低危若干：`resolve()` 断言实为 `assert_eq!` :1208 / `resolve()` :1209；§5.1「四处都写否定式」不实（仅 `:2370` 是否定式）；待发包下推守卫在 `:2738`；`tuning.rs:38/47`→`:37/:45`；「22 条」判据不可操作；raw 清单漏 `echo-any.txt`；§2.2③ 复用 `QuicWriteHalf` 的固定 10s 环与「≤dialMs」措辞；H1 的残余窄窗（开流回执丢失 ⇒ 无 id 可关） | **全部认同并已吸收**：§0.2/§2.4 行号改 `:1208-1211`；§5.1 改为「一处否定式 + 三处正守卫（QUIC 档只留痕）」；§0.2 行号 `:2738`；`tuning.rs` 行号全量订正；§2.3 判据改「`mod tests` 只允许一处改动（`resolve()` 断言）」；§12 补 `echo-any.txt`；§2.2 补「严格上界 = `remain()` + 病态 10s」措辞；**新增岛侧防御项**（`StreamOpen` 回执投不出 ⇒ 就地关流）+ §2.4 白名单放行 `driver.rs` 该行 |

**r20 的行号复核（以源码为准，本文据此定稿）**：`l3_probe` = `tun_exec.rs:2297` ✓；dial 拒入测试 =
`exit/tests.rs:3055` ✓；`serve.rs` 自检 mod `:253` / fn `:259-285` / 两码注记 `:278-284` ✓；
`resolve()` 断言 = `portfwd.rs:1208-1211`（**r19 给的 1205-1209 与 §13.2 原写的 1209-1212 均差 1，已按 r20 订正**）。

**门结论（两轮合计）**：r19「有条件通过（9 项 must-close）」→ 全部闭合；r20「有条件通过（M4 部分闭合 + 新增 2 条）」
→ **本轮已全部闭合**。⇒ **设计门通过**（无阻塞项、无未闭合必改项）；残余全部登记（§8 十三条 + §10-W1…W12）。

### 13.4 门后残余（防静默漏做）

- **实现棒开工前必须闭合的项（r19 的 13 条 + r20 的 3 条中危 + 低危）已在本文正文逐条落**——收口时请对照：
  §2.2（H1/H2/L1/L6 + r20 的 10s 措辞与岛侧防御）、§1.1+§1.4（L2/L3 + r20-c 端口 0）、
  §2.4+§3.3（M7/L4/L5）、§3.1（M-[中]）、§5.3（M3/M4/M5/L7/L8 + r20-a 世代限定）、
  §6（M6）、§7+§7A（M1/M2）、§8 行 5/11/12/13（含 r20-b 的 0x23 订正）、
  §10-W1/W2/W7/W11/W12、§11 硬约定与 S1/S2/S4/S5 判据。
- **未闭合即进实现 = 违反 §11 的硬约定③（不得静默降级）**，须先在本文追加「实施期订正」并上报。
- 评审**未能核实**的项（本文如实登）：真机面（§9）全部未验；`/tmp/m4lab` 的 TLS 用「不钉定身份」的
  测试证书（产品面是 RPK 钉定）——评审已核该差异与结论无关（结论只依赖流/复位语义）。

---

## 14. 待主会话裁决

1. **NAPI 分档本期修 vs 转交 M5**（本设计**建议本期修**，方案与理由见 §5.3；转交路径亦已写明）。
2. **`SERVER_TUNNEL_IP` 是否作为「出口本机」别名保留**（本设计**建议不保留**，§1.3 裁决 D-1；理由是
   无兼容包袱 + 避免旧承载隐含依赖）。
3. **W1（pf 并发上界被承载收紧到 62）** 是否需要一个「额度调整/保留份额」的后续动作
   （本期建议只登记 + 真机打点；**备选① 的内存论证已按 S9 定值订正**，见 §10-W1）。
4. **§8 的 13 条登记草案**是否按本文原样落（尤其行 3 的「1B 回执」= M3 §1.2 描述的**订正**、
   行 13 的 rc 可达集与归因面）。
5. **`exit/pump.rs` 泛型化**（r19 M7）是否放行（本设计已放行；若主会话判定「不碰既有泵」，则须改为
   显式论证重复实现并挂残余 3 的收口指针）。

---

## 15. 设计门后记（本棒自检）

- 门后本文的**改动面**（供主会话核对）：§0.2 行号/新增两行、§1.1（向量精确化）、§1.4（+3 行 + typed enum 段）、
  §2.2（RAII + 回执值空间 + 6B 前提 + 自有有界等待）、§2.4（白名单 + doc 注释 + pump 泛型化）、§3.1（同源不变量）、
  §3.3（复用泵 + 计数口径 + 目标侧中断行）、§4（+回执行）、§5.3（判据改 `l3_on_island` + 复探 + rc 可达集 +
  真机指标）、§6（逐 requirement × 三类证据 + R5 vacuous）、§7（残余 4/9/10 改数）、**§7A 新增**、
  §8（行 5 补输入 + 行 11 改数 + 行 12/13 新增）、§10（W1 改论证 + W11/W12 新增）、§11（硬约定 + S1/S2/S4/S5 判据）、
  §12（台架参数订正）、§13（本轮记录）、§14（+第 5 条）。
- **未改**：§12 的原始读数（实测锚点不可改写）；§1.1 的帧格式结论；§2.1/§2.3 的结构论证（评审已核为真）；
  §6 的「必须真机」判定（评审认可）。

---

## 15. 实施期订正（一）（主会话裁定，2026-10-09；**后到的切片以本节为准**）

> 依据：`docs/QUIC-ROADMAP.md`「每期执行协议」第 6 条 + 「口径重申（2026-10-09）：**无兼容包袱常设有效**」。

1. **NAPI `ClientCoreTunRecover` 分档 = 本期修**（采纳设计建议，M4 S4 切片）：判据改 `l3_on_island()`（非
   `bearer`），岛在世 ⇒ 快探 + 一次复探（700ms/1.4s，与岛内阶梯同粒度）⇒ `0`/`-1`；未 attach / 岛不在 ⇒
   `-2` 回落 WG 原路。理由：现状 rc 与 QUIC 数据面**无关**（错误证据源）、改动小、且 M5「只删不设计」。
   预登记的 falsify 指标按设计（`-1` 误判率 ≤1/5、零 RECOVER 行、耗时 ≤8s）。
2. **不保留 `SERVER_TUNNEL_IP` 别名**（采纳设计建议）：按无兼容包袱口径直接删并在 S6 登记（这是判据行
   / 常量面的差异，登记即可，**不留旧形态**）。
3. **W1（并发上界）** = 按设计登记为残余；若实现期发现「上界被路径放大」有实测证据，再单独立项——
   现阶段不扩面（M4 是 1–2 会话日的小期）。
4. **13 条判据登记草案 = 原样落**（S6，与代码同批）；其中与 `100.64.255.1` 字面拨、虚拟端口、
   `target_port=0` 旁路相关的三条已在设计 §8 写明差异理由——**登记即合规**。
5. **`exit/pump.rs` 泛型化 = 放行**（采纳设计建议：复用泛型化泵给 dial 腿，避免第二份泵实现）。
6. **A8 判定表定稿即执行**（14 行：允许/拒 `0x25`+行/不单独判三类）；**SSRF 面结论 = 不新增权限面**
   （已准入设备本就有 L3 任意目的地址全局代理）——与 `probe_addr_acceptable` 的**分面关系**写死，
   **禁止混用**（代码门专项核）。
7. **spec 不回退核验 = 5/5 requirement（10/10 scenario）全覆盖**；其中 **6 项必须真机**（R1-S①/S③、
   R2-S①/S②、R3-S①/S③、R4-S①），R5 标 **vacuous 达标**（全仓 `forward-via-proxy` 零命中）。

---

## 16. 实施期订正（二）（主会话登记，2026-10-09；S6 代码门 r21 交下）

1. **§5.3 的耗时上界订正：7.1s → 12.1s**。原文只计一次 RPC 余量；按代码真实最坏 =
   `(0.7s + 5s) + (1.4s + 5s) = 12.1s`（两次快探各自的 RPC 预算都计满）。**falsify 指标只对实测设门**
   （实测最坏 **2.108s** ≤ 8s），故本条不影响已登记判据；登记面已落 `INTEROP-CRITERIA.md` 的 M4 行 14。
2. **§5.3 ③ 的 `-2` 构成句订正**：`-2` **只**由「无世代 / 陈世代」两条前置产生；**岛不在 ⇒ 走 WG 原路**
   （可达集 `-1/-3/-4`）。原句「岛不在 ⇒ `-2`」与实装相抵（r21 F1）——以本节为准；
   `facade/mod.rs` 的 doc 已同批订正。
