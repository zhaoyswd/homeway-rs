# r5 实现与交叉复核原始记录

各条为子任务当时的原始判断；并行开发期间的临时编译诊断与待补项，最终处置以主整改记录为准。独立复核发现的并发收工与 hostname 兼容问题均已修正并补回归。

## DoH 技术门

# DoH 整改技术门（2026-10-21）
设计：以 DeadlineTcp 持有 TcpStream、绝对期限与借用停止位，底层 Read/Write 在每次 syscall 前后检查总期限，并按50ms剩余量重设 timeout；rustls 内层握手和记录读循环同样受约束。候选查询期限=min(整轮期限,now+3s)，按查询类型在候选内过滤。显式全坏DoH不回退系统，空配置仍保留旧语义。DNS wire 改类型错误，HTTP增加有界chunked/EOF body。
技术评审：PASS WITH ACTIONS
对齐：TLS根校验不改、默认供应商不改、DNS wire query不改、非公网污染闸不改。
风险：connect轮询必须跨平台稳健；采用socket2 connect_timeout一次调用，共享剩余期限，读写取消片段50ms，connect取消延迟最多单候选3s。HTTP framing须拒TE/CL歧义及未知transfer coding、所有响应字节总计<=64KiB。
行动：真实rustls慢握手/慢记录回归；qtype后备逻辑与过期/停止位测试；记录connect取消的有界限制给主会话。
惯用Rust检查：借用AtomicBool、不增加Arc/Mutex、不引入外部依赖；wire错误thiserror。


## DoH 代码门

# DoH 整改代码门（2026-10-21）

判定：PASS（自主结构化技术/代码复核；未调用 dsh）。技术门先于实现记录于 `/tmp/homeway-doh-design-review.md`。

## 变更与不变量
- `server/deadline_io.rs`：共享 DeadlineStream<TcpStream>，借用 AtomicBool；每次 TCP read/write/flush 前后检查绝对期限与停止位，syscall timeout=min(remaining,50ms)，超时仅在同绝对期限内重试。停止返回 ConnectionAborted，避免标准库/rustls 对 Interrupted 的隐式重试。
- `server/doh.rs`：每候选期限=min(整轮deadline,now+3s)，resolve4_before/resolve6_before 传透停止位；rustls 直接持有 DeadlineStream，TLS 握手和记录内部循环也受限。证书主体、SNI、webpki-roots 校验未改变。
- 所有平台钉卡都调用 egress::pin_socket_to_iface，包括 Linux index=0/name有效，消费层不再提前跳过。
- 查询类型过滤移入候选循环；错误族/IPv4-mapped AAAA 等于该候选NODATA，可继续下一台。
- DNS wire build_query/parse_answers/skip_name 改 WireError 类型，并补末尾压缩指针/越界 label 检验。
- HTTP framing 支持 chunked（含有界扩展与 trailer）、close-delimited EOF；头/编码体/trailer 总量<=64KiB；拒绝TE+CL、重复/非法CL、未知TE、chunk坏长度/截断。所有返回成功前再检查deadline。
- 显式全坏DoH配置 fail-closed，不回系统；候选耗尽/停止/预算触发统一可见日志；默认供应商不改变。
- 显式空DoH保留系统解析语义，但经 OnceLock 的进程级唯一worker + sync_channel(1)，前台等待每50ms检查stop/deadline；Busy/timeout日志可见。底层getaddrinfo无法取消，最多1个常驻lookup+1个排队请求；worker不持TokenCtx/实例/socket，不随重启增加线程。

## 复核发现与处置
1. 停止错误若为Interrupted，会被write_all/rustls重试 → 改ConnectionAborted，加入write_all无发送/即时失败回归。
2. TLS慢记录测试若服务端只调用一次write_tls可能只写1字节后EOF → 循环wants_write，确保真滴流；慢记录分支断言客户端先完成可信TLS握手，防证书错误制造假阳性。
3. HTTP已有缓冲区的解析可能跨deadline后返回成功 → 外层统一成功出口二次检查。
4. 显式空配置的系统解析仍无界阻塞pubep → 按主会话裁定改唯一worker+有界队列，不使用每次请求spawn。
5. 字符串wire错误残留一处 → 编译检查检出并改WireError::Truncated。
6. 惯用Rust检查：无逐行移植；原有Arc/Mutex只用于iface；取消借用，wire错误thiserror，解析数据按借用切片，唯一worker的通道所有权清晰。

## 验证
- `cargo test -p homeway-core --lib doh --no-fail-fast`：11 passed, 0 failed, 1 ignored（手动外网验证）。含真实rustls慢握手与慢加密记录、候选族过滤与mapped后备、系统worker有界队列/超时/停止、HTTP chunked/EOF/歧义和超限。
- `cargo test -p homeway-core --lib deadline_io --no-fail-fast`：2 passed, 0 failed。含挂起TCP读的预算/取消、停止write_all不重试/不发送。
- `cargo clippy -p homeway-core --all-targets -- -D warnings`：当时唯一失败为其他并行子任务 ddnscheck::run_ddns_self_check_cancellable 尚未接线 dead_code；本任务文件无warning。最终总门由主会话汇总。

## 明确限制
- TCP connect_timeout 是一次有界调用，停止最长等到该候选剩余预算（<=3s），connect前后都检查门。没有引入跨平台手写connect状态机。
- getaddrinfo本身不可取消，进程级唯一工作线程可能卡在OS，但前台观测线程在整轮deadline/stop内返回；队列容量1，队列忙立即失败，过期排队项跳过。
- 手动外网钉卡测试未运行；此次回归均用本地受控TCP/TLS，不碰生产。

最终补验：`cargo clippy -p homeway-core --lib -- -D warnings` 已通过；all-targets第二次被并行UPnP新增测试unused_io_amount阻断，已通知主会话修复。强化TLS回归要求错误Display必须含“总预算耗尽”，确实由绝对期限截断而不是普通TLS/EOF错误；`cargo test -p homeway-core --lib server::doh::tests::tls_handshake_and_record_trickle_respect_deadline -- --exact` 实跑1例通过（0.41s）。`git diff --check` 本任务跟踪文件通过。

UPnP兼容性复核补修：共享 `pub(crate) doh::resolve_system(name, deadline, stop) -> Result<Vec<IpAddr>, SystemResolveError>`，直接返回未过滤原始地址，UPnP LAN解析复用同一个进程级worker与有界队列。DoH公网调用方继续自行过滤污染地址。新增 `shared_system_resolver_preserves_lan_addresses`，锁定共享层允许127.0.0.1，不误用公网污染闸。


## 引擎技术与代码门

# 公网观测 engine 修正技术门与代码门（2026-10-21）

范围：仅 `crates/homeway-core/src/server/engine.rs`。不提交，不动生产。不调用 dsh；按主会话要求做实现者/评审者分阶段结构化自评。

## 技术门（实施前设计，实施后核对）

1. F2 根因不是缺一个 load，而是停止和提交之间缺少线性化点。新增每世代 publication 门，同一事务完成 cache / probe 列表 / last_published / token 台账追加；fallback 同样经门，并在门内重读当前公布快照，避免旧fallback快照回写。shutdown先置取消再跨过门，返回后旧事务绝不再写持久面。
2. 任何 logger 都不得在发布门内执行。用 DeferredLogs 缓冲日志，释放门后回调；停止后丢弃尚未回调的旧世代日志。没有把网络探测包进门里，门只保护本地提交面。
3. 保存 pubep / tokfb 句柄并可唤醒、取消、join。外部 stop_public_observation 返回才可缩租。与主会话 UPnP/DoH/DDNS 可取消接口共同完成退出闭环。
4. 并发调用不能把句柄 take 误认为线程完成：外部观察停机持 observers 锁直到 join 完成；完整 shutdown 另有 completion 锁，覆盖 driver/relay worker 释放。immutable observer ThreadIds 识别观察回调重入，仅请求取消/Stop，不等锁、不自 join。只有外部 shutdown 返回才承诺完整收工；CLI均为外部调用。
5. F3 输入与回包共用地址族校验，mapped-v4 / 错族 / 污染地址不能截断后备。IPv6 无 v4 NAT，公布真实监听口，与 v4 UPnP 外口分离。
6. 第一轮观测黑洞也不能无限阻塞 token：首轮信号与15s独立兜底竞争；等待/重试均可取消。UPnP+DoH+STUN共享60s整轮deadline，STUN每候选仍为4s，等待50ms片检查停止。DDNS在首轮信号之后，使用其独立可取消15s预算。
7. relay worker 接口由 fix_relay 提供，engine保存两路 worker；spawn半边失败立即stop+join成功半边，shutdown等待驱动后stop+join两路，消除公共UDP clone悬留。

评审结论：方案收敛为事务提交与资源所有权两条不变量；没有仅堆叠停止位检查。可继续实现。

## 代码门（完工后结构化自评）

- 对齐完整性：未改wire/注册/准入；保留原公布与token日志正文；改变预算与v6端口的行为交主会话登记判据变更。
- 并发：锁序 external shutdown_completion → publication短屏障 → observers/join；发布路径 publication → inner，所有logger均在publication外。观察线程重入在任何等待锁之前早退，仅发停止请求，防外部join等待日志回调构成环。
- 持久面：manual / inferred / fallback全部走发布门；停用后直接helper调用也无追加。缓存写失败仍按既有fail-visible语义继续token提交。
- 边界：STUN6 IP字面量mapped过滤；回包实际地址族过滤；v4外口49999且本地42650时，v6端点42650。deadline已过不发新STUN。
- 资源/错误面：spawn失败保留降级行为，成功半边worker立即释放；pubep线程起失败由首轮信号断开回退15s；公共端口释放等待所有relay clone收尸。
- Rust形态/直译痕迹：新增Mutex分别代表发布事务、join完成、完整收工三个实际不变量；沿用既有Arc stop位；未新增字符串错误/API仿写/包结构迁移；DeferredLogs只用于低频发布，并允许FnMut错误sink避免复制写盘实现。没有Go逐行直译。
- 非目标/限制：观察回调内shutdown只有请求语义，须外部shutdown才能确认释放；CLI已由主会话处理次序。非本文件的可取消I/O质量由对应实现者与主会话评审。

结论：通过，无未处置高/中风险。

## 回归证据

最终命令 `cargo test -p homeway-core server::engine::tests --lib`：26 passed, 0 failed（0.73s；2026-10-21）。

新增5例：

- `stopped_refresh_cannot_append_after_replacement_from_logger_callback`：真实manual refresh，cache写失败logger暂停，外部停旧世代并发布新世代，再恢复旧logger；新cache与台账末行保持最新，旧fallback/helper也不能追加；回调期间publication.try_lock成功。
- `inferred_refresh_skips_mapped_v6_and_wrong_family_reply_and_uses_listen_port`：生产推断主路径注入已取得UPnP外口证据，候选序mapped-v4→真v6但回包v4→真v6回包；mapped不发包，错族不结束后备；v4外口49999/v6监听42650各自正确。
- `stun_wait_respects_cancel_and_round_deadline`：在途STUN等待stop后小于1s返回；超期不投递新查询。
- `first_probe_blackhole_is_bounded_and_cancellable`：持有发送端但不发首轮信号，80ms测试预算独立到期；停止/驱动死亡立即取消。
- `shutdown_with_relay_releases_public_udp_port`：真实引擎/QUIC/双relay worker（私有loopback、无外网）；真实pubep logger暂停，两外部线程分别stop_public_observation与shutdown均不得提前返回；观察logger重入stop+shutdown不死锁；全部退出后公共UDP立即同地址重绑。

现有21项engine回归同时通过。check编译通过（并行开发期间出现的其他文件临时接口错已随对应agent完成消失）。未触达生产，无网络公网对端依赖。

## 末轮复核补记

- 分离 `stun_family_matches` 与污染段判断，错族回包准确打印 `STUN[6]：{label} 观测到 {ap} 地址族不符——换下一个候选`，不再误报污染段；推断回归同时断言这条诊断。
- 最终稳定源重跑：`cargo test -p homeway-core server::engine::tests --lib`：26 passed，0 failed，0.59s。
- `git diff --check -- crates/homeway-core/src/server/engine.rs`：通过。
- 修改范围保持engine.rs一文件；root负责跨文件API/CLI次序/文档与全量门。


## 中继技术与代码门

# F4 中继后台线程生命周期整改

## 技术门（开工前结构化自评）

实现者：问题来自 spawn 丢弃 JoinHandle；UDP 注册腿持有公共 socket 的 dup，stop 位不能唤醒 recv_timeout(5s)，导致引擎收工后仍占公共端口。方案为 RelayWorker 类型持有线程、停止位与 UDP 事件唤醒 sender，stop_and_join 设置停止、非阻塞投 Stop、join；Drop 同样兜底，覆盖后续构造失败。控制面也返回相同类型，收工后无独立旧命令发送者。

评审者：协议字节、注册/保活/重试常量与日志判据不变；只增 Stop 内部事件。Full 队列本身已有事件，recv 返回后 stop 检查即可退出，禁止阻塞投递 Stop。控制面向 engine 投命令为无界 sender，不等待回执，所以驱动先收工与 worker 后 join 无循环等待。控制连接已有有界拨号/读/写，join 保证结束。无需新增 Arc/Mutex（复用现有 stop Arc，只clone到worker所有者）。风险为 engine 必须保存成功 worker，并在 spawn 失败时收工成功半边，已与 fix_engine 同步接口。

## 实现

- spawn_relay_leg(events=(SyncSender,Receiver)) 与 spawn_relay_ctl 均返回 io::Result<RelayWorker>。
- RelayWorker::stop_and_join(self) 与 Drop 都将 stop 置位；UDP 用 try_send Stop 唤醒；Option<JoinHandle> take 后 join，重复收工幂等。
- UDP 初次 Hello、事件处理与重试前检查 stop；TCP 握手后清腿表与读完消息后检查 stop。
- engine 集成由 fix_engine 负责，未在本子任务改动。

## 回归

server::relayleg::tests 新增：
1. relay_leg_stop_joins_and_releases_udp_duplicate：回环中继真实收到 Hello；原 socket 关闭后仍不能重绑；钩子 sender 保持存活；stop_and_join <2s；返回后重绑同端口成功，事件接收者已关闭。
2. relay_leg_stop_handles_full_event_queue：预置满事件队列，将工作线程控在启动日志处；另线程 Drop，观察停止位确实置位后放行；Drop 路径也必须完成线程回收与端口释放。
3. relay_ctl_stop_joins_silent_connection：私有回环 TCP 完成 HELLO/CHALLENGE/PROOF/OK；收到 LegsClear 后保持静默；stop_and_join 后命令通道断开、TCP 对端收到 EOF。

首次 cargo test -p homeway-core server::relayleg::tests --lib：共享树正在并行编辑 engine/doh/upnp，编译失败为其他文件未完成符号/字段/borrow；relayleg 无编译诊断。稳定后重跑结果待补。

## 验证终态

- `cargo test -p homeway-core server::relayleg::tests --lib`：5 passed，0 failed，1.01s（2条既有解析回归 + 3条新增生命周期回归）。
- `cargo test -p homeway-core shutdown_with_relay_releases_public_udp_port --lib`：1 passed，0 failed，0.63s（fix_engine 的引擎整体回归，确认 shutdown 返回后公共 UDP 端口立即可重绑）。
- `git diff --check -- crates/homeway-core/src/server/relayleg.rs`：通过。

## 代码门（完工后结构化自评）

评审者结论：通过。所有线程句柄均有明确所有者，显式收工与 Drop 均 join，退出期间不会遗留 UDP dup；Stop 投递非阻塞，满队列已有事件可唤醒，stop 守卫阻止继续处理排队控制帧。控制线程不会等待 engine 回执，驱动先 join、后台线程后 join 无环；正常生效期协议字节/地址族转换/重试与保活常量/日志判据未改变。TCP 的 stop 轮询与既有读写/拨号超时保持原行为，join 等待线程真正完成。回归使用 127.0.0.1 动态端口，无外网/生产依赖。Go 直译痕迹检查：采用 Rust RAII 的 Drop + Option<JoinHandle>，没有新增字符串错误、生产 Mutex、无谓拷贝或平行包结构。醒目限制：控制面停止可能仍等待当前已有的有界 socket I/O；本修复保证收工返回前终止，不新增硬实时退出承诺。


## 独立交叉复核

# 公网观测整改独立代码门复核

范围：当前 engine.rs 的发布门 / 观察线程收工 / relay 集成，upnp.rs 的取消与 hostname 连接策略，ddnscheck.rs 取消，以及 CLI 缩租顺序。仅代码复核，未改被复核文件；位置随并行格式整理可能变化，以函数锚点为准。

## 确认问题 1（P2）：并发 stop/shutdown 的第二调用会提前返回

位置：engine.rs `stop_public_observation`，读取时约 1214-1219 行（`let observers = std::mem::take(...); for handle in observers { ...join... }`）；`shutdown` 同类 `let handle = self.driver.lock(...).take()` 约 1195-1198 行。

具体交错：A 调 stop_public_observation，把 observers 移到局部后释放句柄锁，开始 join 正在结束一次 socket I/O 的旧观察线程；B 此时也调用 stop_public_observation，获取空 Vec，不等待 A 的 join 即返回。B 作为 CLI 的缩租路径会立即开始缩租，而 A 尚未证明旧观察线程退出。对 shutdown 也一样：A 取 driver 句柄但仍在 drain/QUIC stop，B 从空 Option 跳过 join，其 shutdown 可在旧 driver/公共 socket 仍存活时返回。外层调用者若据返回值开始同端口重建，可能退让端口。

建议：以明确的 Running/Joining/Done 完成态协调 join，普通重复调用必须等待完成；不可只把句柄取空当作 Done。注意观察线程日志回调自己调用 shutdown 的现有支持：不能简单持 observers 锁 join 后让自身回调抢同一锁，也不能令自收工等待别的线程对自己的 join；需针对本线程 worker 的重入保留免自等待路径，并让外部调用仍能等待该线程真正退出。

## 确认问题 2（P2）：hostname 解析被全局移除会破坏既有合法调用

位置：upnp.rs `connect_within` 约 591-593 行：非 IP host 一律退到 `context.hostname_addr`，无来源时直接 UnresolvedHost；`igd_from_location` / `igd_from_location_deadline` 约 695-700 行生成的 context 没有 hostname_addr。

具体触发 1：现有公开入口 `igd_from_location("http://localhost:<真实HTTP端口>/desc", budget)` 以前通过 ToSocketAddrs 连接本机服务，现在即使服务可达也必然在连接前报 `DescFetch(UnresolvedHost)`。`find_wan_service` 构造出来的 hostname controlURL 执行 soap 也同样失败。

具体触发 2：discovery 的 LOCATION 与描述中的绝对 controlURL 使用不同 hostname，而后者实际解析到另一地址；当前 context 只保存一个 SSDP 来源 IP，两个 hostname 都会被无条件重定向到该来源地址，绝对 controlURL 的连接目标错误。解析代码 `join_url` 明确接受绝对控制 URL，因此该输入并未被上层拒绝。

建议：来源 IP 只能作为绑定到已发现 LOCATION 原 host 的直连来源，不能套用于任意后续 hostname。公开非取消入口至少维持既有 hostname 行为；需要被 join 的观察路径采用真正有界/可取消的解析或注入式域名解析接口，避免将“取消”与“拒绝合法域名”混为一体。localhost 回环以及不同-host绝对控制URL应有回归。

## 其他已核查项

- 发布门把 cache/probe/state/token 台账提交放在同一互斥区，停用先置停止位再过该门；已进入提交的事务在 stop_public_observation 过门前完成，后续事务会看到停止位而跳过。DeferredLogs 在门外派发，未发现门锁内用户回调导致的新增死锁。
- RelayWorker RAII join、停止事件 try_send、engine 保存成功半边、spawn不完整时停成功半边均正确；控制面投命令不等待驱动回执，驱动先 stop/join 再收 relay 没有循环等待。
- UPnP 的 DeadlineStream 按同一绝对期限检查取消；SSDP 50ms 读拍与纯数值连接保留 HTTP Host，未发现这些部分的协议字节变化。connect_timeout 可仍等当前至多3s连接尝试，但不是无界。
- DDNS 取消能打断当前 UDP recv，每域名前检查共用预算；check_one 的结果落状态前有停止位检查。单独该检查并非线性化，但非并发外部 stop_public_observation 完成 join 后没有迟到状态写入。
- CLI 三处正常停止/重启/最终清理都先 stop_public_observation 再缩租再 shutdown，单调用顺序正确；缺口在上述并发完成保证。
- 未核验外部服务或生产环境，未执行 dsh，未提交。

## 最终复查裁定（后续整改树）

### 原问题 1：已关闭

当前 engine.rs 约 1166-1220 行：observer_threads 在构造时从 JoinHandle 提取不可变 ThreadId。shutdown/stop_public_observation 先识别观察线程重入，重入仅置停止位/投Stop，不等待句柄锁或join。外部 stop_public_observation 持 observers 锁直到 drain 的所有 handle join 完成，因此第二个外部调用会阻塞到第一调用完成；shutdown 另以 shutdown_completion 锁覆盖观察收工、driver join、relay join与文件清理，第二个完整 shutdown 也不能提前返回。未发现锁顺序环；shutdown→observers 单向，观察线程重入绕开两锁。新增 shutdown_with_relay_releases_public_udp_port 将观察回调置于可控阻塞，并并发调用完整 shutdown 和 stop_public_observation，放行前不得返回，放行后再次完整 shutdown 并验证 UDP 重绑，覆盖了原触发交错。

### 原问题 2：已关闭

当前 upnp.rs 约 590-612、705-706 行：context 保存 LOCATION 原 hostname，只对相同 hostname 使用 SSDP 来源地址；非同host与公开无来源context都调用 doh::resolve_system。resolve_system（doh.rs 约138-155行）复用 OnceLock 的唯一系统解析 worker，容量1请求队列；等待者 deadline/stop受控，worker不持出口实例或发布权。connect_within 的系统解析与各地址连接共用至多3s期限；localhost及独立hostname不会再被无条件拒绝或错投SSDP源。unrelated_control_hostname_does_not_reuse_ssdp_source 以错误来源192.0.2.1和 localhost 真回环服务验证，旧实现会失败；discovered_hostname_uses_source_address_and_keeps_host 保留同host直连及原 Host。根会话报告全workspace/clippy/交叉检查已绿，本次只读确认未重复整套执行。

### 隔离门新增例外：主体收窄正确，但另有一项明确缺口（P3）

精确 core rustls.workspace=true 一条声明 + server/doh.rs 单文件消费者例外符合实际同步TLS状态机需求；其他同步文件仍扫描rustls，DoH文件仍扫描runtime/await，例外没有扩大到整个server目录。TOML解析与源码注释剥离沿用现行门口径。

但 tools/check-quic-isolation.sh 约234行 `SYNC_ASYNC_PATTERN='tokio::|quinn::|async[[:space:]]+fn|\\.await([^A-Za-z0-9_]|$)'` 仅拦 async fn，不能拦同步DoH文件中的 `fn bad() { let _f = async { 1 }; }` 或 `async move { 1 }`。两种纯std future不需要tokio依赖，也没有await，因此实际可编译并通过当前正则；这与注释“仍禁async”不符。独立正则复核结果：async fn=HIT，async块=MISS，async move块=MISS。当前负例自校准也只测async fn+tokio+await同时出现，无法覆盖该缺口。建议按关键字边界拦async（保留非关键字方法名不误报），并分别加纯async块和async move块负例。此缺口不影响本批DoH代码运行正确性，但应在宣布隔离门收紧完整前补上。

### 隔离门 P3 采纳终态：已关闭

根会话已将 SYNC_ASYNC_PATTERN 改成完整 async 关键字边界 `(^|[^A-Za-z0-9_])async([^A-Za-z0-9_]|$)`，覆盖 async fn、无空格async块及async move；新增纯async块负例自校准，同时保留await_quota同步方法名的放行自校准。本次只读核对脚本230-260行确认已落盘。同步源码继续禁止async/runtime，rustls唯一例外仍是core精确依赖+server/doh.rs单文件。该项关闭，最终全门执行由根会话统一完成；本轮独立复核无未关闭发现。
