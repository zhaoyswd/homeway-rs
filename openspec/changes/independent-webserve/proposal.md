## Why

用户需要通过自己的 HTTPS 域名，在浏览器中操作指定 homeway 主机的完整 TUI；已有出口、中继端口只承担网络协议，没有浏览器入口。Web 入口应仅监听本机，并与出口、中继分别启停，覆盖没有任何网络角色的本机使用。

## What Changes

- 增加 `homeway webserve start/stop/restart/status`，独立固定端口，仅监听 `127.0.0.1`，通过用户自配反代提供 HTTPS/WSS。
- 增加 `homeway webserve token [rotate]`，获取独立、稳定的实例 Web 管理凭证；命令不启动 Web、出口或中继服务。
- 浏览器使用内嵌 xterm.js 展示目标机上独立 PTY 中的现有 `homeway tui`；关闭页面仅回收界面，保留真实工作会话。
- 本机 Web 入口始终允许本机凭证；本机同时运行中继时，也允许有效注册目标的凭证，最终由目标认证。无中继的入口不接受远程目标。
- 出口运行时经现有 QUIC 承载提供独立管理准入，不要求启动本机 HTTP/WS。管理凭证不能作为网络设备准入凭证使用。
- 在统一进程中独立管理 Web 生命周期；全新 Web-only 启动沿用现有出口/中继关闭默认，不隐式启用网络角色。保留既有普通启动默认值和网络协议字节。

## Capabilities

### New Capabilities

- `webserve`: 本机 Web 入口、命令配置、webtoken、浏览器 TUI、管理认证和中继目标访问。

### Modified Capabilities

无。现有联网准入、终端服务与网络 token 语义保持；新增管理协议和验收行在同批登记。

## Impact

- CLI：命令分发、独立严格 webserve.toml、统一进程角色控制、静态网页、HTTP/WS、PTY 与凭证持久化。
- QUIC 叶子 crate：首流管理分支、独立认证、同步入口和中继管理拨号；保留已有 TLS/RPK 和无 ALPN 协商形态。
- core：中继有效注册查询、管理准入后的中继腿确认及统一装配缝；不把 HTTP、xterm.js 或 PTY 网页依赖引入 C-ABI 核。
- 新增 Rust HTTP/WS 依赖限 CLI Unix 图；前端依赖打包进单二进制，离线运行不使用 CDN。
- 本轮只提交本地代码、规格、评审与私有实例证据；不修改生产进程、其他仓库，不推送或发布部署。
