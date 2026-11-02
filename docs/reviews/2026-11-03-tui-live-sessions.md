# TUI 会话列表实时刷新（2026-11-03）

## 根因与实现前方案

左侧会话列表只在后台 LIST 成功时替换，与主机探测共用 2 秒 poll_refresh；右侧则直接
消费当前 surface 腿的 STATE 和 SNAPSHOT。STATE 原先丢掉 agent/state，仅写 terminal_title，
SNAPSHOT 的 title 也只写右侧，因此当前会话已有实时消息，左侧仍等待下一轮 LIST。

本轮仅修改 TUI。当前已验证会话的 STATE 标题/state 即刻同步左右两处，SNAPSHOT 只同步
标题；其它会话以及 cwd、agentLabel 等完整元数据独立每 500ms 调度 LIST。主机仍每 2 秒
探测；会话查询仍走既有独立 worker、单在途/最新待查询槽，不在 UI 线程阻塞或每 tick
积压请求。失败后自动 LIST 等 2 秒，切主机、创建、关闭等显式刷新仍立即执行。

代码常量真源为 tui/mod.rs 的 HOST_REFRESH_INTERVAL=2s（原值）、
SESSION_REFRESH_INTERVAL=500ms、SESSION_REFRESH_RETRY=2s；本节为同批登记。
500ms 是健康时的请求调度周期，远端连接/网络/服务采样和 UI 调度仍影响端到端时延，
不是对任意远端的 500ms 保证。探活 5s/10s/15s 预算及恢复退避、core/wire/夹具/
INTEROP-CRITERIA 均不变。STATE 不带 agentLabel，代理标签继续由完整 LIST 更新，
不把用户自定义标签替换为通用 agent 名称。

## 技术评审（实现前）

按 ROADMAP fallback 使用非实现者 design_review 子代理进行只读技术评审，结论通过。
原始意见与处置：

1. 标题与状态必须独立 revision。接受：LiveMetadata 两字段各保存事件 revision，
   请求捕获当时 revision，回复仅覆盖该请求后变化的字段。标题快照不能连带旧 state
   回滚更新的 LIST。被较新 LIST 确认的旧字段清缓存，避免后续同步重新写回。
2. 未知或未验证 PID 仅暂存；必须同名、正 PID、已验证当前腿才应用。接受：直接更新
   还要求当前列表 session_pids 与 active_pid 一致，merge 只碰同 PID 的条目；
   detach/disconnect/begin_attach 均清缓存，原 target/epoch/generation 过滤保留。
3. dec_state 原解码宽松，坏帧不能把左侧刷为 Unknown。接受：先检查至少 4 字节头与
   声明标题长度不超过实际帧，再消费状态。
4. STATE 没有 agentLabel；失败退避不能阻止显式操作，通知应沿一个去重流程。接受：
   标签随 500ms LIST；显式 refresh_sessions 绕过自动 due；通知只在合并后的 LIST
   进入既有 BlockingStates，事件直刷不另建通知源，通常随下一次列表刷新发出。

## 实现与验证

产品改动仅 crates/homeway-cli/src/tui/mod.rs。LiveMetadata 是私有值类型，无新增线程、
共享锁或网络协议。按 createdMs/name 的稳定顺序与按 name 保持选择均沿用原实现。

- `cargo test -p homeway-cli tui:: --offline` 初版产品复用旧测试实际 exit0，
  **128 passed / 0 failed / 1 ignored**；日志
  `/tmp/homeway-tui-live-sessions-tests-initial.log`。
- 新测试第一次编译 exit101，原因是测试把 Notifications::recording 返回的 Receiver
  误当 Mutex；修正为 try_iter，无产品失败或降低断言。保留
  `/tmp/homeway-tui-live-sessions-tests-new.log`。
- 新增 6 项回归后同命令最终实际 exit0，**134 passed / 0 failed / 1 ignored**；日志
  `/tmp/homeway-tui-live-sessions-tests-verified.log`。覆盖独立周期/单在途/失败退避与显式
  绕过、STATE 同拍左右标题/state/保留自定义 label 与 notice、坏帧不刷状态、逐字段
  合并/同名新 PID 不覆盖、未验证暂存与断线导航清理、旧 LIST 保护与通知一次、真实
  surface_frame(SNAPSHOT) 标题接线及 snapshot 不复活较旧 state。
- `cargo build -p homeway-cli --offline` 实际 exit0；日志
  `/tmp/homeway-tui-live-sessions-build.log`。
- 最终 `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` 实际 exit0；
  日志 `/tmp/homeway-tui-live-sessions-clippy-verified.log`。仅目标文件 rustfmt、Python AST、
  git diff --check 均通过。未跑完整 workspace/ci-local/RRR；修改不涉及承载与 wire。

新增 `tools/tui-live-sessions-smoke.py`，复用既有 Tui/Term/pyte 工具，以 /tmp 私有 state
启动 serve/relay 均关闭的统一 daemon，真实 PTY 写 OSC title，观察当前可见物理侧栏与
终端标题，不以原始历史输出判定。最终五项 PASS：当前会话连续四次标题更新左右均可见；
未选中会话连续四次标题到达侧栏；背景 cwd 更新且实际侧栏顺序/当前终端保持；退出恢复
TTY 且两 PTY 仍在；ExitStack 严格回收自有终端与 daemon，二进制哈希不变。

第一次烟囱的八次标题均通过，但背景 cwd 测试用 fresh-cwd 长 basename，100 列侧栏
裁切导致断言失败。改为合法短 basename q7，保持 1.5 秒每次可见预算重跑通过，不放宽
产品判据；失败日志 `/tmp/homeway-tui-live-sessions-smoke-initial.log`。随后完整通过日志
`/tmp/homeway-tui-live-sessions-smoke-verified.log`：当前标题四次 **0.106–0.111 秒**，
背景标题四次 **0.377–0.527 秒**。这是本地样本，包含工具排空与采样开销。

同件 debug SHA256：`43092027890a63e71e74a158ee4462a2b2c83e63922f7f1afa3b4ea220212ed1`。
产品最后修改后仅新增测试/工具清理与排序断言，未改变该件产品源码。最终工具增量再次
实跑 **exit0、5 PASS**，日志 `/tmp/homeway-tui-live-sessions-smoke-final.log`，当前标题
**0.105–0.111 秒**、背景标题 **0.369–0.520 秒**，同 SHA 且真实侧栏顺序断言通过。

## 代码评审与收口

同一非实现者 design_review 独立只读代码门通过，无高/中危阻断。逐段确认目标/epoch
过滤、正 PID 与身份门、probe 验证后补同步、三处缓存清理、逐字段防回滚、通知仅走合并
LIST、失败退避不被主机刷新取消、显式请求立即执行。未发现额外 Arc/Mutex、字符串错误、
逐行移植或无谓接口映射。建议补真实 surface_frame(SNAPSHOT) 接线回归，已增加并通过。
最后增量门亦通过：评审者核真实 SNAPSHOT 回归和 134/0/1 ignored 日志、新烟囱
隔离/逆序清理/物理侧栏与标题判断/排序/SHA 断言及先前五 PASS。最终工具复跑退出码
由实现者独立取回为 exit0。评审者不跑生产操作，实际执行证据由实现子任务汇总。

本轮未访问默认 state、生产服务、远端主机或真实剪贴板，未安装、推送、发版或部署。
主会话更新 ROADMAP 后本地中文单元提交，运行中的官方二进制需后续发布换装才包含本改动。
