# 拦截面单线程 reactor 简化批 · 技术设计（reactor-design）

> 批次入口 = ROADMAP「拦截 reactor 简化批」（2026-10-07 挂单）。目标：把出口拦截层的
> 重拨 OS socket 从 8-worker 池收进**单个事件循环**（redis/nginx 式：每流一个状态结构
> 〔读/写兴趣位 + 缓冲〕、无锁无 Arc 无 worker 分配、无跨线程流队列）。**协议语义零
> 改动**——语义真源仍是 `docs/reviews/R3-design.md`（SYN 缓存三态拨号 §4.1 / UDP
> 五元组会话与 pending 重放 §3 / 豁免 demux §4 / RST+ICMP responder §3 / Stats 与判据行
> §4.1）；本批是**执行结构的替换**，不是行为变更。被消灭的 bug 族（本会话历史 bug
> 密度最高处）：R3 池族三 bug（fd 属主表泄漏 EMFILE〔H1〕/拨号先行时序〔H2〕/池
> 形态选型〔H4〕）、Written 差额补报（files 上传「写通道长时间无进展」，2026-10-02
> 实测）、Ack 只认进栈量 + 短返丢弃面、收工 Closed 回执 vs 宽限窗口竞态（丢尾包）、
> ubuntu CI 共享 runner 饿死 worker 线程（exempt/dial 测试三轮红的根因注释在案）。
> 全部为**跨线程管道的结构性 bug**——单循环下这些代码路径整体不存在。

## 一、必答① 边界裁定：候选 B（数据面全归一），reactor = 驱动线程本线程

**裁定：B。** reactor 不新起线程——**拦截层的全部 OS socket 归驱动线程独占**，与
smoltcp Interface、流表、NAT 面、定时器同线程。拨号线程、8 条 worker、mpsc 命令/
事件双通道、唤醒管道、fd 跨线程 Adopt——整族删除。

「一个循环」的**实现形态**：readiness 发现 = `pump()` 内部 `poll(2, timeout=0)` 自查
（就绪集是**提示不是契约**——全部 fd 非阻塞 + poll 电平触发，漏看的事件下一拍必
然重报，误看的事件读出 EAGAIN 自然跳过）。**不采用**「引擎把拦截层 fd 并进主
pollfd 数组」的合流形态，理由：

| 面 | pump 内 poll(0)（采用） | 合流进引擎 poll（弃） |
|---|---|---|
| 交接 | 同为**零跨线程交接**（fd/缓冲/状态全在驱动线程） | 同左——两者都满足 B 的「彻底无交接」 |
| API | `pump()/pump_hold()/pump_grace()` 签名不动：引擎、harness 三臂、单测、`drain()` 兼容面零改动 | 引擎要拼兴趣集 + 回填就绪切片，pump 族签名全改（5 个调用面） |
| 唤醒时延 | 上游事件最晚 = 引擎 poll 超时拍（见下——1ms/5ms 两档） | 立即 |
| 系统调用 | 每拍 2 次 poll（引擎 1 + pump 1）——仍少于现状（引擎 1 + 8 worker 各 1） | 每拍 1 次 |

**时延补齐**：引擎 poll 超时从两态 `tx_shape_wait()` 扩成
`wait_hint()`——`整形滞留非空 ∨ reactor 存在等待者（connecting 中 ∨ 任一流
out_buf 非空）` ⇒ 1ms 拍，否则 5ms 拍（`reactor_waiters` 在 pump 建 poll 集时顺带
算出缓存，O(1) 读）。静默客户端的上游推送（long-poll/QUIC 静默段）最晚 1ms 被
发现；活跃传输期客户端 ACK 本身持续唤醒引擎，pump 拍频 ≫ 超时档。引擎侧净改动
= 一行匹配臂。

「循环职责过大」的反权衡（为何仍选 B）：① 驱动线程**已经**串行承担 decap+NAT+
栈 poll+encap+定时器——worker 池是唯一的多线程例外，收编是补完不是扩权；
② 每拍工作量有界（见必答④），nginx/redis 单循环承 10k+ fd 是行业基线形态；
③ 拆 A（独立 reactor 线程 + 队列交接）保留的正是本批要消灭的管道——交接面上
每一类 bug（序、拷贝、清账、收据）都会以新形态复发。**发送线程（homeway-serve-tx）
照旧分离**（P1 收益与收工语义不动——它交接的是「已 encap 密文」的发送，不在
拦截层流状态里）。

等待原语 = `poll(2)`（与全仓一致：引擎驱动循环/发送线程/既有 worker 全是 poll；
v0.2.2 刚把 pselect 亚毫秒面删净归一）。kqueue/epoll 的优势（O(1) 注册、长命集）
在「每拍重建兴趣集、fd ≤ ~5k」的形态里不兑现，且引入平台分派——不用。

## 二、必答② 每流状态结构：struct + 兴趣位（两层），缓冲策略沿用并显式化

**形态裁定：struct + 兴趣位（不采用单一 enum 状态机）。** 流的**协议相位**与
**IO 相位**是正交面（DNS 腿无 IO 也有 Established；TCP 连接中 IO 相位 =
connecting 而协议相位 = Dialing）——enum 状态机会造出 2×3 组合态的伪状态。
沿用两层：

```rust
// 协议相位（不变，mod.rs 现状）
enum Phase { Dialing { cache: Vec<Vec<u8>> }, Established }

// IO 面（新；pool.rs 的 FlowIo 收编进 Flow——一个流一个结构）
struct ReactorIo {
    fd: RawFd,                 // -1 = 无 OS socket（DNS 进程内腿）
    udp: bool,
    out_tcp: VecDequeLite,     // 栈→upstream 字节流待写（TCP/UDS；前缀消费）
    out_udp: Vec<Vec<u8>>,     // 栈→upstream 数据报队列（UDP；整取整发——帧界显式）
    connecting: bool,          // 非阻塞 connect 在途（POLLOUT 兴趣 + SO_ERROR 验收）
    dial_deadline: Instant,    // connect 10s 死线（Go dialTimeout 同值）
    dead: bool,                // upstream EOF：out 缓冲排空后才关 fd（尾数据不丢）
    linger_rst: bool,          // 关 fd 前设 SO_LINGER(0)（Drain 到期 RST 收口）
}
// Flow 增 io: Option<ReactorIo>；pool.rs 的 FlowIo/fd_index/属主表删除。
```

**兴趣位**（pump 每拍重建，纯函数面可单测）：

| 位 | 条件 |
|---|---|
| POLLIN | io 在 && !dead && !connecting && `tx_backlog.len() < WATERMARK`（下行背压门控读端——Ack 清账通道删除后，门控即水位直读） |
| POLLOUT | `connecting`（connect 验收）∨ 任一 out 缓冲非空（写续传） |

**背压（语义不变、清账通道删除）**：
- 下行（upstream→栈）：读 fd 进 `Flow.tx_backlog`（现状缓冲语义原样——部分写回补
  /FIN 挂起全保留）；`tx_backlog ≥ 256KB` 停读（摘 POLLIN）。flush_backlog 在
  service_sockets 开窗即续写——**水位随写自然回落，无 Ack 往返**（现状的
  「Ack 只认进栈量」语义 = 直读 backlog 长度，一字不差）。
- 上行（栈→upstream）：service_sockets 读栈内 socket 追加 out 缓冲 + 立即试写
  （非阻塞 send 到 EAGAIN）；EAGAIN 余量留缓冲等 POLLOUT。**unacked_out 记账
  删除**——`unacked_out ≡ out 缓冲待写量`，直接可读；Written 差额补报通道
  （written_total/written_reported 双计数器）整族删除。

**缓冲拷贝（R6.6 双拷贝收敛）**：现状每方向 2 拷（通道消息 Vec ↔ 缓冲 extend），
新形态每方向 1 拷（栈缓冲 ↔ 流缓冲）——零拷贝需栈内缓冲外露，不值。

**关闭路径**：`linger_rst` 在关 fd 前设 SO_LINGER(0)（teardown 直调——Close 命令/
Closed 回执/forget_flow 三条收口路径删除，fd 恰好关一次 = remove_flow 的
ReactorIo drop）。

## 三、必答③ UDP 五元组会话入环：拨号同步化，pending 重放窗口坍缩为零（语义不变）

非阻塞形态下 UDP「拨号」= `bind(0.0.0.0:0)` + `connect(target)`——两者都是**本地
状态操作，无网络往返**，循环内同步完成（µs 级、永不阻塞）。⇒

- **窗口坍缩**：现状 pending 窗口 = 「首包 → 拨号线程 DialOk 回驱动」（异步线程
  调度的 ms 级）；新形态拨号在 `udp_new` 同一调用内完成，`on_plain` 也在驱动线程
  ——**窗口内不可能到达任何后续包**（同线程串行）。pending 队列
  （`UDP_PENDING_MAX=16` 丢最新）与 Dialing{cache} 的 UDP 臂删除；首包直接
  入 out_udp 队列。
- **语义核验**（对客户端可观察面）：① 首包到 upstream 的时序不变（现状 = DialOk
  后重放，新 = 即时，更快且不乱序）；② 窗口内后续包：现状缓存后按序重放直投
  upstream，新形态无窗口、后续包经栈内 socket → service_sockets 读出 → out_udp
  ——同一 FIFO 序（栈内 socket rx 缓冲 64 槽 ≥ 现状 pending 16 上限，突发容量
  只增不减）；③ 拨号失败（EMFILE/EADDRNOTAVAIL）：同步失败 → 现状 DialFailed
  同路径（日志行 + remove_flow）——`udp intercept` 会话行（E12）与计数不变；
  ④ UDP 会话上限 4096/ICMP responder/idle 60s（DNS 10s）/E12 建立/关闭行/会话号
  分配时点全部原样。
- **connected UDP 的 ICMP 回错**（macOS ECONNREFUSED 教训在案）：recv/send 非
  EAGAIN 错误 → 拆会话（现状 worker mark_eof → finish_udp 同语义，判据行同串）。

TCP 的 Dialing 窗口**保留**（真网络 connect 异步）：SYN 缓存 ≤4、DialOk 注入
（SYN-ACK 由此产生）、失败 RST、黑洞 10s 三态逐字不动。

## 四、必答④ 公平性与饥饿：poll 返回序轮转 + 每流每拍字节预算

- **每拍处理**：poll(0) 返回的就绪 fd 集按数组序逐个处理，每个 fd 恰好一轮
  「读尽到 EAGAIN 或水位 / 写尽到 EAGAIN 或空」——单流单拍上限 ≈ 256KB 读
  （WATERMARK 门）+ 其待写缓冲（≤ 水位余量）；**任何就绪 fd 不可能在同拍被跳过**，
  未就绪 fd 下一拍 poll 必然重报（电平触发）——无持续性饿死路径。
- **单流风暴有界**：256KB/拍 × 拍频（活跃期 ≥200/s）⇒ 单流 ≤ ~50MB/s 的循环占用
  上界，其余流同拍照常处理；超过水位的读被摘 POLLIN 门控——**慢消费者自我降速**
  （现状 Ack 清账达到同一稳态，新形态少一次往返延迟）。
- **worker 饿死族结构性消失**：无 worker 线程即无「共享 runner 满载饿死拦截层
  工作线程」形态（mod.rs 测试头注释在案的 ubuntu CI 三轮红根因）。
- 定期扫描面（reap_idle/cc_stats/dial_fail_seen）O(flows)/拍，与现状相同（现状
  也是每拍全表扫）——不新增。

## 五、必答⑤ 定时器面：无新原语，死线检查搭现有拍

| 定时器 | 现状 | 新形态 |
|---|---|---|
| TCP connect 死线 10s | `TcpStream::connect_timeout(10s)`（拨号线程内） | `dial_deadline` 字段，pump 的 reap_idle 同扫（逾期 → DialFailed 路径：RST + 计数 + 降噪行） |
| idle 回收（TCP 5min/UDP 60s/DNS 10s/TCP-DNS 30s） | reap_idle 每拍全表扫 | 原样（dial_deadline 并入同一扫） |
| smoltcp 栈定时器 | pump 内双 poll + poll_delay 不消费 | 原样 |
| 整形续水/观测窗 | tx_shape_wait 两态 + Instant 差分 | 原样 |

**不做 min-of-timers 定时轮**：引擎 poll 恒 ≤5ms 返回（1ms/5ms 两态 + wait_hint
扩档），所有死线的检查粒度 ≤5ms ≪ 最短死线 10s——定时轮是负收益复杂度。
poll(2) 毫秒粒度足够（无亚毫秒等待需求；亚毫秒面属发送线程批速，v0.2.2 已裁定
归一 poll）。

## 六、必答⑥ 扩容后路：nginx 式 N-reactor 分片（只注记，不留缝）

单循环的容量上界 = 单核驱动（现状判定锚 = Go 出口 ±50%，R8 实测远超锚位）。
若未来需要分片（10k+ 并发流/多核卸载）：**分片线 = Flow.io 的 fd 归属**——按 fd
哈希把流分到 N 条 reactor 线程（各持 poll 集与 out 缓冲），与驱动线程之间只剩
「smoltcp socket 读写请求 + upstream 数据回投」一对队列——即回到候选 A 的形态，
但**只在单循环被实测证明不够时**才付这个复杂度（A 的交接面 bug 族就是代价表）。
本批不实现、不预埋抽象（预埋 = 过度设计）；保持 `Flow` 自含（io 状态不散落）使
未来分片线干净即可。

## 七、循环内禁阻塞清单（本批第一评审硬项）

| 面 | 处置 |
|---|---|
| DNS 上游查询（阻塞面全长 2.5s/查询） | **保持在外**——DnsProxy 专用 worker 线程，`submit_*` 非阻塞投递 + DnsReply 回投通道（现状 H3 形态原样） |
| 阻塞 connect | **删除**——TCP/UDS 非阻塞 connect（O_NONBLOCK + EINPROGRESS + POLLOUT + `getsockopt(SO_ERROR)` 验收）；UDP bind/connect 本地即时 |
| 文件 IO | 拦截层循环内零文件 IO（纯内存状态；dial_fail_seen 等同） |
| 同步 sleep | 循环内零；收工宽限循环的 10ms sleep = 引擎侧既有形态（非 reactor 拍），保留 |
| 日志写入 | `logf` 行级 Mutex 包 writer（低频/降噪门控行，现状原样——判据行频次不因本批变化） |
| files/term/speedtest 服务面 | **保持在外**——UDS listener 服务线程原样；拦截层只以非阻塞 connect 拨它们的 UDS |

## 八、判据行与计数不变清单（同串核验面）

`intercept: 过境拦截就绪（隧道IP %v；豁免=转投本机同端口；TCP 并发上限 %d）`（E5）、
`intercept: tcp %s %v ← %v（dialok）`（E10）、`… 关闭`（E11）、
`udp intercept: 会话 #%d %s 建立/关闭（…）`（E12）、tcp/udp 拒绝行、
拨号失败降噪行（首行 + 每 100 次）、Stats 六键 `[dialok, dialfail, flows,
rejected, udpReplied, udpNoReply]`（键序与「dialok/dialfail 仅 TCP」口径原样；
AtomicU64 保留——udpcap 线程跨线程读）。dns/整形/cc 观测行原样。**新行零增**
（启动判据面不添串——E5 行文案不动，reactor 形态不进判据）。

## 九、删改清单与简化收益度量

- **删**：`pool.rs` 全文件（619 行：Worker 池/双通道/唤醒管道/Adopt/dial 线程/
  IntoRawFdArc/pool_end_to_end_uds 测试）；mod.rs 的 `events`/`on_event`/
  `maybe_reap`/Ack/Written 全站点/`unacked_out`/`forget_flow`/worker 选择算式。
- **增**：`ReactorIo`（含缓冲与连接态）、非阻塞 dial + connect 验收、兴趣集
  构建 + pump 内 poll(0) + 就绪处理、`wait_hint`。
- **度量**：净删行数 = 本批 diff 统计（`git diff --stat` 前后），入册 ROADMAP 收口
  节——「保持代码最为简洁」的兑现度量。
- 线程账：拦截层 8 worker + 暂态拨号线程 → 0（出口常驻线程 -8 起）。

## 十、验收网

① 单测/E2E：`cargo test --workspace`（exempt/dial-fail/udp/DNS 四 E2E 面语义断言
原样过 = 行为不变的机器证明）；② harness 有损链路三臂（`--ignored` 串行：
downlink_lossy_link_recovery A/B + cold_air_form_verification + pacing_three_path_arms
——吞吐门自校准，reactor 化不得劣化）；③ `tools/ci-local.sh` quick 全绿（clippy
-D warnings + OHOS 交叉 check + 向量/词表门 + RRR 矩阵冒烟）；④ 真机烟囱：v0.2.3
滚动后手机自动重连 → E5/E10/E11/E12 同串 + Stats 计数推进 + `stats:` fdRead/fdWrite
双向增长（坑 1 真凭据判据）。发版 = v0.2.3（B0-2a 手法；阿里云 **linux-amd64**
〔v0.2.2 拿错 arm64 的实录教训〕）。
