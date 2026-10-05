# Changelog

## v0.1.0（2026-10-05）

首个公开仓版本（转正 A 批）。R0–R8 完成：三角色（出口/客户端/中继）与 Go 版 homeway
任意组合互操作（线协议字节对齐：golden 夹具 + 对照向量 + 词表三方门）；CLI 命令面
`token / connect / serve / relay / speedtest / files / dnstest / portfwd`；E2E 完成真机
连现役 Go 出口全链冒烟（对账快照零回归）；性能 harness PERF-AB 收口（RFC 合规 CUBIC、
出口发送整形、UDP 批化）。

发布产物 = `homeway-cli` 四目标二进制（darwin-arm64 / darwin-amd64 /
linux-amd64(musl 静态) / linux-arm64(musl 静态)），tar.gz + SHA256SUMS 随 Release 发布。
