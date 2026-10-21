# 终端断腿 SIGPIPE 根因修复（2026-10-22）

## 范围与现场

- v0.3.17 官方 Linux musl 在真实终端 KILL/断腿后静默退出；主会话已回退官方 v0.3.16。
- 本子任务只操作 `/tmp` 私有 HOME/state 与独立端口，不替换/重启生产，不使用 review skill。
- 官方 v0.3.17 私有零参 unified（serve+relay）对新 term UDS 连接立即 shutdown/close，
  第3次时 `Popen.returncode=-13`（SIGPIPE），排除生产凭证/config 与 nohup 形态特有问题。
- 原始证据目录：阿里云 `/tmp/homeway-terminal-0ef14e5e7227.9tSHtI`；
  `sigpipe-before-unified.log`、`sigpipe-before-unified-arqsq0bx/runtime.log`。

## 第一道门：技术评审（实现者方案 / 评审者检查）

| 检查 | 方案与结论 |
|---|---|
| 根因 | main 明确恢复 SIGPIPE=DFL；term wire 绕过安全 socket helper，直接 libc::write。对端在 POLLOUT 与发送间关闭会生成进程级 SIGPIPE，而非返回可处理 EPIPE。 |
| 最小修复 | 仅将一次裸 write 换成现有 sysock::send（MSG_NOSIGNAL），复用 Result，不增加新 unsafe/依赖/信号全局策略。 |
| 平台边界 | Mac Rust 1.99 的 UnixStream::pair 不设置 SO_NOSIGPIPE，std Write::write 在 SIG_DFL 子进程仍死亡；直接复用跨平台 helper 优于假设标准库所有 socket 来源均安全。 |
| 并发 / 错误面 | poll 与发送的关闭竞态不可消灭，但 send 不发信号，EPIPE/ECONNRESET 均走原 Hard 断腿。默认 CLI stdout SIGPIPE 不变。 |
| 对齐 | 不改线协议、HELLO、surface、PTY 收尸、超时常量或队列预算；只改内部传输安全性。 |

技术门：通过；不选择全局 ignore SIGPIPE，也不把问题归为脚本/环境后放过。

## 第二道门：代码评审与验证

- `poll_write` 仍依据每次实际短写字节数前移；零字节仍 WriteZero，WouldBlock 重 poll，
  原绝对 deadline、部分余尾、同帧/换帧续写语义不动。
- 新增测试启动当前 test binary 的精确子测试，只在子进程设 SIG_DFL，关闭配对流后
  要求 BrokenPipe 且子进程正常退出；不污染并行测试进程全局 signal。
- 惯用 Rust 检查：复用本仓 sysock::send，不另写平台 cfg/常量，不加 Arc/Mutex 或拷贝。
- Mac wire 专项5通过（含读期限、零进度断尾、正常大帧、排空后续尾与 SIGPIPE 子进程）。
- Mac `cargo clippy -p homeway-core --lib --tests -- -D warnings` 通过。
- Linux 真实 `cargo test --release -p homeway-core --lib` 的新增精确子测试：修前稳定
  SIGPIPE（signal 13）失败，日志 `sigpipe-before-test.log`；非仅静态推断。
- Linux 修后用同一 `wire.rs` 源文件与现有 core 的 sysock/frames 编译轻量测试 harness，
  wire全部5通过，含默认SIGPIPE子进程，日志 `sigpipe-after-harness.log`；这是专项，
  不冒称全仓/完整core测试通过。Mac仍为标准cargo wire专项5通过。
- 私有GNU CLI增量重建与零参unified整体验证进行中；尚未声称1000次立即断开与完整
  PTY冒烟通过。源码/两道门在此冻结，后续结果由主会话统一登记。

代码门：通过；默认SIGPIPE回归已前红后绿，现有wire短写/断尾/期限专项无退化。

发布限制：本地/临时 GNU 只作私有诊断，绝不部署；主会话另升 v0.3.18，按 main/tag/
GitHub Release 下载校验流程发布，并对官方 musl 重跑私有与正式验收。
