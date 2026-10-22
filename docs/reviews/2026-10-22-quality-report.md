# 出口质量历史与报告评审（2026-10-22）

## 技术评审（实现前）

范围：默认 5 秒采集的 QUIC / 过境流观测、异步有界落盘、按时间及完整 devTag 查询本机报告。无 wire / NAPI 变更，不触碰生产。

设计：driver 只收集内存 DTO 并 try_send 至 4 槽有界通道；worker 独占差分状态、JSON 序列化与磁盘。路径按连接 / path_epoch 分段，连接累计字节独立差分，首样本只作基线。短流关闭摘要和活跃流窗增量由 intercept 提供；拨号统计按 flow_id 去重。认证设备在入站时固定，未知单列。文件权限 Unix 0700/0600，4×8MiB 硬容量轮转；磁盘错误只记观测损失，不影响 VPN。查询窗口采用 [from,to)，只计完全落入窗口的增量，跨边界不推算；RTT 等时点观测按 at 过滤，分位数从原始观测重算。

结构化双角色自评：reviewer skill 限定用户明确要求评审才触发，本次为仓内实现门，因此采用 ROADMAP 允许的结构化自评。

- 实现者：核心风险为累计重置、重连归属、跨窗重复、短连接遗漏及观测 I/O 反压。
- 评审者（对齐）：只增加观测，不更改转发、常量或判据行；HTTP 加密内容不解析。
- 评审者（边界）：首次基线、重启、路径变化及计数回退均标记，绝不饱和相减冒充质量良好；不足数据明确说明。
- 评审者（并发）：无阻塞发送、无 driver 文件 I/O，队列满递增丢失计数，worker 持有独立所有权。
- 评审者（错误面）：坏行 / 半尾 / 重启 / 缺采样 / 数据保留边界均在 coverage 中揭示；磁盘失败仍能继续采样并恢复。
- 结论：按以上约束实施；代码门补充测试证据及问题整改。

## 代码评审（完成后）

实现入口：`server/quality/{mod,storage,report}.rs`，接线 `server/engine.rs`。公开 `QualityQuery` / `read_report` / serde JSON / `render_text`；设备使用完整 devTag，认证入口固定流的设备身份。

- 窗口：`[from,to)`；完整增量窗包含 `end == to`，跨边界不推算；0ms 短流按事件时点保留字节。拨号、首上游接收与错误有独立发生时间，按 `(run,flow_id)` 去重；不把重复生命周期字段累计。重传指标按 socket 原累计差分，平台包数与字节数分列。
- 路径：连接标识、迁移 epoch、计数回退、路径不稳定均分段；首样本只作基线，短 QUIC 尾样本缺基线的数量公开。RTT 从原始观测求分位，吞吐为已覆盖观测窗字节速率，不声称可用带宽。
- 并发 / 错误：4 槽 try_send；worker 独占 I/O，无额外 mutex。写盘失败和队列丢弃通过现有 logf 显式节流。停止转发后 `finish` 最多等待 200ms，超时或线程失败明确记录尾批未保证落盘，不无限 join。
- 数据有界 / 隐私：4×8MiB，Unix 0700/0600，拒绝写跟随文件符号链接。仅保存设备、目标 IP/端口与质量指标；无 payload / URL / token。CPU 和主动带宽未采集，报告明确局限。
- 报告：按设备、目标及协议 / scope 聚合；未知设备单列。错误 kind / OS error、TCP 建连 / OS RTT / 重传、背压、QUIC 发送端判丢 / 丢弃均呈现。文本优先异常 / 等待 / 背压并展示前 20 组，JSON 保留全部；不使用健康二元猜测或武断阈值。
- 地道 Rust / Go 直译检查：使用封闭枚举、thiserror、单所有者 worker、typed serde DTO，未复制 Go 包结构；无不必要 Arc / Mutex、字符串错误、锁跨 I/O 或 async runtime 新依赖。计数聚合用 saturating_add，合法但被破坏的大计数也不触发溢出崩溃。

独立集成复核见 `2026-10-22-quality-integration-review.md`，收到问题均整改：非法 UTF-8 坏行改为按字节跳过并计坏行；QUIC stop 后保留句柄收集最终摘要；查询前后比对文件身份，轮转 / 替换明确覆盖不完整，当前文件追加标近似快照；停止增加 200ms 有界排空。

验证：

- `cargo test -p homeway-core --lib server::quality -- --nocapture`：**11 passed，0 failed**。覆盖差分重置 / 迁移 / 短 QUIC、相邻窗口、短长流与 0ms UDP、跨设备 / 未知隔离、生命周期事件与延后摘要、非法 UTF-8 / JSON / 半尾恢复、32MiB 轮转与权限、队列满、缺数据 / 时间缺口、正常排空 / 慢盘有界收尾、存储失败可见。
- `cargo clippy -p homeway-core --lib -- -D warnings`：通过。
- 根会话私有真实 QUIC + TCP 端到端：两设备相同目标，设备隔离及长短流字节分别 18/18、25/25 通过；完整跨平台 / workspace 验证由根会话补齐。

结论：本模块两道门通过，无未整改高危项。正常关闭只作有界排空，慢盘超时与进程崩溃尾数据可能丢失；报告和日志已说明，不能把无数据当健康。

## 增量代码复核：TCP 重传的不可用语义

根复核发现 `TargetReport` 把 TCP 重传聚合为裸 `u64`，平台只支持 RTT / 重传包数而不支持字节数时会输出 `0`，掩盖未知能力。已将 `tcp_retransmitted_packets` / `tcp_retransmitted_bytes` 分别改为 `Option<u64>`：只有字段有真实读数且可归窗（窗内新流累计、或完全归窗的两次快照差分）才产生 `Some`；无基线的窗外旧流累计不直接计入；字段不可用、差分跨边界、计数回退均不填零。真实计数零保留 `Some(0)`。JSON 输出 `null`，文本输出「不可用」，两字段独立聚合。报告局限明确数值只代表可归窗的已观测子集，部分可用不能冒充全量。

新增回归 `partial_tcp_metrics_preserve_unavailable_and_window_attribution`：RTT / packets 可用且 bytes=None、全部不可用、真实零、旧流首次累计不可归窗、后续有效差分与跨窗差分均覆盖；同时核对 JSON null 与文本「7/不可用」「不可用/不可用」。

验证：`cargo test -p homeway-core --lib server::quality::tests::partial_tcp_metrics -- --nocapture`：1 passed，0 failed；质量模块现共 12 项，根会话补跑 affected 与最终 clippy。仅改 report / tests 及本评审文件，未触碰采样与转发。
