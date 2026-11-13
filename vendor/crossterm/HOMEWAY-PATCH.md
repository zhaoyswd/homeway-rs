# homeway 本地补丁：Unix 终端输入关闭

来源为 crates.io 正式发布的 **crossterm 0.29.0**，不是上游 master 或未发布 fork：

- 包下载：<https://static.crates.io/crates/crossterm/crossterm-0.29.0.crate>
- 原包 SHA256：`d8b9f2e4c67f833b660cdb0a3523065869fb35570177239812ed4c905aeff87b`
- 原包 `.cargo_vcs_info.json`：`36d95b26a26e64b0f8c12edfe11f410a6d56a812`
- 原始 MIT 许可完整保留在 `LICENSE`；保留原 manifest、src、docs、examples、README、CHANGELOG
  及 vcs 元数据。上游 CI 配置与发布/构建状态文件未导入；独立测试生成的 `Cargo.lock`、`target/`
  不入库。`UPSTREAM-SHA256SUMS` 记录本次导入的原始文件摘要，用于与原包重建差异。

根 workspace 的 `[patch.crates-io]` 让 CLI 与 Ratatui 共用同一份修复。包不属于根 workspace
成员，不把第三方测试/示例加入默认全仓测试；不改变 core/capi 依赖图。

本地差异只有：

1. `src/event/source/unix/mio.rs`：EOF 返回 `UnexpectedEof`；非 WouldBlock/Interrupted
   错误向上传递。仅保存 mio 明确的 read-closed 状态，保证 parser 缓存和关闭前剩余字节先交付，
   再报告关闭；不能缓存普通 readiness 后直接对阻塞 TTY 再读，否则按键后空闲会挂起。
2. `src/event/source/unix/tty.rs`：TTY 的 WouldBlock 与 EOF 分开；POLLHUP/POLLERR 或无效
   readiness 必须结束，完整可读事件先交付。信号/唤醒管道的 `read_complete` drain 语义不变。
3. 两个后端接入共享 `input_closed_tests.rs`：EOF 不自旋、正常空闲后仍可读、4KiB 事件与
   7168B UTF8 Paste 在关闭同拍时全部先交付；mio 另测真实 write-only FD 的异常读取传播。`.gitignore` 只隔离本地独立测试产物；本说明和原始摘要为维护材料。

不改解析器、键鼠/Paste 编码、正常输入调度、既有 poll 时间窗或 Interrupted 重试策略；普通 TUI
和 Web 输入 worker 都直接受益。这个补丁不承诺修复上游所有大粘贴边沿问题，也不承诺任意外部
VMIN/VTIME 配置或持续 EINTR 下的可取消性。homeway 自身 raw mode 为 VMIN=1/VTIME=0。

维护时先核对原 `.crate` SHA，再展开原包，对照 `UPSTREAM-SHA256SUMS` 与上述三处源码差异。
上游 issue <https://github.com/crossterm-rs/crossterm/issues/1110> 和 fork 提交
<https://github.com/daulet/crossterm/commit/ea7f652639fa2e5c4248cb0fa3b8bed2c1928714>
是参考资料，不是本地来源或已发布修复的证明。未来正式版本覆盖并通过相同回归后，应同时删除
本目录、Cargo patch 与 exclude，更新 Cargo.lock，而不是无说明地追踪 Git 分支。

独立底层回归：

```sh
cargo test --manifest-path vendor/crossterm/Cargo.toml --lib input_closed_tests
cargo test --manifest-path vendor/crossterm/Cargo.toml --lib input_closed_tests --features use-dev-tty
```

真实产品路径（只创建 `/tmp` 私有 helper 实例，所有子进程限时回收）：

```sh
cargo build -p homeway-cli --locked
python3 tools/tui-input-close-smoke.py --bin target/debug/homeway
```

修前失败、修后结果、独立评审与未测边界见 `docs/reviews/2026-11-13-tui-input-spin.md`。
EOF 终结的是查看界面；不改变服务端工作会话的生命周期。
