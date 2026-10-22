# 2026-10-22 出口过境流质量观测

## 技术门（开工前，结构化自评）

范围：只观察 intercept 单线程 reactor 的现有行为，不改 wire、连接/重试/水位或生产实例。
每流嵌入轻量计数，实际 send/recv 成功位置记字节；以首次实际写 upstream 为起点计算首次
upstream 数据等待（不能解释成 HTTP TTFB）。活跃流按采样窗输出增量；所有收口汇集到
remove_flow，结束摘要进入最多 4096 条队列，满时丢新且显式累计溢出。DNS 进程内面单独
标识，绝不冒充外连；UDP connect 只表示本地 socket 就绪，无回包不自动记失败。

出口待写和回投待写分别记录水位与实际背压持续时间，跨窗截断记账。TCP 系统指标仅在
采样/关 fd 时读取；Linux TCP_INFO 与 macOS TCP_CONNECTION_INFO 保留原始计量单位，
不支持/调用失败显式区分，未知 RTT 用 None，不把 0 填成测量值。DTO 不含载荷、域名、URL、token。

评审者检查：单线程属主，无额外 Arc/Mutex；逐包只更新数值，无新增同步日志或磁盘 I/O；
新增内存受流并发上限+4096结束摘要约束。OS 结构按原生稳定前缀读取且检查返回长度，
不假设不同平台 libc 的 tcp_info 一致。短流失败、异步 connect、超时、fd 被提前关闭均
须保全摘要；活跃窗与关闭窗只输出未消费的字节差额，生命周期值显式累计。设计通过。

## 实现与验证

- `intercept::quality::{FlowQuality, InterceptorQuality}` 为可序列化 DTO；
  `Interceptor::drain_quality()` 一次消费结束队列与活跃窗，结束队列上限 4096，
  `closed_overflow` 为丢新数量。每流累计值与窗增量分列，结束摘要不重复累计字节。
- `on_plain_for_device(Vec<u8>, [u8; 8])` 在建流时固化认证设备标记；直接测试入口
  `on_plain` 保留 None，不在结束时反推设备。连接完成、首个上游字节和首次错误均有
  明确事件时间，以单调时钟相对建流墙钟锚点映射，避免采样时点冒充事件时点。
- 上行字节仅在实际成功 send 时递增；下行仅在实际成功 recv 时递增。队列水位与
  drain/send 留有余量后的背压持续时间单列；报告须将此解释为出口用户态待写队列，
  不是 OS 全部在途量、应用请求耗时或可用带宽。独立采样计数不受旧 CC 日志清窗影响。
- macOS 原生 TCP_CONNECTION_INFO 的 `srtt` 单位为 ms，转 us；Linux TCP_INFO RTT
  原生 us。macOS SDK 原文 `netinet/tcp.h` + C `offsetof` 验证：srtt=44、重传字节=72、
  重传包=104、结构大小=112。Linux 对照 libc/Linux tcp.h 稳定前缀 RTT=68、total_retrans=100。
  逐字段检查返回长度；不支持为 Unsupported，调用失败为 Unavailable，未知 RTT 为 None。
- `cargo test -p homeway-core --lib server::intercept -- --test-threads=1`：81 通过 / 0 失败，
  4 个既有性能 harness ignored。本批新增流级 11 例覆盖：真实非阻塞 connect 延后验收、
  实际 TCP 双向字节、首个上游字节、活跃/结束增量、服务端先发、跨窗背压、真实
  WouldBlock、超时/原始 errno、读 RST 与写失败、UDP 本地就绪无回包、有界队列溢出。
- 原生 OS 两例（`sysock::quality_tests`）通过：TCP 实际读取/能力状态与非 TCP 未知字段；
  `cargo test -p homeway-core quality` 在本批当时的联合树上 21/0（含报告侧测试）。
- core 的 x86_64-unknown-linux-gnu、aarch64-unknown-linux-ohos、x86_64-pc-windows-msvc
  交叉 check 通过；ring C 面按 `.cargo/config.toml` / WIN-design 的 shim 或真 NDK 配方。
  首轮无 CC 环境导致依赖 ring 找不到 sysroot，配置后通过，不把依赖环境失败写成代码失败。
- `cargo clippy -p homeway-core --all-targets -- -D warnings` 本批代码无告警；联合树当时仅
  report.rs 的 type_complexity 阻塞，已交由报告工作单元修正，最终全门由主会话汇总。
  `git diff --check` 对本批跟踪文件通过。未运行公网/生产/真机，不据此宣称运行现场归因精度。

## 代码门（实现后，结构化自评）

评审者逐路径核对 `remove_flow` 唯一回收、connect 即时/异步/重试/死线、上游 EOF/错误、
fd 提前关闭、idle 与 shutdown。TCP 系统指标在 fd 关闭前留档；结束队列消费与 active
增量不存在双计。错误保留首次类型和原始 OS 码，不生成错误字符串；连接失败在清流前
补终态与耗时。真实 syscall WouldBlock/读 RST/写错误测试防止仪表只测理想路径。

自评发现并修正：非阻塞测试不能假设一次 recv 必然读到刚写数据（以有界到达循环替代）；
当前队列水位需在 drain/remove 前刷新，跨窗峰值须保留仍在排队的当前量；连接/首字节/
错误按自身事件时间记录，不能用下一拍采样时间代替；重复验收不得重写已建立连接耗时。
全部通过针对性回归。

并发与资源：驱动线程独占，新增 meter 没有 Arc/Mutex/跨线程访问；每包只更新数值，
无新增同步日志/磁盘 I/O、无逐包新分配。分配只在建流、关闭入队和采样 DTO 向量，新增
驻留量受现有并发流上限与 4096 关闭摘要控制。无 Go 逐行移植/接口仿写/无谓 clone 数据正文。

边界：隧道 IP :53/:5300 真 listener 不建过境流，外部 :53 被内部代答的流标 scope=dns，
真实 OS I/O 字节为 0，DNS 质量须另看 DNS 自有计数；协议不解析 HTTP/TLS，无法把一条
长 TCP/UDP 会话拆成业务请求；无回复 UDP 不自动标远端失败。并发上限拒绝发生在建流前，
仍由既有 rejected 计数覆盖，不伪造连接记录。闭环无高危残余；最终联合验证由主会话登记。
