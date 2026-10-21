# 终端加固独立集成复核（2026-10-21）

## 范围与方法

- 按 `AGENTS.md` / `ROADMAP.md` 的两道门执行；未使用 review skill。
- 独立复核 tier 在途修改：surface codec / scroll / selection、stream transport、native IME、ArkTS OSC 消费；没有代替父线程编辑这些文件。
- 经父线程明确追加分工后，仅修改 tier `terminal/src/main/cpp/terminal/session.{h,cpp}` 及 `hosttests/session_host_test.cpp`，修复输入受理/发送失败的可见性与重复键批次。
- 本记录不宣称完成真机、生产或发版验证；未 commit / push / 操作生产实例。

## 技术评审

| 项目 | 结论 | 依据 |
| --- | --- | --- |
| 协议连续性 | 合理 | v5 的 `seq` 用于逐次输出连续性，`revision` 保留网格世代；不能再以旧的 snapshot revision 代替丢帧检测。 |
| 历史行身份 | 合理 | `historyEpoch` 标识行号空间，`historyBase` 单调推进；FETCH 请求携带 epoch。旧响应只有同 epoch 且仍在保留区间才允许并入，不应仅因当前输出 seq 前进而拒绝不可变历史。 |
| 一致渲染 | 合理 | `renderSnapshot()` 在同一 grid 锁内取窗口、几何、模式、光标、选择位图；dirty 标志先 exchange 再重建避免吞掉并发输出。 |
| 选择复制 | 合理 | 选择按下时捕获行内容和 epoch；后续输出不改变待复制文本，当前行与捕获行不同则不继续反白，避免复制内容与看到的选择不一致。 |
| IME 边界 | 有条件通过 | proxy 绑定所属 host，主线程回调、隐藏时清预编辑、销毁时解绑，较 active-host 路由安全。终端不是任意文档编辑器：远端已提交文本范围不能伪装成可替换的本地预编辑范围。 |
| 发送分层 | 修复后合理 | UI 只做有界受理，writer 独占实际网络发送；队列受理失败和网络发送失败必须分别可见，成功入队不等于已经送达远端。 |
| 地道实现 | 通过 | CLI 使用 RAII、可取消队列和拥有者关闭；App session 保留既有独立 writer/小锁，不引入新传输层或额外共享全局状态，无逐行移植扩张。 |

## 代码复核发现与处置

### I1：ERROR 帧解除历史单飞时遗漏 grid 锁（P1，父线程已修）

- 位置：tier `terminal/src/main/cpp/surface/surface_codec.cpp:763`，`SurfaceSession::noteRemoteError()`。
- 初次复核只有 `m_statsMu`，直接调用 `m_scroll.onFetchFailed()`；泵线程在 `m_gridMu` 下调用 `takeFetchRequest()`，UI 也在该锁下读取滚动状态。并发 ERROR / 滚动 / 预取存在 C++ 数据竞争。
- 父线程已按既有 `statsMu → gridMu` 顺序补锁；未引入反向锁序。

### I2：IME 非本地预编辑范围被无条件确认成功（P2，父线程已修）

- 位置：tier `terminal/src/main/cpp/napi_init.cpp:2016`，SetSetPreviewTextFunc / SetImePreview。
- 初次复核忽略 `start/end` 并返回成功，但官方 CAPI 契约要求按 UTF-16 范围处理预上屏。
- 父线程改为本地预编辑范围校验，不能处理的远端已提交文本替换明确失败，不再接受后静默丢弃范围。
- 参考：HarmonyOS 离线官方文档 `capi-inputmethod-text-editor-proxy-capi-h.md` 的 SetPreviewTextFunc；未猜测框架缓冲容量或 API 签名。

### I3：输入队列拒绝/网络失败仍可对用户静默（P1，本子任务已修）

- 位置：tier `terminal/src/main/cpp/terminal/session.cpp:210`、`:337`，`postItem()` / `WriterLoop()`。
- 原先受理失败只写 hilog，而宿主多处忽略 bool；writer 也忽略所有 transport 返回值。因此用户可能以为完整输入已发送。
- 修复：通过既有 OSC 回调的本地 type 3 上报静态错误，不使用远端通知 type 2、不包含输入内容。发送失败后清空队列并停止受理，避免继续发送可能已失去边界的输入。
- oversized 文本 / 剪贴板先检查预算再复制，避免超限输入先分配大 vector。

### I4：IME 多次删除可能只受理一半按键（P1，本子任务已修）

- 位置：tier `terminal/src/main/cpp/terminal/session.h:207`，`sendRepeatedKey()`。
- 原先逐个入队 press/release，队满时可只受理 press，或静默截断重复数。
- 修复：1–1024 次重复键作为一个队列项整体受理；不足预算整批失败。每项按结构体、文本和展开后的每次 press/release 20 字节计账，不能用单条消息绕过 1MiB 上界。
- 网络层中途失败无法回滚已发出的按键；此时明确报告“可能未完整发送”，不宣称原子网络交付。

### I5：无输入的控制请求永远没有 writer（P1，本子任务已修）

- 位置：tier `terminal/src/main/cpp/terminal/session.cpp:273`、resize。
- 原先只有文本/按键入口启动 writer；首次回前台探活或 resize 可以成功入队但永远没有消费者。
- 修复：控制帧也统一经过可启动 writer 的受理路径，并保留 resize / snapshot 合并。快照序号分配与入队同锁，disconnect 等 writer 退出后才清零序号。

### I6：运行中重装 OSC 回调与 reader 并发（P1，本子任务已修）

- 位置：tier `terminal/src/main/cpp/terminal/session.cpp:242`，`setOnOscEvent()`。
- 原先更新受锁回调后又重写 transport 的 `std::function`，reader 可同时调用该函数对象。
- 修复：transport 只在连接前安装稳定转发函数；运行中只更新 `m_oscCbMu` 保护的回调副本，调用发生在锁外。

### I7：最大几何下历史缓存预算无法覆盖完整历史视口（P2，父线程已修）

- 位置：tier `terminal/src/main/cpp/surface/surface_scroll.cpp:53` / `:191`，`surface_scroll.h:140`。
- 可复现输入：1000 列 × 500 行；每格是 `a` 加 14 个 combining acute，29 字节合法 UTF-8 字素簇。500 行历史缓存超过固定 32MiB，但 wire 体仍小于 32MiB 解压上界。
- 宿主实测：仅保留 471 行，滚动在绝对行 529 冻结；预取连续四轮重复 `[17,+512)`，新旧行因保护当前窗口而被反复淘汰，无法更深入滚动。
- 临时复现程序：`/tmp/tier-scroll-budget-check.cpp`；结论需由父线程的最终预算策略和回归测试关闭，不以“有内存上限”冒充“所有允许几何都可完整滚动”。

## 验证

- `tools/term/stream-host-test.sh`：通过；包含实际滴流 deadline、控制帧合并，以及 session 控制帧启动、队满错误、writer 失败、断开拒收、1024 次完整 press/release、展开预算与错误内容不泄露输入。
- session 宿主测试另以 `-Wall -Wextra -Wno-unused-parameter -Werror` 编译并运行：通过。
- session 宿主测试另以 `-fsanitize=undefined` 编译并运行：通过。
- `tools/term/surface-golden-test.sh`：通过；v5/cell2 跨仓常量、Rust golden、序号断档、历史 epoch、不可变选择、UTF-8、换源桥、键鼠输入编码均通过。
- `tools/term/renderer-host-test.sh`：通过；组合字素簇、Arabic 上下文、完整 127B 字素、权威列宽、RGB 表、同屏 atlas 容量失败不重复重建。
- `git diff --check`（本子任务 session 三文件）：通过。
- ASan：宿主运行时初始化失败 `sanitizer_malloc_mac.inc:189`，尚未进入用例，不作已通过断言；不以此替代并发真机验证。

## 剩余验证边界

- OHOS IME 系统回调、中文预编辑候选框坐标、后台隐藏/返回、系统剪贴板授权和通知权限需父线程完成 HSP/HAP 编译并在设备实测；宿主测试不能覆盖系统生命周期。
- I7 最终处置：两端保留单轴上限并增加 128Ki 格乘积门，超面积在分配前拒绝；镜像/FETCH 同样限制材料总格数。客户端回归覆盖最大合法量级 256×500、每格127B稠密历史在32MiB预算内仍铺满视口，后台并入后当前窗口保留；1000×500明确拒绝。
- NAPI 已接入 sendRepeatedKey，ArkTS 已消费本地 type 3；父线程 HSP/HAP 集成门统一登记。
