# homeway

不用账号，把家里的网络带在手上：
- 建立手机到个人电脑、多个个人电脑之间的点对点连接
- 通过手机访问内网服务，将手机请求通过内网主机转发
- 多设备间共享终端会话

## 安装

一条命令（macOS / Linux）：自动识别系统、下载最新版并校验、装进 bin 目录、询问是否开机自启动：

```bash
curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh | sh
```

手动安装：到 [Releases](https://github.com/zhaoyswd/homeway-rs/releases) 取 `homeway-<版本>-<平台>.tar.gz`
（平台：`darwin-arm64` / `darwin-amd64` / `linux-amd64` / `linux-arm64`；Windows 为 `.zip`），
`SHA256SUMS` 校验后把 `homeway` 放进 PATH 内的目录（macOS 若提示无法打开，先 `xattr -c homeway`）。

从源码构建：`cargo build --release`（产物 `target/release/homeway`；工具链见 `rust-toolchain.toml`）。


## 部署中继（可选）

当无法直连成功时，可以在有公网 IP 的机器上部署中继服务。

```bash
homeway relay start # 启动中继，显示中继token
homeway relay token # 获取中继token
homeway relay stop/restart/status # 管理/获取中继状态
```

## 部署主机

*配置中继*

```bash
homeway serve relay set 'rl1…' # 通过中继token设置中继服务（可选）
homeway serve relay clear # 清楚中继服务配置
```

*配置DDND*

```bash
homeway serve ddns add exit.example.com     # 为主机配置 ddns 域名
homeway serve ddns delete exit.example.com  # 删除 ddns 域名
```

*启动与管理服务*

```bash
homeway serve start # 启动主机端，显示主机token
homeway serve token # 获取主机token
homeway serve stop/restart/status # 管理/获取主机状态
```

主机token是连接的主机的唯一凭据，可以在其它电脑通过`homeway add host --name mymac '主机token'`进行注册，或者在手机 APP 填入进行注册。注意保管，切勿泄露。

以上参数写在配置文件（`~/.config/homeway/config.toml`）里：

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

## 使用方式

### 添加主机

在APP或电脑上将其它主机添加为出口。

```bash
# 出口机
homeway serve token                     # 获取主机token，形如：hmw2…

# 客户端机
homeway host add --name home '<hmw2…>'  # App 里则是「添加主机」粘贴同一个 token
homeway host list / status [name] / delete <name|id>
```

当只添加了一台出口主机时，以下命令不必指定主机，有多台时需要在命令后添加 `--host name`

### 测速

```bash
homeway speedtest                       # 全部主机依次测上下行
homeway speedtest --host home           # 只测其中一台
```

### 文件

默认只能管理出口主机的用户目录。

```bash
homeway files list /remote/path                     # list / stat / mkdir / read 同形
homeway files download /remote/file ./local         # upload 反向（本地在前）
homeway files get /remote/file -o ./local           # 已存在时加 --force 覆盖
```

### 终端

同一个终端会话可以在多个设备、APP打开，内容实时共享。不支持 Windows 系统。

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
