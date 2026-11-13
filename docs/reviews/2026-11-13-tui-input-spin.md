# TUI 失效终端输入忙循环根因修复

开始：2026-11-13；完成与最终验收：2026-11-14（Asia/Shanghai）。

提交预检补记：暂存后全量 `git diff --cached --check` 会报告随原包引入的 CRLF 与
19 处上游既有空白；此前未暂存检查不包含未跟踪 vendor，不能扩大为全量通过。
按 CRLF 语义检查后，主会话逐项核对这 19 处均与原包相同行字节一致；自有文件检查、
两个被修改后端相对原包的补丁空白检查无新增问题。保留上游字节与来源哈希，不为清空白
修改第三方原始文件。原日志为 `/tmp/homeway-tui-input-spin-design/staged-whitespace.log`。

范围：HEAD `42c916d` 上的本地源码修复。生产 PID 90715、系统 PATH、默认 state、身份/config/凭证与用户工作会话不动。仅 `/tmp` 私有 helper 与本脚本拥有的 PTY/子进程。未发布、推送或部署。

## 独立技术评审（实现前及设计增量）

仓库 ROADMAP 要求两道门。用户级 reviewer skill 已核对，但其触发条件限定用户明确要求评审，因此没有调用 dsh；按仓库授权使用独立子 agent `input_spin_review`，与实现者分离。以下保留全部原意见及否决，不用最终通过覆盖首轮失败。


### 技术门一原文

# 独立技术评审：TUI 输入忙循环根因修复

日期：2026-11-13。评审者：独立子 agent input_spin_review。材料：design.md、已发布 crossterm 0.29.0 的 mio.rs/tty.rs/read.rs/timeout.rs/file_descriptor.rs/Unix raw mode、homeway tui/mod.rs/process_input.rs/terminal.rs 与 webserve/pty.rs。未改产品文件，未操作任何 homeway 进程。

结论：**有条件通过，允许按下列收窄约束进入实现**。根因定位成立，vendor 固定版本并修实际读取循环能同时覆盖普通 TUI 与 Web worker；保留现有 Web 限时回收兜底合理，无需重写输入解析。

## 发现与设计约束

1. **中：mio Interrupted 截止返回也可能丢失现有 readiness，不应以“未消费数据”断言安全。** 位置：设计 Plan/mio，原 mio.rs:94-120。read 返回 EINTR 时已进入的 TTY readiness 事件已从 poll 取出；即使没有消费数据，返回 Ok(None) 后再次 poll 不保证产生新边沿。建议本次只修 EOF/错误传播，保持现有 Interrupted 行为；若确需增加 EINTR 期限，必须保存并在下次优先继续 pending-readiness，并以无额外写入条件下恢复输入测试证明。不要为本次根因修复加入无验证状态机。
2. **中：alternate tty 的辅助函数将 WouldBlock 与 EOF 都映射为 0，必须只在 TTY 读路径拆分，不能修改管道 drain 的既有含义。** 位置：tty.rs:89-100、149-174。TTY 路径直接 match read 或新 helper 显式区分三态；读取前处理 NVAL，HUP/ERR 时先交付可读完整事件，随后在无事件/EOF/错误时结束。HUP-without-POLLIN 也必须立即返回。禁止 POLLHUP 单独 break 后下一轮无期限 poll 热转。
3. **低：vendor 应从 root workspace 默认成员中排除。** 仓库以 cargo test --workspace 为门；第三方包不应自动纳入全部终端交互单测/示例执行面。显式 exclude vendor/crossterm，单独以 manifest/依赖包选择进行定向单测；保留完整 LICENSE、package manifest 及被 include_str! 引用的 docs 文件，记录 `.crate` SHA 和准确本地差异。
4. **低：baseline oracle 必须是独立临时副本，不可改 cargo registry 缓存。** 为同一测试保留修前失败和修后通过；每个潜在挂死用进程级 timeout/finally kill+wait 收口。EOF/EIO 覆盖及实际 CPU/期限证据应独立于模拟闭包单测。

## 已核查无阻塞问题

- homeway 进入 TUI 前启用 raw mode；crossterm rustix make_raw 委托平台 cfmakeraw，Linux raw 实现设置 VMIN=1/VTIME=0。正常 raw 空闲应由 poll 等待，不应凭空产生 read(0)。仍应实测本机 termios 与 idle，不扩大为任意 VMIN=0 模式保证。
- EOF/错误 io::Result 已由普通 run/cmd_tui 以及 Web ProcessInput channel 向外传播；TerminalGuard 析构和 App viewer 析构可沿现有机制执行。工作会话生命周期不由这个补丁改变。
- parser 队列优先返回后再读取的顺序合理；不能因提前检查 HUP 丢弃同时到达的完整事件。原有大粘贴 mio 边沿排空问题若未改动不得冒充已修，也不可引入更差的超时/批预算行为。
- 无新 wire/行为时间窗常量的最小 EOF/error 修补不需新增协议时间窗；若后续选择引入预算应登记真源与判据。
- 只改 Unix 两个后端，Windows 源码保持上游；编译验证与未实测平台需明确区分。
- 工程形态为最小 Rust match/Result，未见 Go 逐行移植倾向、额外 Arc/Mutex、无谓字节拷贝或新字符串领域错误。


### 技术门二原文

# 独立技术评审增量 r2：mio 闭合 readiness 的保存

日期：2026-11-13。评审者：独立子 agent input_spin_review。依据：新增 complete_events_precede_terminal_closure 用例实际失败日志 vendor-tests-default.log；crossterm FileDesc 直接 read 与原 raw TTY 路径。

## 原始方案（tty_ready）

结论：**不通过，高危阻塞，不得直接实现**。

把任意 TTY readiness 保存成 bool，并在下一次 try_read 无条件再 read，不适用于真实 homeway TTY：FileDesc 未设 O_NONBLOCK，正常 raw VMIN=1/VTIME=0；读完一个普通按键且 parser 队列耗尽后，下一次直接 read 会阻塞主线程。UnixStream 单测设 nonblocking 会掩盖此回归。“readiness直到WouldBlock”的写法只能配合实际非阻塞描述符或每次可读性检查，而本批不应顺带改变 stdin 的共享文件描述状态。

## 修订方案（tty_closed）

结论：**按以下约束通过，可进入实现**。

1. 仅在属于 TTY 的 mio Event.is_read_closed() 为真时锁存 tty_closed；不可将普通可读事件或泛 is_error 当作输入已闭合。
2. 每次 try_read 仍先交付 parser 中的完整缓存事件。若 tty_closed 为真，则继续读取已经闭合输入残留的 OS 缓冲；缓冲排空后以 EOF/EIO 结束，不再依赖一个可能不会重发的关闭边沿。closed+WouldBlock 可按输入闭合报 UnexpectedEof。
3. 尚未标记 closed 的正常 TTY 保留原有 poll/read 调度，不根据旧 readiness 直接做阻塞 read。不改本次 EINTR 重试期限。
4. 保留原同批 signal/TTY 事件处理相对顺序；is_error 单独出现仍通过读取暴露实际错误，不先清掉尚可解析的数据。
5. 验证必须同时包含：writer 写入再关闭，完整事件按序交付后 EOF；数据超过1KiB且闭合，无额外写入也能交付全部事件；真实阻塞 PTY 正常按键后的空闲等待与 SIGTERM 可退出。用例进程级限时并精确回收。

此修订只为 EOF 正确传播补保存明确关闭事实；不宣称一并解决上游所有正常大粘贴边沿问题，未修改部分应保留原边界。


### 技术门三原文

# 独立技术评审增量 r3：失效终端上的析构诊断 panic

日期：2026-11-14。评审者：独立子 agent input_spin_review。材料：实际失效终端 smoke exit101、stderr 保留时 input-error.log 的 show-cursor 错误和正常 exit1 对照；ratatui-core 0.1.2 Terminal::Drop 与 Terminal::show_cursor；ratatui-crossterm 0.1.2 CrosstermBackend。

结论：**通过，允许以下窄 RAII 包装实现**。

根因链证据成立：输入 EOF/error 修复后 run 可迅速返回；Terminal::Drop 在 hidden_cursor=true 时尝试 show_cursor，出错后 std::eprintln!；终端被撤销同时使 stderr 失效，诊断输出本身 panic。cmd_tui 单独将 eprintln 换成 fallible Write 不足以覆盖 ratatui Drop。

采用 RenderTerminal 拥有 Terminal<CrosstermBackend<Output>>，Deref/DerefMut 仅维持既有 Terminal 调用；Output 的正常模式直接向原 Stdout 写入并原样传播所有 write/flush 错误。RenderTerminal::Drop 首先调用 terminal.show_cursor；仅当失败时把 CrosstermBackend 替换为同类型的 Output::Closed backend，并再次 show_cursor，使 ratatui 的 hidden_cursor 状态归 false，避免其内建 Drop 进入 eprintln。Closed 是析构专用 sink，不能在正常渲染路径启用。

允许的简化：不必修改第二个 vendor 依赖，也无需改变进程 stderr、signal handler 或 fd 全局状态。CrosstermBackend 当前仅持 writer，整体替换不会遗失待析构资源；这样还避免启用 writer_mut 的额外 unstable feature。TerminalGuard 已独立负责 raw/alternate screen/鼠标/焦点/光标恢复，继续保留。正常 show_cursor 成功时直接清 hidden_cursor，不产生替换。

实现约束：
- 无 mem::forget、ManuallyDrop 泄漏或吞掉正常 draw/flush 错误；恢复失败只在析构阶段 best effort。
- 清晰注释锁定依赖的具体失败点以及 sink 只用于销毁；可见性保持模块私有。
- 确认局部变量析构次序：RenderTerminal 在 TerminalGuard 之前 drop；App 及 viewer 原清理继续执行，工作会话不被关闭。
- 验证真实 PTY stdin/stdout/stderr 同时失效应 exit1，不接受101；stderr 单独仍可读时保持错误可见；正常 SIGTERM/SIGHUP exit0、恢复字节不丢；所有测试精确回收自有子进程。
- 不承诺消除任意其他真实 panic 在 stderr 失效时的运行时后果，本批只覆盖可预期 I/O 错误退出路径。

本包装为依赖析构缺陷的局部适配，改动仍围绕“终端失效后的有界无 panic 退出”，未引入新的协议、时间窗、线程或共享锁。


## 最终实现

- 根 Cargo patch 统一 CLI/Ratatui 引用到 published crossterm 0.29.0 的仓内受控副本。
  原 `.crate` SHA 为 `d8b9f2e4c67f833b660cdb0a3523065869fb35570177239812ed4c905aeff87b`；
  LICENSE、原 manifest/文档/示例、vcs 元数据保留。`vendor/crossterm/HOMEWAY-PATCH.md` 与
  `UPSTREAM-SHA256SUMS` 给出来源和可复现差异；target/Cargo.lock 不入库。
- mio EOF 返回 UnexpectedEof，异常 read 错误传播；仅保存明确 read-closed 标记。
  缓存完整事件及关闭前 OS 残留先交付，正常阻塞 TTY 不缓存普通 readiness、不改正常 poll。
- alternate tty 的 TTY EOF 与 WouldBlock 分开，终止 HUP/ERR/无效 readiness；信号与唤醒
  管道 drain helper 不动。没有加入新时间窗、EINTR 预算或输入解析器重构。
- EOF 修复暴露了既有销毁错误：Ratatui Terminal::Drop 在恢复光标失败后 eprintln，stderr
  同时撤销时触发打印 panic。窄 RAII RenderTerminal 保留正常绘制错误传播；仅销毁时尝试
  恢复光标失败后换到专用 sink，通过正常 API 清除隐藏标记，随后照常析构，TerminalGuard
  负责 raw/alternate screen 恢复。无泄漏、catch_unwind、额外锁或全局 FD 替换。
  cmd_tui 的失败诊断也改 best-effort writeln。退出查看界面不删除服务端工作会话。

## 原始失败与整改

证据根：`/tmp/homeway-tui-input-spin-design/`，所有私有进程 finally 精确回收。

- 最初 raw 中文串就绪探针受 ANSI 分割，随后一次 PTY 未设置尺寸导致只输出178B，均未冒充
  根因复现；首个普通完整空闲终端关闭也正常退出。原记录与 private helper 日志保留。
- 确定复现先送不完整 CSI，再关闭 PTY：`/private/tmp/homeway-input-before-vbpyg1gq/`
  的 baseline-reproduction.json 记录96.3%CPU、SIGTERM 后仍活；最终严格 baseline 脚本
  `/private/tmp/homeway-input-close-0hnlow3l/receipt.json` 在2s后99.2%、SIGTERM 再等2s后
  100.0%且未退出，实际 exit1；基线正常按键后 idle 的 TERM/HUP 及 Web 父退出仍通过。
- 同一共享底层回归接入未经修复的私有原包副本：baseline-vendor-tests.log 实际 exit101，
  idle 通过，EOF/完整事件+关闭两条失败。首次最小分支补丁 vendor-tests-default.log 仍
  在“完整事件+EOF”失败，促成关闭状态缓存的技术二轮；普通 readiness 缓存方案被高危否决。
- alternate 后端首次编译因 filedescriptor 不导出 POLLNVAL 失败，修为只请求 POLLIN 时对
  HUP/ERR 外的无效 readiness 明确报错；不加硬编码平台常量。
- 首个修后真实终端失效可27ms退出但rc101，不能算受控退出；cmd_tui 诊断改best-effort后
  fixed-smoke-r2 仍rc101。保留stderr的独立对照证实Ratatui析构打印问题；技术三轮核准RAII
  清理后 fixed-smoke-r3 严格要求失效路径rc1，正常信号rc0，5/5通过。
- CLI 全量首次因移动导入使测试缺 Terminal 而编译失败；恢复cfg(test)导入后259/0及集成5/0。
  clippy首轮仅多余cfg(test) CrosstermBackend导入失败；删除未使用导入后复验，原日志保留。

## 最终验证（日志名相对证据根）

- `vendor-tests-full-final.log`：上游+本地全部底层库单测 **98 passed / 0 failed / 7 ignored**。
  新增7168B UTF8 bracketed Paste与EOF同拍的回归；旧97/0记录保留。
- 新增定向形态：默认mio **5/0**（包含实际有效write-only FD读取的OS异常传播），libc mio
  **5/0**；use-dev-tty与use-dev-tty,libc各 **4/0**。覆盖无期限EOF不自旋、idle等待后可读、
  4096完整按键与EOF同拍仍全部交付；未使用悬空borrowed fd或关闭/复用fd号。
- `cli-tests-final-r2.log`：CLI **259 passed / 0 failed / 2 ignored**，配置集成 **5/0**
  （63.93s）；包含全部TUI155/0（1ignored）、Web PTY6/0与ProcessInput回归。
- `fixed-smoke-r3.log` / `/private/tmp/homeway-input-close-ec26041z/receipt.json`：
  相同普通按键后空闲信号退出：TERM rc0/52.5ms、HUP rc0/48.1ms；完整stdio撤销后
  rc1/6.9ms；保留stderr诊断rc1/18.8ms且无panic；Web控制父退出rc1/132.4ms。
  正常恢复光标/alternate screen字节齐，工作会话保全，所有viewer已wait回收，helper stop0。
- 最终 `final-build.log` 与 `clippy-final-r3.log` 均 exit0；原包非本批的unused_parens警告
  仍原样保留，没有将其描述为零警告。CLI clippy -D warnings通过。
- 最终私有候选/根debug件 SHA256 均为
  `7934bc41670083f6952f38377eb9b0393ed45aee17b853d4604149d5ff409207`。
  产品源码冻结后的工具整改复验 `fixed-smoke-final-r2.log`、
  `/private/tmp/homeway-input-close-qijmkdf7/receipt.json`：严格5/5；TERM0/54.2ms、HUP0/33.4ms，
  失效PTY1/22.6ms、保留诊断1/15.1ms，Web父退出1/198.9ms。真实鼠标选会话、resize后shell
  `34 92`、7168B Unicode Paste写文件逐字节/sha256全通过，不需要后续按键触发读取。
  复核全部viewer已reap、helper已退出、cleanup错误为空；后续仅工具fd逐项关闭保护与报告补录，
  不改产品候选。测试产物与正式二进制分离。

## 边界

仅本机 macOS arm64 实际运行。没有宣称Windows/Linux/OHOS运行验收、完整ci-local或性能矩阵；
Windows源码不改，core/capi依赖图不增加crossterm。VMIN=1/VTIME=0已实读；不承诺任意外部
TTY配置、持续EINTR或所有上游大粘贴边沿问题均已修复。EOF与实际OS错误传播已测，特定EIO
错误码没有单独注入，真实PTY撤销在本机返回EOF。判据变更已登记INTEROP-CRITERIA同批条目。

## 独立代码评审首轮原文（产品通过、收尾整改前整批暂不通过）

# 独立代码评审：TUI 失效终端根因修复

日期：2026-11-14。评审者：input_spin_review，独立于实现者。基线 HEAD 42c916d，范围包括所有未提交 Cargo/vendor/TUI 适配/私有 smoke/判据和专项记录变更。只读评审，未操作生产或任何 homeway 进程。

## 本轮结论

**产品根因修复与销毁适配通过；整批暂不通过，需先整改下列两项中危测试清理问题。** 没有发现产品代码高危问题。完成整改后应复核并保留本轮原意见。

## 发现

### M1（中）私有父进程信号与 helper stop 前缺少精确身份复核

位置：tools/tui-input-close-smoke.py 当前第227行，及234行 helper stop。

证据：daemon_pid 来自私有 pidfile，随后测试经历多个 TUI 生命周期；直接 os.kill(daemon_pid, SIGTERM) 只凭旧PID。finally 调用 helper stop，而 tools/local-rust-exit.sh 的 our_pid 仅核对 basename=homeway，未核对当前私有 binary/state。若 private daemon 提前退出且 PID 被复用，可能误发给其他 homeway（包括用户有任务的实例）。

建议：开始记录 daemon 精确 command（绝对binary + --state私有唯一目录）/启动身份，发信号和调用 stop 前复核当前 PID 仍匹配。若已消失不必再发信号；若身份变化应记录并拒绝信号/stop，不能以basename放行。Viewer 的 Popen/Child 归属回收方式保持。

### M2（中）finally 中一项回收失败可跳过全部剩余回收及 receipt

位置：tools/tui-input-close-smoke.py 当前第231-239行；Viewer.cleanup 第107-111行。

证据：finally 直接 for viewer.cleanup()；kill/wait/close任一异常会退出finally，使后续viewer、private helper stop以及receipt写入均被跳过。测试本身用于防止忙循环/孤儿进程，不能把“所有 cleanup 都成功”作为清理其他进程的前提。

建议：每个 Viewer 回收独立 try/finally，确保其PTY fd关闭；在外层逐项聚合错误，helper停止和receipt写入放独立最终路径，无论单个viewer回收是否失败都执行。最终若清理有错误应使整轮失败，并在receipt记录；不要清理异常覆盖原测试失败而不留证。

### L1（低）根 manifest 注释已失实

位置：Cargo.toml:7。

证据：仍称“M5 C4起根workspace不再有[patch]”，本批已新增crossterm patch。ring=crates.io的事实仍成立。

建议：改为“不再patch ring”或明确本批仅新增crossterm补丁，避免日后依赖维护误读。

## 已核查且没发现阻塞问题

- mio 的 read(0) 明确UnexpectedEof，实际read非WouldBlock/Interrupted异常保留原io错误；只记录is_read_closed，不把普通readiness复用到阻塞stdin。parser完整缓存先交付，再排空闭合输入，EOF在无额外边沿下也能被看到。多于1KiB的关闭残留与7168B UTF8 Paste已有底层用例。
- use-dev-tty 仅TTY路径区分WouldBlock/EOF；HUP/ERR没有可读数据也返回，未请求readiness（含NVAL）明确报错；信号/waker管道read_complete保持原语义。没有改解析器、Wire、键鼠编码或EINTR策略。
- RenderTerminal的正常Output::Active直接传播write/flush失败；Drop只在show_cursor失败后切sink并通过正常show_cursor API清hidden状态。无mem::forget、ManuallyDrop、unsafe或全局fd改写。变量声明次序使RenderTerminal先于TerminalGuard析构；现有App/viewer回收保留，服务端会话没有被本补丁结束。
- cmd_tui错误诊断改用fallible writeln，避免stderr撤销时再次panic；不吞正常可读stderr错误消息。
- Cargo tree --locked -i crossterm独立核对只有一个仓内0.29.0实例，CLI与Ratatui共享；vendor明确exclude，core/capi依赖图未增加TUI库。
- 独立逐项核对UPSTREAM-SHA256SUMS的69个原始文件，与registry来源均匹配；本地被修改的原始文件只有mio.rs与tty.rs，另加共享测试/维护材料/.gitignore。原MIT许可、原manifest、include文档/示例和vcs信息保留。Cargo.lock仅移除registry source/checksum指向受控path，没有无关版本漂移。vendor target和独立Cargo.lock被gitignore隔离。
- 真实私有smoke receipts验证正常TERM/HUP返回0，PTY全stdio撤销及保留stderr场景返回1（不接受101），控制父退出返回1，工作会话保全；最终鼠标输入/resize/7168B UTF8 Paste实际发送并有私有文件字节/SHA与shell尺寸验证。build/clippy最终日志完成exit0，97/0底层、259/0 CLI+5/0集成及四特征形态日志存在；新增测试之后未把旧数量冒充新数量。
- 已有首红、普通readiness方案否决、首次退出101、构建导入失败如实保留；未声明未跑的平台运行/完整ci-local/所有上游边沿问题已修。判据登记与实现范围一致。
- “逐行移植痕迹”检查：上游第三方源码保留其原样；本地增量为小Rust match/Result与单一关闭事实，未引入多余Arc/Mutex、字符串业务错误、无谓字节复制或Go包结构映射。RAII局部适配范围合理。

持续EINTR、任意外部VMIN/VTIME，以及原上游正常大粘贴边沿问题是已明示的非本批范围；本轮不要求把它们并入根因补丁。

### 首轮处置

- M1认同：裸daemon信号与helper stop前检查ps精确binary+私有state command；已退出不调用
  helper，清理私有pidfile并记录already_absent；身份变化拒绝操作并记录错误，不使用basename放行。
- M2认同：每个viewer、私有helper独立try聚合错误，receipt仍写，清理异常不能算通过。
  Viewer初始化诊断文件失败也关PTY；kill/wait失败后finally仍关闭fd，两个fd逐项尝试且不重试
  可能已复用的fd号。没有扩大任何生产权限。
- L1认同：Cargo历史注释改为“不再有ring patch”，保留ring为crates.io真版的事实。

最终代码门复核见下方追加原文。

## 独立代码评审 r2 最终原文

# 独立代码评审 r2：整改复核与最终门

日期：2026-11-14。评审者：input_spin_review，独立于实现者。对照首轮 code-review.md 与当前未提交代码；本轮只读产品/工具/收据，未运行生产实例或发任何 homeway 信号。

## 最终结论

**通过。首轮 M1/M2/L1 全部处置，当前没有必须整改的高/中/低问题。** 原始首轮暂不通过结论应继续保留。产品补丁仍为已核准的 Unix EOF/异常传播、明确closed状态与窄RAII销毁适配；之后新增的大Paste/resize用例只扩大证据，没有扩展产品行为。

## 首轮整改复核

- **M1 已改**：tools/tui-input-close-smoke.py:136-141 建立绝对 binary+唯一私有state的精确命令身份。控制父信号前第238行复核一致才发送；finally第253-268行读取当前私有pidfile，消失只清本脚本私有pidfile并记already_absent，身份变化拒绝helper stop并记错误，匹配才调用既有helper。没有以homeway basename或旧PID直接放行。
- **M2 已改**：Viewer.cleanup第109-115行在finally关闭PTY；stderr诊断文件打开已移入构造异常保护。外层finally第243-273行对每viewer独立try，helper另独立try，聚合cleanup_errors，随后写receipt；第274行强制任何清理错误使程序失败。单一viewer异常不会跳过后续viewer/helper与记录。Popen拥有的viewer回收没有改用裸PID/进程组。
- **L1 已改**：Cargo.toml:7 现在明确为“不再有ring [patch]”，与本批crossterm patch共存而不矛盾。

## 最终证据核对

- 独立重新计算当前 target/debug/homeway 与私有候选 fixed/homeway 的SHA，均为 `7934bc41670083f6952f38377eb9b0393ed45aee17b853d4604149d5ff409207`。
- 最终整改同件 smoke 收据 `/private/tmp/homeway-input-close-qijmkdf7/receipt.json`：严格 **5/5**，TERM/HUP rc0；全stdio撤销与保留stderr分别rc1；Web控制父退出rc1；绝无101。观察退出约15–199ms。7168B UTF8 Paste逐字节/SHA与34×92 shell尺寸、正常恢复序列及工作会话保全通过。all_viewers_reaped=true、cleanup.errors=[]，父进程已消失因此helper_already_absent=true，无二次信号。
- 新底层库最终全量日志为 **98 passed / 0 failed / 7 ignored**；默认mio的5个新用例包含在全量，libc mio定向5/0，alternate tty两种libc形态各4/0。UTF8 Paste末尾与close同拍后先交Paste再报EOF，4096独立键事件同样先交完整输入。
- CLI最终 **259 passed / 0 failed / 2 ignored**、配置集成5/0；build及clippy最终日志均Finished成功。原上游unused_parens warning仍在vendor中，未为了清警告扩大vendor差异；不影响本批homeway -Dwarnings检查成功。
- Python最终工具编译语法检查通过，git diff --check通过；已核对Cargo依赖只使用单一受控crossterm。来源hash/两原始文件差异、许可、默认workspace隔离与四特征编译事实见首轮核查，未见漂移。

本轮是独立代码/证据复核，未将“查看实施者执行的测试日志”写成评审者另跑完整测试；所有未跑平台、持续EINTR、任意VMIN/VTIME及上游其他边沿问题范围，仍按专项记录明示，不改变最终通过结论。

## 最后收口

- 完整最终版工具（含两个PTY fd逐项关闭保护）再次同件运行：
  `/tmp/homeway-tui-input-spin-design/fixed-smoke-complete.log` 实际exit0，
  `/private/tmp/homeway-input-close-dr6jt1jv/receipt.json` 严格 **5/5**。
  TERM/HUP rc0，等待44.6/30.2ms；stdio全撤销与保留诊断均rc1，等待5.0/7.2ms；
  Web控制父退出rc1/225.7ms。7168B Paste/SHA、鼠标、resize、恢复字节与会话保全再次通过。
  所有viewer wait回收，helper_already_absent=true，cleanup.errors=[]。产品候选SHA保持不变。
- `tools/check-quic-isolation.sh` 实际exit0，十一条断言全绿，日志`isolation-final.log`。
- 独立两道门最终通过；技术二轮否决与代码首轮暂不通过均原样保留并逐项处置，没有剩余必改项。
  正式PID90715仍是2026-11-08 17:48:48启动的系统PATH实例，未对它发信号、换装或改state。
  本批只有本地源码与私有验收，后续生产包含修复仍需另行正式Release/部署链路。
