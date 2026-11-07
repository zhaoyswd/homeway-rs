## Purpose

为 homeway 提供仅本机监听、可通过用户反代访问的浏览器终端入口，以独立实例凭证打开本机或本中继注册主机的完整 TUI。Web 入口、网络出口、中继与目标管理能力分别管理，允许没有网络角色的主机单独使用本机 WebUI。

## ADDED Requirements

### Requirement: Web 入口独立且仅监听回环
系统 SHALL 提供 `webserve start/stop/restart/status`，在独立配置中将固定端口和启用状态持久化，不向旧config.toml写入未知字段，HTTP/WS MUST 只绑定 `127.0.0.1`，端口被占用 MUST 明确失败而非换端口。全新实例仅启动 Web 时 MUST 不隐式开启出口或中继；既有实例 MUST 保留原角色配置。

#### Scenario: 全新 Web-only 启动
- **WHEN** 用户在全新私有 state 执行 `webserve start --port 18080`
- **THEN** HTTP/WS 在 `127.0.0.1:18080` 可用，出口和中继未启动，非回环地址无该监听

#### Scenario: 停止网络角色
- **WHEN** 用户停止出口或中继而 Web 入口仍启用
- **THEN** 本机 WebUI 继续可用，状态如实显示角色状态

### Requirement: Web 管理凭证独立于服务启动
系统 SHALL 提供 `webserve token` 和 `webserve token rotate`。凭证 MUST 归属本实例、使用独立随机秘密、重启稳定；获取命令 MUST 不开启 Web、出口、中继或任何网络监听。轮换 MUST 撤销旧凭证、未使用登录票据和正在使用该凭证的管理连接，且不得结束真实工作会话。

#### Scenario: 离线获取与轮换
- **WHEN** 用户在没有运行服务的私有实例两次获取凭证，然后轮换
- **THEN** 前两次得到相同凭证，轮换得到新凭证，旧凭证认证失败，期间没有网络监听

### Requirement: 本机与中继目标路由受限
Web 入口 SHALL 接受本机的有效 webtoken；仅当本机中继正在运行时，SHALL 允许当前有效注册且支持管理协议的目标凭证。系统 MUST 按完整目标身份验证注册关系，不拨入凭证中的任意地址、不提供匿名注册主机列表，最终认证 MUST 在目标主机完成。

#### Scenario: 非中继拒绝远程凭证
- **WHEN** 在仅 Web 或 Web+出口的入口提交另一台主机的 webtoken
- **THEN** 登录被拒绝，系统不尝试直连该主机

#### Scenario: 中继登录注册目标
- **WHEN** 目标出口已向本中继注册、凭证有效且目标支持管理协议
- **THEN** 浏览器得到目标机 TUI，而非中继机 TUI

### Requirement: 出口管理无需 HTTP 入口
支持管理协议的统一进程 SHALL 在出口 QUIC 链路上提供独立 Web 管理认证。目标主机 MUST 不需要启动自身 `webserve` HTTP/WS、不需要域名或证书。Web 管理凭证 MUST 不授权普通网络设备准入、IP 数据报或普通服务流；现有联网凭证 MUST 不直接授权完整 TUI 管理。

#### Scenario: 目标未启动 webserve
- **WHEN** 目标出口持有 webtoken、已注册中继，但本机 Web 入口关闭
- **THEN** 用户仍可从中继 WebUI 登录并操作目标 TUI，目标无新增 HTTP/WS 监听

#### Scenario: 管理凭证尝试网络服务
- **WHEN** 已管理认证的连接发送普通 IP 数据报或尝试普通服务流
- **THEN** 系统拒绝处理，设备表无新增设备

### Requirement: 网页运行独立 TUI
网页 SHALL 显示可交互的现有 homeway TUI；每条管理界面连接 MUST 拥有独立窗口尺寸和禁止自动拉起守护的界面进程，共享目标现有的工作会话服务。关闭页面、停止本入口或管理连接断开 MUST 只回收相关 TUI/PTY 资源并保留真实工作会话。系统 MUST 不执行用户指定的任意启动命令。

#### Scenario: 两个浏览器独立使用
- **WHEN** 两个页面登录同一主机并设置不同窗口尺寸
- **THEN** 两个 TUI 的菜单、选择与界面尺寸互不干扰，可看到相同工作会话

#### Scenario: 页面退出后恢复工作
- **WHEN** 页面中创建工作会话后关闭页面，再重新登录
- **THEN** 原工作会话仍存在，旧界面进程和 PTY 已回收，断开期间输入不重放

### Requirement: 反代和浏览器入口认证
系统 SHALL 支持用户配置单个公开 origin，并同时允许指定端口的本机 origin；HTTP 登录及 WS 升级 MUST 校验 Host/Origin。登录 MUST 使用有界请求体交换短期随机会话票据，WS MUST 在限时首帧验证票据后才允许终端操作。webtoken 或票据 MUST 不放入 URL、访问日志或浏览器长期存储；HTTP/WS MUST 不因来源为回环而免认证。

#### Scenario: 反代域名访问
- **WHEN** 反代转发匹配配置 origin 的 HTTPS/WSS 请求
- **THEN** 网页、登录和实时终端正常工作，无需由 homeway 管理证书

#### Scenario: 异站和无认证 WS
- **WHEN** 请求使用不允许的 Host/Origin，或 WS 未在期限内提交有效票据
- **THEN** 请求或连接被拒绝，不创建 TUI，资源名额归还

### Requirement: 资源有界和错误可观察
系统 MUST 为 HTTP/WS、未认证请求、登录票据、管理连接、PTY 尺寸和输入输出积压设置显式上限及取消机制。失联、禁用终端、旧版本目标、身份不符、端口占用或启动失败 MUST 返回可理解的错误，不能把队列接收当作服务成功。

#### Scenario: 慢消费者和异常尺寸
- **WHEN** 客户端停止读取、发送超限消息或越界终端尺寸
- **THEN** 系统实施背压或关闭该连接，不无限增长内存、不影响普通出口或中继转发

### Requirement: 现有协议与平台兼容
系统 MUST 保持已有 TLS/RPK、无 ALPN 协商、联网注册帧及中继信封字节兼容，新增管理协议需独立版本识别和明确拒绝。首版浏览器 TUI SHALL 覆盖已有 TUI 支持的 Unix CLI；Windows 和缺少统一进程本机控制服务的旧式前台单角色 MUST 明确报告能力不可用，不隐式启动同 state 第二进程。

#### Scenario: 旧联网客户端继续工作
- **WHEN** 更新服务端后，原客户端使用原 token 进行 QUIC 登记和普通终端访问
- **THEN** 原准入、转发、服务功能通过原判据，不要求重铸联网 token
