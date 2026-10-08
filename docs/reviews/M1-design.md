# M1「QUIC 承载 + 全局代理」设计文档

> 批次：WG → QUIC 传输层换代程序 **M1**（真源 `docs/QUIC-ROADMAP.md` 的「M1 QUIC 承载 + 全局代理」节）。
> 第 1 棒（设计）产出。**本棒不写产品代码**（实现是后续棒）——本文件是实现的输入契约。
> 基线：工作树 `/Users/zhaozhe/Documents/projects/homeway-rs-quic`（分支 `quic`），
> `HEAD = 1de72a2`，`git status` = `nothing to commit, working tree clean`（复验时刻 2026-10-08 22:00 前后）。
> **行号均为本 HEAD 实测值**，实现以符号定位为准（M0 先例：行号会漂）。
> **范围边界**：本批只做 ①出口 QUIC 端点 + DATAGRAM↔intercept 直通 ②客户端岛（赛跑/迁移/TUN⇄DATAGRAM）
> ③A/B 开关与观测面 ④门槛实测；**不改中继代码、不动 M2 身份面（RPK/Proof）、不动 M3 服务流（STREAM 分发/
> stackb 退役）、不删 WG**。主检出 `~/Documents/projects/homeway-rs`（并发批 Q-L 在途）、
> `~/Documents/projects/homeway`、`~/Documents/projects/tier`、`baseline/` 全程只读。
> **状态**：v2（设计门**已过** @ `/tmp/dsh-review/r12.CrR3qv/`，2026-10-08——结论、原文摘要、逐条处置与残余见 §11；
> v2 已并入全部高危/中危意见；**四个拍板点已由用户 2026-10-08 拍板（结论与落地边界见 §12）**）。

---

## 0. 复验（证据先行）

### 0.1 工作树与绿基线

| 项 | 实测 | 证据 |
|---|---|---|
| 工作树 | 分支 `quic`，`HEAD 1de72a2`，`nothing to commit, working tree clean` | `git status --porcelain`（空输出） |
| `cargo test --workspace` | **exit 101**：10 个测试二进制里 9 个 ok，`homeway-core --lib` **657 passed / 2 failed / 4 ignored**（86.6s） | `/tmp/m1-baseline-test.log` |
| 两条红的定性 | **已登记的 `daemon::tests::*` 时序族**（`handshake_deadline_beats_slow_drip`、`server_bad_frame_gets_goodbye_and_disconnect`）；**隔离复跑（`--test-threads=1` 单跑两例）全绿**（`2 passed`） | 隔离复跑输出；loadavg = `2.04 2.37 2.08`（并行负载）；路线文件「已知 flake 登记」节 + 本程序 flake 口径④（红了先隔离单跑再判回归） |
| M0 收口绿基线 | 735 passed / 0 failed / 17 ignored；clippy `-D warnings` = 0 | `docs/reviews/M0.md` §5.5.1 |
| 岛的静态面 | `Cmd` 三成员、专用线程 + `current_thread`、`hw-quic-reap` 收割；1000 行（`wc -l crates/homeway-quic/src/*.rs`） | 本棒逐文件读取 |
| 依赖面 | quinn 0.11.12 / rustls 0.23.45(ring) / tokio 1.x（rt+time+sync+macros）/ `bytes` **M1 新增** | `Cargo.lock`；M0 设计 §1.1 |

**两条红的处置（如实登记，不掩盖）**：本棒的复验结论 = **无 M1 相关回归**（两例均为既登记 flake，且隔离复跑绿）；**但 M1 实现棒收口前必须再跑一次全量**（`docs/QUIC-ROADMAP.md` 每期执行协议第 1 条：绿基线 = 工作树前提）。

### 0.2 源码接缝重定位（HEAD 1de72a2，逐条实测）

| 接缝 | 位置 | M1 用法 |
|---|---|---|
| `Device::decapsulate` | `crates/homeway-core/src/server/device.rs:179` | **被替代的入口**（QUIC 入站不经过它） |
| `StepOut::PlainV4` 投递点 | `device.rs:279`（`consume_step` 内 `out.plain.push(pkt)`） | 语义等价面：QUIC 入站 = 同一「明文 IPv4 向量」形态 |
| 明文源校验 `src_allowed` | `device.rs:314`（`src ∈ {tunnel_ip, tun_ip}`） | **必须复刻**（M1 QUIC 入站同判） |
| 出站 `Device::encapsulate` | `device.rs:323`；`route_out` `:335` | **M1 出站路由的分流点**（见 §1.5） |
| `device.tick_timers` | `device.rs:363` | WG 档保留（M1 QUIC 用协议自带 keepalive/PTO） |
| 引擎主循环 | `server/engine.rs:1049`（`out`）／`:1171`（tick_timers）／`:1322 handle_inbound`／`:1334 decapsulate`／`:1358 route_encap` | M1 的 QUIC 面接入点（poll 集 + 出站分流） |
| 引擎命令通道（`try_recv`，无唤醒 fd） | `engine.rs:1052`；`EngineCmd` `:141` | 5ms 拍上界 ⇒ QUIC 入站**必须**自带唤醒 fd（§1.7） |
| 出口大缓冲 `SO_SNDBUF/SO_RCVBUF=4MB` | `server/bind.rs:239-260` | M1 `datagram_send/receive_buffer_size` 对齐量级（§1.2） |
| 收工顺序（实测） | `facade/tun_exec.rs:1420-1456`：`pf.stop_all()` → `bridge.stop()` → `client.stop_within` → 缓存终写 → `finish_generation` | 岛停止点与 WG 客户端**同址同序**（M0 设计 §3.5 契约） |
| `Client::start`（生产构造点） | `facade/tun_exec.rs:1100` | 岛构造点参照位（同区段） |
| `link:` 行（隧道域） | `facade/tun_exec.rs:1746` | C10 快照形态出口 |
| `link:` 行（服务会话巡检） | `session/mod.rs:1218` | C10 巡检形态出口 |
| `link` 段 JSON | `facade/tun_status.rs:50-56`／`:154` | `via/ep/rttMs/at` 语义保留面 |
| `wgcore::Cmd` | `wgcore/mod.rs:125`（`TunAttach` `:209`/`TunPacket` `:215`/`Stop` `:225`） | 岛 `Cmd` 的形态先例 |
| `Client` 公面（接缝参照） | `wgcore/mod.rs:1069`；`attach_fd:1471`／`set_candidates:1457`／`rebind:1450`／`path_probe:1260`／`swap_out_pkts:1492`／`tun_stats:1483` | 岛 M1 公面的**语义镜像清单** |
| `tun_read_loop`（阻塞读 + 投递自退） | `wgcore/mod.rs:1764`（`send(...).is_err() ⇒ return`） | 岛 M1 的 TUN 读线程照此（M0 设计 §3.6-2④） |
| C4 MIRROR 行 | `wtransport/bind.rs:404`（节流 `:73-76`） | 赛跑对应行（§3.3） |
| C5 赛跑结算行 | `wtransport/bind.rs:645`（`adopt` `:621`） | 同上 |
| C6 路径确立行 | `wtransport/bind.rs:673` | 同上 |
| C15 RREG 行 | `wtransport/bind.rs:726` | M1 控制流登记刷新（§3.3） |
| 中继准入限流 200pps/源 | `relay/mod.rs:665`（`handle_udp_packet` → `rate_ok`）；`rate_ok` `:1576`；`DEFAULT_RATE_LIMIT` `:51` | **M1 预算复核锚点**（§7） |
| 中继上行转发 | `relay/mod.rs:907 forward_up`（保 kind 原样：`:1046` 附近 `encode_frame(kind, payload, …)`） | 新帧类型 `kind=5` 可透传（§1.6） |
| 中继下行转发 | `relay/mod.rs:1202`（`udp.send_to(pkt, key.client)` = 主 socket 发回） | 客户端看到的源 = 中继主端口（QUIC 端点地址一致） |
| 中继会话键 | `relay/mod.rs:156` `AssocKey { label, client }` | 客户端换源 ⇒ 新建 assoc/腿（迁移链已核，§2.3） |
| 腿帧信封 | `wtransport/frame.rs:93` `[0xAA][id8]‖[0xBB][kind]‖payload`（**上行 +11B**）；下行恒 `[0xBB][kind]`（**+2B**） | MTU 头寸与线开销（§5） |
| 岛驱动循环 | `crates/homeway-quic/src/driver.rs:242`（`select!` 两源）；`cmd.rs:18` `Cmd` | M1 增第三源（连接 IO）+ 成员（§2.1） |

### 0.3 本棒实测（探针 `/tmp/m1-probe`，**仓外**，与 M0 依赖面同版）

> 目的：把 M1 设计里**只能靠实测**才能定的五件事钉死（迁移 / ACK 预算 / 缓冲语义 / 超限分类 / 握手成本）。
> 探针 = 单文件 `main.rs`（quinn 0.11 + rustls(ring) + tokio，自签 DER 现场生成于 `/tmp/m1-probe/certs/`），
> 产物 = `/tmp/m1-res/*.txt`。**全部读数标注为「本机 Mac mini M2 / 回环 / 单进程」口径**，不外推为真机结论。

| # | 断言 | 实测（证据文件） |
|---|---|---|
| P1 | **握手包数**：客户端 4 包（Initial+Handshake+…）/ 服务端 3 包；回环建连 1–3ms | `m1-res/hs-cli.txt`（`udp_tx_dg=4`）、`hs-srv.txt`（`udp_rx_dg=3`） |
| P2 | **MTU 与 `max_datagram_size()`**：MTU1400→**1362**、MTU1200→**1162**（与 `docs/QUIC-BASELINE.md` 逐字符一致）；`initial_mtu(1400)` 起手即 1400，回环 DPLPMTUD 升到 **1452**（默认上界） | `tl-1400-*`、`tl-1200-*`、`hs-cli.txt`（`current_mtu`） |
| P3 | **`TooLarge` 分类**：`len ≤ mds` 全 Ok，`len = mds+1` 起恒 `Err(TooLarge)`；MTU1200 下 1280B 内层包 = `Err(TooLarge)` | `tl-1400-cli`（1362 Ok / 1363 Err）、`tl-1200-cli`（1162 Ok / 1163/1280 Err） |
| P4 | **⚠️ 裸 `send_datagram()` 在缓冲满时静默淘汰最旧**：缓冲 1MiB、对端不读 ⇒ 连发 **200,001** 个 1280B 数据报**全部返回 Ok**（`space` 钉在 256B，无任何错误/计数） | `buf-cli.txt`（`accepted=200001 err=None space_now=256`） |
| P5 | **下行 ACK 预算**（对上行 200pps 闸最关键）：服务端单向发 3196 个数据报（3206 UDP 包），客户端回 **805 个纯 ACK 包**（≈ **1 个 ACK / 3.97 个下行包**）；设 `ack_eliciting_threshold=10` 后 ⇒ **271 个 ACK**（≈ 1/11.8） | `push-0-cli.txt` / `push-0-srv.txt`；`push-10-cli.txt`（`fr_tx_acks=271`） |
| P6 | **上行 piggyback**：边发边收时 3196 个数据报只占 **3366** 个 UDP 包（**1.05 包/数据报**——ACK 与数据共包，几乎无独立 ACK 包） | `echo-cli.txt` / `echo-srv.txt` |
| P7 | **迁移**：客户端 `Endpoint::rebind()` 换本地地址（`127.0.0.1:*` → `192.168.3.12:*`）后**连接保持**（`closed=false`、后续 6 发 6 回收全通），服务端 `remote_address()` 就地变为新地址（`REMOTE-CHANGED`），过程中客户端收到 **3 个 PATH_CHALLENGE**（路径验证由协议自动完成）；`current_mtu` 重探。**口径订正（r12 专1-2）**：v1 写的"`cwnd` 回落 12000 = 拥塞态重置"**证据不成立**——迁移前 cwnd 本来就是 12000（只发过 6 个小包、从未增长），读数无法区分"重置"与"没动过"；协议上真迁移确实重置（`quinn-proto/src/connection/paths.rs`）、同 IP 换端口（NAT rebinding）**不重置** ⇒ 计量口径见 §2.3（先跑 bulk 再迁移） | `migL-on-*`（`post_ok=6 recv_back=12`、`fr_rx_path_challenge=3`） |
| P8 | **`migration(false)` 的反例**：服务端把新源当陌生包丢弃、继续发往旧地址 ⇒ 上行全死（12 次回包只收到 6 个 = 迁移前的），连接不自愈 | `migL-off-cli.txt`（`recv_back=6`）、`migL-off-srv.txt`（无 `REMOTE-CHANGED`） |

### 0.4 复现命令（可直接跑）

```bash
# 仓内绿基线（本棒开工复验；日志 /tmp/m1-baseline-test.log）
cd /Users/zhaozhe/Documents/projects/homeway-rs-quic && cargo test --workspace

# 探针（仓外；先建 certs，再 build）
cd /tmp/m1-probe && openssl req -x509 -newkey rsa:2048 -keyout certs/key.pem -out certs/cert.pem \
  -days 3650 -nodes -subj "/CN=localhost" >/dev/null 2>&1 \
  && openssl x509 -in certs/cert.pem -outform DER -out certs/cert.der \
  && openssl pkcs8 -topk8 -nocrypt -in certs/key.pem -outform DER -out certs/key.der
CARGO_TARGET_DIR=/tmp/m1-probe/target cargo build --release
# 每项实测的起服/发压命令见 /tmp/m1-res/README.txt（与 §0.3 表逐行对应）
```

---

## 1. 出口端点设计

### 1.1 端点形态与端口（**新增独立 UDP 端口**）

**决策**：出口新增一条独立 QUIC 监听（`serve.quic_listen`，缺省 = `serve.listen + 1`，被占用按既有「退让」语义换端口并把**实际端口**写进 token），**不复用 WG 的 `serve.listen` 端口**。

理由（不这么做会撞的三件事）：

1. `quinn::Endpoint` **独占**一枚 UDP socket（`Endpoint::new(config, server_config, socket, runtime)`——socket 交给它后由它读）；而今天 `serve.listen` 的 socket 由 `ServerBind` 独占（`bind.rs` 的 poll 集 + 腿 fd 同构）。两栈共用一个 fd 需要自写 demux（WG 首字节 0x01–0x04 vs QUIC 长头 0x80–0xff/短头 0x40–0x7f，判据可行但**双栈共享 socket 是纯增复杂度**）。
2. M1 是**双栈并存期**：WG 路径必须原样可用（A/B 回退 + 服务面自带连接仍走 WG，见 §1.5/§2.6），单一端口会强迫两条栈的生命周期耦合。
3. 中继路径的腿 socket 本来就是**每条会话一枚**（`relay/mod.rs:907 forward_up` → 拨腿模式下 `a.sock` 独立），端口复用并不减少任何真实资源。

**token 面**：端点表新增一类 QUIC 端点（形态照 `（内网）/（公网）/（中继）` 的既有 kind 后缀，例：`192.168.3.12:42652（QUIC 内网）`）。**这是 token 格式的 additive 变更**（M2 会重写 token，本批只加一类端点、不动 MAC/字段布局），登记条目见 §3.6。

### 1.2 TransportConfig 定稿（逐项给理由）

| 参数 | 值 | 理由 / 依据 |
|---|---|---|
| `initial_mtu` | **1400** | 内层 1280 + QUIC 头；M0/本棒实测 1400⇒`max_datagram_size()=1362`（§0.3 P2）够装 1280 |
| `min_mtu` | **1320**（**不是** 1200） | **黑障回退的落点**：黑障检测命中时 `current_mtu` 一跳落到 `min_mtu`（`quinn-proto/src/connection/mtud.rs:55/160`，无逐级下探）。要装下 1280 内层包需 `max_datagram_size() ≥ 1280` ⇒ MTU ≥ **1318**（1RTT 开销实测 38B：1362 = 1400−38）⇒ 取 1320（mds 1282，2B 余量）。**取 1200 = 黑障后 mds 1162 ⇒ 1280 内层包全丢（隧道功能性死亡）**，故不用协议地板值 |
| `mtu_discovery_config.upper_bound` | **1400**（= `initial_mtu`，**构造性关闭上探**） | ①上探的意义是"用满路径"，而我们的上限被**信封头寸**锁住（中继上行 +11B；IPv6 路径 1500 的可用 UDP 载荷 = 1500−48 = 1452，取 1452 时 1452+11 = 1463 ⇒ IPv6 总长 1511 > 1500 会超；取 1400 时 1400+11+48 = 1459 ≤ 1500，**余 41B**）；②`upper_bound == current` ⇒ 二分区间退化 ⇒ 一个探测包都不发（`mtud.rs:303-345` 的 `SearchState::new` + `next_mtu_to_probe`，本棒复核）；③**黑障检测仍活**（`MtuDiscovery` 恒建 `BlackHoleDetector`，与搜索态无关）⇒ 窄路径保护不丢。**代价登记**：黑障后停在 1320 且不会自行回升（≈4.3% 线开销），且 `mtu_discovery_config` 不能设 `None`（那会连黑障检测一起关掉） |
| `datagram_send_buffer_size` | **1 MiB** | quinn 默认量级（`transport.rs:396`）；**不是** 4MB——4MB 是今日**单 socket 全设备共享**的量级，逐连接直译 = 32×8MiB 不可接受（裁决见 §6.3） |
| `datagram_receive_buffer_size` | **1 MiB** | 同上（quinn 默认 = `STREAM_RWND` = 1,250,000B，`transport.rs:370/395`；取 1MiB 便于口径统一） |
| `ack_frequency_config` | `ack_eliciting_threshold = **16**` + `max_ack_delay = **5ms**`（出口侧下发） | **中继 200pps 预算是硬约束**（§7.2 B3 重算：默认 ACK 比值 3.97 ⇒ 下行上界 ≈9 Mbps；需比值 ≥ **8.4** 才够 19 Mbps、≥12 才够 27 Mbps）⇒ 必须显式开启；`max_ack_delay` 必须一起设（不设 = 沿用对端 TP 的 25ms，会给内层 TCP 的 RTT 观测与慢启动节奏加 25ms 噪声）。副作用登记：ACK 阈值拉大后乱序 ACK 行为变化（`reordering_threshold` 默认 2）+ ACK-based 丢包检测变粗 |
| `max_idle_timeout` | **30s**（默认值，暂不改） | 与今日 WG 的"无流量即有界回收"量级匹配；中继注册腿/分配腿回收窗 90s（`relay/mod.rs:44-45`）⇒ 需 `keep_alive_interval` 兜底 |
| `keep_alive_interval` | **10s** | 中继回收窗 90s（扫描粒度 5s ⇒ 实际 90–95s，`reap_interval`）+ NAT 老化；10s PING = 0.1pps，占上行预算 0.05% |
| `congestion_controller_factory` | 默认 CUBIC | 与 smoltcp 侧 CUBIC 同族（AGENTS 技术底座）；不做实验性替换 |
| 接收缓冲溢出行为 | **静默丢最旧**（`datagrams.rs:145-152`） | 与今日内核 rcvbuf 溢出（丢新）**同为丢**、位置不同 ⇒ 靠自计计数暴露（§6.1/§6.4）；另有**第 5 条静默通道** `drop_oversized`（MTU 变小时清掉已排队超限包，`datagrams.rs:176-198`，调用点 `connection/mod.rs:1429/1799/3089`）⇒ 岛内**每拍比对 `max_datagram_size()` 变化并把自有队列中超限包计入 `超限`**（§6.4） |

### 1.3 连接 ↔ 设备绑定（M1 的准入面：**复用 reg 帧 + 连接绑定；不弱化**）

**决策**：M1 的登记面 = **把今天的 `reg` 帧（`wtransport/reg.rs`）放到 QUIC 的第一条双向控制流上**，出口侧仍走 `server/table.rs::register()`（devTag 键、TTL/grace/max_devices/吊销语义逐字不变），**并加一条连接绑定**：

```
reg3: "H3" ‖ pubkey32 ‖ devTag8 ‖ ts8 ‖ mac16
      mac = HMAC-SHA256(token secret, "hr-reg3" ‖ pubkey ‖ devTag ‖ ts ‖ exporter32)[:16]
      exporter32 = Connection::export_keying_material(out, b"hw-quic-reg", b"")[:32]
                   （quinn/src/connection.rs:625 —— 两端同一连接上导出，实测 API 存在）
```

- **为什么必须做连接绑定（设计门 r12 的 B2，高危）**：去掉「pubkey 必须完成 WG 握手」这条绑定后，仅凭 MAC 的 reg 帧在 ±90s 内被**重放**（被动窃听者）就能让攻击者获得一条**可用**的 QUIC 隧道连接（今日重放只能污染设备表——攻击者没有 WG 私钥，完不成握手）。把 TLS exporter 混入 HMAC 后，重放的帧**换一条连接就验不过**，回放面关闭；代价 = reg 帧加一个版本（`H2`→`H3`）与一个 32B 绑定值（`reg.rs:1-18` 的 v1→v2 先例同款做法）。
- **安全性口径（订正后）**：M1 的准入 = 「持有 token secret **且** 在该连接上完成 exporter 绑定」。与今日同档的部分 = token 持有即准入；**新增**的部分 = 绑定到具体连接（杀掉回放）。
- **客户端证明的另一半（RPK + Hello/Challenge/Proof）仍留 M2**：M1 不做客户端主动挑战应答（那条的价值在"设备表按连接 = 设备"的完整语义里，M2 落地）。
- **服务端身份（设计门 r12 的 B1，高危；本设计门必须正面回答）**：**M1 必须给客户端一个可验证的服务端身份**——否则客户端只能在产品面用 `dangerous()/SkipVerify`，而 M0 隔离门（`crates/` 内 `dangerous()` 零命中）与 AGENTS 的安全纪律都不允许，且 M1 的退出口（"浏览器/任意 App 经隧道上网全通"）没有意义。
  - **本设计的裁决**：**把路线文件 M2 交付物的"服务端身份半边"提前到 M1**——出口 Ed25519 RPK（RFC 7250，rustls `requires_raw_public_keys` + `AlwaysResolves*RawPublicKeys` + `verify_tls13_signature_with_raw_key`，M0「立项依据」已核过 API 存在）+ **公钥进 token**（token 格式 additive 变更，用户拍板①"无兼容包袱"允许）+ 客户端钉定校验。**客户端证明半边（Hello/Challenge/Proof/抗放大）仍在 M2**。
  - **已裁决：提前到 M1**（**用户 2026-10-08 拍板**，见 §12-②；路线文件把身份整块排给 M2 ⇒ 属范围**前移**，由主会话同批同步路线文件）。理由与最小面：**不做 ⇒ M1 无法安全收口**；做 ⇒ 提前的只是"出口密钥 + 公钥进 token + 客户端钉定"，工作量 < M2 的一半；**客户端证明半边仍留 M2**（拍板原文见 §12-②）。
  - ~~兜底（若裁决"不提前"）~~：**未采纳**（用户已拍板提前落地）——保留原文以备追溯：那时 M1 的 `transport=quic` 只能作本地/实验档、生产默认保持 wg，且"客户端不验证服务端"是不可接受形态。
- **绑定产物**：`table.register()` 返回的设备条目（`tunnel_ip` / `tun_ip`）与 QUIC 连接句柄构成双向表 `DevKey ↔ ConnectionHandle`；入口校验 = 仅接受「已完成 bidi 控制流登记」的连接的数据报（未登记者数据报直接丢 + 计数）。
- **入站源校验**：复刻 `device.rs:314 src_allowed`（`src ∈ {tunnel_ip, tun_ip}` 才投 `intercept`），拒绝计数（对应今日 `src_rejects`）。
- **撤销/轮换**：`table` 的 remove/rotate 语义（`peer: - …` / `peer: ~ … rotate`）在 QUIC 侧对应「拆连接」（发 `CONNECTION_CLOSE` 或直接 drop 句柄），M1 只做最小对齐（M2 完整设备表语义）。

### 1.4 入站：DATAGRAM → intercept（在出口**不经过 device.rs**）

```
QUIC DATAGRAM(Bytes)                       [岛线程/面]
  → 连接查绑定（1.3）→ 源校验（src_allowed 复刻）→ 入队 + 唤醒 fd
      → 引擎 poll 醒来 → drain → intercept.on_plain(pkt)      [引擎线程]
          → 既有 DNS 代答 / 过境重拨（5333 行 intercept 原样）
```

- `intercept.on_plain(pkt: Vec<u8>)`（`server/intercept/mod.rs:947`）**签名与语义零改动**——这正是路线文件说的「接缝 = 今日 `device.rs` 的明文面」。
- 入队/唤醒是**必须的**（不是优化）：引擎主循环的命令面用 `try_recv` + `poll(…, 1|5ms)`（`engine.rs:1140-1152`），没有唤醒 fd 时每个入站包最多等 5ms ⇒ TCP RTT/吞吐直接受损。**唤醒 fd 直接进 poll 集**（与腿 fd 同构：`pollfds.push(quic_wake_fd)`），到点即 `intercept.on_plain`。

### 1.5 出站：intercept → DATAGRAM（**按目的地址分流，双栈并存的判定键**）

`route_encap`（`engine.rs:1358`）今天对每个出站包调 `device.encapsulate(dst)`。M1 改成**两道闸**：

| 出站包 dst | 去向 | 依据 |
|---|---|---|
| `dst == 某设备的 tun_ip`（`hw-app` 派生地址 = App 的 TUN 接口地址） | **QUIC DATAGRAM**（发往该设备连接） | App 侧浏览器/任意 App 流量（全局代理面） |
| `dst == 某设备的 tunnel_ip`（`hw-tun`，= 客户端核栈 B 地址；服务面自带连接） | **WG `device.encapsulate`（原样）** | M1 不动服务面（M3 才把 files/term/speedtest 换成 STREAM） |
| 其它 | 原样丢 + 计数 | 与今日 `route_out` 未命中同义 |

**为什么要这条分界（而不是"全走 QUIC"）**：客户端核的 `bridge_host` 拨号走 `session_connect_target` → `gen_client.connect_deadline`（`facade/tun_exec.rs:229/238`）——**今天就是 WG/栈 B**；M3 才换 `STREAM[tag]`。若 M1 把 tunnel_ip 的回程也搬到 QUIC，服务面（files/term/speedtest/portfwd dial）会当场断。地址语义真源 = `tunnel_addr.rs:1-17`（两个派生函数、标签 `hw-tun`/`hw-app`）。

**背压**：出站**禁止**裸 `quinn::Connection::send_datagram()`——实测它会静默淘汰最旧（§0.3 P4）。定稿形态：

```
per-conn: if conn.datagram_send_buffer_space() >= pkt.len() { send_datagram(pkt) } else { drop + 计数 }
```
（`datagram_send_buffer_space()` = `quinn/src/connection.rs:508`；语义 = 「小于等于它的发送不会挤掉旧数据报」，正是我们要的不静默面。）

### 1.6 中继承载（**中继零改动**；出口侧加一层「帧类型 + 自定义 socket」）

中继把客户端的标签帧 `[0xAA][id8]‖[0xBB][kind]‖payload` 按 kind **原样**转发（`forward_up` 保 kind：`relay/mod.rs:907` → `encode_frame(kind, payload, …)`）；下行由中继主 socket 原样发回客户端（`:1202`）。据此：

1. **新增帧类型 `kind=5`（QUIC 载荷）**：客户端发 QUIC 包时用 kind=5（而不是 Data=0），出口的腿读路径按 kind 分流——`5 ⇒ QUIC 面`，其余 ⇒ 今日 WG 路径。中继侧**零改动**（它不解释 kind；`decode_tagged` 只校验魔数）。
2. **出口 QUIC 面用自定义 `AsyncUdpSocket`**（`quinn::Endpoint::new_with_abstract_socket(config, server_config, socket: Arc<dyn AsyncUdpSocket>, runtime)` — `quinn/src/endpoint.rs:133`；trait = `quinn/src/runtime.rs:42`）：
   - **recv 侧**：直连 QUIC 端口 socket + 各中继腿 socket + 引擎注入队列（kind=5 帧从这里进）；
   - **send 侧**：`try_send(Transmit)` 按 `Transmit.destination` 路由——目的地是某条腿的对端（中继 assoc）⇒ 走该腿 socket 并包 `[0xBB][5]`；否则走直连端口 socket。
   - 这条包装是**「中继零改动」与「一条 QUIC 连接既走直连又走中继」两件事的交叉点**：不引入它，就得让中继理解 QUIC 或者让出口为每条腿起一个端点（后者无法在腿建立时判定"这条腿是 QUIC 腿"——SESSION 通告只带 `{id, data_port, cookie}`，见 `relay/mod.rs:907` 区段）。
   - **复杂度如实登记**：约 200–300 行新代码（一个实现 `AsyncUdpSocket` 的结构 + 腿/直连两张 socket 表）；「实现期须复核」= `Transmit.src` 是否可用作 socket 选择键（本棒只核到 trait 与构造函数签名，未核 `Transmit` 字段——**不确定项，实现棒第一步验证**）。
   - **段能力如实登记（设计门 r12 增补）**：自定义 socket 的 `max_transmit_segments/max_receive_segments` 默认 = 1（`quinn/src/runtime.rs:72-80`）⇒ **丢掉多段批处理**（quinn-udp 内建在 Linux/OHOS 上可用 GSO/`sendmmsg`）。**这条直接落在每包 CPU 门槛上**（`tools/quic-ab.sh cpu` 测的是**内建 socket**，不是产品路径）⇒ ①S5 的端到端 A/B 必须是**产品路径**读数（不是探针臂）；②若实测回退明显，退路 = 按底层 socket 分别实现抽象 socket（直连 socket 走 quinn-udp 的 `UdpSocketState`，腿 socket 保持 1 段）。**登记为 M1 的已知性能风险（§9.3 Q-K）**。
   - **客户端同样需要包封/剥壳（设计门 r12 的 B6，高危）**：§1.6 只写了出口侧。客户端岛必须成对实现（**M1 新代码，不是改 WG 侧**）：
     - 上行：候选属 `via=relay` ⇒ 组 `[0xAA][label8][0xBB][kind=5]‖quic_pkt`；属 `via=direct` ⇒ 裸 QUIC 包；
     - 下行：从直连 socket 收到裸包、从「中继接收路径」收到 `[0xBB][kind=5]` ⇒ 剥壳后喂 quinn；非 kind=5 帧忽略（不喂 quinn，也不当数据）；
     - 归属：**放在岛内的 socket 层**（与出口侧同构），不是 `wtransport`（WG 面零改动）。切片与完成判据见 §10 S2-7。

### 1.7 出口线程模型与生命周期

- QUIC 面 = **一枚专用线程 + `current_thread` runtime**（与客户端岛同构，M0 设计 §3.2），线程名 `homeway-quic-exit`；不启 `rt-multi-thread`（构造性单线程不变量）。
- 与引擎的接口 = **两向队列 + 唤醒 fd + 命令通道**：入站（DATAGRAM→引擎）与出站（引擎→QUIC）都走有界队列（容量按 §1.2 的 4MiB 量级折算条数，满 ⇒ 丢 + 计数，不许静默）。
- 收工顺序（出口域）：`bind.shutdown_legs()` → `intercept.halt_new()` → **QUIC 面 `stop_within(CLOSE_BUDGET)`** → `intercept.close()`（与 `engine.rs:1290-1320` 的既有顺序同址插入；QUIC 面停止点在 `intercept.close()` **之前**，理由：关闭前还要把尾包投给 intercept，反之会丢尾）。

---

## 2. 客户端岛设计（赛跑 / 迁移 / 数据面）

### 2.1 `Cmd` 声明面增补（照 M0 §3.3 纪律：逐条给「带 reply 与否 + 理由」）

| 成员 | reply | 理由 | 期 |
|---|---|---|---|
| `SetOnUnhealthy{h}` | ✗ | 与 M0 声明面一致（回调在岛线程执行；`mark_unhealthy_if_current(gen,"panic")`） | M1 |
| `Connect{cands: Vec<Candidate>, budget, reply}` | **✓** | 赛跑结果是同步面要等的**一次性结论**（via/ep/rtt 入状态面），且失败要能归因（`IslandErr::NoCandidate`）。`budget` = **整轮预算**（不是每候选）：到点未完成者按 drop 收（见 §2.2） | M1 |
| `Rebind{local: Option<SocketAddrV4>, reply}` | **✓** | 迁移是**用户可见事件**（C 系列行 + 状态面）；失败必须可回报（否则同步面只能靠超时猜）。⚠️ 语义提醒：`Endpoint::rebind` 是 **endpoint 粒度**（影响该端点全部连接，`quinn/src/endpoint.rs:247-263`），旧 socket 只续收片刻、**对端不可达没有专用错误**（只能等 `max_idle_timeout` 静默收场）⇒ 调用方必须自己给"rebind 后 N 拍无回包 ⇒ 回落重连/重赛跑"的判据（§2.3） | M1 |
| `SetCandidates{cands}` | ✗ | 与 `wgcore::Cmd::SetCandidates` 同形（高频、无回执、丢一次无害） | M1 |
| `Probe{reply}` | **✓** | 巡检判活要结论（替代 `path_probe`）；预算由调用方给 | M1（**接线见 §2.5**） |
| `DatagramDropped{reason: DropReason, n}`（事件） | ✗ | 岛 → 同步面的**分类计数事件**（超限/发送缓冲满/回程队列满/未登记——**enum 而非裸计数**，AGENTS 原则 1）；M0 声明面已预留此位 | M1 |
| `StreamOpen{tag,reply}`/`StreamWrite`/`StreamRead`/`StreamClose` | ✓/✓/✓/✓ | 服务流面（M3 定型）；M1 只落 `tag=5 probe` 的最小占位 | M3 |

**候选类型（类型承担不变量，AGENTS 原则 1）**：

```rust
/// 候选 = 地址 + 承载类别（中继候选必须带 label 才能组信封帧）。
pub struct Candidate { pub addr: SocketAddrV4, pub via: Via }
#[non_exhaustive]
pub enum Via { Direct, Relay { label: [u8; 8] } }
```
（`homeway-core` 侧的 `Candidate` 在边界转换成岛侧形态——M0 设计 §1.2 的边界纪律；**候选按 `transport` 过滤**：quic 档只吃 QUIC 类端点 + 中继端点的 QUIC 形，WG 档不吃 QUIC 端点——防两族候选互相投喂（设计门 r12 专2-6）。）

`IslandSnapshot` 增补（同步面轮询，无锁阻塞）：`via`（none/direct/relay）、`ep`、`rtt_ms`、`mirrors`（赛跑尝试数）、`packets_in/out`、`drops`（四类丢弃计数）、`mtu`（`max_datagram_size()` 现值）。

### 2.2 赛跑裁决（**首个完成握手者胜**）

| 问题 | 裁决 | 依据 |
|---|---|---|
| 谁赢 | **首个 `Connecting` 转为已建连（1-RTT 可用）者** | QUIC 握手完成即双向可用（无 WG 的"响应过/首包"两段语义）；实测回环 1–3ms（P1） |
| 何时算输 | 输家 `Connecting` **drop**（释放句柄）。⚠️ **订正（设计门 r12 专2-1，高危）**：quinn 的 drop **会**触发 `implicit_close()` ⇒ 在最高可用密钥空间发一个 `APPLICATION_ERROR` CONNECTION_CLOSE 并停表（`quinn/src/connection.rs:946-962/1255-1257`、`quinn-proto/src/connection/mod.rs:1273-1281`）。所以口径应写「**输家被主动关闭**（出口因此更快释放半开连接），岛不等待关闭完成」——这正是想要的副作用，但**不是**"不发包" | 本棒复核 quinn 源码 |
| 同 devTag 现任裁决 | **后到者替换并关闭旧连接**（沿用 `table` 的"同 devTag 同 pubkey = Refreshed/替换"语义）；岛在胜出时对**任何已建立但未胜出的连接**显式 `Connection::close()`；出口的 `DevKey ↔ conn` 绑定表以**最新登记者**为现任（避免回程发往被丢弃的连接造成黑洞到 idle timeout） | 设计门 r12 专2-2（高危）：并行 connect 可能两条都完成握手并各自登记 |
| 候选面 | token 端点表（LAN/公网/QUIC 类）+ 学习缓存（`EndpointCache` 原样复用，M1 不新增缓存面） | 路线文件 M1 范围「替代候选镜像」 |
| 与 C4 的关系 | **只对 quic 档**：quic 档不再有「镜像包」（QUIC 无镜像概念），由新增 `quic: 赛跑投出 …` 行承接；**WG 域 C4 原样保留**（服务会话仍走 `wtransport::Bind`；A/B=wg 档亦然）——两处措辞的冲突就此消除（r12 覆盖度节的"低"条） | 本条 = 判据行改写的第一处（登记草案 §3.6） |
| 与 C5/C6 的关系 | C5（赛跑结算：胜者/响应过/未响应）语义保留（字段含义变：镜像包数 → 尝试候选数）；C6（路径确立：首个回包来源）语义保留（= 首个完成握手的候选） | 「语义保留、行文改写」 |
| 失败面 | 全候选失败 ⇒ `IslandErr::NoCandidate`（同步面按今日 `NoCandidates` 语义升级恢复阶梯；M1 的阶梯 = 既有 `session/recover` 原样，M3 才重写） | 承接 `docs/QUIC-ROADMAP.md` M3 范围 |
| 触发时机 | 世代装配（= 今日 `Client::start`/`Bind::open` 位）；rearm（软/硬）沿用 `session/recover` 的调用点 | 接缝表 §0.2 |

### 2.3 迁移（`rebind()`，WiFi→蜂窝）

- **原语**：`Endpoint::rebind(new_socket)`（`quinn/src/endpoint.rs:243`，影响该端点的全部连接）+ 服务端 `migration(true)`。**两条都必须**：实测只做客户端 rebind、服务端 `migration(false)` ⇒ 路径当场死（P8）。
- **触发链**（复用今天已成立的信号面，不改 `session/recover` 结构）：巡检失败/写失败 → `unhealthyReason` 家族信号 → **M1 新增一个动作**：`Rebind`（换 socket）优先于「重连/重赛跑」（因为 QUIC 迁移保设备表、保连接状态）。
- **可观测**：服务端 `remote_address()` 变化即「出口设备表不新增条目」的证据（连接未重建 ⇒ 不触发 `table.register`；实测服务端 `REMOTE-CHANGED`，P7）。
- **代价（必须写进判据）**：迁移后**拥塞态重置**（实测 `cwnd` 回初始 12000，P7）⇒ 迁移瞬间吞吐掉到慢启动；`current_mtu` 重探。M6 真机要按「迁移后 X 秒恢复」计量，M1 只登记事实。
- **迁移的本地可验代理**（真机 WiFi→蜂窝不可本地跑）：
  - 探针已证 **两段式注入**可行——①`socket A = 127.0.0.1:0 → rebind → socket B = <LAN IP>:0`（**源地址族变化**，等价「换网卡」）；②断言「连接未断 + 服务端 remote 变化」（P7 命令，证据 `/tmp/m1-res/migL-on-*`）。
  - ⚠️ **订正（设计门 r12 专1-2/专1-3）**：①**`<LAN IP>:0` 的目的地址仍是 `127.0.0.1`**，即"同接口、无 MTU 变化、无丢包、无 NAT 重绑"——它**只证明协议侧迁移路径可用**，**不能**支撑"窄路径/换网"结论；②"迁移后拥塞重置"的 P7 读数**不成立**（迁移前 `cwnd` 就是 12000 = 从未增长过，读数无法区分"重置"与"没动过"；协议上真迁移确实重置、同 IP 换端口（NAT rebinding）**不重置**——`quinn-proto/src/connection/paths.rs` 两条分支不对称）。
  - ⇒ **M1 的本地验证升级**：集成测试的服务端绑 `0.0.0.0:0`、客户端在**两个真实本地地址**间 rebind（darwin 上 `127.0.0.1` ↔ `<LAN IP>`；Linux/CI 用 `127.0.0.1` ↔ `127.0.0.2` 制造 **IP 变化**以走真迁移分支），并**显式标注**：路径 MTU 变化 / 丢包 / NAT 重绑三类**只能真机验**（M6）。
  - 迁移的"拥塞重置"计量改为：**先跑 bulk 让 cwnd 长到 MB 级再迁移**（探针 P5 的 `push` 形态已能到 2.1MB），M6 真机口径 = 迁移后 X 秒恢复。
- **经中继的迁移（M1 必须实测，不许只写"协议侧已核"）**：客户端换源 ⇒ 中继 `AssocKey{label, client}` 不等 ⇒ **新 assoc + 新 sid + SESSION 通告**（`relay/mod.rs:156/908/1015`）⇒ 出口重拨腿（LEGUP）⇒ 出口才看到新源端口。期间上行进 `a.pend`，上限 `CTL_PEND_MAX=16`（`relay/mod.rs:54`），第 17 包起 `dropped`；`dial_wait=15s` 内无 LEGUP 整条 assoc 回收（`:1501-1505`）；出口腿表上限 `RELAY_LEG_MAX=64`（`bind.rs:46`）。⇒ S5-3 增加一条用例（`local-rust-relay.sh` + 只留中继候选 + rebind）：断言连接保持、**腿表/assoc 峰值实测**、pend 窗丢包量登记。
- **`rebind` 的失败面（订正，设计门 r12 专1-6）**：`rebind` 后若对端不可达，**没有专用错误**，只能等 `max_idle_timeout`（30s）静默收场 ⇒ 触发链必须补：**rebind 后 N 拍（N 取既有 patrol 节拍）无回包 ⇒ 记行 + 回落"重连/重赛跑"**（计数入 N-c 的"未登记"邻域，具体字段实现期定）。

### 2.7 M1 的**候选来源收窄**（学习缓存与 hint 的 QUIC 缺口，设计门 r12 6.4 高危）

- **问题**：`EndpointCache` 学的是 **WG 端点**（hint 携带的是 WG 腿地址、端口是 WG 端口），而 QUIC 走**另一个端口**（`listen+1`，且可能"退让"）。§2.2 的"缓存原样复用"会喂出**打到 WG 端口的错候选**。
- **M1 的裁决（收窄，登记为已知缺口）**：**QUIC 候选只来自 token 端点表的 QUIC 类 + 中继端点**；`EndpointCache`/hint **在 M1 只服务 WG 面**（`wtransport::Bind` 原样），**不喂岛**。理由：①正确性优先——错候选会造成"握手超时"噪声与赛跑结算失真；②hint 的语义（WG 腿地址）需要中继侧改协议才能携带 QUIC 端口，而 M1 的红线是"中继零改动"；③把 hint→QUIC 候选的映射留给 M3（服务流迁移后统一候选模型）。
- **代价如实登记**：QUIC 档**失去"学习候选/打洞升级"能力**（今天 C13 的「·学习」条目与 RELAY-UPGRADE 条纹在 quic 档**不会发生**）⇒ 这也是"中继驻留后无法自动升直连"的 M1 已知收窄，M3/M6 再补（写进 M1 的残余登记与真机验证脚本的预期）。

### 2.4 数据面：TUN fd ⇄ DATAGRAM

| 方向 | 形态 | 依据 / 纪律 |
|---|---|---|
| TUN → 岛 | 专用 std 读线程（`homeway-tun-read` 同形）阻塞读 → `Cmd::TunPacket(Box<[u8]>)`；投递失败即自退 | `wgcore/mod.rs:1764` 先例 + M0 设计 §3.6-2④ |
| 岛内发送 | `max_datagram_size()` 检包 ⇒ 超限 **丢 + 计数行**；否则 `datagram_send_buffer_space()` 预检 ⇒ 不足 **丢 + 计数**；都过 ⇒ `send_datagram(Bytes)`（**零拷贝**：`Box<[u8]>` → `Bytes`） | §0.3 P3/P4；判据「超限丢弃可观测、不静默」 |
| DATAGRAM → TUN | 岛内 `read_datagram()` → **有界队列 → 专用 std 写线程**（阻塞写 + 既有 `write_fd_all` 期限语义）⇒ 队列满 **丢 + 计数** | 岛内不得做阻塞 syscall（M0 设计 §3.6-6）；写线程形态照 `wgcore` 的 `write_fd_all` |
| 计数行 | 四类：`超限`（TooLarge）/`发送缓冲满`/`回程队列满`/`登记前丢弃` | 「丢弃可观测」判据的落点（§8） |
| 需求信号 | 读线程更新 `out_pkts`/`last_outbound`（`TunCounters` 语义）——**M1 保留同一套**（demand-driven 恢复逻辑不动） | `wgcore::TunCounters`（`:260`） |

### 2.5 巡检 / 需求信号（**必须真接线，不是占位**）

> **订正（设计门 r12 的 B3，高危）**：本文 v1 把岛 `Probe` 写成"占位"，但**隧道域的巡检今天探的是 WG**（`facade/tun_exec.rs:1661 patrol_loop` → `client.path_probe()` 探 `tunnel_ip:1`，失败 3 连 → `mark_unhealthy_if_current(gen,"patrol")`）。若 QUIC 档下巡检仍探 WG，就会出现**「QUIC 死而 WG 活 ⇒ App 流量黑洞且无人恢复」**——M1 判据"断线恢复 ≤3.5s"没有支撑。故 M1 必须：

| 项 | 做法 |
|---|---|
| 巡检探活 | `transport=quic` 时，隧道域 `patrol_loop` 的探活**改走岛 `Probe{reply}`**（QUIC 连接上的 STREAM[probe] 回显）；节拍/失败计数/阈值沿用既有 patrol 常数（不改时间窗） |
| 不健康分类 | QUIC 连接死 ⇒ 归入**既有取值 `patrol`**（不新增枚举值——`unhealthyReason ∈ {patrol,fd,panic,stop}` 是判据语义，Q-F 批已定「不扩面」）；岛同时把 `Connection::closed()` 事件映射成"探活失败"输入（避免等下一拍） |
| 反向保护 | WG 面死但 QUIC 活时，**不得**因 WG 的巡检失败拆世代（M1 双栈期特有）：判据 = 只以**当前承载**的探活结论驱动 `mark_unhealthy`，另一条腿的失败只记行 |
| `Probe` 的 M3 交接 | M3 巡检定型时把 `Probe` 换成正式的服务流探活（本设计只要求"能判活 + 能归因"） |
| 需求信号（demand-driven） | 生产者由 `wgcore` 实例换成岛（§2.4 的 `TunCounters` 语义保留），消费点（`session/recover` 的 rearm/heal 读 `swap_out_pkts`/`last_outbound`）在 quic 档改读**岛快照的同名字段**（接口不变，来源切换；登记「取值来源变化」） |

### 2.6 生命周期与收工（沿用 M0 设计 §3.5 的写死契约）

- 构造点 = `gen_loop` 内、`Client::start`（`tun_exec.rs:1100`）之后、attach 之前；**同世代内两条传输并存**（岛 + WG 客户端），岛装 `SetOnUnhealthy`。
- 停止点 = 世代收尾链**同址同序**：`pf.stop_all()` → `bridge.stop()` → **岛 / client 各自 `stop_within(now + CLIENT_CLOSE_BUDGET=2s)`** → 缓存终写 → `finish_generation`（实测顺序位 `tun_exec.rs:1420-1456`）。
- 岛内禁裸 task：`JoinSet` + 收工 abort+join（M0 §3.5）；panic 面照 M0 §3.6 六条（就地分类 + `resume_unwind`）。
- **A/B 开关**：见 §4（世代级定型）。
- **M0 遗留残余承接**：`stop_within` 到点 detach 后，老世代岛线程仍持 runtime + quinn `Endpoint`（UDP fd + 缓冲）⇒ 可能继续对出口发包。**M1 判据面加一条**「detach 后老世代 UDP 源端口/连接数可观测」（M0 设计 §8.1 已登记，M1 落成计数行 + 测试）。

---

## 3. 判据行变更与登记条目草案（**本程序第一次真改判据行**）

### 3.1 政策与边界（先定口径，再列条目）

- 政策真源 = `docs/INTEROP-CRITERIA.md`「判据变更记录」节（2026-10-07 Q-A 批起）：**行文变更必须登记**（日期/条目/从→到/原因/影响面），与代码同批 commit；未登记 = 静默破坏对齐。
- **本设计的 M1 变更策略 = 「新增为主 + 取值来源变化 + 极少数行文改写」**，而不是「C/E 全表重写」。理由（**与路线文件「判据与登记预算」表的关系要说清**）：
  - 该表把 C 系列标为 **M1/M3**、E 系列标为 **M1/M5**——即两族都是**跨期**收敛。M1 这一期 WG 路径**仍然活着且仍在打这些行**（服务面自带连接走 WG：`facade/tun_exec.rs:229/238` 的 `session_connect_target`；A/B 开关回退档也走 WG）。**在这一期把 WG 族行改写/删除会与"服务会话仍旧走 WG"的事实冲突**（同一行在服务域与隧道域含义不同 = 新的对齐隐患）。
  - 因此 M1 的登记面 = ①**新增** QUIC 档一族行（`quic:` 前缀，additive，不影响既有读者）；②**取值/输入集变化**（行文不变，登记到「计数输入集 / 数值语义变化」节）；③**必要行文改写**（只有确与 WG 强绑定、且在 QUIC 档语义不成立的行，见 §3.3 的 C2/C8/C15）。
  - **M3/M5 承接**：WG 族行的**退役/重写**落在服务流迁移（M3）与 WG 删除（M5）——那时才能一次写清"新栈"的唯一形态。本设计在 §3.5 给出**交接清单**（哪几行留给 M3/M5，以及理由）。

### 3.2 分类总表（改 / 增 / 保留）

| 类 | 条目 | 去向 |
|---|---|---|
| **新增（additive）** | `quic: 隧道侧就绪…`（C2 的 QUIC 档）/ `quic: 赛跑投出…`（C4）/ `quic: 赛跑结算…`（C5）/ `quic: 路径确立…`（C6）/ `quic: 注册刷新…`（C15）/ `quic: 迁移完成…` / `quic: 丢弃 …`（四类计数）/ `transport: 本世代 L3 承载 = …`（A/B 开关）/ 出口侧 `quic: …` 族（端点就绪 / 连接采纳 / 迁移 / 丢弃） | §3.3 / §3.4 |
| **行文改写（仅 3 处）** | C2（`wgcore:` 前缀 → QUIC 档新前缀，**WG 档原串保留**）/ C8（`判据=%s` 值域加 `quic`）/ C15（RREG → QUIC 档注册刷新） | §3.3 |
| **取值/输入集变化（行文不变）** | C3（端点计数含 QUIC 类）/ C10（via/ep/rtt 来源改岛快照）/ C11（阶梯触发集变少）/ C13（候选条数含 QUIC 类）/ E23（新源行的输入集：QUIC 迁移不再产生 WG 新源行） | 「计数输入集」节 |
| **保留原样** | C1/C4/C5/C6/C7/C9/C12/C16/C17；E1–E4/E6–E9/E13–E20/E22/E24；X1；DC/CA 全族 | 本节说明理由 |
| **留给 M3/M5** | C2/C4/C5/C6/C11/C13/C15 的 **WG 档形态退役**（服务面不再走 WG 后）；E5/E10–E12/E14/E17/E21/E23 的「措辞去 WG」（WG device 删除后） | §3.5 交接清单 |

### 3.3 C 系列（客户端）逐条「从 → 到」

| # | 现状行（`INTEROP-CRITERIA.md`） | M1 动作 | M1 后的形态 |
|---|---|---|---|
| C1 | `身份：新建（…）` / `身份：复用（…）` | **保留** | 身份体系 M1 不动（RPK 是 M2） |
| C2 | `wgcore: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 B 拨隧道 IP）` | **新增 QUIC 档**（WG 档原串保留给 A/B） | 新增：`quic: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 WG 拨隧道 IP）`——末句改「经 B」→「经 WG」：M1 的核自连仍在 WG/栈 B 上（M3 才退役栈 B） |
| C3 | `新栈会话已建立（token 端点 %d 个，后端隧道地址 %v）` | **行文不变 / 取值来源变** | 端点计数含 QUIC 类端点（登记「计数输入集」） |
| C4 | `MIRROR 镜像包#%d → %d 候选（直连优先：本次直连 %d / 中继 %d；本行每轮限 3 条）` | **QUIC 档新增**（WG 档**保留**：服务会话仍走 `wtransport::Bind`） | 新增：`quic: 赛跑投出 %d 个候选（直连 %d / 中继 %d；本行每轮限 3 条）`（节流同源：每轮 ≤3 行、两行间隔 ≥1s —— 照 `wtransport/bind.rs:73-76` 常数） |
| C5 | `赛跑结算：胜出 %s %v（镜像 %d 包，耗时 %v）；响应过=%v；未响应=%v` | **QUIC 档新增** | 新增：`quic: 赛跑结算：胜出 %s %v（候选 %d 个，耗时 %v）；完成=<端点清单>；未完成=<端点清单>`——**保留端点清单**（不是布尔/计数：排障要看得见"哪个候选没起来"，r12 专2-3 订正）；「镜像 N 包」在 QUIC 无对应物 ⇒ 换成「候选 N 个」 |
| C6 | `路径确立：%s %v（首个回包来源）` | **QUIC 档新增** | 新增：`quic: 路径确立：%s %v（首个完成握手）`（语义：确立当前路径来源；判据：首个完成 QUIC 握手的候选） |
| C11 | `RECOVER R1/R2/R3 …`（恢复阶梯族） | **保留（行文/档位/时间窗零改动）**；登记「触发集变少」 | M1 的换网由 QUIC 迁移**在协议内吸收**（不再产生"换源/R2"），阶梯只在真断连时走；阶梯重写 = M3 |
| C13 | `候选端点（%d 条，标记·学习=…）：%s` | **行文不变 / 条件变** | 端点含 QUIC 类（登记）；`·学习` 判定沿用（Q-H 输入集修正不受影响） |
| C14 | `出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v` | **保留** | 探针目标/口径不变（Q-J 取值来源规则不变） |
| C15 | `RREG 注册刷新 → %v（dev=%s，中继=%v）`（60s 巡检刷新） | **QUIC 档新增**（WG 档保留） | 新增：`quic: 注册刷新 → %v（dev=%s，中继=%v）`（QUIC 控制流上的登记刷新；节拍沿用 60s 巡检） |
| C8 | `warmup pong: 就绪（判据=%s）` | **值域扩展** | 值域 `{wg, quic}`：QUIC 档打 `warmup pong: 就绪（判据=quic）`（依据位 = `tun_exec.rs:1189`，今日恒 `wg`） |
| C10 | `link: via=%s ep=%s rtt=%dms（新栈状态快照）` / `（服务会话巡检）` | **行文不变 / 取值来源变** | 快照形态的 `via/ep/rtt` 改由岛快照供给（`via ∈ {direct,relay,none}` 语义保留；`rtt` = `PathStats::rtt`，实测源 `quinn-proto/src/connection/stats.rs:139`）；巡检形态不变（服务会话仍 WG） |

**新增（QUIC 档，本设计新定义，全部 additive）**：

| # | 行 | 触发 / 判读 |
|---|---|---|
| N-a | `quic: 端点就绪（本地 %v，MTU %d，max_datagram_size=%d）` | 岛起端点后一行（量级同 C2；`max_datagram_size` 取 `Connection::max_datagram_size()`） |
| N-b | `quic: 迁移完成（%v → %v，耗时 %v）` | 客户端 `rebind` 成功后一行（**真机判据「WiFi→蜂窝连接保持」的客户端侧证据行**） |
| N-c | `quic: 丢弃 超限=%d 发送缓冲满=%d 回程队列满=%d 未登记=%d（明细分行：首次 + 每 100 次）` | 四类丢弃（§2.4）；**「丢弃可观测、不静默」判据的落点**；节流口径照仓内既有「首 3 + 每 100」族（`relay/mod.rs` `reject_log_due` 同款） |
| N-d | `transport: 本世代 L3 承载 = %s（A/B 开关：env HOMEWAY_TRANSPORT / tunConfig.transport；回退 = wg）` | 世代装配时一行（观测面一致性的锚，§4） |

### 3.4 E 系列（出口）逐条「从 → 到」

| # | 现状行 | M1 动作 | 说明 |
|---|---|---|---|
| E1/E2/E3 | `serve 就绪…` / 凭证 / `客户端 token（…%d 个端点）` | **行文保留；E3 取值来源变** | 端点表新增 QUIC 类 ⇒ 条数与后缀变（登记「计数输入集」）；`serve 就绪` 行的 `wg=:%d` 字段**保留**（WG 端口仍在监听） |
| E4 | `dns 代答就绪：…` | **保留** | DNS face 零改动（承载跟随：App 的 DNS 查询 src=`tun_ip` ⇒ 走 QUIC 回程，§1.5） |
| E5 | `intercept: 过境拦截就绪（…）` | **保留** | intercept 零改动（M1 的接缝在 `device.rs` 的明文面，不在 intercept） |
| E6–E9 | `peer 表…` / `peer: +/~/-` | **保留** | `table.rs` 语义 M1 不动（登记面在 M2） |
| E10–E12 | `intercept: tcp …` / `udp intercept: 会话 …` | **保留** | 同上；输入集变化（QUIC 档的 transit/DNS 流量现在经 DATAGRAM 进来）登记到「计数输入集」 |
| E13–E20/E22/E24 | speedtest / files / term / 凭证 / 身份 / 公网端点 / dns 计数 / 统一进程 | **保留** | 与承载无关 |
| E21 | `绑卡：自动挑到 %s（%s）` / 绑卡看护族 | **保留 + 新增差异登记** | WG socket 绑卡逻辑不变；**QUIC 端口 M1 不绑卡**（理由：绑卡动机是 STUN 观测防污染；QUIC 端点的公网可达性走自己的 UPnP/STUN 面，见下「E-quic」）；M6 视真机再定 |
| E23 | `入站新源：%v（%s，%d 字节）` | **行文保留；输入集变（需一处实现配合）** | WG 设备面的新源行不变；出口侧 `bind.rs` 的**每个 kind 分支都会调 `note_new_src`**（含兜底臂）⇒ M1 必须让 **kind=5 分支显式跳过**，QUIC 档的"换源"由 E-q2 承接（设计门 r12 专1-5） |

**新增（出口侧，additive）**：

| # | 行 | 触发 / 判读 |
|---|---|---|
| E-q1 | `quic: 端点就绪（%v，migration=%v，initial_mtu=%d，datagram 缓冲 %dB）` | 出口 QUIC 端点起后一行（形态同 E5） |
| E-q2 | `quic: 连接采纳 dev=%s tun=%v ← %v` / `quic: 路径变更 dev=%s %v → %v` | 登记的连接（`table.register` 成功后一行）/ 服务端 `remote_address()` 变化（**实测位：探针 P7 的 `REMOTE-CHANGED`**）——这是「出口设备表不新增条目」的出口侧证据行 |
| E-q3 | `quic: 丢弃 超限=%d 发送缓冲满=%d 未登记=%d 源校验拒=%d` | 出口侧四类（源校验拒 = 复刻 `device.rs:314` 的拒绝计数） |
| E-q4 | `UPnP：QUIC 端口 %d 映射 %s` | QUIC 公网端点的 UPnP 映射结果（成功/失败**都必须打**，失败时 token 只公布 LAN QUIC 端点 + 中继 = fail-visible） |

**范围登记（照 M0 §10 的先例，标「路线文件 M1 范围的必要细化，非范围扩张」）**：
①`serve.quic_listen`（路线文件只写"quint server 端点（单 UDP 端口）"，未写端口从哪来、与 WG 端口什么关系）；
②新帧类型 `kind=5` + 自定义 `AsyncUdpSocket`（路线文件只写"中继零改动 + 数据面复测透明转发"——**透明的代价**是在出口侧加一层包装）；
③**QUIC 公网端点的 UPnP 映射**（路线文件未写公网可达性；不做 ⇒ QUIC 只剩 LAN/中继两条候选）；
④`tunConfig.transport` 键 + `HOMEWAY_TRANSPORT` env（路线文件写"A/B 开关（env/config）"，未定名）。

### 3.5 交接清单（留给 M3/M5，防静默漏做）

| 项 | 留给 | 理由 |
|---|---|---|
| C2/C4/C5/C6/C15 的 **WG 档形态退役** | M3（服务面换 STREAM、栈 B 退役）+ M5（WG 删除） | 两条形态同时存在的期里，两族行各有真实现场；退役必须与 `wtransport` 删除同批 |
| C11 阶梯重写 | M3（`session/recover` 重写） | M1 只登记「触发集变少」，不改档位/时间窗 |
| E5/E10–E12/E21/E23 的「措辞去 WG」 | M5 | 措辞绑的是 WG device 的语义（腿表/绑卡/新源）；device.rs 删除时一并改 |
| C8 的 `判据=wg` 常量 | M3（巡检/probe 定型） | M1 只是**加一个值**（`quic`），不改既有值 |

### 3.6 登记条目草案（照 `INTEROP-CRITERIA.md` 登记表字段，**收口时逐条粘贴**）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-XX（M1 落地） | **C2**（隧道侧就绪行） | `wgcore: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 B 拨隧道 IP）` → **新增同义行** `quic: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 WG 拨隧道 IP）`（**WG 档原串保留**，A/B=wg 时仍打） | M1 的 L3 承载由 QUIC 岛承担；前缀 `wgcore:` 在 QUIC 档不再成立（该行打不出来），故新增前缀行；末句「经 B」在 M1 仍是事实但 B 即 WG 面（M3 退役栈 B 后再改） | `docs/INTEROP-CRITERIA.md` C2 行、`facade/tun_exec.rs`、`crates/homeway-quic`、tier（App 核日志消费方）；按「同串或登记」验收：M1 起 **两串都合法**（取决于 `transport`） |
| 2026-10-XX | **C8 值域** | `warmup pong: 就绪（判据=wg）` → 值域 `{wg, quic}`：QUIC 档 = `warmup pong: 就绪（判据=quic）` | 暖机判据位在 QUIC 档由 `Probe`（QUIC 探活）满足；`判据=wg` 在 QUIC 档是假话 | C8 行、`facade/tun_exec.rs:1189`、`crates/homeway-cli/src/main.rs:449/704`（CLI 形态仍打 `wg`——**CLI 不走 QUIC 档**，本批不动）；任何按 `判据=wg` 写断言的脚本须改按"取值 ∈ {wg,quic}" |
| 2026-10-XX | **C15**（注册刷新行） | `RREG 注册刷新 → %v（dev=%s，中继=%v）` → **新增** `quic: 注册刷新 → %v（dev=%s，中继=%v）`（WG 档原串保留） | QUIC 档的登记面 = QUIC 控制流上的 reg 刷新（无 WG 的 RREG 语义） | C15 行、`wtransport/bind.rs:726`、岛侧控制流实现、脚本/测试 |
| 2026-10-XX | **新增 C 族行（N-a…N-d）** | 无 → 有（四条，见 §3.3 表） | QUIC 档需要自己的就绪/迁移/丢弃/A-B 观测面（「丢弃可观测」与「迁移保持」两个 M1 判据的落点） | 新行读者 = 排障脚本 / `docs/QUIC-ROADMAP.md` M1 判据证据；不影响既有读者 |
| 2026-10-XX | **新增 E 族行（E-q1…E-q4）** | 无 → 有（四条，见 §3.4 表） | 同上（出口侧）；其中 E-q4 = QUIC 公网端口 UPnP 结果，**失败必须可见** | 出口排障读者（新行不进状态 JSON 的既有字段） |
| 2026-10-XX | **E3 取值来源**（token 端点表） | 端点表形态不变 → **新增一类端点**（`<host>:<quic_port>`，后缀 `（QUIC 内网/公网/中继）` 一种或多种） | QUIC 端口与 WG 端口不同（§1.1），客户端必须从 token 学到它 | E3 行、token 铸造/解析（`token.rs`）、tier App（**App 侧不改**——端点字符串由核消费）、`tools/local-*.sh` 的 token 断言 |
| 2026-10-XX | **C3/C13 取值来源** | 端点计数 → 含 QUIC 类端点 | 同上 | C3/C13 行、`session/mod.rs`、`facade/tun_exec.rs`（候选清单打印） |
| 2026-10-XX | **C10 取值来源**（快照形态） | `via/ep/rtt` 由 `wgcore::Snapshot` → **QUIC 档由岛 `IslandSnapshot`** | 承载替换；`via ∈ {direct,relay,none}` 与 `ep/rtt` 语义保留 | C10 行、`facade/tun_exec.rs:1746`、`facade/tun_status.rs`（`link{via,ep,rttMs,at}` 结构不变） |
| 2026-10-XX | **C11 输入集** | 阶梯触发集含"换源（R2）"类 → QUIC 档由协议迁移吸收（阶梯只在真断连时走） | QUIC 迁移不换本地 socket 语义（`rebind` 保连接），故不再产生 R2 类触发 | C11 行（行文/档位/时间窗零改动）、`session/recover.rs`（M1 不动）、真机恢复实录的解释口径 |
| 2026-10-XX | **E23 输入集** | WG 设备面新源行 → QUIC 档的路径变更由 **E-q2** 承接（不产生 E23） | QUIC 迁移不是"设备面新源" | E23 行（行文不变）、出口排障脚本 |
| 2026-10-XX | **准入绑定口径**（非编号判据行，**安全面登记**；r12 的 B1/B2 处置） | 「token reg2 MAC（`"H2"`，`hr-reg2`）+ **WG 握手绑定 pubkey**」→ M1：**`hr-reg3` MAC 混入 TLS exporter 连接绑定**（`mac = HMAC(secret,"hr-reg3"‖pubkey‖devTag‖ts‖export_keying_material)`）走 QUIC 控制流；**服务端身份** = Ed25519 RPK（公钥进 token + 客户端钉定，**M2 交付物的服务端半边提前到 M1**）；客户端证明半边（Hello/Challenge/Proof/抗放大）仍留 M2 | ①去掉 WG 握手后仅凭 MAC 的 reg 帧在 ±90s 内可被**重放**成一条可用隧道连接（今日重放只能污染设备表）⇒ 必须给连接绑定；②客户端必须能验证服务端（M0 隔离门禁产品面 `dangerous()`/SkipVerify）⇒ 服务端身份不可推迟 | `wtransport/reg.rs`（reg3）、token 铸造/解析（`token.rs`）、出口 TLS 配置、岛侧登记路径、**威胁模型（M2 必含条目）**；tier 侧无需改代码（端点/公钥由核消费）；版本混用（reg2/reg3）在 M2 前台并存或按用户拍板①（无兼容包袱）直接切 reg3 |
| 2026-10-XX | **token 端点表新增 QUIC 类 + 服务端 RPK 字段**（additive） | 端点表只有 WG/relay/域名类 → 新增 QUIC 类端点（`<host>:<quic_port>`）与出口 RPK 公钥字段 | M1 独立 QUIC 端口（§1.1）+ 客户端钉定服务端身份（§1.3）都需要 token 携带 | `token.rs`、`serve` 侧 token 铸造与 `serve token` 渲染、`docs/INTEROP-CRITERIA.md` E3 行、tier App（只透传字符串） |

**登记条数的口径**：上表 **13 条**（新增行合并成 2 条：C 族四条、E 族四条；逐行明细见 §3.3/§3.4 的两张表；另含 r12 处置后新增的「token 端点表 + RPK 字段」条）。收口时按本表逐条粘贴进 `INTEROP-CRITERIA.md`「判据变更记录」，行文措辞可在实现期微调（**但"从→到"的语义必须逐字对上**）。

### 3.7 与 tier `connection-lifecycle.md` 的边界（**M1 只动哪一部分**）

- tier 文档是**连接行为常量的只读真源**（恢复阶梯档位/节拍/时间窗）。M1：
  - **不动**任何档位、节拍、时间窗（`session/recover.rs` 878 行零改动 → C11 行原样）；
  - **只新增一个动作**：「网络路径变化 ⇒ `rebind` 优先于阶梯」（§2.3）。该动作**不产生新的判据行**（迁移成功打 N-b；迁移失败回落既有阶梯，行文不变）；
  - **不改 L3 语义**：TUN 直通、`src_allowed` 源校验、demand-driven 需求信号三件全部保留（行为等价面）。
- **M1 需要报 tier 的**：`transport` 开关的 App 可见形态（`tunConfig.transport`，缺省 `quic`）+ 端点表新增 QUIC 类（App 只透传字符串，无需改代码）——按用户触点清单属于「tier 侧动作」的**知会项**（不阻塞 M1 收口）。
- **M3 才是改写期**：服务流迁移 + 栈 B 退役时，`connection-lifecycle.md` 的「恢复阶梯」节必须重写（路线文件「判据与登记预算」表的 tier 文档行 = M3/M7）。

---

## 4. A/B 开关与观测面

### 4.1 开关形态（**env + config 双面，世代级生效**）

| 面 | 名字 / 取值 | 优先级 | 粒度 | 说明 |
|---|---|---|---|---|
| env（排障/CI/本地） | `HOMEWAY_TRANSPORT` = `quic`（默认）\| `wg` | **高** | 进程启动后首次读取（`envflag` 的 `OnceLock` 缓存语义，`envflag.rs:1-10`） | 非法值 ⇒ **记行 + 按默认（quic）走**（fail-visible 不 fail-fast：这是排障开关，不该把隧道打进死路）；同时接受 `1`/`0` 两形（`1`=quic） |
| config（App 可切） | `tunConfig.transport` = `"quic"（缺省）\|"wg"` | 低 | **世代级**（每次世代装配读一次；改后下次重建生效） | 缺键 = `quic`（**默认新路径**，路线文件口径）；JSON 未知键既有忽略语义 ⇒ 旧 App 不传 = 新路径，新 App 可一键回退 |

- **为什么两套**：env 是「进程级一键回退」（本地/CI/排障；App 内嵌 `.so` 形态下由 App 在启动前设置）；`tunConfig` 是「世代级、App 可控」（无需重启进程），也是 M7 生产切换期要用的旋钮。
- **开关粒度写死**：开关**只决定 L3 承载**（TUN 数据面走岛还是走 WG）。**服务面（files/term/speedtest/portfwd dial）与核自连在 M1 恒走 WG**（§1.5）——开关不改变它们，避免"半切"状态（回退档也必须功能全保）。

### 4.2 回退档（`transport=wg`）的观测面一致性

| 面 | 行为 |
|---|---|
| 岛 | **不构造**（QUIC 面零成本：无 UDP fd、无 runtime）；`quic:` 族行不打 |
| 判据行 | C2/C4/C5/C6/C8/C10/C15 **全部回落今日原串**（`wgcore:` 前缀、`判据=wg`）——这正是 §3.6 登记条目里「两串都合法」的取值面 |
| 状态 JSON | `link{via,ep,rttMs,at}` 结构不变；来源回落 `wgcore::Snapshot` |
| 出口 | QUIC 端口是否监听 = 出口配置 `serve.quic`（缺省 true）；回退档配套建议 `false`（本地/CI）：**生产常开**，否则 App 切到 quic 档时无直连端点可用 |
| 反向（默认档） | `transport=quic` 时 WG 面**照旧在跑**（服务面 + A/B 对照）⇒ 回退 = **重建世代即换档**（无动态迁移两套生命周期） |

### 4.3 观测面（`link:` / 状态 JSON）

- `link:` 行：快照形态的 `via/ep/rtt` 由岛快照供给（`via ∈ {direct,relay,none}` 语义保留；`relay` 判定 = 胜出候选属「QUIC 中继端点」类）。巡检形态（服务会话）不动。
- 状态 JSON：`link` 段结构与键名不变；新增**平级**段 `quic{mtu,maxDatagramSize,lostPkts,congEvents,migrations,drops:{…}}`（additive；tier 只校验既有键存在 ⇒ 不破坏 App 解析）。
- 判读入口：`quic: 丢弃 …`（N-c 行）与状态 JSON 的 `drops` **同源**（同一计数器两处渲染）⇒「丢弃可观测」既可 grep 又可程序读。

---

## 5. MTU 与降级（**拍板结果：候选 A**，2026-10-08 —— 见 §12-①**）

### 5.1 事实底座（全部实测，§0.3 P2/P3）

| 项 | 值 | 后果 |
|---|---|---|
| `max_datagram_size()`（MTU 1400） | **1362 B**（= MTU − 38B 1RTT 开销） | 装得下 1280B 内层包（余 82B） |
| **内层可用下界** | `max_datagram_size() ≥ 1280` ⇒ **MTU ≥ 1318**（直连与中继同值——信封加在**外层**，不减内层容量） | **不变量**：`内层MTU ≤ mds`。低于 1318 时 1280 内层包**每一个都被丢**，端到端 TCP 重传同样的满段 ⇒ 用户感知是**断**不是"卡"（订正 v1 的"窗口收敛"说法） |
| MTU 1200（**v1 取值，已废弃**） | mds 1162 | **1280 内层包全丢** ⇒ 故 `min_mtu` 取 1320 而非协议地板 1200（§1.2） |
| 内层 MTU | **1280**（路线文件口径；随 `TunAttach{fd,mtu}` 进来，同 `wgcore/mod.rs:1471`） | 内层包典型尺寸 ≤1280 |
| 中继信封余量 | 上行 **+11B**（`[0xAA][id8]`+`[0xBB][kind]`）、下行 **+2B** | QUIC MTU 上限取 1400 ⇒ 上行线上 1411B UDP 载荷；**IPv4/PPPoE**：1439 ≤ 1492（余 53B）；**IPv6 1500**：1400+11+48 = **1459 ≤ 1500（余 41B）**；若取默认上界 1452 ⇒ IPv6 1511 **超 11B**（v1 把 41B 误写成 PPPoE 余量，已订正） |
| DPLPMTUD | 本设计 = `upper_bound(1400)` = `initial_mtu` ⇒ **上探构造性关闭**；**黑障检测仍活**（恒建 `BlackHoleDetector`，`mtud.rs:55`） | 黑障命中 ⇒ `current_mtu` **一跳落到 `min_mtu`**（`mtud.rs:160`，无逐级下探）⇒ 1320（mds 1282，仍可用）；**代价**：停在 1320 不回升（≈4.3% 线开销） |
| 超限错误 | `SendDatagramError::TooLarge`（`quinn/src/connection.rs:1324`），**同步返回、可分类** | ⇒ 丢弃可观测（不需侧信道） |
| 第 5 条静默通道 | `drop_oversized`（MTU 变小时清掉**已排队**超限包，`datagrams.rs:176-198`；调用点 `connection/mod.rs:1429/1799/3089`） | 岛内每拍比对 `max_datagram_size()` 变化 → 自有队列超限包计 `超限`（§6.4）；**迁移期恰是 MTU 变化期** ⇒ 必须做（设计门 r12 专3-5） |

### 5.2 降级候选与**拍板结果**（已拍板 2026-10-08：**候选 A**）

> **拍板结果（用户 2026-10-08，见 §12-①）**：**候选 A = 推行**（客户端本地检包丢 + 计数 + 计数行 + 显式上限旋钮
> `HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap`，默认 1400、有效区间 [1320,1400]；状态 JSON 暴露
> `maxDatagramSize`/`currentMtu`；`mds < 内层MTU` 时另打「窄路径不可用」行）；
> **候选 B 保留为 M6 真机后的候选**（自动档位 + App 重建接口改 MTU，需 tier 触点）；
> **候选 C 不做**（出口 ICMP 反馈与 Q-B 批已登记口径「非 TCP/UDP 不建会话、不产出 ICMP 不可达」直接冲突）。
> 下表三行**保留为决策依据**（含代价），实现棒按 A 落地、不得自行改档。

| 候选 | 形态 | 代价 | 优点 |
|---|---|---|---|
| **A（推荐）**：客户端本地降级 + 显式上限旋钮 | ①内层 1280 保留；②每包按 `max_datagram_size()` 检、超限丢 + 计数 + 节流记行；③显式上限旋钮（env `HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap`，默认 1400，**有效区间 [1320,1400]**——低于 1320 时 1280 内层包必丢，故下限设 1320，与 `min_mtu` 同值）；④状态 JSON 暴露 `maxDatagramSize/currentMtu`；⑤`mds < 内层MTU` 时除计数外**再打一条"窄路径不可用"行** | 窄路径（`mds < 1280`）下内层满段**每一个都被丢**，端到端 TCP 反复重传同尺寸段 ⇒ 用户感知是**断**（订正 v1 的"卡"）；需看状态面才能定位 | 全部落在 quinn 既有面（零新增信令）；行为**可观测**（计数 + 状态位 + 专行）；失败模式已知 |
| **B**：自动档位 + App 反馈 | A 之上，把 `currentMtu` 折成"建议内层 MTU"写进状态 JSON（`suggestedTunMtu`），由 **App 决定是否重建 VPN 接口改 MTU** | 需 tier 动作（重建接口 = 用户可见断流一次）；跨仓协调 | 窄路径下恢复全尺寸吞吐（1280 → ~1100），不是"丢到能用为止" |
| **C**：出口侧 ICMP 反馈 | 出口对超限/黑洞包回 ICMP `frag needed (MTU=x)`，客户端据此降内层 MTU | 工程量最大（新信令 + ICMP 面 + 与 App 的契约）；且与 Q-B 批已登记口径「非 TCP/UDP 协议不建会话、**不产出 ICMP 不可达**」（`INTEROP-CRITERIA.md` Q-B F9 注记）**直接冲突** | 终端侧完全自动 |

**推荐理由（即拍板依据，逐条留档）**：A 落在 quinn 既有面（零协议改动、零 tier 触点），并把"窄路径可用性"表达为**可观测丢弃计数**+专行，而不是静默行为；B 需 tier 触点（改 VpnService MTU = 重建接口 = 用户可见断流一次），**列为 M6 真机后的候选**；C 工程量最大且与既有 ICMP 口径冲突（先改口径才谈实现）。**用户 2026-10-08 全部按推荐项拍板** ⇒ 本节的"推荐/候选"措辞自此为**记录性**，不再是开放选项。

### 5.3 窄路径"不静默"的落地判据

- `TooLarge` ⇒ `quic: 丢弃 超限=…`（N-c）+ 状态 JSON `drops.tooLarge`；**首次**与**每 100 次**各打一行明细（含 `max_datagram_size` 与包长）⇒ 排障一眼看出"是 MTU 不够"。
- 取证：本地可复现（QUIC MTU 压到 1200 + 投 1280 内层包；P3 已证 `TooLarge`）。

---

## 6. 背压与丢包语义（附录 D 风险 5 专项）

### 6.1 出站（TUN → DATAGRAM）与回程（intercept → DATAGRAM）

| 面 | 今日 WG | M1 QUIC | 同档证据 / 差异 |
|---|---|---|---|
| 出站队列满 | 内核 `SO_SNDBUF`（4MB，`bind.rs:239`）满 ⇒ `sendto` ENOBUFS/EWOULDBLOCK ⇒ 丢 + `localErr*` 计数 | `datagram_send_buffer_space()` 预检不足 ⇒ **丢 + 计数**（N-c `发送缓冲满`） | **同档**（丢 + 计数、不阻塞）；差异：WG 的 4MB 是**单 socket 全设备共享**，QUIC 是**每连接**（§6.3 预算） |
| 出站"不丢"的替代 | 无（WG 无重传） | `send_datagram_wait()`（等缓冲、老优先）——**M1 不用**：它把背压变成岛内 await 挂起，而 TUN 读线程是同步面（要么丢要么阻塞同步面）。**纪律：热路径禁用 `send_datagram_wait`** | — |
| ⚠️ 裸 `send_datagram()` | — | 实测：缓冲满时**静默淘汰最旧**且恒返 Ok（P4） | **禁用**（§1.5/§2.4）；代码门专项 grep：数据面只允许经 `send_datagram_checked(...)` 包装 |
| 回程队列满 | WG 回程 = 驱动线程直接写 TUN fd（阻塞写 + 期限）⇒ 无应用队列；内核 rcvbuf 满 = 内核丢新（**应用不可见**） | 岛读 `read_datagram` → **有界队列** → std 写线程；满 ⇒ **丢 + 计数**（N-c `回程队列满`） | **比今日更可观测**；风险：队列过大掩盖慢 TUN 写 ⇒ 上限取「1 拍 × 峰值」量级（实现期由 harness 定） |
| quinn 接收缓冲溢出 | 不适用 | quinn 内部**静默丢最旧**（`quinn-proto/src/connection/datagrams.rs:145-152`）——应用不可见 | **设计规避**：岛内 drain 循环每拍清空 + 自有有界队列 ⇒ 丢弃发生在**我们的**计数面上；接收缓冲只作突发吸收 |
| 连接未就绪 | WG：握手期包在 boringtun 内排队/丢 | QUIC：握手完成前 `max_datagram_size()==None` ⇒ **丢 + 计数**（归 `未登记`） | 差异登记：QUIC 握手期**不能**发 DATAGRAM（需 peer 参数）；对端到端 TCP 无实质影响（握手期本就在建连） |

### 6.2 丢包与端到端 TCP 的相互作用（逐项对照，不许只写"语义保留"）

1. **DATAGRAM 无重传**（协议本身）⇒ 丢一个内层 IP 包 = 端到端 TCP 少收一段。**与今日 WG 完全同档**：WG 的 data 包也无重传，恢复机制同样是端到端 TCP 的重传/拥塞控制。
2. **新增一层拥塞控制**：QUIC 带 CUBIC + 丢包检测（`packet_threshold=3` / `time_threshold=9/8`，`transport.rs:381-382`）；**WG 侧没有 in-protocol CC**（boringtun 不控速率，只有内核 socket 缓冲 + 端到端 TCP）。⇒ **"双拥塞控制"叠加**：蜂窝丢包时 QUIC 降 cwnd，端到端 TCP 同时降窗 ⇒ 最坏两次降窗。**这是 M1 相对今日唯一实质性的丢包语义变化**，必须以实测（`congestion_events`/`lost_packets` + 吞吐 A/B）判定是否造成真机回退（附录 D 风险 3/5 交叉点）。
3. **观测面**：`PathStats{lost_packets,lost_bytes,congestion_events,black_holes_detected,current_mtu}`（`quinn-proto/src/connection/stats.rs:139-160`）进状态 JSON；`lost_plpmtud_probes` 单列（探测丢失不算拥塞信号，`stats.rs:155`）。
4. **迁移时拥塞重置**：实测 `cwnd` 回初始值（P7）⇒ 换网后一段慢启动；写入 M6 真机计量口径（迁移后 X 秒恢复）。
5. **立场**：M1 只需保证"**稳定建连期**丢包语义与 WG 同档"（同为"丢 = 端到端 TCP 的事"）；**迁移瞬间与弱网**下 QUIC 多一层 CC ⇒ 归 M6 终验（劣化则按 PERF-AB §9 归因：先怀疑 CC 叠加，再怀疑 DPLPMTUD 抖动）。

### 6.3 每连接缓冲预算（**门槛冲突的正面处理**）

- 事实：今日出口 4MB×2 是**单 socket 全设备共享**；quinn 的 `datagram_send/receive_buffer_size` 是**每连接**。照 4MB 直译 ⇒ 32 设备最坏 ≈ 256MB（不可接受）。
- 裁决：**每连接 1 MiB 收发各一**（= quinn 默认量级，`transport.rs:396`；**§1.2 表已同步为 1MiB**，v1 的"4MiB"是遗留矛盾——设计门 r12 的 B4），且 quinn 缓冲是 `VecDeque` 按需增长不预分配 ⇒ 稳态不占。M1 判据替换为两条可测项：
  ①稳态每连接边际 **≤ 96K**（= 本 harness 三点口径实测值；五点口径 81.6K 只作对照，§9.1-1）；
  ②**负载态出口 footprint 增量 ≤ 64 MiB + 自有队列**（32 连接 × 收发各 1 MiB = 64 MiB 上限，非 32MiB——v1 低估 2×，设计门 r12 专3-4）+ 自有队列（§6.4 的绝对上限）。
- 与路线文件 M1 范围「datagram 缓冲上限对齐今日 `SO_SNDBUF/SO_RCVBUF=4MB` 量级」**有意偏离**（4MB 是"单 socket 全设备"量级，逐连接直译会爆预算）⇒ 登记为**范围细化**（§3.4 范围登记邻域），并在 §9.1 的门槛裁决里一并给出（该裁决**已获用户 2026-10-08 拍板**，见 §12-③）。

### 6.4 错误分类与计数行（「丢弃可观测、不静默」的逐项落地）

**两侧队列的绝对上限与归类矩阵（设计门 r12 专5-4/专5-5 要求，防实现棒各自命名）**：

| 进程 × 队列/错误 | 上限（绝对） | 计数字段 | 备注 |
|---|---|---|---|
| 客户端 · TUN 读线程 → 岛的 `Cmd::TunPacket` | **有界通道：4096 条**（≈5MB @1280B；M0 遗留 R-H 的落点） | `未登记`（并入岛侧"投递不进"面） | **v1 的 unbounded 是缺陷**：M0 `cmd.rs:26-30` 明说"队列上限与丢弃计数是 M1 项"，本设计必须落；满 ⇒ **丢新 + 计数**（TUN 读线程不阻塞） |
| 客户端 · 岛发送缓冲（quinn） | 1 MiB（§1.2） | `发送缓冲满` | 预检 `datagram_send_buffer_space()`；**单线程岛**是该预检的前提（两次加锁读，`connection.rs:449/509-511`）⇒ 写进纪律 |
| 客户端 · 岛回程队列（`read_datagram` → TUN 写线程） | **2048 条**（≈2.6MB） | `回程队列满` | 满 ⇒ 丢新 + 计数（TCP 会重传） |
| 客户端 · 超限 | —（`mds` 决定） | `超限` | 含 `ToooLarge` **与** `mds` 变小后自有队列的超限包 |
| 出口 · QUIC 面 → 引擎入站队列 | **8192 条**（多设备共享；≈11MB） | `未登记`（出口侧名 = 入境队列满） | 引擎 poll 唤醒后 drain；满 ⇒ 丢 + 计数 |
| 出口 · 引擎 → QUIC 面出站队列 | **8192 条** | `发送缓冲满`（出口侧） | 满 ⇒ 丢 + 计数（**不许**退化成阻塞引擎 poll） |
| 出口 · 源校验拒 | — | `源校验拒` | 复刻 `device.rs:314` |
| quinn 内部接收缓冲淘汰 / `drop_oversized` | —（不可见） | **不计数**（`drop_oversized` 部分由自有队列的 mds 变化检查补记） | 明确登记为**已知不可观测面** |

| 错误/事件 | 来源（实测/源码） | 归字段 |
|---|---|---|
| 内层包 > `max_datagram_size()` | `SendDatagramError::TooLarge`（P3）**或**本地发送缓冲单报文上限（`datagrams.rs:28-41`） | `超限` |
| 发送缓冲不足（预检不过） | `datagram_send_buffer_space()`（§1.5） | `发送缓冲满` |
| 连接未就绪/未登记/已断 | `UnsupportedByPeer`（握手未完成或对端不支持，**不是** `mds==None`）/ 无绑定 / `ConnectionLost` / `Disabled` | `未登记`（**按变体映射**，0-RTT 恢复会话可能提前有 `mds`，`datagrams.rs:28-41`） |
| 出口侧源校验拒（src ∉ {tunnel_ip,tun_ip}） | 复刻 `device.rs:314` | `源校验拒`（出口侧） |

### 6.5 与 WG 的两层对照（**订正 v1 的层次错位**，设计门 r12 专5-1）

| 层 | 今日 WG | M1 QUIC |
|---|---|---|
| 应用层队列（第一层） | 出口**发送 ring 满 ⇒ 丢新 + `ring_drops` 计数 + 首3/每1000 行**（`server/bind.rs:747-783`，注释"队尾丢 = TCP 尾丢语义"）；客户端无此层 | 自有有界队列（§6.4）⇒ 丢新 + 计数 |
| 内核/协议缓冲（第二层） | 内核 socket `SO_SNDBUF`/`SO_RCVBUF`（4MB 共享）：发送满 ⇒ `sendto` ENOBUFS（**可能静默**）；接收满 ⇒ 内核丢新（应用**不可见**） | quinn `datagram_*_buffer_size`（每连接 1MiB）：发送走**预检**（可见）；接收满 ⇒ 丢最旧（应用不可见）；另有 `drop_oversized`（不可见） |

**结论**：QUIC 档在**发送侧**比 WG 更可观测（预检 + 计数），在**接收侧**与 WG 同档（都不可见）——登记为「同档，且发送侧更优」。

---

## 7. 中继零改动与预算复核（附录 D 风险 4 专项）

### 7.1 零改动的**可复核**判据（不只"我没改"）

| # | 判据 | 方法 |
|---|---|---|
| Z1 | 代码面：本批 diff **不含** `crates/homeway-core/src/relay/**` 与 `relaywire.rs`；`wtransport/frame.rs` 只允许**新增** kind 常量 | `git diff --stat`（证据入 `docs/reviews/M1.md`） |
| Z2 | 行文面：R1–R13 判据行**逐字不变** | 脚本 grep 对照（收口留证） |
| Z3 | 行为面：`tools/local-rust-relay.sh start 1` + 本地出口 + 客户端经中继全链跑通 | 实测日志 |
| Z4 | 依赖面：relay 模块不新增依赖（零 TLS/QUIC） | **订正（r12 专4-7）**：`cargo tree -p homeway-core` 恒含 quinn（M0 已挂）⇒ 不可测。改为 grep 断言：`relay/mod.rs`/`relaywire.rs`/`relay/*` 的 `use` 面**不含** `quinn|tokio|rustls` + relay 单测可单独编译 |

### 7.2 预算与尺寸的**实测复核方案**（本地可跑）

| # | 待复核项 | 方法 | 判据 |
|---|---|---|---|
| B1 | **隧道包尺寸**（含 DPLPMTUD 探测包与 1280+ 数据报） | 客户端 QUIC MTU ≤1400 ⇒ 上行线上 = **1411B** UDP 载荷（IPv4 总长 1439）、下行 = **1402B**；断言方式 = **读出口/中继的字节计数**（或 `tcpdump` 的 `length`），**不是**"lo0 上无分片"（lo0 MTU 16384 ⇒ 该断言恒真，r12 专6.7） | 线上字节与理论值逐字节一致；relay `丢弃` 不增 |
| B2 | **200pps/源闸下的上行** | 单向上行灌 1280B 数据报（客户端 → relay → 出口），读 relay 的 `中继统计：… 转发 上 X / 下 Y 包｜丢弃 Z`（60s 一拍，`relay/mod.rs:635-642`）+ 客户端 `udp_tx_dg` | ①上行 pps 上界 ≈200（`--rate-limit` 缺省；**含出口控制面同桶**——loopback 下出口 RREG/保活/探测与客户端共 `127.0.0.1` 桶，故本地用 `127.0.0.2` 起出口或登记该形态差异，r12 专4-5）；②触闸时**同时观察 QUIC 拥塞事件**（`congestion_events/lost_packets`）——中继静默丢对 QUIC 不可区分于拥塞，会触发 CUBIC 降窗（r12 专5-3） |
| B3 | **经中继下行吞吐：QUIC 档 vs WG 档（同刻 A/B，本地）** | **订正（r12 专4-1/专4-3/专4-4）**：v1 用"19 Mbps 锚"与"比值 ≥1.68"**均错**（19 Mbps 无出处：`INTEROP-CRITERIA.md:188` 的 U2 行只有 23.7MB 体积与 −0.28% 偏差，**在册锚**是 `docs/reviews/R2.md` 的"经中继 speedtest 27/2.7 Mbps"与"上行 200pps ≈2.7Mbps"，且 `PERF-AB.md` 把"speedtest 经中继"列为 KNOWN-GAP）。**改为相对判据**：`local-rust-relay.sh` + 本地出口，同刻交替跑 WG 臂与 QUIC 臂（各 ≥3 轮），记 `下行吞吐比 = quic/wg`、`客户端上行 pps 比`、`congestion_events`。算术口径保留作**解释工具**：上行 200pps × 比值 × 1411B × 8 = 下行上界 ⇒ 默认 ACK 比值 3.97 ⇒ **≈9 Mbps**；`ack_eliciting_threshold=16` ⇒ 比值 ≈18 ⇒ **≈40 Mbps**（比值为实测/推算量，**实现棒必须复测**）。**关键认识（r12 专4-4）**：真实 TCP 下载时客户端上行被**内层 TCP ACK**（每 2 个满段一个）占用，QUIC 的 ACK 只是搭车（P6 已证上行 1.05 包/数据报）⇒ `ack_frequency` 只是**部分杠杆**，真正约束是"上行包预算 ÷ 内层 ACK 密度"——故判据必须是**相对 A/B**，不能是绝对门槛 | 无数量级回退（QUIC 档下行 ≥ WG 档的 0.9×，或差异登记到 M1 报告）；`ack_frequency` 的净收益**实测登记**（默认 vs T=16 两档） |
| B4 | **DPLPMTUD 探测包** | 读 `sent_plpmtud_probes/lost_plpmtud_probes`（起手实测 1–2 个，P2） | 经中继不额外触发；量级 = 个位数包/600s，不打满 200pps |
| B5 | **中继会话/句柄放大** | 每条客户端 QUIC 连接 = 1 条 relay assoc + 1 条出口腿；**迁移换源 ⇒ 新 assoc**（旧的 90s 回收，`relay/mod.rs:44-45`） | 单设备峰值 = 2 条（迁移瞬间）；`MAX_ASSOCS_TOTAL=1024` / `max_per_peer=32` 不变 |
| B6 | **连接保活** | `keep_alive_interval=10s` ⇒ 客户端上行 0.1pps | 占上行预算 0.05% |

**立场（订正 + 已拍板）**：①**中继代码零改动这条红线不变**——任何预算问题都用「客户端/出口侧的 `ack_frequency_config` + 自有队列」解决，**不改中继**；
②**判据 = 同刻 A/B 相对判据**（§7.2 B3；**用户 2026-10-08 拍板**，见 §12-④）：经中继 vs 直连的同刻比值 + 客户端上行 pps 比 + **同时记 `congestion_events`/`lost_packets`**（分辨"限速器静默丢被 QUIC 当拥塞"，§9.3 Q-L）；**绝对数字（200pps、≈9 Mbps/≈40 Mbps 的上界推算值）只作登记并标注"本机回环口径、不可比"**；
③v1 的「默认 ACK 行为已满足今日锚」**已作废**（锚无出处 + 算术错 5×，r12 B5）⇒ `ack_frequency_config`（阈值 16 + `max_ack_delay=5ms`）**按 M1 默认配置落地**，其净收益在 S5-3 用 A/B 实测登记。

---

## 8. 门槛与测量计划

| 维度 | M1 门槛（路线文件） | 本设计的测量法 | 判定期 |
|---|---|---|---|
| 每包 CPU | ≤ 现役 WG+shim ×1.0 | ①`tools/quic-ab.sh cpu --arms raw,wg-shim,wg-ring,quic --rounds 3`（**独占机器**，对照 `loadavg.tsv`）复现 M0 基线（14.623 vs 12.668 µs）；②**新增端到端 A/B**：本地出口 + 岛客户端（合成 IP 流）分别 `HOMEWAY_TRANSPORT=wg\|quic` 跑同一流量 | M1 收口 |
| 线开销 | ≤ 40B/包 | `tools/quic-ab.sh overhead`（M0 = 30.34B ≤ 40B ✓）+ 中继口径另计信封（上行 +11B/下行 +2B，§7.2 B1） | M1 收口 |
| 真机吞吐 | 热态 ≥0.95×；冷/热 ≥0.70 | **真机**（用户触点）同刻 A/B（PERF-AB §1/§9）；本地代理 = 端到端合成流量 A/B（**标注差异**：合成 UDP 流无 TCP 反馈，只能作"无数量级回退"筛子）；**经中继面另按 §7.2 B3 的同刻 A/B 相对判据**（含拥塞事件，不设绝对门槛） | M6 终验 |
| 换网迁移 | 连接保持、设备表不新增条目 | 本地集成测试（rebind 后连接不断 + 出口 `remote_address()` 变化；P7 已证）；真机 WiFi→蜂窝（用户触点） | M1 本地 / M6 真机 |
| 断线恢复 | ≤3.5s | 沿用 R2 批故障注入（出口重启）+ `session/recover` 阶梯（M1 不动） | M1（本地）/ M6 |
| 体积 | ≤3.8MB（M5 判；M1 只登记增量） | `tools/quic-ab.sh size`（product 档）+ `tools/build-app-core.sh` 的 `[size]` 行 | M1 登记 / M5 终值 |
| 内存 | 单连接 ≤+256K；每设备 ≤64K；32 设备 ≤+2MB → **已修订**（见右列） | **已获用户 2026-10-08 拍板**（见 §12-③）：稳态边际 **≤96K**（三点口径实测；五点 81.6K 作对照）/ 单连接 **≤320K** / 32 设备 **≤+3.1 MiB**（稳态）/ 负载态 **≤64 MiB + 自有队列**；路线文件门槛表由**主会话同批同步** | M1 收口（按修订后的口径判） |
| 丢包可观测 | 有计数行、不静默 | 窄路径注入（QUIC MTU 1200 + 1280 内层包；P3 已证 `TooLarge`）⇒ `quic: 丢弃 超限=…` + 状态 JSON | M1 收口 |
| 全局代理等价 | 浏览器/任意 App 经隧道全通 | 本地：合成 IP 流（DNS 查询 / UDP echo / TCP 握手+数据）经岛 → 出口 intercept → 本地目标逐项断言；真机：层 0 对照（用户触点） | M1 本地 / M6 真机 |

**harness 纪律**（M0 已登记，沿用）：并发编译/其他会话会让四臂整体上抬（wg-ring +12.1% 实测）⇒ **跑门槛必独占机器**；读数对照同目录 `loadavg.tsv`。

---

## 9. 风险与未决（含 M0 两遗留的裁决）

### 9.1 裁决 ①：每连接内存边际的**拟合口径与门槛**（M0 遗留）

**事实**（`docs/QUIC-BASELINE.md` §4）：M0 实测三点拟合 **96.0K/连接**（base 1157.3K，N=1,3,5 全点最小二乘）、五点拟合 **81.6K**（base 1208.0K，N=1..5；同源复跑第二轮 76.8K）；附录 A 手抄 37.6K 的原始采样文件**已不留存**（同「只有手抄 SUMMARY」问题），差 ≈2×。

**裁决（两者都改，逐条给理由）**：

1. **拟合口径改五点（N=1..5）+ 三点仅作对照**：三点拟合的 base 截距与斜率对 16K 页粒度台阶极敏感（M0 同一份数据里 96.0K vs 81.6K 的差就来自采样密度），五点把自由度提上去；harness 命令固定为 `tools/quic-ab.sh mem --mode conns --conns-points 1,2,3,4,5`（**该命令 M0 已存在**，只需把默认口径切过去）。
2. **门槛按实测重订**：`每设备 ≤ 64K` → **`每设备 ≤ 96K`（稳态边际；口径 = 三点拟合实测值 96.0K，五点口径实测 81.6K 作对照；**标签订正**：v1 把 96K 写成"五点口径上界"是错的）**。理由：①37.6K 无原始证据链（不能作为门槛依据）；②quinn 的每连接结构（`CryptoBuffer` 16KiB + 流/路径状态 + 定时器堆 + CID 表）在本机实测就是 77–96K；③96K × 32 = **3.0 MiB**，故配套把「32 设备 ≤ +2MB」改为 **≤ +3.1 MiB（3.25 MB）**（单位统一为 MiB，避免 v1 的"3MB vs 3.0MiB"错位）。
3. **`单连接 ≤ +256K` 改为 `≤ +320K`**（措辞订正：v1 同一段里既写"保留"又写"改判"，自相矛盾）。依据：实测单连接 +272K 相对地板 960K（M0 登记）——该格口径是「相对地板的全量（含 runtime/Endpoint 固定成本）」，越界 16K，留 17% 余量后取 320K；M1 收口用产品形态（岛 + 客户端）复测。
4. **新增负载态门槛**：出口 32 连接 + 持续流量 ⇒ footprint 增量 ≤ **64 MiB + 自有队列上限**（= 32 × (1 MiB send + 1 MiB recv)，订正 v1 的 +32MiB 低估 2×；§6.3/§6.4）——这条替代「32 设备 ≤ +2MB」在**负载态**的适用（稳态仍按 +3.1 MiB 判）。

**范围/触点声明（已拍板）**：以上四条是**路线文件「性能/体积/内存门槛」表的数值修改**——**已获用户 2026-10-08 拍板**（见 §12-③）；本棒**不改**该文件，**路线文件门槛表由主会话同批同步**（本设计只留裁决原文与理由）。M1 判据自此按修订后的四条口径验收。

### 9.2 裁决 ②：OHOS 运行期（tokio/mio/epoll）与 `panic="abort"` 跨仓约束（M0 遗留）

| 子项 | M1 能本地验？ | 处置 |
|---|---|---|
| OHOS 编译期（三目标 check + 真链接） | **能**（M0 已建：CC 配方 + `build-app-core.sh` 三道门） | M1 每个实现棒收口必跑（`.so` 含 QUIC 面后体积/符号面复测） |
| OHOS **运行期**（epoll/mio、真机 socket 语义） | **不能**（本机无 OHOS 运行时） | ①**可选代理**：在 Linux（本机 `x86_64-unknown-linux-musl` 或容器）跑同一二进制做「非 darwin 运行期」冒烟（epoll 路径与 OHOS 同族，`target_os="linux"`）——**M1 尽力项**，跑得成即留证；②**必做**：登记为**真机触点**（tier 出包 + App 安装冒烟），M1 不宣称运行期已验证 |
| `panic="abort"` 跨仓约束 | 不适用（跨仓） | 沿用 M0 设计 §3.6-5 的口径，**M7 tier 触点复核**；M1 岛沿用 M0 的 `catch_unwind` 面 ⇒ 无新增暴露；**再转达一次**（M0 已登记，M1 继续挂在 `docs/reviews/M1.md` 的转达项） |

### 9.3 风险表（M1 新增 / 承接）

| # | 风险 | 证据 / 来源 | 处置 |
|---|---|---|---|
| Q-A | **裸 `send_datagram` 静默淘汰**（若实现棒照常见写法用裸调用） | P4 实测（200,001 次全 Ok、buffer 满仍无错误） | 设计层禁用（§1.5/§2.4）+ 代码门 grep 专项 |
| Q-B | **中继 200pps 闸与 ACK 开销**（附录 D 风险 4） | P5 实测比值；§7.2 B3 门槛 | 默认 ACK 行为已满足；`ack_frequency_config` 作余量；真机复测 |
| Q-C | **双拥塞控制叠加**致弱网劣化（附录 D 风险 3/5 交叉） | §6.2-2 论证（QUIC CC + 端到端 TCP CC） | M1 本地 A/B 先筛；M6 真机终验 + 归因 |
| Q-D | **迁移后拥塞重置**（换网瞬间慢启动） | P7（`cwnd` 回 12000） | 登记为已知代价；M6 计量恢复时长；不做"迁移后预置 cwnd"这类未验证优化 |
| Q-E | 出口 QUIC **公网可达性**（新端口需自己的 UPnP/STUN） | §3.4 范围登记③；`upnp.rs` 既有 API（`add_port_mapping` 等） | M1 实现 UPnP 映射 + 失败可见（E-q4）；失败时 token 只公布 LAN QUIC + 中继 |
| Q-F | **出口双栈的内存/句柄放大**（WG 面 + QUIC 面并存） | §6.3 预算裁决 | 每连接 1MiB 上限 + 负载态判据 + M1 实测 |
| Q-G | 自定义 `AsyncUdpSocket` 的正确性（缓存/丢包/顺序） | §1.6（本棒只核到 API 签名） | 实现棒第一步做**最小可跑原型**（直连 + 一条腿）再铺开；代码门专项 |
| Q-H | M0 残余：detach 后老世代仍可能发包 | M0 设计 §8.1 | M1 判据面「detach 后老世代 UDP 源端口/连接数可观测」+ 测试 |
| Q-I | `kind=5` 新帧类型与中继旧版本互操作 | 中继 `decode_tagged` 只校验魔数（`frame.rs:101`），kind 原样透传（`relay/mod.rs:907` 区） | **无兼容包袱**（用户拍板①）：中继与出口同批发版；旧客户端不受影响（仍发 kind=0） |
| Q-J | **M1 的准入绑定弱于 M0 之后的预期**（pubkey 与连接未绑） | §3.6 准入登记条 | **订正**：M1 加 **TLS exporter 连接绑定**（§1.3，`hr-reg3`）⇒ 回放面关闭，但「pubkey 与连接的身份绑定」仍由 M2 的 Hello/Challenge/Proof 补 |
| Q-K | **自定义 `AsyncUdpSocket` 丢多段批处理**（`max_transmit_segments=1`）⇒ 每包 CPU 可能高于 `quic-ab.sh cpu` 的内建 socket 读数 | `quinn/src/runtime.rs:72-80`；§1.6 段能力登记 | S5 的端到端 A/B 必须用**产品路径**；退路 = 直连 socket 走 `quinn-udp` 的 `UdpSocketState`（保留 GSO）、腿 socket 保守取 1 |
| Q-L | **中继 200pps 闸的静默丢被 QUIC 当拥塞**（限速器 → CUBIC 降窗闭环，WG 侧无这一层） | r12 专5-3 | B2/B3 判据必须同时记 `congestion_events/lost_packets`；必要时上调本地 relay `--rate-limit` 做对照 |
| Q-M | **经中继迁移的控制面往返**（新 assoc/sid/腿 + `CTL_PEND_MAX=16` 上行缓冲 + `dial_wait=15s`）可能吃掉迁移窗 | r12 专1-1；`relay/mod.rs:54/908/1015/1501-1505`、`bind.rs:46` | S5-3 增加"经中继迁移"用例（连接保持 + 腿表/assoc 峰值 + pend 窗丢包量） |
| Q-N | **腿表增长**（每次中继迁移 +1 条腿，回收靠 relay 90s 空闲或出口 sweep；`RELAY_LEG_MAX=64`） | r12 专1-4；`bind.rs:46/491-493` | 门槛行必须写"设备表不新增 **且腿表峰值 ≤N（实测登记）**"，否则验收会把腿表增长误判为通过 |
| Q-O | **未认证 QUIC 连接的资源上限缺失**（握手对任意源开放，M2 才做抗放大/限流） | r12 6.5 | M1 加保守上限：并发握手 ≤64、连接总数 ≤ `2 × max_devices`、握手期限 10s（形态照 Q-H 批的 control 面加固先例）；超限记行 |
| Q-P | **`E23` 在 QUIC 档仍会被腿帧触发**（`bind.rs:382-421` 每个 kind 分支都调 `note_new_src`，含 `_`/kind=5） | r12 专1-5；`bind.rs:932-943` | 二选一：kind=5 分支显式跳过 `note_new_src`（并在 §3.6 登记"E23 对 QUIC 腿不适用"），或改登记条承认两行都出现。**本设计取前者**（QUIC 档的"新源"语义由 E-q2 承接） |
| Q-R | **候选类型/缓存端口语义**（WG 端口 vs QUIC 端口；hint 携带的是 WG 腿地址） | r12 6.4 | §2.7 的收窄（QUIC 档不吃 hint/学习缓存）；M3 统一候选模型 |

---

## 10. 实施清单（切片 + 完成判据；主会话据此切棒）

**依赖顺序**：S1 → S2 → {S3, S4} → S5 → S6（S3/S4 可并行，但都依赖 S1 的出口端点与 S2 的岛公面；S6 必须在全部代码切片合入后、且**独占机器**跑）。

### S1 出口 QUIC 面（端点 + 准入 + 数据面 + 中继承载）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S1-1 | 依赖：`homeway-quic` 加 `bytes`（M0 已登记「M1 必需」）；出口侧新增 `quic` feature/模块（**不改 relay/、不改 intercept/**） | `cargo check` 三目标绿；`tools/check-quic-isolation.sh` 五条仍绿 |
| S1-2 | 端点：`serve.quic_listen`（缺省 `listen+1`，退让语义同 WG）+ §1.2 的 TransportConfig + `migration(true)` + `E-q1` 行 | 端点起后 `E-q1` 行含实际端口/MTU/缓冲；`quic-ab.sh`（未变）仍绿 |
| S1-3 | 准入：控制流（首条 bidi 流）= **`hr-reg3` 帧（含 TLS exporter 连接绑定，§1.3）** → `table.register()` → 绑定表 `devKey ↔ conn`（**同 devTag 后到者替换并关闭旧连接**）；未登记连接的数据报丢弃 + `E-q3` 计数 | 单测：合法 reg 通过（`peer: +` 行）/ 坏 MAC 拒绝 / **重放（同帧换连接）必须失败** / 未登记数据报被丢且计数 +1 |
| S1-9 | **服务端身份（M1 必需，§1.3；已拍板提前到 M1，见 §12-②）**：出口 Ed25519 RPK（rustls `requires_raw_public_keys` + `AlwaysResolves*RawPublicKeys`）+ 公钥进 token + 客户端钉定 | 单测：错 RPK ⇒ 客户端握手中止（不是"连上再拒"）；`crates/` 内 `dangerous()/SkipVerify` **零命中**（隔离门已有断言） |
| S1-4 | 入站：DATAGRAM → 源校验（`src_allowed` 复刻）→ 唤醒 fd → `intercept.on_plain` | 单测/集成：合成 DNS 查询包 → `intercept` 收到（DNS 代答行 `E22` 计数增长）；源非法包 → `E-q3 源校验拒` |
| S1-5 | 出站：`route_encap` **按 dst 分流**（tun_ip → QUIC；tunnel_ip → WG 原样）；发送走 `send_datagram_checked`（预检 + 丢 + 计数） | 单测：两个 dst 各自走对的路径（可用桩替身断言调用面）；缓冲满注入 ⇒ 计数 +1 且不静默 |
| S1-6 | 中继承载：`kind=5` 常量 + 自定义 `AsyncUdpSocket`（直连 socket + 腿 socket + 注入队列）+ **三处落地**（订正 r12 专4-6）：①出口 `bind.rs` 的 kind 分支与 `Inbound` 承载字段 + `engine.rs::handle_inbound` 消费；②`frame.rs` 的 `FrameKind` 具名值；③客户端岛侧包封/剥壳（S2-7）。`wtransport/bind.rs`（WG 面）**零改动** | **本地中继实测**：`local-rust-relay.sh` + 本地出口 + 探针客户端经中继打通（§7.2 Z3/B1/B2 读数留证） |
| S1-7 | 公网可达性：QUIC 端口 UPnP 映射 + 实际端口写进 token 端点表（新类）；失败可见（E-q4） | `--upnp=false` 形态 ⇒ 只出 LAN/中继 QUIC 端点且打失败行；`--upnp=true` 形态 ⇒ 映射行 + token 含公网 QUIC 端点 |
| S1-8 | token：端点表加 QUIC 类（encode/decode + 既有端点行渲染） | `serve token` 行含 QUIC 端点；既有 WG 端点逐字节不变 |

### S2 客户端岛（赛跑 + 迁移 + 数据面）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S2-1 | `Cmd` 增补（§2.1 六成员，逐条写「带 reply 与否 + 理由」）+ `IslandSnapshot` 增补 | 公面签名钉定测试（M0 §9.1-10 先例）随成员同步；`check-quic-isolation.sh` 层 3 仍绿 |
| S2-2 | 赛跑：并行 `connect`，首完成者胜、余者 drop；`via/ep/rtt` 入快照；`N-a/N-b/C4'/C5'/C6'` 行（节流照 `bind.rs:73-76`） | 单测：三候选（一活两死）⇒ 胜者正确、耗时上界、`via` 正确；全死 ⇒ `IslandErr::NoCandidate` |
| S2-3 | 迁移：`Rebind` 命令 + rebind 后连接保持检测；失败回落既有阶梯 | 集成测试（**本地注入代理**）：`127.0.0.1:0` → `<LAN IP>:0` rebind 后 ①连接未断 ②服务端 `remote_address()` 变化（E-q2 行） ③收发继续（探针 P7 形态）；不设精确墙钟断言（flake 口径②） |
| S2-4 | 数据面：TUN 读线程 → `Cmd::TunPacket`；岛内 `max_datagram_size()` 检包 + 缓冲预检 + 发送；回程 `read_datagram` → 有界队列 → std 写线程 | 单测：超限丢 + 计数（窄路径注入）；回程队列满丢 + 计数；TUN 读线程投递失败自退（M0 形态复用） |
| S2-5 | 生命周期：构造点/停止点/收工顺序照 M0 §3.5 契约；`JoinSet` abort+join；panic 面六条 | 单测：预算内收工；卡死 detach + `hw-quic-reap` 收割；**detach 后老世代 UDP 源端口/连接数可观测**（M0 残余项，落成计数行 + 用例） |
| S2-6 | 控制流登记 + 刷新：`hr-reg3` 帧上控制流 + 60s 刷新（`N-e`）；`Probe` 与**巡检接线**（§2.5：隧道域 patrol 改探岛、连接死归 `patrol` 分类、反向腿失败不拆世代） | 单测：登记成功后出口 `peer: +` 行；刷新行按 60s 节拍（`tokio::time` + `start_paused`，M0 flake 口径②要求同批加 tokio dev-dep `test-util`）；**QUIC 连接被人为关闭 ⇒ `mark_unhealthy_if_current(gen,"patrol")` 可达**（判据：分类值 ∈ 既有取值集） |
| S2-7 | **客户端中继包封/剥壳**（§1.6 末段）：候选类别决定裸包/标签帧；下行按来源剥壳；非 kind=5 帧忽略 | 单测：直连+中继混合候选赛跑（§2.2）；下行剥壳正确还原（字节级）；非 kind=5 帧不入 quinn；**经中继全链（本地）打通** |

### S3 观测面与 A/B 开关

| 项 | 动作 | 完成判据 |
|---|---|---|
| S3-1 | `HOMEWAY_TRANSPORT` env + `tunConfig.transport` + `N-d` 行 + 非法值记行回落 | 单测：两种取值各自装配（岛构造/不构造）；非法值 ⇒ 记行 + 默认；回退档下 `quic:` 族行零输出；**_wg 档全链 E2E（本地）：L3+服务面全走 WG、C2/C4/C5/C6/C10/C15 打原串_**（r12 6.1：A/B 开关的全部价值在这条）；**配置不一致形态**（`serve.quic=false` × `transport=quic`）⇒ 按"token 无 QUIC 端点 ⇒ 岛全候选失败 ⇒ 记行 + 回落 WG"的裁决验收（r12 6.2） |
| S3-2 | `link:` 快照来源切换（岛快照）+ 状态 JSON `quic` 段（additive） | 单测：`link{via,ep,rttMs,at}` 键序/形态不变；`quic` 段字段齐（含 drops 四类） |
| S3-3 | 丢弃计数行同源（N-c ↔ JSON `drops`） | 单测：注入一次超限 ⇒ 行与 JSON 同时 +1 |
| S3-4 | 出口侧：`serve.quic` 开关（缺省 true） | `serve.quic=false` ⇒ 无 QUIC 端口监听、无 E-q1 行 |

### S4 判据行登记（与代码同批）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S4-1 | 按 §3.6 把登记条目粘进 `docs/INTEROP-CRITERIA.md`（含「计数输入集」节的三条：C3/C10/C11/C13/E23） | 条目 11 条齐全（字段四列）；`git diff` 与代码同批 commit |
| S4-2 | 新增行落地（N-a…N-d、E-q1…E-q4）并与实现逐一对照 | 每条新行都有一处实现 + 一处测试/实测证据 |
| S4-3 | 词表门复核（`tools/check-vocab.sh`） | PASS（M1 不新增词表 unit 的预期需实测确认；若新增须同步 ledger 口径） |

### S5 门槛实测（实现全绿后）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S5-1 | `tools/quic-ab.sh cpu/overhead/size/mem`（**独占机器**，留 `loadavg.tsv`） | 五臂读数在 M0 基线 ±10% 内（CPU/overhead/体积）、`mem` 按 §9.1 新口径判 |
| S5-2 | 端到端 A/B 吞吐（本地代理） | `HOMEWAY_TRANSPORT=wg` vs `=quic` 同刻交替 ≥3 轮；结论"无数量级回退"（本地合成的局限已在 §8 标注） |
| S5-3 | 中继预算与迁移读数（§7.2 B1/B2/B3/B5） | 包尺寸 1411/1402B（读字节计数，不用 lo0 分片断言）；上行 pps 上界 ≈200（含出口控制面同桶的形态说明）且触闸时同时记 `congestion_events`；**经中继下行吞吐 QUIC/WG 同刻 A/B**；**经中继迁移**用例（连接保持 + 腿表/assoc 峰值 + pend 窗丢包量） |
| S5-4 | 体积/内存登记（product 档 `.so` 增量 + footprint） | `[size]` 行 + `mem` 读数入 `docs/reviews/M1.md` |

### S6 代码门（第二道门）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S6-1 | dsh 代码门评审（commit 范围 = S1–S5） | 记录入 `docs/reviews/M1.md`（原文摘要 + 逐条处置 + 高危必改/豁免登记） |
| S6-2 | 专项 grep：`send_datagram(` 裸调用零命中（数据面只允许 `send_datagram_checked`）；`send_datagram_wait` 零命中；`relay/`+`relaywire.rs` 的 `use` 面不含 `quinn\|tokio\|rustls`（Z4 订正）；数据面**单线程**前提（预检两次加锁读的前提） | 脚本或 `check-quic-isolation.sh` 增断言 |

### 10.1 本地代理验证 vs 真机（本棒能/不能做的边界）

| M1 判据项 | 本地代理（可做） | 真机（用户/硬件触点） |
|---|---|---|
| 全局代理等价（层 0） | **合成 IP 流**：岛 → 出口 QUIC 面 → intercept → 本地目标（DNS 查询 / UDP echo / TCP 握手 + 数据），逐项断言 + `E10/E11/E12/E22` 行出现 | 浏览器 / 任意 App 经隧道全通（**必真机**：涉及 App 的 VpnService 与真实 TUN fd） |
| WiFi→蜂窝迁移 | rebind 注入（`127.0.0.1:0` → `<LAN IP>:0`）+ 断言连接保持 + 出口 `REMOTE-CHANGED`（P7 已证） | 真 WiFi→蜂窝（**必真机**：涉及系统网络切换与真实地址族变化） |
| 真机吞吐 | 端到端合成流量 A/B（筛"无数量级回退"） | 同刻 A/B 冷/热（**必真机**：PERF-AB 口径） |
| 中继预算/尺寸 | 全部可做（`local-rust-relay.sh` + 本地出口） | 经真中继（M6 可选） |
| OHOS 运行期 | **不可做**（无运行时）；可选用 Linux 二进制做 epoll 同族冒烟 | App 安装冒烟（tier 触点，M6/M7） |

**真机验证操作脚本（草案，供 M6/用户执行）**：

```bash
# 0) 前置：出口（本仓 release）在公网可达；App 装 QUIC 档核；手机与出口同 Wi-Fi
# 1) 层 0（浏览/任意 App）
#    手机浏览器访问 http://<出口所在网段的一台 HTTP 服务>；出口侧应见：
#      intercept: tcp transit <dst> ← <手机隧道 IP>:<port>（dialok）
#    且手机 App 状态页 link: via=direct ep=<出口:quic_port> rtt=<N>ms
# 2) WiFi→蜂窝迁移（关键判据）
#    保持一个长连接（如 speedtest 或 curl 大文件循环）→ 关闭 Wi-Fi（或飞行模式切换）
#    期望：① 客户端打 `quic: 迁移完成（<旧> → <新>，耗时 …）`
#          ② 出口侧打 `quic: 路径变更 dev=… <旧> → <新>` 且 **不出现新的 `peer: +`**
#          ③ 连接不断（长连接不 RST，或 TCP 重传后恢复）
#    判据：无重连、出口设备表条数不变（`serve.status` peers 计数不变）
# 3) 吞吐（同刻 A/B）：`homeway-cli speedtest` 形态在 wg/quic 两档交替各 ≥3 轮，取中位
# 4) 经中继：token 只留中继端点（或用 --dead-direct 形态）→ 重复 1)/3)
# 5) 窄路径：在出口上把 QUIC MTU 压到 1200（HOMEWAY_QUIC_MTU=1200）→ 期望 `quic: 丢弃 超限=…` 出现且不静默
```

### 10.2 与 M0 交付物的关系（防重复劳动）

- `tools/quic-ab.sh`（M0 转正）**本批不改**（只跑）；若端到端 A/B 需要新子命令（如 `e2e`），按「范围登记」处理并在收口列明。
- `crates/homeway-quic` 的 M0 骨架（`Cmd`/`Island`/生命周期）**形态冻结**，M1 只增不觉（新增成员照 §2.1 声明面纪律）。
- `docs/QUIC-BASELINE.md` 的基线数字**本批不改**（M1 只在 `docs/reviews/M1.md` 登记增量读数）。

---

## 11. 设计门记录（dsh）

> 本节在评审回填后定稿（v2）——**回填前本设计文档的状态是「未过门」**（防"门做假"，M0 先例 §10.1 的教训）。

### 11.0 轮次目录与指路

- **轮次目录**：`/tmp/dsh-review/r12.CrR3qv/`（`prompt.txt` / `output.md` **303 行** / `stderr.log`）；本轮**无子轮**。
- prompt 指路（不喂结论）：本设计文档 + `docs/QUIC-ROADMAP.md`（M1 节 / 门槛表 / 附录 D/E / 用户触点）+ `docs/reviews/M0-design.md` + `docs/reviews/M0.md` + `docs/QUIC-BASELINE.md` + `docs/INTEROP-CRITERIA.md`（判据行 + 变更记录 + 计数输入集）+ `AGENTS.md` + 代码入口（`server/{device,engine,bind,table,intercept/mod,upnp,egress}.rs`、`wgcore/mod.rs`、`wtransport/{bind,frame,reg,endpoint_cache}.rs`、`facade/{tun_exec,tun_status,bridge_host}.rs`、`session/{mod,recover}.rs`、`relay/mod.rs`、`tunnel_addr.rs`、`envflag.rs`、`crates/homeway-quic/**`、`capi/lib.rs`、`cli/main.rs`、`tools/*`）+ 上游 quinn/quinn-proto 源码 + 本棒探针 `/tmp/m1-probe` 与读数 `/tmp/m1-res/*`。
- 评审重点（专项）：迁移 / 赛跑裁决 / MTU 降级 / 中继预算 / 背压语义 + 路线文件评审协议 checklist（功能等价面 / 边界与错误面 / 并发与生命周期 / 残留 WG 语义隐含依赖 / 地道 Rust / 安全面 / 预算可测性），并要求"看过没问题的方面也明确说明"。
- **仓内副作用文件检查**：评审后 `git status --porcelain` = 仅 `?? docs/reviews/M1-design.md`（本设计文档）；探针与读数全在 `/tmp` ⇒ **无 dsh 副作用文件**（无需转存/删除）。

### 11.1 结论

- **dsh exit code = 0**（成败只认 exit code）。
- 评审意见 **52 条**：**高 14**（其中 6 条被评审列为**阻塞项** B1–B6）/ **中 25** / **低 13**；另 5 条**记录性/确认性**条目（§0 总体判断、§7 数字复核表、§8 覆盖度声明里的"看过没问题"清单）。
- **逐条处置：认同 51 / 部分认同 1 / 不认同 0**（部分认同 = 专4-4，见 §11.4）。
- **评审独立复核的结论（关键几条）**：§0.2 接缝表 **21 行行号逐条命中** ✅；§0.3 的 P1–P6 读数**逐字复核一致** ✅；**推翻 3 处**——①§1.2 的 `initial_mtu(1400)+upper_bound(1400)` 之说「仍有 DPLPMTUD」**错**（upper==initial ⇒ 二分退化 ⇒ 不发探测包，本棒已独立复核 `mtud.rs:303-345`）；②§2.2「输家 drop 不发 CONNECTION_CLOSE」**与 quinn 实际相反**（`implicit_close` ⇒ 发 `APPLICATION_ERROR` 关闭）；③§7.2 B3 的 ACK 预算**算错 5×**（需比值 8.42 而非 1.68）且"19 Mbps 锚"**无出处**。评审还独立复跑了 §0.1 的两条红（隔离复跑 = 2 passed ✅）。
- **过门结论：通过（v1 → v2，带残余）**。评审给的是「建议本轮设计门**不通过**，先处置 6 条阻塞项」；6 条阻塞项与本批高/中意见**已全部落到 v2**（逐条见 §11.3），故升 v2 并判**通过**，同时把三项范围问题显式挂账（§11.5）；**这三项 + MTU 降级形态已由用户 2026-10-08 拍板**（结论与落地边界见 **§12**），§11.5 的对应行已改为"已拍板"。

### 11.2 评审原文摘要（逐条）

| # | 评审意见（摘要） | 严重度 |
|---|---|---|
| **B1** | M1 没有出口身份校验方案（M0 隔离门禁 `dangerous()/SkipVerify`，产品方案被推到 M2），而 §4.1 默认 `transport=quic` | **高（阻塞）** |
| **B2** | reg 帧去掉「pubkey 必须完成握手」后，±90s 内重放捕获的 reg 帧即可获得**可用**隧道连接；"与今日同档"不成立 | **高（阻塞）** |
| **B3** | QUIC 连接的死活不进恢复阶梯（`tun_exec.rs:1661 patrol_loop` 仍探 WG 的 `path_probe`），QUIC 死而 WG 活 = App 流量黑洞且无人恢复 | **高（阻塞）** |
| **B4** | §1.2 定 4MiB×2/连接 vs §6.3/§9.1 裁决 1MiB×2/连接，自相矛盾（照 §1.2 实现 = 256MiB） | **高（阻塞）** |
| **B5** | §7.2 B3 的 ACK 预算算错 5×（需比值 8.42），"默认 ACK 已够"与自身 P5 数据相反；"19 Mbps 锚"无出处 | **高（阻塞）** |
| **B6** | 客户端侧 kind=5 包封/下行剥壳整块设计缺失（§1.6 只写出口），S2 无对应切片 | **高（阻塞）** |
| 专1-1 | 中继路径的迁移零证据，链上有"新 assoc→新 sid→新腿"控制面往返（pend 上限 16 / dial_wait 15s / 腿表 64） | **高** |
| 专1-2 | "迁移后拥塞态重置"的实测证据不成立（迁移前 cwnd 就是 12000，从未增长） | 中 |
| 专1-3 | "本地注入代理"不是 WiFi→蜂窝等价物，且不可移植到 CI（Linux runner） | 中 |
| 专1-4 | "设备表不新增条目"须拆成「设备表 + 腿表」两条判据（中继迁移会让腿表 +1） | 中 |
| 专1-5 | kind=5 帧同样会打 E23 形态行（`bind.rs:382-421` 每个 kind 分支都调 `note_new_src`），与登记冲突 | 中 |
| 专1-6 | `rebind` 无 per-connection 版本、对端不可达无事件（静默等 30s）——失败面没写进触发链 | 低 |
| 专2-1 | 输家 drop **会**发 CONNECTION_CLOSE（`implicit_close`），设计与 quinn 实际相反 | **高** |
| 专2-2 | 并行 connect 会在出口产生"同一 devTag 多条已登记连接"，没有现任裁决（回程可能发往被丢弃的连接） | **高** |
| 专2-3 | C5 新行丢了"响应集/未响应集"的端点清单（与"语义保留"声明冲突） | 中 |
| 专2-4 | 候选类型（`Vec<SocketAddrV4>`）无法表达中继候选（缺 via/label） | 中 |
| 专2-5 | 客户端侧"包封/剥壳 + 自定义 socket"设计缺失（= B6 的专项表述） | 中 |
| 专2-6 | 候选的"按 transport 过滤"未定义（WG/QUIC 端点互相投喂） | 中 |
| 专2-7 | `Connect{budget}` 语义未定（每候选/整轮、超时后 drop 还是保留、`NoCandidate` 上报时点） | 低 |
| 专3-1 | `initial_mtu(1400)+upper_bound(1400)` ⇒ **DPLPMTUD 实际关闭**；窄路径是"黑障跳到 min_mtu"不是逐级下探 | **高** |
| 专3-2 | `min_mtu=1200` 是"kill 档"（mds 1162 ⇒ 1280 内层包全丢，用户感知是断）；可用下限 1318/1329 | 中 |
| 专3-3 | §1.2 与 §6.3/§9.1 缓冲预算互相矛盾（= B4） | **高** |
| 专3-4 | 负载态门槛低估 2×（收发各 1MiB ⇒ 64MiB），且与"+3MB"单位不一致 | 中 |
| 专3-5 | `drop_oversized()`（MTU 变小时清掉已排队超限包）是**第 5 条静默丢弃通道**，"不静默"判据有洞 | 中 |
| 专3-6 | §1.2 的 PPPoE/余量算术两处错（41B 实为 IPv6 余量；真正会超的是 IPv6 1500） | 低 |
| 专3-7 | `mtuCap` 与既有 `tunConfig.mtu` 语义重叠（命名易混） | 低 |
| 专4-1 | B3 的 ACK 预算算错 5×（= B5） | **高** |
| 专4-2 | ack_frequency 的机制与副作用没写全（是 ACK_FREQUENCY 帧；`max_ack_delay` 不设 = 25ms） | 中 |
| 专4-3 | "经中继下行 ≈19 Mbps（R4 批 U2）"锚点无出处（在册锚是 27/2.7 Mbps；PERF-AB 列为 KNOWN-GAP） | **高** |
| 专4-4 | "谁限谁"讲错半张表：真实 TCP 下载的上行被**内层 TCP ACK**占满 ⇒ `ack_frequency` 杠杆基本无效 | **高** |
| 专4-5 | 本地测试拓扑把出口控制面算进客户端的 200pps 桶（同 IP） | 中 |
| 专4-6 | "中继零改动"成立，但端到端至少三处改动（出口 bind/Inbound + frame.rs + 客户端），设计只写一处 | 中 |
| 专4-7 | Z4 不可测（`cargo tree -p homeway-core` 恒含 quinn） | 低 |
| 专4-8 | 下行字节桶（16MiB/s）与 down_silent 未进预算表；"90s"未写扫描粒度（实际 90–95s） | 低 |
| 专5-1 | §6.1 的"今日 WG"侧描述层次错位（第一层是应用 ring 丢新 + 计数，不是 `localErr*`） | 中 |
| 专5-2 | 预检纪律正确但覆盖不完整（两次加锁读的单线程前提、错误变体映射、TooLarge 还含本地缓冲上限） | 中 |
| 专5-3 | "双拥塞控制"漏了最实质一条：**中继限速丢包对 QUIC 也不可区分于拥塞**（限速器→cwnd 闭环） | 中 |
| 专5-4 | 队列预算悬空（出口回程队列无上限；`Cmd::TunPacket` 无界是 M0 登记的 M1 项，本设计没有落点） | 中 |
| 专5-5 | 两侧丢弃计数不对称、缺"（进程×队列/错误）→ 计数字段"矩阵 | 低 |
| 6.1 | 回退档验收不足（缺"`transport=wg` 全链 E2E + 判据行原串"这条） | 中 |
| 6.2 | 出口 QUIC 端口不可用 × `transport=quic` 的配置不一致行为未定义；`route_encap` 分流未写"从 wire 面摘除" | 中/低 |
| 6.3 | 同 fd 双线程读写归属未写；`migrations` 计数字段无来源（quinn 无 PathEvent，只能轮询自计） | 低×2 |
| 6.4 | **巡检/恢复信号仍来自 WG**（= B3）；**学习缓存/候选端口语义冲突**（`EndpointCache` 学的是 WG 端点） | 高×2 |
| 6.4 | 需求信号（demand-driven）在 quic 档的生产者/消费者未定义 | 中 |
| 6.5 | **准入身份缺失**（= B1）；**reg 重放面**（= B2）；未认证连接无资源上限 | 高×2 + 中 |
| 6.6 | `Connect{cands}` 应换承载类型（= 专2-4）；`DatagramRejected{n}` 应为 `enum DropReason` | 中/低 |
| 6.7 | **CPU 门槛测的不是产品路径**（自定义 socket 丢 GSO 批处理）；B1 的"lo0 无分片"恒真；"≤40B"须写"不含中继信封"；§9.1 标签/单位错；`conns` 默认口径未切 | 中×3 + 低×2 |
| 覆盖度 | §3.2 与 §2.2 对 C4 的措辞小冲突 | 低 |

### 11.3 逐条处置表

| # | 处置 | 落到 v2 的位置 / 证据 |
|---|---|---|
| B1 | **认同**——M1 必须给客户端可验证的服务端身份；裁决 = **把 M2 的"服务端身份半边"（Ed25519 RPK + 公钥进 token + 客户端钉定）提前到 M1**，客户端证明半边仍留 M2；**绝不**在产品面用 `dangerous()`/`SkipVerify`；**用户 2026-10-08 拍板：按推荐项提前**（§12-②） | §1.3 末段、S1-9、§12-② |
| B2 | **认同**——加 **TLS exporter 连接绑定**：`hr-reg3`（`mac` 混入 `Connection::export_keying_material(out,b"hw-quic-reg",b"")`，API 已核 `quinn/src/connection.rs:625`）⇒ 重放帧换连接即失效 | §1.3（帧格式 + 理由）、S1-3 完成判据（重放必须失败）、§3.6 准入登记条 |
| B3 | **认同**——隧道域 `patrol_loop` 在 quic 档改探岛 `Probe`；连接的 `closed()` 事件映射成探活失败；QUIC 死归**既有分类 `patrol`**（不扩枚举）；反向（WG 死/QUIC 活）不得拆世代；需求信号消费者改读岛快照同名字段 | §2.5 全节重写、S2-6 |
| B4 | **认同**——§1.2 两行改 **1 MiB / 1 MiB**（并注明"见 §6.3 裁决"）；§1.7 队列改**绝对条数** | §1.2 表、§6.3、§6.4 矩阵 |
| B5 | **认同**——B3 判据**改为同刻 A/B 相对判据**（QUIC/WG 下行吞吐比 + 客户端上行 pps 比 + 拥塞事件），删除无出处的 19 Mbps 锚；保留算术口径作解释工具（附"需比值 8.42"的重算） | §7.2 B3、§9.3 Q-B |
| B6 | **认同**——客户端包封/剥壳 + 候选类型（via/label）落成 S2-7 与新切片；归属 = 岛内 socket 层 | §1.6 末段、§2.1（候选类型）、§10 S2-7 |
| 专1-1 | **认同**——§2.3 加"经中继迁移"事实链（新 assoc/sid/腿、`CTL_PEND_MAX=16`、`dial_wait=15s`、`RELAY_LEG_MAX=64`）；S5-3 加用例（连接保持 + 腿表/assoc 峰值 + pend 窗丢包量） | §2.3、§9.3 Q-M、S5-3 |
| 专1-2 | **认同**——P7 的"拥塞重置"改为"读数与协议一致但**未构造出可观测重置**"；计量口径改"先跑 bulk 再迁移"；登记"同 IP 换端口不重置"的不对称 | §0.3 P7（已在 §0.3 表保留原文 + §2.3 订正段）、§2.3 |
| 专1-3 | **认同**——本地代理升级为"服务端绑 `0.0.0.0` + 客户端在两个真实本地地址间 rebind"（CI/Linux 用 `127.0.0.1↔127.0.0.2` 制造 IP 变化走真迁移分支）；明确标注路径 MTU/丢包/NAT 重绑三类只能真机验 | §2.3、§10.1 |
| 专1-4 | **认同**——门槛行改"**设备表不新增 且 腿表峰值 ≤N（实测登记）**" | §7.2 B5、§9.3 Q-N、§10.1 真机脚本第 2 步 |
| 专1-5 | **认同**——取「kind=5 分支显式跳过 `note_new_src`」方案（QUIC 档"新源"由 E-q2 承接）并写进登记条 | §3.4 E23 行、§3.6 E23 条、§9.3 Q-P |
| 专1-6 | **认同**——触发链补"rebind 后 N 拍无回包 ⇒ 记行 + 回落重连/重赛跑" | §2.3 末条 |
| 专2-1 | **认同**——改为"输家被主动关闭（implicit_close 发 CONNECTION_CLOSE）+ 岛不等待关闭完成" | §2.2 第 2 行 |
| 专2-2 | **认同**——明确"同 devTag 后到者替换并关闭旧连接"（沿用 table 语义）+ 岛对已建立非胜出连接显式 `close()` | §2.2 新增"同 devTag 现任裁决"行、S1-3 |
| 专2-3 | **认同**——C5 新行**保留端点清单**（`完成=<清单>；未完成=<清单>`） | §3.3 C5 行 |
| 专2-4 | **认同**——定义 `Candidate{addr, via: Via}` / `Via::Relay{label:[u8;8]}`（岛侧 newtype） | §2.1 代码块 |
| 专2-5 | **认同**（并入 B6 处置） | §1.6 末段、S2-7 |
| 专2-6 | **认同**——候选**按 transport 过滤**（两族候选不得互相投喂）；WG 档不吃 QUIC 端点 | §2.1 末段、§3.6 C3/C13 条 |
| 专2-7 | **认同**——写死：`budget` = **整轮预算**；到点未完成者按 drop 收；全候选失败 ⇒ `NoCandidate` | §2.1 `Connect` 行 |
| 专3-1 | **认同（本棒独立复核一致）**——写明"upper==initial ⇒ 上探构造性关闭（`mtud.rs:303-345`）；黑障检测仍活、落 `min_mtu`（`mtud.rs:160`）"；`min_mtu` 由 1200 改 **1320**（黑障后 mds 1282 仍可服务 1280 内层包）；登记"黑障后不回升"代价 | §1.2 表、§5.1 表 |
| 专3-2 | **认同**——写入不变量 `内层MTU ≤ mds` ⇒ **QUIC MTU ≥ 1318**；旋钮有效区间 **[1320,1400]**；`mds < 内层MTU` 时除计数外加"窄路径不可用"行 | §5.1、§5.2 候选 A、§5.3 |
| 专3-3 | **认同**（= B4） | §1.2、§6.3 |
| 专3-4 | **认同**——负载态门槛改 **64 MiB + 自有队列**；单位统一 MiB | §6.3、§9.1-4 |
| 专3-5 | **认同**——`drop_oversized` 登记为第 5 条静默通道；岛内**每拍比对 `max_datagram_size()` 变化**并把自有队列超限包计 `超限` | §1.2 末行、§5.1 末行、§6.4 矩阵末行 |
| 专3-6 | **认同**——算例订正：IPv4/PPPoE 余 53B；**IPv6 1500** 才是约束（1400 余 41B；1452 超 11B） | §5.1 信封行 |
| 专3-7 | **认同**——改名 `tunConfig.quicMtuCap`（与既有内层 `tunConfig.mtu` 区分） | §5.2 候选 A |
| 专4-1 | **认同**（= B5） | §7.2 B3 |
| 专4-2 | **认同**——写明 ACK_FREQUENCY 帧机制 + **必须同设 `max_ack_delay=5ms`**（不设 = 沿用 25ms 反伤内层 TCP）+ 乱序 ACK 行为变化登记 | §1.2 `ack_frequency_config` 行 |
| 专4-3 | **认同**——删无出处锚；引用改行号可查（`INTEROP-CRITERIA.md:188` U2 只有体积/偏差；在册锚 = `docs/reviews/R2.md` 的 27/2.7 Mbps 与上行 2.7Mbps；`PERF-AB.md` 列 KNOWN-GAP）；判据改相对 A/B | §7.2 B3、§8 门槛表 |
| 专4-4 | **部分认同**（见 §11.4）——接受"内层 ACK 占上行预算、`ack_frequency` 只是部分杠杆"的方向，并据此把判据改成相对 A/B；但对"QUIC ACK 只是搭车"的普遍性保留（quinn 会把多个小 DATAGRAM **共包**，见 §11.4 证据） | §7.2 B3（"关键认识"段） |
| 专4-5 | **认同**——本地 relay 拓扑用 `127.0.0.2` 起出口，或登记"共桶"形态差异 | §7.2 B2 |
| 专4-6 | **认同**——S1-6 补三处落地（出口 bind/Inbound + frame.rs + 客户端）；Z1 同时给 `relay/` 与 `relaywire.rs` 零 diff 的 grep 证据 | §7.1 Z1/Z4、S1-6 |
| 专4-7 | **认同**——Z4 改 grep 断言（`relay/**` 的 `use` 面不含 quinn/tokio/rustls） | §7.1 Z4、S6-2 |
| 专4-8 | **认同**——预算表补**下行字节桶**（16MiB/s/assoc）与"90–95s（5s 扫描粒度）"表述 | §1.2 `keep_alive_interval` 行、§7.2 B5/B6 |
| 专5-1 | **认同**——对照表改两层对两层（应用 ring（丢新+计数）↔ 自有有界队列；内核 socket ↔ quinn 缓冲/预检） | §6.5 新表（§6.1 保留概述） |
| 专5-2 | **认同**——`send_datagram_checked` 伪码补：①**单线程岛**是预检前提（两次加锁读）；②错误变体映射（`UnsupportedByPeer`/`Disabled`/`ConnectionLost`/`TooLarge`）；③`TooLarge` 含本地缓冲单报文上限 | §6.4 表 + 备注、S6-2 |
| 专5-3 | **认同**——§6.2 补"中继限速丢包 ⇒ QUIC 不可区分于拥塞 ⇒ CUBIC 降窗"闭环；B2 判据加拥塞事件观察 | §6.2、§7.2 B2、§9.3 Q-L |
| 专5-4 | **认同**——给出两侧**绝对上限**（客户端 TUN→岛 4096 条；岛回程 2048 条；出口入站/出站各 8192 条）+ 丢弃归类；`Cmd::TunPacket` 改**有界** | §6.4 矩阵、S2-4 |
| 专5-5 | **认同**——加"（进程 × 队列/错误）→ 计数字段"矩阵 | §6.4 矩阵 |
| 6.1 | **认同**——S3-1 判据加"**wg 档全链 E2E**（L3+服务面全走 WG、C 族判据行打原串）" | §10 S3-1、§4.2 |
| 6.2 | **认同**——①配置不一致形态裁决（token 无 QUIC 端点 ⇒ 岛全候选失败 ⇒ 记行 + 回落 WG）；②分流出的 QUIC 包**不得留在 `out.wire`**（避免 `bind.send_wire` 二次发到 WG peer） | §4.1/§4.2 备注、§1.5、S1-5 |
| 6.3 | **认同**——①写清"引擎读腿 fd / QUIC 线程写腿 socket"的 fd 读写归属与非阻塞前提；②`migrations` 计数字段**来源 = 轮询 `remote_address()` 自计**（quinn 无 PathEvent） | §1.6/§1.7、§4.3 |
| 6.4 | **认同**——巡检（= B3）；学习缓存的端口语义 ⇒ **§2.7 收窄**（quic 档不吃 hint/学习缓存，登记为已知能力缺口）；需求信号（= B3 末行） | §2.5、§2.7、§9.3 Q-R |
| 6.5 | **认同**——身份（= B1）；重放（= B2）；**新增未认证连接资源上限**（并发握手 ≤64、连接总数 ≤2×max_devices、握手期限 10s） | §9.3 Q-O、S1-3/S1-2 |
| 6.6 | **认同**——候选类型（= 专2-4）；`DatagramDropped{reason: DropReason, n}`（enum） | §2.1 |
| 6.7 | **认同**——①CPU 门槛必须用**产品路径**（自定义 socket 丢 GSO ⇒ 登记 Q-K；S5 端到端 A/B 用产品路径）；②B1 判据改"读字节计数"（不用 lo0 分片断言）；③"≤40B"注明**不含中继信封**；④§9.1 标签/单位订正；⑤`conns` 默认口径切换列为**实现动作**（要改脚本，不只是"切过去"） | §1.6、§7.2 B1、§8、§9.1、S5-1 |
| 覆盖度-低 | **认同**——C4 措辞冲突消除（quic 档新增行 / WG 域原样保留） | §2.2、§3.2/§3.3 |

### 11.4 不认同项（含证据）

**部分认同 1 条（专4-4）**：接受其**方向**（真实 TCP 下载时客户端上行预算被**内层 TCP ACK** 占用，`ack_frequency` 只是部分杠杆；判据必须是相对 A/B 而非绝对门槛——已按此改 §7.2 B3）。**但对"QUIC ACK 只是搭车 ⇒ 杠杆基本无效"的普遍性保留**：

- 证据：quinn-proto 的 DATAGRAM 组包会把**多个小数据报塞进同一 QUIC 包**（`quinn-proto/src/connection/datagrams.rs:191-204` 的 `write()`：`if buf.len() + datagram.size(true) > max_size { push_front; return false }`，注释明确写"we could be more clever about cramming small datagrams into mostly-full packets when a larger one is queued first"——即**小包本来就共包**）。内层 TCP ACK 是小包（40–60B），在岛上按拍入队后**很可能被合并进同一个 UDP 包**，其 pps 成本因此**不是 1:1**。
- 结论：本设计**不**据此下"经中继下行只能到 4Mbit/s 量级"的结论（那会把一个未测的悲观值写进门槛）；改为**实测判据**（§7.2 B3 的 QUIC/WG 比值 + 客户端上行 pps 比 + `congestion_events`）。这条**部分不认同仅限量级推断**，不影响对评审判据改法的采纳。

### 11.5 门后残余与不做项登记（防静默漏做）

| 项 | 结论 | 理由 / 承接 |
|---|---|---|
| **服务端身份（RPK）提前到 M1** | **已拍板：提前（2026-10-08，§12-②）** | 不做 ⇒ M1 无法安全收口（B1）；做 ⇒ 提前的只是"出口密钥 + 公钥进 token + 客户端钉定"，客户端证明半边仍留 M2 |
| **内存门槛数值修订**（每设备 ≤96K / 单连接 ≤320K / 32 设备 ≤+3.1MiB / 负载态 ≤64MiB+队列） | **已拍板：四条全接受（2026-10-08，§12-③）** | 本棒不动路线文件；**路线文件门槛表由主会话同批同步** |
| **中继下行吞吐判据（绝对锚 → 相对 A/B）** | **已拍板：改相对 A/B（2026-10-08，§12-④）**；绝对数字只作登记并标注不可比 | §7.2 B3、§7.2 立场段、§8 真机吞吐行 |
| `transport=wg` 全链 E2E | **必须做**（S3-1） | A/B 开关的全部价值所在（r12 6.1） |
| 自有队列的绝对上限（4096/2048/8192） | **初值，须实测校正** | §6.4；实现期用 harness 定 |
| QUIC 档不吃 hint/学习缓存（无 RELAY-UPGRADE/打洞升级） | **M1 已知收窄** | §2.7；M3 统一候选模型 |
| `E23` 对 QUIC 腿不适用（kind=5 跳过 `note_new_src`） | **已裁决**，须在 §3.6 登记条里写明 | §3.6 E23 条 + 覆盖度条 |
| OHOS 运行期 / 真机项 / `panic="abort"` 跨仓约束 | **未验**，真机/用户触点 | §9.2（沿用 M0 口径） |
| 未认证连接的资源上限（M1 保守面） | **M1 做**，M2 换成正式准入 | §9.3 Q-O |
| §0.1 的两条红 | **已登记 flake**（隔离复跑绿） | M1 实现棒收口前须再跑全量（每期执行协议第 1 条） |
| dsh 副作用文件 | **无**（`git status` 仅本设计文档） | §11.0 末条 |

### 11.6 覆盖度声明

- **本轮覆盖**（评审者独立复核面）：§0.2 接缝表 21 行（**逐条命中**）、§0.3 的 P1–P8 读数（逐字复核 + 3 处推翻 + 2 处口径提醒）、§1.1 独立端口理由、§1.2 配置表（多格推翻/订正）、§1.3 准入面（新增 2 条高危）、§1.4 唤醒 fd、§1.5 出站分流键、§1.6 中继零改动（确认成立）+ 自定义 socket（补段能力面）、§2.1–§2.6 岛设计（赛跑/迁移/数据面/巡检/生命周期）、§3.1–§3.7 判据行与登记（字段/政策/13→11 条计数）、§4 A/B 与观测面、§5 MTU（3 处订正）、§6 背压（5 条）、§7 中继（8 条）、§8 门槛表、§9.1/§9.2 两件 M0 遗留裁决、§9.3 风险表、§10 实施切片。
- **本轮未覆盖**：真机/OHOS 侧任何行为；M2–M5 范围项（按要求不评）；探针源码非关键路径（评审声明只读了关键函数，未逐行审 448 行）。
- **"看过没问题"（评审者明确记录）**：§0.2 接缝表全部 21 行；§1.1；§1.4；§1.5；§1.6 的 relay 零改动主张（kind=5 不命中任何 relay 分支、信封尺寸 +11/+2 正确）；§2.4（除无界队列）；§2.6；§3.1/§3.2/§3.5；§4.1（envflag OnceLock 首读语义、`TunConfigJson` 无 `deny_unknown_fields`）；§4.3（`link` 段结构不变）；§9.2；§10（除 S2 缺客户端包封切片）；token 端点 kind 的 wire 兼容性（`EndpointKind::from_wire` 对未知值宽松 ⇒ additive 可行）；`Bytes::from(Box<[u8]>)` 零拷贝（bytes 1.12.1 `bytes.rs:984`）；`reject_log_due` 的"首 3 + 每 100"引用（与 `bind.rs` 的"首 3 + 每 1000"口径不同，须写明用哪一个）。
- **评审独立复跑**：§0.1 的两条红（隔离复跑 = **2 passed**）；本棒的探针读数与 `/tmp/m1-res/*` 对照。

### 11.7 回填后的再复验声明

- 本轮回填**只改文档**（`docs/reviews/M1-design.md`），**不改任何产品代码、不改路线文件、不碰主检出** ⇒ 无需重跑 `cargo test`/clippy/交叉 check；§0.1 的两条红与"隔离复跑绿"的结论仍是当前实测状态。
- **回填后 `git status --porcelain`**：应仅 `?? docs/reviews/M1-design.md`（§11.0 已核）。
- **本设计棒的实测资产**（供 M1 实现棒复核）：探针源码 `/tmp/m1-probe/src/main.rs`、读数 `/tmp/m1-res/*.txt` 与复现命令 `/tmp/m1-res/README.txt`、评审原文 `/tmp/dsh-review/r12.CrR3qv/output.md`。
- **本设计棒未做的**（明确登记，防下棒当作已完成）：任何产品代码、`docs/QUIC-ROADMAP.md` 的更新（主会话触点）、`docs/INTEROP-CRITERIA.md` 的登记条目落库（实现棒与代码同批）、探针之外的实测（真机/中继 A/B）。

---

## 12. 用户拍板记录（2026-10-08）

> **本节 = 实现棒的边界真源**：四个拍板点（全部按设计门 v2 的推荐项）逐条记「拍板点 / 候选 / 用户选择 /
> 依据（引用本设计的推荐理由）/ 对实施清单的落地影响」。**只看本节即可知道边界**，不必回翻正文。
> 拍板点出处 = `docs/QUIC-ROADMAP.md` 的「用户触点清单」（① 为路线文件明列的拍板点；②③④ 为本设计门
> 新增的上报项，由主会话转达并获拍板）。**路线文件的门槛表与拍板点由主会话同批同步——本棒不改该文件。**

### 12-① 内层 MTU 降级开关的形态（路线文件明列的用户拍板点）

| 项 | 内容 |
|---|---|
| **候选（§5.2）** | **A** 客户端本地降级 + 显式上限旋钮 / **B** 自动档位 + App 反馈（重建接口改 MTU） / **C** 出口侧 ICMP 反馈 |
| **用户选择** | **A = 推行**；**B = 保留为 M6 真机后的候选**；**C = 不做** |
| **依据（§5.2 推荐理由原文）** | A 落在 quinn 既有面（**零协议改动、零 tier 触点**），并把"窄路径可用性"表达为**可观测丢弃计数 + 专行**而非静默行为；B 需 tier 触点（改 VpnService MTU = 重建接口 = 用户可见断流一次）；C 工程量最大，且与 Q-B 批已登记口径「非 TCP/UDP 协议不建会话、**不产出 ICMP 不可达**」（`docs/INTEROP-CRITERIA.md` Q-B F9 注记）**直接冲突**——先改口径才谈实现 |
| **A 的完整形态（实现契约）** | ①内层 1280 保留；②每包 `max_datagram_size()` 检包 ⇒ 超限**丢 + 计数 + 节流记行**（`quic: 丢弃 超限=…`，首 3 + 每 100）；③显式上限旋钮 = env `HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap`，**默认 1400、有效区间 [1320,1400]**（低于 1320 时 1280 内层包必丢 ⇒ 区间下限即 `min_mtu` 同值）；④状态 JSON 暴露 `maxDatagramSize`/`currentMtu`；⑤`mds < 内层MTU` 时除计数外**另打一条「窄路径不可用」行** |
| **落地影响（切片）** | **S1-2**（出口 TransportConfig：`initial_mtu 1400` / `min_mtu 1320` / `upper_bound 1400`，§1.2）；**S2-4**（岛数据面：检包丢 + 计数 + 计数行）；**S3-2/S3-3**（状态 JSON `quic.maxDatagramSize/currentMtu` + 丢弃计数同源）；**S5-1**（窄路径注入实测：MTU 压到 1320 以下 ⇒「窄路径不可用」+ `超限` 计数可见）。**不得**自行实现 B/C 的机制（不得写 ICMP 反馈、不得自动改 App 接口 MTU） |

### 12-② 服务端身份（RPK）是否提前到 M1（本设计门上报项 B1）

| 项 | 内容 |
|---|---|
| **候选** | ①提前到 M1：只做**出口密钥 + 公钥进 token + 客户端钉定**（客户端证明半边仍留 M2）；②不提前：quic 档只能作本地/实验档、生产默认保持 wg（本设计不建议） |
| **用户选择** | **①提前到 M1** |
| **依据（§1.3 原文）** | 不做 ⇒ 客户端无法验证服务端 ⇒ 产品面只能用 `dangerous()/SkipVerify`，而 M0 隔离门（`crates/` 内 `dangerous()` 零命中，`tools/check-quic-isolation.sh` 断言）与 M1 退出口（"浏览器/任意 App 经隧道上网全通"）都不成立；做 ⇒ 提前的只是"出口密钥 + 公钥进 token + 客户端钉定"，工作量 < M2 的一半 |
| **实现契约（边界写死）** | 出口 **Ed25519 RPK**（RFC 7250：rustls `requires_raw_public_keys` + `AlwaysResolves*RawPublicKeys` + `verify_tls13_signature_with_raw_key`）；**公钥进 token**（additive 变更，按用户既有拍板①"无兼容包袱"）；客户端**钉定校验**（错 RPK ⇒ 握手中止，不是"连上再拒"）；**产品面 `dangerous()`/`SkipVerify` 零命中**（隔离门已有断言）。**M1 不做**：Hello/Challenge/Proof、抗放大（Retry/token）、握手限流——但**M1 加保守资源上限**（并发握手 ≤64、连接总数 ≤2×max_devices、握手期限 10s，§9.3 Q-O） |
| **落地影响（切片）** | **S1-9**（新切片：出口 RPK + token 公钥 + 客户端钉定 + 单测"错 RPK 握手失败"）；**S1-3**（准入：`hr-reg3` = reg MAC **混入 TLS exporter 连接绑定**，杀掉 reg 回放面——与身份同批落，见 §1.3/B2）；**S4-1**（§3.6 的两条登记：准入绑定口径 + token 端点表/RPK 字段）。**M2 承接**：客户端证明半天 + 抗放大 + 威胁模型文档 |

### 12-③ 内存门槛的四条修订（本设计门上报项，§9.1 裁决）

| 项 | 内容 |
|---|---|
| **候选** | ①只改拟合口径（不动门槛）；②两者都改（口径 + 门槛数值）；③维持原门槛、压实现 |
| **用户选择** | **§9.1 的四条修订全盘接受**：①拟合口径改**五点**（N=1..5；三点作对照）；②每设备 ≤64K → **≤96K**；③单连接 ≤+256K → **≤+320K**；④新增**负载态门槛**：出口 32 连接持续流量下增量 **≤64 MiB + 自有队列上限** |
| **依据（§9.1 原文）** | 附录 A 的 37.6K/连接**没有原始证据链**（只有手抄 SUMMARY，`m_s-*.out` 全为 11 字节空壳）⇒ 不能作门槛依据；本 harness 三点拟合实测 96.0K、五点 81.6K（复跑 76.8K），quinn 每连接结构（CryptoBuffer 16KiB + 流/路径 + 定时器堆 + CID 表）就是 77–96K 量级；96K × 32 = 3.0 MiB ⇒ 配套把「32 设备 ≤+2MB」改 **≤+3.1 MiB（稳态）**；负载态 = 32 ×（1 MiB send + 1 MiB recv，§6.3 裁决）+ 自有队列（§6.4 矩阵） |
| **落地影响（切片 + 判据）** | **S5-1**：`mem` 子命令按**五点口径**跑（`--conns-points 1,2,3,4,5`；**把默认口径从 1,3,5 切过去 = 实现动作**，要改 `tools/quic-ab.sh`）+ 负载态扩展（32 连接）读数；**S1-2/S2-4**：每连接 `datagram_send/receive_buffer_size = 1 MiB`（**不是 4MB**）+ 自有队列绝对上限（§6.4 矩阵）；**§8 门槛表**按修订后四值判。**主会话同批同步**路线文件「性能/体积/内存门槛」表 |

### 12-④ 中继下行吞吐的判据形态（本设计门上报项 B5/专4-1/专4-3/专4-4）

| 项 | 内容 |
|---|---|
| **候选** | ①保留绝对锚（需先补测在册锚）；②改**同刻 A/B 相对判据** + 拥塞事件，绝对数字只作登记并标注不可比 |
| **用户选择** | **②**：经中继 vs 直连的**同刻比值**作门槛，同时记 `congestion_events`/`lost_packets`（分辨"限速器静默丢被 QUIC 当拥塞"）；**绝对数字只作登记、标注不可比** |
| **依据（§7.2 B3 原文）** | v1 的"19 Mbps 锚"**无出处**（`INTEROP-CRITERIA.md:188` 的 U2 行只有 23.7MB 体积与 −0.28% 偏差；在册锚是 `docs/reviews/R2.md` 的 27/2.7 Mbps，且 `PERF-AB.md` 把"speedtest 经中继"列为 **KNOWN-GAP**）；且 v1 的比值算术**错 5×**（需比值 8.42 而非 1.68）；更关键：真实 TCP 下载时客户端上行被**内层 TCP ACK** 占用，`ack_frequency` 只是**部分杠杆** ⇒ 绝对门槛会把一个未测的悲观值写进验收 |
| **实现契约** | 门槛 = **`下行吞吐比 = quic/wg`（同刻、各 ≥3 轮）** + 「无数量级回退」判定 + 客户端上行 pps 比 + `congestion_events/lost_packets` 读数；`ack_frequency_config`（阈值 16 + `max_ack_delay=5ms`）**按 M1 默认配置落地**，其净收益（默认 vs T=16 两档）**实测登记**；**中继代码零改动**（红线）：预算问题一律在客户端/出口侧解决 |
| **落地影响（切片）** | **S1-2**（出口下发的 ACK_FREQUENCY + `max_ack_delay`）；**S5-3**（`local-rust-relay.sh` + 本地出口：同刻 A/B + 拥塞事件 + 包尺寸 1411/1402B + 经中继迁移用例）；**S5-1**（`congestion_events/lost_packets` 进状态 JSON 与 `docs/reviews/M1.md` 的证据包） |

### 12.5 拍板后的**不变项**（防实现棒越界解释）

- **不在拍板范围、按本设计原样执行**：①②③④ 以外的全部设计决策（§1–§11）——包括但不限于：独立 QUIC 端口与 token 端点类（§1.1）、`kind=5` + 自定义 `AsyncUdpSocket`（§1.6）、出站按 `tun_ip`/`tunnel_ip` 分流与"服务面留在 WG"（§1.5）、A/B 开关 `HOMEWAY_TRANSPORT`/`tunConfig.transport`（§4）、丢弃四类计数与矩阵（§6.4）、中继零改动红线（§7.1）、判据行登记 13 条（§3.6）。
- **明确不做（拍板后仍不做）**：候选 B/C 的 MTU 机制；M2 的客户端证明/抗放大/威胁模型（M1 只做保守资源上限）；M3 的服务流迁移与栈 B 退役；任何中继代码改动；`docs/QUIC-ROADMAP.md` 与 `docs/INTEROP-CRITERIA.md` 的本棒改动（前者 = 主会话触点；后者 = 实现棒与代码同批落库）。

### 12.6 实施期订正（主会话裁定，2026-10-08；**后到的切片以本节为准**）

> 纪律依据：路线文件「每期执行协议」第 6 条——「实测/源码与设计矛盾 → **先在设计文档登记再改**」。

1. **隔离门第 ⑤ 条形态变更（原文不可能成立，替代形态更强）**：§1.3/§12-② 写的「`crates/` 内
   `dangerous()`/`SkipVerify` 零命中」与「客户端 RPK 钉定」**不能同时成立**——rustls 0.23 公开面里
   装自定义 verifier **只能**经 `dangerous().with_custom_certificate_verifier(...)`
   （`with_webpki_verifier` 只收 `WebPkiServerVerifier` 具体类型）。
   **裁定（接受 S1a 的替代形态）**：允许面收窄到**单一文件**（`crates/homeway-quic/src/exit/rpk.rs`），
   且该文件必须出现 `verify_tls13_signature_with_raw_key`（钉定 ≠ 跳过验证）——**其余 `crates/` 仍零命中**；
   `tools/check-quic-isolation.sh` 第 ⑤ 条按此改（已改，白名单 + 同文件自证，fail-closed）。
   语义：**「产品面不得出现跳过验证」这条纪律没有放松，放松的只是"哪个 API 能承载钉定"这个实现形态**。
   ⇒ **S4 登记**须把本条一并记入（隔离门口径变更）。
2. **M1 中间态被接受（S1a→S1b→S1c 之间）**：S1a 之后 `serve` 已监听 QUIC 端口、握手可完成，但
   **准入（S1-3）与数据面（S1-4/5）尚未接线**，`serve.quic` 开关（S3-4）也还没来 ⇒ 该端口在此窗口内
   **无实际数据能力**。裁定：**接受**（分支开发态；M1 收口前 S1b/S1c/S3 补齐；**合回 main 前必须
   开关可用**——列入 M1 收口核对项）。
3. **S1a 的其余偏离（一并接受）**：①Q-O 闸①口径收紧为「存活连接 + 在途握手」合计（比 §9.3 Q-O 原文严）；
   ②`handshake_cap`/`handshake_deadline` 做成可配（缺省 = 设计定值 64 / 10s，仅为让拒绝路径可确定性测）；
   ③`crates/homeway-core/src/relay/rltoken.rs` +1 行 `rpk: None`（token 加字段后编译器强制；**relay 语义与
   字节零变化**，红线未破）；④`client_pin` 暂挂 `#[allow(dead_code)]`（消费方 = S2 岛接线，S2 落地即删）。
4. **本节的登记归属**：第 1 条进 S4 的 `INTEROP-CRITERIA.md` 登记条目；第 2 条进 M1 收口核对项
   （`docs/reviews/M1.md`）；第 3 条随 S1a 各 commit 的偏离说明留档。
