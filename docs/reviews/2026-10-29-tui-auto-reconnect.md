# TUI 活性检测与原会话自动重连（2026-10-29）

## 根因与设计（实现前）

当前错误/EOF 后 disconnect 只清 attach/输入，保留屏幕，等待用户重新选择；transport
read(None) 没有读超时，黑洞不能靠 EOF 发现。协议没有 PING/PONG，但同一 surface 腿的
既有 LIST 会直接返回 JSON，且不改 active/尺寸，可做主动双向探活。仅看输出静默会误判
idle shell/全屏程序，仅看任意下行又会掩盖单向上行阻塞，故均不采用。

本轮用户明确授权替换旧手动重选恢复边界。只改 TUI，使用既有 LIST/HELLO，无新 wire 或
core 判据。TUI 常量真源写在 recovery.rs，review 同批登记：首次接入/重连整体15秒预算；
接入时立即 LIST 验证、健康期每5秒一笔 LIST、响应预算10秒；从第一次尝试入写队列开始
计时（Busy 也在同一预算内，不被无限延后），仅匹配合法 LIST 应答清未决，普通输出不清。
单一未决请求，不每tick入新请求。失败退避0.5→1→2→4→8秒封顶，持续可取消重试；
完整快照与 LIST 身份验证均通过才算恢复并重置退避。

接回原 target/name，HELLO 仅 OBSERVER，不 CREATE/TAKEOVER。记录已知会话 PID，并用
同腿 LIST 验证目标仍存在且 PID 未换；not_found/ENDED/PID变更停止恢复。切主机、会话、
退出取消计划；旧 generation 丢弃。保留旧 surface 可见内容，但重连重置其 seq/ready/
未决fetch基线，避免新腿seq从1开始被旧seq拒绝；新完整快照覆盖后才允许新输入。
不重放任何输入/粘贴；断开清释放跟踪、剪贴板guard/选区与旧写队列。重接仍被动，用户
新的真实输入才沿既有规则接管尺寸。已握手健康 attach 优先于陈旧 host offline 摘要，
不因此禁发输入或重拨。

UI 线程只poll状态与非阻塞队列，连接仍由既有后台 Attach 完成。每次失败/开始重试给
短行内提示，保留详细原因；成功显示短恢复提示，不能每tick刷通知。不会重建/kill PTY。

## 技术评审

按 ROADMAP fallback，由非实现者 independent_review 只读本设计及相关 TUI/core 既有
LIST 分支执行。技术门通过，可开工，无阻断，三项原始意见与处置：

1. LIST 合法回复须严格解出 sessions 与目标 name/正 PID，仅未决请求可清预算；15秒接入
   与10秒探活预算从首次入队尝试起，Busy/普通输出不续命，快照与身份验证均满足才在线。
   **接受**：借用现有帧、严格 typed JSON 解析、单未决状态机；成功前与消费事件前检查期限。
2. reconnect 保留材质但清 seq/ready、旧 fragment/fetch/输入/clipboard guard；旧 Sessions
   “selected 缺失就 detach”须调整，陈旧列表/host offline 不能抢先终止健康连接或恢复。
   **接受**：surface.reconnect 保屏清基线；background 缺项不清有 recovery 的原会话；
   connected 优先于主机摘要，停止只认同腿明确缺失/PID变化、not_found 或真实 ENDED。
3. 同 target/name 只 observer HELLO，保留最初 PID；完整成功才 reset 退避，切换/退出
   彻底取消。**接受**：沿用 Attach 的 OBSERVER，无 CREATE/TAKEOVER；验证身份前不接受
   新 INPUT 或应用 CLIPBOARD，已知正 PID 不因重试更新。初始 PID 未知/0 不固定身份，
   第一次合法正 PID 应答建立身份。REPLACED 表示腿被替换而非 PTY 结束，允许重连；
   其他真实 ENDED、not_found、同名 PID 变化停止，不创建新 PTY。

## 实现与常量真源

修改限于 TUI mod/surface、新 recovery 状态机、README 与新私有故障工具。recovery.rs
集中 UI 常量：PROBE_INTERVAL=5秒、PROBE_TIMEOUT=10秒、ATTACH_TIMEOUT=15秒、
RETRY_DELAYS=500ms/1s/2s/4s/8s（封顶后重复）；这是新增 TUI 行为的代码真源及同批登记。
无 core 时间常量、wire 字节或现有互操作判据变化，故不修改 INTEROP-CRITERIA。

每个 attach 的初次 LIST 从第一次尝试排队开始计时，包括等待 connect/HELLO 的时间；
排队 Busy 继续使用同一个未决期限，成功入队后不重复发；合法 LIST 应答后健康周期重新
计5秒。15秒总接入预算不能被先到的探活应答或迟到快照清掉。恢复需要原 PID 的 LIST 与
新腿完整快照同时成立；失败清 generation、旧写队列/输入释放跟踪、片段/fetch、剪贴板
请求和选区，同时保留 surface 旧材质。恢复后仅新输入沿既有服务规则接管 active/尺寸。

连接仍由既有 Attach 后台线程执行；UI 每拍非阻塞 poll。丢 Attach 的 cancellation 用独立
socket shutdown 取消读写，不等待探活定时器或 join 阻塞 writer；远端 ControlClient::close
使用独立 ctl_sock 关闭，不等待 write_sock 锁。提示仅在失败/新一次尝试/成功时变化，
不每tick刷通知。切主机/会话、退出清计划，旧 generation 事件丢弃。应用剪贴板仍走原
取消 guard，只有当前 attach 的 PID 验证通过才消费 WRITE。

## 实际验证

- 初版产品 `cargo test -p homeway-cli tui:: --offline` 实际 **exit0，119 passed / 0 failed /
  1 ignored**；日志 `/tmp/homeway-tui-auto-reconnect-tests-initial.log`。
- 新回归首编译实际 **exit101**：保屏测试把 TerminalView 的真实 `rows` 字段误写为 `grid`，
  产品未运行失败；仅改测试字段。原日志 `/tmp/homeway-tui-auto-reconnect-tests-final.log`。
- 修后同命令完整 TUI 实际 **exit0，126 passed / 0 failed / 1 ignored**；日志
  `/tmp/homeway-tui-auto-reconnect-tests-verified.log`。新增7项回归用模拟 Instant、不长睡眠：
  Busy 不续命/单未决、健康静默回复与初始快照独立预算、退避封顶且完整成功才reset、
  保屏接受新seq1、晚15秒快照不能清预算、身份/快照双门与陈旧摘要不取消、缺失/PID替换停止。
- `cargo build -p homeway-cli --offline` 实际 **exit0**，最终日志
  `/tmp/homeway-tui-auto-reconnect-build-final.log`。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` 实际 **exit0**；日志
  `/tmp/homeway-tui-auto-reconnect-clippy.log`。rustfmt、Python AST、git diff --check 均 exit0。
- `cargo test -p homeway-core observer_ --offline` 实际 **exit0，4 passed / 0 failed**（其余
  目标按过滤器0项）；日志 `/tmp/homeway-tui-auto-reconnect-term-observer.log`。确认既有
  observer 注册/尺寸/退出、不选举未活动腿与服务端真实输入接管几何语义保持。

独立 reconnect_smoke 子任务仅写并实际运行新工具（不编辑 Rust 或生产环境）：

```sh
python3 tools/tui-auto-reconnect-smoke.py --bin target/debug/homeway
```

实际 **exit0、5 PASS**，日志 `/tmp/homeway-tui-auto-reconnect-smoke.log`。默认5/10/15秒
预算未缩短，没有工具/产品失败后降低断言或改参数：

1. 私有 unix 完整帧代理只故障 HELLO clientID=tui-* 的腿；原 raw/control 不受影响。
   健康静默12秒跨至少两次同腿 LIST，连接数不增，没有输入/active或140×40几何接管。
2. 真实 socket EOF 后自动用 observer-only HELLO 接回原 target/name/PID，实际新 LIST
   同 PID 与 SNAPSHOT_DONE 才判就绪；恢复后 fresh 输入的 hits 文件恰为 `EOF\n`，一次执行。
3. 仅黑洞上行，raw shell 保持真实输出，下行 surface 仍超过5帧，旧腿仍因 LIST 不应答
   在预算内失活，普通输出不能续命。hold 下一次 HELLO 时键入 KEY/粘贴均不回放；恢复后
   同 PID、被动尺寸归属保持，新 hits 只追加一次 `BLACKHOLE\n`。
4. 再次真实断腿、hold 重试，确认 retry 已出现后退出；放开 held 不再产生新 HELLO，
   TTY 恢复，原 PTY 仍存活，不创建/kill会话。
5. finally 回收自有 PTY/代理线程/daemon，binary SHA 前后一致。

为核新身份门与恢复清理不回归上单元，再用同件执行既有
`tools/tui-application-clipboard-smoke.py --bin target/debug/homeway` 实际 **exit0、4 PASS**，
日志 `/tmp/homeway-tui-auto-reconnect-clipboard-regression.log`：同文两次精确 helper 字节、
空值、重连无回放、唯一精确终端 OSC52、被动归属与私有清理均保持，未触真实剪贴板。

两个真实烟囱均关联同一最终调试件 SHA256：
`e10982b5c366b880c01601b54327ee3631e9fa2729109ba384ee654c1f3c4dae`。
实现者最后另实际 shasum 确认一致、diffcheck exit0。主会话已独立读故障烟囱五 PASS；
完整退出码来自工具子任务真实返回，不把日志存在当作成功。未跑全workspace或外部网络，
未安装/推送/发布/部署，没有触达默认 state、生产服务或真实系统剪贴板。

## 代码评审与整改

非实现者 independent_review 在构建/真实烟囱并行期间审产品，发现1项中等问题：主循环
先 process_attach 后 poll_connection，迟到的完整 SNAPSHOT 可能先调用 ready 清15秒期限。
**接受并修复**：Recovery.expired(now)集中判断，合格 generation 的每条事件消费前检查，
connection_ready(now)成功前再次检查；模拟 now+15秒回归确保转入保屏/清未决/重试而非成功。
同时移除 surface_frame 旧的额外 complete_progress，避免预算失败的新重试提示被清。

增量代码门通过，**无剩余阻断**。原中等项已核整改与回归；其余产品检查结论：只用
observer HELLO、保留原PID、无输入重放、同腿严格 LIST、身份与快照双门、新旧片段/选区/
剪贴板清理、陈旧摘要不抢先取消、真实停止条件与封顶退避均成立。新增状态机是私有值类型，
无额外 Arc/Mutex、字符串协议错误、接口仿写、无谓拷贝或逐行移植痕迹。

新工具静态门也通过：仅 /tmp 私有 state，故障只选tui腿，包含真实默认预算静默/EOF/
上行黑洞与下行持续输出、同PID/observer尺寸、新输入一次、离线输入不回放、退出取消与
严格finally回收。同SHA、LIST应答与新快照就绪判据有真实证据。本门为独立静态审查与
日志核验，评审者未自行跑服务或测试；真实运行结果见上节，无待办项。

主会话负责更新 ROADMAP 并本地中文单元提交，本子任务不提交、不安装或部署。
