# homeway

不用账号，把家里的网络带在手上：
- 建立手机到个人电脑、多个个人电脑之间的点对点连接
- 通过手机访问内网服务，将手机请求通过内网主机转发
- 多设备间共享终端会话

## 安装

一条命令（macOS / Linux）：自动识别系统、下载最新版并校验、装进 bin 目录，然后交互询问：

- 是否停止正在运行的旧守护进程（换二进制后必须重启才生效）；
- 是否配置为后端：出口端口（默认 `38241`，同时作为 QUIC 公网端口）、文件根目录（默认用户目录）、可选的上游中继；
- 是否配置为中继：监听地址（默认 `:38341`）、对外公告地址（留空自动探测）；
- 是否添加主机、是否做 macOS 文件夹预授权、是否配置开机自启动（默认否）。

```bash
curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh | sh
```

安装脚本会引导完成下面的后端/中继配置；也可以全部跳过，之后按「部署出口 / 部署中继」手动执行。
脚本不接受命令行参数；配置只改 `~/.config/homeway/config.toml` 的相关键，保留其它内容。

手动安装：到 [Releases](https://github.com/zhaoyswd/homeway-rs/releases) 取 `homeway-<版本>-<平台>.tar.gz`
（平台：`darwin-arm64` / `darwin-amd64` / `linux-amd64` / `linux-arm64`；Windows 为 `.zip`），
`SHA256SUMS` 校验后把 `homeway` 放进 PATH 内的目录（macOS 若提示无法打开，先 `xattr -c homeway`）。

从源码构建：`cargo build --release`（产物 `target/release/homeway`；工具链见 `rust-toolchain.toml`）。


## 部署中继（可选）

当无法直连成功时，可以在有公网 IP 的机器上部署中继服务。默认监听 `:38341`；对外公告地址留空会自动探测。

```bash
homeway relay start               # 启动中继
homeway relay token               # 获取中继 token（rl1…）
homeway relay stop/restart/status # 管理中继
```

## 部署出口（主机）

在出口机上启动服务，并按需追加中继 / DDNS 配置：

```bash
# 可选：先配置中继 / DDNS，再启动出口（首次 token 就包含它们）
homeway serve relay set 'rl1…'      # 直连不通时经中继转发；rl1… 来自中继机的 relay token
homeway serve relay clear
homeway serve ddns add exit.example.com
homeway serve ddns delete exit.example.com

homeway serve start                 # 启动出口（首次需要；配置自启动后会带起）
homeway serve token                 # 获取主机 token（hmw2…）
homeway serve stop/restart/status   # 管理出口
homeway serve quality --since 10m   # 回看本机出口近期网络质量（可加 --json）
```

`serve relay` / `serve ddns` 只写 `~/.config/homeway/config.toml`，不需要出口先运行；
目录不存在时会自动创建，不需要手动 `mkdir`。启动后再改则执行 `homeway serve restart` 并重新
`homeway serve token`——中继/DDNS 端点是 token 的一部分，App 里要重新添加主机。

网络卡顿时可用 `serve quality` 查询最近时段或按设备过滤；字段、覆盖限制与用法见[出口质量历史](docs/QUALITY.md)。历史从包含此功能的新版出口启动后开始采集。

主机 token 是客户端连接出口的唯一凭据：在其它电脑执行 `homeway host add --name mymac '<hmw2…>'`，
或在手机 App 的「添加主机」里粘贴同一串。注意保管，切勿泄露。

以上配置写在 `~/.config/homeway/config.toml`；安装脚本会自动写入相关键，手动修改后重启对应角色生效：

```toml
[serve]
enabled = true
listen = 38241                 # 出口端口
quic_listen = 38241            # QUIC 公网端口；省略 = listen+1；与 listen 相同 = 单端口
files_root = "/srv/files"      # 可选：文件服务根目录
relay = "rl1…"                 # 可选：上游中继（直连不通时）
[[serve.ddns]]
domain = "exit.example.com"    # 可选：DDNS 记录

[relay]
enabled = false
listen = ":38341"              # 中继监听地址
advertise = ""                 # 留空 = 自动探测
```

### macOS 目录预授权

macOS 出口主机首次访问「文稿 / 桌面 / 下载」时会弹系统授权窗，期间远端的文件 / 终端命令会像卡住。可以在出口机上预先授权，一次长期有效：

```bash
homeway serve grant    # 触发系统授权弹窗（最多 3 次），在屏幕上点『允许』
```

- 需要先让出口跑起来（`homeway serve start`）；
- 误点了『不允许』：到 系统设置 → 隐私与安全性 → 文件与文件夹 里改回；或把 homeway 加进「完全磁盘访问」；
- 升级换二进制后需要重新授权一次。

## 作为客户端使用

`homeway` 同一个二进制也可以作为客户端：添加其它出口主机后，即可像 App 一样使用文件管理、端口转发、终端，以及 SOCKS5 代理；不需要单独安装客户端。

常用命令：

```bash
# 添加出口主机（token 来自出口机的 homeway serve token）
homeway host add --name home '<hmw2…>'
homeway host list
homeway host status home
homeway host delete home

# 文件管理
homeway files list /remote/path
homeway files get /remote/file -o ./local
homeway files put ./local /remote/path

# 终端
homeway term new work
homeway term list
homeway term attach work

# 端口转发 / SOCKS5
homeway forward add --listen 15432 --target :5432
homeway socks on --listen 1080

# 连接状态与测速
homeway status
homeway speedtest

# 自更新（GitHub Release 最新 tag；同 tag = 提示没有更新）
# 下载官方产物并按 SHA256SUMS 校验 → 烟囱 --version → 备份 → 原子替换 → 重启守护
# （launchd 主机由 KeepAlive 拉起；裸进程主机自动 setsid 自拉起；守护没跑则只换文件）
homeway update
```

只有一台出口时命令可省略 `--host`；有多台时加 `--host <name>`。

### 测速

```bash
homeway speedtest                       # 全部主机依次测上下行
homeway speedtest --host home           # 只测其中一台
```

### 文件

默认根目录是出口主机的用户目录（可在 `[serve] files_root` 修改）。

```bash
homeway files list /remote/path                     # list / stat / mkdir / read / remove 同形
homeway files remove /remote/path                   # 删除文件，或目录（连同内容）
homeway files download /remote/file ./local         # upload 反向（本地在前）
homeway files get /remote/file -o ./local           # 已存在时加 --force 覆盖
```

### 交互界面（TUI）

在 macOS / Linux 的终端中运行：

```bash
homeway tui                            # 进入交互式主机与会话界面
homeway tui --help                     # 查看操作提示
```

需要交互式终端（标准输入 / 输出均为 TTY），窗口至少 40×10；不支持 Windows。
TUI 与其它命令共用 `~/.config/homeway` 的配置和主机列表，本机服务未运行时会自动启动。
无需先开启出口，即可在「我的电脑」创建本地 shell。

#### 界面布局

- **左上：主机列表**。包含「我的电脑」和已添加的出口主机；点击主机切换其会话列表。
  本机第二行显示出口 / 中继开关；其它主机显示直连 / 中继 / 离线、连接地址、延迟及代理状态。
- **左下：会话列表**。显示当前主机的会话标题、`working` / `idle` / `blocking` / `unknown`
  状态、应用类型及项目名；点击会话即可在右侧接入。项目名取当前目录的最后一级：优先终端
  上报，未上报时由服务端采样 shell 目录；暂不可用时显示 `—`，普通 shell 切换目录后也会更新。
  末行显示 `Ctrl+B 快捷指令`（窄栏为 `Ctrl+B 指令`）；浏览历史时显示距底部行数与回底按键。
- **右侧：会话内容区**。直接操作当前 shell 或其中运行的程序；没有会话时可点「创建 shell」。
  标题栏的 `[ X ]` 用于关闭当前会话，空态时隐藏。

#### 常用操作

| 操作 | 用法 |
|---|---|
| 添加主机 | 点击主机标题栏的 `+`，输入名称和出口机 `homeway serve token` 生成的 `hmw2…`；`Tab` 切换字段，`Enter` 添加，`Esc` 取消。 |
| 启停出口 / 中继 | 右键「我的电脑」，选择「启动出口 / 关闭出口」或「启动中继 / 关闭中继」；启动后会尝试复制对应 token。端口、上游中继等配置见前面的部署说明。 |
| 启停代理 | 右键其它主机，选择「启动代理 / 关闭代理」；在本机开启通往该出口的 SOCKS5 代理，启动后会尝试复制 `socks5h://127.0.0.1:<实际端口>`。 |
| 新建会话 | 先选主机，再点会话标题栏的 `+` 或空态的「创建 shell」；自动命名并接入新会话。 |
| 切换会话 | 点击左下会话条目；键盘操作时用方向键选择，`Enter` 进入终端。切换只分离原会话。 |
| 关闭会话 | 右键会话选「关闭会话」，或点右侧标题栏的 `[ X ]`；直接结束该会话及其中程序，所有接入端都会断开。 |
| 删除主机 | 右键其它主机选「删除主机」，移除本机保存的主机，不结束远端会话。 |
| 退出界面 | 按 `Ctrl+B` 后按 `q`；只分离 TUI，会话继续运行，重新进入即可接回。 |

右键菜单也可用 `↑` / `↓` 选择、`Enter` 执行，`Esc` 取消。操作进度、错误和复制结果
显示在对应主机或会话条目的第二行；成功提示约 3 秒后恢复原信息，正常接入不显示提示。
按 `Ctrl+B` 可查看完整结果和错误详情。系统工具成功时提示“已复制”，通过 OSC52
请求承载终端时提示“已发复制请求”；是否实际写入剪贴板取决于终端的支持和允许设置。

#### 快捷键

`Ctrl+B` 打开快捷指令；**v0.3.23 使用居中弹窗**，四周保留界面，执行指令后关闭，
`Esc` 取消。以下「后按」表示先按 `Ctrl+B`，再按对应键：

| 按键 | 操作 |
|---|---|
| 后按 `h` / `s` / `Enter` | 聚焦主机列表 / 会话列表 / 当前终端。 |
| 后按 `a` / `n` | 添加主机 / 在当前主机新建会话。 |
| 后按 `m` | 打开当前侧栏所选主机或会话的操作菜单；从终端操作时先用 `h` 或 `s` 切回侧栏。 |
| 后按 `q` | 分离并退出 TUI。 |
| 弹窗内 `↑` / `↓` | 滚动查看完整状态和错误详情。 |
| 连按两次 `Ctrl+B` | 向当前会话发送一次 `Ctrl+B`。 |
| 侧栏内 `↑` / `↓`（或 `k` / `j`）、`Enter` | 选择主机 / 会话，进入所选会话的终端。 |
| 侧栏内 `+`、`m` / `F2` | 添加主机或新建会话、打开操作菜单。 |
| 侧栏内 `Tab` / `F6`、`F10` | 切换焦点、分离退出。 |
| 终端内 `Shift+PageUp` / `Shift+PageDown` | 向上 / 向下翻阅历史。 |
| 终端内 `Shift+Home` / `Shift+End` | 到最早可用历史 / 返回实时画面。 |

macOS 的 `Command+B`（`⌘+B`）在承载终端能上报该组合键时也可打开快捷指令；
该兼容入口取决于终端设置，会话内 Command 键的透传还取决于应用启用的键盘协议。
终端焦点下的普通按键、功能键和普通翻页键交给会话中的程序；返回侧栏可用 `Ctrl+B` 后按 `h` / `s`。

#### 历史与复制

滚轮浏览会话历史；应用捕获鼠标时，使用 `Shift+滚轮` 强制浏览。
浏览历史时新输出保持位置，按 `Shift+End` 返回实时画面后继续跟随；窗口尺寸或屏幕世代
变化会重新同步历史。

在终端内容内按住左键拖动选择，松开自动复制；单击不复制。应用捕获鼠标时使用
`Shift+拖动`。拖动期间固定当前可见画面，后台会话继续运行；切换会话、滚动、输入、
调整窗口或历史失效会取消选择。只复制面板内可见文本，单次上限 32 KiB；超过上限会提示失败。
SSH 或 `homeway term` 内优先通过 OSC52 请求承载终端复制，出现“已发复制请求”时需由该终端允许。

#### 主题与注意事项

深色 / 浅色模式随承载终端背景自动识别，在重新聚焦和低频查询时更新；不支持查询时读取
`COLORFGBG`，缺少提示则用深色。没有手动切换入口，也不读取旧的 `tui-theme` 文件。
完整合法的背景色应答不会进入会话输入；异常或不完整应答无法与按键完全区分，按原事件
回退并停止本次运行后续探测，优先保留用户输入。

「我的电脑」的本地会话由后台统一进程持有，关闭 / 重启出口不会结束它们；出口开启后，
远端与本地使用同一份会话列表。退出 TUI 保留会话，退出或重启后台统一进程会结束本地会话。
在 `homeway term` 的 shell 内运行 TUI 时，本机承载会话会标记为“自身”，不能在当前界面
重复接入或关闭；请选择其它会话或新建 shell。

同一个会话可以在多个设备、App 打开，内容实时共享。TUI 按服务端完整网格显示，在窄面板
裁剪，不重新折行；只看画面或缩放非活动 TUI 不会改变其它活跃端的尺寸，实际输入后再切换
到本面板尺寸。此观察接入需要目标主机的新服务端能力；若提示能力不足，更新并重启目标
后台统一进程后重试，TUI 不会自动重启已有守护。更换本机二进制后，已打开的 TUI 需退出重进。

TUI 会监看当前选中主机的会话，从其它状态进入 `blocking` 时发送“会话需要操作”通知，
包含主机名和会话标题。连续刷新不重复，恢复后再次进入会重新提醒；首次读取列表只建立
基线，不补发已有的 blocking。macOS 使用系统通知，Linux 使用 `notify-send`；SSH 或
`homeway term` 内请求承载终端通过 OSC9 通知。本机工具不可用时也回退到终端请求，
是否显示取决于系统或终端的通知设置；不会后台监看其它主机，也不会额外重复响铃。

### 终端

也可用命令行创建、接入和结束会话（macOS / Linux）：

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
