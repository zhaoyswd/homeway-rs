# Changelog

## v0.2.2（2026-10-07）

P1 收口 + 简洁化删除批（出口发送线程默认开启并成为唯一发送路径）：

- **发送线程默认 on 并唯一化**（多因子框架裁定：架构合理性/实际性能/理论性能/设备负载/
  复杂度五维，覆盖原纯经验 15% 收益门——裁定记录 = ROADMAP P1 终档「P1 默认开启
  裁定」+ PERF-AB §9.17 注记）：密文→sendto 从驱动线程独立（浅拆）。裁定输入：
  真机稳态 +7% 从未负值、回环 lo0 +25%、收益随更强网卡放大、复杂度已沉没、
  默认 off 的死代码路径反成维护负担。**简洁化删除（v0.2.2 双裁定之二）**：
  Queued 为唯一路径——内联直发形态、TxMode 双模式/降级切换（takeover_consumer）、
  `HOMEWAY_TX_SENDTHREAD` 消融臂整体删除；发送线程无条件起（socketpair/spawn/
  dup 失败 = 出口起不来，与 `expect("spawn serve driver")` 同款纪律）；发送线程
  panic 后无内联接管，ring 满丢（丢新，TCP 尾丢语义）+ 重传兜底。
- **启动判据新行**：`发送线程：就绪（homeway-serve-tx，单轮排空上界 …KiB）`——
  此前该面只有流量驱动的 5s 观测行（`UDP 出站[发送线程]`），启动期无判据可查。
- **内层 MTU 机器删除**（P2 同框架裁定维持 1280 + 简洁化裁定）：手机侧升档门
  （mtu_gate.rs DF 探针/三条件门/mtuEff）与出口侧 `inner_mtu`（config/flag/env）
  全链删除，恒 1280（坑 4/23 保守值）；tier 侧 tunMtu 注入/扩展键同批退役。
- **pacing 时刻表删除（令牌桶整形本体保留）**：逐包时刻表（adaptive/fixed、
  `HOMEWAY_TX_PACING`/`HOMEWAY_TX_PACE_MBPS`、est 估计器、补账量子）删除——
  默认 off 且受益形态窄；承重的令牌桶（R/burst 团块钳制 + `HOMEWAY_TX_SHAPING`
  off 逃生口 + `HOMEWAY_TX_DBG` 诊断面）原样保留。
- **测量遗留 env 清理**：`HOMEWAY_CC`、`HOMEWAY_UDP_NO_BATCH` 删除（纯消融臂，
  结论已入档 PERF-AB）；`HOMEWAY_BINDWATCH_PROBE`（测试缝）保留。
- P2（内层 MTU 1380）同框架裁定**维持 opt-in 不可达**（失败族是坑 4 连通性黑洞 +
  常见形态零收益——PERF-AB §9.14 注记）：删除后 1280 为唯一档。

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
