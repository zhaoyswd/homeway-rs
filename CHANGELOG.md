# Changelog

## v0.2.1（2026-10-05）

生产可观测性优先批（B0-2a，GAP-AUDIT P1-1 收口）：

- **文件日志体系（P1-1）**：events.log（摘要，2MB×3 轮转）+ debug.log（细节，8MB×2 轮转）
  双文件落 `<state>/cache/`——统一进程与前台 `serve` 两形态都接线；`peer: +/-/~`、
  `dns: q=`、`intercept: …（dialok）` 判据族自此生产可查（对齐 Go `internal/logfile` 语义：
  O_APPEND 续写、超限轮转最旧删、失败丢段重试自愈）。统一进程 stdout+文件双写不变
  （launchd stdout 重定向继续可用）。
- **token 兜底重试**（DEPLOY-RUST-EXIT §5 登记缺口修复）：首轮公网端点探测结束
  （成不成都算）后 10×1s 重试铸出 token（Go `role.go` 终端兜底 goroutine 同义）——
  修复「公网端点暂不公布（端口改写/无证据）形态下重启后 token 恒缺中继端点」。
- **files 动词别名（P1-7 半边）**：`get`/`put` = `download`/`upload` 双名等价
  （Go 脚本契约名不破）。
- **token 台账吊销分支告警（P1-5 收口）**：append 吊销拒写时打专用行
  （「在用凭证已被吊销…本轮 token 未入台账…」）。

## v0.1.0（2026-10-05）

首个公开仓版本（转正 A 批）。R0–R8 完成：三角色（出口/客户端/中继）与 Go 版 homeway
任意组合互操作（线协议字节对齐：golden 夹具 + 对照向量 + 词表三方门）；CLI 命令面
`token / connect / serve / relay / speedtest / files / dnstest / portfwd`；E2E 完成真机
连现役 Go 出口全链冒烟（对账快照零回归）；性能 harness PERF-AB 收口（RFC 合规 CUBIC、
出口发送整形、UDP 批化）。

发布产物 = `homeway-cli` 四目标二进制（darwin-arm64 / darwin-amd64 /
linux-amd64(musl 静态) / linux-arm64(musl 静态)），tar.gz + SHA256SUMS 随 Release 发布。
