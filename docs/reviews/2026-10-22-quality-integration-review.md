# 出口质量观测集成独立复核（2026-10-22）

## 范围与方法

复核者为 QUIC 子任务 agent；本轮只读 CLI / 历史报告 / 集成测试的实现，不参与这些模块的实现，不提交、不推送、不碰生产。

入口：`crates/homeway-cli/src/quality_cli.rs` 与 `main.rs` 命令接线；`crates/homeway-core/src/server/quality/{mod,storage,report,tests}.rs`；`server/engine.rs` 采集/收工；`crates/homeway-core/tests/quality_history_e2e.rs`；`tools/quality-e2e.sh`。开始前已向 report agent 确认稳定核心与其最后的展示层增量；整改后重新检查相应路径。

## 发现与处置

### 中：非法 UTF-8 坏行导致整个查询失败（已整改）

原 reader 使用 `read_line(&mut String)`，历史中只要出现单个非法字节，后续有效行也无法查看。复核用真实 `target/debug/homeway serve quality --state <私有临时目录> --from 0 --to 3000 --json`，在两条有效 heartbeat 之间插入 `0xff\n`：原实现 exit=1，报 `stream did not contain valid UTF-8`。

整改为 `read_until` 字节读取 + `serde_json::from_slice`，无效 UTF-8 与其它 JSON 坏行一致计入 corrupt_lines 并继续；测试夹入 0xff 且恢复追加后保留两条有效 heartbeat。

### 中：正常 QUIC 收工生成的最终 DTO 未送至 worker（已整改）

原 engine 在 stop 前采活动样本，随后 move/drop ExitQuic；stop 内 clear 生成的最终 DTO 随对象释放，最后仅采集流摘要。整改为通过 as_ref 保留句柄，stop 后关闭 intercept，再 `collect_quality(Some(q))`，最终摘要及溢出计数进入有界队列；不扩大 QUIC stop 预算。

### 中：并发轮转可能静默漏读历史（已整改）

原查询按 .3→.2→.1→current 打开文件；若中间 rename 轮转，尚未读的文件可能移到已经扫描的位置，漏掉其中 path/flow，且心跳不一定也缺失。整改在查询前后采四个文件 identity/长度/创建及修改时间；轮转或替换显式标记覆盖不完整并建议重查，普通追加标记为近似快照。该提示不承诺事务快照。

### 低：异步 worker 与正常前台退出的最终落盘竞态（已整改）

原 Collector 线程 detach，最后一批存在前台进程退出先于落盘的风险。已增加显式 finish：关闭唯一发送端后等待 worker 完成确认，等待预算上限 200 ms；超时/线程异常写明确日志，调用位于转发、QUIC 与 intercept 收工后。慢盘及崩溃仍可能缺最后一拍，不能承诺持久化事务或无限等待。

## 逐面复核

- 时间窗：查询契约为 `[from,to)`；瞬时样本按发生时点过滤，字节/背压只纳入完全覆盖的窗，不按比例推算。恰在 to 结束的字节区间可入窗，而 at=to 的 RTT/关闭事件不入窗；相邻查询不重复字节。
- 累计与身份：QUIC 首采/迁移/回退/不稳定路径置基线，禁止跨连接、重启或路径直接相减；缺基线短会话数量可见。流记录的字节为窗增量，拨号、首字节与错误按真实事件时点筛选，再按 `(run,flow_id)` 去重。
- 设备过滤：CLI 必须提供完整 16 位 devTag；未知身份不会混入指定设备。报告按设备+目标+协议+scope 聚合；内核 TCP 重传按流生命周期累计差分，不混设备。
- 指标口径：QUIC 发送端判丢不称双向丢包；实际吞吐不称带宽容量；首次上游等待不称 HTTP TTFB；UDP 本机 ready 不称远端响应成功；无历史、低流量或没有错误不推断健康。
- 资源：driver 只取内存 DTO 并 try_send，4 槽队列有界；worker 独占序列化与文件；最终队列、流关闭队列以及 4×8MiB 历史均有界且公开损失。查询每个文件最多读 8MiB，文本显示前 20 目标，JSON 保留可追溯原数据聚合。
- 错误与恢复：缺文件返回证据不足，坏行/半尾可见；崩溃半行在下一次追加前补换行，不能吞掉后续新记录。磁盘失败只关闭当前句柄并累计错误，后续可重开恢复，不阻断 VPN。
- 命令：`main.rs` 直接将 serve quality 分派到离线读取；默认 state 与既有 CLI 一致，无 daemon 启动或凭证读取。缺值、重复参数、未知参数、冲突/倒置时间均 fail-fast；RFC3339 要求明确时区，日期/闰日/小数时间与毫秒整数边界已有测试。
- 私有集成：脚本仅管理 /tmp 独立实例；真实两设备经 QUIC DATAGRAM 与 intercept TCP OS 重拨访问回环 echo，并核对不同 payload 字节、长短流、设备过滤和关闭摘要。实际数据路径包含出口 L3 拦截，不以服务流 echo 代替 transit 验收。未触碰生产。
- Rust 结构与直译痕迹：纯 DTO 边界、worker 单一文件所有权、有界标准通道和显式错误类型；未见无谓新协议、逐行仿写、热路径文件 IO 或新增异步栈泄漏。

## 收尾复核与判据

本轮独立执行：

- `cargo test -p homeway-cli --bin homeway quality_cli::tests -- --test-threads=1`：4/4 通过。
- `cargo test -p homeway-core --lib server::quality::tests -- --test-threads=1`：最终 11/11 通过，包含非法 UTF-8 与半尾恢复、相邻窗口、身份过滤、迁移/计数回退基线、短/长流、错误发生时点、正常 finish/超时边界、存储失败、容量和权限。
- `zsh -n tools/quality-e2e.sh`：通过。
- 修前真实 CLI 的非法 UTF-8 失败已复现；整改后的字节读取及继续处理由最终测试验证。
- `git diff --check`（CLI、新 E2E、脚本及本评审文档）：通过。

主会话提供的集成证据：真实私有出口 E2E 1/1 通过；本轮已独立复核其构造、断言与实际 L3 路径，不重复创建私有实例或触碰生产。工作区完整 CI / Clippy 与跨仓门由主会话统一执行，不在此记录中冒充本轮独立执行。

结论：本轮发现的 3 项中风险与 1 项低风险均已整改并复核；当前范围无未处置的高/中风险阻断项。轮转并发查询和慢盘退出可能导致覆盖不完整，均有显式说明；报告只提供可核对证据，不从缺数据自动断言网络健康。

## 增量独立复核：TCP 重传不可用语义（最终收尾）

主会话最后发现：Linux 能给 RTT / 重传包数而不给重传字节数时，旧聚合 `unwrap_or(0)` 将未知误报为零。本轮依主会话要求，只复核 report agent 的针对性修正，不重跑广测，不修改实现。

已检查：

- `TargetReport` 两个重传字段分别为 `Option<u64>`；只有真实可归窗读数或合法差分才调用 `add_available_count`。字段 None 不初始化累计；真实零保留 Some(0)。
- 窗内新流的首读数可按累计归窗；窗外旧流的首读数不能归窗。后续只有前后采样区间完全在查询内才差分，`checked_sub` 遇回退不填假零。两个字段各自判定，不因为 RTT / 包数可用就伪造字节数。
- JSON 保留 null；文本经 `display_count` 显示「不可用」，不展示 Some/None 调试形态。部分流 / 区间可用时 Some 只代表可归窗的已观测子集，报告局限明确未宣称完整总量。
- 新回归 `partial_tcp_metrics_preserve_unavailable_and_window_attribution` 覆盖 RTT+包数可用且字节 None、全部 None、真实零、窗外旧流首采、有效差分、跨边界差分，并检查 JSON null 与文本「7/不可用」「不可用/不可用」。实现者提供针对性 1/1 通过证据；主会话承担最终 targeted / Clippy / cross / 清洁核构建。

另外复核私有 E2E 的测试拓扑调整：生产 `StackB` 包装引用已替换为测试文件自持 `PacketDevice` / `TestNetwork`，用 Medium::Ip / MTU 1280 的 smoltcp 客户端继续产生真实 L3 IPv4/TCP 包。两设备仍经 QUIC DATAGRAM → 出口 intercept OS 重拨 → echo，字节及设备过滤断言保留。主会话提供调整后私有 E2E 1/1 与 QUIC 隔离十一门全绿证据；没有扩大隔离白名单。

结论：本次定向修正符合不可用语义和时间窗契约，无新增高/中风险阻断项。未重跑广测，未提交或推送。
