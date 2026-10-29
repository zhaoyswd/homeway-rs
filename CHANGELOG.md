# Changelog

## v0.3.24（2026-10-29，TUI 状态、复制与原会话自动恢复）

- 主机标题只保留状态点与名称；第二行以单空格显示 `direct` / `relay` / `offline`、
  `12ms`（未知为 `--ms`）及 `proxy:on/off`，不再显示 IP。本机使用 `exit:on/off relay:on/off`。
- 修复会话内应用通过 OSC52 写剪贴板时被 TUI 忽略的问题，复用本机异步复制及 SSH/终端
  OSC52 路由；严格校验 UTF-8、长度和 32 KiB 上限，支持空值清空，不读取系统剪贴板。
  切换/分离取消未开始的请求，过期会话结果不输出终端请求或覆盖新提示。
- TUI 用同一 surface 连接的 LIST 请求主动探活，EOF 或探活无响应后有界退避重接原主机、
  原会话及 PID；保留画面，不重建或杀死 PTY，不自动重放断开期间的键入、粘贴或未确认输入。
  新快照和身份验证通过才恢复输入；会话结束、同名 PID 变化、切换或退出时停止恢复。
  重接保持观察者身份，新的真实输入才沿既有规则接管活动状态与尺寸。
- 快捷指令正文按 Unicode 展示宽度与可见高度整组居中，块内保持左对齐；长详情保留滚动，
  40×10 紧凑窗口完整保留四行命令、详情标题与一行详情。
- 会话列表按创建时间稳定排列，同时间以名称补齐顺序；输出、状态和活动时间变化不再导致
  列表跳序，新会话放后，旧端缺创建时间时按名称确定顺序。
- macOS blocking 通知改由承载终端处理 OSC9，移除点击进入 Script Editor/Finder 的
  AppleScript 来源；homeway 不猜窗口或自动打开其它应用。宿主不支持或未启用 OSC9 时
  可能没有系统通知，不保证所有终端版本都能点击精准返回原会话；Linux 保持无点击动作
  的 `notify-send` 与终端回退。
- 本版产品行为仅调整 CLI TUI，无 core、wire、NAPI 或 QUIC 连接岛恢复策略变化。

## v0.3.23（2026-10-28，TUI 对齐与居中快捷指令）

- 会话空态描述和左下快捷指令提示与标题文字同列，统一左侧布局。
- 全平台界面主提示、帮助与 README 统一使用 `Ctrl+B`；保留双按 `Ctrl+B` 向会话透传，
  以及承载终端能够上报时的 `⌘+B` 兼容入口。
- 快捷指令改为局部居中弹窗，最大 64×16，40×10 窄窗口下为 36×8，四周保留背景；
  保留模态输入隔离、执行后关闭、详情滚动与原终端尺寸。
- 本版产品行为仅调整 CLI 界面与帮助，无 core、wire 或 NAPI 行为变化。

## v0.3.22（2026-10-28，交互式 TUI 与持久本地终端）

- 新增 `homeway tui`（macOS / Linux）：左上管理主机，左下切换会话，右侧操作终端；
  支持添加主机、出口/中继/代理操作、创建与关闭会话，退出界面只分离，会话继续运行。
  控制面与终端请求使用有界后台队列、绝对截止和代际过滤，离线或旧请求不阻塞界面。
- 非 Windows 统一进程持有 `local-term.sock`，与出口的本地/QUIC 入口共用一份终端注册表；
  无需启用出口即可创建本地会话，`serve stop/restart` 保留会话，统一进程退出才结束。
  原 `homeway term` 优先连接本地服务并保留既有入口回退，`HOMEWAY_TERM=off` 仍禁用终端。
- TUI 消费服务端权威 surface 网格与历史，支持滚轮、Shift 翻页、回到最早/实时画面；
  浏览历史时新输出保持位置，窄面板裁剪而不重新折行。新增显式协商的 observer 接入，
  查看或调整非活动 TUI 不抢占其它客户端尺寸，真实输入后才接管；本机承载自身的会话
  标记为“自身”并避免自嵌套。新 TUI 需要目标服务支持 observer，缺少能力时提示升级，
  不降级为 raw 接入；已有 CLI/App 默认接入保持兼容，协议版本与 surface 格式不变。
- 修复长会话 raw 回放因历史裁剪丢失键盘、鼠标、焦点与括号粘贴模式：回放末尾补发
  输入模式检查点，不改屏幕、光标、历史或实时输出。修复多列 `ls` 等控制字符导致的
  TUI 渲染崩溃，完善宽字符、组合符、TAB 与窗口变化时的显示。
- 会话目录优先使用有效 OSC 7；未上报时由服务端只读采样 shell 的真实系统工作目录
  （macOS / Linux），普通 shell 初始目录、`cd` 与 raw-only 会话均可显示，无需 shell hook。
  LIST 增加可选 `agentLabel`，保留 pi 等 manifest 名称，既有 agent 枚举不变。
- 界面自动识别终端深浅主题，精简侧栏与行内状态，终端使用完整高度；`⌘+B`（macOS）
  或 `Ctrl+B` 打开全屏快捷指令和完整操作详情。Command 键依赖承载终端的增强键盘支持。
  终端标题栏可关闭当前会话，空态隐藏按钮；拖选可见文本后自动复制，应用捕获鼠标时
  用 Shift 拖选。系统剪贴板成功与 OSC52 请求分别提示，实际请求效果取决于承载终端。
- 当前选中主机的会话进入 blocking 时通知一次，首次列表只建基线，持续 blocking
  不重复，恢复后重入可再提醒。macOS 系统通知、Linux `notify-send` 与 SSH/终端内 OSC9
  回退均异步有界；实际显示取决于系统或终端设置，不后台监看其它主机。
- 修复终端关停时强杀回退只发 SIGHUP、随后无期限等待子进程的问题：使用真实 SIGKILL
  与有界收尸，未确认退出时保留句柄和运行态继续回收；退出码及协议格式保持不变。
- TUI 依赖 ratatui/crossterm 仅进入非 Windows CLI；Windows 明确提示 TUI 不支持。
  本版含 daemon/term 服务变更，使用本地持久会话或新 observer 能力需更新并重启目标
  统一进程；无 NAPI、网络准入或连接恢复策略变更。

## v0.3.21（2026-10-23，homeway update 自更新）

- 新增 `homeway update`：查询 GitHub 最新 Release tag（与当前相同 = 提示「没有更新」），
  自动下载本平台官方产物并按 SHA256SUMS 校验、解包、`--version` 烟囱、备份现役到
  `<state>/cache/homeway.prev` 后同目录 rename 原子替换，再按托管形态重启统一进程
  （launchd 主机由 KeepAlive 拉起；裸进程主机 3s 宽限后自动脱离会话自拉起；守护未
  运行只换文件），以 control.sock 就绪收工。角色与配置不受影响（config 期望态即模式
  真源），回退 = 备份文件放回原路径后重启。
- 出口内部新增极小同步 HTTPS GET 件（minhttp：rustls + Mozilla 根束、仅 https 重定向
  ≤5 跳、Content-Length/chunked/EOF 三种分帧、绝对截止）；不新增外部依赖。GitHub
  不可达的主机（如阿里云）在查询阶段如实报错，仍按既有部署纪律由本机下载校验后上传
  换装；Windows 不支持（运行中的 exe 无法原位替换）。
- 本命令自 v0.3.21 起可用（旧版无此命令）；无 wire/NAPI/连接策略变更。

## v0.3.20（2026-10-23，出口质量历史）

- 出口每5秒采集认证设备的QUIC路径、过境目标连接和转发背压，短连接结束留摘要、长连接按窗增量；有界异步通道与4×8MiB本机轮转，不记录正文、URL或凭证。
- 新增 `homeway serve quality` 本机时间段报告，支持最近时长、带时区起止时间、完整设备标识过滤和JSON；按隧道、目标建连和出口排队给证据，明确缺测、首次基线、重启、坏行、并发轮转及测量边界。
- Linux/macOS原生TCP RTT/重传按平台能力采样，未知字段为null/不可用；不把低吞吐当容量不足、连接首上游字节当HTTP TTFB、UDP无回包当请求失败。
- 无wire/NAPI或连接恢复策略变更，现有客户端无需因质量采集更新。部署与回退仍使用官方Release；本次仅换装用户指定的本机Mac出口。

## v0.3.19（2026-10-22，raw 终端回放备用屏修复）

- 修复有界输出历史裁掉备用屏进入指令后，raw attach/自动重连把全屏 TUI 画到主屏的缺陷：
  输出环保留裁剪边界的解析检查点，raw 握手按**回放起点**的主/备用屏状态选屏（`47h/l`，
  不重复 `1049` 保存槽）、清屏后再回放；不用 ATTACHED 期末模式反推起点。
- 完整未截断历史不再无条件丢弃首行（避免删掉起始切屏/模式初始化序列）；扫描器忽略
  DCS/SOS/PM/APC 字符串载荷中的伪切屏指令并处理 RIS 复位；检查点携带跨块解析状态。
- 实时 DATA 保持字节透传；初始化字节只用于 raw 握手、不入环不计 replayed。
  不改帧格式/surface/NAPI/导出面。本版同时携带 v0.3.18 后入 main 的 surface 接入
  人工尺寸哨兵退役（`f883d7a`）。

## v0.3.18（2026-10-22，终端断腿 SIGPIPE 修复）

- 修复终端非阻塞写绕过 socket 的 no-signal 保护：对端关闭时返回连接硬错误并摘腿，
  不再触发 SIGPIPE 杀死整个出口/中继统一进程；保持短写进度、断尾、poll 与期限语义。
- 增加默认 SIGPIPE 处置的独立子进程回归，避免测试进程默认忽略该信号掩盖问题。
  v0.3.17 正式换装验收发现，阿里云已先回退 v0.3.16；后续只使用本版官方 Release 重新部署。

## v0.3.17（2026-10-21，终端硬化与当前单版本）

- surface v5/cell v2 增加历史 epoch/base/seq 与软折行、明确列宽；修复 VT 历史、颜色失效、
  应答归属、粘贴事务、同步输出超时与背压/编码预算。CLI 使用有界读写队列、公平调度和可取消输出。
- 删除旧 App/出口握手、Legacy 腿与旧夹具自动转换，严格要求当前 HELLO/GREETING 与显式平台。
  App 与出口必须配套升级，不保留旧版兼容；标准 ANSI/DEC/kitty 与当前 raw CLI 保留。
- 当前格式夹具与 fuzz 种子同步；详见终端专项评审。发布前源码 CI 两平台各1251通过、0失败。
- 部署固定使用 GitHub tag Release 产物；由本机下载校验后转传阿里云，不用临时本机构建替代。

## v0.3.16（2026-10-21，终端无腿收尾回收修复）

- 修复终端最后一条输入腿先退出时的收尾竞态：不再将「没有待发送 ENDED 的腿」
  当成「已收尾」，首次收尾仍须收尸、移除注册与运行态，释放 PTY master 和历史。
  保留重复收尾幂等与活跃写者排空语义；增加无腿/写者先退出的确定性回归。
- 测试隔离取消与总期限两个判据：取消分支使用独立安全期限，避免线程调度稍晚就
  被90ms期限抢先；保留非取消90ms期限、两臂350ms速度断言，生产逻辑与预算不变。

## v0.3.15（2026-10-21，中继 TCP 快速重启修复）

- Unix 中继 TCP 监听恢复标准地址复用语义，避免旧连接 TIME_WAIT 导致同端口重绑失败、
  启动时静默落为 UDP-only。保持显式 IP/双栈监听范围与在役端口冲突拒绝，不启用
  SO_REUSEPORT；Windows 路径不变。增加主动关闭已接入连接后立即重绑的回归。
  该问题在 v0.3.14 生产换装验收时发现；PTY 修复随 v0.3.14 已通过双平台 CI。

## v0.3.14（2026-10-21，中继准入/持续直连 + 公网观测腿整改 + Linux PTY 修复）

- 修复 Linux PTY 输入背压收工：不再假设 SIGHUP/子进程退出会解除 master 的阻塞写。
  master 改非阻塞短写与 poll，并以独立取消位在 kill/finish/收尸前停止输入；读端保持
  阻塞语义和尾部输出排空，输入顺序与正常背压不变。原 Linux CI 失败真实复现，
  增加仍存活子进程的取消、自然退出、延迟输出与大输入完整往返回归。无 wire/NAPI 变更。


- **中继、候选赛跑与地址学习整改**：中继私钥证明、TCP 绝对期限与监听范围、
  预准入及正式额度隔离、数据字节限速；以出口真实客户端准入的会话认证标记确认额度。
  恢复 token/学习候选缓存与异步 A/AAAA 重解析；TLS 首胜后保留准入后备；健康中继
  可验证后切回直连并保留连接/服务流，支持后学跨地址族。协议新增 CLADM 与 UDP_SESSION，
  **滚动升级顺序：先出口、后中继**（旧出口配新中继仅有临时窗口）。判据行变更见
  `docs/INTEROP-CRITERIA.md` 同日条。两道评审见同日 relay-security、quic-selection、
  endpoint-learning 及交叉复核记录。

- **公网观测腿 r5 全项整改**：TLS 下层共享绝对期限/取消 IO，慢握手与记录不再延长
  DoH 的 3s 预算；首轮 token 独立 15s 兜底，公网推断整轮 60s；发布事务与停机串行，
  观测/兜底/中继 UDP+TCP 线程有明确收工等待，缩租前先停止观测。修复 STUN6 字面量
  mapped 与错误族回包阻断后备、IPv6 误用 IPv4 UPnP 外口；Linux DoH 按名钉卡不因
  index=0 跳过。HTTP 增加有界 chunked/EOF 定界，查询类型在 DoH 候选内过滤；显式
  错误配置 fail-closed。系统解析后备通过唯一进程 worker、有界队列保护调用方期限；
  UPnP/DDNS 在途读写支持取消，UPnP 主机名保留 Host 与异主机控制 URL 语义。详情与
  验证见 `docs/reviews/2026-10-21-公网观测腿整改.md`。

- v0.3.13 tag 后的其余评审收尾（不改运行行为）：r4 的引用环说明、panic 回归实例
  gate 注释与历史发布归因统一；r3 收敛批（实例级 panic gate / DoH 读后越界 /
  manual pubep 停止位 / 域名 255B 边界 / CL:0 按 NODATA）；r1(`ee4a42d`)/r2 收敛批
  （DoH 腿健壮化、panic 路径断环、Windows 钉卡、clippy 归零、`DEFAULT_STUN` 兜底、
  pubep 停止位扩到 manual 路径）。


## v0.3.13（2026-10-20，tag fbfd421）

- **公网观测腿抗代理污染**（外网手机落中继排障）：
  - STUN 默认换**国内多候选**（v4 = miwifi/hitv/bilibili；v6 = hitv/cloudflare；候选逗号
    分隔逐个试、首个可信应答者胜）；域名经 **DoH** 解析（新模块 `server/doh.rs`：
    `[serve].doh` 可配，默认阿里双 IP 直连 + TCP 钉物理网卡 + rustls/webpki-roots），
    **不再走系统解析器**——fake-IP 污染环境下整条观测腿不再作废；解析与观测结果一律过
    污染闸。*（v4 的 cloudflare 国际兜底是 tag 后整改加入 DEFAULT_STUN，见「未发布」节。）*
  - 判据行变更：`STUN：监听 socket（本地 …）在 %s 眼里是 …` 的 `%s` 从「整串候选」改为
    「胜出的单个候选」；新增 `DoH：…` 族与 `STUN：… 无应答——换下一个候选` 族诊断行。
- **serve restart 端口退让修复**：QUIC 面正常收工清绑定表，断
  `bindings → Connection → quinn socket(ExitSock) → bridge` 的 Arc 自环——旧监听 socket
  （与 PubFace dup）此前永不释放，restart 只能 EADDRINUSE 退让 +1，token/端点缓存里的
  旧端口全部失联。*（panic 路径 clear 与 pubep 停止位是 tag 后整改，见「未发布」节。）*
- 依赖面：+`webpki-roots`（DoH 根束）+`socket2`（钉卡 TCP；均为既有传递依赖或纯数据）。
  `libclientcore.so` 与上版同量级（server 面被链接期裁掉，不进手机核；tag 时点
  3,081,184 B，r1 后重建 3,081,312 B——差量未细分归因；测量 = tier
  `tools/core/build-core.sh` 产物 `core/libs/arm64-v8a/libclientcore.so`，release）。

## v0.3.12（2026-10-20）

- 连接与终端评审整改（tier docs/reviews 8 项中的核侧 3 项）：PTY 控制面分锁（`PtyCore`
  + writer 独立锁、`kill_start` 无锁化）；R 采纳保留失败链（`rearm` 挪到赛跑/一次性重连
  采纳点，B 门可达 + 重连耗时全周期）；替换风暴计数跨采纳保留 + `storm_parked` 门。
- 复核整改 R2：风暴冷却到期清位即产出一次探活（不等在用/待机节拍）。

## v0.3.11（2026-10-20）

- **恢复面三修（2026-10-20 评审整改）**：
  - **替换风暴冷却到期自愈**：`Ladder` 记录 `storm_until`，`due` 在截止后自行清 `dormant`
    并恢复周期快探（不再只等新连接被采纳）；`next_wait` 对齐冷却截止。与
    `connection-lifecycle.md`「暂停 R 30s……冷却到期复位」的契约一致。
  - **服务会话状态与岛内恢复对齐**：`RaceInFlight` 按「让路」处理（不再写 failed）；
    岛配置补 `on_adopt`（采纳即 failed→ready）；巡检探活成功也把 failed 拉回 ready；
    暖机 `NoFace` 不再返回无巡检线程的 failed 会话，照常启动巡检由事件重连。
  - **交接/注册刷新失败清服务流**：两条失去 `live` 的路径补
    `clear_on_connection_loss()`（按 EOF 收，不置承载位 false），避免死流虚占 capacity。
  - 判据行变更：`替换风暴熔断中 —— 阶梯休眠（原因=…），N 后自动恢复周期探活（或新连接采纳）`；
    新增 `宿主会话：岛内阶梯采纳新连接 —— 状态由 failed 回 ready` /
    `一次性重建：让路（源=…，岛内阶梯 R 在赛中）` /
    `替换交接 —— 旧连接上的 N 条服务流按 EOF 收` / `注册刷新失败 —— N 条在册服务流按 EOF 收`。
    已登记 `docs/INTEROP-CRITERIA.md` 判据变更记录。

- **term attach 退出归还本地终端输入模式**：远端程序经 raw 腿直接开过的鼠标上报
  （`?1000/1002/1003/1006` 等）、焦点（`?1004`）、括号粘贴（`?2004`）、备用屏（`?1049`）、
  kitty 键盘等模式，退出时统一重置——修「出口服务退出、attach 收尾后移动鼠标仍往 shell
  刷 `ESC[<…M` 乱码」。顺序 = 先关模式（停止新上报）→ 50ms 排空（在途上报/查询应答）→
  还原 termios；信号管道/poke 管道建立失败的早退路径同样归还终端。

- **files 新增 `remove` 动词（App 文件管理删除 + `homeway files remove`）**：wire 协议由六动词
  扩为七动词——文件/符号链接删条目本身（不跟随目标）、目录递归删除（连内容）、服务根拒删；
  父目录仍走根内沙箱逐分量复核，叶子按字面拼接后 `symlink_metadata` 判型（避免「删链接=删目标」）。
  核 facade 同步增 `remove` 分派；App 长按菜单的「删除」经确认后调用，CLI 为
  `homeway files remove <路径>`（`rm` 别名）。旧出口不识别该动词（回 `invalid_arg`），
  升级出口后生效。

- **CI 隔离门修复**：`crates/homeway-quic/src/tun.rs` 测试里的 `std::thread::sleep`
  改为有界让权等待（负例注入同样用忙等），恢复「QUIC 岛隔离源码门」全绿；行为断言
  不变（300ms 观测窗 / 20×10ms 阻塞回调下界）。
- **install.sh 改为纯交互式安装（无命令行参数）**：下载并校验二进制后，检测正在运行的
  homeway 守护进程并询问是否停止（避免「只换二进制、旧进程继续跑」导致的版本/配置不生效）；
  随后依次询问是否配置后端（端口/文件根目录/中继凭据）、是否配置中继（监听/公告地址）、
  是否添加主机（名称/token）、是否做 macOS 文件夹预授权、是否配置开机自启动（默认否）。
  配置写入 `~/.config/homeway/config.toml` 时只改相关键，保留其它内容；选择自启动会通过
  launchd/systemd 重启守护进程，确保新二进制与新配置立即生效。README 使用教程同步了端口
  默认值（出口 `38241` / 中继 `:38341`）、`quic_listen` 语义与可选中继/DDNS 命令；
  新增「作为客户端使用」章节（同一二进制支持主机管理/文件/终端/端口转发/SOCKS5）。
- **README 出口启动顺序**：把可选中继/DDNS 配置移到 `serve start` 之前（首次 `serve token`
  即包含它们）；注明启动后再改需 `serve restart` + 重新取 token 并在 App 重新添加主机。
  `serve relay set` / `serve ddns add` 是纯配置写入，不要求出口已运行；目录不存在时会自动创建。
- **配置写命令自动建 state 目录**：`serve relay set` / `serve ddns add` 等纯配置写入在
  `~/.config/homeway` 不存在时自动创建目录，不再要求用户先手动 `mkdir` 或先跑一次 serve；
  新增单测覆盖。README / App 教程同步去掉“先建目录”的手工步骤，并把 macOS 预授权作为
  手动安装代码块最后的可选命令；App 教程「配置 App」卡片上移到「部署中继」之前。

## v0.3.10（2026-10-16）

- **term attach 三连修（term-attach-resilience，openspec 变更 + 两道评审门）**：
- **TUI attach 后画面不完整（间隔哨兵）**：opencode 类按行差分渲染 TUI attach 后只有动画行可见、
  手动 resize 才恢复——根因 = 尺寸哨兵两跳 SIGWINCH 背靠背被内核合并，App 读到「尺寸未变」跳过
  重绘（真机实验：背靠背 0 字节输出 / 间隔 300ms 全屏重绘 14732B；vim 无条件重绘不受影响）。
  修复 = 仅「attach 且几何未变」的 raw 腿走间隔形态（第一跳原位 + 第二跳 300ms 延迟线程，五重验
  让位、panic 防护、spawn 失败锁外兜底）；几何真变路径与 surface 腿保持现状背靠背（真 SIGWINCH
  已足够 / 快照真源是 vt 不需要逼重绘）。`HOMEWAY_TERM_SENTINEL_GAP`（毫秒）可调、0 = 退回背靠背。
  e2e 铁证：回放窗被增量顶空后 re-attach，静态内容 **0.3s** 内重现（与 300ms 精确吻合）。
- **断网即退出（attach 重连循环）**：远程面链路级断链不再退出进程——保持终端 raw、显示重连横幅、
  退避重拨（0.5s×2 封顶 5s、成功即复位）；`new` 创建的会话重拨自动降级创建位（不再被
  already_exists 打死）；ENDED/会话级错误/本地面断链照常终局。等待期分离键与孤立 Ctrl-C 仍可退出。
  e2e 铁证：杀 daemon 后 **1s** 出横幅、**12s** 自动恢复，会话全程保活（服务端 self_reconnect 复用）。
- **退出后 shell 留数字乱码（应答排空）**：应答让位设计下查询直达本地真终端，断链/退出竞态里孤儿
  应答（DA1 `62;1;2;…c` 形态）漏给 shell 回显——所有退出路径在还原终端前统一 `poll(50ms)+
  tcflush` 排空。e2e：注入在途应答后 SIGINT，退出输出零残串。
- **评审与工程面**：r1 方案评审（1 高 2 中等十余条全采纳）+ r2 完工代码评审（H1 GREETING 档泄漏/
  M1 拨号预算挂死窗/M2 矩阵收编等全整改）两道门记录入 `docs/reviews/`；e2e 另抓出并修复空 poke
  管道裸 read 永久阻塞（连接段卡死主循环前）；拨号失败路径显式 close ControlClient（长重试 fd
  稳定复测恒定）；判据变更登记 5 行（哨兵间隔/退避/排空常量、重连横幅新行为面、断链三态文案
  产出面收窄）。规格真源 = `openspec/changes/term-attach-resilience`。

## v0.3.9（2026-10-16）

- **客户端核自愈三态化（event-driven-heal，核侧主体）**：断线处置从「秒级轮询 + 无限冷却重试 +
  半死态重装」收敛为「自救轮次 + 熄灯门 + 翻沿传感面」——传输类故障恢复唯一由轮次承载
  （RescueRounds：轮 1 免包起跑、其后包新鲜门、30s→60s→120s 指数节流、3 轮耗尽上报熄灯门、
  耗尽态首包重武装）；巡检自续探重赛跑退役（RERACE_* 删除）；新翻沿通道
  `ClientCoreTunWaitFlip`（挂起式等待 ≤2min，JSON 进出）+ `tunStatusJSON` 新顶层键
  `rescueRoundsExhausted`；`ACTIVITY_FRESHNESS` 90s→10min。App 侧（tier 仓）配套：5s 事件泵
  退役（灯塔拍 5min 唯一周期机器）、四决策入口（翻沿/网络/前台/灯塔）、熄灯执行器。
  **实测**：稳态扩展 0% CPU；杀出口→轮 1 完成 59s（旧 5.5 分钟）→耗尽熄灯全链 7m50s（旧
  15.7 分钟）；熄灯后扩展进程彻底退出；打开 App 自动重连 0.7s connected。
- **TUN 读循环防自旋**：僵死 fd（poll 恒可读而 read 恒空）双阈值判死（≥100 拍 ∧ 墙钟 ≥1s →
  TunFdDead）——修 2026-10-15 真机事故（半死态误报重装引入恒约 7% 单核零日志自旋 107 分钟）；
  静默片（poll 无事件返 `Ok(Ready::default())`）不计数，健康空闲 fd 不误判。
- **五轮外部评审整改（连接/重连全链）**：消除「rerace 让路被当恢复成功⇒自救停摆永不熄灯」
  静默黑洞（ReraceOutcome 三态 + 在飞守卫提到 Rebind 之前 + 让路复读重跑）；B 门失败直达
  自救轮次（感知→首轮 ~3min 空窗消除）；健康位翻回即退出自救（换网路径不再在健康隧道上多
  挨一次全量重连）；`TunShared::is_healthy()` 语义单点（字段私有化）；pusher 闲置分档
  （1s→5s 业务拍）；世代收尾唤醒在飞等待者。
- **真机验收修复**：`tun_wait_flip` 入参 camelCase 漏 rename_all ⇒ 翻沿环忙轮询（118% CPU）；
  `patrol_reported` 本地账本不跟旁路恢复 ⇒ 同世代第二次死亡进不了自救。
- 判据行变更已登记 `docs/INTEROP-CRITERIA.md`（自救轮次行族/让路形态/传感观测改名/B 门直达
  自救）；规格真源 = tier 仓 `openspec/changes/event-driven-heal`（五轮评审记录 r1-r5）。

## v0.3.8（2026-10-15）

- **CLI 简化第一批：删除 `homeway portfwd` 直连命令**（用户指令：除 host add 外 --token 形态对用户无价值）——
  正式用法 = `homeway forward`；承载面（forward 命令 / 词表 / STREAM[dial]）零变化。依赖盘点确认无工具依赖；
  `connect --token` / `speedtest --token` / `files --token` / `token 变换` 被 matrix/perf-ab/qi-ab 使用，删除需
  连带改造三工具（另批）。**README 补 SOCKS5 段**（socks on/off/status）。
- **CLI 一致性批（全命令排查后修 7 项）**：attach/new 路由修复引出的系统排查——① socks on/off 无主机时的
  退出码 2 → **1**（与其余命令一致）；② `socks status` 认 `--host` 过滤（对齐 forward list，缺省仍列全部）；
  ③ files `--token` 直连形态**尊重 `--timeout`**（此前解析后静默忽略、固定 30s）；④ speedtest 的「还没有添加
  主机」提示与 default_host_ref **同源**（`no_host_hint` 单源）；⑤ term 与 files 的 stream.open 错误文案
  **合一**（`daemon_cli::stream_open_err_text`：stream_refused 用新版可行动文案 + 保留 bad_request 代际分支）；
  ⑥ `--timeout` 非法值文案两版统一；⑦ forward 组帮助注明「list 缺省列全部、--host 作过滤」。排查另确认
  `--state` 缺省三处同源、守护拨号缝无绕过、term 面无直组残留。
- **`term attach` / `term new`（进入形态）在客户端机报「出口未在运行」**（用户实测）：attach 直组目标、没走
  v0.3.5 起 list/new -d/delete 共用的 `--host` 缺省判定（本机面 / 默认主机规则）——`host_ref` 为空一律落本地面，
  出口机上碰巧成立所以一直没暴露。现与其它子命令同一条路；本地面拒 `--timeout` 的校验也由同一处内建。
- **macOS 文件夹预授权 `homeway serve grant`**（用户实测：term/files 碰「文稿/桌面/下载」时 TCC 弹授权窗、
  系统调用挂到用户点选，远端像卡死）：经控制面让**守护进程自己**触发三个受保护根的授权弹窗（TCC 按负责进程
  记录——终端里跑的 CLI 自己碰会把授权记到终端名下）；`serve token` / `serve start` 首次给一行提示
  （`grant` 完成后静音）；安装脚本配完自启动后询问「要现在预授权吗」（`--grant yes|no`）。判据登记
  `INTEROP-CRITERIA.md`「macOS 文件夹预授权」条（含残余：升级换二进制可能需重新授权，Developer ID 为根治）。
- **客户端断线自愈（MacBook Pro 实测缺陷，三层合修）**：Wi-Fi 瞬断后岛的重连只投当前端点、连败到 B 门
  后会话置 failed 终态永不恢复、半死连接上远程 term 操作永久挂。① 岛：重连赛跑改投**现任优先 + 全名册**
  （直连优先窗口照旧）；B 门前显式关掉半死连接、在册服务流按 EOF 收（之后开流立刻失败而非挂死）。② 宿主
  会话：收到不健康上报后**起世代重建线程**——退避 1s→2s→…封顶 30s 重赛跑全名册，成功回 ready（此前只置
  failed 终态）；`host status` 的 reason 显示「自动重连中」进度。③ CLI：`term list/new -d/delete/explain`
  的应答读受 `--timeout` 预算（缺省 10s），超时给可行动文案；`stream_refused` 文案改为「稍等几秒重试」。
  判据登记：`docs/INTEROP-CRITERIA.md` 2026-10-14「岛恢复接棒」条；`QUIC-ROADMAP.md` R 单候选「未动面」销账。

## v0.3.7（2026-10-14）

- **出口角色须显式启用**（用户实测驱动：新客户端机第一次 `term list` 提示加主机、第二次却列本机
  空会话表；`host add` 后 `term new` 建在了本地）：新 state 的配置缺省 **`[serve] enabled = false`**
  （模板与 `FileServe::default()` 两面；手编 `[serve]` 节省略 `enabled` 键仍 = 启用）——任何机器
  第一次拉起守护都不再意外成为出口（此前会生成身份、签发 token、建 `serve/term.sock`、监听 41641）。
  **出口机首装多一步 `homeway serve start`**（写 `enabled = true` + 装配，零参 launchd/服务随后按它起）；
  现有已写 `enabled = true` 的机器不受影响。
- **`--host` 缺省寻址：主机表文件不存在时不拉守护**——直接提示「还没有添加主机」（纯读命令在新机器上
  不再有「悄悄起一个守护」的副作用；文件在时仍经守护，主机表真源不变）。
- **`term` 本机面成立条件加第二维**：`term.sock` 在 **且** config `serve.enabled = true`——`serve stop`
  后残留的 sock 或曾意外起过出口的机器不再遮蔽远端主机。
- 判据登记：`docs/INTEROP-CRITERIA.md` 2026-10-14「出口须显式启用」条（CA11 拉起触发集收窄）。
- **CLI 输出梳理**（用户指令：简洁、少术语、提示直接说怎么做）：`homeway --help` 与全部子命令用法重写为
  「一行用途 + 人话选项」，删内部引用（M5 / F0 / carve-out / 矩阵 / 手机口径…）；未知命令时列可用命令表；
  `connect` / `token` / `portfwd` 标「调试用」；0 台主机提示与拉起守护的括注精简；**`socks on/off` 的
  `--host` 可省**（统一 host 风格时漏了它）。
- **默认端口换区间**（用户指令：旧值沿袭自 tailcat，同机并存撞端口）：出口 `41641` → **`38241`**、中继
  `:41741` → **`:38341`**（中继 4 处硬编码收成单常量）。只影响新装机器——已部署机器 config 显式写了端口；
  本机服务端口（7802/7724/7803/5300）不动（App 侧固定访问）。

## v0.3.6（2026-10-14）

- **term 多 attach 异宽场景的几何治理**（用户实测驱动，九项；全部在出口侧 raw 腿面）：
  - **清屏序 + 跳环尾**：会话几何真变时，上报几何 ≠ 新几何的 raw 腿收 `\x1b[2J\x1b[H`
    （不含 `3J`——回滚是历史不是残影；触发腿 / 同几何腿不清，单腿拖窗口零闪屏），
    且**同锁把该腿的环游标抬到环尾**——清屏前积压的旧几何画面不再在清屏后涌出重画残留
    （登记残余：pump 锁外读的在途 ≤32KiB 可闪现一瞬，通常被 TUI 全屏重画覆盖）。
  - **输入活动面收窄**（尺寸同步契约的「输入」= 用户操作）：**无键鼠标移动**（`?1003`
    hover 流）、**终端应答**（OSC / DCS / CSI 应答全集——DA、颜色、像素尺寸 `t`、kitty、
    modifyOtherKeys `m`、图形能力 `S` 等）、**焦点失去**（`CSI O`）三类**不记活动**——此前
    它们把会话几何在两腿间反复搬（每搬一次对面清屏 + 哨兵重排 + TUI 重画又发新查询，
    成反馈环；用户看到 UI 宽↔窄连跳）。焦点获得、键盘、按键鼠标、滚轮、粘贴仍算输入；
    判定保守（识别不出 = 记活动，不破坏「一敲即回本机网格」）。
  - **应答让位收窄到 active 腿**（用户拍板）：非 active 腿的纯应答帧不写 PTY（程序每次查询
    只得 active 腿一份答案，不再是到达顺序决定的多路矛盾答案）；hover / focus-out 不在丢弃面。
    已登记代价：查询与应答间 active 切换 ⇒ 该轮查询程序侧超时（TUI 有降级）。
  - **几何切换观测行**（debug 级）：`term: 会话 … 几何切换 → WxH（腿 … 输入 nB: <hex>）`
    三条入径全覆盖——再有异常直接按字节证据定位。
  - 判据登记：`docs/INTEROP-CRITERIA.md` 2026-10-13/14 共七条（均为超出 Go 基线的改进行为）。
- **CI 修复两处**：① `term_cli` 本地面测试去环境依赖（v0.3.5 的 `--host` 可选化后，测试依赖跑测试
  那台机器有出口在跑——CI 空环境自 v0.3.5 起一直红）；② 新增 PTY 测试程序改 POSIX `trap … WINCH`
  （zsh 专属 `TRAPWINCH()` 在 CI 的 bash 登录 shell 下不触发，首跑挂 30+ 分钟）+ 墙钟上界。
- App 核：surface 腿面零改（App 走 INPUT 帧，改动全在 raw 腿 DATA 入径与写者环游标）
  ⇒ tier pin 不动、设备包不出。

## v0.3.5（2026-10-13）

- **仓库改 MIT 许可**（用户指令）：新增 `LICENSE`；workspace 与四个 crate 的 `license = "MIT"`。
- **README 收束为用户面**（用户指令）：只讲功能与命令——去掉 `connect --token` / `files --token`
  这类调试形态、`--json`、`--state`、多实例与环境变量说明、`export/import/reset` 工件命令；
  命令示例统一为「加过主机就直接用」的简洁形态。
- **`files` / `term` / `forward` 的 `--host` 可选化**（用户指令：统一 host 风格）：
  `--host` 缺省时 = **单主机直接用、无主机提示先 `host add`、多主机要求显式指定**；
  `term` 另有本机出口面（`term.sock` 在就用本机）。`portfwd` 用法说明改向 `homeway forward`
  （其 `--token` 直连形态保留为自动化缝，不再进帮助）。
- **Windows 段一句话**（用户指令）：只说「`term` 不支持」。

## v0.3.4（2026-10-13）

- **CLI 本地 term 面：补 `<state>/serve/term.sock` 回退**（用户实测缺口）：出口角色的本地
  服务 socket 建在 `<state>/serve/` 子目录（`serve/files.sock` / `serve/term.sock` /
  `serve/speedtest.sock`），而 `homeway term …` 的**本机形态**按早期平坦布局口径拨
  `<state>/term.sock` ⇒ 「零参正式部署」下在出口机上拨不通（报「出口未在运行…」）。
  现改为**扁平优先 + serve 子目录回退**：`<state>/term.sock` 存在就用它（`--state`
  直指 serve 目录的老用法照旧）；否则用 `<state>/serve/term.sock`（统一根 / 零参形态）；
  都不在 ⇒ 错误文案逐字不变。落点 `crates/homeway-cli/src/term_cli.rs::local_term_sock`；
  证据 = 单测 `term_cli::tests::local_term_sock_flat_then_serve_fallback` + 出口机零参实测。

## v0.3.3（2026-10-13）

- **选路回归「直连优先」（用户指令）**：岛赛跑改为两阶段——**窗口内只投直连**（缺省 2s，
  `IslandConfig::direct_first`；`0` = 显式关），窗口到点或直连已无在途才**解锁中继**
  （同一轮）；有效窗口 = `min(配置值, 整轮预算/2)`。解锁两档各记一行（不静默）：`赛跑：直连窗口
  %v 到点仍无胜者 → 解锁中继候选 %d 个` / `赛跑：直连候选全部失败 → 提前解锁中继候选 %d 个`。
  口径登记：`赛跑投出` 行 = 本轮**候选名册**（中继的实际投出时点见解锁行）；结算的
  「完成/未完成」两清单并集 = **实际投出**的候选（判据变更记录 = `docs/INTEROP-CRITERIA.md`
  2026-10-13 三条 + 计数输入集一条）。
- **部署形态改简（用户指令）**：正式部署 = 系统 PATH 内的 `homeway`、**零参 / 子命令直跑、
  不带 `--state`**（state 走缺省 `$HOME/.config/homeway`）；`--state <dir>` 只用于测试、
  多实例、临时验证。`README.md` 的安装 / 运行节按此重写。
- **文档收束（用户指令）**：`README.md` 只保留功能与命令描述；`AGENTS.md` 重写为只描述本仓
  （工作规则 / 工程原则 / 技术底座 / 速查表）。
- **二进制改名 `homeway-cli` → `homeway`**（2026-10-13，用户指令：发布产物不带 `-cli`
  后缀）：`crates/homeway-cli` 包名不动（`-p homeway-cli` 构建契约零波及），产物 =
  `target/release/homeway`（Windows 面 `homeway.exe`）；版本横幅/用法文案/工具脚本
  的二进制路径与进程匹配形同批跟进；发版产物名 = `homeway-<tag>-<平台>.tar.gz`。
  判据行输出串零变化（登记 = `docs/INTEROP-CRITERIA.md`「判据变更记录」2026-10-13 条）。
- **Windows 支持（x86_64-pc-windows-msvc）**（2026-10-13，用户指令；设计 =
  `docs/reviews/WIN-design.md`）：relay/客户端域/出口（serve）三角色编译可用；
  **term（终端）不支持**（用户明示豁免——`homeway term` 报不支持退出）；唯一新增
  依赖 = `windows-sys`（cfg(windows) 门控，unix 构建零波及）。要点：
  - 平台底座四件：`sysock`（socket/poll：unix=libc 直通 / windows=winsock，reactor
    与 egress 钉卡面走它）+ `syslock`（flock/LockFileEx）+ `systime`（localtime_r/
    时区 API）+ `daemon/ipc`（unix=UDS 原样 / windows=TCP loopback+端口文件——控制面
    协议帧传输无关）。
  - App 核面（ClientCore/三桥/tun_exec/世代件）与 term 在 windows 不编译；跨平台件
    （BridgeStream trait/出口虚拟端口）拆 `facade/bridge_io`。
  - quic 岛：服务流泵的承载对 = unix socketpair / windows 回环 TCP 对（`sysio`）；
    出口三服务的 UDS 调试腿在 windows 不装配（QUIC STREAM 为主入口）。
  - CLI 停止面收敛 `stopsig`（unix=SIGINT/SIGTERM+pipe 原语义 / windows=控制台事件+
    回环对；SIGTERM 不可捕获——服务管理走控制面）；daemon 化 = setsid /
    DETACHED_PROCESS。
  - 门与验证：CI 增 `win-check`（cargo check 三 crate + --version/token 向量烟囱）；
    发版增 `windows-amd64` 产物（zip）；本地交叉 check 配方 = cc-check-shim 垫片
    （ring C 面）。windows 测试面是后续批（非目标登记）。

## v0.3.0（2026-10-10）

**传输层换代：WG → QUIC 单承载（M0–M7）**——**破坏性变更**，客户端与出口必须同代：

- **单承载 QUIC**（quinn 0.11 + rustls 0.23 RPK）：出口唯一公共 UDP 端口，`migration=true`
  + MTU 1400（DPLPMTUD）；一条连接 = 一台设备。
- **token 换代 `hmw1` → `hmw2` 段容器**：**存量 token 一律失效**（版本拒 + 可行动归因）；
  新增 `rpk` 段（出口 RPK 裸公钥）与端点类 `type=2`（QUIC）；客户端与出口必须同代
  （交叉两格硬失败）。中继 `rl1` wire 逐字节不变。
- **RPK 钉定 + 四帧准入**（Hello/Challenge/Proof + 刷新）与设备表语义（devTag 钉定、表满闸）。
- **服务流 `STREAM[tag]`**：files / term / speedtest / dial / probe 走 QUIC 双向流
  （端口 → tag 映射保留原值仅作映射输入）；出口侧服务入口 = UDS（调试面）+ STREAM。
- **WG 面全量退役**：`wgcore` / `wtransport` / `boringtun` / `tools/ring-shim` /
  `[patch.crates-io]` / 旧恢复阶梯（`RECOVER R1–R3`）整体删除（净删 ≈13,269 行）；
  `tools/check-wg-removed.sh` 十条常驻门防复活。
- **恢复阶梯重写**（QUIC 档）：快探（700ms 预算）→ 复探（×2）→ **M（迁移/Rebind，优先，
  成功即保连接）** → R（重连/重赛跑）→ **B 门**（连续 2 次 R 失败且窗 ≥10s 才上报世代重建）；
  `T_recv ≤ 3.5s`（在用档；待机档 60s 拍不承诺）。
- 构建档位改 **LTO + `codegen-units=1`**：OHOS `.so` = **2,958,896 B = 0.779× 判据**
  （≤3,800,000 B）；M6.7 修复后 2,968,016 B。
- 配置新增 **`serve.quic_listen`**（缺省 = `listen + 1`）：**回滚到 v0.2.x 必须删除该键**
  （旧二进制 `deny_unknown_fields` 拒启）。承载三键（`HOMEWAY_TRANSPORT` / `serve.quic` /
  `tunConfig.transport`）退役。
- 真机读数（M5/M6/M6.7 同设备同仪器）：T2 热 **0.893×**（**未达 0.95×，已登记**，
  `docs/PERF-AB.md` §9.20.11）/ T6 1.36× / 断线恢复 **2.447s·0.918s** / 内存五格过
  （+3 MiB 显式 socket 缓冲入账）/ 体积 0.779×。
- 判据面：`docs/INTEROP-CRITERIA.md` 新增 M0–M7 各批登记（含 M7 补登 7 条：M6.5 TUN 读面
  形态 + 写阻塞语义偏离 / M5 C8 值域收窄 / `SessionSnapshot::stats` 来源换岛 /
  `--dead-direct`·`--loopback-only` 承载无关 / `daemon::StreamConn` 写停滞 30s→10s /
  M6.7 socket 缓冲与内存账 / **M7 S1 旧 token 换代归因**）。
- 回滚件（生产切换前置，**七件**，切换/回滚步骤见 `docs/DEPLOY-RUST-EXIT.md` §12）：
  两台旧二进制 + 两台 `config.toml` + plist + 两枚旧 `hmw1` token + G1 核 `.so` +
  旧 HSP 重建配方 + 两台 state tar（`key.bin`/`tokens.jsonl`/`relay.key`）。

## v0.2.3（2026-10-07）

拦截层单线程 reactor 简化批（出口重拨 OS socket 从 8-worker 池收进单事件循环——
消灭本仓 bug 密度最高的管道族；协议语义零改动）：

- **worker 池/泵管道整体拆除**（pool.rs 全删）：重拨 socket 归驱动线程单循环
  （pump 内 `poll(2, timeout=0)` 自查就绪——电平触发 + 全非阻塞，就绪集是提示不是
  契约），每流一个 `ReactorIo`（读/写兴趣位 + TCP 字节流/UDP 数据报双待写缓冲 +
  `OwnedFd` 恰一次关闭），无锁无跨线程流队列——出口常驻线程 **-8 起** + 暂态拨号
  线程 0。R3 池族三 bug（fd 属主表泄漏 EMFILE/Adopt 时序/池形态）、Written 差额
  补报（files 上传卡死根因族）、Ack 短返清账、收工 Closed 回执竞态（丢尾包）、
  ubuntu CI worker 饿死——这些**代码路径整体不存在**。
- **非阻塞拨号三分类**（libc socket+fcntl+connect，禁 std connect〔std 把
  EINPROGRESS 当 Err〕）：即时成功与 POLLOUT 验收统一收口 dial_accept；EAGAIN
  （Linux UDS backlog 满）每拍重拨共用死线；UDP 拨号同步化（bind+connect 本地
  即时——pending 重放窗口坍缩为零）；connect 10s 死线（Go dialTimeout 同值）。
- **背压清账通道删除**：unacked_out/Written 差额补报/Ack 往返全删——水位直读
  （tx_backlog ≥256KB 摘 POLLIN 停读；待写缓冲 >256KB 门控栈读）；flush_out 三态
  步进（EAGAIN 必 break 等 POLLOUT——原地重试 = 驱动线程自旋 = 整出口挂死，评审
  高危整改）。
- **死分支删除**：drain()/linger_rst/SO_LINGER 面（Rust 侧从未接线——R3-design
  §4.1 M13 差异正式登记）。
- **新观测行**（verbose 5s 面）：`intercept: reactor 观测 pump=N/5s（均周期…ms）
  名下fd峰=N 单拍峰=Nµs`。**判据行/Stats 同串同口径零变化**（E5/E10/E11/E12/
  dialfail 计数——评审机械比对，新增仅观测与 POLLNVAL 防御两行 verbose）。
- 两道 dsh 评审门全过：设计门 16 条（1 高：非阻塞 connect 即时成功无验收点）+
  代码门 11 条（1 高：flush_out EAGAIN 自旋——修复含单元钉反向验证闭环〔注入
  必红 abort〕）。记录 = `docs/reviews/reactor.md`，设计 = 
  `docs/reviews/reactor-design.md`（v2.1）。
- 测试面 +5：reactor_interests_matrix（兴趣集四态纯函数）/ uds_exempt_flow_
  end_to_end（UDS 即时 connect 回归钉 + reactor_fds 归零）/ flush_out_eagain_
  returns_with_backlog（EAGAIN 语义单元钉 + 看门狗 abort）/ upstream_partial_
  write_continues（停读→开读→按序完整）/ wait_hint_states。harness 有损链路
  A/B 臂绿；ci-local quick 全绿（含 RRR 矩阵冒烟）。

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
