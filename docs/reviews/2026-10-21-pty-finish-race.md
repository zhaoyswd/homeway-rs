# 2026-10-21 PTY 无腿收尾与最后写者竞态

## 技术门（开工前，结构化自评）

用户要求自行评审，不使用 review skill。本子任务使用实现者/评审者双角色结构化自评。

根因已逐调用核对：`SessionRegistry::finish` 首次调用也可能返回空 `ends`（CREATE 不接入、最后一腿已经断开），空列表仅表示没有 ENDED 接收者。`finish_session` 把它误判为已收尾，直接返回，导致注册表的 `done` 位已设置、LIST 看不见，运行态却继续持有 PTY master 和输入 fd。另外，收尸前快照 `writers == 0` 不能决定收尸后的释放；最后写者在收尸期间退出时，原实现会提前删运行态，或被旧快照错过。

拟定最小方案：

- 以运行态 `stopped` 领取一次收尾所有权，ENDED 列表只负责逐腿投递，空列表仍继续收尸和回收。
- 注册表保留至收尸结束；写者仅在注册表已移除后释放运行态。收尸完成在锁内按当前写者数复核，覆盖最后写者在收尾前、期间、之后退出三个次序。
- 入口和锁外收尸回归均校验会话代数，旧 pump 或旧收尾线程不能误删/误杀同名新会话。
- 收尾在 PTY core 锁内先检查退出状态，再决定是否发 SIGHUP，避免自然退出已收尸后向可复用 PID 补信号。保持原 2 秒收尸宽限、ENDED 词表与 wire 不变。

评审检查：无新增依赖、无新线程/锁、无协议/常量变化；服务锁内只做状态迁移，PTY 收尸仍在锁外；重复收尾不重复等待或发信号；最后写者必须保留运行态直至收尸结束。无腿、收尾前写者已退、收尾期间最后写者退、重复收尾与同名新代隔离均须有无固定 sleep 的回归断言，既有 raw/surface ENDED 用例继续验证。

技术门结论：通过，按上述方案实施。

## 实现与回归

仅修改 `crates/homeway-core/src/term/service.rs`，未更改线协议、版本、生产配置或连接策略。

- `finish_session` 在服务锁内用 `stopped.swap(true)` 领取收尾所有权。没有腿时仍执行完整的子进程收尸、注册表移除和运行态释放。
- 收尸期间保留注册表；`writer_exited` 仅递减计数，收尾拥有者完成收尸后按当时计数决定是否删除运行态。收尸后的最后写者则由 `writer_exited` 删除运行态。spawn 失败回滚也复用此入口，避免在晚于收尸时只减计数却漏删。
- close 快照及 pump 自然退出/异常退出都携带 `sess_gen`。收尾入口和锁外收尸完成后的状态回归均校验代数。
- `PtyShared::stop_and_wait` 在 core 锁内先 `try_wait`，只有尚未退出才发 SIGHUP；已经收尸的自然退出路径不再补发旧 PID 信号。

新增四个确定性回归：无腿 CREATE 收尾、最后腿与写者先退、core 锁暂停收尸期间最后写者退出与重复 finish、收尸后写者排干及旧代回调隔离。第三例以锁和完成通道控制次序，未增加固定 sleep；重复调用的完成通道有 2 秒断言预算。已有 raw/surface ENDED 测试保留。

修复前仅添加前三个回归，原实现 **0 通过/3 失败**：无腿注册表未回收、最后写者先退仍遗留运行态、收尸期间最后写者提前删除运行态。证据 `/tmp/pty-finish-before.log`。修复后新增四例 **4/0**（`/tmp/pty-finish-after.log`）。

## 代码门（完工后，评审者角色）

- 边界：空 ENDED 列表不参与幂等判断；无腿、断腿、自然退出、服务关闭共用一次收尾。
- 并发：服务锁内领取所有权和迁移注册表；子进程等待在锁外；最后写者在收尸前/期间/之后三种次序均被覆盖。重复收尾不再发信号或等待 core 锁。
- 身份：旧 pump、旧 writer 与旧 finish 回调不能修改同名新代；收尸完成后的注册表移除也有代数核对。
- 错误面：spawn 回滚统一 writer 退出入口；信号与等待都在现有 core 小锁中，不引入写锁等待，不恢复被修复的阻塞写死锁。
- 对齐：ENDED code/reason、逐腿排干和 2 秒收尸宽限不变；原 raw/surface 端到端用例继续通过。
- Rust 形状/直译痕迹：未增加 Arc、Mutex、运行态字段或依赖；复用已有 stopped、registry 生命周期与会话代数，删除重复回滚逻辑，无新增逐行移植形状。

代码门结论：通过，无遗留高危项。生产重复背压与 fd 验证由主会话在用户授权的正式部署流程执行；本子任务未接触生产实例。

最终补丁验证：macOS `cargo test --locked -p homeway-core --lib term:: -- --nocapture` **181 通过/0 失败**（13.80 秒）；`cargo clippy --locked -p homeway-core --all-targets -- -D warnings` 通过；`git diff --check` 通过。日志分别为 `/tmp/pty-finish-macos-term.log`、`/tmp/pty-finish-macos-clippy.log`。按主会话安排，Linux 完整回归交给推送后的 GitHub 双平台 CI；本子任务创建的 `homeway-pty-finish-20261021` 私有 Docker 容器已删除，未宣称本轮 Linux 本地测试通过。
