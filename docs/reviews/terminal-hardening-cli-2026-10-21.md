# CLI 终端输出背压与公平调度整改（2026-10-21）

## 技术评审

范围为 `crates/homeway-cli/src/term_cli.rs`。用户授权修复现有终端问题，不调用 review skill，不触碰生产实例，不提交或推送。

- 无界输出通道改为按字节和条数有界的帧队列；raw 数据保序且不丢弃，容量耗尽时阻塞 reader 向上游施加背压。
- 队列关闭是独立于 socket 的取消入口，必须先关闭队列唤醒等待者，再关闭流并 join reader。
- stdout 使用暂存并恢复原标志的非阻塞模式，部分写保留剩余字节，poll 同时服务 stdin、信号、唤醒和 stdout。
- 上行新增单一 writer，主循环只非阻塞入队；独立取消句柄不争用写锁，`shutdown`/`ControlClient::close` 中断背压写。
- 每轮仅消费有限帧数和字节，持续输出不能无限排空队列而饿死输入和信号。
- stdin、信号管道的 HUP/ERR/NVAL 与负数读取均显式处理，重连等待不能将负长度转换为切片边界。

边界自评：raw 字节不可丢；队列上限必须包括零载荷帧的条数；生产者阻塞期间取消必须立即可达；非阻塞标志必须跨重连保持且在进程退出前恢复；不新增多余线程或运行时。

## 代码评审与验证

实现者自检与独立评审视角分开检查了容量、丢唤醒、fd 生命周期、收尾与基线差异：

- 输入与输出各有 1MiB / 256 帧上限，另加读写各一帧与 stdout 当前部分帧；零载荷帧也受条数约束。raw 输出不丢弃、不重排，容量不足阻塞 reader。
- 主循环每轮最多处理 32 帧 / 64KiB，每次 stdout 系统调用最多 16KiB；剩余字节保留到下一轮，终端不可写时同时等待输入与信号。
- 队列取消先于 socket 关闭与两个线程的 join，关闭时清除积压、唤醒 Condvar 等待者。reader 的 EOF 与最后一帧状态在同锁下判断，避免将并发到达误当断线。
- 独立 writer 保证上行停止接收时 UI 主循环仍响应分离与信号；取消句柄无需拿 writer 锁。
- signal/poke 写端在发布前设置为非阻塞，设置失败即回收所有资源；退出与 resize 另有原子闩锁，管道满不能丢退出事件。
- stdin 错误与 HUP/ERR/NVAL 显式收尾；重连等待的负 read 不再转换为巨大切片。TTY 增加 Drop 兜底，正常收尾仍显式检查 termios 恢复结果，非阻塞标志跨重连持有、终局归还。
- 队列溢出明确报“输入未完整发送”；不把未受理输入伪装成已送达。没有协议字节变化，没有追加运行时或第三方依赖。
- Rust 形态检查：队列所有权明确，reader/writer 各独占 I/O 半边，无写锁包裹阻塞 I/O；一个 Arc 取消句柄连接两线程，不仿写 Go goroutine 接口。

验证：

- `cargo test -p homeway-cli term_cli:: -- --test-threads=4`：27 项通过（原 21 项 + 6 项，其中一项为压力测试子进程入口）。
- 新增容量/取消测试：填满字节预算后 producer 阻塞，关闭队列后 1s 内被唤醒；零载荷帧达到条数预算后拒收，出队顺序不变。
- 新增 stdout 测试：UnixStream 填满后非阻塞返回零进展，排空接收端后继续发送；终局恢复 O_NONBLOCK 原值。
- 新增真实伪终端子进程测试：满 stdout 时分离键退出；上行 socket 发送缓冲压至 4096B、对端不读且 stdout 堵塞时，SIGTERM 在 2s 内完成取消与收尾。
- `cargo clippy -p homeway-cli --bin homeway -- -D warnings`：通过。
- `git diff --check`（本文件与 CLI 文件）：通过。

边界：生产出口未触碰；压力测试用本机临时 UDS 与 openpty，不以模拟器或生产数据作证明。远程取消沿用 `ControlClient::close` 的既有解阻塞契约；主会话负责跨仓集成与统一判据登记。

## 追加批：仅支持当前协议，移除旧版握手回退

### 技术评审

用户明确尚未发布，不考虑旧 App / 出口兼容。本批只删除版本降级，不误删 raw 终端能力、本地 UDS 位置解析、网络断线恢复和同实例重连。

- CLI 始终声明 `RAW_TERMINAL | PROTO_VER` 与恰好一个键平台位：macOS 为 `KEY_ALT_NO_ESC_PREFIX`，其他目标为 `KEY_ALT_ESC_PREFIX`。
- HELLO 尾始终包含当前协议版本及非空进程 clientID；不再根据出口是否声明 `features::PROTO_VER` 决定省略版本。
- GREETING 必须为当前协议版本，且 raw 消费者的 `REPLAY | MODES | PROTO_VER` 必需能力齐全；缺位明确作为协议错误退出并关闭连接，不进入网络恢复重试。
- 不强求 App 专用 `SURFACE` 位，也不拒绝未消费的额外能力位；这不引入版本回退，raw / surface 是当前协议并存的两种合法消费方式。

### 代码评审

- HELLO 编码调用改为服务端本批统一的两参 `enc_hello_tail(caps, clientID)`，删除 CLI 自己的可选版本分支。
- 新增真实 UnixStream GREETING 测试：当前全能力、最小 raw 必需能力与额外能力正常；逐项缺少 replay / modes / proto-version、错协议版本均明确拒绝，并检查对端观察到 EOF，连接没有泄漏。
- 新增 HELLO 线字节断言：capLen=1、完整 caps、当前 ver、ID 长度与内容严格匹配；验证显式且互斥的平台声明，raw 不伪装为 surface。
- 保留本地 UDS 两种当前 state 入口与连接故障恢复测试；没有引入新层、新锁或额外拷贝，无 Go 直译扩张。
- App / STATE 独立扫查只反馈父线程，不修改 tier：App 条件版本声明及陈旧兼容注释由父线程清理；LIST `clients` 缺字段静默当零的旧出口口径已指出。STATE 字节和 LIST 状态只消费当前 stateV2，未发现需要保留的旧状态映射分支。

本批验证：

- `cargo test -p homeway-cli term_cli:: -- --test-threads=4`：29 项全部通过（本批新增 GREETING 严格门和完整 HELLO 两项），含前批真实 PTY 背压取消回归。
- `cargo clippy -p homeway-cli --bin homeway -- -D warnings`：通过。
- `git diff --check`（CLI 文件及本报告）：通过。

服务端 frames/service 的严格门与 App 集成由各自所有者完成。未提交、推送或部署。

### 最终 App 去旧兼容独立复核

本轮按父线程追加分工只读 tier 代码，不修改父线程文件；结论为**技术方案通过、代码复核通过，没有新的阻断问题**。

技术评审：

- 删除 `decodeRowsAtMost` 合理：当前出口 `bounded_history_rows()` 的返回 count 是实际编码行数，不再是旧形态的请求行数。App `decodeFetchRowsReply()` 必须精确解出 count 行并耗尽字节；缺行、额外尾字节均拒绝，count=0 的合法空响应仍保留。
- `validSurfaceGreeting()` 要求恰 5 字节、当前外层协议版本和 `SURFACE | PROTO_VER` 必需能力，不以删除兼容为由删除版本检查，也不要求 App 消费 CLI 专用 replay 能力。
- `encodeHelloTail(clientID)` 不再暴露可选 caps / 可选版本参数，固定写 App surface、明确非 Darwin 键平台、当前版本和非空 ≤64 字节 ID；调用方将空返回值当失败，不可能继续发送缺失尾部的 HELLO。
- LIST 必需数据改为显式契约：缺 `stateV2` / `clients`、非当前 app/host 的客户端类型均进入 `bad_reply`。空数组仍合法，当前 host 徽章语义和 stateV2 的 unknown 防御显示未误删。

代码评审：

- tier `surface_codec.cpp:491` 的 FETCH 解码仍保留尺寸、格数、行数上界、historyBase 区间与完整消费检查；Rust 出口 `service.rs` 的实际行数和单次 cell/byte 预算与此一致。
- tier `surface_codec.cpp:1095` / `:1104` 与 `stream_transport.cpp:429` 的 HELLO / GREETING 生产消费一致，当前 surface v5 / cell codec v2 校验、错版本硬拒绝、分片超时、快照恢复及握手绝对 deadline 均仍存在。
- 未把 CLI raw 的正常终端消费方式、VT 标准模式、合法零脏行更新、空 FETCH 响应、断线恢复或本地 UDS 位置解析误删为旧兼容。没有新线程、新锁或无谓重编码扩张。
- 仅发现非阻断的陈旧说明：tier `surface_codec.h:55` 仍称缺平台位由出口推断；严格服务端现已拒绝该形态，已通知父线程清理这两行注释，不涉及实际兼容分支。

独立验证：

- 重新执行 `tools/term/surface-golden-test.sh`、`tools/term/stream-host-test.sh`、`tools/term/renderer-host-test.sh`：全部通过，覆盖当前握手版本/必需能力、固定 HELLO 字节、FETCH count 短尾拒绝、输入队列/写阻塞/超时及渲染形态。
- 实读 tier `entry/.test/default/intermediates/test/coverage_data/coverage.log`：`Tests run: 311, Failure: 0, Error: 0, Pass: 311`；不以 hvigor 退出码代替断言判据。
- 父线程已报告 HSP / HAP 构建通过；本子任务未重复执行完整构建，不将父线程的构建结果冒充独立真机验证。
