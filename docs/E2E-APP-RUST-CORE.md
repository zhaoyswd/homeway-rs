# E2E：真机 App（Rust 核 libclientcore.so）× {Rust exit, 现役 Go 出口}

> R7 第 2 棒（HSP 集成）真机全量判据记录。日期：2026-10-04 18:19–18:43。
> 手机：FMR0224116011480（主力机），Rust 核产物 = `9af328fd5753+dirty-rust`
> （1.98MB vs Go 版 9.7MB），tier `CORE_IMPL=rust` 档构建（51036b4）。
> 判据口径：全部真凭据（核日志行 / 出口日志行 / 系统计数器 / UI dumpLayout 文本）。
> 现役出口（~/bin/homeway v0.16.0，launchd）全程只读（取 token 一次、读日志），未动。
>
> **第 3 轮（7l 整改批复验 + 7m 补验）见 §6——2026-10-04 19:53–20:21，核 =
> `ea00b2c4e781-rust`（整改批 0baca9f + 微修，tier rust 档新脏检出闸/钉定门首跑通过）。**

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
| A1 | running 行 | ✓ | `running (mtu=1280 tunIp=100.64.204.150)`（真实派生地址；mtu = 请求值——默认 1280，P2 opt-in 档 1380 时此处为 1380，生效值看 `mtu: 档位` 判据行） |
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

> **第 3 棒收口（§6.A4）**：上述折衷已由「对账快照」面收口——attached 后 1.5s
> 每世代产一行与 `ClientCoreTunStatus` 同源（同纯函数 + 同组装件 runner_of/
> transport_of）的真机字节快照，键面/键序/值域逐项过账，见 §6。

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

## 6. 第 3 轮（7l 整改批复验 + 7m 补验四项；2026-10-04 19:53–20:21）

核 = `ea00b2c4e781-rust`（整改批 0baca9f + SpeedHost 相位微修；2055944B ≈ 1.9MB）。
tier rust 档**新脏检出闸 + HEAD 钉定门首跑通过**（4b8a0a1：核源干净检出、
产物标记 SHA == HEAD）。轮 3 拓扑 = 轮 1 同款本地 Rust exit（42670）+ relay（42770），
token 经 `serve token` 取、`aa start --ps host_token` 注入。

### A. 判据逐项

| # | 项 | 结果 | 证据 |
|---|---|---|---|
| A1 | 版本/身份/数据面装配 | ✓ | `身份：复用（dev=7dc61647 pub=0e6dc129）`（与轮 1/2 同 devTag 跨核稳定）；`隧道侧就绪（L3 直通…）`；`新栈会话已建立（token 端点 2 个…）` |
| A1 | 候选/赛跑/warmup/attach/桥 | ✓ | `候选端点（2 条…LAN、中继）`→`赛跑结算：胜出 直连 192.168.3.12:42670（镜像 1 包，耗时 24ms）`→`warmup pong: 就绪（判据=wg）`→`running (mtu=1280 tunIp=100.64.248.46)`（mtu = 请求值，同上）→`attached（数据面已接管 fd=90，L3 直通）`→三座桥监听行 |
| A1 | link/RREG/peer 表 | ✓ | `link: via=direct ep=192.168.3.12:42670 rtt=25ms`；`RREG 注册刷新 → …（中继=false）`；exit `peer: + dev=7dc61647 pub=0e6dc129 ip=100.64.34.94 n=1/32`（三指纹逐字段一致）→ `peer: ~ refresh` |
| A4' | **tunStatusJSON 真机对账快照**（7m-④ 收口） | ✓ | attached 后 1.5s 产出完整 JSON（§6.B 逐项过账）；与 `ClientCoreTunStatus` 同源同字节面（同纯函数 + runner_of/transport_of 同组装）；轮 3 两世代各产一行（本地 exit 轮 + 现役出口轮），键面/键序/值域全符合 |
| A5' | **测速相位 UI**（M-7 真机验证） | ✓ | 卡片副标题实测 `下行测速中` → `上行测速中` 两相位切换（整改前恒「连接中」的 UI 死相消除）；再点卡片 = 取消 → UI 回 idle 并恢复旧结果副标题（M-8 取消面真机无 hang） |
| A5' | **测速 EOF 归因**（7m-①） | △ 定位收口 | 下行 4 会话全成（各 ~12s、109–142MB ≈ 94Mbps——桥/WG/服务/代答回执全通）；上行 4 连接建立 + 请求送达 + **数据帧头已到**（exit EOF 落在帧载荷中途 ⇒ 有真实数据流入），但窗口内未跑完 → 客户端轮挂到 82s 看门狗 kill（App 终态 `失败（timeout）测速连接失败：连接超时` = 看门狗重写面）→ 双侧 EOF。**归因 = phone→exit 的 WG 上行 bulk 吞吐退化**（同引擎 host 侧 up=411Mbps、同签名第 2 棒旧核即存在 ⇒ 非本批引入；与 R6.6 登记的真机 encap/UDP 发送路径瓶颈同族——CC 垫片只覆盖下行）。修复归 R8 性能终测（smoltcp 0.14 + 发送路径 profile 判据已在 R6.6 §三立项） |
| A4' | **term 键盘输入**（7m-② 收口，超出预期） | ✓ 机器采证完整 | ① 回显视觉证据：surface 上 `zhaozhe@MacBook-Pro ~ %` + `echo R7KEYTEST` 回显行（`uitest uiInput text` 能进 native surface——第 2 棒登记的「HID 不进 surface」对 `uiInput text/keyEvent` 不成立）；② 副作用硬证据：`touch /tmp/R7KEY_OK` + Enter(2054) → 文件真实出现在 exit 主机 `/tmp/`（键盘→INPUT 帧→隧道→term 服务→PTY→zsh 执行全链闭环）；③ 退出自灭：`exit` + Enter → `term: 会话 … 腿断开（kind=app 原因=finish）` + App `长时任务来源全部释放（-term）`；期间检测状态机 `shell/working（依据=output）`→`shell/idle` 转变。**无需真手指复测** |
| A3' | **files 上传**（7m-③） | △ 维持登记 | 系统 picker（DocumentViewPicker）对自动化不稳定：dumpLayout 时隐时现、行选择 tap 无法稳定注册（「已选 0/1」不翻），无法机器完成「选文件→完成」步；picker 关闭后无任何上传日志行（`beginUpload` 未到或选择为空——两态不可区分）。**归因维持 E2E P2-4**：picker→copy→beginUpload 是 App（ArkTS）侧已知静默问题面，非 Rust 核新增面（轮 2 已证 files 桥/list/下载本体）。真手指复测清单保留 |
| A6' | 整改批回归 | ✓ | 建连全链（M-2 早期登记/M-3 attach 时序无回归——首次 attach 即 0）；巡检/旁路探测（8s 窗）/stats 正常节拍；挂起位/健康位无异常行；hint/缓存接线在跑（`旁路探测：…已入学习缓存` 每拍） |

### B. tunStatusJSON 对账快照过账（7m-④）

真机快照（attached 形态，节选）：`{"bridgeAuth":"5449…（96 hex，TIERBRIDGEAUTH01 前缀）","bridgeFilesSock":…,"bridgeSpeedSock":…,"bridgeTermSock":…,"code":"","demand":{"active":false,"at":…,"fg":false,"localErrAdopted":0,"localErrTotal":0,"outboundAt":…,"reason":"熄屏（位陈旧）"},"elapsedMs":1502,"exitIp":"100.64.255.1","identity":{"dev":"7dc61647","pub":"0e6dc129"},"link":{"at":…,"ep":"192.168.3.12:42670","rttMs":11,"via":"direct"},"meowed":true,"portForwards":[],"readyBy":"wg","reason":"","running":1,"state":"attached","stats":{…},"tunIp":"100.64.248.46"}`

- **键序**：顶层 18 键严格字典序（Go `json.Marshal(map)` 同序）；demand 7 键字典序 ✓
- **键面**：与 Go `TestServiceSnapshotJSONReadyWithBridgeKeys` 守卫集合一致 +
  **`localErrAdopted/localErrTotal` 两键真机在位**（拍板① bind 发送统计面的消费端）
- **值域对账**：identity 与同刻 `身份：复用` 行逐字段一致；tunIp 与 `running (…tunIp=…)` 行一致；link 与同秒 `link:` 行一致；portForwards 空表 = `[]`（Go make 同形）

### C. 第 3 轮收尾确认

- 手机最终态：连现役 Go 出口（`link: via=direct ep=192.168.3.12:41641`、RREG 刷新）；
  活跃主机已恢复（hmw1GiFSJ…izJ5tQ）。
- 本地实例（exit 42670 / relay 42770）已 kill + state 清除；`pgrep homeway-cli` 空；
  现役 `~/bin/homeway`（PID 52279）全程未动。
- 真机铁律遵守：无锁屏/息屏；hdc 一律 `-t`。

### D. 收口烟囱（r3 复核批后的最终核）

复核 r3 整改批（3ab9891：F1-F14）装机回归烟囱——核 `861094592d8d-rust`（2066152B
≈ 1.9MB；tier 整串钉定门过）：连**现役 Go 出口** `身份：复用（dev=7dc61647
pub=f4f522f0）` → `warmup pong: 就绪（判据=wg）` → `attached（数据面已接管 fd=90，
L3 直通）` → `link: via=direct ep=192.168.3.12:41641 rtt=7ms` → 对账快照产出——
r3 批零回归。手机终态 = 该核连现役出口（用户常态）。

## 7. 三轮判据总表（R7 真机全量合并）

| 域 | 判据 | 轮 1（Rust exit） | 轮 2（现役 Go 出口） | 轮 3（整改批复验） |
|---|---|---|---|---|
| 建连 | 身份/候选/赛跑/warmup/attach/桥/link/RREG/peer | ✓ | ✓（RREG v2 被现役接受） | ✓ |
| 数据面 | L3 真负载（vpn-tun 双向 + 核 stats 对表） | ✓ | ✓（+109KB） | ✓（stats 节拍 + 计数对齐） |
| 数据面 | TCP 过境拦截 transit（dialok） | ✓（38 条） | ✓（15 条） | ✓（7803 exempt dialok ×N） |
| 数据面 | DNS 代答 | ✓ | ✓ | ✓（轮 3 有 dns 查询流量） |
| files | 浏览/list | ✓ | — | ✓ |
| files | 下载 10MB 本体 | ✓ | — | — |
| files | 上传 | ✗ 受限 | — | △ picker 自动化受限（App 侧 P2-4 维持登记） |
| term | 新建/接入/stateV2 | ✓ | — | ✓ |
| term | 键盘输入回显/退出自灭 | △ HID 受限 | — | **✓（机器采证完整：回显+touch 副作用+exit 自灭）** |
| 测速 | 桥腿 dialok（7803） | ✓ ×4 | — | ✓ ×8 |
| 测速 | 相位 UI（down→up 切换） | — | — | **✓（M-7 真机验证）** |
| 测速 | 完整轮（done 信封） | △ EOF 未定位 | — | △ 归因收口：上行 bulk 吞吐退化（R8 修复；下行 ~94Mbps 全成） |
| 测速 | 取消（cancelled 归因） | — | — | ✓（再点取消 → idle 恢复，无 hang） |
| 恢复 | 杀 exit 阶梯 R1→R3 同串/最坏带 | ✓ | — | —（整改批 M-4 外层硬超时单测面覆盖） |
| 恢复 | exit 回来 R1 命中 3.1s/中继腿兜底 | ✓ | — | — |
| 恢复 | 中继驻留 via=relay | ✓ | — | — |
| 挂起 | 后台 90s 存活 | ✓ | — | — |
| 契约 | tunStatusJSON 字节对账 | △ 构造守卫替代 | — | **✓（对账快照：键面/键序/值域过账）** |
| 集成 | 版本注入/体积 | ✓ 1.98MB | — | ✓ 1.9MB（ea00b2c4e781-rust，钉定门首跑过） |

**R7 收口口径**：全量判据里剩两处非阻断残留——① 测速上行吞吐（归 R8 性能批，
判据已立项）；② files 上传 picker 自动化（App 侧已知问题，真手指复测清单）。
均按项目惯例登记不阻塞 R7 收口。
