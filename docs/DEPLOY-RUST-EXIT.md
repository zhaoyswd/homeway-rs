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
  （bind_interface=auto / upnp=true / stun=stun.miwifi.com:3478,stun.hitv.com:3478,stun.chat.bilibili.com:3478,stun.cloudflare.com:3478 / stun6=stun.hitv.com:3478,stun.cloudflare.com:3478 / doh=dns.alidns.com@223.5.5.5,dns.alidns.com@223.6.6.6（2026-10-20 起国内多候选 + DoH 钉卡解析）/
  files_root=$HOME / max_peers=32 / dns_port=5300）。

**启动判据**（12:40:14–12:40:22，`cache/exit.log` = `cache/events.log` 同串）：

- `serve: 按期望态装配（config serve.enabled=true）` → `凭证：…已铸出新凭证（客户端需重新粘贴新 token）` → `凭证台账：1 行记录 / 1 枚在用凭证…`
- `绑卡：自动挑到 en0（index=6 up addrs=[192.168.3.12/24]）` → `绑卡看护：WG socket 钉在 en0（index=6 …）`
- `dns 代答就绪` / `files 就绪：root=/Users/zhaozhe (rw) sock=…files.sock…` / `speedtest 就绪` / `# Serving terminal sessions on …term.sock (shell=/bin/zsh …)`
- `后端身份：标签 d07c57dd5bde1fa7 ｜公钥 8aee740b7220…`（新身份）→ **`serve 就绪：wg=:41641（配置端口…）… key=8aee740b7220…`（端口沿用 41641）**
- **v4/v6 双公布**：`STUN：监听 socket（本地 41641）在 stun.miwifi.com:3478 眼里是 114.242.60.128:41641`（候选首个胜出者） + `公网端点：IPv6 路径可用（STUN 看到 [2408:…deb7:9c81]:41641），公布 …` + `公网端点：已公布 [114.242.60.128:41641 [2408:8207:2518:2550:383e:8734:deb7:9c81]:41641]（写进 public_endpoint.txt…）`
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

**v0.2.2 未发（2026-10-07 登记，防操作侧困惑）**：~~P1 批（出口发送线程）原计划
随 P1c 全绿发 v0.2.2 并滚动两台——终验 T2 未达 15% 收益门 ⇒ 走预登记无收益
分支，默认 off 维持 ⇒ 无生产行为变化 ⇒ **不发版、不滚动**（ROADMAP P1 终档）。~~
**〔已被 §10 覆盖〕**同日用户改用多因子框架终局裁定发送线程默认 on + 简洁化
删除批，v0.2.2 已发并滚动——见 §10；两台生产出口现为 **v0.2.2** 形态。

## 10. 滚动升级 v0.2.2（2026-10-07 12:37–12:41，P1 发送线程默认 on/唯一化 + 简洁化删除批）

> 发版 = tag `v0.2.2`（commit `981e6f5`，Release run `37572093951` 绿；四目标
> 产物 + SHA256SUMS `--check` 全 OK；darwin-arm64 本机与 linux-amd64 阿里云
> `--version` = `homeway-cli v0.2.2` 实测）。内容：发送线程默认 on 并唯一化
> （Queued 唯一路径；启动新判据行「发送线程：就绪」）+ MTU 机器全删（恒 1280，
> tier 侧 tunMtu 注入/mtuEff 白名单同批退役）+ pacing 时刻表删（令牌桶/
> `HOMEWAY_TX_SHAPING`/TX_DBG 保留）+ `HOMEWAY_CC`/`HOMEWAY_UDP_NO_BATCH`
> 清理（裁定记录 = ROADMAP P1 终档「P1 默认开启裁定」+ CHANGELOG v0.2.2 段；
> ci-local quick 全绿 + RRR 矩阵冒烟全过 + 真机烟囱 L3/files/term 全绿）。
> **兼容性预检**：两台 config 均无已删键（`inner_mtu`/`pacing`/`pace_mbps`）
> ——serve_cli `deny_unknown_fields` 不会拒启（滚动前已核）。

**Mac（launchd，12:37:54）**：

- 备份在位：`~/bin/homeway-rs.bak-v0.2.1`（sha `96b02537…` = v0.2.1 装机件字节）。
- 换装：`~/bin/homeway-rs` = v0.2.2 darwin-arm64（sha `7b65d49e…`，**临时件 +
  `mv` 原子换名**——不撞运行中二进制的写锁）→ `launchctl kickstart -k` →
  running（pid 55950）。
- 判据行（exit.log/debug.log，12:37:54–59）：`serve 就绪：wg=:41641
  tunnel=100.64.255.1 … tokens=27`；**`发送线程：就绪（homeway-serve-tx，单轮
  排空上界 256KiB）`（本批新行，debug.log 面）**；`绑卡：自动挑到 en0` +
  `绑卡看护：WG socket 钉在 en0`；v6 双公布 `已公布 [114.242.60.128:41641
  [2408:…]:41641]`；`中继：注册成功（腿 41641 → 123.56.218.212:41741）` +
  `中继控制面已连`。5s 观测行 `UDP 出站[发送线程]`（mode 判别已删——恒发送
  线程面）。
- **手机自动重连**（未触碰手机）：出口重启窗口手机走 R1 重握手 → R2 换源 →
  R3 重赛跑，`RECOVER 恢复于 R3 重赛跑（… 耗时 18.495s）` → attached、link
  via=direct。L3 一轮：browser 流量出口侧 `intercept: tcp transit …（dialok）`
  成串 + 手机 `stats:` fdRead 48100→186432B / fdWrite 116096→3633288B 双向增长。

**阿里云（nohup 双角色，12:40:31）**：

- 备份在位：`/usr/local/bin/homeway-rs.bak-v0.2.1`（sha `2ea2e025…`）。
  ⚠️ 操作实录：首次换件拿错目标产物（linux-**arm64**）→ `Exec format error`
  ——**catzhao.com 是 x86_64**（`uname -m`；v0.2.1 件 = x86-64 static-pie），
  换 linux-**amd64** 即好；进程先停净再 cp（§9 的 Text file busy 教训未重犯）。
- 换装：sha `d0f51bff…`（static-pie）→ 重跑 §2 同款 nohup 命令行（pid 1565023；
  UDP 41641@eth0 + 41741 同 pid）。启动两轮竞态同 §9 形态（STUN 先「不可用/
  无应答」→ `已公布 [123.56.218.212:41641]`，v6 无 = 该机无 v6，预期）。
- 判据行（unified-stdout.log/debug.log，12:40:31）：`统一进程就绪（… serve=true
  relay=true … version=v0.2.2）`；`serve 就绪：wg=:41641 … tokens=4`；**
  `发送线程：就绪（homeway-serve-tx，单轮排空上界 256KiB）`（debug.log 面）**；
  `绑卡看护：WG socket 钉在 eth0`；`relay: 端点：123.56.218.212:41741`。
- **中继互注（v0.2.2↔v0.2.2）**：Mac 重启（12:37:54）后 relay.log
  `中继：后端 d07c57dd5bde1fa7 注册成功（腿 114.242.60.128:41641）`——两侧
  同版互注零人工干预。

**手机侧（12:29 起，先于本批滚动）**：v0.2.2 批核（.so = cbd45f0，pin 前进）
覆盖装机：`身份：复用` / `warmup pong: 就绪（判据=wg）` / `attached（数据面已
接管 fd=89，L3 直通）` / `running (mtu=1280 …)`（**tunStatusJSON 无 mtuEff 键
——删除面行为一致**）；files 两轮（home 列表 + `~/.cargo` 层级+文件元数据过
隧道）+ term 一轮（`Connect … (fresh attach)` → `Session state -> Connected`，
term-1Surface 帧在渲）。

**回退（如需，v0.2.1 件在位）**：Mac `cp ~/bin/homeway-rs.bak-v0.2.1 ~/bin/homeway-rs
&& launchctl kickstart -k …`；阿里云 pkill 等退净后 `cp …/homeway-rs.bak-v0.2.1
/usr/local/bin/homeway-rs` + 重跑 nohup 命令行。state 兼容（无布局变更；已删
config 键本就未配置）。**注意回退后 tier 侧核（v0.2.2 形态）与出口 v0.2.1 的
组合**：wire 面无差异（删除批不动协议），此前双版混跑形态（12:29–12:37 手机
v0.2.2 核 × Mac v0.2.1 出口）实测四判据全绿——回退出口不需要回滚手机。

## 11. 滚动升级 v0.2.3（2026-10-07 16:26–16:31，拦截层单线程 reactor 简化批）

> 发版 = tag `v0.2.3`（commit `da95496`，Release run `37593598650` 绿；四目标产物
> + SHA256SUMS `--check` 全 OK；darwin-arm64 本机与 linux-amd64 阿里云 `--version`
> = `homeway-cli v0.2.3` 实测）。内容：出口拦截层重拨 OS socket 从 8-worker 池收进
> **单线程 reactor**（pool.rs 全删 619 行；每流 ReactorIo/非阻塞拨号三分类/背压清账
> 通道删除；协议语义零改动——判据行/Stats 同串）。两道 dsh 评审门（设计 16 条 + 代码
> 11 条，代码门 1 高：flush_out EAGAIN 自旋——修复含单元钉反向验证闭环）；记录 =
> `docs/reviews/reactor.md`，设计 = `docs/reviews/reactor-design.md` v2.1。
> **兼容性**：零 config 变更（无新删键）；state 无布局变更；wire 面零差异——手机
> （v0.2.2 核）与出口 v0.2.3 混跑无需动手机。

**Mac（launchd，16:26–16:28）**：

- 备份在位：`~/bin/homeway-rs.bak-v0.2.2`（sha `7b65d49e…` = v0.2.2 装机件字节）。
- 换装：`~/bin/homeway-rs` = v0.2.3 darwin-arm64（sha `98f337e5…`，临时件 + `mv`
  原子换名）→ `launchctl kickstart -k` → running。
- 判据行（16:28:27–28:59）：`serve 就绪` + `intercept: 过境拦截就绪（隧道IP
  100.64.255.1；豁免=转投本机同端口；TCP 并发上限 1024）`（E5 同串）+ `发送线程：
  就绪（homeway-serve-tx，单轮排空上界 256KiB）` + 绑卡 en0 + v4/v6 双公布 +
  `中继：注册成功`。**本批新 verbose 观测行**：`intercept: reactor 观测
  pump=819/5s（均周期 6.11ms）名下fd峰=1 单拍峰=196µs`（idle 形态：低拍频 5ms 档、
  单拍 wall ≤196µs——低-4 的 fds>0 门控下静默期不打，有流才出）。
- **手机自动重连**（未触碰手机）：`peer: + dev=aca645d3 pub=14ffdc3b
  ip=100.64.119.141`（16:30:37，重启窗口后自动恢复）；手机核自连探测 :1 走 reactor
  拨号失败路径——`intercept: tcp exempt 100.64.255.1:1 ← …:33731 拨号失败：连接失败`
  （降噪形态行同串，非阻塞 connect→RST 真机在跑）。

**阿里云（nohup 双角色，16:29–16:31）**：

- 备份在位：`/usr/local/bin/homeway-rs.bak-v0.2.2`（sha `d0f51bff…`）。流程照 §10
  教训：**先 pkill 等退净（宽限 drain 收尾 ~数秒）再 cp**（首cp 撞 Text file busy——
  二次等净后成功）；产物 = linux-**amd64**（sha `2dabce88…`）。
- 换装后重跑 nohup 命令行（pid 1567909）：`统一进程就绪（… serve=true relay=true
  … version=v0.2.3）`（16:30:34）+ `serve 就绪：wg=:41641 … tokens=4` + 绑卡 eth0 +
  STUN 公布 `123.56.218.212:41641`（UPnP 无 IGD = 云主机常态）。
- **中继互注（v0.2.3↔v0.2.3）**：阿里云 relay.log `中继：后端 d07c57dd5bde1fa7
  注册成功（腿 114.242.60.128:41641）`（16:30:40）+ Mac 侧 `中继：注册成功` ——
  同刻互注零人工。

**观察项**：滚动时刻手机闲置（无人使用），E10 dialok 行待手机下次自然 transit 流量
时出现（语义等价已由 19 单测 + harness 三臂 + RRR 矩阵 + 评审判据行机械比对背书；
隧道健康面已验：peer 注册/refresh/keepalive + 探测拨号失败路径 + reactor 观测行）。

**回退（如需，v0.2.2 件在位）**：Mac `cp ~/bin/homeway-rs.bak-v0.2.2 ~/bin/homeway-rs
&& launchctl kickstart -k …`；阿里云 pkill 等退净后 `cp …/homeway-rs.bak-v0.2.2
/usr/local/bin/homeway-rs` + 重跑 nohup 命令行。state/config 兼容（零变更）；
wire 面零差异——回退出口不需要动手机。

## 12. 滚动升级 v0.3.0（QUIC 单承载，**待执行**——U1/U2/U3 用户触点）

> **本节 = 执行手册 + 实录位**。设计真源 = `docs/reviews/M7-design.md`（§4 切换方案 / §5 失败预案 /
> §6 上线前核验清单 / §7 tier 交付物）；本节只写**执行面**并给出与设计不同的**实测口径订正**。
> **历史节 §0–§11 一字不动**（v0.2.x 实录保留）；**v0.3.0 是破坏性换代**（token `hmw1`→`hmw2`、
> WG 面整件退役），**故本节不是「换个二进制」而是三阶段代际切换**。
>
> **三阶段顺序（不变式 I：设备须始终有一条「核与出口同代」路径）**：
> **U1 阿里云出口 → G2（设备零扰动）→ U2 设备核 → G2（+ 贴阿里云新 token）→ U3 Mac 出口 → G2
> （+ 贴 Mac 新 token）**。⇒ **核升级必须夹在两次出口升级之间**。

### 12.1 代际矩阵（四格；交叉两格**硬失败**）

| 核 \ 出口 | **G1 出口**（≤ v0.2.3） | **G2 出口**（≥ v0.3.0） |
|---|---|---|
| **G1 核**（pin ≤ `cbd45f0`，WG 核） | ✅ 全通（v0.2.x 生产态） | ❌ 硬失败（旧核把 `Quic` 端点当直连发 WG 握手 + 出口已无 WG 面） |
| **G2 核**（≥ v0.3.0，QUIC 岛） | ❌ 硬失败（`UnsupportedVersion`：存量 token 一律失效） | ✅ 全通 |

**回滚的最小单元 = 设备核 + 至少一台出口 + 至少一枚同代 token**（三者必须同代）。

### 12.2 共同前置（A 步之前一次做完；**缺任一件不做切换**）

| # | 件 | Mac（本次实施棒已在位） | 阿里云（U1 现场做） |
|---|---|---|---|
| 1 | 现役二进制 | `~/bin/homeway-rs.bak-v0.2.3`（sha `98f337e5…` 逐字节同）+ 副本 `~/homeway-rollback/homeway-rs.bak-v0.2.3` | `cp /usr/local/bin/homeway-rs /usr/local/bin/homeway-rs.bak-v0.2.3`（sha 期望 `2dabce88…`） |
| 2 | `config.toml` | `~/homeway-rollback/config.toml.bak-pre-m7`（`diff` 与现役件同） | `cp …/config.toml …/config.toml.bak-pre-m7` |
| 3 | plist | `~/homeway-rollback/me.zhaozhe.homeway-exit.plist.bak-pre-m7`（`plutil -lint` OK） | —（nohup 形态无 plist） |
| 4 | 两枚旧 `hmw1` token | `~/homeway-rollback/tokens-mac-pre-m7.txt`（0600；端点清单见同目录 `tok-endpoints.txt`） | U1 现场 `serve token` 抄存（**升级后读不到旧串**：台账 append-only + 读末行） |
| 5 | G1 核 `.so` | `~/homeway-rollback/libclientcore-g1.so`（2,348,864 B，sha `6a183e86…`）+ `homeway-cli-g1-macos`（sha `3008074a…`，`/tmp/m6-ab` 易失面已转存） | — |
| 6 | 旧 HSP 重建配方 | §12.5（本节） | — |
| 7 | state 全量 tar | `~/homeway-rollback/state-mac-pre-m7.tar.gz`（3 条目：`config.toml` + `serve/key.bin` + `serve/tokens.jsonl`；sha `fd8b55f9…`；**Mac relay 角色停用 ⇒ 无 `relay/relay.key`**，与设计 §4.0-C-7 的「若有」一致——已核 `relay/` 目录为空） | `tar czf /opt/homeway/backups/data-rs-pre-m7.tar.gz -C /opt/homeway/data-rs config.toml serve/key.bin serve/tokens.jsonl relay/relay.key`（阿里云 relay 双角色 ⇒ **有** `relay.key`） |

**配置面（两台都要）：显式 `quic_listen = 41641`**

```toml
[serve]
enabled = true
listen = 41641
quic_listen = 41641     # ← 新增行（缺省 = listen+1 = 41642 ⇒ 端口/防火墙/NAT/文档口径全换号）
relay = "rl1…"
```

**回滚的配置面陷阱**：`quic_listen` 是 M1 新键，旧二进制 v0.2.3 的 `serve` 表是
`deny_unknown_fields`（`serve_cli.rs`）⇒ **回滚 = 三件套「换二进制 + 还原 config（删该键）+ 启动」**，
不许只换件。

**台账跨代可读性（本实施棒已实测，§4.0-E 的可复跑校验）**：新出口会把 `hmw2` 行 append 进
`serve/tokens.jsonl`；**旧 v0.2.3 二进制能读混合台账**——①`serve token` 探针：33 行 `hmw1` +
1 行真 `hmw2`（带 `rpk` + `"Quic":true` 端点）⇒ **rc=0**，按旧格式重编码打印 `hmw1…`；
②**整体启动**：同款混合台账起旧出口 ⇒ `serve 就绪：wg=:41681（…）tokens=34 key=8aee740b7220…`，
零 `BadTokenLine`。**探针命令（设计 §4.0-E 的路径形态需订正）**：

```bash
# ⚠️ 设计 §4.0-E 原文 `cp -a <state>/serve /tmp/rollback-probe` 会把 serve 的内容平铺进探针目录，
#    二进制找的是 <state>/serve ⇒ 报「台账为空」。正确形态（两层）：
rm -rf /tmp/rollback-probe && mkdir -p /tmp/rollback-probe
cp -a ~/.config/homeway-rs/serve /tmp/rollback-probe/     # ⇒ /tmp/rollback-probe/serve/*
~/bin/homeway-rs.bak-v0.2.3 serve token --state /tmp/rollback-probe   # 期望 rc=0 且打印 hmw1…
```

### 12.3 取 token 的命令形态（**实测订正**：设计 §4.1/§4.2/§4.3 的 `sed` 管道在守护运行态取空）

实测（2026-10-10，本机现役 v0.2.3 出口在跑）：`serve token --state <dir>` **两态输出不同**：

| 形态 | stdout | stderr |
|---|---|---|
| **守护/统一进程在跑**（控制面可达：Mac 生产态、阿里云统一进程） | **裸 token 一行**（**无** `serve token：` 前缀） | `（已连 control.sock：serverVersion=… generation=… seq=…）` + `（端点：…；来源=ledger）` |
| 守护不在跑（回落台账直读） | `serve token：<token>` + 来源 + 端点（三行中文） | — |

⇒ 设计里的 `sed -n 's/^serve token：//p'` 在**运行态取空串**；配合设计的 `case "$TOK" in hmw2*)`
fail-closed 断言只会「抓取失败 ⇒ 停」（**不会误用坏串**，这是 fail-closed 生效而非静默错），
但会让 U1/U2/U3 卡在第一步。**两态通吃的取法（本节口径）**：

```bash
TOK=$(/usr/local/bin/homeway-rs serve token --state /opt/homeway/data-rs 2>/dev/null \
      | sed -n 's/^serve token：//p;/^hmw[0-9]/p' | head -1)
case "$TOK" in hmw2*) ;; *) echo "token 抓取失败（拿到：${TOK:0:8}…）——停"; exit 1;; esac
```

**凭证卫生**：`set +o history`（或注入后 `history -D`）；token 只经 shell 变量传递，**不落任何
报告/commit/评审原文**（只留前缀 + 尾 4）；不从 `exit.log`/`unified-stdout.log`（无轮转）抄 token。

### 12.4 三阶段执行（判据行 = `docs/reviews/M7-design.md` §4.1–§4.3 的 V1–V24）

**U1 阿里云出口 → G2**（判据 V1–V9）：**取 token 一律用 §12.3 的两态通吃形态**；
`pkill -x homeway-rs` 等退净（`Text file busy` 教训，§9/§10）
→ 上传 `homeway-cli-v0.3.0-linux-amd64.tar.gz` + `sha256sum -c SHA256SUMS` + `--version` = `v0.3.0`
→ 加 `quic_listen` → nohup 重起 → 逐条对 V1（**端口未退让**：`cache/quic_listen_port.txt == 41641`
且 `grep -c '被占用' <日志> == 0`；E1 打的是配置值，**不能**用它判退让）/ V3（身份连续：`key=` 与
`后端身份：` 标签机械 diff 逐字同）/ V4（relay 角色行 **现行 = `[::]:41741` 双栈**）/ V7（新 token `hmw2…`）/
V8（中继互注零人工）。

> **「中继就绪」样件的现行串（L-9 的更新说明）**：§2 的实录样件是 **v0.2.x 单栈形态**
> （`中继就绪：0.0.0.0:41741（…）` 形态，`:122` 附近）；**M5 起 = 双栈**：
> `中继就绪：[::]:41741（token 模式（中继 ID …）；…）`（v4 映射地址仍可连）。历史正文不动，本节给现行串。

**U2 设备核 → G2**（判据 V10–V20，含 T11 真机档 V19 / DNS 真机轮 V20）：**取 token 用 §12.3**；
另**补一条观察项**（代码门 r31 中-6）：**App Pss 读数**（`hidumper --mem <vpn_pid>`，与 M6 基线
33,823 KB 对照；超 +3 MiB 回看 M6.7 显式 socket 缓冲条）——它不在设计 V10–V24 清单里，但为
「+3 MiB 设定值入账」的产品侧实测面。tier 侧 pin 前进 +
出包（正式路径被 `log-index` 门拦时走手拷逃生口，须留痕）→ `force-stop` 后注入**阿里云**新 token
（`--ps host_token`）→ 屏上点 VPN 授权 + 开「全局代理」→ 逐条对 V10–V18 + V19/V20 + V13b。

**U3 Mac 出口 → G2**（判据 V21–V24 + 观察期）：**取 token 用 §12.3**；临时件 + `mv` 原子换名 + `launchctl kickstart -k`
（`KeepAlive=true` ⇒ **不要**用 `bootout`/`bootstrap`，失败会 crash-loop）→ 逐条对 V21–V24。

### 12.5 回滚（按「回退手段序」从小到大）

> **纪律（代码门 r31 中-2 整改，两处都是实测教训）**：①**Mac（launchd）**：`cp` 覆盖**运行中**
> 的二进制在 Darwin/APFS 上会**直接把进程杀掉**（实测 `rc=0` + 进程 `Killed: 9`），配合
> `KeepAlive=true` ⇒ launchd 立刻用**尚未还原的 config**（仍含 `quic_listen`）拉起旧二进制 ⇒
> `deny_unknown_fields` 拒启 ⇒ **crash-loop**。故 Mac 一律「**先还原 config → `install` 临时件 + `mv -f` 原子换名 → `kickstart -k`**」。
> ②**阿里云**：Linux 的 ETXTBSY（`Text file busy`，§9/§10 三次实录）要求 **先 `pkill -x homeway-rs` 等退净再换件**。

1. **设备切另一台 G2 出口**（一次粘贴；零出口动作、零核回退）——首选；
2. **单台出口回退 + 同批核回退**（Mac）：
   ```bash
   cp ~/.config/homeway-rs/config.toml.bak-pre-m7 ~/.config/homeway-rs/config.toml   # ① 先还原 config（删 quic_listen）
   install -m 0755 ~/homeway-rollback/homeway-rs.bak-v0.2.3 ~/bin/homeway-rs.new      # ② 临时件
   mv -f ~/bin/homeway-rs.new ~/bin/homeway-rs                                        # ③ 原子换名（不撞运行中进程）
   launchctl kickstart -k gui/$(id -u)/me.zhaozhe.homeway-exit                        # ④ 重启（KeepAlive 形态）
   ```
   + 设备装回 G1 核 HSP（`~/homeway-rollback/libclientcore-g1.so` 两处手拷 → 重建 HSP → 覆盖装）
   + 重注入旧 `hmw1` token（`~/homeway-rollback/tokens-mac-pre-m7.txt`）；
3. **双台出口回退 + 核回退**：② + 阿里云同款三件套（**顺序照 §4.1**，不许压缩成一条）：
   ```bash
   pkill -x homeway-rs; until ! pgrep -x homeway-rs >/dev/null; do sleep 1; done   # ① 先停净（ETXTBSY 教训）
   cp /usr/local/bin/homeway-rs.bak-v0.2.3 /usr/local/bin/homeway-rs                # ② 换件
   cp /opt/homeway/data-rs/config.toml.bak-pre-m7 /opt/homeway/data-rs/config.toml  # ③ 还原 config（删 quic_listen）
   cd /opt/homeway && HOME=/opt/homeway/served nohup /usr/local/bin/homeway-rs \
     --state /opt/homeway/data-rs >> /opt/homeway/data-rs/cache/unified-stdout.log 2>&1 &
   ```

**旧 HSP 重建配方（第 6 件）**：`libclientcore-g1.so`（2,348,864 B）拷到 tier
`tailcat/libs/arm64-v8a/` 与 `tailcat/src/main/cpp/prebuilt/arm64-v8a/` 两处（两处 md5 一致留证）
→ `hvigorw --mode module -p module=tailcat@default … assembleHsp` → `hdc install -r` 覆盖装。

### 12.6 执行实录（U1/U2/U3 现场填写）

> **执行版本订正（2026-10-11）**：本节判据行的**实际执行版本 = v0.3.1**（不是 v0.3.0）。原因 =
> 首发产物在 U3 现场暴露一个**部署阻塞级缺陷**（出口面双栈 socket 的裸 v4 发送在 Darwin 报
> `EINVAL(22)` ⇒ STUN 观测恒失败 ⇒ **公网端点不公布**）；热修后以 v0.3.1 重发四目标再执行。
> 热修的根因/修法/门记录/证据 = `docs/reviews/M7-HOTFIX-fam.md`。v0.3.0 **未承载任何生产流量**：
> Mac 上只活了 6 分钟（且该窗口内无客户端连入），阿里云与设备**从未**装过 v0.3.0。

| 阶段 | 时刻 | 版本/身份读数 | 判据行 | 结论 |
|---|---|---|---|---|
| 前置（M7 实施棒，2026-10-10） | — | 七件在位（sha 见 §12.2）+ 台账跨代可读 rc=0 ×2 | — | 就绪 |
| **Mac 首次换装（v0.3.0）** | 2026-10-10 23:28 | `v0.3.0`；身份 `key=8aee740b7220…` 逐字不变；token 已转 `hmw2` | V21 部分 / **V22 失败** | **发现缺陷**：`公共端口明文发送失败（→ 162.159.207.0:3478，20B：EINVAL 22）` ⇒ 公网端点不公布 ⇒ 停在 U3、转热修 |
| **U1 阿里云** | 2026-10-11 00:16 | `v0.3.1`（sha `1bfb4c04…`）；`后端身份：标签 1d4ebb7e3fa93dbd ｜公钥 1020c3eae8b3…` **逐字不变**；`serve 就绪：quic=:41641 … tokens=4 key=1020c3eae8b3…`；台账 4→5 行 | V1 ✓（`quic_listen_port.txt=41641`，无退让）V2 ✓ V3 ✓ V4 ✓（`中继就绪：[::]:41741`）V7 ✓（`hmw2…` 214 字符；端点 = 内网 `172.17.12.106:41641` + 公网 `123.56.218.212:41641`，**不含中继腿**——同机双角色，腿无意义）V8 ✓ | **通过**（`公网端点：已公布 [123.56.218.212:41641]`） |
| **U3 Mac** | 2026-10-11 00:22 | `v0.3.1`（sha `314082fd…`）；`serve 就绪：quic=:41641 … tokens=38 key=8aee740b7220…`；`后端身份：标签 d07c57dd5bde1fa7 ｜公钥 8aee740b7220…` **逐字不变**（`tokens` 由 37 起步 = 装配时刻行数、新 token 的 append 在其后 ⇒ 与 §12.4 注记一致） | V21 ✓（token **四端点齐**：内网 + 公网 v4 + 公网 v6 + 中继）V22 ✓（绑卡 en0 + `绑卡看护：QUIC 端口 socket 钉在 en0`；**v4 与 v6 公网端点均已公布**）V23 ⧗（待设备侧）**V24 ✓**（00:52:49 收：进程时长 30 m 35 s，`ERROR\|panic\|FATAL` = **0**，中继腿注册 **1** 次无抖动，公网端点每轮稳定复公布） | **通过**（热修生效：`STUN：… 眼里是 114.242.60.128:41641`） |
| **U2 设备核** | 2026-10-11 00:24 装机 / **11:06–11:20 真机验收** | 新核 `.so` = **2,995,376 B**（标记 `77068d72c938-rust`）；签名 HSP `hdc install -r` 成功；`--ps host_token` 注入两台出口的 hmw2 token（阿里云先、Mac 后 ⇒ Mac 为当前） | **V10–V15 / V17（设备侧）/ V18 / V20 全过**；V16 过（待机档）；V19 未做；**App Pss 34,164 KB**（M6 基线 33,823 ⇒ +341 KB，判据 +3 MiB 内） | **通过**（详见下方真机实录） |

**U2 真机实录（2026-10-11，hdc 驱动设备 `FMR0224116011480`，出口 = Mac）**

| 判据 | 实测原文（设备侧 `tier-core` / 出口侧） | 结论 |
|---|---|---|
| V10 代际 | `新栈会话已建立（token 端点 4 个，后端隧道地址 100.64.119.141）` | ✓ |
| V11 岛建连 | 同上 + `link: via=direct ep=114.242.60.128:41641 rtt=25ms`（**措辞与设计 V11 的 `岛已建连（…）` 不同**，见下方订正） | ✓ |
| V12 准入保活 | `warmup pong: 就绪（判据=quic）` → `attached（数据面已接管 fd=91，L3 直通）` → `link: via=direct ep=… rtt=25→23→19→17ms` | ✓ |
| V13 出口采纳 | 出口 `连接采纳 dev=aca645d3 tun=100.64.210.203 ← [::ffff:114.242.60.128]:55043`（**设计里的 `peer: + …` 行是 WG 时代措辞**，现行设备表行为 `peer 表：设备表就绪（cap=32…）` + `serve status` 的 `peers：N`） | ✓ |
| V14 层 0 | 出口 `transit 198.18.x.x:443 ← 100.64.210.203:50902（dialok）` 成串（9 条）+ `intercept：dialOk=9 dialFail=0` | ✓ |
| V15① files | UI 列表读到 Mac 家目录全套；深链→`m7-probe.txt` 预览显示内容 `homeway-rs V15 样本 …`（与 Mac 文件逐字一致） | ✓ |
| V15② term | App 终端 attach 到 Mac 的 shell（`zhaozhe@zhaozhedMac-mini-6 ~ %`）；手机键入 `touch /tmp/v15-term-marker && echo V15TERM` ⇒ **Mac 上出现该 marker**；`cat ~/m7-probe.txt` 的回显在手机上正确渲染（截图存档） | ✓ |
| V15③ speedtest | 一轮 `↑56MB/s ↓62MB/s`（服务流 `id=1 tag=speedtest ↑65B ↓181180274B`） | ✓ |
| V15④ portfwd | 手机内建映射 `127.0.0.1:9080 → 主机:9000`（状态 `监听中`）；Mac 侧 `nc -l 127.0.0.1:9000` 经 **Mac→hdc fport→手机监听器→隧道→Mac** 收到载荷 `V15-PORTFWD-PAYLOAD-OK` 逐字一致 | ✓ |
| V16 断线恢复 | 三轮 `launchctl kickstart -k`（出口重启）：**检测 ≈36–42 s**（QUIC idle 30 s + keep-alive 10 s，空闲/轻载隧道）→ `连接已断 —— 交快探阶梯（M/R/B；§3.1）保世代重连` → `链路动作选 R` → `链路重连完成（耗时 6–8 ms）`（墙钟 ~4 s）；同隧道 IP `100.64.210.203` 无世代重建，重连后自动改走内网端点 | ✓（**待机档**；在用档 3.5 s 未能从 UI 触发，见订正） |
| V17 收工链 | 设备侧 `链路巡检退出（隧道已停止）` → `收到停止信号，正在回收（client/stack）` → `port-forward: 已停止全部监听器（1 个）` → `隧道桥: 已停止监听` → **`岛收工（连接面随端点关闭）`**；`vpn-tun` 网卡与状态栏 VPN 图标同时消失 | ✓ |
| V18 零 panic | hilog / `/data/log/faultlog/faultlogger` / App 日志页「错误」筛选 三面 **0 命中** | ✓ |
| V20 DNS | 出口 `dns: q=14 qtcp=0 resp=8 filter=7 trunc=0 fallback=0 fail=0 drop=0 malformed…` | ✓ |
| V19 路径变更 | **未做**（需本地中继做腿 + `pkill` 打死腿；设计标注属 S2b，本次未起本地中继） | 未取 |

**判据措辞订正（两处，实测口径）**

1. **V17 的出口侧期望行错位**：设计写「出口 `出口收线（连接数 1 → 0）`（E25）」，实测该行是**出口进程收工行**
   （`crates/homeway-quic/src/exit/mod.rs:1152`，在 `endpoint.close(b"exit stopping")` 之后打）——**客户端断开时出口按设计静默回收**（`conns.retain` 摘除死连接、不落行；漫游设备频繁断连，落行即刷屏）。
   ⇒ V17 出口侧应以「设备侧 `岛收工` + 隧道/图标消失」为判据；E25 只用于出口自身重启（U1/U3）的时点观测。
2. **V11/V13 的期望串是 WG 时代措辞**：现行核打 `新栈会话已建立（token 端点 N 个，后端隧道地址 …）`（无 `岛已建连（候选 N 个，胜出 …）` 那行），设备表行为 `peer 表：设备表就绪（…）` 且 `serve status` 有 `peers：N` 段。
   ⇒ 判据行按本表实测串对齐（不影响语义）。

**新发现（未修，登记）**：出口侧三次 `源校验拒 src=非 IPv4（76B/116B） ∉ {tun_ip=…}`——设备把**非 IPv4 内层包**送进了隧道（App 侧只配 `0.0.0.0/0`，疑为 OHOS 把 v6/ICMPv6 也交付给 VPN 面），出口按「隧道是 v4」丢弃并如实计数。危害面小（计数 3、会话不受影响），但属「v6 流量进隧道被丢」这一形态，与已登记的 IPv4 分片/ICMP 一族同族，**未立批**。


**U2 未完项与原因（如实登记）**：设备侧 VPN 需**屏上手点**（授权 + 连接 + 开「全局代理」）。
App 的免手工外部通道只有 `--ps host_token`（主机注入）与 `--ps openPage`（开页）两条，
**都不能代点 VPN 开关**（本表上方的真机实录即由 **hdc 驱动**完成：`uitest uiInput` 点按 + `dumpLayout` 读判据 +
`screenCap` 截图取证 + `aa start --ps` 深链；用户 2026-10-11 指令「直接用这台电脑连接的手机来进行测试」）。
**验收时设备状态**：VPN 保持连接（验收结束后未关），当前主机 = Mac，`quic:192.168.3.12` 直连 rtt 17–23 ms。
测试残留已清理（端口映射、`hdc fport`、Mac 侧 `nc` 监听与样本文件均已撤）。

**出包路径留痕（逃生口）**：`tools/tailcat/build-core.sh` 的正式路径在最后一道门
`gen-log-index.sh --check` 处**拦下**（`docs/agents/log-index.md` 陈旧——差异主体是 QUIC 换代的
**行号漂移**，153 insertions / 138 deletions）。该门属 **tier 侧文档**（本进程按隔离条款不动 tier
的跟踪文件；tier 侧触点已登记在 `docs/QUIC-ROADMAP.md`「待用户拍板」③）⇒ 按 §12.4 的**手拷
逃生口**出包：`target/aarch64-unknown-linux-ohos/release/libclientcore.so`（2,995,376 B，
`md5 5a821cfb…`）拷到 tier `tailcat/libs/arm64-v8a/` 与 `tailcat/src/main/cpp/prebuilt/arm64-v8a/`
两处 → `assembleHsp` → `hdc install -r`。**两道人工校验已做**：① 两侧 md5 一致；② HSP 内
`libs/arm64-v8a/libclientcore.so` 尺寸 = 2,995,376 B。手拷前核构建的其余门全绿——pin 后代放行
（`77068d72c938` ⊃ `cbd45f04…`）、脏检出闸通过、`[ver] 77068d72c938-rust` 整串钉定、
`[sym] 20/20` 导出面、NAPI 同步门 `不一致 0 条`。

**顺带订正（本节判据可复跑）**：`grep -c '被占用' <日志> == 0` **判不出退让**——就绪行文案本身含
「（配置端口；被占用会自动退让）」（V1 现场实测计数 5，全来自就绪行）。**端口未退让的唯一判据 =
`cache/quic_listen_port.txt == 配置值`**（两台实测均为 41641）。

**token 注入面的一处**UI 侧**限制（新发现，tier 触点）**：App 的 `entry/src/main/ets/model/TokenCheck.ets`
把 `HMW_PREFIX` 硬编码为 `'hmw1'` ⇒ **手工粘贴 hmw2 token 会被前端校验拒**（「token 应以 hmw1
开头」）。`--ps host_token` 注入路径**不经过**该校验（直接进核解析 + HostStore）⇒ 本批部署不受
它阻塞，但「在 App 里手工粘贴新 token」这条路**目前不可用**，需 tier 侧放宽（`hmw1|hmw2`）。
**（2026-10-11 已由 tier 侧修掉：`HMW_PREFIX='hmw2'` 只认当前代 + 错误文案给自救指引 + 脱敏正则
改版本位通配 `hmw[0-9]`，见 §12.7。）**

### 12.7 二次滚动升级 v0.3.2（核侧审查修复批；2026-10-11）

**触发**：核侧「审查修复①–⑦ + 评审 r1 整改」批收口（工作树干净、CI 绿、19 分钟无新提交）⇒ 按
§12.4 的同一条路径把两台出口与设备核一并换到该版本（无 config 变更：`quic_listen` 已在位）。

| 阶段 | 版本读数 | 判据 | 结论 |
|---|---|---|---|
| 发版 | `v0.3.2` 四目标 + Release（tag = main HEAD `75ee175`） | 发版 CI 3m24s 绿 | ✓ |
| 前置 | 阿里云 `homeway-rs.bak-v0.3.1`（sha `1bfb4c04` 逐字节同）+ `config.toml.bak-v0.3.1` + `data-rs-pre-v032.tar.gz`（3 条目）；Mac `~/homeway-rollback/homeway-rs.v0.3.1`（sha `314082fd`）+ `config.toml.v0.3.1` | 两处 sha 与 v0.3.1 产物一致 | ✓ |
| **U1 阿里云** | `v0.3.2`（sha `ff4e2904…`）；`serve 就绪：quic=:41641 … key=1020c3eae8b3…` | 停净 6 s → 换件 → nohup 起（新 pid 1588460）；身份逐字不变 | ✓ |
| **U3 Mac** | `v0.3.2`（sha `edf27e8a…`）；`serve 就绪 … key=8aee740b7220…`；`后端身份：标签 d07c57dd5bde1fa7`；`STUN：… 眼里是 114.242.60.128:41641` + v4/v6 公网端点已公布 | 原子换名 + `kickstart -k`（新 pid 70052）；身份逐字不变；热修（族归一）在新版继续生效 | ✓ |
| **U2 设备** | 新核 `.so` = **2,997,296 B**（标记 `75ee1751220c-rust` = tag 提交，与出口二进制同源）；`tailcat-default-signed.hsp` 2,180,710 B 装机成功 | 重出 HAP/HSP 全链：`build-core.sh` 门全绿（pin/脏检出/napi-sync/log-index）→ `assembleHsp` → `hdc install -r` → force-stop + 起 → 点开关重连 | ✓ |
| 端到端复验 | 设备 `主机 quic:192.168.3.12, 直连, 17 ms`；出口 `连接采纳 dev=aca645d3 tun=100.64.210.203 ← [::ffff:192.168.3.67]:44176`；真机测速 **↑54 MB/s ↓57 MB/s**；`serve token` 的 control 面 `serverVersion=v0.3.2` | 同 §12.6 的口径（层 0 + 服务面） | ✓ |

**凭证连续性**：台账 append-only ⇒ 换装前设备持有的 `hmw2` token **继续有效**（未吊销），设备无需
重新注入即自动重连；本轮两次出口重启（U1/U3）后设备均自动回到「直连」。
**回滚**：单台出口 = `cp <bak> /usr/local/bin/homeway-rs`（阿里云，先 `pkill`）/ `install + mv -f`（Mac）
+ `launchctl kickstart -k`；设备核 = 重装 v0.3.1 的 HSP（`libs/` 新旧 `.so` 均在 git 外，需从
`~/homeway-rollback/` 或重建取）。**v0.3.1 的回滚件已就位（本表「前置」行）。**



## 13. 部署形态改简 + v0.3.3 换装（2026-10-13，用户指令）

**改简后的正式形态**（本节起，两处生产主机同款；测试 / 多实例仍走 `--state`）：

- 二进制 = **系统 PATH 内**的 `homeway`（Mac = `/opt/homebrew/bin/homeway`〔该机 `/usr/local/bin`
  属 root、装它需 sudo〕；阿里云 = `/usr/local/bin/homeway`）；
- 命令行 = **零参 / 子命令直跑、不带 `--state`**；state = 缺省 `$HOME/.config/homeway`
  （因此终端里 `homeway serve status` / `homeway serve token` 直接作用于在跑实例）。
- 变化点 = state 从各自旧路径**迁到缺省路径**：Mac `~/.config/homeway-rs` → `~/.config/homeway`；
  阿里云 `/opt/homeway/data-rs` → `/root/.config/homeway`。

### 13.1 发版 v0.3.3

| 阶段 | 读数 | 结论 |
|---|---|---|
| 版本 / tag | 0.3.2 → **0.3.3**；tag = main 头 `f1512ce`（含 Windows 支持、二进制改名、直连优先回归） | ✓ |
| 首跑 | windows job 在**发版纪律门**击红（bash 语法跑在 pwsh 下）+ upload-artifact 只列 `*.tar.gz`（windows 产物是 zip ⇒ `if-no-files-found` 也会红）；`v0.3.3` 未产出 Release | ✗ → 修 workflow（纪律门加 `shell: bash`；artifact path 两形都列） |
| 二跑 | 五目标全绿（linux-arm64 2m36s / linux-amd64 2m50s / darwin-amd64 3m19s / darwin-arm64 4m4s / windows 4m19s）+ release job 14s | ✓ |
| 重打 tag | 首跑死在 build 阶段、**无 Release 产出** ⇒ `v0.3.3` 重指到修后的 main 头 `f1512ce`（发版纪律：tag 必须在 main 头上） | ✓ |

### 13.2 Mac 换装（22:29–22:31）

| 件 | 读数 |
|---|---|
| 产物 | `homeway-v0.3.3-darwin-arm64.tar.gz`（sha256 `ba442c51…` 对 `SHA256SUMS` 通过）→ `/opt/homebrew/bin/homeway` |
| 停 | `launchctl bootout gui/501/me.zhaozhe.homeway-exit`（旧 pid 61963，2 s 退净 + 端口释放） |
| state | `~/.config/homeway-rs` → `~/.config/homeway`；旧缺省路径的 prerust 存档 → `~/.config/homeway.bak-20261013`（不参与运行） |
| plist | `ProgramArguments = [/opt/homebrew/bin/homeway]`（**零参**）；`StandardOut/ErrorPath` 与 `WorkingDirectory` → 新 state 路径 |
| 起 | `launchctl bootstrap` → pid 53696；`homeway: 统一进程就绪（state=/Users/zhaozhe/.config/homeway，serve=true relay=false，version=v0.3.3）` |
| 判据 | `serve 就绪 … key=8aee740b7220…`（**身份逐字不变**）；`公网端点：已公布 [114.242.60.128:41641]`；`中继：注册成功（腿 41641 → 123.56.218.212:41741）`；零参 `homeway serve status` = `state=running listenPort=41641`，token 端点 = LAN + 公网 v4 + Relay |

### 13.3 阿里云换装（22:30–22:31）

| 件 | 读数 |
|---|---|
| 产物 | `homeway-v0.3.3-linux-amd64.tar.gz`（sha256 `f7fcaabc…`；static-pie）→ `/usr/local/bin/homeway`；旧件留 `/usr/local/bin/homeway-rs.bak-v0.3.2` |
| 停 | `kill -TERM 1588460`（6 s 退净 + 41641/41741 释放） |
| state | `/opt/homeway/data-rs` → `/root/.config/homeway` |
| 起 | `cd /opt/homeway && setsid /usr/local/bin/homeway >> /root/.config/homeway/cache/unified-stdout.log 2>&1 &`（**无 `--state`**）→ pid 1601626 |
| 判据 | `key=1020c3eae8b3…`（**逐字不变**）；零参 `serve status` = `state=running listenPort=41641`、token 含公网 `123.56.218.212:41641`；零参 `relay status` = `state=running listen=:41741`、`rl133ZkGIEpR…`（**与 Mac 出口 config 引用的 rl1 逐字同**）；`ss` = 41641/udp(eth0) + 41741/udp+tcp |
| 互注 | Mac `中继控制面已连（22:30:59）` ↔ 阿里云 `中继：后端 d07c57dd5bde1fa7 控制面就绪（114.242.60.128:55684）`——**中继重启后出口自动重注册** ✓ |

### 13.4 第二台 Mac（客户端机，192.168.3.8）

`homeway-v0.3.3-darwin-arm64`（sha 与 Mac 产物同：`ba442c51…`）→ `/usr/local/bin/homeway`
（该机 `/usr/local/bin` 属 zhaozhe、可写）；登录 shell `homeway --version` = `homeway v0.3.3` ✓。
**无服务、无 state**——客户端机按需零参使用（`homeway host add <token>` 等，state 自动落在
`~/.config/homeway`）。

### 13.5 回滚与注记

- **回滚（Mac）**：`launchctl bootout` → plist 的 `ProgramArguments` 换回
  `~/bin/homeway-rs.bak-v0.3.2` + `--state ~/.config/homeway`（旧 state 目录被迁走 ⇒ 需先把
  `~/.config/homeway` 挪回 `~/.config/homeway-rs`，或直接改指 `~/.config/homeway`）
  → `launchctl bootstrap`。
- **回滚（阿里云）**：`kill -TERM <pid>` → `cp /usr/local/bin/homeway-rs.bak-v0.3.2
  /usr/local/bin/homeway-rs` → `cd /opt/homeway && HOME=/opt/homeway/served nohup
  /usr/local/bin/homeway-rs --state /root/.config/homeway …`（state 位置可从缺省路径指回）。
- **凭证连续性**：state 整体搬迁（key.bin / tokens.jsonl 随迁）⇒ 台账与身份都不变，
  设备持有的 `hmw2` token 继续有效。换装时设备不在线（`peers=0`），下次连接即用新实例。
- **如实注记①**：换装前旧 state 存档（`~/.config/homeway.bak-20261013`，prerust 形态）曾被一次
  误执行的自刷新（`homeway export --help` / `import --help`——帮助串被当位置参数）：内容等价，
  仅白名单外 1 个 `client/hosts.json.corrupt-*` 条目被替换；该存档**不参与运行**，无影响。
- **如实注记②**：Mac 出口本轮 `公网端点` 只公布 IPv4（IPv6 STUN 无应答，换装前一轮曾公布 v6）——
  属网络侧瞬时形态，出口会按既有节拍重测。

### 13.6 补丁换装 v0.3.4（2026-10-13 23:11–23:12，CLI 本地 term 面修复）

**触发**：用户实测 `homeway term list`（零参 / 统一根）报「出口未在运行…」——出口角色的本地服务
socket 建在 `<state>/serve/` 子目录（`serve/{files,term,speedtest}.sock`），而命令面的本机形态按
早期平坦布局口径拨 `<state>/term.sock` ⇒ 与「零参正式形态」不合（与 13.2/13.3 的 state 迁移
**无关**：旧 `--state <根>` 形态同样拨不通）。修复 = `term_cli.rs::local_term_sock` 扁平优先 +
serve 子目录回退（登记 = `docs/INTEROP-CRITERIA.md` 2026-10-13「v0.3.4 补丁」条）。

| 机器 | 换装 | 判据 |
|---|---|---|
| 发版 | tag `v0.3.4` = main 头 `df2989e`；五目标全绿（darwin-arm64 6m13s 最长）+ release 12s | ✓ |
| Mac | `/opt/homebrew/bin/homeway`（旧件留 `homeway.bak-v0.3.3`）+ `launchctl kickstart -k` → 新 pid 62397 | `key=8aee740b7220…`（不变）；**`homeway term list` 零参通**（「当前没有会话」）；中继腿 23:11:34 重注册 ✓ |
| 阿里云 | `/usr/local/bin/homeway`（旧件留 `homeway.bak-v0.3.3`）；SIGTERM 7s 退净 → `setsid` 零参起 → 新 pid 1603223 | `key=1020c3eae8b3…`（不变）；41641/41741 在听；**零参 `term list` 通**；中继控制面 23:12:09 重连（后端身份 `d07c57dd5bde1fa7` 不变）✓ |
| MacBook Pro | `/usr/local/bin/homeway`（旧件留 `homeway.bak-v0.3.3`） | 登录 shell `homeway v0.3.4` ✓ |
| 三机一致性 | 三处 `--version` 全 = `homeway v0.3.4` | ✓ |

**注**：重启会丢当轮 term 会话（会话在服务进程内；Mac 上换装前那两条 `opencode`/`shell` 会话随之
结束）——这是既有语义（会话由出口进程持有），非本次回归。

### 13.7 换装 v0.3.5（2026-10-13 23:41–23:43，MIT 许可 + README 收束 + CLI 主机寻址统一）

**触发**：用户指令六条（MIT 许可 / README 只讲功能与命令 / `term new` 直接进入 / files-term-forward
统一 host 风格且不带 `--token` / Windows 段一句话 / 去「迭代·测试形态」节）。CLI 面 = `--host` 缺省走
「默认主机」（单主机直接用、无主机提示先 `host add`、多主机要求显式指定；term 另有本机出口面）；
`portfwd` 用法改向 `homeway forward`（登记 = `docs/INTEROP-CRITERIA.md` 2026-10-13「v0.3.5」条）。
**App 核零改**（CLI 不进 `.so`）⇒ tier pin 不动、设备包不出。

| 机器 | 换装 | 判据 |
|---|---|---|
| 发版 | tag `v0.3.5` = main 头 `fe17931`；五目标全绿 + release 11s | ✓ |
| Mac | `/opt/homebrew/bin/homeway`（旧件留 `homeway.bak-v0.3.4`）+ `kickstart -k` | `key=8aee740b7220…` 不变；零参 `term list` 通；**零参 `files list` 出「还没有添加主机」提示**（新行为；该机未配主机）✓ |
| 阿里云 | `/usr/local/bin/homeway`（旧件留 `homeway.bak-v0.3.4`）；SIGTERM → `setsid` 零参起 | `key=1020c3eae8b3…` 不变；41641/41741 在听；零参 `term list` 通 ✓ |
| MacBook Pro | `/usr/local/bin/homeway`（旧件留 `homeway.bak-v0.3.4`） | 登录 shell `homeway v0.3.5` ✓ |
| 互注 | Mac `中继控制面已连（23:43:01）` ↔ 阿里云 `中继：后端 d07c57dd5bde1fa7 控制面就绪（114.242.60.128:62088）` | ✓ |

### 13.8 换装 v0.3.6（2026-10-14 15:27–15:34，term 多 attach 异宽几何治理 + CI 两修）

**触发**：用户两窗口异宽 attach 同一会话跑 opencode 的实测（2026-10-13 深夜～14 日凌晨）暴露一条
四环振荡链 + 两处残留——九项修复全部在出口侧 raw 腿面（清屏序 + 跳环尾 / hover·应答·focus-out
不记活动 / 应答让位收窄到 active 腿 / 几何切换观测行；清单见 `CHANGELOG.md` v0.3.6、登记见
`docs/INTEROP-CRITERIA.md` 2026-10-13/14 七条）。**App 核 surface 腿面零改**（App 走 INPUT 帧）
⇒ tier pin 不动、设备包不出。

**发版前先修 main CI**（否则 tag 打在红 CI 的 main 头上）：① `term_cli::local_mode_rejects_timeout`
自 v0.3.5「`--host` 可选化」起双 runner 一直红——测试用缺省 state，本地面成立条件 = 本机有
`term.sock`（跑测试那台机器有出口在跑就过，CI 空环境报「先 host add」）；v0.3.5 发版只看了 release
workflow 的绿、漏看 ci。修 = 临时 state 显式造/不造 sock。② 本批新增 `clear_screen_skips_backlog`
首跑在 CI 挂 30+ 分钟（已取消）——程序用 zsh 专属 `TRAPWINCH()`，CI 登录 shell 是 bash 永不触发
`(W)` 标记、退出条件「收满 1MB」在 50ms/点限速相下 ≈14 小时；修 = POSIX `trap … WINCH` + 10s 墙钟
上界。两修后 ci 转绿（test 1m24s / 1m52s，v0.3.5 以来首次）。

| 机器 | 换装 | 判据 |
|---|---|---|
| 发版 | tag `v0.3.6` = main 头 `34e7fbf`（ci 绿之后）；五目标全绿（darwin-arm64 4m23s 最长、linux 两目标 2m41s）+ release 11s | ✓ |
| Mac | `homeway-v0.3.6-darwin-arm64.tar.gz`（sha 对 `SHA256SUMS` OK，7,540,056 B）→ `/opt/homebrew/bin/homeway`（换装前跑的是含本批前八项的 **devel 测试件**，直接覆盖；正式回滚件 = `homeway.bak-v0.3.5`，由早先备的 `homeway.v0.3.5.bak` 规范化改名）+ `launchctl kickstart -k` → 旧 pid 78467 → 新 **31174** | `统一进程就绪（… version=v0.3.6）`；`key=8aee740b7220…` **不变**；`中继：注册成功` + `中继控制面已连`（15:30:43）；端点 = 内网 + 公网 v4 + 公网 v6 + Relay；零参 `serve status`=running、`term list` 通 ✓ |
| 阿里云 | `homeway-v0.3.6-linux-amd64.tar.gz`（sha OK，4,234,217 B）→ `/usr/local/bin/homeway`（旧件留 `homeway.bak-v0.3.5`）；`kill -TERM 1604309` **6 s 退净** → `cd /opt/homeway && setsid /usr/local/bin/homeway >> /root/.config/homeway/cache/unified-stdout.log 2>&1 &`（零参）→ 新 **1608318** | `后端身份 … 公钥 1020c3eae8b3…` **不变**；`统一进程就绪（… serve=true relay=true … version=v0.3.6）`；零参 `serve status`=running(41641) / `relay status`=running(:41741)；`ss` = 41641/udp + 41741/udp+tcp；零参 `term list` 通 ✓ |
| 互注 | 阿里云重启后 **0.7 s**：`中继：后端 d07c57dd5bde1fa7 控制面就绪（114.242.60.128:60465；SESSION 通告启用拨腿模式）`（15:33:31.6）——Mac 出口自动重注册 | ✓ |
| MacBook Pro | **离线**（ping 100% 丢包、ssh 22 超时）——**待换装**：件在 Mac mini `/tmp/hw-rel-0.3.6/homeway` 或 release 直取；形态同 13.4（→ `/usr/local/bin/homeway`，旧件留 `homeway.bak-v0.3.5`） | ✗ **待办** |
| 两出口一致性 | Mac / 阿里云 `--version` 均 `homeway v0.3.6` | ✓ |

**注**：
- 重启收工当轮 term 会话（既有语义；两出口换装时均无会话在跑）。
- 操作坑（本轮实踩，记忆已同步）：① `cp` 覆盖写旧 inode 的新二进制被 macOS SIGKILL（exit 137）——
  须 `rm` 后 `cp` 换新 inode；② `homeway serve restart` 只在进程内重建引擎、**不换进程**——换二进制
  后必须 `launchctl kickstart -k`；③ 经 ssh 用 `setsid … &` 起远端进程，ssh 会话会被继承的 fd 挂住
  （远端进程已脱离、不受影响）——另开会话核判据。

**首装流程变更注记（2026-10-14，待随下版发布）**：新 state 的配置缺省改为 `[serve] enabled = false`
——**新出口机首装多一步** `homeway serve start`（写 `enabled = true` 并装配；之后零参 launchd /
`setsid` 形态按它起出口）。三台现有机器 config 已显式 `enabled = true`，不受影响。动机：客户端机
上任何纯读命令（如 `term list`）按需拉起守护时，曾按缺省装配出口（生成身份、签发 token、监听
41641）并以本机空会话表遮蔽远端主机（用户实测：`host add` 后 `term new` 建在本地）。详见
`CHANGELOG.md`「未发布」与 `docs/INTEROP-CRITERIA.md` 2026-10-14「出口须显式启用」条。

### 13.9 换装 v0.3.7（2026-10-14 19:38–19:42，出口须显式启用 + 默认端口换区间 + install.sh + CLI 输出梳理）

**触发**：用户四件（新客户端机 `term list` 两次行为不一致 / `host add` 后 `term new` 建在本地 → 出口须显式启用；
tailcat 沿袭端口撞端口 → 38241/:38341；一条命令安装；CLI 输出简洁可行动）。**全部为新装机器语义**：已部署
三机 config 显式写了 `enabled = true` 与端口，零影响；App 核零改 ⇒ tier pin 不动。清单见 `CHANGELOG.md` v0.3.7。

**发版纪律本次执行到位**：先看 `gh run list --branch main` ——四个功能 commit 的 ci 全绿；bump 提交的 ci 在
macos runner 挂 `homeway-quic` 的 `normal_races_do_not_trigger_retry_or_gate`（第 7 轮候选 CONNECTION_REFUSED，
本地 UDP 端口瞬时形态、与本批无关）→ `gh run rerun --failed` 绿 → 才打 tag。

| 机器 | 换装 | 判据 |
|---|---|---|
| 发版 | tag `v0.3.7` = main 头 `6bebd0a`（ci 绿后）；五目标全绿（darwin-arm64 4m8s 最长）+ release 11s | ✓ |
| Mac | **用 `install.sh` 装**（脚本真实验收：`sh install.sh --bin-dir /opt/homebrew/bin --autostart no`——已有 launchd 代理，不让脚本再建一个）：识别 darwin-arm64 → 302 取版本 → 下载 + SHA256SUMS 校验 → 旧件备份 `homeway.bak-v0.3.6` → rm+cp → `xattr -c` → `--version` 通过；`kickstart -k` → 新 pid **76397** | `统一进程就绪（… version=v0.3.7）`；`key=8aee740b7220` **不变**；serve 仍 `listenPort=41641`（config 显式，端口缺省变更零影响）；`中继：注册成功`（19:39:29）；零参 `term list` 通（本机面 = sock + config enabled 两维均成立）✓ |
| 阿里云 | `homeway-v0.3.7-linux-amd64.tar.gz`（sha OK）scp → `/usr/local/bin/homeway`（旧件留 `homeway.bak-v0.3.6`）；SIGTERM 6 s 退净 → `setsid` 零参起（起进程的 ssh 本地 8 s 限时后释放）→ 新 pid **1610493** | `version=v0.3.7`；`公钥 1020c3eae8b3` **不变**；serve 41641 + relay :41741 running、`ss` 两端口在听；零参 `term list` 通 ✓ |
| 互注 | 阿里云 relay 日志：19:39:30（Mac 重启后）与 19:41:00（阿里云重启后 0.3 s）两次 `中继：后端 d07c57dd5bde1fa7 控制面就绪` | ✓ |
| MacBook Pro | **仍离线**（ping 100% 丢包）——**待换装**：上线后在它上面跑 `curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh \| sh`（客户端机，不配自启动）即可；v0.3.6 那次也欠着，一并补 | ✗ **待办** |

**注**：出口机上 `install.sh` 的 `--autostart no` 是必须的——脚本的自启动项 Label 是 `homeway`，与既有
`me.zhaozhe.homeway-exit` 并存会双实例撞 state 锁。新机器用脚本配自启动则无此问题。

**13.9 补：MacBook Pro 全新安装测试（2026-10-14 21:10–21:30，用户指令「删干净再全新装」）**。机器现 IP
**192.168.3.71**（`zhaozhedeMacBook-Pro-2.local`，DHCP 换址——此前 ping .8 不通的原因）。盘点残留 =
`/usr/local/bin/homeway`（v0.3.5）+ `.bak-v0.3.3/.bak-v0.3.4` + `~/.config/homeway`（含 `serve/key.bin` +
`tokens.jsonl` + `config.toml [serve] enabled = true`——**正是 v0.3.6 前「纯读命令意外成出口」的现场**）；
无进程、无 launchd。全删后 `curl -fsSL …/main/install.sh | sh`（ssh 非交互 ⇒ 不配自启动）装 v0.3.7。

| 验收 | 读数 | 结论 |
|---|---|---|
| 两次 `term list` | 均「还没有添加主机…」exit 1；无守护、`~/.config/homeway` **未创建** | ✓（v0.3.7 ③） |
| `host add --name home <Mac mini 凭证>` | 三端点可达：192.168.3.12 直连 5ms / 114.242.60.128 直连 12ms / 123.56.218.212 中继 16ms | ✓ |
| `term list` / `term new fresh-test -d` | 转发到 Mac mini（空表 → 建会话）；Mac mini 本机 `term list` 见 `fresh-test 80x24 shell idle` | ✓ 会话在出口 |
| 客户端 state | `[serve] enabled = false`；`serve/` 空目录（无身份/凭证/sock） | ✓（v0.3.7 ①②） |
| `term delete fresh-test`（从 MBP） | **挂死 > 6 min**（CLI 无超时阻塞读；守护侧流 `↑0B ↓0B`） | ✗ **新发现缺陷**（见下） |

**新发现（岛层，严重，待立批）**：`new -d` 成功后 0.6 s，客户端岛 UDP 发送面报 `No route to host (errno 65)`
（MBP Wi-Fi 瞬断——该机今晚掉线过、换过 IP）→ 阶梯 M（换 socket）→ 迁移 700 ms 未确认 → **重连赛跑只投
1 个候选（当前直连；未用公网直连/中继）** → 16 连败 → 世代重建 → `⚠️ 宿主会话不健康（patrol）`、主机
`state=failed` → **此后 7 分钟零重连尝试**（网络 10 s 内已恢复，ping 5 ms）；期间所有远程 term 操作开流
成功但零字节、CLI 永久挂（list/delete 皆然）。`pkill` 客户端守护重拉即恢复 ⇒ 卡死态。待修三层：
①岛重连赛跑投全名册 + 阶梯耗尽后退避重试而非永久 failed；②daemon 对 failed 主机的 `term.open` 立刻
报错 + 世代重建关闭旧世代孤儿流（给 CLI EOF）；③CLI 一次性 term 操作首应答加 `--timeout` 预算。

**13.9 补②：岛恢复接棒三层修（`7d3cea3`）的 MacBook Pro 真网演练（2026-10-14 22:10–22:25，测试件 devel）**。
演练 = 停 Mac mini 出口（`launchctl bootout`）再拉起，MBP 客户端守护在线观察：

| 演练 | 读数 | 结论 |
|---|---|---|
| ① 出口停 27s | 断线期间 MBP `term list` **10 s 超时报可行动文案**（`读 GREETING：出口 10s 内未应答——主机可能刚断线…`；修前永久挂）；岛 R 赛跑 **`候选 3 个`**（修前 1 个；直连窗口 1.4s 到点解锁中继）；出口回来后 R#4 立即成功——**胜出公网直连 114.242.60.128**；host 全程 ready（B 门差一次未到） | ✓ |
| ② 出口停至 B 门 | t+21 `state=failed reason=岛不健康（patrol）；自动重连中`；日志：`B 门僵尸收口 —— 现任连接已关、0 条在册服务流按 EOF 收`（v5 行文——reconnect-oneshot 前为 `世代重建前收口 —— …`） → `…置 failed，起世代重建` → `世代重建：重赛跑全名册（第 1 次，候选 3 个，退避 1s）` → 失败 → `第 2 次…退避 2s` → **`世代重建成功（第 2 次，胜出 直连 114.242.60.128:41641，耗时 17ms）—— 会话回 ready`**（出口拉起后 5 s 内）；`term list` 通 | ✓ |

**环境发现（与产品无关、值得关注）**：两轮演练 MBP 的岛都再次报 `上行发送面错误（No route to host (errno 65)）`，
且重连胜出的总是**公网直连**而非局域网 192.168.3.12——MBP（Wi-Fi）→ Mac mini 的局域网 UDP 路径本身不稳
（ping 通但 UDP 发送偶发 EHOSTUNREACH；疑 AP 隔离 / ARP 抖动）。全名册 R 正是靠公网路径兜住的。
MBP 现为 devel 测试件（含本修复，回滚件 `homeway.bak-v0.3.7`），待下版发版换正式件。

### 13.10 换装 v0.3.8（2026-10-15 15:00–15:10，客户端断线自愈 + serve grant + attach 路由修复 + CLI 一致性/简化）

**触发**：用户在装 v0.3.7 的机器上 `term attach` 报「term.sock 不存在」（attach 路由 bug `02067d0` 已修未发版）→ 发版。
清单见 `CHANGELOG.md` v0.3.8 条；App 核零改 ⇒ tier pin 不动。

**发版前修了两处 CI 红**（此前 4 个 push 挂）：① `tcc.rs` 的 `#[cfg]` 特判在非 macOS 恒 Granted + unused-import
（三平台挂——**本地 mac 全绿看不见**，教训：平台分支的编译面本地交叉 check 一下）；② grant 两个 helper 只被
macOS 代码调用（非 mac 死代码挂 clippy/windows，`#[cfg(macos)]` 门）。修后 ci 全绿（macos test 上一轮 20 min
挂死判环境性 flake——同代码本轮 1m34s 绿）。**install.sh 在 tag 前已坏**（--grant 参数说明行丢行首 `#`，被当命令
执行、`set -eu` 中止在安装前——Mac mini 首装实测击中）：tag 后即时修复推 main（README 的 curl 拉 main 即刻生效；
**v0.3.8 tag 内的 install.sh 仍是坏的**——须用 main 版）。

| 机器 | 换装 | 判据 |
|---|---|---|
| 发版 | tag `v0.3.8` = main 头 `eb6219b`（ci 绿后）；五目标全绿（darwin-arm64 4m29s 最长）+ release 13s | ✓ |
| Mac | 修复版 install.sh（`--bin-dir /opt/homebrew/bin --autostart no`；旧 devel 测试件备份 `homeway.bak-(devel)`）+ `kickstart -k` → pid **82769** | `version=v0.3.8`；`key=8aee740b7220` **不变**；serve 41641 running；零参 `term list` 通 ✓ |
| 阿里云 | `homeway-v0.3.8-linux-amd64.tar.gz`（sha OK）→ `/usr/local/bin/homeway`（旧件留 `homeway.bak-v0.3.7`）；SIGTERM 6s 退净 → `setsid` 零参起 → pid **1615258** | `version=v0.3.8`；`公钥 1020c3eae8b3` **不变**；serve 41641 + relay :41741 running；`term list` 通；Mac 出口控制面重就绪（15:06:57）✓ |
| MacBook Pro | **离线**（ssh 超时——`sleep 1` 分钟的省电形态）——**待换装**：机器唤醒后一条命令 `curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh \| sh`（客户端机，自启动选 N）| ✗ **待办** |

### 13.11 换装 v0.3.9（2026-10-16 09:52–10:05，event-driven-heal 核侧 + 五轮评审整改 + 防自旋）

**触发**：event-driven-heal 变更收口（真机验收 + 五轮外部评审 r5 终审通过 + 整改后重采五项全过）。
清单见 `CHANGELOG.md` v0.3.9 条；**本版主要是客户端核（App 侧）行为**——出口面无 CLI/协议变更，
出口机换装属例行跟进（App 核 = tier 仓 pin 同批前进）。

| 机器 | 换装 | 判据 |
|---|---|---|
| 发版 | tag `v0.3.9` = main 头 `2b808e1`（ci 绿 2m14s 后）；release 五目标全绿（4m37s）+ SHA256SUMS | ✓ |
| Mac mini | `homeway-v0.3.9-darwin-arm64.tar.gz`（sha `5011bb0b…` = SHA256SUMS）→ 备份 `homeway.bak-v0.3.8` → rm+cp（换二进制坑：cp 覆盖旧 inode 会被 SIGKILL）→ `launchctl kickstart -k` → pid **46243** | `version=v0.3.9`；`key=8aee740b7220` **不变**；serve 41641 running（LAN/公网 v4/v6 + Relay 端点公布）；零参 `term list` 通；peers=1 ✓ |
| 阿里云 | `linux-amd64.tar.gz`（sha `b4953c7d…` 双侧核对）scp 上传 → 备份 `homeway.bak-v0.3.8` → SIGTERM 退净 → rm+cp → `/opt/homeway` 下 `setsid` 零参起 → pid **1619224** | `version=v0.3.9`；`公钥 1020c3eae8b3` **三版不变**（0.3.7→0.3.8→0.3.9）；serve 41641（UDP %eth0）+ relay :41741 双角色；STUN 公网端点公布；Mac 出口中继控制面 09:59:54 重认证（阿里云 09:59:53 就绪后 1s 内）✓ |
| MacBook Pro | **离线**（ping 通、ssh 22 超时——省电形态，同 v0.3.8 时）——**待换装**：机器唤醒后 `curl -fsSL https://raw.githubusercontent.com/zhaoyswd/homeway-rs/main/install.sh \| sh`（客户端机，自启动选 N） | ✗ 待办（v0.3.8 起累计） |

**过程注记**：①ci-local 门修一件——fbb149e 测试里的 `std::thread::sleep` 违反 QUIC 隔离门层 3
（全文件面含测试），换有界让权等待（门不弱化）；②本机 push 前远端有用户的 README commit
（9f4c5b3）——rebase 后推，无冲突。

### 13.12 换装 v0.3.10（2026-10-16 22:36–22:43，term attach 三连修）

**触发**：openspec 变更 `term-attach-resilience` 收口发版（间隔哨兵/断链重连/退出应答排空，
两道评审门 r1 方案 + r2 完工代码评审全过）。清单见 `CHANGELOG.md` v0.3.10 条。
哨兵是**出口侧**行为（新 CLI 对旧出口/旧 CLI 对新出口均退化为现状不劣化）。

| 机器 | 换装 | 判据 |
|---|---|---|
| 发版 | tag `v0.3.10` = main 头 `0312b57`（CI 绿 2m18s）；release 五目标 6m15s + SHA256SUMS | ✓ |
| Mac mini（= 工作机本机） | `darwin-arm64.tar.gz`（sha `6ac3b83b…` = SHA256SUMS）→ 备份 `homeway.bak-v0.3.9` → rm+cp + `xattr -c` → `launchctl kickstart -k gui/501/me.zhaozhe.homeway-exit` → pid **20323** | `version=v0.3.10`；`key=8aee740b7220` **不变**；serve 41641 running；公网端点双栈（v4 114.242.60.128 + v6 2408:8207:…）+ Relay 公布；中继腿 22:40:26 重注册成功（阿里云 22:38:41 就绪后）；零参 `term list` 通 ✓ |
| 阿里云 | `linux-amd64.tar.gz`（sha `fabeb451…` 双侧核对；scp 上传——直连 GitHub 断流旧坑）→ 备份 `homeway.bak-v0.3.9` → SIGTERM **6s 退净** → rm+cp → `/opt/homeway` 下 `setsid` 零参起 → pid **1624575** | `version=v0.3.10`；`公钥 1020c3eae8b3` **四版不变**（0.3.7→0.3.10）；serve 41641（UDP %eth0）+ relay :41741（UDP+TCP）；`serve status` running listenPort=41641；零参 `term list` 通 ✓ |
| MacBook Pro | **在线**（v0.3.8 起累计待办清账）：devel 旧件（10-14 岛断线自愈批）→ 备份 `homeway.bak-devel-1014` → SIGTERM 退净 → rm+cp + `xattr -c` → `--state` 形态 setsid 起（pid **61504**，host list 自动拉起兜底） | `version=v0.3.10`（serve=false relay=false 客户端形态，22:42:28 就绪）；`host list` 通 ✓ |

**过程注记**：①Mac mini 的 ssh 免密通道不存在（本机 key 不在其授权表）——**mini 即工作机本机**，
直接本地换装（hosts 里 `mac-mini` 指到代理 fake-ip 段 198.18.x 的坑一并记档）；②`launchctl
kickstart` 需全限定域 `gui/501/<label>`（裸 label 报 target specifier）；③MBP 的 `homeway`
不在非交互 ssh 的 PATH（`/usr/local/bin` 缺省不进），全路径操作；④SHA256SUMS 登记的是 **tarball**
sha，解压后二进制 sha 需对照本机解压件比对（MBP 件即此口径 `c19ae5b6…` 与本机一致）。


### 13.13 换装 v0.3.14 与快速重启现场（2026-10-21）

用户明确授权「推送+部署」。两台实读旧版均为 v0.3.13；v0.3.14 原先没有 tag/Release，
本批 PTY 修复随 v0.3.14 一并发布。main/tag = `1fca5e9935b1`，
[CI 39542506433](https://github.com/zhaoyswd/homeway-rs/actions/runs/39542506433) 八 job 全通过；
[Release 39543501804](https://github.com/zhaoyswd/homeway-rs/actions/runs/39543501804) 五目标与发布全通过。
本地 ci-local quick 通过（RRR 317s，既有 WARN 1 / SKIP 1；二者未计为通过）。

| 机器 | 产物与操作 | 现场判据 |
|---|---|---|
| Mac | darwin-arm64 tar SHA256 `b2411f84e59236a7eca2b54ba1763675f1cc89068574cb83240ce5e712c458ae`；二进制 SHA256 `8decdc663b638ad0db6b85d9f6f0d5daf9a70ec4cf63dcff767639548ba18a4c`。临时件+原子换名，launchd 重启，12:27:00 pid53578 | v0.3.14，41641未漂移；公网v4/v6已公布；身份/config hash不变。真实PTY背压下独立控制连接KILL应答0.5ms，临时会话出表 |
| 阿里云 | linux-amd64 tar SHA256 `468283d7aa07b50d364209cf16c42c409d835d574787e2f6eaefffd7692d4f3e`；二进制 SHA256 `f7b71d69d3e8f0d39d068cb40d275f289f08fa613547d8e081f398746a451d49`。scp后比对，SIGTERM退净并确认端口释放，原子换名，12:27:32零参起pid1650833 | v0.3.14，serve41641；身份/relay key/config hash不变。真实PTY背压KILL应答0.7ms，临时会话出表，master fd 0→0 |

**验收发现并处置**：阿里云首轮 TCP41741 `Address in use`，UDP41741可用但控制面降级。
当时没有 live listener，旧TCP连接状态在端口上仍可阻止不带地址复用的新 bind。
`Relay::listen_tcp` 此前从std迁到socket2时漏了Unix SO_REUSEADDR；原生Linux私有容器中
主动关闭已接入连接后，false重绑必报Errno98、true成功。12:29:13再次重启后TCP v4/v6均
恢复（pid1652831），12:29:33 Mac OK-MAC认证与控制面恢复。为避免未来升级复发，补丁
v0.3.15恢复Unix地址复用，不启用SO_REUSEPORT；代码/回归见同日relay-tcp-restart评审。

**回退件**（目录0700，state归档0600；不改身份与token）：Mac
`~/homeway-rollback/v0.3.13-before-v0.3.14-20261021T041307Z/`，阿里云
`/opt/homeway/backups/v0.3.13-before-v0.3.14-20261021T041309Z/`，各有homeway与state.tgz。
本次没有安装手机应用或更换其它客户端。


### 13.14 换装 v0.3.15 与重复PTY回收验收（2026-10-21）

main/tag `a2576d0e97bc`；[CI 39544607934](https://github.com/zhaoyswd/homeway-rs/actions/runs/39544607934)
八项全过，Ubuntu/macOS各1221/0、各37 ignored；真实主动关闭后立即重绑回归在两平台均过。
[Release 39545014604](https://github.com/zhaoyswd/homeway-rs/actions/runs/39545014604)五目标与发布全通过。
本机额外并发全仓首轮clear_screen_skips_backlog收尸卡在wait4，留存sample后终止；该用例单独复验
1/0（3.53s）、同配置默认并发全仓复验1221/0，未改断言/超时、未把卡住轮计绿。

| 机器 | 产物与启动 | 判据 |
|---|---|---|
| Mac | tar SHA256 `7803651a9e697c81fd0fda0910141a78e704455909854dcf0739ac76df7eea91`；binary SHA256 `3ad88601320d0c6accfdc5871bd31ae30854acd81823a4ea48526fc09896a55d`。原子换名，launchd零参，pid61094，12:43:42起 | v0.3.15，41641不漂移；公网双公布；身份/config不变；PTY背压控制KILL应答0.8ms |
| 阿里云 | tar SHA256 `d21c5a2c114a15fcff98623e2cda71a0bf80c69ead1b8a87881c5ec0eb0edb45`；binary SHA256 `a8e723d1d1d725124de12aa597859e8c556e9a676b13bd545ae5d542a6ef8819`。旧进程退净后等待旧版TCP状态自然回收62s，再零参起，pid1654207，12:45:05 | v0.3.15，41641/udp、41741/udp+tcp双族；身份/relay key/config不变；PTY背压KILL应答0.9ms；12:45:35 Mac控制面OK-MAC恢复 |

**TCP快速重启实证**：存在1条真实已认证后端控制连接时，12:46:19调用零参 `homeway relay restart`，
61.0ms内TCP41741双族监听恢复，统一进程pid与serve41641不变，12:46:20 Mac重认证；本轮关闭连接
未留下TIME_WAIT（记0，未冒充生产TIME_WAIT重绑证据；该形态由上方真实FIN/EOF单测覆盖）。
首次从未设复用的旧监听器升级仍应等待其旧连接状态过期，不修改系统TCP参数。

**重复PTY验收发现**：会话出表并不等于资源回收。重复背压KILL两轮后，Linux无PTY子进程、
无pump/resp/writer线程，但残留4枚master（tty-index0/2各2枚，core+writer）超过60秒不退。
定位为无活动腿首次finish的空ENDED集被误判为幂等收工；需要v0.3.16补齐运行态回收。
本轮fd计数断言失败如实保留，未将仅LIST空写成无泄漏通过。

核/App：pin `a2576d0e97bc`，干净正常构建3,217,016B、a2576d0e97bc-rust，NAPI/log-index/
vocab/code-map门通过；签名debug HSP内核与两处落盘逐字节一致，未装手机。
核SHA256 `2d4c4cb5895810ce6680510e71f771775a95eda1e369bf7a918d9ba9bb1a09c1`。
回退目录：Mac `~/homeway-rollback/v0.3.14-before-v0.3.15-20261021T043311Z/`；
阿里云 `/opt/homeway/backups/v0.3.14-before-v0.3.15-20261021T043311Z/`。


### 13.15 最终换装 v0.3.16（2026-10-21）

用户授权链同§13.13。最终main/tag `ccda247059c3cc1ee6ffe3107072f451cef74724`。
[CI 39547125580](https://github.com/zhaoyswd/homeway-rs/actions/runs/39547125580)八项通过，
Ubuntu/macOS各1225/0、各37 ignored；
[Release 39547545534](https://github.com/zhaoyswd/homeway-rs/actions/runs/39547545534)
五目标与发布全通过。此前0524cb0轮的macOS取消测试失败已仅以独立测试安全期限修正，
生产代码/90ms期限臂/两臂350ms断言未放宽；完整记录见同日deadline-cancel-test评审。

| 机器 | 已核验产物 | 最终在役与验收 |
|---|---|---|
| Mac | tar SHA256 `9b70a555048f9e42f1a4371539ea793b3281ca3aa4a9c9250e6c474f52dde37b`；binary SHA256 `089c88af47ed80b33336029b4946149cd21fcc20e5924e36fc1557a9eb3428b0` | launchd零参 `/opt/homebrew/bin/homeway`，pid70081，v0.3.16，41641未漂移。连续4轮真实输入背压+独立KILL，控制应答0.7–0.9ms，每轮LIST出表且lsof master归零；最终公开候选含LAN/公网v4/Relay，IPv6按观测动态公布 |
| 阿里云 | tar SHA256 `f77664089f9f940dba1bfc1227b02841606aca4db5335a1d55b97c517354d622`；binary SHA256 `0a82c5cd12aa52010490222c41052327bda6095d198d0e6657cf51e797935d2f` | 13:11:36零参 `/usr/local/bin/homeway` 起，pid1657944；v0.3.16，41641/udp、41741/udp+tcp双栈正常。连续4轮背压KILL，控制应答0.7–1.2ms，每轮LIST出表且/proc master归零 |

**重复回收判据**：每轮发送4MiB数据到不读输入的 `exec sleep 30` 会话，先确认客户端发送仍在
真实背压，再由另一条控制连接KILL；Linux实际接受81920B、Mac16384B即进入背压。原raw腿可因
取消写而先EOF，不把它误当ENDED必达；控制OK、写者收工、会话出表和master归零分别验证。
每轮独立名，不改用户文件；4轮结束后两台均没有测试会话和PTY master残留。

**中继快速重启**：13:12:24有1条真实已认证后端控制连接时调用 `homeway relay restart`，
52.0ms内TCP41741 v4/v6恢复监听；统一pid1657944与serve41641保持，Mac13:12:24.993
OK-MAC认证恢复，阿里云13:12:25.117后端控制面就绪。换装自v0.3.15无需额外等待旧状态，
本轮停旧进程后TIME_WAIT记0；不把这个0写成TIME_WAIT覆盖，FIN/EOF重绑由CI回归验证。

**身份/配置**：Mac serve/key.bin与config、阿里云serve/key.bin/relay.key/config哈希均与升级前
一致；保留凭证台账，不重建身份。Mac已有2枚peer自动恢复注册（未据此冒充手机L3流量验收）。
**App核**：最终pin ccda247059c3，干净正常构建3,217,464B，ccda247059c3-rust；NAPI21导出/
词表/代码地图/日志索引门通过；签名debug HSP内核与两处落盘一致。未安装手机或其它客户端。

**回退件**：Mac `~/homeway-rollback/v0.3.15-before-v0.3.16-20261021T045020Z/`；
阿里云 `/opt/homeway/backups/v0.3.15-before-v0.3.16-20261021T045020Z/`，各有旧homeway与state.tgz，
保留之前v0.3.13/v0.3.14备份。Mac回退走临时件+原子换名+kickstart；阿里云先SIGTERM退净、
确认监听释放再换旧件，沿用既有零参nohup形态。此次未增加或更换系统自启动项。

证据：`/tmp/pty-finish-{mac,aliyun}-repeat.log`、`/tmp/pty-finish-github-ci.log`、
`/tmp/pty-finish-final-core-{build,hsp}.log`与本机artifact-hashes/core-artifacts JSON。
