# E2E：真机 App（Rust 核 libclientcore.so）× {Rust exit, 现役 Go 出口}

> R7 第 2 棒（HSP 集成）真机全量判据记录。日期：2026-10-04 18:19–18:43。
> 手机：FMR0224116011480（主力机），Rust 核产物 = `9af328fd5753+dirty-rust`
> （1.98MB vs Go 版 9.7MB），tier `CORE_IMPL=rust` 档构建（51036b4）。
> 判据口径：全部真凭据（核日志行 / 出口日志行 / 系统计数器 / UI dumpLayout 文本）。
> 现役出口（~/bin/homeway v0.16.0，launchd）全程只读（取 token 一次、读日志），未动。

## 0. 形态

- 轮 1：Rust 核 × **本地 Rust exit**（`homeway-cli serve --state /tmp/homeway-r7-exit
  --listen 42670 --public-endpoint 192.168.3.12:42670 --files-root … + 本地 Rust relay
  42770`）；token 经 `serve token` 取、`aa start --ps host_token` 注入。
- 轮 2：Rust 核 × **现役 Go 出口**（41641；token 只读经 `~/bin/homeway serve token`）。
- UI 操作：uinput 点击（全局代理开关 [1056,779]）+ dumpLayout 文本判据。

## 1. 首轮发现并当场修复（Rust 核缺陷 1 枚）

**TUN fd 非阻塞 EAGAIN 判死健康隧道**：首轮 18:21 建连成功后立即
`tun fd 读取失败：Resource temporarily unavailable (os error 11)（标记隧道不健康）`
——OHOS 的 VPN fd 是**非阻塞**的（Go tunfd_unix.go 有专门注释与 poll 处理，
「裸读立即返回 EAGAIN，被当错误上报会当场把健康隧道判死」——Rust 版漏移植该
语义）。修复：`wgcore::tun_read_loop`/`write_fd_all` 的 EAGAIN 分支改
`poll(fd, POLLIN/POLLOUT, 500ms)` 再续（同 Go 节拍）；stop 位在 poll 片界检查。
重编重装（18:28）后复验：建连全链路干净、无 fd 错误行。

## 2. 判据逐项表

时间轴 2026-10-04 本地；「手机」= `tailcat-tun.log`（Rust 核，时间戳格式
`[YYYY-MM-DD HH:MM:SS.mmm] tier-core: …`），「exit」= 本地 Rust exit stdout，
「现役」= `~/.config/homeway/cache/debug.log`。

### 轮 1：Rust 核 × 本地 Rust exit

| # | 项 | 结果 | 原文判据行 |
|---|---|---|---|
| A1 | 版本面 | ✓ | 扩展 hilog `tailcat tun prepare ver=tier core 9af328fd5753-rust (rust 1.99.0, c-shared)` |
| A1 | 身份 | ✓ | 手机 `身份：复用（dev=7dc61647 pub=2f6f8870）`（devTag 与 Go 核同源——master.key 跨核复用） |
| A1 | 数据面装配 | ✓ | 手机 `wgcore: 隧道侧就绪（L3 直通；隧道地址 100.64.0.216，后端隧道 IP 100.64.255.1，核心自连经 B 拨隧道 IP）` |
| A1 | 会话/候选 | ✓ | `新栈会话已建立（token 端点 2 个…）`；`候选端点（2 条…）：192.168.3.12:42670（LAN）、192.168.3.12:42770（中继）` |
| A1 | 赛跑/路径 | ✓ | `赛跑结算：胜出 直连 192.168.3.12:42670（镜像 1 包，耗时 22ms）`；`路径确立：直连 …（首个回包来源）` |
| A1 | warmup | ✓ | `warmup pong: 就绪（判据=wg）` |
| A1 | running 行 | ✓ | `running (mtu=1280 tunIp=100.64.204.150)`（真实派生地址） |
| A1 | attach | ✓ | `wgcore: 应用面就绪（TUN fd=90 已接上，mtu=1280，transit 直通）` → `attached（数据面已接管 fd=90，L3 直通）` |
| A1 | 隧道桥 | ✓ | 三座桥 `files-bridge/term-bridge/speed-bridge unix:…/identity/bridge/*.sock → 出口虚拟端口（经会话）监听中` |
| A1 | 链路 | ✓ | `link: via=direct ep=192.168.3.12:42670 rtt=27ms（新栈状态快照）` |
| A1 | RREG | ✓ | `RREG 注册刷新 → 192.168.3.12:42670（dev=7dc61647，中继=false）` |
| A1 | exit peer 表 | ✓ | exit `peer: + dev=7dc61647 pub=2f6f8870 ip=100.64.0.216 n=1/32`（三指纹与手机身份行逐字段一致） |
| A2 | L3 真负载（系统侧） | ✓ | `vpn-tun` RX 576B → 1,086,746B(1199 包) / TX 121,108B(864 包)——双向增长（baidu+wikipedia） |
| A2 | L3 真负载（核 stats） | ✓ | 手机 `stats: fdReadBytes=121148B fdWriteBytes=1094573B ｜ pf=0/0`——与设备 vpn-tun 双向对表吻合（fdRead=TX / fdWrite=RX） |
| A2 | TCP 过境拦截 | ✓ | exit 38 条 `intercept: tcp transit 198.18.x.x:443 ← 100.64.204.150:xxxxx（dialok）`（transit 源=TUN 派生地址——双地址口径正确） |
| A2 | DNS 代答 | ✓ | exit `dns: q=34 qtcp=0 resp=18 filter=17 trunc=0 … malformed=0` |
| A2 | 页面真加载 | ✓ | dumpLayout 见 `维基百科`/`中国大陆维基媒体活动` 文案 |
| A3 | files 浏览 | ✓ | App `文件管理：发起桥请求（重建=0 sock=有 auth=有）` → `已连接主机 …（经 VPN 通道的文件桥）` → 页面列出 `r7-10mb.bin`/`r7-marker.txt`；exit 7802 `intercept: tcp exempt …（dialok）` |
| A3 | files 下载 10MB | ✓（本体） | 长按→「下载」→ 10MB 取件完成（模态到「将文件保存至 "Download"」选择器）；App `长时任务来源变化：-files`。保存选择器的系统 UI 确认按钮自动化受限（下载链路已证；E2E P2-5 同类） |
| A3 | files 上传 | ✗ 未测 | UI 自动化受限（picker 流；E2E P2-4 已知 App 侧静默问题——非 Rust 核新增面） |
| A4 | term 新建会话 | ✓ | exit `term: 新建会话 2d397261（pid=45129 49x34 shell=/bin/zsh）`；App `[term] state → Connected (gen=1, 距受理 989ms)`；7724 exempt dialok |
| A4 | term 版本门/stateV2 | ✓ | App `[term] 会话状态 → 空闲（STATE 帧 stateV2）`（Rust term 服务 × C++ surface 兼容） |
| A4 | term 输入回显/退出自灭 | △ 受限 | HID 键事件未进 native terminal surface（自动化限制）；链路面〔新建/接入/stateV2/surface 帧〕已证。真手指复测留第 3 棒 |
| A5 | 测速完整轮 | △ 受限 | 测速卡片点击不导航（3 败即停）；**7803 桥腿 dialok ×4 已证桥面通**；exit 侧 `speedtest: 会话 #9-#12 role=send warmup=2s window=10s` 后 `异常（读上行载荷：EOF）`——归因未定位（不确定来自本轮或装机前旧轮），登记第 3 棒复查 |
| A6 | 恢复：杀 exit → 阶梯 | ✓ | 18:37:27 杀 exit：`对端巡检失败 1/3: 连接超时` → `RECOVER R1 重握手（原因=巡检失败）：补注册 + 丢会话（保采纳）` → `RECOVER R2 换源…换本地 socket（保采纳）` → `RECOVER R3 重赛跑…清采纳，学习缓存候选兜底` → `RECOVER 走完 R1→R3 仍未恢复（…耗时 39.908s）—— 交上层升级`（失败当拍 R1 语义 ✓、档位推进同串 ✓、最坏带 39.9s ≈45s 界内 ✓） |
| A6 | 恢复：exit 重启 → 自愈 | ✓ | exit 18:38:5x 回来：`RREG 注册刷新 → 192.168.3.12:42770（dev=7dc61647，中继=true）`（**中继腿兜底注册**）→ `RECOVER 恢复于 R1 重握手（原因=巡检失败，起跑=R1 重握手，耗时 3.149s）`（R1 命中 3.1s ≤4s ✓）→ `巡检失败后阶梯已恢复（不用等 3 连败）` |
| A6 | 中继腿驻留 | ✓ | `link: via=relay ep=192.168.3.12:42770 rtt=12ms（新栈状态快照）`（连续多拍） |
| A7 | 挂起（后台 90s 回前台） | ✓ | Home 退后台 90s → 回前台：隧道全程存活（3 拍 link 行连续无恢复动作；`链路巡检：App 回到前台，立即探测一次` kick 生效） |

### 轮 2：Rust 核 × 现役 Go 出口（v0.16.0，41641）

| # | 项 | 结果 | 原文判据行 |
|---|---|---|---|
| B1 | 身份 | ✓ | 手机 `身份：复用（dev=7dc61647 pub=f4f522f0）`（与现役出口历史身份行一致——跨后端 devTag 稳定） |
| B1 | 候选/学习缓存 | ✓ | `候选端点（15 条…）`（token 4 端点 + IPv6 学习缓存）；`发送失败：Address family not supported by protocol (os error 97)（…本地错误=该候选在本机就发不出去，与对端无响应是两回事）`（AF 不支持候选的本地错误行——Go 同构） |
| B1 | 赛跑/路径/warmup/attach | ✓ | `赛跑结算：胜出 直连 192.168.3.12:41641（镜像 1 包，耗时 15ms）`；`warmup pong: 就绪（判据=wg）`；`attached（数据面已接管 fd=90，L3 直通）` |
| B1 | 链路/RREG | ✓ | `link: via=direct ep=192.168.3.12:41641 rtt=10ms`；`RREG 注册刷新 → …（中继=false）`——**Rust 客户端注册报文 v2（H2）被现役 Go 出口接受** |
| B1 | 现役出口侧 peer 表 | ✓ | 现役 `peer: ~ dev=7dc61647 refresh (idle=0s) n=4/32`（同 devTag 只刷新——设备身份持久化语义跨核跨后端一致） |
| B2 | L3 真负载 | ✓ | vpn-tun RX +109,497B（wikipedia）；现役 exit 本轮 `intercept: tcp transit` 15 条；`dns: q=96 qtcp=0 resp=50 filter=49 …` |

## 3. tunStatusJSON 真机字节对账（第 1 棒尾巴）——折衷记录

完整字节对账未在真机执行（设备侧无直接调 NAPI 的通道）；替代验证 =
① runner/transport 键面的构造输入守卫（tun_status.rs，Go 向量 10 案 + 键集合
守卫已绿）；② 真机消费面证明：App 首页卡片实时显示 `直连`/`15 ms`/
`↑121.6 KB ↓1.1 MB`（= link/stats/identity 段经 App IPC 消费成功）+
`界面状态 → connected（…系统VPN=up，心跳时间戳=…）`。字节级 diff 归第 3 棒
（如需可加 App 侧诊断 dump 面）。

## 4. 问题分级清单

**P1（本轮修复）**
1. TUN fd 非阻塞 EAGAIN 判死隧道（§1）——已修 + 复验。

**P2（受限/登记，不阻塞）**
2. 测速完整轮未完成：UI 入口交互受限 + exit 侧 speedtest 会话 EOF 异常归因未定位（可能为装机前旧轮残留）——第 3 棒复查（App 侧 SpeedTest 日志行 + Rust 引擎 logf 落核日志面）。
3. term 输入回显/退出自灭：HID 事件不进 native surface——真手指复测（链路面已证）。
4. files 上传：E2E P2-4 已知 App 侧静默问题（picker→copy→beginUpload），非 Rust 核新增。
5. 「保存选择器」确认按钮自动化受限（下载本体已证）。
6. tunStatusJSON 字节级对账以构造输入守卫 + 消费面替代（§3）。

## 5. 恢复与清场确认

- 手机最终态：连现役 Go 出口（UI `主机 192.168.3.12 / 直连 / 10 ms`）；活跃主机已恢复。
- 本地实例（exit 42670 / relay 42770）已 kill；`pgrep homeway-cli` 空；现役
  `~/bin/homeway`（PID 52279）全程未动。
- 真机铁律遵守：无锁屏/息屏操作（仅 wakeup/timeout 只读查询）；hdc 一律 `-t`；
  tier 仓跟踪文件改动仅 build-core.sh（CORE_IMPL 开关）。
