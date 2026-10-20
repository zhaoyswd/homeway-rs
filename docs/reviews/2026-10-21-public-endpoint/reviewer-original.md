## 结论

方向是对的，根因也对上了：DoH 直连 IP + 钉卡 TLS 绕开 fake-IP 系统解析；STUN 候选惰性尝试；QUIC 正常/panic 收尾清绑定表，确实断开了 `bindings → Connection → ExitQuicSocket → bridge` 的自环。

但最终状态还不能算完全收敛。仍有 **4 个中等问题** 会在“代理/TUN 环境 + restart”这一目标场景下重新制造原症状或造成新卡死：DoH 的 3s 总期限没有覆盖 rustls 内部 IO；首轮 token 兜底没有超时且没有整轮预算；停止位与 token 追加未串行化；relay 注册腿还持有一份独立 UDP dup，`bridge.clear()` 管不到。另有 2 个低风险边界（Linux index=0 钉卡被跳过、STUN6 的 v4-mapped 字面量提前 break）。

---

## 范围与工作树注记

- 复核范围按 `62dbdf2..c8f56be`，包含初始 `68e8ce4`、`fbfd421` 和 r1–r4；未改产品代码、未碰生产、未派 agent、未调用 dsh。
- 开始复核时 `git status` 实际不是完全干净：有未跟踪的 `docs/reviews/2026-10-21-*` 两项（不属于本批提交）。我没有改动它们；其中两个隔离 PoC 我只用于复核 TLS 期限和 mapped-v6，不把该材料当作本批提交结论。
- 我重跑了两个本地隔离 PoC，未跑真网/生产：TLS 3s 预算实测 **4.018s** 返回；mapped-v6 选择复现“选中 `::ffff:8.8.8.8`、真正 IPv6 后备未被消费”。

---

## 逐条问题

### 1. 中｜DoH 的 3s 总期限没有覆盖 TLS 内部 IO 循环

**位置**：`crates/homeway-core/src/server/doh.rs:250-257`，`doh.rs:303-362`；期限设置见 `doh.rs:226-235`。

**问题**：`remain()` 只在 `StreamOwned::write_all` 前设置一次 TCP 读写超时；但 rustls 的 `write_all`/`read` 会在 `complete_io` 内部循环读底层 socket，每次底层 `read` 都能重新拿到完整的 socket 超时。外层的 `Instant` 检查只在 `StreamOwned::read` 整体返回后才执行，TLS 握手或 TLS record 慢速滴流期间拿不到控制权。

**触发与影响**：DoH 服务器或中间层每 500ms 送 1 字节 TLS record，单次查询就会超过 3s；多候选/双服务器会继续放大。它直接拖住 `homeway-pubep`，使首轮公网探测、token 兜底和 restart 收尾都被拖长；与下面第 2 条叠加时，旧线程可以长期滞留。

**建议**：在 `TcpStream` 外包一层实现 `Read + Write` 的 deadline-aware 适配器，每次底层 IO 前按剩余时间设置 socket 超时；或改用非阻塞 TLS 状态机自行推进。补一个 TLS record/握手滴流的回归测试，而不是只测 `Cursor`/普通 `Read`。

**严重程度 / 归因**：中；本批引入。

---

### 2. 中｜首轮 `first_probe` 等待无超时，且整轮探测没有总预算

**位置**：`crates/homeway-core/src/server/engine.rs:1003-1010`、`engine.rs:1025`（`first_probe_rx.recv()`）、`engine.rs:1945-1947`（发送信号）、`engine.rs:1140-1154`（shutdown 不 join pubep）。

**问题**：`fallback_wait` 只在发送端全部 drop 时使用；`first_probe_rx.recv()` 本身没有超时。`first_probe` 又必须等 `refresh_public_endpoint` 完成，而该函数最前面有 UPnP 40s 预算（`server/upnp.rs:33`），候选逐个走 DoH（每服务器 3s）再走 STUN（每候选 4s）；默认 v4 4 个、v6 2 个候选，没有整轮 deadline，也没有取消位。`shutdown` 只 join driver，不 join pubep/tokfb。

**触发与影响**：UPnP 无 IGD、DoH 连接/握手慢、或上面第 1 条触发时，`first_probe` 迟迟不发；`homeway-tokfb` 永久阻塞在 `recv()`，首轮 token 不打印。旧 pubep/tokfb 线程持有 `TokenCtx`（以及进行中的 DoH TCP fd）跨 restart 不退出，重复 restart 会累积线程/fd。代码注释宣称的“15s 兜底档”在这种形态下不成立。

**建议**：把 `recv()` 改为 `recv_timeout(fallback_wait)`，超时后用当前 `last_published` 走一次安全发布；给整轮探测加绝对 deadline/取消位（DoH、UPnP、STUN 共享），shutdown 时能打断；至少为 pubep/tokfb 保存 join handle 并在收工时 join。

**严重程度 / 归因**：中；等待结构是既存问题，本批新增的 DoH/TLS 长阻塞面使其更易触发，且 r1–r4 的停止位没有覆盖它。

---

### 3. 中｜停止位只在调用前采样，旧世代仍可重置 token 台账

**位置**：`crates/homeway-core/src/server/engine.rs:2117-2120`、`engine.rs:2252-2268`、`engine.rs:2492-2494`；首轮信号路径 `engine.rs:1945-1947` + `engine.rs:1025-1043`；shutdown 不 join 见 `engine.rs:1140-1154`。

**问题**：
- `refresh_public_endpoint` 在写盘/发布会话前检查 `pubep_stopped`，但检查与 `print_client_token` 之间没有锁或世代号；旧线程越过检查后可在 `print_client_token` 内被暂停，等新实例写入新 token 后再继续。
- `print_client_token` 先 `append_token`（`engine.rs:2492-2494`），之后才在 `inner` 锁内比较 `last_token`；并发/旧世代追加不会被拦截。
- `public_endpoint_loop` 在 `refresh` 因停止位返回 false 后，仍会执行 `first_probe_tx.try_send(())`；`tokfb` 的 `print_client_token`（`engine.rs:1043`）没有检查 `pubep_stopped`，`driver_alive` 在 shutdown 进行中仍可能为 true。

**触发与影响**：在 `serve restart`/`serve stop` 正好落在 UPnP/DoH 探测或发布窗口时，旧世代可在新世代之后把旧端点写进 `tokens.jsonl` 末行。随后 `serve token`/兜底读到旧端口或旧端点，客户端直连失败，重新落中继——正是本批要修的用户症状。该交错是确定性的，不依赖极小概率的寄存器竞争。

**建议**：用一个“发布锁 + 世代号/epoch”包住“停止检查、写盘、`last_published`、token 追加”整体；shutdown 在同一把锁下使旧世代失效；pubep 不要被 join 之外的方式“尽力停止”。`first_probe` 只在未停止且确实完成一轮时发送，`tokfb` 发布前也检查停止位。补确定性并发测试（barrier 卡在 append 前，让新世代先写）。

**严重程度 / 归因**：中；本批新增停止位但未闭合，底层无 join 是既存结构。

---

### 4. 中｜relay 注册腿持有同一 UDP 端口的独立 dup，`bridge.clear()` 不能保证端口释放

**位置**：`crates/homeway-core/src/server/engine.rs:709`、`engine.rs:742-748`、`engine.rs:845-864`；`crates/homeway-core/src/server/relayleg.rs:119-135`、`relayleg.rs:171-200`；shutdown 见 `engine.rs:1140-1158`；本次断环见 `crates/homeway-quic/src/exit/mod.rs:1184-1188`。

**问题**：`relay_sock_dup = sock.try_clone()` 是公共 QUIC UDP socket 的独立 fd 副本，被 `spawn_relay_leg` 的线程长期持有。该线程只在线程顶部检查 stop；阻塞在 `events.recv_timeout(5s)` 或 `handle_control` 时不会立即退出，`shutdown` 也没有保存/join 它的 handle。`bridge.clear()` 只断开 QUIC 面自环，无法关闭这份独立 dup。

**触发与影响**：配置了 `--relay` 时，`serve restart` 在旧 relay 腿线程尚未被调度退出前装配新引擎，新 `bind` 仍可能 `EADDRINUSE`，于是端口继续 +1，token/UPnP 的端口缓存再次失配。新增的 `stop_releases_listen_port_after_admission` 用的是 `plain_hook` 里的 dup，路径属于 QUIC 面自环，不能证明 relay dup 也被释放；`stop_releases_listen_port_after_panic` 同样没覆盖。

**建议**：为 relay 腿保存 `JoinHandle`，shutdown 在装配新引擎前有界 join；或让 relay 腿 socket 的生命周期显式绑定到 QUIC 面（由同一 owner 关闭），不要依赖 detached 线程自然退出。补一条带 `--relay` 的 restart 回归，至少断言旧 relay dup 已关闭后端口可重绑。

**严重程度 / 归因**：中；独立 dup 是既存结构，本批端口释放修复未覆盖，测试也未覆盖。

---

### 5. 低｜Linux index=0 时 DoH 跳过钉卡，和“按名可钉”的平台语义冲突

**位置**：`crates/homeway-core/src/server/doh.rs:278-289`；平台语义见 `crates/homeway-core/src/server/egress.rs:371-386`；cell 写入见 `engine.rs:727-733`。

**问题**：Linux 的 `pin_socket_to_iface` 按网卡名 `SO_BINDTODEVICE` 钉卡，index 只是诊断值，index=0 仍可成功；引擎也允许 `resolved.index == 0` 时把 `(0, name)` 写入 `pin_iface`。但 DoH 侧有 `if index != 0` 的额外保护，Linux 上 index=0 时直接跳过钉卡，TCP 走默认路由。

**触发与影响**：Linux 上 `if_nametoindex` 失败但网卡名有效（例如特定 netns/接口形态）时，公共 UDP socket 已按名钉卡，DoH 却没钉；在 TUN/fake-IP 环境中解析腿可能仍被代理捕获，或者解析失败，公网端点不公布。属于平台边界，但代码自己的契约明确支持该形态。

**建议**：不要在 DoH 侧用 index 先验拦截；有 name 就调用 `pin_socket_to_iface`，让该函数按平台决定（macOS/Windows index=0 会返回错，Linux 按名绑定）。补 Linux index=0 + 有效 name 的单测。

**严重程度 / 归因**：低；本批引入。

---

### 6. 低｜STUN6 成功分支未校验地址族，v4-mapped 会提前 break

**位置**：`crates/homeway-core/src/server/engine.rs:2330-2332`（字面量分支）、`engine.rs:2213-2215`（成功即 break）、`engine.rs:2225-2230`（事后才判 `global_v6`）。

**问题**：字面量分支只用 `ip.is_ipv6()` 判族，而 `::ffff:8.8.8.8` 也返回 true；`poisoned_ip` 对公网 mapped 地址不拦。候选被返回后，v6 循环收到任意非污染应答就 `v6 = Some(ap); break;`，之后 `global_v6` 才发现是 v4-mapped 并丢弃，但真正的 IPv6 后备已经不会再试。v6 服务器返回 v4/v4-mapped 观测地址时同理。

**触发与影响**：显式配置 `--stun6 [::ffff:8.8.8.8]:3478,[2001:...]:3478`（或 v6 服务器返回 mapped 结果）时，v6 路径不公布，尽管后面的真 IPv6 候选可用。默认域名路径的 `resolve6` 已排除 mapped，所以默认配置不受影响。

**建议**：字面量分支要求 `v6.to_ipv4_mapped().is_none()`；v6 循环在 `break` 前校验 `ap.ip()` 是“非 mapped 的 IPv6”，不满足就 `continue`。补 mapped 在前、真 v6 在后的回归。

**严重程度 / 归因**：低；本批引入。

---

## 低风险/非阻塞观察

- **默认 DoH 是新的单点，fail-closed 的可用性代价已登记但值得明确**：`DEFAULT_DOH` 在 `engine.rs:143-148`，运行时 fail-closed 在 `doh.rs:203-213`。若 Alidns 的 443 被网络策略阻断、证书/服务故障，所有 STUN 域名都无法解析，公网端点不公布，客户端只能落中继。这是有意的安全取舍（CHANGELOG 已写“不退回系统解析器”），不按新缺陷阻塞；但若目标是“少落中继”，更简做法是允许“系统解析结果经污染闸后仍可用”或直接用 IP 字面量 STUN 候选。建议至少补一条 fail-closed 的可观测计数/告警。
- **停止位没有覆盖 UPnP 长段**：`engine.rs:2095` 的 `refresh_quic_public` 在停止检查之前，`unified_cli.rs:898-901` 又是先 `shrink_upnp_lease` 再 `shutdown`。旧的 pubep 线程若正在/刚进入 UPnP 映射，可能在缩租后重新续租，留下旧端口映射；影响低，但属于资源释放面。建议先置停止位、在 UPnP 前后都检查，并 join pubep。
- **DoH 的 qtype 过滤放在 server 循环之后**：`doh.rs:178-201` 先返回首个服务器的 clean 集合，`resolve4/6` 才按族过滤（`doh.rs:142-155`）。若首个 DoH 服务器返回了“干净但错误族”的应答，第二个服务器不会再试。触发面很窄（非规范/异常服务器），但修起来简单：在 `resolve` 里按 qtype 过滤，过滤后为空就继续下一个候选。
- **Rust 惯用法**：`DohError` 是 thiserror 类型，`StunCandidates` 用 `Iterator` 惰性化，整体没有明显逐行移植痕迹。小瑕疵是 `build_query`/`parse_answers` 仍返回 `Result<_, String>`（`doh.rs:403`、`doh.rs:428`），与 AGENTS 的“错误一律 thiserror/Result 链、不用字符串错误”不完全一致；无运行时影响。
- **文档/测试小缺口**：`CHANGELOG.md:3` 写“r1–r3”，而范围已含 r4；`nodestate.rs:549-554` 的镜像测试仍把 `stun6`/`doh` 写成硬编码，只有 `stun` 引用了 `DEFAULT_STUN`；INTEROP 的 E20 条目没有列 `STUN6` 行族。都属于低风险登记问题。

---

## 看过且认为没问题的方面

- **DoH 根因方向**：`host@ip` 直连 + TLS/webpki 校验 + TCP 钉卡 + 不经系统解析器，确实解决了 fake-IP 把域名答成 `198.18.x` 的根本问题；解析结果和 STUN 观测结果复用 `poisoned_ip` 单源闸，v4-mapped/CGNAT/私网/ULA 的处理方向正确。
- **HTTP/DNS 解析**：`Content-Length` 精确读、截断、chunked 显式拒绝、状态码字段解析、NODATA 与污染分开、域名 label/255B 边界，逻辑和测试都到位；没有发现新的越界/无限读问题。
- **STUN 多候选**：惰性 `StunCandidates`、逐候选 4s 预算、v4/v6 域名解析选族、观测结果过闸、失败候选继续，整体符合“首个可信应答者胜”的目标；默认候选和 `DEFAULT_STUN` 镜像在主要路径上一致。
- **端口自环修复**：正常收尾和 panic 路径都调用 `bridge.clear()`，位置在 `endpoint.close`/`drop(conns)` 之后、`exit.mark_exited` 之前；`lock_unpoison` 能处理 panic 留下的中毒锁。新增的正常/panic 端口重绑用例方向正确，能覆盖 `Connection → ExitQuicSocket → bridge` 这条主环。
- **Windows 钉卡**：`IP_UNICAST_IF` 网络序、`IPV6_UNICAST_IF` 主机序的修正是对的；真实运行证据仍按 INTEROP 登记待补，这次不重复判为缺陷。
- **CLI/config**：`--doh` 的空格形、内联形、空串 carve-out、`serve_config_of` 覆盖和默认配置链路都通；`--doh ''` 显式回退系统解析器的语义清楚。
- **Rust 形态**：没有新增不必要的 `Arc`/`Mutex`（`pin_iface` 的共享有实际用途），DoH 的 `Read` 泛型单测是好的设计；没有发现明显的逐行移植痕迹。

---

## 总评

- **方案根因**：解决得对；DoH 直连/钉卡、STUN 多候选、污染闸、`bridge.clear()` 都是针对根因的修法。
- **最终状态**：不建议直接宣称“完全收敛”。第 1–4 条建议在 r5 处理，尤其是第 3 条（旧世代 token 台账）和第 4 条（relay dup 端口释放）会直接复活“旧端口/旧端点导致手机落中继”的症状；第 5–6 条可低风险跟进。
- **更简做法**：DNS 腿若允许“系统解析结果过闸后可用”，可避免 DoH 单点 fail-closed 带来的新可用性回退；shutdown 侧与其增加多个停止位检查，不如用 join/单一 owner + 世代锁，把端口和 token 的最后一次发布串行化。
