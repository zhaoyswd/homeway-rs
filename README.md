# homeway

「回家」网络工具：把手机 / 电脑的全部 IPv4 流量经**自建出口节点**出网。
**三角色（出口 / 中继 / 客户端核）由同一个二进制 `homeway` 供给**，纯 Rust 实现。

- **出口（`serve`）**：QUIC 端点（单 UDP 端口，支持连接迁移）+ 动态设备表（token 凭证）
  + 过境拦截层（TCP/UDP 五元组终结后本机重拨）+ files / 终端 / DNS 代答 / 测速 /
  端口转发 / DDNS / UPnP / STUN。
- **中继（`relay`）**：信封转发 + 准入，零 QUIC 依赖的轻量角色；出口与客户端打不通直连
  时自动经它中转。
- **客户端核**：`crates/homeway-capi` 的 C-ABI 壳（产物 `libclientcore.so`，20 个
  `ClientCore*` 导出），供鸿蒙 App 的 VPN 扩展进程加载；核内 = QUIC 连接岛（连接 / 准入 /
  候选赛跑 / 迁移 / 恢复阶梯）+ TUN 读写 + 服务流分发。
- **选路与自愈**：**直连优先**（窗口 2s，中继纯后备）；连接中换网走本地重绑（rebind）
  迁移；断链走「快探 → 复探 → 迁移 → 重连」阶梯。
- **平台**：macOS arm64 / amd64、Linux amd64（musl 静态）/ arm64（musl 静态）、
  Windows amd64（无终端能力）；鸿蒙核为 OHOS 交叉产物。

## 安装

发版产物 = GitHub Release 的 `homeway-<tag>-<平台>.tar.gz`（Windows = `.zip`）+ `SHA256SUMS`。
平台名：`darwin-arm64` / `darwin-amd64` / `linux-amd64` / `linux-arm64` / `windows-amd64`。

```bash
# 以 darwin-arm64、tag v0.3.3 为例（其余平台换产物名）
curl -LO https://github.com/zhaoyswd/homeway-rs/releases/download/v0.3.3/homeway-v0.3.3-darwin-arm64.tar.gz
curl -LO https://github.com/zhaoyswd/homeway-rs/releases/download/v0.3.3/SHA256SUMS
grep darwin-arm64 SHA256SUMS | shasum -a 256 -c -
tar xzf homeway-v0.3.3-darwin-arm64.tar.gz
sudo install -m 0755 homeway /usr/local/bin/homeway     # 装进系统 PATH
homeway --version
```

Windows：解压 `.zip`，把 `homeway.exe` 放进 PATH 内的目录。

从源码构建：

```bash
cargo build --release                  # 产物 target/release/homeway
cargo build --release -p homeway-cli   # 同上（包名 homeway-cli，产物名 homeway）
```

工具链版本见 `rust-toolchain.toml`；鸿蒙核的 OHOS 交叉配方见 `AGENTS.md`「技术底座」。

## 运行

**正式部署 = 参数最简**：二进制在 PATH 内，零参 / 子命令直接跑，**不带 `--state`**——
state 走缺省目录 `$HOME/.config/homeway`。`--state <dir>` 只用于测试、多实例或临时验证。

```bash
homeway                                 # 统一进程：client/control 恒开 + serve/relay 按 config 期望态装配
homeway --verbose                       # 同上，多打过程行

homeway serve                           # 前台单出口（Ctrl-C 收工）
homeway serve --listen 41641 --relay 'rl1…'   # 首次起出口常用参数（之后写进 config.toml，零参即可）
homeway relay --listen :41741 --advertise 203.0.113.7:41741   # 前台单中继
```

`<state>/config.toml`（= `$HOME/.config/homeway/config.toml`）是**期望态**：
`[serve]` / `[relay]` 的 `enabled` / `listen` / `advertise` / `files_root` / `[[serve.ddns]]`
等写一次，之后 `homeway`（零参）与 `homeway serve start` 都按它装配。

### 起停与运行面

```bash
homeway serve start | stop | restart | status | token     # 出口（控制面，不占前台）
homeway relay start | stop | restart | status | token     # 中继
homeway status [--json] [--watch]                         # 运行面聚合（link / 统计 / 计数）
homeway export <dest.tar> | import <file> | reset cache   # 状态工件：备份 / 迁移 / 清 cache
```

### 凭证（token）与主机管理

```bash
homeway serve token                     # 出口凭证 hmw2…（粘进 App「添加主机」，或喂给下面的命令）
homeway serve token list | revoke <id>  # 台账 append-only：吊销即时生效
homeway host add --name home '<hmw2…>'  # 本机客户端加一台主机（daemon 托管）
homeway host list | status [name] | delete <name|id> [--yes]
```

token 是**凭证**：不内置、不进仓库、不随包分发。客户端 token 前缀 `hmw2`；中继凭据前缀 `rl1`。

### 客户端命令面

```bash
homeway connect --token '<hmw2…>'                     # 服务会话（无 TUN）：最直接的连通性验证
homeway speedtest [--host home] [--json]              # 隧道上下行测速（无 --host = 全部主机轮转）
homeway files upload --host home <本地> <远端目录>     # files：list/stat/mkdir/read/get/put/download/upload
homeway files <verb> --token '<hmw2…>' …              # 同命令的裸客户端形态（不落主机）
homeway term new work -d && homeway term attach work  # 远程终端（Ctrl-b d 分离；Windows 不支持）
homeway term list | delete <name> | explain …
homeway portfwd --token '<hmw2…>' --map 15432:5432    # 本机 TCP 端口转发（--map 15433:10.0.0.5:5432 可指定目的）
```

`homeway status --json` / `homeway speedtest --json` / `homeway host list --json` 出机器可读输出。

### Windows 注记

- 三角色编译可用；**终端（`term`）不支持**。
- 停止信号 = 控制台事件（Ctrl-C / Ctrl-Break / 关窗）；SIGTERM 不可捕获 ⇒ 服务管理走控制面
  （`homeway serve stop` / `homeway relay stop`）。
- 控制面本地承载 = 回环 TCP + `<state>/control.port`（unix = `control.sock`；协议层一致）。

### 迭代 / 测试形态（多实例）

联调、矩阵、性能 A/B 一律用**私有实例**：显式 `--state /tmp/…` + 独立端口，绝不碰正式实例。

```bash
tools/local-rust-exit.sh start 1      # 起本地私有出口（/tmp state、4265x 端口）
tools/local-rust-exit.sh token 1      # 取它的 token
tools/local-rust-relay.sh start 1     # 起本地私有中继
target/release/homeway connect --token '<上一步 token>' --state /tmp/hw-test   # 测试才带 --state
```

## 构建与自检

```bash
cargo build --release                       # 本机（macOS / Linux）
cargo test --workspace                      # 单测 + 集成
cargo clippy --workspace --all-targets -- -D warnings
tools/ci-local.sh                           # 本地一键门（测试 / clippy / 向量 / 词表 / 矩阵冒烟）
tools/matrix.sh --smoke                     # 本机互操作矩阵冒烟
```

发版 = 推 `v*` tag 触发 Release CI（五目标产物 + tag 纪律门 + `SHA256SUMS`），见
`.github/workflows/release.yml`。

## 许可

本仓**暂未声明许可**（无 `LICENSE` 文件）。在明确声明之前，请勿假定任何再分发或商用条款
——有需要请先联系仓库所有者。
