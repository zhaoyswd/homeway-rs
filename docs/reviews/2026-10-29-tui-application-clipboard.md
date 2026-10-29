# TUI 应用剪贴板写入（2026-10-29）

## 根因与设计

PTY 中应用输出 OSC52 后，core 的 VT 已解析为 ClipboardStore；TermService::forward_clipboard
实时编码为 Op::CLIPBOARD 的 WRITE 帧，发给各 surface 腿（raw 腿自身含原序列）。TUI 的
process_attach 未匹配 CLIPBOARD，落入默认分支而丢弃；现有 clipboard worker 仅接自己
拖选和主机菜单操作，因此应用提示复制后没有系统侧写入。

本次用户明确要求支持应用写剪贴板，收窄旧 `2026-10-27-tui.md` 的不自动触达边界：
允许当前已接入会话的应用 WRITE，仍不读取系统剪贴板、不处理 READ_REQ/READ_ANSWER。
当前 TUI 是 passive observer 也接受其正在展示会话的实时 WRITE；不升级 active、不改变
PTY 尺寸。服务端不回放 clipboard，每个实时帧消费一次；同文再次复制属于新请求，不按
文本值去重，也不跨会话汇总或广播。

修复限于 TUI 消费端：严格验证 WRITE 的长度/UTF-8/32KiB 上限，复用既有有界异步 worker
及本机 helper / SSH、tailcat-* 的 OSC52 路由。新增 Application purpose 与独立 pending
owner（generation/target/session/ticket），不借用 Selection/Operation 的通知所有权。
分离/断开/切会话撤销未开始请求；已经开始的本机 helper 写属于请求仍有效时已提交的操作，
无法撤销，迟到结果不更新界面或发 OSC52。结果通知还需原通知所有权未变，避免覆盖新错误。
队列满/失败显示原会话错误；未知 kind 忽略，不把远端控制字符写到宿主终端。

不改 core/wire、判据或行为常量；INTEROP-CRITERIA 无 TUI 应用写判据，故不修改既有 core
判据。补 README 与真实私有 PTY 应用 OSC52→surface帧→TUI→mock helper / 捕获 OSC52 的
端到端证据，不使用真实系统剪贴板、正式 state 或生产服务。

## 技术评审（实现前）

按 ROADMAP fallback 由非实现者 independent_review 执行；前一小单元 dsh 过度扩展耗时已
留证，本单元经主会话明确安排不重复该外评路径。独立技术门通过，可开工，无阻断。三项原始意见与处置：

1. 不直接依赖宽松 dec_clipboard（会接受尾随字节并复制 Vec）；TUI 借用解析头部，WRITE
   须精确声明长、≤32KiB、严格 UTF-8；READ/未知 kind 忽略，空文本允许清空。
   **接受**：新增借用 parser 与 typed PayloadError，覆盖合法空文本、UTF-8/长度/大小边界。
2. Application 有效性与提示所有权分开：generation/target/session/ticket 决定副作用，
   operation/notice 快照仅决定是否更新提示；同 attach 连续同文 WRITE 各自入队，后一个
   不能仅因 ticket 更新使前一个失效。**接受**：每请求独立 pending owner，通知仅控制反馈。
3. 每个待处理请求保留可取消标志，detach/disconnect 撤销尚未开始者；worker copy 前检查，
   UI Terminal 结果发 OSC52 前再核 attach，队列有界。**接受**：每请求轻量 Arc<AtomicBool>
   guard，清 pending 撤销；复用原 4 请求/4 结果通道，最多 9 个 application owner；无通用框架。
   回归覆盖队满、同文、通知竞争、迟到/分离；E2E 使用私有 mock。

## 实现与实际验证

TUI 新增 CLIPBOARD WRITE 分支、独立 Application purpose 及 pending owner；不改 core 或
transport。App Drop/detach/disconnect 清理 guard，worker 开始 copy 前检查有效性；过期
Terminal 结果不输出，当前请求不被新通知或同文请求取消。README 补当前应用写支持和不读边界。

首轮原 TUI 实际 114 passed / 0 failed / 1 ignored、exit0，日志
`/tmp/homeway-tui-application-clipboard-tests.log`；补回归后实际 **119 passed / 0 failed /
1 ignored、exit0**，日志 `/tmp/homeway-tui-application-clipboard-tests-final.log`。
`cargo build -p homeway-cli --offline` 实际 exit0，日志
`/tmp/homeway-tui-application-clipboard-build.log`。
`cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` 实际 **exit0**，日志
`/tmp/homeway-tui-application-clipboard-clippy.log`。rustfmt / Python AST parse /
`git diff --check` 均实际 exit0。

首次私有应用烟囱实际 exit1：新 surface 探针把 ATTACHED 误写为 DATA 的 0x01，正确 wire
常量为 0x09，未到应用复制断言；私有 finally 清理已执行。仅修工具常量，产品不变。
原失败日志保留 `/tmp/homeway-tui-application-clipboard-smoke.log`，复验将另存日志。

修正探针常量后第二轮日志 `/tmp/homeway-tui-application-clipboard-smoke-final.log` 实际
**exit1、2 PASS**：真实应用写、同文两次、精确 Unicode/控制字节、empty 清空均通过，
后半新 TUI 等目标 grid 超时。加入诊断屏和 LIST 后同件第三轮
`/tmp/homeway-tui-application-clipboard-smoke-diagnostic.log` 仍实际 **exit1、2 PASS**，
当前屏显示误接入 `tui-b45d9fe1` 新 shell，真实 LIST 显示目标 `application-copy` 只有
原 active host 与探针 passive app 腿。原因是工具仅等“会话”面板标题便发 Down/Enter，
异步列表未到，Enter 触发新建 shell。只修工具：等待侧栏确切目标条目后 SGR 点击，
保留严格 `wait_terminal(name)`，不改产品或降低复制断言。

代码门同时指出：重连使用 Terminal 路由时仅看 helper count 不能排除回放；接受意见，
在新应用 emit 之前断言整个新 TUI output 没有 OSC52 写，再记录新请求 output_start。

最终同件实际命令：

```sh
python3 tools/tui-application-clipboard-smoke.py --bin target/debug/homeway
```

最终日志 `/tmp/homeway-tui-application-clipboard-smoke-verified.log` 实际 **exit0、4 PASS**：

1. 真实私有 PTY 的 shell printf OSC52，独立被动 surface 探针确认实际 `Op::CLIPBOARD`
   WRITE 的 kind/声明长度/字节，TUI 的 mock helper 连续两次得到相同精确内容（Unicode、
   组合字符、换行、TAB、NUL、ESC），每次只写一次，raw active/140×40 与所有 app passive 不变。
2. 真实 empty WRITE 清空 mock sink，不触真实系统剪贴板。
3. 重连不回放旧写（native count 与新 TUI 初始完整输出均核）；homeway term 的 tailcat-*
   marker 经同一 remote_terminal 路由发出唯一精确 OSC52，native helper 次数/内容不变，
   不假称系统复制成功，观察腿归属/几何不变。SSH 环境标记路由由已有单测覆盖，本烟囱没有
   启动真实 SSH 服务或声称真实桌面剪贴板收到请求。
4. 两次正常分离后 TTY 恢复、私有 PTY/探针/daemon 收尾；binary SHA 前后一致。

所有应用烟囱轮次均关联同一调试件完整 SHA256：
`5c800746201d60f0ef6fb000d5ca82b584a18fabf3600a7a9c7dd7cec6284cb5`。
前述工具失败记录保留，不能把全部轮次混记为单轮通过。产品实现之后未再变更，仅工具
就绪同步/断言修正；没有安装、发布、推送、生产 state 或真实剪贴板访问。

新增五项回归核严格借用 payload/empty/上限/非法 UTF-8/尾随字节/READ 与 unknown 无副作用、
取消后 worker 不工作、相同文本两请求都被记录、不激活 observer、新错误不阻断实际 OSC52
但不被覆盖、detach/切主机迟到结果不输出、队满及 Failed 不假称成功。测试用 recording、
blocked 队列及 Vec 输出，取消 worker 用超限文本防守即使回归也不启动真实 helper。

主会话独立读取最终烟囱四 PASS、TUI 119/0/1 ignored 和 clippy 完成日志，另实际 shasum
确认 `target/debug/homeway` 为上述完整 SHA、diffcheck 通过；没有安装或部署。

## 代码评审（实现后）

非实现者 independent_review 与实际验证并行审阅产品 diff，产品代码门通过，无产品阻断。
检查结论：严格 borrowed WRITE 长度/UTF-8/32KiB/empty 与 READ/unknown 处理成立；
per-request PendingWrite RAII 取消、VecDeque 由 4 结果 + 4 请求 + 1 worker 约束；
detach/disconnect/Drop 清 owner，Terminal 结果先核 generation/target/session；
无关 notice 只抑制反馈，同文请求逐一保留。原 Selection/Operation 路径未变；新增 Arc 仅
跨 worker 传取消位，没有多余锁、接口仿写、无谓拷贝或逐行移植痕迹。已开始 copy/helper
不可撤销的边界与实现一致。

烟囱重连证据问题已按上述意见修正，独立增量代码门最终通过，**全单元无剩余阻断**。
评审者实读新工具、原诊断失败屏/LIST 与同 SHA verified 四 PASS，确认 attach_target 没有
放宽功能断言、完整初始输出检查补足回放证据、工具改动无新问题。该门是静态审查与证据
核验，评审者未自行跑服务、测试或真实剪贴板。实现者接受并完成唯一工具整改，无待办项。

本子任务不提交代码；主会话更新 ROADMAP 并做独立工作单元的本地中文提交。

