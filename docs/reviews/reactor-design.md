# 拦截面单线程 reactor 简化批 · 技术设计（reactor-design）

> **v2 = dsh 设计评审（2026-10-07，1 高/7 中/6 低）整改后版本**；评审原文与逐条处置见
> `docs/reviews/reactor.md` 第一道门。本版本即实装基线，再偏离须回评。
>
> 批次入口 = ROADMAP「拦截 reactor 简化批」（2026-10-07 挂单）。目标：把出口拦截层的
> 重拨 OS socket 从 8-worker 池收进**单个事件循环**（redis/nginx 式：每流一个状态结构
> 〔读/写兴趣位 + 缓冲〕、无锁无 Arc 无 worker 分配、无跨线程流队列）。**协议语义零
> 改动**——语义真源仍是 `docs/reviews/R3-design.md`（SYN 缓存三态拨号 §4.1 / UDP
> 五元组会话与 pending 重放 §3 / 豁免 demux §4 / RST+ICMP responder §3 / Stats 与判据行
> §4.1）；本批是**执行结构的替换**，不是行为变更。被消灭的 bug 族（本仓 bug 密度最高
> 处）：R3 池族三 bug（fd 属主表泄漏 EMFILE〔H1〕/拨号先行时序〔H2〕/池形态选型
> 〔H4〕）、Written 差额补报（files 上传「写通道长时间无进展」，2026-10-02 实测）、
> Ack 只认进栈量 + 短返丢弃面、收工 Closed 回执 vs 宽限窗口竞态（丢尾包）、ubuntu CI
> 共享 runner 饿死 worker 线程。全部为**跨线程管道的结构性 bug**——单循环下这些代码
> 路径整体不存在。

## 一、必答① 边界裁定：候选 B（数据面全归一），reactor = 驱动线程本线程

**裁定：B。** reactor 不新起线程——**拦截层的全部 OS socket 归驱动线程独占**，与
smoltcp Interface、流表、NAT 面、定时器同线程。拨号线程、8 条 worker、mpsc 命令/
事件双通道、唤醒管道、fd 跨线程 Adopt——整族删除。

「一个循环」的**实现形态**：readiness 发现 = `pump()` 内部 `poll(2, timeout=0)` 自查
（就绪集是**提示不是契约**——全部 fd 非阻塞 + poll 电平触发，漏看的事件下一拍必然
重报，误看的事件读出 EAGAIN 自然跳过）。**不采用**「引擎把拦截层 fd 并进主 pollfd
数组」的合流形态，理由：

| 面 | pump 内 poll(0)（采用） | 合流进引擎 poll（弃） |
|---|---|---|
| 交接 | 同为**零跨线程交接**（fd/缓冲/状态全在驱动线程） | 同左——两者都满足 B 的「彻底无交接」 |
| API | `pump()/pump_hold()/pump_grace()` 签名不动（harness 三臂/cross_pump/收工面零改动）；`tx_shape_wait` 扩为 `wait_hint`（engine 一行 + 单测同步扩档） | pump 族签名全改（5 个调用面）+ 兴趣集拼接/就绪切片回填 |
| 唤醒时延 | 上游事件最晚 = 引擎 poll 超时拍（wait_hint 两档 1ms/5ms，见下） | 立即 |
| 系统调用 | 每拍 2 次 poll（引擎 1 + pump 1），**每次 O(N) 扫描**（N = 名下 fd 数），成本随活跃流数线性增长、全在驱动线程关键路径——现状是 8 条 worker 各自 1000ms 阻塞 poll（事件驱动、不占驱动拍），此项是**本批的净增成本面**；缓解 = pollfd Vec 字段复用（clear 不重建）+ §十 reactor 观测行剂量面（pumps/5s、均拍、名下 fd 数） | 每拍 1 次 O(N) |

**时延补齐**：引擎 poll 超时从 `tx_shape_wait()` 两态扩成 `wait_hint()`——
`整形滞留非空 ∨ reactor 存在等待者（任一流 connect 未完成 ∨ 任一流待写缓冲非空）`
⇒ 1ms 拍，否则 5ms 拍。**计算时点 = pump 返回前**（service_sockets/整形释放之后）
重扫一次缓存（评审 R-5：缓存在 pump 开头算会漏掉本拍 service_sockets 新攒出的
out_buf——恰是 files 上传卡死族的形态，要多等一个 5ms 拍）；扫描 O(N) 与兴趣集
构建同级、自愈式重算（非增量计数——增量会漂）。

「循环职责过大」的反权衡（为何仍选 B）：① 驱动线程**已经**串行承担 decap+NAT+
栈 poll+encap+定时器——worker 池是唯一的多线程例外，收编是补完不是扩权；
② 每拍工作量有界（见必答④），nginx/redis 单循环承 10k+ fd 是行业基线形态；
③ 拆 A（独立 reactor 线程 + 队列交接）保留的正是本批要消灭的管道——交接面上每一类
bug（序、拷贝、清账、收据）都会以新形态复发。**发送线程（homeway-serve-tx）照旧
分离**（P1 收益与收工语义不动——它交接的是「已 encap 密文」的发送，不在拦截层
流状态里）。

等待原语 = `poll(2)`（与全仓一致：引擎驱动循环/发送线程全 poll；v0.2.2 刚把 pselect
亚毫秒面删净归一）。kqueue/epoll 的优势（O(1) 注册、长命集）在「每拍重建兴趣集、
fd ≤ ~5k」的形态里不兑现，且引入平台分派——不用。

## 二、必答② 每流状态结构：struct + 兴趣位（两层），缓冲策略沿用并显式化

**形态裁定：struct + 兴趣位（不采用单一 enum 状态机）。** 流的**协议相位**与
**IO 相位**是正交面（DNS 腿无 IO 也有 Established；TCP 连接中 IO 相位 = connecting
而协议相位 = Dialing）——enum 状态机会造出 2×3 组合态的伪状态。沿用两层：

```rust
// 协议相位（不变，mod.rs 现状）
enum Phase { Dialing { cache: Vec<Vec<u8>> }, Established }

// IO 面（新；pool.rs 的 FlowIo 收编进 Flow——一个流一个结构）
struct ReactorIo {
    fd: OwnedFd,               // 类型承担不变量：drop 恰关一次（无 -1 哨兵——
                              //   无 OS socket = Flow.io 为 None，DNS 进程内腿即此）
    udp: bool,
    out_tcp: VecDequeLite,     // 栈→upstream 字节流待写（TCP/UDS；前缀消费）
    out_udp: VecDeque<Vec<u8>>, // 栈→upstream 数据报队列（UDP；整取整发——帧界显式）
    conn: ConnState,           // None / InProgress（POLLOUT 验收）/ Retry（EAGAIN 重拨）
    dial_deadline: Instant,    // connect 10s 死线（Go dialTimeout 同值；reap_idle 先判）
    dead: bool,                // upstream EOF：待写缓冲排空后才关 fd（尾数据不丢）
}
```

**fd 关闭规则表**（评审 R-3——「恰一次」由 `OwnedFd` drop 保证，关闭**时点**按规则）：

| 场景 | 动作 |
|---|---|
| upstream EOF 且待写缓冲排空（`dead ∧ out 空`） | **立即关 fd、`io = None`、Flow 保留**（栈侧 FIN/idle 收尾照旧——对齐现状 worker「排空即收 fd、流记录等栈侧」） |
| 拨号失败 / 验收 SO_ERROR≠0 / connect 死线 | `remove_flow`（io 随流 drop 即关——socket() 已建而 connect 失败的 fd 同此收口） |
| **硬写失败**（send 非 EAGAIN 错误，如对端 RST 后 EPIPE） | **先清待写缓冲（余量静默丢 = 旧 worker「EOF 后不再写」口径）再走 EOF 路径**——不清则 `dead ∧ 排空` 永不成立、fd 挂到 5min idle + 每拍注定失败的 send + wait_hint 恒 1ms 空转（评审 r2-中1） |
| teardown / close() / 收工 / idle 回收 | `remove_flow`（同上） |

**兴趣位**（pump 每拍重建，纯函数面可单测——`interests_for(io, backlog_len) -> events`）：

| 位 | 条件 |
|---|---|
| POLLIN | `conn == None ∧ !dead ∧ tx_backlog.len() < WATERMARK`（下行背压门控读端——Ack 清账通道删除后，门控即水位直读） |
| POLLOUT | `conn == InProgress`（connect 验收）∨（`conn == None ∧` 任一待写缓冲非空）（`Retry` 态**不挂任何位**——每拍主动重拨，见下） |

**revents 分发规则**（原样搬自 pool.rs，评审 R-10）：

| revents | 处置 |
|---|---|
| POLLIN | 读（读尽到 EAGAIN / 水位） |
| POLLOUT | `InProgress` → connect 验收；否则待写缓冲续写 |
| POLLHUP∨POLLERR 且无 POLLIN | EOF 路径（TCP：dead + FIN 挂起；UDP：finish_udp）——**macOS connected UDP 的 ECONNREFUSED 正是只有 POLLERR 无 POLLIN 的形态**，两条发现路径（POLLERR-only 与 recv 错误）都保留 |
| POLLNVAL | 防御：摘除该 fd（不应发生——debug 断言 + 记行） |

**背压（语义不变、清账通道删除）**：
- 下行（upstream→栈）：读 fd 进 `Flow.tx_backlog`（现状缓冲语义原样——部分写回补/
  FIN 挂起全保留）；`tx_backlog ≥ 256KB` 停读（摘 POLLIN）。flush_backlog 在
  service_sockets 开窗即续写——**水位随写自然回落，无 Ack 往返**（现状「Ack 只认
  进栈量」语义 = 直读 backlog 长度，一字不差）。
- 上行（栈→upstream）：service_sockets 读栈内 socket 追加待写缓冲 + 立即试写
  （非阻塞 send 到 EAGAIN）；EAGAIN 余量留缓冲等 POLLOUT。**unacked_out 记账删除**
  ——`unacked_out ≡ 待写缓冲量`，直接可读；Written 差额补报通道
  （written_total/written_reported 双计数器）整族删除。

**待写续写的三态步进**（评审 r2-高1 整改后写死）：`Wrote`（写进了——续写）/
`Full`（EAGAIN——**必须 break 等 POLLOUT**：原地重试 = 驱动线程自旋 = 整出口挂死，
单线程收编后无任何线程能解围）/ `Dead`（硬错误——先清待写缓冲再走 EOF，见规则表）。
**缓冲拷贝（R6.6 双拷贝收敛）**：现状每方向 2 拷（通道消息 Vec ↔ 缓冲 extend），
新形态每方向 1 拷（栈缓冲 ↔ 流缓冲）——零拷贝需栈内缓冲外露，不值。

**非阻塞 connect（评审 R-1/R-2 整改核心）**——拨号一律 `libc::socket + fcntl
(F_SETFL, O_NONBLOCK) + libc::connect`（**禁止 std 的 connect/connect_timeout**：
std 在非阻塞 socket 上把 EINPROGRESS 当 Err 返回；`SOCK_NONBLOCK` 在 libc 的 apple
目标未定义——必须 fcntl）。返回错误**三分类**：

| 类 | 错误 | 处置 |
|---|---|---|
| 即时成功 | connect 返回 0（UDS 豁免腿常态；TCP 回环偶发） | 直接进 `dial_accept`（与验收路径**同一收口函数**——R-1：即时成功不产生任何 poll 事件，若验收只挂 POLLOUT ⇒ 流卡到 10s 死线） |
| in-progress | EINPROGRESS / EALREADY；EINTR = 立即重试本调用 | `conn = InProgress`：POLLOUT/POLLERR/POLLHUP 任一 → `getsockopt(SO_ERROR)` 验收（0 = 成功 → dial_accept；EISCONN 亦视同成功；≠0 → on_dial_failed）；**conn 的唯一清零点 = 验收函数**（否则 POLLOUT 兴趣常驻空转） |
| retry | EAGAIN（**Linux** AF_UNIX 监听队列满——阻塞形态下内核会让等，非阻塞报 EAGAIN） | `conn = Retry`：**每拍主动重拨 connect**（共用 dial_deadline，不挂 poll 位——未连接 socket 恒可写，POLLOUT 等不到有用信号且可能 SO_ERROR=0 误验收）；平台注记：macOS 队列满 = ECONNREFUSED（fail 类，与无监听同错——现状阻塞形态亦如此，非本批漂移） |
| fail | 其余 errno | `on_dial_failed`（fd 经 remove_flow 收口） |

UDP「拨号」= socket + bind(ephemeral) + connect——全部本地操作无网络往返，恒
「即时成功」类（路由不可达类 errno 如实走 fail）。TCP_NODELAY 在验收成功后设
（即时成功路径与 InProgress 验收路径同一处）。

`Upstream` 枚举随 pool.rs 删除**改名 `DialTarget` 落 intercept/mod.rs**（pool 时代
的旧名不留跨模块公开面）。

**pool.rs 不变量搬迁清单**（评审 R-14）：`MSG_NOSIGNAL`（裸写 flag——D-1 中-4：
主进程 SIGPIPE 已恢复默认处置，裸写会打死进程；实现处保留原注释）、
fcntl 建非阻塞（上述）、`VecDequeLite`、`READ_CHUNK=64KB`、`WATERMARK` 归一
（pool 副本删，mod.rs 常量为唯一）。

## 三、必答③ UDP 五元组会话入环：拨号同步化，pending 重放窗口坍缩为零（语义不变）

非阻塞形态下 UDP「拨号」= bind + connect——本地即时，循环内同步完成。⇒

- **窗口坍缩**：现状 pending 窗口 = 「首包 → 拨号线程 DialOk 回驱动」（异步线程调度
  ms 级）；新形态拨号在 `udp_new` 同一调用内完成，`on_plain` 也在驱动线程——
  **窗口内不可能到达任何后续包**（同线程串行）。pending 队列（`UDP_PENDING_MAX=16`
  丢最新）与 on_plain Dialing 分支的 UDP 臂删除；`Phase::Dialing{cache}` 对 UDP 仅
  在 udp_new → udp_ready 之间瞬时存在（DNS 腿的 submit 面继续从 cache 取首包）。
- **三段次序写死**（评审 R-4：Go 真源是「先写后记」，Rust 现状是「先记后写」——
  **保持 Rust 现状口径，不借对齐之名重排**）：
  ① `udp_seq += 1` + `stats.incr_flow()` + **E12 建立行** →
  ② 首包（cache 重放）入 `out_udp` 并立即试写 →
  ③ **首包写失败按 `finish_udp` 收口**（E12 关闭行 + `incr_udp_session(false)` +
  `decr_flow`——与现状「DialOk → udp_ready → Out → worker 写失败 → UpstreamEof →
  finish_udp」同序同串），**不走 DialFailed 路径**。
- **语义核验**（对客户端可观察面）：① 首包到 upstream 时序不变（更快且不乱序）；
  ② 窗口内后续包：现状缓存后按序重放直投 upstream，新形态无窗口、后续包经栈内
  socket → service_sockets 读出 → out_udp——同一 FIFO 序（栈内 socket rx 64 槽 ≥
  现状 pending 16 上限，突发容量只增不减）；③ 拨号失败（EMFILE/EADDRNOTAVAIL）：
  同步失败 → 现状 DialFailed 同路径同串（`incr_fail` **含 UDP**，见 §八）；④ 会话
  上限 4096/ICMP responder/idle 60s（DNS 10s）/E12 全套不变。
- **connected UDP 的 ICMP 回错**（macOS ECONNREFUSED 教训在案）：recv/send 非 EAGAIN
  错误 → finish_udp（§二 revents 表两条发现路径）。

TCP 的 Dialing 窗口**保留**（真网络 connect 异步）：SYN 缓存 ≤4、DialOk 注入
（SYN-ACK 由此产生）、失败 RST、黑洞 10s 三态逐字不动；cache 注入的收口函数 =
`dial_accept`（即时成功与 POLLOUT 验收两路共用，R-1）。

## 四、必答④ 公平性与饥饿：每拍处理全部就绪 fd，预算按协议分列

- **每拍处理**：poll(0) 返回的就绪集**全量**逐个处理（数组序来自 HashMap 迭代，
  跨拍不稳定——**不构成轮转语义，但无饥饿结论不依赖序**：就绪 fd 同拍必处理，
  未处理事件电平触发下拍重报；未就绪 fd 不消耗任何工作）。
- **每流每拍预算（按协议，评审 R-11 分列）**：
  - TCP/UDS 读 ≤ `WATERMARK − 现存 backlog`（256KB 门——读进 tx_backlog 才算数）；
    写 ≤ 待写缓冲存量。单流单拍上界 ≈ 512KB。
  - UDP 读 = drain 到 EAGAIN，上界 = **内核 rcvbuf**（默认 ~200KB 量级）——**现状
    既有上界**（in-stack socket 64KB tx 满即丢）。**上行门（评审 r2-低2 登记为等价
    迁移）**：旧形态 worker 的 `unacked > WATERMARK` 读门对 UDP 生效（作用点 = worker
    读 upstream fd，积压滞留内核 rcvbuf）；新形态统一为驱动侧栈读门（`pending_out`
    含 out_udp，积压滞留栈内 socket 64 槽 rx）——同为 256KB 积压后停止摄取、内存有
    界同；突发吸收面略降（64 槽 < 内核 rcvbuf），登记。
- **单流风暴有界**：水位门控读端 = 慢消费者自我降速（现状 Ack 清账同一稳态，少一次
  往返延迟）；超过 rcvbuf 的 UDP 到达由内核丢弃——与现状一致。
- **worker 饿死族结构性消失**：无 worker 线程即无「共享 runner 满载饿死拦截层工作
  线程」形态（mod.rs 测试头注释在案的 ubuntu CI 三轮红根因）。
- 定期扫描面（reap_idle/cc_stats/dial_fail_seen/wait_hint 刷新）O(flows)/拍——
  现状同为每拍全表扫，不新增（wait_hint 刷新是本批 +1 扫）。

## 五、必答⑤ 定时器面：无新原语，死线检查搭现有拍

| 定时器 | 现状 | 新形态 |
|---|---|---|
| TCP connect 死线 10s | `TcpStream::connect_timeout(10s)`（拨号线程内） | `dial_deadline` 字段；reap_idle 里**先判** `conn != None && now ≥ dial_deadline → on_dial_failed`（独立判定，不复用 last_active 的 idle 分支——评审§五附注），再跑 idle 扫描 |
| idle 回收（TCP 5min/UDP 60s/DNS 10s/TCP-DNS 30s） | reap_idle 每拍全表扫 | 原样 |
| smoltcp 栈定时器 | pump 内双 poll | 原样 |
| 整形续水/观测窗 | tx_shape_wait 两态 + Instant 差分 | wait_hint 扩档（§一），续水面原样 |

**不做 min-of-timers 定时轮**：引擎 poll 恒 ≤5ms 返回（wait_hint 1ms/5ms 两态），
所有死线的检查粒度 ≤5ms ≪ 最短死线 10s——定时轮是负收益复杂度。poll(2) 毫秒粒度
足够（无亚毫秒等待需求；亚毫秒面属发送线程批速，v0.2.2 已裁定归一 poll）。

## 六、必答⑥ 扩容后路：nginx 式 N-reactor 分片（只注记，不留缝）

单循环的容量上界 = 单核驱动（现状判定锚 = Go 出口 ±50%，R8 实测远超锚位）。若未来
需要分片（10k+ 并发流/多核卸载）：**分片线 = Flow.io 的 fd 归属**——按 fd 哈希把流
分到 N 条 reactor 线程（各持 poll 集与待写缓冲），与驱动线程之间只剩「smoltcp
socket 读写请求 + upstream 数据回投」一对队列——即回到候选 A 的形态，但**只在单
循环被实测证明不够时**才付这个复杂度（A 的交接面 bug 族就是代价表）。本批不实现、
不预埋抽象；保持 `Flow` 自含（io 状态不散落）使未来分片线干净即可。

## 七、循环内禁阻塞清单（本批第一评审硬项；评审 R-9 升格为「整循环」）

拦截层自身：

| 面 | 处置 |
|---|---|
| DNS 上游查询（阻塞面全长 2.5s/查询） | **保持在外**——DnsProxy 专用 worker 线程，`submit_*` 非阻塞投递 + DnsReply 回投通道（现状 H3 形态原样） |
| 阻塞 connect | **删除**——§二三分类非阻塞 connect |
| 文件 IO | 拦截层循环内零文件 IO（纯内存状态） |
| 同步 sleep | 拦截层循环内零；**收工宽限循环的 10ms sleep = 引擎侧既有形态保留**（宽限期 reactor 拍 = 宽限循环拍 ~100Hz——现状 worker 独立线程不受此限，10s 宽限下单流排空上限 ~25MB/s×10s，登记继承关系，评审 R-16） |
| 日志写入 | `logf` 行级 Mutex 包 writer（低频/降噪门控行，现状原样） |
| files/term/speedtest 服务面 | **保持在外**——UDS listener 服务线程原样；拦截层只以非阻塞 connect 拨它们的 UDS |

引擎循环（reactor 化后这些停顿**同时**停顿 upstream fd 读写——现状由 8 条 worker
吸收；逐条裁定，评审 R-9）：

| 面 | 频率/量级 | 裁定 |
|---|---|---|
| `fs::metadata(revoked.jsonl)`（吊销跟随） | 1/s，本地 state 盘 µs 级 | 低频有界可接受（登记） |
| `table.gc` | 10min ± 抖动 | 低频可接受 |
| `bind.send_wire`（判定+入队） | 每拍；P1 已把 sendto 移发送线程 | 既有面不变 |
| 5s 观测行/logf | 低频 | 既有面不变 |
| **剂量面** | — | §十 reactor 观测行（pumps/5s + 均拍 µs + 名下 fd 数）——停顿劣化可观测 |

## 八、判据行与计数不变清单（同串核验面；评审 R-8 改述）

`intercept: 过境拦截就绪（隧道IP %v；豁免=转投本机同端口；TCP 并发上限 %d）`（E5）、
`intercept: tcp %s %v ← %v（dialok）`（E10）、`… 关闭`（E11）、
`udp intercept: 会话 #%d %s 建立/关闭（…）`（E12）、tcp/udp 拒绝行、拨号失败降噪行
（首行 + 每 100 次）原样。Stats 六键 `[dialok, dialfail, flows, rejected,
udpReplied, udpNoReply]` 键序原样；AtomicU64 保留（udpcap 线程跨线程读）。**计数
口径**：`dialok` 仅 TCP（INTEROP-CRITERIA :197-198 只约束 dialok）；**`dialfail`
含 UDP**（建端点/开 socket/首包写失败——Go 实现与 Rust 现状一致同形；Go stats.go
注释「仅计 TCP」与其自身实现不符，两实现同形即现状，不借本批「整改」）。dns/整形/
cc 观测行原样；新增 reactor 观测行（verbose 5s 面，非启动判据——E5 行文案不动）。

## 九、删改清单与简化收益度量

- **删**：`pool.rs` 全文件（619 行：Worker 池/双通道/唤醒管道/Adopt/dial 线程/
  IntoRawFdArc/pool_end_to_end_uds 测试）；mod.rs 的 `events`/`on_event`/
  `maybe_reap`/Ack/Written 全站点/`unacked_out`/`forget_flow`/worker 选择算式；
  **`drain()` 兼容面 + `linger_rst` 参数 + SO_LINGER 面**（评审 R-15：drain 全仓
  零调用者、`teardown_flow(_, true)` 无生产路径——「到期 RST 收口」Rust 侧从未接线，
  close() 一直走 FIN teardown；既有差异在 R3-design §4.1 M13 登记注记，本批删除
  死代码而非补接线——补接线属行为变更）。
- **增**：`ReactorIo`（OwnedFd/双缓冲/ConnState）、非阻塞 dial（三分类）+ 验收
  （dial_accept 统一收口）、兴趣集纯函数 + pump 内 poll(0) + 就绪分发、`wait_hint`、
  reactor 观测行。
- **测试面**（评审 R-7：四 E2E 原样过 ≠ 新机制有测试网——删 pool_end_to_end_uds
  后必须补）：① **UDS 豁免腿 E2E**（local_services 映射 → UDS echo：即时 connect
  路径 + dialok + 数据往返——正是 R-1 形态的回归钉）；② **flush_out EAGAIN 语义单元钉**（socketpair 自设小
  SNDBUF + 对端不读：flush_out 必须在预算内返回且余量保留——看门狗 abort 兜底，
  高-1 的直接回归钉；E2E 面为「停读→开读→按序完整」行为钉——本机 sndbuf 自动调
  过大，E2E 层逼不出确定性 EAGAIN）；③ **fd 收口断言**
  （`#[cfg(test)] reactor_fds()` 计数：exempt/dial-fail/udp/close 四路径跑完后归零
  ——R-3 规则表的机器验收）；④ **兴趣集纯函数单测**（水位摘 POLLIN / InProgress
  只 POLLOUT / Retry 无位 / dead 只 POLLOUT）。
- **度量**：净删行数 = 本批 diff 统计，入册 ROADMAP 收口节——「保持代码最为简洁」
  的兑现度量。线程账：拦截层 8 worker + 暂态拨号线程 → 0。

## 十、验收网

① 单测/E2E：`cargo test --workspace`（§九 四条新测 + 既有 exempt/dial-fail/udp/
DNS 四 E2E 面语义断言原样过）；② harness 有损链路三臂（`--ignored` 串行：
downlink_lossy_link_recovery A/B + cold_air_form_verification + pacing_three_path_arms
——吞吐门自校准；**cold_air 的 `air_dropped ≤ 1/10000` 是绝对门且机制依赖拍间隔**
〔团块 = rate×拍间隔，reactor 进 pump 每拍多 poll(0) 会使 dt 变粗〕——列为**复测项**
而非口号，6 流规模下余量预计充足，红了按剂量面归因，评审 R-16）；③
`tools/ci-local.sh` quick 全绿（clippy -D warnings + OHOS 交叉 check + 向量/词表门 +
RRR 矩阵冒烟）；④ 真机烟囱：v0.2.3 滚动后手机自动重连 → E5/E10/E11/E12 同串 +
Stats 计数推进 + `stats:` fdRead/fdWrite 双向增长（坑 1 真凭据判据）+ reactor
观测行剂量核阅。发版 = v0.2.3（B0-2a 手法；阿里云 **linux-amd64**〔v0.2.2 拿错
arm64 的实录教训〕）。
