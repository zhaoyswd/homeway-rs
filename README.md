# homeway

「回家」网络工具：把手机 / 电脑的全部 IPv4 流量经**自建出口节点**出网。
**三角色（出口 / 中继 / 客户端核）由同一个二进制 `homeway` 供给**，纯 Rust 实现。

- **出口（`serve`）**：QUIC 端点 + 设备表（token 凭证）+ 过境拦截（TCP/UDP 五元组终结后
  本机重拨）+ files / 终端 / DNS 代答 / 测速 / 端口转发 / DDNS / UPnP / STUN。
- **中继（`relay`）**：信封转发 + 准入的轻量角色；出口与客户端打不通直连时自动经它中转。
- **客户端核**：`crates/homeway-capi` 的 C-ABI 壳（产物 `libclientcore.so`，20 个
  `ClientCore*` 导出），供鸿蒙 App 的 VPN 扩展进程加载。
- **选路与自愈**：直连优先（中继纯后备）；连接中换网自动迁移；断链自动恢复
  （快探 → 复探 → 迁移 → 重连）。
- **平台**：macOS（arm64 / amd64）、Linux（amd64 / arm64，静态）、Windows（amd64）。

## 安装

一条命令（macOS / Linux）：自动识别系统、下载最新版并校验、装进 bin 目录、询问是否开机自启动：

```bash
curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh | sh
```

`curl | sh` 形态无交互、不会配自启动；要配的话下载后带参跑：`sh install.sh --autostart yes`
（其它参数：`--version vX.Y.Z` 装指定版本、`--bin-dir DIR` 指定目录；缺省 `/usr/local/bin`
可写就用它，否则 `~/.local/bin`）。装完会打印最常用的几条命令。

手动安装：到 [Releases](https://github.com/zhaoyswd/homeway-rs/releases) 取 `homeway-<版本>-<平台>.tar.gz`
（平台：`darwin-arm64` / `darwin-amd64` / `linux-amd64` / `linux-arm64`；Windows 为 `.zip`），
`SHA256SUMS` 校验后把 `homeway` 放进 PATH 内的目录（macOS 若提示无法打开，先 `xattr -c homeway`）。

从源码构建：`cargo build --release`（产物 `target/release/homeway`；工具链见 `rust-toolchain.toml`）。

## 运行

```bash
homeway                                 # 统一进程：按配置装配出口 / 中继（推荐）
homeway serve                           # 前台单出口（Ctrl-C 收工）
homeway relay                           # 前台单中继
```

启动参数写在配置文件里，之后零参启动即可：

```bash
homeway serve --listen 38241                            # 起出口
homeway relay --listen :38341 --advertise <公网IP>:38341   # 起中继（对外公告地址）
homeway relay token                                     # 取中继凭据 rl1…（出口侧 --relay 用它）
homeway serve --relay '<rl1…>'                          # 出口经该中继可达（打不通直连时走它）
```

参数写进 `homeway` 的配置文件（`~/.config/homeway/config.toml`）后，`homeway`（零参）
与 `homeway serve start` 都按它装配。新机器的配置缺省 `enabled = false`——不会因为跑了一条
命令就意外成为出口；**出口机执行一次 `homeway serve start`**，它会写入 `enabled = true`
并装配，之后零参 `homeway`（launchd / 服务）也按它起出口：

```toml
[serve]
enabled = true
listen = 38241
files_root = "/srv/files"      # 可选：文件服务根目录
relay = "rl1…"                 # 可选：中继凭据
[[serve.ddns]]
domain = "exit.example.com"    # 可选：DDNS 记录

[relay]
enabled = false
```

### 起停

```bash
homeway serve start | stop | restart | status | token     # 出口（后台，不占终端）
homeway relay start | stop | restart | status | token     # 中继
homeway status                                            # 运行面（连接 / 流量 / 计数）
```

## 主机

出口凭证是连接的唯一凭据：在出口机上取 token，在客户端（App 或命令行）把它加为主机。

```bash
# 出口机
homeway serve token                     # 输出 hmw2… 凭证

# 客户端机
homeway host add --name home '<hmw2…>'  # App 里则是「添加主机」粘贴同一个 token
homeway host list / status [name] / delete <name|id>
```

命令行工具默认用本机主机：**只有一台主机时不必指定**；有多台时用 `--host <name|id>`
选中其中一台；还没有主机时会提示先 `homeway host add`。

### 测速

```bash
homeway speedtest                       # 全部主机依次测上下行
homeway speedtest --host home           # 只测其中一台
```

### 文件

```bash
homeway files list /remote/path                     # list / stat / mkdir / read 同形
homeway files download /remote/file ./local         # upload 反向（本地在前）
homeway files get /remote/file -o ./local           # 已存在时加 --force 覆盖
```

### 终端

```bash
homeway term new work                   # 新建并直接进入 work 终端（Ctrl-b d 分离）
homeway term list                       # 会话列表
homeway term attach work                # 分离后再接入
homeway term delete work                # 结束会话
```

### 端口转发

```bash
homeway forward add --listen 15432 --target :5432    # 本机 127.0.0.1:15432 → 出口侧同主机 :5432
homeway forward list                                 # 规则与运行态
homeway forward delete --listen 15432
```

`--target <IP>:<端口>` 可指定出口侧的目标地址（缺省 = 出口自己同端口）。

### SOCKS5 代理

把浏览器或任意支持 SOCKS5 的程序的流量经出口出网（域名在出口侧解析）：

```bash
homeway socks on --listen 1080     # 本机 127.0.0.1:1080 起 SOCKS5，流量走出口机
homeway socks status               # 开关态 / 端口 / 连接数
homeway socks off                  # 关掉
```

多台主机各开一个端口（`--host <name|id>` 指定），浏览器按端口选走哪个出口。

## Windows

三角色可用；`term`（终端）不支持。

## 构建与自检

```bash
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
tools/ci-local.sh                       # 本地一键门（测试 / clippy / 向量 / 词表 / 矩阵冒烟）
```

发版 = 推 `v*` tag 触发 Release CI（五目标产物 + `SHA256SUMS`），见 `.github/workflows/release.yml`。

## 许可

MIT，见 [LICENSE](LICENSE)。
