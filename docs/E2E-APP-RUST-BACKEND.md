# E2E：真机 App（现役 Go 核）× Rust 后端（homeway-rs serve/relay）

> R6.5 真机 E2E 实测记录。日期：2026-10-04 14:00–14:35（接棒会话，前会话起好环境后异常中断）。
> 目的：在 R7（Rust 核替换 App 核）之前，先用**现役 App（Go 核 + C++ term surface + FilesClient）**
> 打 **Rust 后端**（本仓 `homeway-cli serve` / `homeway-cli relay`），验证协议面/拦截层/服务面
> （files/term/speedtest/dns）与连接生命周期（建连/中继/自愈）的完整互通。
> 手机：FMR0224116011480（Mate 60 Pro，HarmonyOS），tier App 进程 45776。
> 判据口径：全部真凭据（tun 日志行 / 出口日志行 / 系统计数器），App 内 http 与 hdc ping 不算。

## 0. 环境与接棒说明

- **续用前会话实例**：relay（PID 67122，`--state /tmp/homeway-rs-e2e-relay --listen 0.0.0.0:42770 --advertise 192.168.3.12:42770`）
  与 exit1（PID 67246，`--state /tmp/homeway-rs-e2e-exit --listen 42670 --public-endpoint 192.168.3.12:42670 --relay rl1… --files-root /tmp/homeway-rs-e2e-files --verbose`）。
  接棒时 exit stdout 仅 2.9KB、无 peer——**前会话未注入手机**（手机当时停在旧主机 zhaozhe 的终端页，自 10-03 23:40 起扩展进程已退、idle）。
- exit1 四条就绪判据齐（原文，stdout.log）：
  - `serve 就绪：wg=:42670（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true tokens=1 key=8ada9552a019…`
  - `中继：注册成功（腿 42670 → 192.168.3.12:42770）—— 客户端可经它到达本机`
  - `term: 检测规则已加载 22 份（覆盖目录 /tmp/homeway-rs-e2e-exit/serve/agent-detection）`
  - `intercept: 过境拦截就绪（隧道IP 100.64.255.1；豁免=转投本机同端口；TCP 并发上限 1024）`
- 本会话自起实例：exit1 重启一次（恢复测试，同参、cwd=/tmp 防仓库根副作用）、exit2 迭代三轮
  （42671 → 误参 41642 → 42672，中继腿测试）；全部已收（见 §5 清场）。
- **注入通道注意**（方法论）：`aa start --ps host_token` 走 `onNewWant → applyHostInjection`，
  只**新增/替换主机并设为当前**，不自动建连——连接仍需 UI「全局代理」开关发起。注入判据行：
  `10-04 14:08:04.961 …/testTag: 主机地址注入：替换已有主机的地址并设为当前 h179109408495437（清掉 1 条同身份旧条目）`。
- token 均经 `homeway-cli serve token --state …` 取；现役出口 token 只读未动其它。

## 1. 判据逐项表

时间轴均为 2026-10-04 本地时间；「手机」= 沙箱 `tailcat-tun.log`（核），「App」= `tier-app.log`，
「exit」= 测试 exit 的 stdout.log，「relay」= `/tmp/homeway-rs-e2e-relay/cache/relay.log`。

| # | 项 | 结果 | 原文判据行（时间戳） |
|---|---|---|---|
| B1 | VPN 建连：身份 | ✓ | 手机 14:11:04 `身份：复用（dev=7dc61647 pub=6e18bb87）`（devTag 与现役出口共用=master 派生，跨后端稳定） |
| B1 | warmup | ✓ | 手机 14:11:04 `warmup pong: 就绪（判据=wg）` |
| B1 | attached | ✓ | 手机 14:11:04 `attached（数据面已接管 fd=90，L3 直通）`（另 `wgcore: 应用面就绪（TUN 已接上，transit 直通，MTU 1280）`） |
| B1 | 链路直连 | ✓ | 手机 14:11:04 `link: via=direct ep=192.168.3.12:42670 rtt=26ms（新栈状态快照）`；`赛跑结算：胜出 直连 192.168.3.12:42670（镜像 1 包，耗时 21ms）` |
| B1 | 出口能力通告 | ✓ | 手机 14:11:04 `出口能力：构建 homeway-rs-dev ｜ 默认路径 UDP：DNS:53 可用 / 通用（非 53）可用 … ｜ 探测往返 21ms` |
| B1 | exit peer 表 | ✓ | exit `peer: + dev=7dc61647 pub=6e18bb87 ip=100.64.59.106 n=1/32`（与手机身份行逐字段一致）；后 `peer: ~ dev=7dc61647 refresh (idle=0s) n=1/32` |
| B1 | RREG 注册 | ✓ | 手机 14:11:04 `RREG 注册刷新 → 192.168.3.12:42670（dev=7dc61647，中继=false）` |
| B2 | L3 真负载（系统侧） | ✓ | `grep vpn-tun /proc/net/dev`：14:11:43 `TX=520B` → 14:12:05（开 baidu 15s 后）`RX=4048754B(3544包) TX=227542B(2534包)` → 14:13:17（wikipedia 后）`RX=4353860B TX=244728B`——双向增长 |
| B2 | L3 真负载（手机核 stats） | ✓ | 手机 14:12:04 `stats: fdReadBytes=227542B fdWriteBytes=4037666B ｜ pf=0/0` |
| B2 | 出口 TCP 过境拦截 | ✓ | exit 多条 `intercept: tcp transit 198.18.15.135:443 ← 100.64.73.151:48582（dialok）` 与成对 `… 关闭`（baidu/wikipedia 全站 HTTPS；transit 源=TUN 派生地址 100.64.73.151，与 peer 表双 /32 口径一致） |
| B2 | DNS 代答 | ✓ | exit `dns: q=46 qtcp=0 resp=23 filter=23 trunc=0 fallback=0 fail=0 drop=0 malformed=1 aaaa-mixed=0`（14:13:17 轮；首个 baidu 轮 q=0 系浏览器缓存了昨日同 Mac Surge 的假 IP，非代答缺失——wikipedia 新城名立即 q 增长） |
| B2 | 页面真加载 | ✓ | 浏览器 dumpLayout 见 Wikipedia 页面文案（`Wikipedia 自由的百科全书`、`中文 1,558,000+ 条目`…）；浏览器为唯一合规第三方流量源 |
| B3 | files 浏览 | ✓ | App 14:13:43 `文件管理：发起桥请求（重建=0 sock=有 auth=有）` → `文件管理：已连接主机 主机 192.168.3.12（经 VPN 通道的文件桥）` → `文件管理：files:flow：第 1 击成功`；页面列出 `r65-big-10mb.bin / 10.0 MB · 2026-10-04 14:13`；exit `intercept: tcp exempt 100.64.255.1:7802 ← 100.64.59.106:25857（dialok）` |
| B3 | files 下载 ≥10MB | ✓ | 长按→「下载」，10MB 取件 <8s 完成（14:14:23 模态→14:14:31 保存选择器）；二轮出现系统对话框 `已有同名文件，是否替换？`（= 首轮已落 Download 的系统侧证据）→「替换」完成 |
| B3 | files 上传 | ✗ | 3 次尝试（选择 Kuromis.yaml→完成）后 App 无任何动作：无新 7802 腿（exit 侧无对应 exempt 行）、目标目录无文件、AppLog 无上传/失败行——失败点在**连接发起之前**（App 侧 picker→copy→beginUpload 链路静默断，tier 仓只读未深究；非 Rust 后端问题） |
| B4 | term 新建会话 | ✓ | exit `intercept: tcp exempt 100.64.255.1:7724 ← 100.64.59.106:21158（dialok）`；`term: 新建会话 e4eb92bb（pid=90468 49x34 shell=/bin/zsh）`；`term: 会话 e4eb92bb 腿接入（kind=app 49x34 id=04739516f1de6013 首腿=true）n=1/8`；App 14:20:04 `[term] state → Connected (gen=2, 距受理 999ms)`；14:20:03 `[term] reveal: gen=2（suppress 解除：首屏已上屏 92ms，距受理 93ms，传输=surface）` |
| B4 | term 版本门 | ✓ 未拒 | App 14:20:04 `[term] 会话状态 → 空闲（STATE 帧 stateV2）`；exit `term: 会话 e4eb92bb 状态 shell/idle（fg=90468 procs=579 依据=shell-idle）`——stateV2/检测规则 22 份全兼容，无版本差 |
| B4 | term 输入+回显+真执行 | ✓ | App surface 计数 帧 2→34→87、下行 268B→3382B→8985B；**决定性**：手机键入 `touch /tmp/homeway-rs-e2e-files/r65-term-marker` 回车后，Mac 上 `r65-term-marker` 于 14:21 真实创建——命令在 Rust 出口 zsh 内执行 |
| B4 | term 退出自灭 | ✓ | 手机键入 `exit` 回车后：exit `term: 会话 e4eb92bb 腿断开（kind=app 原因=finish）｜快照=1 差分=87 降级=0 背压=0 队列溢出=0 编码失败=0 分片=89 下行=8985B FETCH 命中=0 落空=0`；App 14:21:37 `[term] state → Disconnected (gen=2, 距受理 94029ms)` |
| B5 | 测速一轮（Rust 出口） | ✓ | 首页卡片显示 `↑24MB/s ↓947KB/s`（14:23:05）；exit 7803 腿 dialok（日志 18 条命中）；结果不对称另记 P1-2 |
| B5 | 测速对照轮（生产 Go 出口） | ✓ | 恢复现役出口后复测：`↑28MB/s ↓19MB/s`（14:37）——同机 A/B 判定 P1-2 归属 Rust 侧 |
| C1 | 中继腿：手机经中继建立 | ✓ | 手机 14:30:49（dead-direct token 注入后）：`MIRROR 直连窗口 2s 内无响应 → 解锁中继候选` → `赛跑结算：胜出 中继 192.168.3.12:42770` → `路径确立：中继 192.168.3.12:42770（首个回包来源）` → `⚠️ 链路走了中继（本应直连，属需排查的 bug）…` → **warmup 在中继上完成**（`warmup pong: 就绪` 在中继确立之后）；14:32:39 二次复现 |
| C1 | 中继腿：relay 侧计数 | ✓ | relay 14:30:49.585 `中继：客户端 192.168.3.66:52838 起会话 #2（拨腿模式）→ 后端 ead4b8846ba452bd（数据口 0.0.0.0:64025）`；14:32:39.650 `…起会话 #3…`；统计行 `中继统计：注册腿 2（累计成功 10，伪造 0）｜分配腿 1（累计 2，回收 1）｜转发 上 5 / 下 3 包｜丢弃 0`（自 上1/下0 增长） |
| C1 | 中继→直连升级（hint 打洞） | ✓（设计行为） | 手机 14:32:39 `中继 hint 192.168.3.12:42672 → 重新武装候选赛跑，打一发握手兼打洞` → `赛跑结算：胜出 直连 192.168.3.12:42672` → `link: via=direct ep=192.168.3.12:42672 rtt=13ms` |
| C1 | 长驻 via=relay 快照 | ✗（环境不可达，见 P2-3） | 同 LAN 拓扑下中继 hint 取自腿源地址（真实 LAN 地址），0.0.0.0 监听必被 1s 内升级直连 |
| C2 | 恢复：kill→自动回连 | ✓ | 14:23:57 kill exit1；14:24:30 重启（95569）。手机阶梯完整：14:24:14 `RECOVER R1 重握手（原因=巡检失败）：补注册 + 丢会话（保采纳）` → 14:24:27 `RECOVER R2 换源…` → 14:24:40 `RECOVER R3 重赛跑…` → 14:24:45 `RECOVER 恢复于 R3 重赛跑（原因=巡检失败，起跑=R1 重握手，耗时 34.202s）` + `巡检失败后阶梯已恢复（不用等 3 连败）`；R1 未命中系 exit 尚未就绪（kill 后 33s 才重启），非阶梯缺陷；exit 侧 `peer: + dev=7dc61647 pub=6e18bb87 ip=100.64.59.106 n=1/32` 回归 + `中继：收到对端地址线索 192.168.3.66:52742 → 盲打 3 包` |

## 2. 问题分级清单

**P1（Rust 侧待修）**

1. **DNS 代答自检误报 + malformed 常驻**：每次启动必打 `⚠️ dns 代答自验证未通过（自验证无应答（查询处理失败））——上游此刻不可达…期间查询按 SERVFAIL/兜底处理`，
   且 `malformed=1` 恒存（两台 exit 均复现）。但真实客户端查询完全正常（q=46/resp=23/filter=23、
   假 IP 应答、页面可开）——疑自检查询构造/解析自身即那 1 条 malformed。影响：启动窗口内真实查询
   可能被按 SERVFAIL 兜底 + 运维判据被污染。归属：Rust 修（dns 自检路径）。
2. **下行吞吐 ~1MB/s，同机 A/B 铁证 Rust 侧**：对 Rust 出口测速 `↑24MB/s ↓947KB/s`；**同一手机
   同一 Wi-Fi 15 分钟后对生产 Go 出口（41641）复测 `↑28MB/s ↓19MB/s`**——上行相当、下行差 20 倍
   ⇒ 瓶颈在 Rust 出口下行发送路径，非手机/Wi-Fi。交叉印证：经 Rust 出口的浏览器整页 ~4MB/15s、
   files 10MB 下载 <8s（≈1.3MB/s），三口径一致 ≈1MB/s 下行。（PERF-AB 的 down 0.79× 是 CLI 对
   CLI 口径，未暴露此问题——CLI 客户端与真机 App 消费路径不同。）归属：Rust 修（下行发送路径：
   分块大小/flush 策略/sendmmsg 缺位、或 WG downlink 拥塞窗），建议 R7 前排。

**P2（观察项/环境限制/App 侧，不阻塞）**

3. **长驻 via=relay 不可测**：serve `--listen` 只收端口（`127.0.0.1:42671` 静默回退默认 41641、占产线
   邻位后自动退让 41642——顺带暴露 CLI 参数校验弱）；LAN 自检端点 + 中继 hint（取腿源地址）使同
   LAN 手机必然升级直连。中继腿本身已证通（会话 #2/#3 + 转发计数）。归属：Rust 补 loopback 绑定或
   hint 抑制开关后可补测；亦可将 `--listen` 非法值改为报错退出。
4. **files 上传 UI 自动化 3 次未成**：App 侧静默（无连接发起、无日志行）。归属：App（tier 仓）或
   自动化限制，与 Rust 后端无关；R7 真机复测清单保留「上传」一项。
5. **files 下载保存的 AppLog「已保存」行未出现**（保存成功由系统同名文件对话框侧证）——App 侧
   日志点缺失/迟到。归属：App（tier 仓）。
6. **`intercept: tcp exempt 100.64.255.1:1 ← … 拨号失败：连接失败` 周期刷屏**：手机核自连探测拨
   隧道 IP:1，Rust 出口照实重拨本机 1 端口失败并记行。功能无害（探测判活走 UDP 参照点），但日志
   噪音大、易误导读日志的人。归属：Rust 侧可对豁免重拨失败做归类降噪（或确认 Go 出口同口径后保持）。
7. **方法论备注**：`aa start --ps host_token` 注入不自动建连（需 UI 开关）；本会话 Read 截图通道
   无法回显，UI 判断全走 dumpLayout 文本通道——对后续会话是可复用经验，非缺陷。

## 3. 恢复与清场确认

- **恢复现役出口**（14:33:44 注入 `~/bin/homeway serve token` 只读取得的 token → 开关重建）：
  手机 14:33:53 `身份：复用（dev=7dc61647 pub=f4f522f0）` / `路径确立：直连 192.168.3.12:41641` /
  `warmup pong: 就绪（判据=wg）` / `attached（数据面已接管 fd=90，L3 直通）` /
  `link: via=direct ep=192.168.3.12:41641 rtt=10ms` / `RREG 注册刷新 → 192.168.3.12:41641`；
  App `界面状态 → connected（状态通道已连，系统VPN=up…）`（14:33:53.88）。现役出口进程/launchd
  全程未动（仅只读取 token 一次）。
- **清场**：kill exit1（95569）、exit2（7321）、relay（67122）及中间迭代实例；`pgrep -fl homeway`
  复核仅剩 `/Users/zhaozhe/bin/homeway`（PID 52279，现役）。仓库根前会话残留的未跟踪 `cache/`、
  `serve/` 已确认无进程占用后删除（`git status` 干净）。/tmp 测试 state（homeway-rs-e2e-*）留作
  证据（含 marker 文件），系统重启自清。
- 真机铁律遵守：全程无锁屏/息屏类操作；hdc 一律 `-t`；tier 仓只读；无 push。

## 4. 结论

**现役真机 App（Go 核 + C++ term surface + FilesClient + 测速引擎）与 Rust 后端（homeway-rs
serve/relay）的 E2E 主链路全部打通**：token 注入→建连（身份/双地址/RREG）→L3 直通真负载
（浏览器 + DNS 代答 + 过境拦截 dialok）→files（浏览/2×10MB 下载；上传卡在 App 侧）→term
（新建/输入回显/命令真执行/exit 自灭，版本门零阻力、帧统计零降级）→测速→中继腿（拨腿模式
会话 + 转发计数 + hint 升级直连）→故障自愈（R1→R2→R3 阶梯 34.2s 全自动恢复）→生产 token
无损切回。协议兼容性面（WG/注册 v2/intercept/files/term stateV2/speedtest/DNS）**未发现任何
不兼容或拒识**。遗留两笔 Rust 侧 P1（DNS 自检误报、下行吞吐 20× 劣于 Go 出口——同机 A/B 实证）
与若干 P2 观察项，均已给出归属；不影响 R7 以 Rust 核替换 App 核的推进判断——但**下行吞吐一项
必须在 R7 前或 R7 真机全量判据里优先排查**（用户可感知的浏览/下载体验差距）。
