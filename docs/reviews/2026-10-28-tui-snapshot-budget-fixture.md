# TUI 快照总预算夹具与 KILL 失败诊断（2026-10-28）

基线 `1987612`。v0.3.23 尚未打 tag。root 已对该提交失败的 Mac job 有界重跑一次，
attempt2 仍红，停止盲目重跑。本单元仅改 cfg(test) 夹具与失败诊断，不改产品、版本、
冻结部署工件、发布主报告或 ROADMAP；实际定向/CLI/CI 验证由 root 执行。

## 真实失败与范围

- 首轮 GitHub CI40696331454：Ubuntu及全部 check/clippy/隔离成功，Mac CLI190/0通过，
  core929/1/6ignored。`sentinel_spaced_not_cross_generation` 在 KILL 后等 OK(0x0b)
  见 UnexpectedEof，尚未到同名重建/跨代尺寸断言。日志
  `/tmp/homeway-v0.3.23-main-ci-first-failure.log`。
- 同提交 attempt2：Mac CLI189/1/1ignored，快照预算 test 的 server 对第二个
  control_read(None) unwrap（backend.rs1065）失败，parent join unwrap1084连带失败；
  原三种 timeout 分类断言已经通过。日志
  `/tmp/homeway-v0.3.23-main-ci-second-failure.log`。该轮止于CLI，没有core复验通过证据。
- 原快照夹具120ms预算包括握手/线程启动及首50ms响应，合法耗尽预算后不能无条件假定
  第二请求必到。修正必须实际覆盖多个请求，不能将首请求超时当多请求总预算验证通过。
- KILL 的旧静态竞态候选：先 signal，再 enqueue OK；并发 pump 若先 finish_ended，
  closed_for_prod 会拒绝 OK，expect(OK)可能跳过ENDED后见EOF。失败日志无逐帧/队列时序，
  尚未因果复现或归因，不将它称为环境问题，不猜改 core 产品；本轮只增强错误取证。

## 技术门（完成）

版本作者 `release_0323/version` 提设计，部署作者 `release_0323/deploy` 独立实读日志、
Worker共享deadline与ControlClient语义后通过技术门。仓库reviewer此前因dsh profile写
EPERM不可用，沿用ROADMAP独立两门fallback，不重试提权。

- backend 先预握手，将既有 client 放入 worker.control，再起2秒测试预算。服务器必须
  按序收到真实 host.list 与 snapshot.get；首请求等1秒响应，第二请求不响应，服务端
  read等待1.5秒。正确剩余预算约1秒先超时，parent释放worker后服务端须精确读到EOF；
  错误逐请求重置2秒则由服务端先关闭，conn_closed/IO不能冒充timeout通过。
- 原 Timeout、ClientErr::Timeout、Control timeout 三分类保留，拒绝成功/conn_closed/IO，
  不依赖瞬时elapsed阈值，缺少第二请求必须失败。2秒仅为该夹具预算，产品3秒预算不变，
  不是单纯把原测试超时调大。
- service.rs的cfg(test) Client.expect仅在本地记录最近16个u8 opcode、是否截断及是否
  曾见ENDED。只在原读失败/ERROR panic追加证据，不带payload或凭据；原预算、读写、
  过滤、返回与断言不变。此诊断不宣称修复KILL竞态。

代码门补查发现原候选 accept/join/response-write 没有独立期限，不能笼统称全有界。
作者最小补齐设计再经非作者增量技术门：accept非阻塞轮询及5秒watchdog，accepted
socket显式改回blocking并设每次read/write 5秒；drop(worker)后以10秒watchdog等待
server.is_finished，再join已完成线程。这些清理watchdog不参与预算行为判据。

## 代码门（完成）与等待边界

非作者完整实读最终diff，独立Python逐字核对：除backend目标cfg(test)函数（含局部
read_request helper）和service的cfg(test) Client.expect外，两文件所有其它字节均同
HEAD，完整产品区域逐字不变。移除service局部诊断变量、记录操作及panic附言后，精确
恢复原expect。代码门通过，无待处置项；无新增产品Arc/Mutex、字符串错误、接口仿写、
拷贝或逐行移植问题。

| 文件 | 冻结 SHA256 |
|---|---|
| `crates/homeway-cli/src/tui/backend.rs` | `e00f3e23bc07fbdce1b9336b83f429b106419b9b70a26974111ef6037ac66f08` |
| `crates/homeway-core/src/term/service.rs` | `219797036917d5c63aa01d6a44a275300973359cb68ea220fbebd79a0734977f` |

作者backend rustfmt首轮仅assert排版check1，修正后check0；service只emit stdout作语法
解析exit0，没有整文件重排。作者与非作者git diff --check均实际exit0。上述是静态
校验，没有编译或执行测试，不能记为运行通过。

夹具的同步点为5秒dial完成HELLO/WELCOME并获得ControlClient之后，才构建
Worker(Some(client))及开始snapshot的2秒预算。服务端严格解析两个Req及操作顺序，
第二请求缺失必须失败；parent先drop(worker)关闭连接，再等待服务端完成，最后同时
断言原timeout分类和服务端raw read真正Ok(0)。错误按请求重置预算即使因调度最终也
报timeout，服务端先到read期限返回Err仍会被第二断言拒绝，不会把Option::None当EOF。

各等待和清理边界如下，不以任何瞬时elapsed阈值判断snapshot语义：

| 阶段 | 边界 |
|---|---|
| server accept | 非阻塞、每5ms轮询、5秒watchdog，WouldBlock/Interrupted受同一检查 |
| client握手 | dial_with_timeout 5秒，在snapshot预算之前完成 |
| server常规读写 | accepted显式blocking，单次read/write timeout各5秒 |
| 首请求响应延迟 | 固定sleep 1秒，已实际读到host.list后开始 |
| snapshot总预算 | 2秒，两个真实请求共用 |
| 第二请求后server等待 | 单次raw read timeout 1.5秒，不响应snapshot.get |
| parent收尾 | drop(worker)后每5ms检查is_finished、10秒watchdog，完成后才join |

这些是夹具显式I/O/轮询边界，不声称整帧read_exact、OS线程回收或remove_dir_all系统
调用另有独立绝对期限。正常共享总预算约在第二请求后剩1秒先超时并关闭；错误重置
2秒或旧3秒预算由server的1.5秒先关闭，严格timeout和真EOF双断言使其失败。

opcode诊断为**每一次expect调用**的局部历史，只从该次等待成功读到的帧记录opcode
（含被跳过帧和ERROR），仅保留尾16项，读取失败本身没有opcode。尾16个u8满后pop_front；truncated
显示前缀是否省略，saw_ended独立保留曾见ENDED，即使其已滑出尾16也不丢证据。新增
panic字段为最近opcode（十六进制）、截断、曾见ENDED；不新增payload/凭据输出。

## root实际验证入口（待执行）

```sh
cargo test -p homeway-cli tui::backend::tests::snapshot_uses_one_total_deadline_for_multiple_requests --offline -- --exact
cargo test -p homeway-cli --offline
cargo test -p homeway-core term::service::tests::sentinel_spaced_not_cross_generation --lib --offline -- --exact
```

root另计划对sentinel exact作小有界重复批次（如16轮），保留任何首红的opcode取证。
绿只表示该批未复现，不表示KILL候选根因已修复；若再现先按帧序据实定位再定单独产品
方案。准备子任务未执行cargo、测试、网络、生产、提交，未修改主报告或ROADMAP。
真实运行结果由root完成后追加。

## root实际复验

root在两文件冻结后实际执行`cargo test --workspace --locked --offline`，**exit0、顶层
1447 passed / 0 failed / 40 ignored**，新预算夹具与sentinel用例均包含在完整运行中。
日志`/tmp/homeway-v0.3.23-after-fixture-workspace.log`。随后workspace/all-targets
clippy `-D warnings` **exit0、4.01秒**，日志
`/tmp/homeway-v0.3.23-after-fixture-clippy.log`。产品区域逐字不变，原完整quick的非测试门
与316秒私有矩阵证据仍适用，不把它改写为此后又完整重跑了一遍quick。

root从上述Cargo实际输出取得同一core测试二进制，SHA256
`68275dd3b593807037c351cb7db64892f9835edb9d5ebdf877f9a5c9af132e47`，做有界重复采证。
第一批原计划16轮/并行4，实际首组4轮为3成功、1失败后立即停止；回执
`/tmp/homeway-v0.3.23-sentinel-1n2cnocb/summary.json`。失败round01在fixture目录创建
处errno17/AlreadyExists、0.00秒，尚未进入connect/KILL/expect，不是KILL竞态复现。
源码目录名仅prefix+进程内计数+毫秒，不保证跨独立进程隔离；现象与并发命名冲突一致，
但未记录失败路径，不把具体冲突对象写成已证实。

不改产品或测试源码，root仅将操作harness改为串行，独立新批实际 **16/16成功、0失败**；
回执`/tmp/homeway-v0.3.23-sentinel-serial-wjevr1ex/summary.json`，二进制前后SHA相同。
两批日志分别保留，不把首批写成16轮通过，不把串行未复现写成修复KILL候选。新增诊断
仍用于后续CI实际取证；GitHub完整门尚待本次修正提交后运行。
