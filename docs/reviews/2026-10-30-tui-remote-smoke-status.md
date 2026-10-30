# TUI remote smoke 两行主机状态适配（2026-10-30）

## 实际失败与范围

官方 v0.3.25 同件烟囱在 `tools/tui-remote-smoke.py:325` 等新增主机超时，实际
`/tmp/homeway-v0.3.25-official/remote-smoke.log` 为 0 PASS / exit 1。当前帧已经显示
`● tui-remote` 和 `direct 0ms proxy:of`（合法右裁）；旧 `host_ready` 仍匹配
`● 【直连】name` 等中文标题，不能识别已发布的两行状态。不是产品连通失败的证据。

本批只改工具和离线回归，不改产品代码、版本、官方 Release 或签名。保留 current-frame
同步、原 15/25 秒预算、精确 hostname、selected/bold、remote online 门及私有实例纪律。
用户已明确本轮不部署/重启本机；官方件仅在 `/tmp` 私有 state 跑烟囱。

## 技术门

独立 `deploy_v0325_prepare` 已实读旧 `host_ready`、view.rs 标题/详情绘制和失败当前帧，
技术门通过，无阻断。采纳：精确标题 `● name`；主机面板内相邻详情状态；remote 仅
完整 `direct/relay/offline` 首词、online 排除 offline；local 匹配 exit/relay；保留
overlay 拒绝、会话边界与物理列 2 的 bold 判定，合法尾部裁切不能阻断。

实现按实际 sidebar 物理宽度读取当前标题/详情，要求原左右边框；local 四种布尔组合、
remote RTT/proxy 语法按同一可见宽度裁切后精确比较；窄屏若裁在 RTT，仅接受填满真实
右界的合法 token 前缀，online 仍须完整路径状态词。没有读取旧 output、放宽等待或泛化
搜索屏幕文字。新增标准库 unittest/现有 pyte 的离线当前帧回归，无额外依赖和服务。

## 实际验证与代码门

实际日志目录 `/tmp/homeway-tui-remote-status.OoJggf/`。

- `python3 tools/test-tui-remote-status.py`：exit0，6 tests / 0 failures；
  `unit-first.log` 和最终 `unit-final.log`。包括 40/70/100/140 列，完整 direct/relay/
  offline 与 online 门、proxy/RTT 合法右裁、local 四种角色组合与 Unicode 标题、
  选中/非选中、错名/旧中文标题/错相邻详情、dialog、终端正文/会话列表/旧 output 拒绝。
- Python `compile` 两文件及 AST 对比：exit0（`static-check.log`），确认除了旧
  `host_ready` 与新增 `host_status_visible`，原脚本编排、current-frame 同步与预算不变。
- `git diff --check`：exit0（`diff-check.log`）。没有全仓格式化或产品构建。

独立 `deploy_v0325_prepare` 最终代码门通过，无阻断。实读 diff、fixture、view.rs 的
Option<i64> RTT 绘制与记录，另实际执行上述 unittest 6/0 和 diff-check0，并独立 AST
逐函数比较确认原编排及预算无变化；精确面板/标题/邻行/bold 与合法右裁边界适当。
审查者未运行服务、官方件或生产。本轮实际原始 0 PASS 保留。

工具已可用原 SHA 前缀 `d1431158` 的官方 v0.3.25 二进制重跑；真实官方件烟囱由 root
原件执行并另记实际结果，本子任务不将离线测试当成真实 smoke 通过。

## Root 同官方件实际复验

root 使用原官方 `d1431158902a15e0283295c16b6860e9926d57fefed3b3a6d8d51aa82eb2c3f9`
二进制单次重跑 `python3 tools/tui-remote-smoke.py --bin
/tmp/homeway-v0.3.25-official/darwin-arm64/homeway`，session45535 **exit0、8 PASS**。
日志 `/tmp/homeway-v0.3.25-official/remote-smoke-status-r1.log`：精确新增/选中远端主机、
创建并输入终端 marker、cwd、SOCKS 65,536B QUIC echo、停代理/删主机保会话、私有本地
出口/中继启停及 shell 保留、detach/收尾均通过。原 `remote-smoke.log` 的0PASS失败保留；
所有实例均私有，未换装/重启本机正式进程，未改或重签发布二进制。
