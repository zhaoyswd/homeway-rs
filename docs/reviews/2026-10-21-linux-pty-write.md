# Linux PTY 输入背压退出修复

## 技术门（开工前）

基线：`2dd95fd`。用户要求定位 GitHub CI 的 Linux PTY 背压写入挂死，禁止使用 review skill；本记录采用实现者/评审者分离的结构化自评。

**根因**：`PtyShared` 已将控制与写入分锁，但仍使用 portable-pty 的阻塞 `write_all`。原注释和测试假设 SIGHUP 杀死子进程会解除 master 写阻塞；Linux 不保证该行为，写线程可永久保留会话资源。macOS 能通过，不能证明 Linux 正确。

**方案**：master 使用 `O_NONBLOCK`，输入通过短写和 `poll(POLLOUT)` 等待，50ms 一拍检查会话级取消标志，取消返回 BrokenPipe。控制关闭/收尾先取消，再发信号、收尸；写锁保留，因此继续维持输入顺序和调用方背压，不引入队列。dup 共享 file status flags，读端也必须包装为 `poll(POLLIN)` 后重试，保留对上层的阻塞读取和 Linux PTY EIO→EOF 语义。每个使用端持有自己的 OwnedFd/File，禁止跨线程关闭裸 fd。

**评审者检查**：

- 对齐完整性：wire、ENDED 退出码、信号宽限和输入顺序不改；仅会话终止时未完成输入被取消。
- 边界：零长度读写立即返回；短写、EINTR、WouldBlock、HUP/EIO 和读取 EOF 需分别覆盖。
- 并发：取消不取 writer/core 锁；写锁等待者随后也立即检查取消。不会让非阻塞标志泄漏成 pump 的 WouldBlock 错误。
- 生命周期：fd 独占所有权，销毁只关闭自己的引用；取消不能依赖子进程退出，也不能通过 fd 号复用唤醒。
- 风险：portable-pty writer 的 Drop 会写 VEOF，改为自管 fd 后不能保留这种潜在阻塞析构；服务结束已有显式信号/收尸，故本路径不需要 Drop 注入。
- 验证：真实 PTY 下子进程仍活着时取消满队列写；信号退出/自然退出的写者收工；延迟输出确保读端不会漏出 WouldBlock；多段输入顺序。

技术门结论：方案可实施，不改变协议或连接生命周期策略。

## 代码门与验证

**实现者结论**：仅修改 `term/pty.rs` 与 `term/service.rs`。原阻塞 portable-pty writer 改为自管 File（OwnedFd 转入）且没有阻塞析构；输入每次实际 write 都为非阻塞，短写和 WouldBlock 正常保留背压，写锁继续维持顺序。`POLLHUP/POLLERR` 在输入侧直接 BrokenPipe，输出侧先排空再解释 EOF/EIO。`POLLNVAL` 显式报 EBADF。输入取消独立于子进程，kill/wait/已退出的 try_wait/控制面 Drop、输出 EOF/Drop、服务 finish 的幂等与首次路径均接入。

**评审者检查与处置**：

- 并发和所有权：无跨线程 close；master、输入、输出各自拥有 fd 引用。`dup` 的共享 O_NONBLOCK 已由输出 poll 包装覆盖。取消用 Release/Acquire，无写锁依赖。
- 输出完整性：输入停止不阻止 pump 读取剩余输出；读到 HUP 仍 read 排空。空缓冲 read 返回 0 不改变停止标志，已新增显式回归。
- 真实背压：新回归在子进程不读输入且仍存活时单独取消，完成后再杀子进程；避免把信号效果当成取消证据。
- 自然退出边界：测试先用 ready 证明 raw/trap 就绪，实际写未完成且子进程仍活，再用 USR1 通知 shell 正常 exit 7。去除固定退出时刻与 CI 调度的竞态。
- 输入顺序：256KiB 全字节序列在读者延迟消费时往返完全一致，无任意正常输入超时、无新队列、无半帧重试。
- Go 直译痕迹：无；使用 File/OwnedFd 和 Read/Write trait，新增一个会话级取消原子，不仿写 Go 接口、不引入无消费者的导出。
- wire/日志/常量：wire、现有日志和信号宽限未改变；50ms 是取消检查粒度，正常输入不因此超时，亦非网络连接策略。

**验证证据（2026-10-21）**：

- Linux 旧 `2dd95fd` 隔离快照真实复现：`pty_control_ops_survive_input_backpressure` **0/1，4.53s**，断言为“子进程死后写线程必须解除阻塞退出”。由主会话在 OrbStack Linux aarch64 / Rust 1.99 容器实跑，日志 `/tmp/pty-linux-before.log`。
- 修后同一 Linux 容器全终端测试 **177/0，13.18s**（第一轮包含全部四个新增回归，随后自然退出测试改为受控 USR1 触发；最终全量由主会话补验）。日志 `/tmp/pty-linux-after-term.log`。
- macOS `cargo test -p homeway-core --lib term::pty::tests -- --nocapture` **6/0**；`cargo test -p homeway-core --lib backpressure -- --nocapture` **4/0**；受控 USR1 与空读断言加入后背压 **4/0，2.59s**。
- `cargo clippy -p homeway-core --all-targets -- -D warnings` 通过；`git diff --check` 通过。

代码门结论：没有遗留高危项。生产实例、远端仓库、用户设备均未触碰；完整跨平台门和打包证据由主会话汇总。

## Linux 全仓门发现的独立测试竞态

Linux 全仓第一轮 core **843/1** 的唯一失败为既有 `speedtest_server::tests::session_ids_monotonic_across_rounds`：客户端读到第二轮 REPORT 后立即检查日志，服务端同线程尚未写完第二轮结算行，快照只有三行。旧注释已说明两轮之间受理/结算可能交错，但仅改成不依赖行序，漏掉“末轮结算完成”的同步。该问题与 PTY 无调用关系。

**技术门**：只修此单测，不改生产测速逻辑。每轮读帧必须成功并确认 TYPE_REPORT；logger 在结算行入日志后发送 channel 通知，主线程在总 2s deadline 内等待两次完成通知，再保留原两轮不同 ID、各出现两次的断言。读连接设 5s 超时，避免协议异常令测试无限等。无固定 sleep，不用重试掩盖失败。

**代码门**：channel 的发送发生于日志 push 之后，接收完成构成明确的结算完成证据；不依赖两个服务线程调度顺序，不改变 ID 判据、不改变生产性能与协议。通知过滤限定会话结算行，与受理/归因日志不混淆。只改该单测，Go 直译痕迹无。

**自测**：macOS `cargo test -p homeway-core --lib session_ids_monotonic_across_rounds -- --nocapture` **1/0，0.01s**。最终 Linux 全仓回归由主会话补录。


## 主任务双平台收口

- GitHub 失败运行：[ci 39527041058](https://github.com/zhaoyswd/homeway-rs/actions/runs/39527041058)，
  基线 `2dd95fd9930cc235f23074faa5a1440c57af5fdd`。Ubuntu 在原背压测试断言
  “子进程死后写线程必须解除阻塞退出”失败，其余 job 通过。
- 使用新建的私有 Docker 容器（Rust1.99.0，原生 Linux aarch64），不接触任何生产实例。
  Git归档基线先原样运行：原用例0/1，4.53s，复现完全相同断言；修后真实终端177/0。
- 最终 `cargo test --workspace --locked`：**Linux 1220/0、macOS 1220/0**，两者各37个
  ignored未计为通过。两平台 `cargo clippy --workspace --all-targets --locked -- -D warnings`
  均通过。Linux为原生运行，并非交叉check。
- OHOS真NDK的交叉check通过；Windows-msvc按仓内已有C shim配方check通过，未宣称
  Windows运行面测试（终端模块本就不装配Windows）。
- 首轮macOS全仓出现一次未改动的QUIC跨族用例 `NoCandidate`，单独复验0.08s通过，
  最终默认并发全仓亦通过；未放宽它的预算或断言。该次失败原因未确证，原日志保留。
- 首轮Linux整仓测速单测先看到REPORT、后落结算日志，产生843/1；已仅以channel补齐
  该单测完成同步，生产测速逻辑不变。最终仍按原会话ID断言验证。

本机证据：`/tmp/pty-ci-failed.log`、`/tmp/pty-linux-before.log`、
`/tmp/pty-linux-after-term.log`、`/tmp/pty-{linux,macos}-workspace-final.log`、
`/tmp/pty-{linux,macos}-clippy-final.log`、`/tmp/pty-{ohos,windows}-check.log`。

## 应用核与交付核对

- 实现提交 `e407bdf199ff0c9c29ccab6646aee949bd529323`，tier 核 pin 同步到此提交。
- 干净核检出运行 tier `tools/core/build-core.sh`，未使用 dirty/unpinned 逃生口；
  真 NDK release 链接、21 导出面、版本注入和 NAPI/log-index 门均通过。
- `libclientcore.so` 两处落盘均为 **3,217,016 B**，版本 `e407bdf199ff-rust`；
  SHA256 `c8999725087d7d51a221b2782034db7a17f39442d5ef1a1ba07415c56ca39b15`。
- core `assembleHsp --no-daemon` 成功，生成签名 debug HSP **2,311,761 B**；
  逐字节核对包内唯一 `libclientcore.so` 与两处核完全一致。
  HSP SHA256 `5baad982b5ead3eb000d31225584b35eb2520a9159a9ee591e85e204dec34a52`。
- tier code-map/NAPI/vocab/log-index 校验及两仓 diff 空白检查通过。
  打包日志 `/tmp/pty-core-build.log`、`/tmp/pty-core-hsp.log`。
- 私有 Linux 验证容器已移除，原始日志留存于本机 `/tmp`。未运行真机或生产验收，
  当前没有推送、打 tag、发 Release 或部署；因此未宣称新的 GitHub CI 已通过。
