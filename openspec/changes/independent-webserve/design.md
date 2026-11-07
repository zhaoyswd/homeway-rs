## Context

动机见 proposal。当前 `homeway tui` 以 `control.sock` 读取主机/角色，以 `local-term.sock` 或出口终端入口访问同一个 TermService。`unified_cli.rs` 在角色开关之外持有 LocalTerm，适合作为本机及远程 TUI 的唯一宿主；旧式前台单角色没有该控制面。

出口 QUIC 叶子 crate 的 `exit/conn.rs` 在首条 bidi 流读取两字节帧头后进入 `hr-reg4`，联网认证后才受理普通服务。出口和客户端 rustls 当前都未设置 ALPN；给共享服务端增加 ALPN 会使未提供 ALPN 的旧 QUIC 客户端失败，因此本方案保持 TLS/RPK 和 ALPN 配置不动。中继本体只转发密文；完整注册身份保存在驱动线程私有 `legs` 中，尚无本机只读查询接口。

中继分配和出口拨腿在未准入时仅驻留约 10 秒。当前只有联网注册成功才以 `remote + RelayGeneration` 调用 `confirm_client`；管理认证必须独立完成同一腿确认，不能用 LEGUP、首个回包或当前重取的代际冒充准入。

## Goals / Non-Goals

**Goals:**
- 实现规格中的本机和中继两条入口，复用现有 TUI、工作会话与 QUIC 加密承载。
- 将 HTTP 入口、实例 Web 凭证和出口管理 agent 的生命周期分开；出口无 HTTP 入口仍可被中继管理。
- 全部实例实验使用 `/tmp` state、独立端口和统一进程；交付可本地验收的单二进制。

**Non-Goals:**
- 不开发另一套浏览器原生管理界面、不共享物理终端窗口、不增加 SSH、公共账号或第三方身份服务。
- 不让无出口的 Web-only 主机自动注册远程管理，不跨中继发现、不根据 token 地址直拨。
- 不为首版补齐 Windows 现有 TUI/TermService 缺口，不改写旧式前台单角色控制契约。
- 不推送、发布、部署，不动其他仓库或生产 state。

## Decisions

### 1. 独立命令，统一进程持有运行态

新增 `webserve start [--port P] [--origin https://host[:port]]`、`stop`、`restart [--port P] [--origin URL]`、`status`、`token [rotate]`。默认端口 18080，固定 `127.0.0.1`，不接受 `--listen` 地址，不做端口退让。独立严格 `<state>/webserve.toml` 保存 `enabled`、`port`、可选 `public_origin`，不把新字段写入旧 `config.toml` 或 FileConfig；serve/relay 启停也不重写此文件。该分离保证基线旧二进制仍能读取原严格配置。

已有统一进程通过新增 `webserve.*` 控制操作装配。实际 `DEFAULT_CONFIG_TOML` 与 FileServe 缺省均为 serve/relay=false；沿用该默认即可满足全新 Web-only，不另建初始化分叉。已有配置保持原角色意图；普通零参启动的既有默认值不改变；订正 unified_cli.rs 陈旧的 default=true 事件串。`token` 路径不调用自动拉起守护逻辑：运行期走控制请求，离线时在实例锁下执行最小身份/凭证读写，不创建 config 或网络监听。若control未就绪但实例锁已占用，禁止走离线凭证写入，明确报“运行实例控制面未就绪”，避免在线缓存与磁盘分叉。

`WebserveStatusResult` 将 HTTP 状态（disabled/starting/running/failed）、配置端口/实际 loopback、公开 origin、local_tui_available、remote_agent_available、relay_gateway_available/epoch 和可行动错误分别返回。只有真实 TCP bind、HTTP 线程和本机控制/终端准备成功后才报告 running。运行中同配置 start 返回 already；运行中不同 port/origin 的 start 明确拒绝且不改配置，提示使用 restart。restart 重新读取独立配置/应用显式参数、取消旧入口、确认释放端口后重新 bind。停止入口关闭由它发起的浏览器管理连接，不停止出口 agent，也不结束工作会话。HTTP disabled 与 remote_agent_available=true 可以同时成立。

### 2. 独立稳定 Web 凭证

`web/auth.key` 使用 0700 目录、0600 文件、create_new/原子替换，正文为独立随机 key id(16B) 与 secret(32B) 的版本化固定二进制。Debug/错误/日志不得输出秘密；secret 非 Copy，Drop 擦除。实例运行时由共享内存凭证句柄提供快照，异步传输任务不读磁盘。

`hww1` token 的精确字节、长度和校验见 protocol.md；承载目标完整 X25519 公钥(32B)、现有出口 RPK(32B)、key id(16B)、独立 secret(32B) 和校验值；不带地址、联网 token secret 或中继准入秘密。离线读取/创建 `serve/key.bin` 沿用 `server::state::State` 和唯一 `quic_rpk_seed` 派生，只初始化身份，不启出口、不铸联网 token。完整公钥对应现有 relay label；RPK 用于连接目标的 TLS 身份钉定。本机 token 验证必须同时匹配本机身份、key id 和秘密。

读取稳定凭证；rotate 在持锁原子持久化成功后更换内存快照，撤销旧 grant 的有效标记。所有活动管理任务、HTTP 票据与本机 PTY 会话观察同一撤销信号，在 1 秒内关闭。持久化失败不回报成功、不丢弃现有效凭证。CLI 与运行期轮换避免离线文件写入和在线缓存分叉。

### 3. 成熟 HTTP/WS，独立受控的 CLI 异步面

Unix CLI 使用 axum =0.8.9、default-features=false、features=[http1,tokio,ws,json]；tokio 沿用 workspace 配置并仅增 net/io-util；HTTP 连接构造用 hyper 1（server/http1）与 hyper-util 0.1（tokio/service）。仅 `webserve/http.rs` 持有异步代码与 `current_thread` runtime，无 `rt-multi-thread`。其余 CLI/core/capi 继续同步。`check-quic-isolation.sh` 为这个精确控制面文件和 CLI Unix 的精确 tokio 声明建立白名单与正反自校准，禁止新增模糊目录豁免；同步 core/capi、QUIC 数据岛原门不变，并在判据变更记录登记。

静态 HTML/JS/CSS 和固定版本 xterm.js/fit addon 连同许可证纳入仓库并内嵌，运行期不加载 CDN。页面只有登录、终端、连接状态、退出及浏览器必要的复制粘贴操作。终端输出仅交 xterm 解析，不拼入 HTML。

Host 只允许 `127.0.0.1:P`、`localhost:P` 以及配置的公开 origin authority。HTTP 写操作和 WS 升级要求 Origin 为精确本机 origin 或配置公开 origin，不信任 `Forwarded`/`X-Forwarded-*` 来扩展允许集。公开 origin 仅允许无路径、无用户信息的 HTTPS URL；本机允许 HTTP。通过标准 URL/authority 解析统一主机大小写和默认端口（HTTPS :443 与省略443等价，HTTP :80 同理），非默认端口严格匹配。首版只接受 HTTP/1.1 回源，文档强制 proxy_http_version 1.1，不额外开启HTTP/2；Host多值/冲突、非法authority均拒绝。NGINX 负责证书及转发 Upgrade/Connection，文档给同机反代模板。

### 4. HTTP 登录换票据，WS 首帧认证

`POST /api/login` 接受至多 4KiB JSON 中的 webtoken。先解析和匹配本机；远程只从当前运行的本机中继有效注册快照查完整公钥，固定拨本机中继实际 UDP 端口（wildcard 归一 loopback），不使用用户输入的拨号地址。

登录开始时以RAII先预留统一界面名额：在途登录 + 未消费票据 + 已消费终端三者合计至多8。认证成功后返回32B随机短期票据，绑定唯一目标、凭证grant和已认证远程连接（如有）；30秒内一次性消费，不另设32张持连接票据。预留名额随所有权从登录迁移到票据，再迁移到活动界面，不重复计数。票据仅在页面内存，不使用cookie、URL参数、localStorage；这样不依赖反代scheme推断，也不会因共享cookie让不同标签页切错目标。后台每250ms清理过期票据并检查目标连接存活/本地grant/中继epoch，消费时再次检查；过期、首帧失败、OPEN失败、目标撤销/连接关闭、relay stop/restart均释放票据及其WebClient guard。

`GET /ws` 的升级仍校验 Host/Origin，并先取得有限名额；5 秒内首条至多 2KiB 的文本帧必须携有效票据。消费成功后才能发送 OPEN 或创建本机 PTY。此后 binary 为终端输入/输出，文本仅允许受限 resize/close 控制。失败返回统一认证错误，不泄漏凭证或注册列表；可区分已认证后的终端禁用、目标失联和资源忙。每个入口同时最多32个TCP/HTTP连接，最多8个未认证WS（独立短命额度，首帧通过后归还），在途登录/票据/活动终端共享上述8个界面额度；全局登录60次/分钟，均RAII归还。HTTP头至多16KiB/32字段、读取绝对5秒；登录体DefaultBodyLimit=4096且读取绝对5秒；每请求总预算15秒（含远程拨号10秒）。HTTP/1.1禁用keep-alive（WS升级除外），避免空闲连接长期占额。WS在线级设置max_frame_size=max_message_size=65536，首帧前同样生效且首帧文本再限2KiB；ping每20秒、60秒无pong/入站活性关闭，发送持续背压10秒关闭。上述期限均为绝对期限，不允许滴流续期。

### 5. 共享 QUIC 端口，独立首流管理协议

传输实现保持 QUIC 叶子隔离。新增 std 公面建议为 `WebAuth`/可撤销凭证 handle、`WebManagementConfig`（共享 auth + 管理 ServiceIntakeTx）、`WebClient` 与 `ManagementRequest`；具体类型可按 Rust 借用与既有 pump 缝调整，外部行为不得变。

出口 `ExitQuicConfig` 增管理装配入口；统一进程在任何HTTP开关状态下都恒装配管理worker与同一个可空、可热替换的WebAuth活句柄。缺少web/auth.key时句柄内容为None，不能传None省掉整个管理装配；首次webserve token经control操作生成并install到该句柄，已有出口无需重启即可接受管理，HTTP仍保持关闭。旧式前台单角色没有宿主时不装配并明确拒绝。未铸凭证或终端不可用时明确拒绝。首条 bidi 的新两字节魔数 `W1` 进入管理分支，其余照原 `hr-reg4`。管理协议固定版本和全部wire见protocol.md：client hello带版本/key id，server返回新鲜16B nonce，client proof使用域隔离且绑定版本/key id/nonce/TLS exporter的HMAC-SHA256；nonce一次一连接、全认证含腿确认绝对5秒，secret常数时间比较/验证。沿用既有 RPK 钉定和真实 TLS 签名验证，不新增 SkipVerify。

管理认证成功发送独立 `ExitInbound::Management(ManagementRequest)` 给同步引擎，携首次固定 remote、RelayGeneration 和单次回执。引擎仅确认有效中继腿及资源配额，不调用 DeviceTable/register 或 bridge.bind；`confirm_client`返回类型化Confirmed/LegMissing/GenerationMismatch/CapacityFull。LegMissing仅在本次认证剩余预算内每20ms有界重试，始终携最初固定generation；GenerationMismatch/CapacityFull立即以0x75拒绝（worker/入口额度不足为0x73），不能换用新代际；总认证5秒/客户端登录10秒预算不延长。成功提升沿用CLADM/ACK有界重试，不因网络ACK丢失抹掉已确认事实，但至少确认真实本地腿已提升后才Accept。直连无代际时不伪造中继确认。入队、worker 和连接全局最多 8 个管理会话，满则拒绝。

确认成功才返回 Accept，并将这唯一 bidi 流通过已有 socketpair/pump 交管理 worker。管理分支显式并发监控额外bidi并立即关闭整个管理连接；DATAGRAM保持未绑定拒绝，不走任何联网绑定或普通tag。凭证轮换、连接断开、入口退出、目标退出均可取消 pump 和 worker；不用旧控制通道搬终端字节。

`WebClient::connect` 以独立单线程宿主建立仅管理连接，复用现有 RelaySock 封装和 RPK client crypto，不构造 IslandCredential、不做普通联网准入。同步边界只暴露拥有连接取消权的 SvcStream 对象。总登录拨号/认证预算 10 秒，取消释放 socket/线程；原有联网客户端无 ALPN、token、reg4、信封和服务 tag 字节完全保持。

### 5.1. 管理关闭的精确资源销账

仅Drop QUIC连接不等于回收中继assoc或出口正式腿，不能依赖90秒空闲TTL使连续登录刷新消耗32个正式腿额度。每次管理认证携唯一conn_id的ManagementLease，内含alive原子位；叶子持有RAII活位guard，认证失败、pump结束、撤销、取消、连接断开等所有退出路径置false。同步ServerBind每轮且至迟200ms内有界扫描失活owner，不依赖可能满载/丢弃的关闭消息队列。

腿所有权由固定(remote,RelayGeneration)下的network_owned与管理conn_id owner集合共同承担。普通reg4认证只置network_owned；管理认证仅加入自己的lease。管理结束只移除该conn_id；只有同代际已无其他管理owner且network_owned=false时，才拆腿和清理该代际admitted_generations记忆。网络连接先取得腿、多个管理连接共腿、同代重登移交owner、跨代晚到清理均不得误删仍有效拥有者；旧lease不能通过重取当前generation清掉新腿。

gateway的远程会话guard同时持有本机relay epoch、完整目标label、WebClient实际local_addr和连接。WebClient显式绑定归一后的本机relay目的IP:0（v4/v6 loopback或明确本机unicast），不以wildcard源地址作为销账键。guard在关闭时（不是login开始时）捕获close_cutoff=Instant::now，先经本机RegistryHandle的有界队列提交release_association(epoch,label,client_local_addr,close_cutoff)，再Drop WebClient。relay只清理相同epoch/精确AssocKey且association.created<=close_cutoff的对象，沿用原有relay→exit的RELEASE方向通知出口；迟到的旧清理不得命中新端口复用association。队列容量与总管理8名额相匹配，失败有可见记录并由现有有界TTL兜底，不能将丢弃清理假报成已回收；独立lease活位使出口资源不受队列成功与否影响。

必验：连续超过32轮（取40轮）登录、OPEN、关闭，观察管理活位、PTY、assoc及正式腿恢复基线，不出现第33轮容量失败；另覆盖已有普通network_owned腿和另一个管理owner共存、同代移交、错代清理、延迟旧命令遇端口复用、满队列/目标或relay退出，不误拆普通联网路径。

### 6. 中继只暴露内部有效注册快照

relay Config新增可选共享注册句柄；TCP握手已经验证完整pubkey，因此让Handover/CtlConn保留该已验公钥，消除ctl-only Leg的[0;32]占位，不修改线上控制帧。驱动线程在UDP PROOF或完整TCP认证成功/更新/过期/连接关闭时同步完整pubkey→label的有界快照，容量沿用max_legs；pending不能进入，合法ctl-only可以进入。入口每次查当前运行的RelayProc，不使用配置enabled充当可达事实。RelayProc新增真实就绪通道/实际SocketAddr，由Relay::run的绑定结果写入，wildcard归一对应loopback；relay status也显示实际地址，不能用配置端口代替。relay stop/drop用RAII清快照并使epoch失效，取消远程票据/会话；重启即使同端口也生成新epoch。

此快照仅供本机装配使用，不增加公开列举API。中继只按完整pubkey做路由，secret对其视为不透明拨号材料，绝不以本地HMAC或路由存在判断登录成功；必须等目标认证并Accept才发票据。TLS RPK钉定再次确认实际目标，防短label相同、错误路由或目标更换。

### 7. PTY 直接运行现有 TUI

仅 Unix。管理 worker 在认证/票据消费后以当前二进制绝对路径和固定 argv `tui --no-spawn --state <目标实例state>` 直接 exec，设置受控终端环境；不用shell -c或浏览器指定命令。新增TUI --no-spawn穿透所有后台控制拨号路径，control不可达时仅退出本界面，严禁自动拉起守护。Web前端PTY不注册成用户工作会话，避免递归显示承载TUI。TERM固定xterm-256color，HOME/PATH等来自既有受控本机终端环境。

每界面独立 PTY、子进程、尺寸、取消令牌；共享原控制/TermService。管理流承载有版本的 OPEN、INPUT、RESIZE、OUTPUT、CLOSE 帧：单帧上限 64KiB，长度先验；OPEN 必须先于输入。尺寸范围沿用 term Size 的 1000×500 上限、拒绝零尺寸；每向缓冲不超过 2MiB，输出不静默丢字节，慢消费者背压/超时关闭。PTY读写仅在有限worker线程，使用既有非阻塞fd + POLLIN/POLLOUT + 每拍取消检查模式；写背压不能无限阻塞，异步HTTP runtime绝不直接做阻塞PTY读写。浏览器断开后置取消、唤醒poll、关闭本界面master，对仅该自有TUI先SIGHUP，500ms未退则SIGKILL，再以500ms上界wait/reap；不能依赖关闭master唤醒阻塞write，不调用用户会话KILL、不向统一进程或其他进程组发信号。

重连重新打开界面，不重放断线期间输入；工作会话由原服务保留。剪贴板保持显式浏览器操作，不自动执行终端输出请求读取系统剪贴板。鼠标/中文输入/宽字符/大粘贴/窗口变化纳入真实浏览器验收。

## Risks / Trade-offs

- [完整 TUI 等价于实例管理权限，可能创建同权限 shell并管理已保存远程主机] → 登录前说明授权范围；webtoken 与联网 token 分离，但不把它宣传为只读或只限一台主机的细粒度授权。
- [反代及中继网页提供者可接触输入和管理秘密] → 仅承诺用户可信自建入口，不宣称对不可信网页宿主端到端保密；静态资源内嵌、严格 CSP、不用 CDN。
- [首流可识别前，管理与设备共享TLS握手/conn上界，不能保证匿名洪泛完全无影响] → 如实保留该边界；W1分流后独立未认证管理名额8、全认证5秒、管理总8。验证不创建设备，饱和回收后普通准入可恢复，不能把测试通过写作握手前结构隔离。
- [管理认证成功但中继腿未提升造成 10 秒后断线] → 等固定代际腿确认回执后才 Accept，并实测超过未准入TTL后仍可交互。
- [legacy 前台单角色或旧服务不支持管理] → 明确 capability unavailable，不试图启动同 state 第二守护。正常零参与 start 命令的统一进程完整覆盖。
- [浏览器保留快捷键与本机终端不同] → 必要时提供少量网页按钮/键映射；不宣称所有操作系统快捷键均能透传。

## Migration Plan

独立webserve.toml默认disabled；既有config.toml/身份/网络token保持，普通启动行为不变。用基线9a844f8二进制或基线同构strict解析验证serve start、webserve token、webserve start/stop后旧config仍可读取；不要求旧二进制识别新控制操作。新 CLI 和目标需支持管理版本，旧目标返回清晰不支持错误。Web 静态依赖与异步控制面只进 CLI，验证 `-p homeway-capi` 的依赖树无 axum/xterm/CLI Web 依赖，OHOS/musl/Windows 编译保持。

本轮开发与私有验收完成后记录两道门与判据，再由主会话本地中文 commit。发布部署需后续用户授权并遵守 GitHub Release 纪律；回退前停止Web入口；旧二进制忽略独立webserve.toml与web目录，继续读取原config.toml，不存在未知新节导致拒启。


### 实现验收约定补充（C5–C12）

- 所有token字符串、base64解码和HTTP请求体使用可擦除Secret/Zeroizing缓冲；解析借用、减少clone。前端提交后清除输入，日志与错误不得包含hww1或秘密；不可承诺清除浏览器/操作系统的所有历史副本。
- `cargo tree -p homeway-cli -e features`必须证明无rt-multi-thread；axum/hyper/tokio仅在CLI Unix声明，唯一异步源码白名单为webserve/http.rs并做正反负例门。`cargo tree -p homeway-capi`不得出现axum/hyper新增Web依赖。
- 平台矩阵：Mac/Linux（含musl）全部命令与浏览器TUI支持；OHOS维持core/capi交叉check，不新增CLI Web运行承诺；Windows全部webserve命令明确unsupported且不写state、不启动服务，原Windows CLI仍check通过。旧daemon返回unknown_op映射为“请升级正在运行的homeway”，不auto-spawn第二进程。
- 状态划分使用窄句柄Auth/TicketStore/RelaySnapshot/HTTP controller，不把HTTP/PTY/票据塞进RolesInner巨锁。bind、文件、拨号、PTY、线程join均不在RolesInner锁内执行；新错误用thiserror，协议帧non_exhaustive，代码门包含逐行移植痕迹检查。
- 在产品实现前登记新增W-web判据与行为常量；具体wire见protocol.md，实施生成fixtures/webserve向量并覆盖全目录SHA256SUMS，不把规划向量当成已实测。
