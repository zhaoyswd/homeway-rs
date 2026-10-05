# INTEROP-CRITERIA — 互操作判据行清单（R0.3）

> Rust 版对齐验收的判据行真源。**行文字节级同串**（含全半角标点、空格）才判绿；
> 改措辞 = 静默破坏对齐（同款教训见 tier AGENTS 坑内 `link: via=…` 注记）。
>
> - **出处** = baseline 克隆（`baseline/homeway`，基线 `621fe0e`）内路径，相对仓根。
> - **真实样例** = 2026-10-02 本地烟囱实测采集：`tools/local-exit.sh start 1`（出口 UDP
>   127.0.0.1:42641）+ `client-start/client-add 1`（服务会话形态客户端）+ `speedtest -host local1`。
>   日志面：出口 `--verbose` stdout（`/tmp/homeway-rs-exit-1/stdout.log`）与摘要
>   `<state>/cache/events.log`；客户端服务会话 `cache/client.log`。
> - 无样例行 = 需要 APP 核形态或故障注入才触发（R1/R2 补采），出处已核。

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
