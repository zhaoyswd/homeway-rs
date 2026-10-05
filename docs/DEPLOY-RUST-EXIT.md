# DEPLOY-RUST-EXIT — 两台生产主机换装 Rust 出口（homeway-rs B 批，2026-10-05）

> 换装执行记录 + 回退手册。授权链：用户明确指令（发版 → 部署 → 停删）。
> §9 = B0-2a 滚动升级（v0.2.1，生产可观测性批）。
> 前置 = B0-1 部署阻塞集清零（v6 族/期望态装配/单实例锁真网实证，判据 =
> `INTEROP-CRITERIA.md` B0-1 节）。发版产物 = **v0.2.0**（tag `v0.2.0` = commit
> `b1f1410`）。**token 是凭证不进仓库**——本文一律掩码（`hmw1…`/`rl1…` 前缀 + 尾 4）。

## 0. 发版 v0.2.0

- **前置红修**：`cb64fce`（B0-1 收口）首跑 CI 双红——
  - ubuntu `pin_socket_dual_family_tolerant`：负例「非法 index ⇒ 两族 Err」是
    macOS 语义，linux 走 SO_BINDTODEVICE 按名（index 不参与）前提不成立；且
    **已钉过的 socket 重复设置在无特权下恒 EPERM**（内核只放行首次绑定；容器
    实测 fresh=OK / re-set-same=EPERM / 坏名=ENODEV），复用 s4 把权限形态误判成
    钉卡语义。修复 = `b1f1410`（负例分平台：macOS 非法 index / linux 新 socket
    + 不存在网卡名 ⇒ ENODEV）。
  - macos `exit_code_passthrough_and_surface_leg` 一轮红（等 ATTACHED 收 EOF）：
    本地 20 连绿 + 修复轮 CI 绿，判 CI 负载 flake 未再现（与 0a92758「共享
    runner 忙转饿死」同族登记）。
- tag `v0.2.0`（annotated，打在 origin/main 头 `b1f1410`）→ Release run
  `37264245655` **5 job 全绿**（darwin-arm64/amd64 + linux-amd64/arm64 + release）。
- 产物核验：SHA256SUMS 双端各下 `--check` OK；**darwin-arm64 本机 `--version` =
  `homeway-cli v0.2.0`**（装机 sha `edf4f739fee269b0…`）；**linux-amd64 阿里云
  `--version` = `homeway-cli v0.2.0`**（static-pie，装机 sha `521bcb38b23d2f0e…`）。

## 1. Mac 出口换装（2026-10-05 12:39–12:48）

**先备份**（只备份不迁移——Rust 用新身份新 token，同主机同端口，手机重贴即可）：

| 件 | 路径 | 校验 |
|---|---|---|
| Go 二进制备份 | `~/bin/homeway.bak-go-prerust` | sha256 `661c2fdb651b55aa…`（= 换装时在跑的本地构建 `0.0.0-dev`，非 Release 工件） |
| Go state 全量 tar | `~/homeway-state-backup-go-prerust-20261005-123942.tar.gz` | 74 条目（socket 类自动跳过）；`serve/key.bin`/`serve/tokens.jsonl`/`config.toml` 在包内、解包验证通过 |

**部署**（Go 二进制不动名先保留，后统一清理见 §3）：

- 二进制：v0.2.0 darwin-arm64 → `~/bin/homeway-rs`（sha `edf4f739fee269b0…`）。
- **plist** `me.zhaozhe.homeway-exit`：ProgramArguments =
  `[/Users/zhaozhe/bin/homeway-rs, --state, /Users/zhaozhe/.config/homeway-rs]`；
  stdout/stderr → `~/.config/homeway-rs/cache/exit.log`；WorkingDirectory =
  `~/.config/homeway-rs`。⚠️ **非纯零参**：Rust 默认 state = `~/.config/homeway`
  与保留为备份的 Go state 同路径，零参会读到 Go 的旧身份/旧 config——必须显式
  `--state` 指向新目录（`~/.config/homeway-rs`）。切换 = `launchctl bootout` →
  改 plist → `launchctl bootstrap`。
- **新 state** `~/.config/homeway-rs/config.toml`：`[serve] enabled=true
  listen=41641 relay="rl133ZkG…（阿里云 Rust 中继，§2 配好后补）"`；其余走默认
  （bind_interface=auto / upnp=true / stun=stun6=stun.cloudflare.com:3478 /
  files_root=$HOME / max_peers=32 / dns_port=5300）。

**启动判据**（12:40:14–12:40:22，`cache/exit.log` = `cache/events.log` 同串）：

- `serve: 按期望态装配（config serve.enabled=true）` → `凭证：…已铸出新凭证（客户端需重新粘贴新 token）` → `凭证台账：1 行记录 / 1 枚在用凭证…`
- `绑卡：自动挑到 en0（index=6 up addrs=[192.168.3.12/24]）` → `绑卡看护：WG socket 钉在 en0（index=6 …）`
- `dns 代答就绪` / `files 就绪：root=/Users/zhaozhe (rw) sock=…files.sock…` / `speedtest 就绪` / `# Serving terminal sessions on …term.sock (shell=/bin/zsh …)`
- `后端身份：标签 d07c57dd5bde1fa7 ｜公钥 8aee740b7220…`（新身份）→ **`serve 就绪：wg=:41641（配置端口…）… key=8aee740b7220…`（端口沿用 41641）**
- **v4/v6 双公布**：`STUN：监听 socket（本地 41641）在 stun.cloudflare.com:3478 眼里是 114.242.60.128:41641` + `公网端点：IPv6 路径可用（STUN 看到 [2408:…deb7:9c81]:41641），公布 …` + `公网端点：已公布 [114.242.60.128:41641 [2408:8207:2518:2550:383e:8734:deb7:9c81]:41641]（写进 public_endpoint.txt…）`
- `homeway: 统一进程就绪（state=…，serve=true relay=false，…，version=v0.2.0）`
- **单实例锁**：手动二次启动被拒（逐字）`homeway 已在运行（pid 18245，形态 unified，state=/Users/zhaozhe/.config/homeway-rs）——拒绝二次启动`
- ⚠️ `UPnP：未取得端口映射（…M-SEARCH: No route to host (os error 65)）`——launchd×新签名二进制被 macOS 本地网络隐私静默拒（与 Go v0.14.0 迁移日事件同环境问题，tier exits.md 在册）；STUN 双公布不受影响，修复 = 用户在场系统设置授权一次。

**手机重贴 + 全链**（token `hmw1iu50C3Ig…`，218 字符，3 端点 = LAN v4 + 公网 v4 + 公网 v6）：

- 注入流程（方法论）：`aa force-stop me.zhaozhe.tier` → `aa start -a EntryAbility
  -b me.zhaozhe.tier --ps host_token <token>`（只设当前主机不建连；扩展进程缓存
  旧 token 须重启进程）→ UI「全局代理」开关 [1056,779]。
- 核（12:45:04）：`新栈会话已建立（token 端点 3 个…）` → `赛跑结算：胜出 直连
  192.168.3.12:41641` → `路径确立：直连` → `warmup pong: 就绪（判据=wg）` →
  **`attached（数据面已接管 fd=89，L3 直通）`** → `link: via=direct
  ep=192.168.3.12:41641 rtt=24ms（新栈状态快照）` → `RREG 注册刷新`（dev=aca645d3
  中继=false；pub=14ffdc3b = 新后端公钥派生的新 WG 身份，devTag 不变——设备身份
  持久化语义正确）。tunStatusJSON：`"state":"attached"` `"via":"direct"`。
- **v6 说明**：家 LAN 内 LAN-v4 端点赛跑恒胜（设计行为），生产会话 via=direct
  走 192.168.3.12；v6 路径证据 = token 携带 v6 端点 + 出口持续收到手机 v6 源帧
  （`入站新源：[2408:…b07b…]:…（参照点探测，213 字节）`）+ B0-1 同码级
  `--v6-only` 全链实证（INTEROP-CRITERIA「B0-1 手机 v6 直连」节）。
- **L3 真负载**：浏览器 wikipedia 经隧道——vpn-tun RX 0→3,132,010B / TX 2,501→362,582B
  双向增长；核 `stats: fdWriteBytes=3,131,623B fdReadBytes=362,462B`；App 卡片
  `直连, 22 ms · ↑371.1 KB ↓3.2 MB`。（出口侧 `intercept: …（dialok）`/`dns:` 计数行
  为 debug 级——生产无 `--verbose` 不落盘，见 §5 P1-1 注记；Go 生产同写在
  debug.log，级别约定两侧一致。）
- **files**：App 文件管理桥连通（`文件管理：已连接主机 …（经 VPN 通道的文件桥）`
  + `files:flow：第 1 击成功` + HOME 目录列表渲染）；CLI `files download` 10MB
  经隧道 1.07s，**sha256 双侧一致 `fd40a652…`**。
- **term**：新建会话 `[term] state → Connected (gen=1, 距受理 975ms)` + STATE
  stateV2 + surface 快照（快照=1/34 行）；键盘 `touch /tmp/rs-deploy-term-marker`
  + Enter(2054) → **文件真实出现在出口主机**（键入→INPUT 帧→隧道→term 服务→
  PTY→zsh 全链）；`exit`+Enter → `[term] ENDED（code=0 reason=∅）` 正常退出码
  直传 + 长时任务释放。
- **speedtest**：CLI 一轮经生产隧道 `down=53Mbps up=303Mbps`（**下行对账偏差
  0.00%**，66.78MB 双侧一致，509MB/24s）。（App 测速格本会话 uinput 不可达，
  见 §6 异常④；App 路径数字引用 B0-1 同码级实测 ~55MB/s。）

## 2. 阿里云换装（2026-10-05 13:03–13:10，serve+relay 双角色）

**先备份**（留存 `/opt/homeway/backups/`）：

| 件 | 路径 | 校验 |
|---|---|---|
| Go 二进制备份 | `/usr/local/bin/homeway.bak-go-prerust` | sha256 `1171fb6b9fcfcf3f…`（= 换装时在跑的 v0.16.0 Release 工件） |
| Go data state tar | `backups/data-go-prerust-20261005-130312.tar.gz` | 28 条目；key.bin/tokens.jsonl/config.toml 在包内（22 文件全量） |
| served 根 tar | `backups/served-go-prerust-20261005-130312.tar.gz` | 目录为空（227B 空目录归档，如实） |

**部署**：

- 二进制：v0.2.0 linux-amd64（scp 上传——直连 GitHub 下载断流旧坑）→
  `/usr/local/bin/homeway-rs`（sha `521bcb38b23d2f0e…`，static-pie）。
- **新 state** `/opt/homeway/data-rs/config.toml`：`[serve] enabled=true
  listen=41641 files_root="/opt/homeway/served"`；`[relay] enabled=true
  listen=":41741" advertise="123.56.218.212:41741"`。
- 停 Go：`pkill -x homeway` → 循环等进程退净 + 41641/41741 释放 → 启动：
  `cd /opt/homeway && HOME=/opt/homeway/served nohup /usr/local/bin/homeway-rs
  --state /opt/homeway/data-rs >> /opt/homeway/data-rs/cache/unified-stdout.log
  2>&1 &`（**重启机器不自启**——与现役 Go 同口径，重启后重跑此命令）。

**启动判据**（13:03:56–13:04:22）：

- `serve 就绪：wg=:41641 … key=1020c3eae8b3…`（新身份）+ `dns 代答就绪
  （upstream=100.100.2.136, 100.100.2.138…）` + `files 就绪：root=/opt/homeway/served
  (rw)` + term（shell=/bin/bash）+ speedtest 就绪。
- `绑卡：自动挑到 eth0（index=2 up addrs=[172.17.12.106/20]）` + 绑卡看护钉 eth0。
- **`中继就绪：0.0.0.0:41741（token 模式（中继 ID df7664188129）…）`** +
  `中继控制面：TCP 0.0.0.0:41741 就绪（后端拨腿模式可用）`（ss 核对 UDP 41641%
  eth0 + UDP/TCP 41741 同 pid）。
- 公网端点：STUN 观测 `123.56.218.212:41641` → `公网端点：已公布
  [123.56.218.212:41641]`（`IPv6 不可用（无应答），本轮只公布 IPv4`——该机无 v6，
  形态正确；UPnP 在 VPS 无 IGD 属预期）。客户端 token `hmw1ECDD6ui…`（154 字符，
  2 端点）。

**中继互注（Mac 出口 → 阿里云 Rust 中继）**：Mac config 补 `serve.relay =
rl133ZkG…`（新 rl1——新 state 新鉴权密钥 `data-rs/relay/relay.key`，旧 rl1 随 Go
state 留存）→ `launchctl kickstart -k` → 13:07:05：

- Mac 侧：`中继：注册腿开跑（中继 123.56.218.212:41741，token 模式（rl1 凭据））`
  → `中继：注册成功（腿 41641 → 123.56.218.212:41741）—— 客户端可经它到达本机`
  → `中继控制面：中继身份已认证（OK-MAC 通过）` → `中继控制面已连…等待会话重放`
  （serve 就绪 key 指纹 `8aee740b7220` 重启不变——身份稳定）。
- 阿里云侧：`中继：后端 d07c57dd5bde1fa7 注册成功（腿 114.242.60.128:24577）`
  + `中继：后端 … 控制面就绪（…；SESSION 通告启用拨腿模式）`——后端标签与 Mac
  出口身份标签逐字互证。**Rust↔Rust 出口-中继跨公网注册全链成立。**

**手机连阿里云出口全链**（token `hmw1ECDD6ui…` 重贴）：

- 核：`赛跑结算：胜出 直连 123.56.218.212:41641（镜像 1 包，耗时 20ms）` →
  **直连 v4 公网**（LAN 172.17.x 候选未响应属预期——那是 Aliyun 内网地址）→
  `warmup pong: 就绪（判据=wg）` → `attached（数据面已接管 fd=89，L3 直通）` →
  `link: via=direct ep=123.56.218.212:41641 rtt=21–32ms` → RREG（dev=aca645d3，
  pub=b6ac1fb4 = 该后端派生身份）；tunIp=100.64.60.6。
- L3：vpn-tun 双向增长（42KB 起步——浏览器缓存所限）+ **CLI speedtest 经公网
  隧道 `down=85Mbps up=95Mbps`**（278MB/25s，VPS 带宽量级）。

## 3. 清理与保留清单（两台验收过后执行）

**停了什么**：
- Mac Go 统一进程（12:40 `launchctl bootout`，优雅收尾）+ plist 指向
  `~/bin/homeway-rs`；
- 阿里云 Go 统一进程（13:03 `pkill -x homeway`）；
- **活体二进制已删**：`~/bin/homeway`、`/usr/local/bin/homeway`（删前 `cmp` 与
  `.bak-go-prerust` 字节一致）——理由：tier AGENTS/exits 等旧文档仍写着
  `~/bin/homeway serve token` 等命令，残留活体会被未来会话按旧文档误起（旧 state
  起第二出口：端口退让 + UPnP 抢映射，exits.md 在册坑）。

**留着什么（回退件/备份，均未删）**：
- `~/bin/homeway.bak-go-prerust` + `/usr/local/bin/homeway.bak-go-prerust`（二进制回退件）；
- **旧 Go state 原样保留**：Mac `~/.config/homeway`（+ 全量 tar）、阿里云
  `/opt/homeway/data`（+ backups tar）——含旧身份密钥与 token 台账；旧 rl1 也在
  其中。如需手删（腾空间）先确认回退窗口已过；
- 阿里云历史 `.bak-*` 族与 legacy `homewayd*`/`homeway-relay*`（2026-09 旧栈时代）
  未动——非本次部署物，归属用户；
- 本会话测试残留已清：`~/0rs-deploy-check.bin`、`/tmp/0rs-dl-check.bin`、
  `/tmp/homeway-rs-v0.2.0/`、阿里云 `/tmp/hwrs-check/` 等。

## 4. 回退步骤（如需）

**Mac**：
1. `cp ~/bin/homeway.bak-go-prerust ~/bin/homeway`
2. plist 改回：ProgramArguments=`[/Users/zhaozhe/bin/homeway]`、stdout/WorkingDirectory
   改回 `~/.config/homeway`（Go state 原样在位，身份/token 台账不变）
3. `launchctl kickstart -k gui/$(id -u)/me.zhaozhe.homeway-exit`
4. 手机重贴旧 token（`hmw1GiFSJPXi…`，Go state 台账末行 / events.log 可取）

**阿里云**：
1. `pkill -x homeway-rs`（等退净、端口释放）
2. `cp /usr/local/bin/homeway.bak-go-prerust /usr/local/bin/homeway`
3. `cd /opt/homeway && HOME=/opt/homeway/served nohup /usr/local/bin/homeway
   --state /opt/homeway/data >> /opt/homeway/data/cache/unified-stdout.log 2>&1 &`
4. Mac config `serve.relay` 改回旧 rl1（Go data/relay 台账可取）+ kickstart Mac 出口

## 5. 已知注记（非阻塞）

- **P1-1 日志面**：~~Rust 统一进程生产形态 dlogf 不落盘~~ **✅ 已修（B0-2a，v0.2.1 滚动升级）**：
  events.log 2MB×3 + debug.log 8MB×2 双文件轮转落 `<state>/cache/`（`peer: +/-/~`、
  `dns: q=`、`intercept: …（dialok）` 判据族在 debug.log；摘要判据行在 events.log），
  stdout+文件双写维持（launchd 重定向继续可用）。剩余注记：**stdout 重定向件**
  （`cache/exit.log`/`unified-stdout.log`）随双写无轮转——增速 = 摘要率（debug 行不进
  stdout），与 v0.2.0 观察值同量级；缓解 = plist 改指向/周期归档/daemon 侧接管（B0-2）。
  详见 §9。
- **UPnP**：Mac launchd 形态 M-SEARCH 被本地网络隐私拒（新二进制签名身份，同
  Go v0.14.0 事件）；v6 直连 + v4 STUN 公布在位，中继后备已接（本节 §2）。修复
  = 用户在场授权一次。
- **重启后 v4 端口改写（13:07 起）**：Mac 出口重启后路由器把 WG 流改映射到外口
  24577（≠ 监听 41641）⇒ 出口走保守分支 `公网端点：暂不公布 —— STUN 观测到
  114.242.60.128:24577，但外部端口与监听/UPnP 不一致…`（E20 在册分支，行为
  正确）。后果：手机 token 里的公网 v4 `114.242.60.128:41641` 可能失效（离网
  场景赛跑不中即弃）；**公网可达性由 v6 直连 + 阿里云中继兜底**（与 Go 生产
  UPnP 失效期同构）。`public_endpoint.txt` 保留 12:40 双栈两行。
- ~~**relay 端点未进 token（登记 C 批小缺口）**~~ **✅ 已修（B0-2a，v0.2.1 滚动升级）**：
  根因 = 探测失败（暂不公布）形态下 `print_client_token` 整个进程生命周期从未被调
  （非 relay 注册慢）；修法 = Go role.go 首轮探测信号 + 10×1s 兜底重试（探测全关 =
  15s 档）——探测失败形态下也铸出「内网+中继」端点集，台账末行不再滞留在旧版本。
  验证见 §9。
- **CLI 一次性会话副作用**：`files/speedtest --token` 各起一个临时 peer（新设备
  身份），TTL/GC 自愈（cap=32），对手机会话无干扰（设备身份隔离）。
- **来源 113.201.200.103 的探测流**：两出口均持续收到该 IP 的 213B 参照点探测
  形态包（未认证探测不验身份）——疑第二台设备 6HR0226405005435 持旧 token 巡检
  （未证实）；无安全影响（WG 数据面验签），登记观察。

## 6. 异常与处置（执行期实录）

1. **发版前置 CI 双红**（cb64fce）：pin 测试 linux 负例前提不成立 + 内核
   SO_BINDTODEVICE 无特权重设 EPERM（语义已容器实测钉死）→ `b1f1410` 修复；
   term 测试一轮 macos runner flake（本地 20 连绿、复跑绿，登记未再现）。
2. **App 注入不建连**：`aa start --ps host_token` 只设当前主机；扩展进程 HostStore
   缓存旧 token 不重启读到旧值（tier `Connection.ets` 头注在案）——正确流程 =
   force-stop → 注入 → UI 开关。首踩走了一段恢复阶梯弯路（12:41–12:45）。
3. **App files 列表可见性**：列表桥正常（第 1 击成功 + 目录渲染）但 HOME 根条目
   多、UI 滚动未及文件区；以 CLI 下载完成 10MB sha256 对账代证（App 侧浏览路径
   已证）。未深究 UI 滚动（非本次范围）。
4. **App 测速格 uinput 不可达**：主页右下「测速」格多组坐标 tap 无导航（同页
   其它格正常）；以 CLI 测速代证出口侧数字。App 路径测速引用 B0-1/R8 同码级
   真机证据。未查根因（疑浮层/点击区域，属 App 侧）。
5. **dumpLayout 排序坑（工具方法论）**：`ls | tail -1` 按名非按时间排序，读到
   旧 layout 文件一度误判 UI「未连接」——必须 `ls -t | head -1` 且看文件 mtime。
6. **`~/bin/homeway` 换装时在跑的是本地构建**（`0.0.0-dev`，10-03 构建——非
   v0.16.0 Release；备份如实存此字节）。回退即回此件，与「v0.16.0 Release」
   有差；如需 Release 件用 `.bak-v0.16.0.6abe64c`。

## 7. 观察记录（10 分钟，13:11–13:21）

- **Mac**：进程存活、`grep -cE "ERROR|panic|FATAL|装配失败"` = 0；期间仅公网端点
  2 分钟周期探测行（§5 的保守分支重复，稳定无刷屏异常）与零星入站探测新源行；
  launchd 状态 running。
- **阿里云**：进程存活（pid 1544258）、错误计数 0；`中继统计：注册腿 1（累计成功
  2，伪造 0）｜分配腿 0 ｜转发 上 0 / 下 0 包｜丢弃 209`——注册腿稳定（Mac 后端
  持续保活）、丢弃计数为无验证探测包（§5 观察项），无中继会话属预期（手机在直连）。
- **手机**：全程连阿里云出口 `link: via=direct ep=123.56.218.212:41641
  rtt=33ms`，stats 持续双向增长，无 RECOVER 行（无断线）。

## 8. 手机终态（13:21）

切回 Mac 出口（重注入同 token——HostStore 同身份去重替换）：`attached（数据面
已接管 fd=89，L3 直通）` + `link: via=direct ep=192.168.3.12:41641 rtt=12ms` +
tunIp=100.64.210.203 / pub=14ffdc3b（与 12:45 首连完全一致——身份跨切换稳定）。
**两台新 token 均已在手机在位**（HostStore 条目：Mac `hmw1iu50C3Ig…`〔当前〕
+ 阿里云 `hmw1ECDD6ui…`，切换经 App 主机卡「切换」即可）。

## 9. B0-2a 滚动升级 v0.2.1（2026-10-05 15:15–15:24，生产可观测性批）

> 发版 = tag `v0.2.1`（commit `7be330c`，Release run `37276297287` 绿；四目标产物
> + SHA256SUMS 双端校验 OK；`--version` = `homeway-cli v0.2.1` 双端实测）。
> 内容：P1-1 双文件日志体系 + token 兜底重试 + files get/put 契约 + 台账吊销分支
> 告警（dsh r1 整改后，评审记录 `docs/reviews/B0-2a.md`；ci-local 两轮绿）。

**Mac（launchd，15:15–15:17）**：

- 备份在位：`~/bin/homeway-rs.bak-v0.2.0`（sha `edf4f739…` = v0.2.0 装机件字节）+
  state tar `~/homeway-state-backup-pre021-20261005-151552.tar.gz`（13 条目）。
- 换装：`~/bin/homeway-rs` = v0.2.1 darwin-arm64（sha `96b02537…`）→ `launchctl
  kickstart -k` → running（pid 73323）。
- **判据行落盘（本批主目标）**：`cache/debug.log` **新建并即时有内容**（`intercept:
  过境拦截就绪`/`peer 表：设备表就绪`/`UDP 默认路径：DNS:53 可用`/60s 周期 `dns:`
  计数行）；events.log 续写（append 语义，历史 121KB 保留）+ 首轮 token 公告带
  「日志：…events.log（摘要）｜ …debug.log（细节）」落点行。
- **token 兜底重试真网实证**（§5 第二条的修复）：重启后探测仍失败（端口改写
  `暂不公布 —— STUN 观测到 114.242.60.128:24577`）→ **首轮探测结束后 2s** 兜底
  铸出 token：`端点：192.168.3.12:41641（内网）、123.56.218.212:41741（中继）`——
  **台账末行首次含中继端点（Relay: true）**，secret 不变（id=61f1b59e）⇒ 手机
  在用 token 仍有效，无需重贴；要拿带中继兜底端点的新串可 `serve token` 取或
  App 重贴（用户侧按需）。
- **手机自动重连**（未触碰手机）：`peer: + dev=aca645d3 pub=14ffdc3b`（15:17:49，
  devTag/身份与 12:45 首连逐字一致）+ 5min 巡检 `peer: ~ dev=aca645d3 refresh`；
  生产 `dns: q=1 … resp=2`（15:24，CLI dnstest 经生产出口——一次性会话副作用与
  §5 同口径，TTL/GC 自愈）。
- 中继腿：Mac 重启后 15:16:00 `中继：注册成功`；阿里云重启窗口（15:18–15:19）
  控制面两次退避重连后 15:19:05 `中继身份已认证（OK-MAC 通过）`——跨主机滚动
  升级零人工干预。

**阿里云（nohup 双角色，15:18–15:19）**：

- 备份在位：`/usr/local/bin/homeway-rs.bak-v0.2.0`（sha `521bcb38…` = v0.2.0）+
  `/opt/homeway/backups/data-rs-pre021-20261005-151844.tar.gz`。⚠️ 操作实录：首次
  `cp` 撞 `Text file busy`（先 cp 后 pkill 的顺序错——进程退净后重 cp 即好，登记
  为操作注意：**先停进程再换二进制**）。
- 换装：sha `2ea2e025…`（static-pie）→ `pkill -x homeway-rs` → 等退净/端口释放 →
  重跑 §2 同款 nohup 命令行（pid 1548689；UDP 41641@eth0 + 41741 同 pid）。
- 启动形态（Go 同款两轮竞态的正确收敛）：首轮 STUN 无证据（`暂不公布`）→ 兜底
  铸 1 端点 token → 13s 后探测成功 `已公布 [123.56.218.212:41641]` → **端点已
  变化轮走 tokf 流：events.log 有、unified-stdout.log 无**（终端不冒第二串 token
  的生产实证）→ 台账末行 = 2 端点（内网 + 公网）。
- debug.log 新建（60s `dns:` 计数行）；relay.log 续写（轮转 2MB×3 到量才动）。

**回退（如需，v0.2.0 件在位）**：Mac `cp ~/bin/homeway-rs.bak-v0.2.0 ~/bin/homeway-rs
&& launchctl kickstart -k …`；阿里云 pkill 后 `cp /usr/local/bin/homeway-rs.bak-v0.2.0
/usr/local/bin/homeway-rs` + 重跑 nohup 命令行。state 兼容（本轮无布局变更；debug.log
对 v0.2.0 只是无人读的多余文件）。

**剩余注记（登记 B0-2/B0-2b）**：stdout 重定向件（`exit.log`/`unified-stdout.log`）
随双写无轮转——增速 = 摘要率（debug 行不进 stdout），v0.2.1 实测与 v0.2.0 同量级，
长期靠 plist 改指向/周期归档/daemon 侧接管；matrix「非 verbose 起 grep debug.log」
自动门与 print_client_token 锁面收窄挂 B0-2b（`docs/reviews/B0-2a.md`）。
