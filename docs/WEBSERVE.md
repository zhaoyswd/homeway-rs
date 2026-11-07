# 浏览器终端与 WebServe

WebServe 在网页终端中运行目标主机的现有 `homeway tui`。每个页面拥有自己的界面、选择和
窗口尺寸，读取目标已有的主机与工作会话。功能在包含本次实现的版本中可用；源码验收不代表
已经发布或安装到系统 PATH。

## 开启一个浏览器入口

```sh
homeway webserve token
homeway webserve start --port 18080 --origin https://home.example.com
homeway webserve status
```

`token` 只读取或生成本机的稳定 Web 管理凭证，不开启 Web、出口、中继或其他网络监听。
`start` 在统一进程中启用独立 Web 入口；全新主机的出口、中继保持关闭，已有角色配置保留。
不配置公开域名时可以省略 `--origin`，直接从本机打开 `http://127.0.0.1:18080`。

端口只接受数字，地址固定为 `127.0.0.1`。端口占用时明确失败，不自动换端口。
独立配置保存在 `~/.config/homeway/webserve.toml`，不会给旧的 `config.toml` 添加新字段。

```sh
homeway webserve restart --port 18081 --origin https://home.example.com
homeway webserve stop
homeway webserve token rotate
```

运行中重复 `start` 同一配置返回已运行；修改端口或域名使用 `restart`。`stop` 撤销该入口
发起的浏览器连接，不停止出口、中继或真实工作会话。轮换会使本机旧 webtoken、对应票据和
已登录管理连接失效；重新取 token 后再登录。

## 配置自己的反代

下面的 NGINX 模板将同机的 HTTPS/WSS 请求转发到本机 WebServe。替换域名和证书路径；
证书申请、更新及反代运维由用户负责。`map` 放在 NGINX 的 `http` 配置块中。

```nginx
map $http_upgrade $homeway_connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    listen 443 ssl;
    server_name home.example.com;
    ssl_certificate     /path/to/fullchain.pem;
    ssl_certificate_key /path/to/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:18080;
        proxy_http_version 1.1;
        proxy_set_header Host $http_host;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $homeway_connection_upgrade;
        proxy_read_timeout 75s;
        proxy_send_timeout 75s;
        proxy_buffering off;
    }
}
```

浏览器访问 `https://home.example.com` 并输入 webtoken。公开 origin 必须与启动时配置一致；
使用非默认 HTTPS 端口时，在 origin 中保留该端口。服务端不通过 `X-Forwarded-*` 自动猜测
允许的域名。WS 升级头需要显式转发，配置方式见 [NGINX 官方说明](https://nginx.org/en/docs/http/websocket.html)。

反代必须能连接 WebServe 所在网络空间的回环地址。若 NGINX 在隔离容器中，容器里的
`127.0.0.1` 不等于宿主机回环；应调整反代部署位置或网络配置，WebServe 仍保持本机监听。

## 从中继入口访问出口

在中继机器启动 WebServe 并配置一次反代。在目标出口机器：

```sh
homeway serve relay set 'rl1…'
homeway serve start
homeway webserve token
```

出口已有上游中继配置时无需重复设置。把出口的 `hww1…` 输入中继网页，即可打开出口的
TUI。出口可以先启动，之后再首次获取 webtoken；无需重启出口，无需在出口执行
`webserve start`，也无需为出口配置域名或证书。

入口接收的目标范围如下：

| 入口主机的运行能力 | 可以登录的目标 |
|---|---|
| WebServe，或 WebServe + 出口 | 本机 |
| WebServe + 正在运行的中继 | 本机，以及本中继当前有效注册且支持管理协议的目标 |

注册关系只用于路由，目标仍独立验证自己的 webtoken。中继不会自动发现其他中继的主机，
也不会根据 token 中的任意地址拨号。登录目标后，完整 TUI 可以继续操作该目标保存的主机。

## 页面、工作会话和权限

- 关闭页面或网络断开会回收该页面的 TUI 与 PTY；工作会话仍由原终端服务持有。
- 重新登录会新开一个界面，工作会话仍可重新选择；断线期间的输入不会重放。
- 两个页面的界面尺寸和选择互不干扰；共同操作同一个工作会话时，仍遵循现有终端规则。
- Web 入口关闭与出口管理能力分开；出口的 HTTP 状态可以为 disabled，而远程管理仍可用。

普通鼠标操作交给 TUI。要在网页终端中拖选文字，macOS 按住 **Option**，其他平台按住
**Shift**，然后点击页面的“复制”；“粘贴”按钮会请求浏览器剪贴板权限。
TUI 发出的复制请求只暂存在页面中，仍需点击“复制”才写入浏览器剪贴板，读取请求不会自动执行。
浏览器剪贴板是全局资源，跨标签页共享；已经发起的复制不能取消。页面保证旧目标的异步复制结果
不会自动写入新目标终端或作为新目标输入，不提供跨标签页的剪贴板隔离。
单次输入与当前待发送内容合计最多2MiB，超限会在发送粘贴内容前拒绝并提示缩小内容。
传输中断可能已发送部分输入，页面会提示重连后检查，不会自动重放。

页面的脚本 CSP 仅允许本站资源（`script-src 'self'`）。样式面为 xterm 运行时样式保留
`style-src 'self' 'unsafe-inline'` 例外；这不允许内联脚本，表单仅可提交到本站。

webtoken 授权完整实例管理，可能创建与 homeway 同权限的 shell、获取连接凭证并管理
已保存主机；它不是只读令牌。仅在可信的自建反代/中继域名输入凭证。网页和反代提供者处于
信任范围，不能把中继到出口加密理解为不可信网页宿主无法看到终端输入。

首版支持现有 TUI 支持的 macOS/Linux（包括 musl）CLI。Windows 的 WebServe 命令明确
报告不支持；OHOS 保持原客户端核能力，不承诺运行此浏览器服务。旧式前台单角色和旧守护
缺少完整宿主时会报告能力不可用，应升级并使用统一进程。

## 源码验收

```sh
python3 tools/gen-webserve-vectors.py --check
python3 tools/webserve-smoke.py --bin target/debug/homeway --browser --extended
```

烟囱使用显式 `/tmp` state、私有统一进程及独立端口，不连接生产实例。浏览器测试需要 Python
Playwright、pyte/wcwidth 和可运行的 Chromium/Chrome（可用 `--chrome` 指定路径），
静态资源校验需要 Node，HTTPS/WSS 反代验收需要 NGINX 与 OpenSSL。
`--browser` 执行真实网页交互，`--extended` 等待真实票据过期；完整验收同时启用两项。
测试的真实执行结果以 `docs/reviews/` 的本批记录为准，未运行项不会标记通过。
