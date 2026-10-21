# 2026-10-21 中继 TCP 重启绑定修复

## 技术评审（开工前）

实现范围仅为 `relay/mod.rs`：在 Unix TCP listener 的 `bind` 之前设置 `SO_REUSEADDR`，恢复标准库 TCP 监听器在 Unix 的重启语义；Windows 保持现有选项，IPv6 `V6ONLY`、显式地址范围、双族监听、backlog 128 均不变。不设置 `SO_REUSEPORT`，不改变 UDP、协议、认证、重试或监听失败降级策略。

根因是 socket2 创建的 TCP socket 默认未开启地址复用，服务端主动关闭已接受连接产生 TIME_WAIT 后，立即重启的 bind 可能报 EADDRINUSE。主会话已在原生 Linux Docker 用实际连接分别验证 `SO_REUSEADDR=false` 重绑 errno 98、true 成功。核查 Rust 1.99.0 本地标准库 `library/std/src/sys/net/connection/socket/mod.rs:554–566`：`TcpListener::bind` 在非 Windows 平台明确设置 SO_REUSEADDR=1，并注释 Windows 的 socket hijacking 风险；这正是切换 socket2 后漏掉的默认行为。核查本地 socket2 0.6.5 `src/socket.rs::set_reuse_address` 与 `sys/{unix,windows}.rs`：该 API 直接按平台调用 SOL_SOCKET/SO_REUSEADDR，无额外可移植语义包装。

Microsoft [SO_REUSEADDR 官方说明](https://learn.microsoft.com/en-us/windows/win32/winsock/using-so-reuseaddr-and-so-exclusiveaddruse)指出 Windows 地址复用可强行绑定已有端口并导致投递不确定；因此必须 `#[cfg(unix)]`，不能无条件开启。技术门结论：通过；只恢复 Unix 的正常重启能力，不开放并发监听复用。

验证设计：先接受真实 TCP 连接，服务端主动 shutdown 写端，客户端确认 EOF 后再关闭自己的写端，服务端确认 EOF；关闭连接与 listener 后立即对原地址重绑，不使用固定等待或改变断言。另断言活动 listener 的同地址重复绑定仍返回 AddrInUse，覆盖端口独占边界。

## 代码评审（完工后）

实现者完成最小补丁后，以评审者角色复核：

- 选项在 bind 之前设置，错误按原 `io::Result` 向上传递；若设置失败，保持原有整体监听失败处理，不伪报就绪。
- 同一闭包覆盖显式地址、v4/v6 两个 listener；未扩大监听范围，保留 V6ONLY 与原端口选择/IPv6 降级逻辑。
- `#[cfg(unix)]` 保留 Windows 行为；没有 SO_REUSEPORT、共享新状态、线程或 unsafe；无 Go 直译痕迹。
- 活动 listener 同地址重复绑定必须 AddrInUse；真实连接的主动关闭后立即重绑必须成功，关闭顺序由双方 EOF 确认，无固定 sleep。
- wire 字节、认证、准入/转发和判据日志均未改变。

自测（macOS，2026-10-21）：`cargo test --locked -p homeway-core --lib relay::tests::tcp_listener -- --nocapture`：2/0；`cargo test --locked -p homeway-core --lib relay:: -- --nocapture`：38/0（2.02s）。`git diff --check` 通过。`cargo clippy --locked -p homeway-core --all-targets -- -D warnings` 通过。测试和 clippy 原始输出分别保留在 `/tmp/relay-restart-tests.log`、`/tmp/relay-restart-clippy.log`。

生产现场由主会话读取：v0.3.14 在 Linux TCP 41741 绑定失败后仅 UDP 运行；等待历史 TIME_WAIT 到期并二次重启后，IPv4/IPv6 TCP 41741 均恢复监听，Mac 12:29:33 收到 OK-MAC，印证该根因。此子任务没有连接或变更生产实例。

代码门结论：通过，无高危未处理项。首次换装仍可能遇到修复前未设置地址复用的旧 TIME_WAIT，需等待它自然过期；本补丁保证由新 listener 接受的连接关闭后可立即重启，不强行清理系统 TCP 状态。Linux 原生源码回归、全仓测试、交叉检查与发版部署由主会话继续记录。


## 主任务最终验收

补丁已随v0.3.15发布，并由v0.3.16两台最终换装承接；最终CI双平台1225/0、Release五目标全绿。
v0.3.16在已认证控制连接存在时真实relay restart 52ms恢复TCP v4/v6，统一pid/serve41641不变，
Mac重认证成功。首次从无地址复用旧版升级曾等旧TCP状态62s；后续新版本之间无需该等待。
完整SHA、进程、回退与范围见 `docs/DEPLOY-RUST-EXIT.md` §13.15。
