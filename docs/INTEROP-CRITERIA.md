# INTEROP-CRITERIA — 互操作判据行清单（R0.3）

> Rust 版对齐验收的判据行真源。**行文字节级同串**（含全半角标点、空格）才判绿；
> 改动措辞若**未按「判据变更记录」节登记** = 静默破坏对齐（同款教训见 tier AGENTS 坑内
> `link: via=…` 注记）。**显式登记后允许演进**——判据行不再是不可变更的冻结物。
>
> - **出处** = baseline 克隆（`baseline/homeway`，基线 `d4148f6`）内路径，相对仓根。
> - **真实样例** = 2026-10-02 本地烟囱实测采集：`tools/local-exit.sh start 1`（出口 UDP
>   127.0.0.1:42641）+ `client-start/client-add 1`（服务会话形态客户端）+ `speedtest -host local1`。
>   日志面：出口 `--verbose` stdout（`/tmp/homeway-rs-exit-1/stdout.log`）与摘要
>   `<state>/cache/events.log`；客户端服务会话 `cache/client.log`。
> - 无样例行 = 需要 APP 核形态或故障注入才触发（R1/R2 补采），出处已核。
> - 判据行政策与变更登记 = 文末「判据变更记录」节（**任何判据行变更必须先在此登记再改**）。

## 出口侧（serve）

| # | 判据行（模板） | 出处 | 真实样例（2026-10-02，截断） |
|---|---|---|---|
| E1 | `serve 就绪：wg=:%d（配置端口；被占用会自动退让）tunnel=%v files=%d term=%d speedtest=%d dns=%v tokens=%d key=%x…` | `internal/server/serve.go:552` | `serve 就绪：wg=:42641（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true tokens=1 key=7bab5077e7ea…` |
| E2 | `凭证：现有凭证全部不可用（首启或已吊销）——已铸出新凭证（客户端需重新粘贴新 token）`（首启铸出） | `internal/server/serve.go:226` | 同串（exit stdout.log） |
| E3 | `客户端 token（粘进 App 的「添加主机」即可；%d 个端点）：%s` | `internal/server/publicendpoint.go:370` | `客户端 token（粘进 App 的「添加主机」即可；2 个端点）：hmw1e6tQd…`（端点变化时 `publicendpoint.go:374` 打「端点已变化」变体） |
| E4 | `dns 代答就绪：tunnel=%v:53（UDP+TCP）resolve=%v:%d（TCP）upstream=%s` | `internal/server/serve.go:310` | `dns 代答就绪：tunnel=100.64.255.1:53（UDP+TCP）resolve=100.64.255.1:5300（TCP）upstream=198.18…` |
| E5 | `intercept: 过境拦截就绪（隧道IP %v；豁免=转投本机同端口；TCP 并发上限 %d）` | `pkg/intercept/intercept.go:187` | `intercept: 过境拦截就绪（隧道IP 100.64.255.1；豁免=转投本机同端口；TCP 并发上限 1024）` |
| E6 | `peer 表：设备表就绪（cap=%d，ttl=%v，grace=%v；按 devTag 记账/刷新/轮换）`（debug 级） | `internal/server/serve.go:394` | `peer 表：设备表就绪（cap=32，ttl=168h0m0s，grace=10m0s；按 devTag 记账/刷新/轮换）` |
| E7 | `peer: + dev=%s pub=%s ip=%v n=%d/%d` | `pkg/servercore/peers.go:445` | `peer: + dev=37a8115c pub=d5cae8cf ip=100.64.56.143 n=1/32` |
| E8 | `peer: ~ dev=%s refresh (idle=%s) n=%d/%d`（重连刷新）/ `peer: ~ dev=%s rotate pub=%s→%s ip=%v→%v …`（换钥轮换） | `pkg/servercore/peers.go:387` / `:415` | refresh 采样例：`peer: ~ dev=37a8115c refresh (idle=0s) n=1/32` |
| E9 | `peer: - dev=%s reason=ttl (idle=%s) …` / `reason=stale` / `peer: ! reject reason=revoked|no-token|verify` | `pkg/servercore/peers.go:466` / `:509` / `:369-373` | **R2 已采（ttl 形态）**：`--peer-ttl 15s` 注入 + 客户端静默后 GC 10min 周期回收，实采行 `peer: - dev=6280f0b6 reason=ttl (idle=7m47s) n=1/32`、`peer: - dev=3e16ea16 reason=ttl (idle=2m53s) n=0/32`（2026-10-02 17:31）；stale/revoked 形态归 R3（表满/吊销注入） |
| E10 | `intercept: tcp %s %v ← %v（dialok）`（kind=transit|exempt；**真凭据判据**） | `pkg/intercept/intercept.go:378` | speedtest 腿为 exempt 形态：`intercept: tcp exempt 127.0.0.1:7803 ← 100.64.56.143:36862（dialok）`；transit 形态 **R1 已采**（2026-10-02，Rust 客户端 `--dial 192.168.3.12:9999`）：`intercept: tcp transit 192.168.3.12:9999 ← 100.64.213.172:34321（dialok）` |
| E11 | `intercept: tcp %s %v ← %v 关闭` | `pkg/intercept/intercept.go:389` | `intercept: tcp exempt 127.0.0.1:7803 ← 100.64.56.143:28709 关闭` |
| E12 | `udp intercept: 会话 #%d %s 建立（%v ← %v）` / `… 关闭（%v ← %v）` | `pkg/intercept/intercept.go:560` / `:641` | （需 UDP 应用流量；R1 客户端无 UDP 拨号面——**归 R2**） |
| E13 | `speedtest: 会话 #%d role=%s warmup=%s window=%s`（受理）/ `… role=recv bytes=%d（含预热 %d）用时=%dms` / `… role=send bytes=%d（含预热 %d）用时=%dms`（结算） | `pkg/speedtest/speedtest.go:209` / `:238` / `:294` | `speedtest: 会话 #8 role=send bytes=203355105（含预热 45677895）用时=10183ms` |
| E14 | `files 就绪：root=%s (rw) sock=%s（隧道IP:%d 经拦截层转投）` | `internal/server/serve.go:467` | `files 就绪：root=/Users/zhaozhe (rw) sock=/tmp/homeway-rs-exit-1/files.sock（隧道IP:7802 经拦截层转投）` |
| E15 | `term: 检测规则已加载 %d 份（覆盖目录 %s）` | `pkg/term/service.go:378` | `term: 检测规则已加载 22 份（覆盖目录 /tmp/homeway-rs-exit-1/agent-detection）` |
| E16 | `# Serving terminal sessions on sock=%s (shell=%s, history=%s, features=%s, vt=%s)` | `internal/server/serve.go:508` | `# Serving terminal sessions on sock=/tmp/homeway-rs-exit-1/term.sock (shell=/bin/zsh, history=1…` |
| E17 | `speedtest 就绪：sock=%s（隧道IP:%d 经拦截层转投；内存收发不落盘）` | `internal/server/serve.go:537` | （烟囱日志在档，措辞出处已核） |
| E18 | `凭证台账：%d 行记录 / %d 枚在用凭证（其中 %d 行已吊销；吊销即时对新注册生效）`（吊销/续期语义判据） | `internal/server/serve.go:239` | `凭证台账：1 行记录 / 1 枚在用凭证（其中 0 行已吊销；吊销即时对新注册生效）` |
| E19 | `后端身份：标签 %x ｜公钥 %x…` | `internal/server/role.go:97` | （烟囱日志在档） |
| E20 | `公网端点：已按 **--public-endpoint 配置**公布 %v（跳过 UPnP/STUN 推断；写进 %s）`（显式端点路径；同族另有 STUN/UPnP 推断各形态行 `publicendpoint.go:187-218`） | `internal/server/publicendpoint.go:129` | `公网端点：已按 **--public-endpoint 配置**公布 [127.0.0.1:42642]（跳过 UPnP/STUN 推断；写进 public_endpoint.txt）` |
| E21 | `绑卡：自动挑到 %s（%s）` / `绑卡看护：网卡 %s 探针失败…` / `…连续探不通，重新挑卡` | `internal/server/serve.go:263` / `internal/server/bindwatch.go:148` / `:152` | `绑卡：自动挑到 en0（index=6 up addrs=[192.168.3.12/24]）` |
| E22 | `dns: q=%d qtcp=%d resp=%d filter=%d trunc=%d fallback=%d fail=%d drop=%d malformed=%d aaaa-mixed=%d`（DNS 代答计数行，debug 级周期输出；与 E10 同族计数语义） | `pkg/dns/server.go:162`（`StatsLine`，判据行注记 `:159-161`） | **R1 已采**（2026-10-02，周期行）：`dns: q=0 qtcp=0 resp=1 filter=0 trunc=0 fallback=0 fail=0 drop=0 malformed=0 aaaa-mixed=0` |
| E23 | `入站新源：%v（%s，%d 字节）`（源学习/漫游跟随证据，debug 级） | `pkg/servercore/bind.go:799` | **R1 已采**（新客户端首包即「新源」；漫游换源场景仍归 R2）：`入站新源：192.168.3.12:54242（容器数据，222 字节）` |

## 客户端侧（core / 服务会话）

| # | 判据行（模板） | 出处 | 真实样例 |
|---|---|---|---|
| C1 | `身份：新建（dev=%s pub=%s，目录 %s）` / `身份：复用（dev=%s pub=%s）` | `clientcore/hostsession/session.go:197` / `:199` | `服务会话: 身份：新建（dev=37a8115c pub=d5cae8cf，目录 /tmp/homeway-rs-client-1/client/identity）` |
| C2 | `wgcore: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 B 拨隧道 IP）` | `clientcore/internal/wgcore/core.go:143` | 同串（client.log） |
| C3 | `新栈会话已建立（token 端点 %d 个，后端隧道地址 %v）` | `clientcore/hostsession/session.go:237` | `新栈会话已建立（token 端点 2 个，后端隧道地址 100.64.56.143）` |
| C4 | `MIRROR 镜像包#%d → %d 候选（直连优先：本次直连 %d / 中继 %d；本行每轮限 3 条）` | `clientcore/internal/wtransport/bind.go:697` | `MIRROR 镜像包#1 → 2 候选（直连优先：本次直连 2 / 中继 0；本行每轮限 3 条）` |
| C5 | `赛跑结算：胜出 %s %v（镜像 %d 包，耗时 %v）；响应过=%v；未响应=%v` | `clientcore/internal/wtransport/bind.go:538` | `赛跑结算：胜出 直连 127.0.0.1:42641（镜像 2 包，耗时 5.025s）；响应过=127.0.0.1:42641；未响应=LAN 192.168.3.12:42641` |
| C6 | `路径确立：%s %v（首个回包来源）` | `clientcore/internal/wtransport/bind.go:550` | `路径确立：直连 127.0.0.1:42641（首个回包来源）` |
| C7 | `暖机就绪（出口可达，rtt=%dms）`（服务会话形态） | `clientcore/hostsession/service.go:405` | `暖机就绪（出口可达，rtt=5029ms）` |
| C8 | `warmup pong: 就绪（判据=%s）`（**APP 核形态**暖机判据，判据=wg） | `clientcore/cmd/clientcore/tunmode.go:751` | **R1 已采**（Rust 客户端按判据移植同串输出，一次会话一次）：`warmup pong: 就绪（判据=wg）` |
| C9 | `attached（数据面已接管 fd=%d，L3 直通）`（**APP 核形态**数据面判据） | `clientcore/cmd/clientcore/tunmode.go:822` | （需 TUN fd，R7 补采） |
| C10 | `link: via=%s ep=%s rtt=%dms（新栈状态快照）` / `link: via=%s ep=%s rtt=%dms（服务会话巡检）` | `clientcore/cmd/clientcore/tunmode.go:982` / `clientcore/hostsession/service.go:640` | 巡检形态：`服务会话: link: via=direct ep=192.168.3.12:42641 rtt=1ms（服务会话巡检）`（**R2 前缀订正**：服务会话 logger = WithPrefix("服务会话: ")，全部 hostsession 族行带前缀——R1 样例的无前缀形态系当时采样口径遗漏）；快照形态需 APP 核（R7 补采） |
| C11 | `RECOVER R1 重握手（原因=%s）：补注册 + 丢会话（保采纳）` / `RECOVER R2 换源（原因=%s）：换本地 socket（保采纳）` / `RECOVER R3 重赛跑（原因=%s）：清采纳，学习缓存候选兜底` / `RECOVER 恢复于 %s（原因=%s，起跑=%s，耗时 %v）` / `RECOVER 走完 R1→R3 仍未恢复（…）—— 交上层升级` | `clientcore/hostsession/recover.go:181` / `:188` / `:198` / `:201` / `:206`（同族：零档位恢复 `:151/:155`、动作生效复探 `:151`、本地动作失败 `:223`、R1 补注册未发出 `:168`、丢会话失败 `:175`） | **R2 已采全族**（故障注入四档实测：出口重启 R1 命中 3.126s / poison-socket R2 命中 18.4s / 换端口 R3 命中 38.7s / 停机最坏 39.8s 走完——行样例见 docs/reviews/R2.md）；档位/节拍/时间窗真源 = `tier:docs/agents/connection-lifecycle.md` |
| C12 | `启动（无 TUN 服务会话）`（服务会话启动形态） | `clientcore/hostsession/service.go:246` | `服务会话: 启动（无 TUN 服务会话）` |
| C13 | `候选端点（%d 条，标记·学习=来自巡检缓存/中继 hint）：%s` | `clientcore/hostsession/session.go:249` | `候选端点（2 条，标记·学习=来自巡检缓存/中继 hint）：192.168.3.12:42641(LAN)、127.0.0.1:42641(LAN)` |
| C14 | `出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v`（参照点探测判据；失败形态 `session.go:260`） | `clientcore/hostsession/session.go:280` | `出口能力：构建 homewayd-dev ｜ 默认路径 UDP：DNS:53 可用 / 通用（非 53）可用 / 实测 还没实测样本（这台出口还没转发过 UDP） ｜ 探测往返 0s` |
| C15 | `RREG 注册刷新 → %v（dev=%s，中继=%v）`（60s 巡检注册刷新） | `clientcore/internal/wtransport/bind.go:933` | `服务会话: RREG 注册刷新 → 192.168.3.12:42641（dev=9b8bf127，中继=false）`（失败形态 `:930`） |
| C16 | `就绪（会话在位%s）`（服务会话形态核心**就绪**判据；`local-exit.sh client-add` 的等待点） | `clientcore/hostsession/service.go:423` | `服务会话: 就绪（会话在位，无桥直通）` |
| C17 | `已收工（state=%s）`（会话收工） | `clientcore/hostsession/service.go:856` | **R1 已采**（Go 客户端 client-stop）：`服务会话: 已收工（state=idle）`（2026-10-02） |

## 命令面结论行（host add 三档结论）

| # | 行 | 出处 | 真实样例 |
|---|---|---|---|
| X1 | `已添加主机 %s（%s）——直连可达 ep=%s rtt=%dms` | `internal/daemon/host_cli.go:142`（add 验证三档结论之一） | `已添加主机 local1（7bab5077…）——直连可达 ep=127.0.0.1:42641 rtt=0ms` |

## 采集与复跑

```bash
tools/local-exit.sh start 1          # 出口：等「serve 就绪」+ 打 token
tools/local-exit.sh client-start 1   # 客户端统一进程（serve/relay 双关，绝不撞 41641）
tools/local-exit.sh client-add 1     # host add → 出口 peer: + / 客户端 C1-C7
bin/homeway-go speedtest --state /tmp/homeway-rs-client-1 -host local1   # E10/E11/E13
tools/local-exit.sh status 1 / client-stop 1 / stop 1
```

数据面旁证（非判据行但同源可查）：`host status` 流量计数（本次烟囱 收 858.3MB / 发 951.0MB）。

## Rust 出口侧实采（R3-3f，2026-10-02；实例 = `tools/local-rust-exit.sh start 1`，端口 42651）

> 同串判定（与 Go 出口逐字对照）已全量实测；以下为 Rust exit 的实采行（摘要 stdout）：

| # | 实控行（Rust exit） |
|---|---|
| E1 | `serve 就绪：wg=:42651（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true tokens=1 key=82f5ccdfb570…` |
| E2 | `凭证：现有凭证全部不可用（首启或已吊销）——已铸出新凭证（客户端需重新粘贴新 token）` |
| E3 | `客户端 token（粘进 App 的「添加主机」即可；2 个端点）：hmw1gvXM37…`＋`端点：192.168.3.12:42651（内网）、127.0.0.1:42651（公网）` |
| E4 | `dns 代答就绪：tunnel=100.64.255.1:53（UDP+TCP）resolve=100.64.255.1:5300（TCP）upstream=198.18.0.2` |
| E5 | `intercept: 过境拦截就绪（隧道IP 100.64.255.1；豁免=转投本机同端口；TCP 并发上限 1024）` |
| E6 | `peer 表：设备表就绪（cap=32，ttl=168h0m0s，grace=10m0s；按 devTag 记账/刷新/轮换）`（`--peer-ttl 15s` 注入时同位打 `ttl=15s`） |
| E7 | `peer: + dev=f1b96b23 pub=57f34f57 ip=100.64.90.95 n=2/32`（Rust 客户端为第二设备——多 peer 混跑） |
| E8 | `peer: ~ dev=f38f48d7 refresh (idle=1m0s) n=2/32` |
| E9 | `peer: - dev=4b7579ee reason=ttl (idle=20s) n=0/32`（`--peer-ttl 15s` 注入 + GC 周期）；revoked 形态：`peer: ! reject reason=revoked（原因=revoked，累计 1）`＋首大声 `⚠️ 注册被拒（原因=revoked，累计 1）——该凭证已被吊销。…`（stale 形态 10min 宽限不可注入，table.rs 单测钉行） |
| E10 | transit：`intercept: tcp transit 192.168.3.12:19999 ← 100.64.229.69:47321（dialok）`；exempt：`intercept: tcp exempt 100.64.255.1:7803 ← 100.64.179.16:37681（dialok）` |
| E11 | `intercept: tcp transit 192.168.3.12:19999 ← 100.64.229.69:47321 关闭` |
| E12 | `udp intercept: 会话 #1 dns 建立（8.8.8.8:53 ← 100.64.132.135:46440）`（dnstest leg 模式采样） |
| E13 | `speedtest: 会话 #1 role=recv warmup=2s window=10s` / `speedtest: 会话 #1 role=send bytes=149616405（含预热 28835400）用时=10024ms`（Go 客户端） |
| E14 | `files 就绪：root=/Users/zhaozhe (rw) sock=/tmp/homeway-rs-rustexit-1/serve/files.sock（隧道IP:7802 经拦截层转投）` |
| E15 | `term: 检测规则已加载 22 份（覆盖目录 /tmp/homeway-rs-rustexit-1/serve/agent-detection）`（6g 实采） |
| E16 | `# Serving terminal sessions on sock=/tmp/homeway-rs-rustexit-1/serve/term.sock (shell=/bin/zsh, history=1024KiB, features=list,replay,modes,agent,title,surface, vt=on)`（6g 实采；Go 出口同串〔vt=on〕） |
| E15a | `term 服务被 HOMEWAY_TERM=off 关闭`（关闭面实采：serve 目录无 term.sock） |
| E16a | `term: 新建会话 p6（pid=81468 80x24 shell=/bin/zsh）`（首接入创建）/ `term: 创建会话 p6（不接入，默认尺寸）`（`new -d`） |
| E16b | `term: 会话 p6 腿接入（kind=host 80x24 id=host-56fdf75f 首腿=true）n=1/8`（多腿注册序判据；surface 腿 kind=app） |
| E16c | `term: 会话 p4 状态 codex/blocked（fg=80526 procs=580 依据=screen:rule=osc_title_blocked,ver=2026.09.22.1,src=embedded）`——检测三态同串实采：`codex/working（依据=output）`、`codex/idle（依据=screen:rule=osc_title_idle,…）`、直报 `codex/blocked（依据=osc21337:blocked）`、回落 `codex/idle（依据=agent-idle-fallback）` |
| E16d | `term: 关闭会话 p6（pid=81468）`（KILL）/ `term: 会话 p6 腿断开（kind=host 原因=finish）`（收尾摘腿；surface 腿带计数尾巴 `｜快照=N 差分=N 降级=…`） |
| E17 | `speedtest 就绪：sock=/tmp/homeway-rs-rustexit-1/serve/speedtest.sock（隧道IP:7803 经拦截层转投；内存收发不落盘）` |
| E18 | `凭证台账：1 行记录 / 1 枚在用凭证（其中 0 行已吊销；吊销即时对新注册生效）` |
| E19 | `后端身份：标签 b0acc6fbce193fe4 ｜公钥 82f5ccdfb570…` |
| E20 | `公网端点：已按 **--public-endpoint 配置**公布 [127.0.0.1:42651]（跳过 UPnP/STUN 推断；写进 public_endpoint.txt）`；同 socket STUN 真观测：`STUN：监听 socket（本地 42697）在 162.159.207.1:3478 眼里是 203.175.12.191:29397`＋暂不公布形态 `公网端点：暂不公布 —— STUN 观测到 203.175.12.191:29397，但外部端口与监听/UPnP 不一致，说明路由器改写端口或有代理抢路由` |
| E21 | 绑卡 + 看护族全量真网实采（B0-1，2026-10-05 本机 auto 模式）：`绑卡：自动挑到 en0（index=6 up addrs=[192.168.3.12/24]）` → `绑卡看护：WG socket 钉在 en0（index=6 up addrs=[192.168.3.12/24]）`；注入（`HOMEWAY_BINDWATCH_PROBE=203.0.113.1:53` 死地址模拟「当前卡出网退化」）：`绑卡看护：网卡 en0 探针失败 1/2（…）` → `…探针失败 2/2（…）` → `绑卡看护：网卡 en0 连续探不通，重新挑卡`（重挑回同卡同指纹 = 维持现状无行——Go 同义）。**单测覆盖形态**（真机不可达/窗口极窄）：无卡 `绑卡看护：挑不到可用物理网卡（…）—— 本轮不绑，走系统默认路由`（需启动挑到卡后 5s 内卡消失）、指纹变化 `…%s → %s`（看护按 index 每拍 live 重枚举——r1-M2 整改后可达）、重钉失败/暂时挑不到——bindwatch.rs 六测试逐字钉死 |
| E20a | v6 观测/公布族真网实采（B0-1，2026-10-05）：`公网端点：IPv6 路径可用（STUN 看到 [2408:8207:2518:2550:383e:8734:deb7:9c81]:42680），公布 [2408:8207:2518:2550:383e:8734:deb7:9c81]:42680` ＋ v4/v6 双公布形态 `公网端点：已公布 [114.242.60.128:42680 [2408:8207:2518:2550:383e:8734:deb7:9c81]:42680]（写进 public_endpoint.txt；下次签发 token 会带上它）` ＋ token 四端点 `端点：192.168.3.12:42680（内网）、114.242.60.128:42680（公网）、[2408:…]:42680（公网）、127.0.0.1:42741（中继）`；v6 观测失败轮 `公网端点：IPv6 不可用（<原因：解析失败/无应答>），本轮只公布 IPv4`（Go 是 %v 带具体 err——Rust 按断点分两类）；未配置 `公网端点：未配置 --stun6（需要有 AAAA 的 STUN 服务器），跳过 IPv6 公布` |
| E24 | 统一进程期望态装配族（B0-1，2026-10-05，零参形态 = `homeway-cli` 无子命令）：`serve: 按期望态装配（config serve.enabled=true）` / `serve: 期望停用（config serve.enabled=false）——不装配` / `relay: 期望停用（config relay.enabled=false）——不装配` / `client: 角色本期未装配（client 恒开语义与 hosts.json 归 B0-2 daemon 批）` / `control: 控制面本期未装配（control.sock/stream.open/CLI 族归 B0-2）` / `homeway: 统一进程就绪（state=…，serve=true relay=false，client/control 留桩归 B0-2，version=…）` / 收工 `homeway: 收到信号，收工`；单实例锁拒起 `homeway 已在运行（pid N，形态 unified，state=…）——拒绝二次启动`；config 生成公告 `config.toml 缺失——已生成默认（serve.enabled=true）` |
| E22 | `dns: q=1 qtcp=1 resp=3 filter=0 trunc=0 fallback=0 fail=0 drop=0 malformed=1 aaaa-mixed=0`（dnstest 三面各一查后） |
| E23 | `入站新源：192.168.3.12:62535（参照点探测，213 字节）` / `入站新源：192.168.3.12:51735（容器数据，222 字节）`（Go 客户端首包容器形态） |
| — | udpcap：`UDP 默认路径：DNS:53 可用（往返 1ms）；通用 UDP（STUN:3478）有可校验应答（往返 782ms，映射 203.175.12.191:22963）；实测 本轮没有转发的 UDP 会话（探测应答 flags=0x0b 也会这么报）`（caps 位经探测应答回报——客户端 C14 已见「DNS:53 可用 / 通用（非 53）可用」） |

**吞吐 A/B（同一时刻）**：Rust 客户端 ↔ Rust exit speedtest down 250-257Mbps / up 398-408Mbps（偏差 1.83%）；
Rust 客户端 ↔ Go exit 同时刻 down 406 / up 367——down 为 Go 的 ~62%（±50% 界内）；Go 客户端 ↔ Rust exit
down 210 / up 460。files 100MB：Rust 客户端上传 2.6s / 下载 41.7s→缓冲修复后 2.6s，**对账偏差 0**（sha256
双侧一致）；Go 客户端 put/get 100MB 经 Rust exit 同样偏差 0。

## B0-1 手机 v6 直连真机实采（2026-10-05，FMR0224116011480 × Rust 核 e1fbd181309d-rust）

形态：`token --v6-only`（v4 直连端点全改死、v6 与中继保留——同 Wi-Fi 下 LAN v4 赛跑恒胜会掩盖
v6 路径）+ Rust 统一进程出口 42680（upnp=true 同号映射成立 ⇒ v4 主体公布成立 ⇒ v6 观测跑）。

- 出口侧：`入站新源：[2408:8207:2518:2550:b07b:6e55:760b:dd2]:39749（容器数据，222 字节）`
  （手机 v6 的 [reg][init] 首包）→ `peer: + dev=aca645d3 pub=b799d11c ip=100.64.161.247 n=1/32`
  → `入站新源：[2408:…b07b…]:39749（腿帧数据，86 字节）`（v6 源 WG 数据帧——v6 直连数据面）；
- L3 大流量：App 测速（4 流）出口侧 `speedtest: 会话 #5 role=send bytes=138999735（含预热
  23658135）用时=10024ms`（#5-#8 四会话各 ~138MB/10s，合计 ~55MB/s——App 显示 ↑51MB/s 吻合）；
- App UI：`主机 127.0.0.1 直连 15 ms`（v6-only 形态下唯一活路 = v6 直连）；
- 前置：手机核须含 B0-1 客户端双栈改动（R8 核 e714b30d 的 wtransport socket 是 v4-only——
  v6 候选发送 EAFNOSUPPORT 被静默吞，手机恒探测不到 WG 面；换核 e1fbd181309d 后首连即 v6）。

## B0-1 统一进程两生产形态验收（2026-10-05）

- (a) 零参 + config 期望态（serve only，42681）：serve 装配行 → E21 自动挑卡 → serve 就绪 →
  统一进程就绪 → SIGTERM `homeway: 收到信号，收工`，events.log 落盘同串；
- (b) `--state` 双角色（serve 42680 + relay 42741 + advertise + serve.relay=rl1…）：双角色就绪 +
  中继注册 `中继：注册成功（腿 42680 → 127.0.0.1:42741）`（OK-MAC 认证行在先）+ 手机式服务会话
  客户端全链（赛跑直连胜出/transit 拨 223.5.5.5:53/speedtest 297/441Mbps 对账 -0.05%）。

## Rust term 服务面实采（R6-6g，2026-10-04；Rust exit 实例 = `tools/local-rust-exit.sh start 1`，
**客户端 = baseline 克隆构建的 Go `homeway term` CLI**（`bin/homeway-go term … --state
/tmp/homeway-rs-rustexit-1/serve` 本地直连 UDS）——「Go 客户端消费 Rust term 服务」全流程）

| 面 | 实测结论（同串） |
|---|---|
| `term list`（表格/JSON） | 表格含状态徽章：`p1  80x24  working  codex  codex ⠋ running task  host*`（STATE=working/blocked/idle 三态、AGENT、TITLE、CLIENTS 的 kind=host/app + `*`=活动腿）；`--json` 解析零错（字段序消费面 = Go CLI 的镜像 struct） |
| `term new -d` / 首接入 | `已创建会话 t-a（不接入）`；attach 后 LIST `attached=true`、clients[0] `{"kind":"host","cols":80,"rows":24,"sinceMs":…,"active":true}` |
| attach 交互（raw 腿） | 回放前序 `[3J[2J[H` + ATTACHED + REPLAY-DONE 后实时流：输入回显（`echo HI-RUST-TERM` 出回显）/退格（`BSB`+`` ⇒ 实跑 `BS`）/Ctrl-C（前台 job 终止、^C 回显）；CLI 状态行随 STATE 帧刷新（`t-a · shell · idle`） |
| 会话自灭 | `homeway term: 会话 t-a 已结束（退出码 7）`（`exit 7` → ENDED code 7 直传；D-19 处置后信号死 = -1、正常码一致） |
| KILL（ENDED -2） | `会话 t-k 已结束`（delete 方）＋在接 CLI 收 `homeway term: 会话 t-k 已被关闭（App 或 homeway term delete）` |
| 多腿接管（attach -d） | 首腿收 `homeway term: 会话 t-m 已被另一客户端接管（replaced）；重新接入：homeway term attach t-m`（ENDED -1/replaced 归因文案）；同实例重连收 self_reconnect 归因（单测钉） |
| 版本门 | caps 带 protoVer 位 + 版本 9 ⇒ `ERROR(term_version: 客户端终端协议版本 9 与本出口 1 不符：请把 App / homeway term 与出口升到同一版本)`（单测钉 + 实测） |
| explain 在线 | `agent：codex（manifest=2026.09.22.1 … 来源=embedded）`＋规则轨迹（Go 同款：explain 只喂屏幕文本——title 证据面与 Go 一样不进 explain，两边口径一致） |
| surface 面（单测实腿） | SNAPSHOT(+SNAPSHOT-DONE) 分片 → gunzip → 解体 revision=1/几何/网格全绿（`exit_code_passthrough_and_surface_leg`，真实 UDS 腿）；差分/背压/裁剪重建面 = codec 向量 + 6e golden 两向 |
| 检测三态造流量 | 伪造 codex（`exec -a codex` 钉进程名 + OSC 2 标题）：working=braille spinner+chatty 重绘（输出腿）/blocked=`Action Required`（osc_title_blocked 规则）/idle=普通标题（osc_title_idle）/直报=OSC 21337 status=blocked（最高权威）——判据行全部入册 E16c |

## Rust 中继侧实采（R4-4d，2026-10-02；实例 = `tools/local-rust-relay.sh start 1`，端口 4278x；判据行进 `<state>/cache/relay.log`，终端只出 token/端点公告）

三链路实测拓扑：链路 1（Go exit + Go client + **Rust relay**，出口换端口令直连死）；
链路 2（Rust 全栈）；链路 3（Go exit + Rust relay + Rust client）。

| # | 实采行 |
|---|---|
| R1 | `中继就绪：127.0.0.1:42781（token 模式（中继 ID 89174bf28aa1）；分配回收 1m30s，注册腿过期 1m30s，每源限速 200 pps，每后端最多 32 条分配，腿总数上限 256）` |
| R2 | `中继控制面：TCP 0.0.0.0:42781 就绪（后端拨腿模式可用）`（同号 TCP 与 UDP 实际端口一致） |
| R3 | `中继：后端 940b4136… 注册成功（腿 127.0.0.1:42645）`（Go/Rust 出口同串；**链路 1 单变量证明的核心行**） |
| R4 | `中继：后端 05e656a8… 注册腿地址变化 → 127.0.0.1:42645（旧分配 0 条已作废，等客户端重建）`（出口换端口注入） |
| R5 | `中继：后端 940b4136… 控制面就绪（127.0.0.1:64477；SESSION 通告启用拨腿模式）` |
| R6 | `中继：后端 940b4136… 控制面重放 1 条活跃会话`（控制面重连对账） |
| R7 | `中继：客户端 127.0.0.1:62387 起会话 #1（拨腿模式）→ 后端 05e656a8…（数据口 0.0.0.0:63977）` |
| R8 | `中继：会话 #1 的后端腿重拨 → 127.0.0.1:64021（cookie 认证通过，跟随）` |
| R9 | `中继：会话 #1 回收：拨腿等待超 15s（通告后无 LEGUP——后端拨腿失败/通告丢失）` / `中继：回收 1 条空闲分配腿（当前 0 条）` |
| R10 | `中继统计：注册腿 1（累计成功 8，伪造 0）｜分配腿 1（累计 6，回收 5）｜转发 上 32 / 下 68500 包｜丢弃 0`（分钟统计） |
| R11 | `中继：后端 05e656a8… 的 token 校验不过（密钥不对/没带 token）—— 拒绝`（换 relay.key 后旧 token 复连注入） |
| R12 | `中继：后端 05e656a8… 注册腿过期（32s 无保活）—— 摘掉`（上游出口停机注入） |
| R13 | ulogf 族：`日志：<state>/cache/relay.log —— 终端只出 token 与端点变化` / `中继 token：rl1…` / `端点：127.0.0.1:42781` / `⚠️ 公布的地址都在内网：公网中继请加 --advertise <公网IP:端口>` / `已生成中继鉴权密钥（<state>/relay/relay.key，0600）—— 重启不变，token 因此稳定` |
| X1 | exit 侧（`--relay`）：`中继：注册腿开跑（中继 127.0.0.1:42781，token 模式（rl1 凭据））` / `中继：注册成功（腿 42645 → 127.0.0.1:42781）—— 客户端可经它到达本机` / `中继控制面：中继身份已认证（OK-MAC 通过）` / `中继控制面已连（127.0.0.1:42781）—— 已清腿表，等待会话重放` / `中继控制面：会话 #1 已拨腿 → 127.0.0.1:62614（认证腿）` / `中继控制面：会话 #1 已拆腿（RELEASE）` / `中继：收到对端地址线索 127.0.0.1:51980 → 盲打 3 包（开自己 NAT 过滤；能否直连仍由 WG 握手决定）` |
| X2 | exit 侧 token 并入：`端点：192.168.3.12:42641（内网）、127.0.0.1:42641（公网）、127.0.0.1:42781（中继）`（中继端点恒标 relay——57012ad 防线） |
| U1 | 客户端经中继（升级条纹实测，`--no-hints` 中继 + 端口搬移拓扑）：5 拍 `link: via=relay ep=127.0.0.1:42810 rtt=1ms（服务会话巡检）` → `RELAY-UPGRADE：已在中继停留 5m0s，重新武装赛跑试直连（下一发出站包镜像到全部候选）` → `RARM 软赛跑（中继立即参与，同时试直连）` → `RELAY-UPGRADE：升级成功 → via=direct ep=192.168.3.12:42811 rtt=6ms` → `link: via=direct ep=192.168.3.12:42811 rtt=1ms（服务会话巡检）`（直连恢复 = 出口搬回 token 原端口） |
| U2 | 中继驻留数据面：`RREG 注册刷新 → 127.0.0.1:42781（dev=bca1bf74，中继=true）`；speedtest 经中继下行 23.7MB 偏差 -0.28%；files 5MB 上传/下载经中继 sha256 双侧一致（952e76af…） |

**测试形态口径**（同机拓扑的固有局限，已在中继/CLI 各有开关）：同机回环上直连永远可达（hint→盲打→
任意源采纳会把客户端翻成直连——这本身是 Go 设计的打洞自愈行为），中继驻留/升级条纹的测试前提
（直连不可达）需注入：`homeway-cli relay --no-hints`（中继不递送两端 hint）+ 客户端 token 直连端点
指向死端口（`--dead-direct` 或端口搬移）。极端形态 `--inject relay-lock`（test-seams 构建：非中继源
按从未到达处理）用于把驻留钉死。上行为中继 200pps 防放大限速所囿（Go 同值——R2 经中继上行 2.7Mbps
即此上限的实测锚）。吞吐/尾延迟量化归 R5 矩阵。

## 已知口径注记

- E10 的 `dialok` 计数**仅 TCP**（`pkg/intercept/stats.go:9-11`：flows 退役后 UDP 会话不再计
  dialok；UDP 观测走 E12 会话行）。Rust 侧计数语义必须同此口径。
- E1 打的是**配置端口**；实际端口（占用退让后）落 `<state>/serve/listen_port.txt`，token
  端点跟实际端口走（`serve.go:553` 注释）。
- E3 的 token 行有去重纪律：端点没变不重打（`serve.go:157` lastToken）。
- **【Q-B 批，2026-10-07】IPv4 分片丢弃（F7，接受的差异）**：拦截层在 `on_plain` 的
  parse 之后、demux 与五元组查表**之前**丢弃分片包（非首片 `frag_off>0` 或 MF 置位）并计
  `fragDrop`——**拒绝 Go/gVisor 会重组的形态**（>1280 的 UDP 被分片后，本层不再当独立会话
  处理）。后果：被分片的 UDP（少见）不转发，客户端按超时/不可达重试；出口侧分片重组**未实现**
  （另议）。DF-only（flags=0x4000、off=0、MF=0）不算分片，照常处理。
- **【Q-B 批，2026-10-07】非 TCP/UDP 协议不建会话（F9，接受的差异）**：ICMP 等非 6/17 协议
  在 `on_plain` 直接丢弃——**不产出 Go netstack 会回的 ICMP 不可达**（`build_icmp_unreachable`
  现仅对 UDP 生效，扩展面另议）；收益 = 移除每包 `socket/bind/connect` 开销与 `dialfail` 计数
  噪声（**非**「占表」——拨号失败同调用内 `remove_flow`，不留存）。
- **【Q-D 批，2026-10-08】差分下发行集合的竞态窗口（F2，行文不变）**：`flushed` 指纹的推进收紧为
  「只在第 y 行内容进入一次下发载荷时推进」——1Hz 采样（`screen_text`）此前会无条件提交指纹，
  导致该行在下次变化前**永久不再下发**（客户端字形/光标错位；最小复现已转正为回归测试）。
  新语义：竞态窗口内下发的行集合可能变化（**以前被静默丢掉的行现在会补发**；快照路径提交全屏
  指纹后，后续增量不重复带已随快照发过的行——幂等且更省）。
- **【Q-D 批，2026-10-08】EXPLAIN 文本面与回滚折行（F8，行文不变）**：`plain_text` 在**备用屏**
  下从「恒空串」→「活动屏视口文本」（对齐 ghostty formatter 只格式化活动屏）；主屏的
  回滚行软折行位（WRAPLINE）从「恒 false」→「按行读」——EXPLAIN 的 `screenBytes`/匹配结果
  在含折行长行滚入回滚的场景下会变化（`region_bytes` 随之变化；定检路径的视口 `ScreenText`
  不受影响）。
- **【Q-D 批，2026-10-08】快照镜像窗口行数（F1c）**：见上「计数输入集/数值语义变化」表——
  宽屏（大 `cols×rows`）下客户端初始滚动窗口变小，超出部分走 FETCH-ROWS 按需拉取（规格场景不变）。
- **【Q-E 批，2026-10-08】files 路径沙箱逐分量复核（F1，行为差异）**：`canonicalize` 快路径换成
  逐分量 walk（Go `os.Root` 同规则，设计文档 §0.4 对 Go 1.24.5 的实测）——**(a)** 目标**绝对**的
  符号链接**一律** `not_found`（即便落在根内：现状 Rust 放行、Go 拒；本批对齐）；**(b)** 相对目标
  词法越界拒；**(c)** 悬空链接 `not_found`（Go 实测亦回 ENOENT）。此前「不存在叶子原样放行」可在
  根外落文件（P0-4）。残余：复核与使用点分离的 **TOCTOU 未收口**（无 `openat2`，macOS 不可用；
  威胁模型 = 经隧道的设备）；`stat` 的 `entry.name`（实址 basename vs Go 请求 basename）与 `write`
  穿透符号链接语义（vs Go `rename` 替换链接）**均维持既有**，未在本批改动。
- **【Q-E 批，2026-10-08】files 客户端响应行不设上限（F2，行为差异）**：>64KB 响应由「报错」→
  「正常返回」；与 `facade/files_op.rs` 的 `read_line_capped(false)` 同口径（原 64KB 门是服务端
  请求行上限的误移植）。残余：客户端内存随对端响应行增长（对端 = 用户自己的出口）。
- **【Q-E 批，2026-10-08】files 服务端请求行上限（F3b，与 Go 对齐）**：由「无界读」→
  「`MAX_REQUEST_LINE` 64KB **累积中判负** ⇒ `invalid_arg`「请求行超过 65536 字节」（Go 同串）+
  收线」；请求行**读出错**（含 busy 路径）按 Go `errorResponse` 语义回 `op_failed`（`server.go:177-183`：
  非 `*Error` ⇒ `op_failed` + 原文）再收线。**这是补 Rust 漏用（移植偏差修复），不是新增偏离**。
- **【Q-E 批，2026-10-08】files 上传磁盘水位（F3a，行为差异）**：目标文件系统可用空间 <
  `UPLOAD_RESERVE_BYTES`（1 GiB）时拒收新上传（起始门 + 每 8MiB 进行中门），拒绝码 = 既有
  `op_failed`（**不新增码**）。Go 无任何上限（照收）⇒ **加固超出 Go 基线**；并发两条上传可越
  保留量 ≤8MiB 窗口；`statvfs` 失败 fail-open（节流告警）。
- **【Q-E 批，2026-10-08】UPnP `http_call` 上限/长度/期限（F7，行为差异）**：①体积闸从无到有
  （**分调用**：描述文件 1 MiB / SOAP 64 KiB = Go 同值），但**超限报错**而 Go 是 `io.LimitReader`
  **静默截断**（理由：截断会切掉 `>713<` 表尾判定体 ⇒ 静默改变映射表语义）；②`Content-Length`
  一致性校验为**新增严格度**（Go 无；声明 > 上限立即拒、实收 ≠ 声明报错）；③拨号与读的**绝对
  期限**（Go ctx 同义）。
- **【Q-E 批，2026-10-08】SSDP 应答来源过滤（F8，行为差异）**：只采纳「私网/环回来源 +
  `HTTP/1.1 200`/`HTTP/1.0 200` + 非空 LOCATION」（Go 只查非空 LOCATION、不校来源）；未采纳者
  **继续读**、不中断重试；公开地址 LAN 会漏配（真机可回退）。
- **【Q-E 批，2026-10-08】UPnP 加映射的所有权门与轮级序（F9，行为差异）**：①轮级「先加后删」——
  `clean_mappings` 跳过 `prefer` 条目 ⇒ **`prefer` 端口**在候选申请成功前**不删**（口径订正，
  代码门 r10 M2/r11：**只保证 `prefer`**——`prefer` 由 `find_our_mapping` 选出，若它与"在用映射"
  不同（例如表里另有 Ours 条目），其余 Ours 仍会被 clean 删除后再在候选上加回 ⇒ 存在一轮真空窗）；
  ②仅当「枚举完整且快照明确属于我们」才允许 718 时先删后加（幂等重建）；fail-open 与「表说空闲
  但路由器报 718」改为**让位下一候选、不删既有映射**（Go 会强删）⇒ fail-open 轮里 `prefer` 端口
  续不上时会**换外部端口**（登记为已知漂移，Go 是"删了再加回同一口"）；③缩租改「先 add(300) →
  718 才 delete + add(300)」（加不回去时原映射仍在）；④候选去重（Go `tried` 同义）。**残余**：
  路由器若**静默覆盖**同名映射（不回 718），"先加"仍会顶掉别人的（Go 先删同样如此）。
- **【Q-E 批，2026-10-08】UPnP 期限穿透与全局预算（F10，行为差异）**：`ssdp_location`/`discover_igd`
  收 `deadline`（内部取 `min(deadline, 5s)`；每轮 recv 超时与 sleep 均按剩余夹取）；公网端点路径的
  40s 预算**贯穿「映射 + 外网 IP 查询」两步**（Go `publicendpoint.go:141` 单 ctx），缩租 8s 为**全局**
  预算。停机路径（`serve_cli.rs:482`、`unified_cli.rs:912/939/1470`）最坏时长由 N×(5s+8s) 降到
  **8s + 1 个 recv/sleep 上界（≤1.7s）**（代码门 M3 订正口径）。**残余**：`to_socket_addrs` 的系统
  域名解析没有可取消面（字面 IP 走快路径；生产 IGD LOCATION 基本是字面 IP）——挂死的 LAN 主机名
  解析不受 deadline 约束（Go 的 ctx 下解析可取消）。
- **【Q-E 批，2026-10-08】UPnP 缩租结果归因（additive）**：`ShrinkOutcome` 分 `NoIgd`/`TableIncomplete`
  （枚举残缺 ≠ 没有映射）/`NoMapping`/`Shrunk`/`Failed` 五态（engine 侧各打归因行）。
- **【Q-E 批，2026-10-08】accept 错误分类（F5，行为差异）**：`EMFILE/ENFILE/ENOBUFS/ENOMEM/
  ECONNABORTED/EPROTO/EINTR` 从「当监听已关、静默退出」→「退避重试（200ms→1s 上限）+ 节流日志」；
  Go 的接受循环遇错即退出（由调用方 Close + 日志）。`Fatal`（EBADF/EINVAL/…）仍退工，但现在**记行**。
- **【Q-E 批，2026-10-08】speedtest busy 路径帧吞（F6c，行为差异）**：拒绝路径的「有界吞一帧」从
  「512B 缓冲 + 超长即报错（载荷留流里 ⇒ 后续对帧错位）」→「按声明长度**流式吞完**（64KB 复用缓冲，
  超 `MAX_BLOCK` 收线）」；帧吞窗 `2s` / 吞输入窗 `1s`（Go `ReplyThenClose` 同值），**两腿都走逐
  syscall 收敛的 `DeadlineIo`**（代码门 H2 订正：只 arm 一次时 `read_exact`/`BufWriter` 的内部
  循环会被「每 <2s 送 1 字节」的滴流无限续命 ⇒ 单条 busy 连接可占线程数小时；现为真绝对期限）。
- **【Q-E 批，2026-10-08】DNS TCP 腿读侧期限（M1 订正，与 Go 对齐）**：`exchange_tcp` 的读/写
  从「一次性 `set_read_timeout(budget)`（per-syscall）」→ **逐 syscall 按绝对期限收敛**（同
  `DeadlineIo` 形态）——滴流上游最坏从 ≈(2+65535)×budget 收敛到 ≤budget（Go `SetDeadline(now+budget)`，
  `server.go:460`）。
- **【Q-E 批，2026-10-08】UPnP `http_call` 收满声明长度即返（M5，与 Go 对齐）**：`Content-Length`
  已知且正文收满即 break（Go `io.ReadAll(resp.Body)` 在长度边界返 EOF）——忽略 `Connection: close`
  的 keep-alive 路由器不再把每次 SOAP 拖到预算耗尽。
- **【Q-B 批，2026-10-07】应用层丢新（F3/F4，接受的差异）**：`out_udp` 超条数/字节上限、以及
  `tx_deferred` 滞留超 `TX_DEFER_MAX_BYTES`（非 TCP 包）时**丢新 + 计数**（`udpDrop`/`shapeDrop`）。
  Go 侧对应面是内核 rcvbuf 界定 / 无界 channel——本层以显式上限换取内存有界，代价是压力下丢新
  率上升（`udpNoReply` 观测漂移）。**TCP 恒不丢**（字节流丢字节 = 流错位）。

## daemon/控制面族实采（B0-2b 第 1 棒，2026-10-05；统一进程本地实例 /tmp/hw-ctl-unified——serve/relay 初始停用，control.sock 0600；出口 = local-rust-exit.sh 实例 1/2〔42651/42652，隔离端口绝不触生产 41641〕）

| # | 判据行/判据面 | 出处形态 |
|---|---|---|
| DC1 | 统一进程：`control: 控制面就绪（sock=<state>/control.sock，0600）`（首启 fail-fast：监听失败 = 报错退出） | stdout/events（Go 同串形态） |
| DC2 | client 角色：`client: 角色已装配（N 台主机，hosts 表 <state>/client/hosts.json）` | daemon-events.log |
| DC3 | host add 验证：`验证：有直连端点应答（reach.tier=direct…）` + 逐端点 `端点 127.0.0.1:42651（直连）rtt=0ms`（CLI 呈现 reach 三档）；全不可达 → 可行动文案退出 1（host_unreachable 码） | CLI |
| DC4 | hosts 表变更（daemon-debug.log）：`hosts: + <id>（<name>）` / `hosts: - <id>（<name>）` / `hosts: <id>（<name>）token 已刷新（同后端重签发）` / 损坏备份 `hosts.json 损坏（…）——已备份 …，按空表启动` | Go 同串形态 |
| DC5 | 会话挂上出口：出口侧 `peer: + dev=bc75993e pub=cec16a33 ip=100.64.67.255 n=1/32`（host add 后 daemon 会话真连） | 出口 stdout |
| DC6 | host list：`<id16> exit1 ready direct 192.168.3.12:42651 7ms`（state/link/rtt/RX-TX 四面） | CLI |
| DC7 | status 聚合面：`守护进程：serverVersion=… generation=… seq=… pid=…` + 四角色行（client/control running、serve/relay stopped）+ 主机行 | CLI |
| DC8 | serve 运行时启停：`serve：started / already / stopped / restarted`（幂等语义在成功载荷）；`serve: 按期望态装配（config serve.enabled=true，经控制面）` → engine 判据 `serve 就绪：wg=:42661` 进 events.log | CLI/daemon-events/events |
| DC9 | serve token（控制面 reveal）：hmw1… 全文 + `（端点：…；来源=ledger）`；status 族只见掩码 `hmw1……（96 字符）`（Go MaskToken 同串形态） | CLI |
| DC10 | relay 运行时启停：`relay：started / stopped`；`relay: enabled=true state=running listen=:41741` | CLI |
| DC11 | 控制面协议族（单测钉死，fixtures/control-cp-v1 43 帧对拍 + 服务器集成）：hello/welcome 握手、版本不匹配 `reload(proto_mismatch)`、超限帧 `goodbye(bad_frame)`（body 不读）、在途超 32 `goodbye(overrun)`、unknown_op 不断连、cursor_stale/bad_request 全错误码面、流 open/双向/close→`stream.end{reason:closed}`、`no_stream` corr=0 回执 | daemon::tests（28 例） |
| DC12 | 多主机/断线重连：exit1（活）+ deadhost2（独立 peer --force）并存互不干扰；同 token 重复 add → host_exists；停出口 → 会话保持 ready（恢复阶梯后台自愈）→ 起出口 → 巡检流量恢复（TX 296→612B） | CLI 实测 |
| DC13 | 收工：SIGTERM → 控制面 goodbye(shutting_down) 尽力送达 + hosts 表 close（session.removed detach 事件面）→ 进程有序退出 | 实测 |

**留桩（如实标注，判据不可见）**：serve.status 的 peers/intercept 观测面（空表——ServeEngine 状态缝未开）、
supervisor 退避重建（角色失败 = 进程退出靠 launchd/nohup 拉回）、term/files `--host` 远程模式与承载面 9 op
（forward/socks/speedtest 回 bad_request+归因）——挂账与接棒指针 = `docs/reviews/B0-2b.md` §三。

## term `--host` 远程 + supervisor/工件族实采（B0-2b 第 2 棒，2026-10-05；daemon =
`homeway-cli --state /tmp/hw-2b2-uni`〔client 角色常驻 exit1 会话 via=direct rtt=1ms〕，
出口 = local-rust-exit #1〔42651〕；CLI = 新构建 release；PTY 驱动逐腿伪终端）

| # | 实采行 |
|---|---|
| DC14 | 远程拨号链（CLI → control.sock → stream.open{kind:term} → 隧道 → 出口拦截）：出口 `intercept: tcp exempt 100.64.255.1:7724 ← 100.64.130.176:<port>（dialok）` + 会话/腿行 `term: 新建会话 pty-…（pid=… 80x24 shell=/bin/zsh）` / `腿接入（kind=host 80x24 id=host-<8B> 首腿=true）n=1/8` |
| DC15 | 远程 attach 全流程 9/9：回放+`echo` 回显到达；`touch /tmp/hw-term-marker-*` **经隧道在出口侧 shell 真执行**（marker 落盘）；`exit 7` → `会话 … 已结束（退出码 7）`；KILL → delete 方 `会话 … 已结束` + 在接腿 `… 已被关闭（App 或 homeway-cli term delete）`；`attach -d` 接管 → 首腿 `… 已被另一客户端接管（replaced）；重新接入：homeway-cli term attach …` + 出口 `腿断开（kind=host 原因=takeover）`；Ctrl-b d → `已分离（会话 … 继续在出口运行）` exit 0 |
| DC16 | blocking_push 漏 notify 形态（修前对照）：CLI `对端已关闭连接（term 服务退出或会话收工；也可能是本端长时间停止读取、出口侧慢腿自治收尾了本腿——停滞超 60s 断腿不发 ENDED）` 挂 15s（= 出口 HELLO_TIMEOUT 收线）——修后 `term list --host exit1` 即时应答 |
| DC17 | supervisor：`role serve: 失败（…）——退避 500ms/1s/5s 后进程内重建`（daemon-events.log，Go 同串）；daemon.status 角色面 `serve failed restarts=N`；健康面 `running restarts=0` |
| DC18 | serve.status 观测缝：`peers：1` + `dev=<16hex> ip=100.64.x.x 空闲=Ns` + `intercept：dialOk=N dialFail=N reject=N flows=N`（speedtest 真连实测 dialOk=4 dialFail=1 flows=4） |
| DC19 | 工件互通：Rust export/import 全往返（config 逐字节一致 + 覆盖导入 + 在跑拒绝 `import: 目标 state 的统一进程在跑（…/lock 被持有）——先停进程再导入`）；Go export → Rust import ✓ / Rust export → Go import ✓（bin/homeway-go 实测） |
| DC20 | status --watch：快照渲染 `exit1 ready direct 192.168.3.12:42651 7ms`；SIGINT → exit 0 |

## 承载面族实采（D-1，2026-10-05；daemon = `homeway-cli --state /tmp/hw-d1-uni`〔serve 显式停用〕，
出口 = local-rust-exit #1〔42651〕+ 出口侧本地 echo/http 目标；CLI = 新构建 release；单测 = `cargo test -p homeway-core --lib carriers::`〔18 例〕）

| # | 实采行 |
|---|---|
| CA1 | forward 往返：`已建转发 exit1 127.0.0.1:42701 → 出口自己:42781（listening，目标经 exit1 出网）`；`nc 127.0.0.1 42701` 发 `ping-forward-1` 收 `ECHO:ping-forward-1`（经隧道往返）；出口侧 `intercept: tcp exempt 100.64.255.1:42781 ← 100.64.213.226:<port>（dialok）` |
| CA2 | forward 持久化：daemon 重启后 `forward list` 规则重建（listening、无 warn）+ 重启后往返复绿（暖机窗口外） |
| CA3 | forward 负例：端口值域外/目标非 v4/跨族占用（`监听端口 42701 已被 exit1 的 forward 规则 占用（可用 --listen 另选）`——socks on 撞 forward 端口的现场诊断）；每主机 8 条上限；损坏 forwards.json → 备份 + 空表重建（单测） |
| CA4 | socks 代理（IPv4 目标）：`socks 已开启：exit1 127.0.0.1:42702（域名经 exit1 出口远程解析，不在本机解析）`；`curl --socks5 127.0.0.1:42702 http://192.168.3.12:42783/f.txt` → `hello-socks-file`（请求过隧道） |
| CA5 | socks 域名目标（远程解析腿）：`curl --socks5-hostname 127.0.0.1:42702 http://example.com/` → `HTTP 200`（DNS-over-TCP → 隧道 IP:5300 出口代答 → 应答候选按序拨）；出口 `dns: … qtcp=N` 计数增长 |
| CA6 | socks 记忆：daemon 重启后 `socks on` 缺省沿用记忆端口（42702）；`socks off` → `socks 已关闭：exit1（在世连接已 RST 收口；端口 42702 记忆保留，下次 on 缺省沿用）` |
| CA7 | speedtest 守护托管：`✓ exit1：↑38MB/s ↓18MB/s` + 精确值行 `down=19005150B/s（152.04Mbps，18.12MB/s） up=40119589B/s（320.96Mbps，38.26MB/s） 用量 ↓51.06MB ↑95.44MB 墙钟 5.4s`；过程提示（stderr）`▶ exit1（via=direct rtt=7ms，开跑时冻结）`/`下行中 17MB/s`；`--json` 逐主机对象（host/name/ok/downBps/upBps/usageDown/usageUp/wallMs/via/rttMs） |
| CA8 | speedtest busy：同主机并发第二台 `✗ exit1：并发满员，稍后再试（busy）` rc=1；Ctrl-C：CLI `已按 Ctrl-C 终止轮转（当前主机已取消；未测的主机不再测量）` rc=1 → daemon 侧 busy ~1s 内解除（重试立即可开新轮） |
| CA9 | speedtest 负例（单测）：link_down 在 waitMs 预算内 waiting 重试（waitRemainMs 递减面）到点收场；refused（7803 回 RST）→ not_supported 立即终态；等待期取消 → cancelled 终态 |
| CA10 | files `--host`：`files list --host exit1 d1-host-test`（root 相对）→ `file 26 src.txt`；put/get 往返 `上传完成 26 字节`/`下载完成 26 字节` + sha256 两侧一致（`2` 同 hash） |
| CA11 | dialControlSpawn 按需拉起：`--no-spawn` → `守护进程未运行且 --no-spawn 已给定（不拉起）`（fail-fast）；无 no-spawn → `守护进程未运行（launchd 代理 me.zhaozhe.homeway-exit 在册）——等 KeepAlive 重拉…` → `KeepAlive 4s 内未重拉——改为自行拉起` → `守护进程未运行，已启动 pid=N（state=…）` → 命令照常完成；子进程 stdio 落 `<state>/cache/spawn.log`（tail 见统一进程就绪行） |
| CA12 | no_host 族：`forward add --host nosuch` / `socks on --host nosuch` → `没有匹配 "nosuch" 的主机（host list 看全表）`（CLI 侧 resolve 先行——与 term/host delete 同一份规则文案） |
| CA13 | 纯读/直改族不拉起（评审中-5 整改后实采）：停机态 `status` → `守护进程：未运行（sock=…/control.sock 不存在/不可连）——本命令纯读不拉起；先启动：…`；`serve status` → `serve：进程未运行（本命令纯读不拉起）；config serve.enabled=false`；`serve stop` → `serve：进程未运行——期望已写为停用（config serve.enabled=false），下次启动不再装配`（直改 config 实查 enabled=false）；`--help` → 用法 + exit 0（speedtest/--host 族同） |

## DDNS 双半边（P0-4；D-2 8o，2026-10-06 实采于本地实例 /tmp/d2-ddns-state）

### server 侧（命令面 + config + token 叠加 + 自检）

- `homeway-cli serve ddns add <domain>` → `ddns 条目 home.example.com 已写入 config`
  （重复 add：`ddns 条目 home.example.com 已在 config（不重复添加）` exit 1）
- `serve ddns delete` → `ddns 条目 … 已从 config 删除` / 不存在幂等
  `ddns 条目 … 不在 config（幂等，无动作）`
- `serve ddns list`（逐行域名 / 空表 `（config 无 ddns 条目）` / `--json`
  `["home.example.com"]`）
- config 写出形态：`[[serve.ddns]]` + `domain = "…"`（原子写 0600）；非法条目
  （空/带端口路径）启动报错 `serve.ddns.domain "…" 非法（只要裸域名，不带端口/路径）`
- 装配行：`DDNS：已配置 1 个域名（token 叠加域名条目、既有端点全保留；自检随公网端点探测同拍跑）`
- 探测全关 + ddns 配置：`DDNS：公网端点探测未开（--upnp=false --stun=''），自检没有观测可比对、跳过；token 的域名条目端口按实际监听口`
- **token 叠加**（本地实采）：`客户端 token（…3 个端点）` 端点行 =
  `192.168.3.12:42677（内网）、127.0.0.1:42677（公网）、home.example.com:42677（域名）`
  ——端口 = 已公布公网端点外部口（`ddnsEntryPort` 同义；无公网观测回退实际监听口）
- **自检真跑**（域名不存在形态，公共解析器直查）：
  `⚠️ DDNS 自检：解析 home.example.com 失败（ddns: 全部解析器（2 个）均无可用应答）——本轮跳过对比`
  （连续失败只打第一拍；恢复行 `DDNS 自检：解析恢复（%s → %v）`）
- 滞后告警族（连续 ≥3 拍不一致）：`⚠️ DDNS 自检：记录滞后——域名解析 … 与本机观测 … 连续 N 拍不一致；请检查 DDNS 更新器（路由器/脚本）是否还在工作`；恢复 `DDNS 自检：记录已恢复一致（…）`
- 缺 AAAA 告警：`⚠️ DDNS 自检：域名 … 没有 AAAA 记录（只解析出 A）——蜂窝用户将失去 v6 直连路径；请让 DDNS 同时更新 AAAA`；恢复 `DDNS 自检：域名已带 AAAA 记录，v6 直连路径恢复`
- 卫兵两档（错误文案）：`ddns: 解析结果落在 fake-IP 段（代理环境污染，检查绑卡/代理）` /
  `ddns: 解析结果非全球单播（DDNS 记录本身不可路由，检查记录值）`
- serve.status 的 ddns 段（载荷）：`{"domain":…,"lagStreak":N,"warnedLag":true?,"warnedAAAA":true?}`
  （false 位省略——omitempty 同义）；CLI 人读面渲染（评审 3.5 整改）：
  `  ddns：` + `    - <domain>（连续不一致 N 拍）[ 已告警滞后][ 已告警缺 AAAA]`
- 顺手补：`serve relay set <token> [--stdin]` / `serve relay clear`（写 config 0600 +
  在跑检测提示 `⚠️ 不热更：需 homeway serve restart 生效`；评审 5.1 整改后位置参数形
  与 --stdin 形均可用）

### client 侧（token 域名端点展开 + 重解析）

- 建会话：域名端点解析一次（A+AAAA，v4 在前；5s 预算）；成功记行
  `token 端点 localhost:41641 解析为 N 个地址（[…]）`；失败
  `token 端点 %q 域名解析失败（跳过）：%v`（其它端点照常；全失败 = NoCandidates）
- 重解析（Rearm/RearmSoft 触发，异步 5s 单飞）：失败
  `域名重解析 %s 失败（退回上次解析结果）：%v`；候选变化
  `域名重解析：N 条候选已刷新（…）`；中继采纳 + 15s 节流窗到
  `域名重解析晚于赛跑结算（当前中继 …）→ 节流软赛跑补投新候选`
- hosts reach 探测同样解析域名条目（父预算内）

## 判据变更记录（政策 + 登记表）

> **政策（2026-10-07 Q-A 批起）**：判据行**不再是不可变更的冻结物**。旧口径「字节级同串、
> 改措辞 = 静默破坏对齐」仍是默认纪律（未登记的改动照样是缺陷），但**显式登记后允许演进**——
> 任何判据行变更必须登记在本节，并**与代码变更同批 commit**；对齐验收口径 = ①同串，或
> ②本表有与变更对应的条目（从/到 + 原因 + 影响面齐全）。登记条目一经写入不修改（可追加
> 「后续」说明）；历史条目保留，供追踪判据行演进链。
>
> **登记字段**：日期 / 条目（E\*、C\*、R\*、DC\*、CA\* 等编号或行名）/ 从 → 到 / 原因 /
> 影响面（哪些验收方/文档/测试引用该行需同步）。
>
> **动机**：`udp_seq_of`（E12 关闭行恒 `#0`）与 `decr_flow` 下溢两条真 bug 修复曾被
> 「判据行冻结」挡下（评审已登记、代码未修，见 AUDIT-2026-10-07 Q-B 条目）——冻结本意是
> 防静默漂移，结果连显式修复也一并阻断。本政策把「防漂移」落在**登记**上而不是「禁改」上。

### 登记表

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-07（Q-B 批落地） | **E12 关闭行会话号**（`udp intercept: 会话 #%d … 关闭`） | `udp intercept: 会话 #0 关闭（…）`（恒 0）→ `udp intercept: 会话 #<本会话建立号> 关闭（…）` | **修复型变更**：`udp_seq_of` 从未赋值（唯一写点 = 0），E12 关闭行恒 #0 是缺陷不是契约；Q-B 批在 `udp_ready` 建立时 `f.udp_seq_of = seq`（F5-1） | `docs/INTEROP-CRITERIA.md` E12 行、`crates/homeway-core/src/server/intercept/mod.rs`（`udp_ready`/`finish_udp`）、单测 `udp_close_line_reuses_establish_seq`；任何按「E12 关闭行 #0」写断言的测试须改按建立号 |
| 2026-10-07（Q-B 批落地） | **`decr_flow` 下溢（flows gauge 回绕）** | 未 `incr_flow` 的流上 `decr_flow` → flows gauge 回绕（`fetch_sub` 记负/极大值）→ 改为**配对守卫**（仅对已建立流计数） | **修复型变更**：`decr_flow` 可在未 `incr_flow` 的流上执行（listen 失败路径、`close()` 期在途 Dialing 流）属计数器缺陷；Q-B 批以 `establish`/`retire` 收口（F5-2，`Flow.counted` 守卫 + `debug_assert`） | `intercept` stats flows 面、`serve.status` intercept 段、单测 `close_does_not_underflow_flow_gauge`/`udp_session_end_to_end`（flows gauge 断言） |
| 2026-10-07（Q-C 批落地） | **中继注册腿上限拒绝行**（`中继：注册腿总数已达上限 %d，拒绝新的 %x（防匿名洪水）`——**非 R1–R13 判据行**，仅中继运维日志） | 现状「HELLO 即占 `legs` 槽、超限在 HELLO 拒」→「HELLO 只写独立 `pending` 挑战表（≤64、满按最旧淘汰**不拒绝**），PROOF 通过才提升进 `legs`（受 `max_legs` 闸）」——**拒绝点从 HELLO 移到 PROOF 提升**；边缘：极端洪水下合法 PROOF 因 `pending` 条目被淘汰而未命中时**静默计 `伪造`**（R10 `伪造` 输入集边缘变化；后端 5s 重发 HELLO 自愈，代码门 M1 登记） | F5：未认证 HELLO 不得占用受 `max_legs` 约束的腿槽（匿名洪水面）。**设计文档「无状态 cookie 挑战」版经复验不可实现**（PROOF 校验必须知道 HELLO 的 pubkey——token 模式 `HMAC(secret,"relay-psk"‖nonce‖pubkey)`、开放模式 DH 均需；而 PROOF wire 固定 50B **不含 pubkey**、`label=sha256(pubkey)[:8]` 不可逆）⇒ 采设计文档「备选」pending 分表（**用户裁决 2026-10-07**） | `relay/mod.rs`（`Leg`/`PendingLeg`/`handle_control`/`reap_round`）、单测 `hello_flood_does_not_occupy_legs`/`pending_full_evicts_oldest_not_reject`/`legs_cap_applied_at_promotion`；中继运维日志读者 |
| 2026-10-07（Q-C 批落地） | **`tunnel_addr` 撞车分支**（派生隧道地址） | 极稀有/可研磨设备（自选公钥使 raw 派生 == `SERVER_TUNNEL_IP` = 100.64.255.1）→ 按 `hw-tun.N`/`hw-app.N`（N=2..9）再散列值 | F11：`SERVER_TUNNEL_IP`（v=65281）落在 `derive_tunnel_ip` 值域 `[1,65534]` 内且两端无守卫（共享缺口）；碰撞**可故意研磨**（~6.5e4 次） | **wire 差异（双侧对称）**：客户端 `derive_tunnel_ip`/`derive_tun_ip`（`tunnel_addr.rs`）与服务端 `table.rs` 派生同规则；旧对端/fixtures 向量对该设备地址不一致 ⇒ 直接连不上（已知代价）；`fixtures/vectors/tunnel_addr.json` 现有样本不撞车（取值不变） |

| 2026-10-08（Q-D 批落地） | **E16a/E16b 尺寸字段**（`新建会话 … 80x24` / `腿接入（kind=… 80x24 …）`；同族面 = LIST JSON 的 `cols`/`rows` 与 ATTACHED 的 `cols`/`rows`） | 任意 u16 尺寸原样接受（`RESIZE/HELLO 65535×65535` ⇒ alacritty 按 `rows×cols` 即时分配两屏，实测 ≈96–192 GiB，分配失败 = abort）→ **>1000×500 夹取到上限**；`RESIZE 0×0` **忽略本次上报**（会话几何保持旧值，对齐 Go 的 0 门在尺寸写点之前） | **修复型变更**（P0-3，实测锚）：尺寸入径此前无上限（alacritty 无 MAX，`MIN_COLUMNS` 全库零引用）；`RESIZE 0×0` 曾把会话几何污染成 0×0 而格流仍按 vt 宽编（客户端错位）——Rust 独有移植偏差 | `docs/INTEROP-CRITERIA.md` E16a/E16b 行、`service.rs`（HELLO/RESIZE 入径 + LIST/ATTACHED 路径）、`term/size.rs`、单测 `vt_size_gate_rejects_oversize_and_zero`/`resize_clamp_and_zero_ignore`/`normalized_boundaries`/`report_gate_zero_and_clamp`；**正常尺寸（≤1000×500 且非 0）逐字节不变**；raw CLI 不回读几何 ⇒ 夹取对它是静默的（残余登记见 `docs/reviews/QD.md`） |
| 2026-10-08（Q-D 批落地） | **surface cell 流 symLen 域**（`[hdr]` 低 7 位 = symbol 字节长） | >127 B 字素簇编码为**错位字节流**（hdr 写 `len & 0x7f` 而体写全量 ⇒ 后续颜色/属性字段全部错位）→ 按 **UTF-8 边界截断到 ≤127 B**（真源 = `codec::marker::SYM_LEN_MASK`，`vt::cell_of` 主截 + `codec::append_cell` 副门） | **修复型变更**：alacritty `push_zerowidth` 无上限（200+ 组合字符可达），7 位长度域约定下现状即错乱帧 | `codec.rs`/`vt.rs`、单测 `vt_symbol_cluster_truncated_at_boundary`/`surface_symbol_truncation_roundtrip`/`append_cell_secondary_gate_alignment`、fuzz 哨兵 `fuzz_term_vt`（每格 ≤127）；**fixtures/vectors 无该形态 ⇒ 无夹具变更** |
> **上表 E12/decr_flow 两行 = 2026-10-07 Q-B 批落地登记**（Q-A 批预登记的占位条目已按本政策补全
> 「从 → 到」实际行文并去掉「占位」标注，同批 commit）；**其下两行 = 2026-10-07 Q-C 批落地登记**；
> **再下两行 = 2026-10-08 Q-D 批落地登记**（E16a/E16b 尺寸字段 + surface symLen 域）。
> 登记生效后，E12 关闭行与 flows 计数按新行文验收（旧行文不再要求同串）；Q-D 的尺寸面按
> 「正常尺寸逐字节同串、极端输入按登记」验收。

### 计数输入集 / 数值语义变化（**行文不变**，登记留痕）

> 口径（Q-B 批确立）：**计数器输入集变化不属判据行变更**（行文未动、不需改同串口径），
> 但**必须在本节列明**——供验收方追踪数值语义演进。以下均不改任何判据行行文。

| 日期 | 条目 | 从 → 到（数值语义） | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-07（Q-B） | **DC18** `intercept：dialOk=N dialFail=N reject=N flows=N` | `dialFail` 含 ICMP 等非 TCP/UDP 协议的每包拨号失败 → 非 TCP/UDP 不再建会话，`dialFail` 不再被 ICMP 抬高（F9） | F9：仅 TCP(6)/UDP(17) 建会话（Go 仅注册 TCP/UDP handler） | DC18 数值、`serve.status` intercept 段 |
| 2026-10-07（Q-B） | **E22** `dns: q=… qtcp=… resp=… malformed=…` | ① `qtcp`/`malformed` 不再被 DNS-TCP 腿的空帧（`mlen==0`）抬高（F2-3 收线，**与 Go 对齐**：Go 读失败即 `return`、不计 qtcp，`server.go:294-300`）；② `resp` 按 DNS 腿**每包**增长（F6，**与 Go 对齐**：`dnsleg.go:35` 每包 `Answer`） | F2-3 空帧收线 / F6 腿会话内每包应答 | E22 数值、DNS 相关单测 |
| 2026-10-07（Q-B） | **`udpNoReply`**（udpcap 实测位） | 压力下上升（F3 UDP 上行门丢新 + F4 非 TCP 滞留丢新） | F3/F4 应用层丢新策略的观测漂移 | E12/udpcap 相关观测 |
| 2026-10-07（Q-B） | **新增独立丢弃计数** `udpDrop`/`shapeDrop`/`fragDrop`（`Stats::snapshot()` 追加末位） | 无 → 有（新增观测） | F3/F4/F7/F10 静默失败/丢新改为可观测 | additive：① `snapshot()` 索引 `[0..=5]` 与键查找语义不变；② 经 `serve.status` 载荷 `ServeInterceptBits`（`udpDrop`/`shapeDrop`/`fragDrop`，serde `default` 兼容旧载荷）暴露；**DC18 人读行文不变**（`daemon_cli` 仍只渲染 dialOk/dialFail/reject/flows） |
| 2026-10-07（Q-C） | **E9** `peer: - dev=… reason=stale (idle=…) n=…/…` | 触发集：Go「先摘表项/设备再查地址、撞车也摘」→ A 版「地址校验通过才摘」（撞车拒绝时**不打** stale 行、表内条目不变） | F1：淘汰改为「先纯选择 victim + **排除视图**校验 + 通过后才落库并产 `Remove`」（表-设备一致，P0-2） | E9 数值、`server/table.rs` 单测 `eviction_view_rejects_collision_with_live_device`/`table_full_and_stale_eviction` |
| 2026-10-07（Q-C） | **C5/C6**（赛跑结算 / 路径确立·切换） | 输入集：任意来源包（含 1 字节垃圾、未知 kind）→ **仅解出 Data 帧** | F2：采纳收紧为「仅 Data 帧」（Control(hint)/未知 kind/垃圾不 adopt；hint 仍学习但不接管路径） | C5/C6 数值、`bind.rs` 单测 `only_data_frames_adopt` |
| 2026-10-07（Q-C） | **E8** `peer: ~ dev=… refresh (idle=%s) n=…/…` | ① 频次：未采纳期 2s reg 补投（**每次未采纳期** ≤15 次 / ≤60s，**rearm 归零**——恢复阶梯重开一轮拿新预算）会让出口多打 refresh 行；② **`idle=` 数值语义**：从「距上次注册」→「距上次（补投）注册」（常 0s/5s） | F3：未采纳期 reg 定时补投（首包丢/时钟偏差自愈）；上界与 F1 淘汰窗口交互（防隧道坏但出站可达的客户端让条目永不 stale） | E8 数值、`bind.rs` 单测 `reg_resend_while_unadopted` |
| 2026-10-07（Q-C） | **C13** `候选端点（%d 条…）` | 学习候选无上界 → **上限 64 + 排序尾部淘汰**（保护 verified；**新条目永不被拒**）+ 投喂配额（每 60s 新增未验证地址 ≤24，hint/probe 共用） | F4：端点学习缓存 cap（落 observe/mark_verified/load/merge_disk/entries 出口） | C13 条数、`endpoint_cache.rs` 单测 `cap_evicts_tail_and_protects_verified`/`load_and_merge_respect_cap`/`feed_quota_shared_and_reset` |
| 2026-10-07（Q-C） | **R1** 就绪行「腿总数上限 256」 | 数值语义：过去含未验证（HELLO 即占）腿 → 分表后只数**已接纳**（PROOF/控制面通过）腿 | F5：`pending` 与 `legs` 分表 | R1 数值、中继运维读者 |
| 2026-10-07（Q-C） | **C14** `出口能力：构建 %s` | 中继 build：`relay-dev`（`Config.build` 全仓无赋值）→ 注入 `BUILD_STR`（`"homeway-rs-dev"`）；**仅中继为首个候选时**生效 | F9：中继探针应答上报真实构建（serve 侧 `build` 不动） | C14 数值、`relay_cli.rs` 装配、`probe.rs` 单测 `respond_build_byte_truncation_no_panic` |
| 2026-10-07（Q-C） | **R10** `中继统计：… 转发 上 %d / 下 %d 包｜丢弃 %d` | ①「转发 上/下」**只计成功**（`send_to` 返 Err 不再计成功）；②「丢弃」**输入集扩大**：新增「全局分配腿上限（1024）拒绝」入 `dropped`（限流/每腿上限等旧口径不变）；③ 拨腿等腿窗 `pend` **入队即计** `forwarded_up`（既有语义，本批未改——回放失败只进 `send_fail_up`，不再动下行面）；新增 `send_fail_up`/`send_fail_down`/`down_limited` 为 **additive** 计数（中继无 JSON 遥测通道，仅日志/单测可见） | F7/F8：静默丢包计成功修正 + 下行字节桶 + 全局 assoc 闸 | R10 数值、`relay/mod.rs` 单测 `assoc_lazy_bump_and_down_fail_counted` |
| 2026-10-07（Q-C） | **中继 assoc 下行速率**（上行 200pps ≈2.7Mbps 口径之外的**下行**面） | 无上限 → 每会话字节桶 16MiB/s（≈128Mbit/s；目标 ≥100Mbit/s） | F7.2：下行准入限流（**性能行为差异**；**禁止**复用 `leg_rate_ok` 的 2000pps ≈22Mbit/s 硬顶） | 中继下行吞吐、`relay/mod.rs`（`ASSOC_DOWN_BYTES_PER_SEC`） |
| 2026-10-07（Q-C） | **观测面 additive**：探针 flags 中继降级位（bit5 `FLAG_RELAY_CTL_DEGRADED`）/ `send_fail_*` / `down_limited` / 中继拒绝日志限流 | 无 → 有 | F7/F8/F10/F6：中继无 JSON 遥测通道，本批多为日志/单测可见 | 探针 flags（**中继命名空间** bit5；serve 侧 bit0–4 与 C14 **不受影响**）、`relay/mod.rs` 单测 `probe_flags_report_ctl_degraded` |
| 2026-10-08（Q-D） | **快照镜像窗口行数**（SNAPSHOT 体的 mirror 段；不在判据行、无夹具） | 恒 `rows × 10`（视口数）→ `min(rows × 10, 32MiB / (cols × 48B))`（下限 64 行；1000 列 ⇒ 699 行） | F1c：快照材质内存上界（上限处最坏 ≈200 MiB/次 + 反复 RESIZE 可反复触发） | 快照镜像面、`service.rs`（`mirror_rows_budget`/`mirror_window_rows`）、单测 `mirror_window_rows_budget`；**窄屏（80/100 列）与既有 golden 形态不变**；宽屏客户端滚动窗口变小（FETCH-ROWS 按需拉取兜底，tier `term-surface-protocol` 场景不变） |
| 2026-10-08（Q-D） | **新增观测行（additive）** | 无 → 有：`term: 会话 {n} 查询应答丢弃 {n} 条（队列满）` / `剪贴板写丢弃 {n} 条` / `PTY 注入丢弃 {n} 条（队列满）`（节流：首 3 次 + 每 100 次；消费者已退出时尾缀换 `（消费者已退出）`——代码门 M3）；`term: {ctx}{term-pump\|term-leg-writer\|term-surface\|term-resp\|term-sample\|term-conn} 线程 panic（已兜住）：{载荷}`；`term: 会话 {n} 尺寸夹取 {cols}x{rows} → {c}x{r}（上限 1000x500）`（**只在夹取结果真变、尺寸真被应用时打**——同值重复上报不刷屏，代码门 L5）；`term: 会话 {n} 忽略 RESIZE 0×0 上报 {k} 次（会话几何保持）`（节流同款）；腿断开归因新增 `原因=panicked` | F6/F7/F1a 观测面（原 `resp_dropped` 只写不读、nudge 丢弃与线程 panic 静默） | 非 E 族判据行；`docs/reviews/QD.md`、`service.rs` 单测 `drop_counters_and_throttled_log`/`guard_thread_logs_and_reports_action`/`nudge_bytes_for_three_states`/`sample_tick_panic_keeps_loop_alive`、E16d 的 `原因=` 归因集合新增 `panicked`（词表是自由文本归因，非 ENDED reason 词表） |
| 2026-10-08（Q-E） | **E22** `dns: q=… qtcp=… resp=… fallback=… fail=…` | ① `fail` 输入集扩大：上游持续灌不匹配包/短包时，此前 worker 被**永久占用**、该查询永不结束（不计 `fail`、无应答）；现在按**本腿预算的绝对期限**结束 ⇒ 计 `fail` 并回 SERVFAIL。② `fallback` 输入集随 ① 上升（坏上游被按期判负后，后续腿与兜底才真正被尝试）——**单腿预算（含 `MAX_PER_TRY=800ms`）与上游回退序不变** | F4a：per-syscall → **per-attempt** 绝对期限（对齐 Go `SetDeadline(now+budget)`，`server.go:415`） | E22 数值、DNS 单测（`exchange_absolute_deadline_under_poison_upstream`）、`dnsproxy.rs` |
| 2026-10-08（Q-E） | **`udpDrop`**（intercept 计数） | ① **输入集不变**（三个来源：DNS 回投 tx 写失败 `intercept/mod.rs:1861-1863`、`udp_send_to_client` 栈 tx 满、`out_udp` 超限）；② 数值下降：DNS 回投 rx/tx 容量 64 槽/64KB → **256 槽 / 316,624B**（F4c 把 worker 2→64 后 64KB 先撞墙，故容量必须同比扩） | F4c+F4d：回投容量与 worker/在途对齐 | `udpDrop` 数值、`serve.status` intercept 段、`dnsface.rs`（`udp_tx_capacity_matches_inflight`） |
| 2026-10-08（Q-E） | **E13** 会话时长 | 滴流客户端此前每次成功读续命（30s 硬超时形同虚设）⇒ 现在**绝对** 30s 上界；正常会话（≤5s 预热 + ≤15s 窗口）不变 | F6a：硬超时绝对化（`Limits` 可注入） | E13 数值（时长字段）、speedtest 单测（`conn_timeout_is_absolute_under_dribble`） |
| 2026-10-08（Q-E） | **UPnP 映射表枚举次数**（`GetGenericPortMappingEntry` SOAP 往返，非判据行） | 每轮 `3 × (N+1)` → **`(N+1)`** 次往返（N = 表长；第 N+1 次取表尾 713） | F9a：枚举一次缓存（**优化，偏离 Go 的 3 次**） | 路由器负载、UPnP 轮次耗时；日志行文不变（`enumerate_once_per_round`） |
| 2026-10-08（Q-E） | **新增观测行（additive）** | 无 → 有：`files`/`speedtest` accept **瞬态错误退避**行（首 3 + 每 100）、accept **Fatal 退工**行（含 engine 侧「服务线程退工」）、`files` 会话线程/busy 线程起不来行、files **水位检查失败 fail-open 告警**（首 3 + 每 100）与**上传中止**行、UPnP「枚举一次 / 未经核验不删（让位）」行、缩租 `ShrinkOutcome` 归因行（NoIgd/NoMapping/Shrunk/Failed）、`http_call` 三条新错误串（超限/长度不符/预算耗尽） | F3a/F5/F6c/F7/F9/F10：静默路径改可观测 | 各日志族读者；**非编号判据行** |
| 2026-10-08（Q-I 前段） | **udpcap 探测周期 / caps 新鲜度**（`UDP 默认路径：…` 行 = 本表 udpcap 的 `—` 行，**行文与语义均不变**） | 频次：**bindwatch 在位形态**（auto 挑卡/显式绑卡 = 生产形态）周期 `~600s`（`recv_timeout(300s)` 超时后又 `sleep(300s)`，等于每拍睡两次）→ **`~300s`**（超时即重探；对齐 Go `udpcap.go:26` 5min ticker + kick，`udpcap.go:140-152`）；**`--bind-interface none` 形态本已 ~300s（不变）**。喂客户端 C14 的 caps 位新鲜度：`≤10min` → **`≤5min`**（kick 面即时重探不变） | Q-I F7：`recv_timeout` 的 Timeout 与 Disconnected 未区分（多睡一拍） | `UDP 默认路径` 行频次（行文不变）、caps 新鲜度、`server/engine.rs` 纯函数 `udpcap_disconnected_backoff` + 单测 `udpcap_wait_three_states`；**非**编号判据行（无 R1–R13/E* 行文变更） |
