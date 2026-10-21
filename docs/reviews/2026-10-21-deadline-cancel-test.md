# 2026-10-21 DeadlineStream 取消回归去除期限竞争

## 技术评审（开工前）

基线 `0524cb0`（v0.3.16）：GitHub Actions run `39546563479` 仅 macOS 的 `server::deadline_io::tests::pending_read_obeys_deadline_and_cancellation` 失败，实际 TimedOut、预期 ConnectionAborted；Ubuntu 全过，本地全仓 1225/0。主会话已定位：两条测试分支均给 90ms 总预算，而取消线程 sleep 20ms 后仍需操作系统调度；负载下迟于 90ms 执行取消，会让测试错误地要求取消一定赢过期限。

设计：只将取消分支的安全总期限设为 5s（严格大于既有 350ms 返回速度要求），非取消分支仍为 90ms；保留取消线程及原 20ms 间隔、错误类型断言和两条分支的 350ms 总耗时断言。不改 DeadlineStream 生产逻辑，不增加 sleep，不预先设置 stop。取消失效仍会在 5s 超时报错或违反 350ms 断言，绝不能假通过。

技术门结论：通过。取消分支隔离取消行为，期限分支隔离总预算行为；不再让无关的 90ms 截止竞争影响取消错误类型。

## 代码评审（完工后）

实现完成后，以评审者角色复核：

- diff 仅位于 `#[cfg(test)]`，DeadlineStream 的 remaining/io/Read/Write 生产实现逐字未改。
- 非取消分支的 90ms 总预算与两条分支的 `< 350ms` 断言原样保留；错误类型断言也未放宽。
- 原取消线程仍异步设置 stop，原 20ms sleep 未增加；没有屏障将测试退化成预先取消状态检查。
- 5s 只用于取消分支的兜底，超过 350ms 即使返回 ConnectionAborted 也会失败；若取消不起作用则类型断言失败，不会将超时当成功。
- 未加线程、共享状态、接口、依赖或 unsafe；无 Go 直译痕迹，未改变线协议、行为常量或判据日志。

macOS 精确回归：`cargo test --locked -p homeway-core --lib server::deadline_io::tests::pending_read_obeys_deadline_and_cancellation -- --exact --nocapture`，1/0（0.15s），日志 `/tmp/deadline-cancel-test.log`。`git diff --check` 通过。`cargo clippy --locked -p homeway-core --all-targets -- -D warnings` 通过，日志 `/tmp/deadline-cancel-clippy.log`。

代码门结论：通过，无未处理项。CI runner 如果连既有 350ms 要求也无法满足，测试仍按原要求失败；本修复仅移除取消与较短总期限之间的错误竞争。未提交、推送或部署，后续 CI 由主会话执行。
