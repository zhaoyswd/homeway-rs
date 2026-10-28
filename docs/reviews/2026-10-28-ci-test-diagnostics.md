# 文档提交后 CI 两项测试诊断（2026-10-28）

发布源 `36d7ec8` 与后续文档 `865a7ed` 的 CI 已通过，纯文档提交 `940aeb4` 的
run40665157450 出现两项失败。执行日志由主会话下载至
`/tmp/homeway-v0.3.22-post-cloud-ci-failed.log`；本子任务只读源码和现有日志，不执行网络、
cargo、测试或生产动作。`36d7ec8..940aeb4` 仅改三份文档，相关生产/测试源码无差异。

## 事实与技术门

Mac 的 `snapshot_uses_one_total_deadline_for_multiple_requests` 失败于只接受
`Error::Control(code="timeout")` 的断言，日志没有打印真实返回值。生产调用链允许三种
真实超时：步骤间总预算归零为 `Error::Timeout`，拨号/欢迎预算到期为
`Error::Client(ClientErr::Timeout)`，已连接请求到期为 `Error::Control(code="timeout")`。
因此仅凭旧断言失败，不能声称生产总期限失效；也不能从该日志断言实际是哪种错误。

最小变更只在该测试接受上述三种超时并在失败时打印完整Debug结果，仍拒绝成功、
conn_closed、IO及其它错误。120ms总预算、服务端50ms延迟/500ms读预算以及
`snapshot.get` 请求/断链检查均保留，不增加sleep/retry、不改生产超时或协议。

Ubuntu 的 `request_port_forwards_rc_zero_with_live_gen_stale_minus_one` 在rc=0、
state=listening均通过后，单次TCP connect失败，原断言只留布尔值，缺少errno与运行态。
同点早在v0.3.20已有诊断，见
[`2026-10-22-quality-mac-deploy.md`](2026-10-22-quality-mac-deploy.md)。探测端口释放后的
TOCTOU存在，但不能仅凭它解释已通过listening的本轮失败；合成dial立即失败后发送RST
与现象相容，历史有限实验没有复现，因此不把RST、端口抢占或环境归因写成已证事实。

本轮仅保留一次connect Result，在失败时输出Debug/kind/raw_os_error、端口、断言前后
状态及本测试已有日志。成功集合仍为单次connect成功，没有重试、额外等待或stub变动；
在原stop步骤前显式drop该Result，维持旧临时TCP连接的资源释放位置。

release_tui 作者先提交具体设计，version 非作者只读相关调用链后签技术门通过。
变更限定两个cfg(test)函数及本记录；不改生产代码、版本、部署工件或已发布tag，官方件
不因本测试改动重签或重发。Rust直译痕迹检查无新增抽象/共享结构，错误信息仅为合成测试
的端口状态和本测日志，不取生产进程或凭据。

## 代码门与验证

**独立代码门通过**（version非作者）：完整读两份源码diff及本记录，核对改动仅在目标
cfg(test)函数，三种真实超时分类准确、原server断言和预算保留；Ubuntu错误借用/Debug/
kind/raw_os_error调用成立，失败format仅在断言失败时求值，连接不跨stop步骤。没有新增
生产/wire/常量/共享结构或逐行移植痕迹，独立 `git diff --check` 通过。

冻结源码SHA256：

| 文件 | SHA256 |
|---|---|
| `crates/homeway-cli/src/tui/backend.rs` | `df391a3cc6494236e92cdfbd39cc104afd92124ba4d712cd1a2fc8ca5f54e833` |
| `crates/homeway-core/src/facade/tun_exec.rs` | `1a45b838309ebd94b4b6d3c0f99e8925eea9955a8bd4f9d3bea027fba5b67652` |

root实际运行两个`--locked --offline --exact --nocapture` targeted：CLI **1 passed、
0 failed、exit0**，编译16.79秒/测试0.13秒；core **1 passed、0 failed、exit0**，
编译6.77秒/测试0.01秒。日志分别为`/tmp/homeway-v0.3.22-ci-deadline-targeted.log`与
`/tmp/homeway-v0.3.22-ci-port-targeted.log`。本机core通过不证明Ubuntu旧失败根因；
后续GitHub完整门及Linux新诊断仍待实际结果，不声称旧失败已复现或归因。

root额外全仓`cargo fmt --all -- --check` exit1，首先报告未改carriers_cli.rs等既有
格式差异，该check未改文件；本次`git diff --check`通过，未扩大为全仓格式重写。
实现者与评审者均未执行cargo、网络或生产动作，以上动态结果由root实际执行。

## GitHub 完整门实测

root提交并推送`10779536b1d67d8f2405b9fd249e17b8045c4ef9`，
[CI40670930060](https://github.com/zhaoyswd/homeway-rs/actions/runs/40670930060)首轮Ubuntu
完整测试及全部check/clippy/隔离门成功，Mac CLI190/0成功，两处本次targeted均在完整门
通过。Mac core另一个未改测试`stats_loop_busy_timeout_do_not_accumulate_fail_streak`
失败于原5389行，日志保留在`/tmp/homeway-v0.3.22-final-ci-failed.log`；不能省略该首红。

其现有失败序列含2/3→1/3重置。只读复核确认Busy/Timeout本身直接continue、不会改连败，
而无出站包/非稳态等其它路径本就允许清零。测试15ms发包与压缩80ms观测拍未逐拍同步
出站计数，存在调度敏感前提；现象与此相容，未采逐拍数据，不将它写为已证根因或产品
缺陷，也未改代码、阈值、ignore或成功判据。

保存首红后，root仅对同一提交失败Mac job执行一次`--failed`重跑，attempt2实际
**success**；最终Mac/Ubuntu完整测试、clippy、OHOS/双musl/Windows check与隔离门
全部成功。该单次观察没有改变旧测试未归因事实；若后续再现，应采逐拍out_pkts、steady、
gate和前后streak继续定位。正式产物仍为原tag官方件，未因test-only收尾重签/重发。
