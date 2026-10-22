# surface 接入历史与尺寸整改（2026-10-22）

## 第一道门：技术评审（实现前）

按 ROADMAP 两道门采用结构化自评；不调用额外评审工具。

| 检查 | 实现者方案 / 评审者结论 |
|---|---|
| 入口边界 | App 只消费 surface v5 网格快照及差分；已有 raw replay 屏幕检查点修改保持原样，不作为 App 修复。 |
| 实证 | 本机 Codex 0.163.0 默认与 `--no-alt-screen` 私有 PTY 启动后直接 Ctrl-C 退出，未发送任务。输出分别有/无 1049h/l；两种输出喂现有 VT，80×24→38×34 的逻辑行保持，默认备用屏内容未混入主屏。inline 合法 transcript 需保留，不能按进程名删除历史。 |
| 确定缺陷 | surface attach 与切换尺寸仍走给 raw 重画用的 PTY 尺寸哨兵：临时 cols-1，VT/ring 始终 cols。shell/TUI 以临时尺寸重画时其控制序列被按旧几何解释；恢复真实尺寸只重画当前 shell 提示符，已写坏的主屏历史无法补救。 |
| 最小方案 | surface 接入直接取权威 VT 快照；真实 resize 只应用一次目标尺寸，主屏通过 alacritty reflow，程序收到真实 SIGWINCH。surface 活动夺回尺寸及退出重新选举同口径；raw 保留既有哨兵，但每跳同步共享 VT，所有 surface 腿强制快照，快照/FETCH 取 VT 实际几何。注册表/ring 基准保持，恢复世代守卫不变。 |
| 并发 / 错误面 | 保持现有服务锁与尺寸应用顺序；不增加线程、模式推断或失败分支，不改 wire/NAPI/surface 版本。 |
| 验证要求 | 真实 PTY shell 的 WINCH 陷阱构造红→绿：同尺寸 attach 不得改历史，异尺寸只看到真实尺寸；检查输出内容和网格，不只检查哨兵计数。VT 回归补主/备用屏输出退出后跨尺寸的逻辑行与 wraps。 |

技术门：通过。此修复针对可复现的人工尺寸污染，不宣称已在用户现场终端重现全部历史内容。

## 第二道门：代码评审与验证

| 检查 | 结果 |
|---|---|
| 红→绿硬证据 | 新增真实 PTY shell WINCH 陷阱。修前仅同尺寸 surface attach 就将 `WINCH:24 79` / `WINCH:24 80` 写进主屏，精确历史断言失败；修后历史逐字相等，真实 resize 只收到 `WINCH:24 38`。不以哨兵计数冒充内容验证。 |
| 混合腿 | raw 同尺寸接入的间隔内，PTY 与 VT 都是79列；真实 PTY 回显80个 X，rows 的79列 wraps 与 plain_text 的80字符逻辑行都正确。surface 临时快照可解码且几何79×24。恢复80列后逻辑行保持；raw真resize38后两者38；surface50覆盖未完成哨兵后，延迟恢复不踩回38。 |
| 历史与 TUI | VT 回归在备用屏期间从80×24改38×34，退出后再100×20/23×12/80×24。主屏长逻辑行完整，默认备用屏 TUI 不进入主屏；不删除 inline 合法 transcript。已污染旧历史不能逆推还原。 |
| 编码几何 | 代码门发现同步 VT 之后，原 flush/FETCH 仍用注册表基准几何，会将79格行标成80列；已统一取 VT.size()。注册表/ring 仍存稳定基准，用于选举和恢复重验，不混用两者。 |
| 并发与错误 | PTY/VT/快照标记均在既有服务锁内同步，pump 无法观察半更新；wake_surface 仅取独立 Condvar 代数锁，沿用 apply_size 的既有锁序，不重入 state。恢复仍验证会话世代、done/stopped 与基准几何。没有新增线程/unsafe/错误分支。 |
| Rust / 直译痕迹 | 共享 resize_pty_vt_locked 小函数复用现有 SessionVt 和 Size，不加 Arc/Mutex、字符串错误、接口仿写、无谓历史复制或包映射。 |
| 范围与原脏修改 | 原 raw replay 检查点/ring/scan 改动保留，新增提交隔离在清洁 worktree；无 wire/NAPI/surface 版本/生产版本变更，不推送、不部署。 |

验证（本地私有 PTY，无生产对端）：

- `cargo test -p homeway-core --lib term:: -- --test-threads=1`：最终工作区 **208 passed / 0 failed / 0 ignored**，40.64s（含此前未提交 raw replay 的6项新增回归；SIGPIPE 子进程输出不重复计数）。
- `cargo clippy --workspace --all-targets -- -D warnings`：最终工作区通过。
- `git diff --check`：通过。
- `CC_aarch64_unknown_linux_ohos=<DevEco NDK>/llvm/bin/aarch64-unknown-linux-ohos-clang cargo check -p homeway-capi --target aarch64-unknown-linux-ohos`：通过，6.24s。
- 隔离掉原有 raw replay 脏改动的清洁 worktree：全 term **202 passed / 0 failed / 0 ignored**，40.54s；workspace/all-targets clippy 通过。
- 上述全量后将首个回归的异尺寸步骤加强为第二条 surface **直接 HELLO 38×24 attach**（此前通过 RESIZE 到38）：最终用例 **1 passed / 0 failed**，0.51s。生产实现未再改变；最终 workspace/all-targets clippy 再验通过（3.72s）。未将先前全量结果冒充最后测试修改后重跑。
- 本批仅出口终端服务/VT 与文档，不改 facade/capi 导出；capi 的实际手机核不包含 TermService，OHOS check 只证明编译兼容。按跨仓硬规则仍由主会话更新 pin、清洁核/HSP 构建，未部署出口或安装手机。

代码门：通过；纠正旧 surface 哨兵要求导致的错误。源码测试不等同于用户终端现场验收；本机真实 Codex 只启动/退出，未执行任务。


## 真实 TUI / 主力真机追加验收

2026-10-22 清洁 `f883d7a` 已完成真实 Codex/opencode、双 surface 腿、C++ 实际解码链与主力
`FMR0224116011480` debug 验收。默认 Codex 退出后 45 条长逻辑行逐字保留，退出→重进的
FETCH 100 个物理行逐格相等；运行中同尺寸重进与捏合跨尺寸通过。图片预览漏画疑点由主会话及
子任务独立原图像素复核证伪。两道结构化自评、私有新版/旧版 A/B、清理与限制见
[追加验收记录](2026-10-22-term-attach-reverify.md)。正常 debug 包已重编重装，临时诊断插桩已撤销；
生产出口没有换装，本段更新不改变上文首次实现时的历史验证范围。
