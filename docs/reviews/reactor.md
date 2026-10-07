# 拦截 reactor 简化批 · 评审记录

## 第一道门：设计评审（dsh headless，2026-10-07）

评审对象：`docs/reviews/reactor-design.md` v1（评审时实装未开始）。评审者独立读
reactor-design v1 / R3-design 语义基线 / pool.rs+mod.rs+engine.rs 现状代码后的结论：
**方向成立，六问裁定自洽（必答①⑤⑥与 §二缓冲/关闭/§三窗口/§七保持在外等八项
「看过，没发现问题」）；但需修订后放行**——1 高 / 7 中 / 6 低，修订面集中，不动
六问裁定本身。v2 = 整改后版本（即实装基线）。

### 处置表（v1 → v2）

| # | 级 | 问题（原文要义） | 处置（v2 落点） |
|---|---|---|---|
| R-1 | 高 | 非阻塞 connect「立即成功」（UDS 豁免腿常态、TCP 回环偶发）无验收点——只挂 POLLOUT 验收 ⇒ 流卡到 10s 死线才 DialFailed，files/term/speedtest 豁免腿全灭；附 std 陷阱（std connect 把 EINPROGRESS 当 Err） | **接受**。v2 §二：拨号结果即时成功路径与 POLLOUT 验收路径**统一收口 `dial_accept`**（= 现 on_dial_ok）；conn 唯一清零点 = 验收函数；EINTR 即时重试 / EISCONN 视同成功 / EALREADY 视同 in-progress；「禁止 std connect/connect_timeout」写死。§九补 UDS 豁免腿 E2E 回归钉 |
| R-2 | 中 | UDS 监听队列满：Linux 非阻塞 connect = EAGAIN（阻塞形态内核会让等）、macOS = ECONNREFUSED（与无监听同错）——分类不写清实现自由发挥 | **接受**。v2 §二错误三分类表：in-progress（EINPROGRESS/EALREADY/EINTR）/ retry（EAGAIN：每拍主动重拨、共用死线、**不挂 poll 位**——未连接 socket 恒可写，POLLOUT 等不到有用信号）/ fail；平台注记 macOS=ECONNREFUSED 属 fail 且现状阻塞形态亦如此（非漂移） |
| R-3 | 中 | `dead`（upstream EOF）路径 fd 关闭点与「remove_flow drop 恰一次」互斥——EOF 后 fd 应立即关（排空即收、流记录留栈侧收尾），且拨号失败/死线/首包写失败路径 fd 归属没落字 | **接受**。v2 §二：`fd: OwnedFd`（类型承担不变量——恰一次由 drop 保证）+ fd 关闭规则表三行（EOF∧排空=立即关+io=None 保留 Flow；拨号/验收/死线失败=remove_flow；teardown/close=remove_flow）。§九 ③ fd 收口断言（reactor_fds() 四路径归零） |
| R-4 | 中 | UDP 同步化后「建立行/会话号 vs 首包发送」次序成实现自由——Go 真源「先写后记」、Rust 现状「先记后写」，照 Go 重排 = 可观察面漂移（首包写失败少一行建立/关闭/udpNoReply） | **接受**。v2 §三三段次序写死：①会话号+E12 建立 → ②首包入 out_udp 试写 → ③**写失败按 finish_udp 收口**（不走 DialFailed）；注明「Rust 现状口径、与 Go 真源不同序，不得借对齐之名重排」 |
| R-5 | 中 | `reactor_waiters` 在 pump 开头算 ⇒ 引擎下一轮读到上拍开工前状态——本拍 service_sockets 新攒的 out_buf（files 上传卡死族形态）要多等一个 5ms 拍 | **接受**。v2 §一：计算时点 = **pump 返回前**重扫缓存（O(N) 与兴趣集构建同级、自愈式重算、非增量计数） |
| R-6 | 中 | 「每拍 2 次 poll 仍少于现状」口径不成立——现状 worker 是 1000ms 阻塞 poll（事件驱动不占驱动拍），新形态每拍 O(N) 全扫且全在驱动线程关键路径；§四「轮转」措辞不实（HashMap 迭代序不稳定） | **接受**。v2 §一表格改诚实口径（净增成本面）+ 缓解（pollfd Vec 复用）+ §十剂量面（reactor 观测行：pumps/5s、均拍 µs、名下 fd 数）；§四措辞改「每拍处理全部就绪 fd，无饥饿结论不依赖序」 |
| R-7 | 中 | 新失败面缺测试网——「四 E2E 原样过 = 行为不变」证明不了非阻塞验收/部分写续写/水位门控/revents 分发/fd 恰一次；删 pool_end_to_end_uds 后唯一直接钉 fd 收口的测试没了 | **接受**。v2 §九测试面四条：①UDS 豁免腿 E2E（即时 connect 路径 = R-1 回归钉）②部分写续写 E2E（慢读对端 EAGAIN 余量最终完整按序送达）③reactor_fds() 四路径归零断言 ④兴趣集纯函数单测（水位/InProgress/Retry/dead 四态） |
| R-8 | 中 | 「dialok/dialfail 仅 TCP」复述与两个实现都不符——Go 实现给 UDP 失败也计 dialfail（stats.go 注释与实现不符），Rust 现状同；照它改 = Stats 判据面漂移 | **接受**。v2 §八改述：dialok 仅 TCP（判据面只约束它）；**dialfail 含 UDP**（两实现一致同形即现状，不借本批整改）；UDP 同步化显式保留 incr_fail |
| R-9 | 中 | 引擎循环既有停顿（revoked stat 1/s、gc、logf Mutex、观测行）现在直接压 reactor IO——现状由 8 worker 吸收（照常读进无界通道），新形态无接管 | **接受**。v2 §七升格「整循环」清单：engine 侧面逐条裁定（低频有界可接受、登记）+ §十 reactor 观测行为剂量面（停顿劣化可观测） |
| R-10 | 低 | 兴趣表缺 revents 分发规则——macOS connected UDP 的 ECONNREFUSED 正是 POLLERR-only 形态，没这条规则 UDP 会话挂到 idle 60s | **接受**。v2 §二 revents 分发表（pool.rs 规则原样搬：POLLIN 读/POLLOUT 写或验收/ERR·HUP 无 IN → EOF/NVAL 防御）+ UDP 两条发现路径注明 |
| R-11 | 低 | 「每流每拍 ≤256KB」对 UDP 不成立——水位门挂 tx_backlog（TCP 专用），UDP 恒空，上界实为内核 rcvbuf | **接受**。v2 §四预算按协议分列：TCP ≤ WATERMARK 门；UDP = 内核 rcvbuf（现状既有上界，加预算属行为变更不掺结构批） |
| R-12 | 低 | 「单测零改动」自相矛盾——tx_shape_wait 改 wait_hint 必改单测与 engine 一行 | **接受**。v2 §一表格改述（pump 族签名不动；wait_hint = engine 一行 + 单测同步扩档） |
| R-13 | 低 | `fd: RawFd // -1 哨兵` 与 `Option<ReactorIo>` 二义 | **接受**。v2 §二：OwnedFd + 删哨兵注释（io=None 即无 socket，DNS 进程内腿即此） |
| R-14 | 低 | 删 pool.rs 带走的不变量没进「增」清单：MSG_NOSIGNAL（裸写会打死进程）、fcntl 建非阻塞（SOCK_NONBLOCK 在 libc apple 目标未定义）、Upstream/VecDequeLite/常量 | **接受**。v2 §二「pool.rs 不变量搬迁清单」+ Upstream 改名 DialTarget 落 mod.rs |
| R-15 | 低 | `drain()` 全仓无调用者、`teardown_flow(_, true)` 只在 drain() 内 ⇒ linger_rst/到期 RST 收口是无生产路径的死分支——与净删目标相反 | **接受**（复核成立：grep 零调用者）。v2 §九：删 drain() + linger_rst + SO_LINGER 面；R3-design M13「到期 Close{linger_rst} RST」登记「Rust 侧从未接线、close() 走 FIN」既有差异（补接线属行为变更，不掺结构批） |
| R-16 | 低 | cold_air `air_dropped ≤ 1/10000` 绝对门依赖拍间隔（团块 = rate×拍间隔）——reactor 每拍多 poll(0) 使 dt 变粗，应列「复测项」；宽限期 reactor 拍 = 宽限循环拍（~100Hz）的继承关系应写明 | **接受**。v2 §十：cold_air 列复测项（红了按剂量面归因）+ §七宽限拍继承注记（10s 宽限 × ~25MB/s 单流排空上限，够用） |

### 评审明确「看过，没发现问题」的方面

必答①（候选 B 裁定与 R3-design §1.1 的演进关系/不合流引擎 poll 的取舍/发送线程边界）、
必答⑤（不做定时轮、dial_deadline 搭 reap_idle）、必答⑥（只注记不预埋、分片线 = fd
归属）、§二缓冲拷贝 2→1 论证、关闭路径删 Close/Closed/forget_flow（结构性消灭收工
竞态）、§七保持在外清单、§四 worker 饿死族消失、§八判据行面（除 R-8 口径）。

## 第二道门：代码评审（dsh headless，2026-10-07，实装 commit f965009）

评审者独立读 diff + 设计文档 + 新旧实现对照（含自建 worktree 复现实验）。结论：
结构收编干净、R-1..R-16 绝大多数逐条兑现、判据行机械比对同串（19→21 行，新增仅
reactor 观测与 POLLNVAL 防御两行）；**1 高 / 1 中 / 9 低**，十六个「看过没问题」面。

### 处置表

| # | 级 | 问题（原文要义） | 处置 |
|---|---|---|---|
| 高-1 | 高 | **flush_out 在 send=EAGAIN 上原地自旋**（WouldBlock 归入「非 eof」→ 无 break → 缓冲不变再 send）——驱动线程永不返回 = 整出口挂死；旧 worker flush_flow EAGAIN 即返，本批新引入。评审者独立复现（对端停读 + 2MB 载荷，pump 700KB 处锁死） | **修**。三态步进：Wrote 续写 / **Full(EAGAIN) break 等 POLLOUT** / Dead 先清待写缓冲再 EOF。单元钉 `flush_out_eagain_returns_with_backlog`（socketpair 小 SNDBUF + 对端不读 + 看门狗 abort）——反向验证闭环（注入 Full→continue 必红 abort，恢复绿）。顺带教训：**本批第一硬项「禁阻塞」的字面检查全过，但忙等自旋比阻塞更糟**——单线程后单点自旋 = 整机挂死 |
| 中-1 | 中 | 硬写失败（EPIPE/ECONNRESET）+ 待写缓冲非空 ⇒ `dead∧排空` 永不成立——fd 滞留至 5min idle + 每拍注定失败的 send + wait_hint 恒 1ms 空转 | **修**。flush_out 的 Dead 步进先 `discard_out`（余量静默丢 = 旧 worker「EOF 后不再写」口径）再 on_upstream_eof；设计 §二 fd 规则表补「硬写失败」行 |
| 低-1 | 低 | UDS `sun_path` 上界写死 108（Linux）——macOS 是 104B，104..107B 路径越界写（栈 UB；装配面 <100B 守卫使其当前不可达） | **修**。`>= ss.sun_path.len()` + 长度自洽（offset_of + len + NUL） |
| 低-2 | 低 | 上行水位门对 UDP 新生效（旧 unacked_out 只对 TCP 累加；但旧 worker 侧读门对 UDP 生效） | **登记为等价迁移**（保留实现）：门从 worker 读门挪到驱动侧栈读门（统一 pending_out 含 out_udp），同为 256KB 积压停摄取、内存有界同；突发吸收面略降（栈内 64 槽 < 内核 rcvbuf）——设计 §四 R-11 行更新 |
| 低-3 | 低 | 下行读门 `> 256KB` 收紧为 `≥`（差一个读块） | 记录不改（量级内、内存更省；service_sockets 侧同旧操作符） |
| 低-4 | 低 | reactor 观测行「均拍」实为 pump 周期非单拍耗时；空载也打行 | **修**。标签改「均周期」+ `fds>0` 门控 |
| 低-5 | 低 | poll_index 的 fd 分量冗余；DialTarget pub+derive 超需；chunks 中间 Vec 三拷 | **修**：poll_index→Vec<u64>（并删 RawFd import）、DialTarget 收可见性、非 DNS 腿边读边 push（每方向 1 拷对齐设计 §二）。retry 的目标重推导分配保留（低频路径） |
| 低-6 | 低 | set_fd_nonblocking 吞 fcntl 失败——「禁阻塞」唯一理论破口 | **修**。两次 fcntl 校验，失败按拨号失败收口 |
| 低-7 | 低 | UDP「connect 恒即时成功」前提未设防（SOCK_DGRAM 上 EINPROGRESS 不可达但未写死） | **修**。dial_nonblocking 文档前提写死（fail 收口口径） |
| 低-8 | 低 | 「conn 唯一清零点」与实装不符（retry 也清） | **修**。注释口径改「验收与重拨成功两处清零、都须立即 dial_accept」 |
| 低-9 | 低 | 部分写测试强度不足（与高-1 同源漏网：对端只停 300ms 且 macOS sndbuf 默认吞下 128KB——EAGAIN 可能从未触发） | **修**。单元钉直测 EAGAIN 语义（见高-1）+ E2E 改「停读→开读→按序完整」行为钉；本机 sndbuf 自动调优过大（4MB 灌不动）实测入册 |
| 存量 | — | 评审者另发现（HEAD~1 既有，非本批）：①`udp_seq_of` 从未赋值——E12 关闭行恒「会话 #0」（M4 评审声称打本会话号）；②`dial_accept` listen 失败路径与 close() 对未 incr_flow 的流调 decr_flow 可能下溢 | **登记不修**（语义零改动边界：①改了会变更判据行字节；两者均为既有形态，移交后续批） |
| R-16 复测 | — | cold_air 三臂 | 负载机：本批与 v0.2.2 基线同红同形（丢 51/34 vs 54/50 包，吞吐持平偏好）——负载敏感；**评审者静默机复跑本批 = 绿（25.8s）**。结论：非本批回归，门对机器负载敏感（R-16 注记成立） |

### 验收面（评审者实测 + 本会话补充）

reactor 相关 8+1 测全绿；harness A/B 臂绿（B 臂 3.5MB/s 高于健康带）；三径臂慢臂
高方差两轮（1.1/0.5 红 → 0.5/0.7 绿；off 臂自身波动 2 倍——按 R8-2「单轮不可 A/B」
纪律无回归定论）；ci-local quick 全绿（含 RRR 矩阵冒烟）。整改后全量
`cargo test --workspace` 435+ 绿、clippy -D warnings 绿、OHOS 交叉 check 绿。

整改 commit：见 git log「拦截 reactor 简化批 Rc-1」（flush_out 三态 + discard_out +
单元钉/测试重写 + 低项批量 + 文档落档）。

