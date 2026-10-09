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
| E22 | `dns: q=%d qtcp=%d resp=%d filter=%d trunc=%d fallback=%d fail=%d drop=%d malformed=%d aaaa-mixed=%d fakeip=%d`（DNS 代答计数行，debug 级周期输出；与 E10 同族计数语义。**`fakeip=%d` = Q-J F3 追加**，见「判据变更记录」） | `pkg/dns/server.go:162`（`StatsLine`，判据行注记 `:159-161`） | **R1 已采**（2026-10-02，周期行）：`dns: q=0 qtcp=0 resp=1 filter=0 trunc=0 fallback=0 fail=0 drop=0 malformed=0 aaaa-mixed=0`（Q-J 前的历史形态，无 `fakeip=` 尾字段） |
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
- **【Q-F 批，2026-10-08】portfwd 诚实态（F1，行为差异 + 功能缺口）**：手机核**未实装**端口转发
  监听器，映射状态由恒 `listening` 改为恒 `failed` + 明确 err（空 `code`），热替换 rc 由 `0`
  改为 `-1`。Go 基线真 bind（`app_portfwd.go:77-113`）⇒ **功能缺口如实化**；`target` 文案收敛到
  `pf_target_text` 单一真源（CLI `cmd_portfwd` 第三份拷贝同批收敛）。**CLI 附带行为变更**：
  `--map L:<ip>:0` 的**实际拨号端口由 0 改为 L**（`port==0 ⇒ port=listen`，与 Go 桌面 facade
  `forward.go:318-325` 同义；旧行为拨 `ip:0` 必失败）——CLI 是 Rust 独有测试动词（baseline 无对应
  命令，Go `clientcore` 的 `pfDial` 对 `TargetIp != ""` 原样拨 `TargetPort`），方向已核、登记在案。
  挂账、实现轮廓与交接块见 `docs/reviews/QF.md`「portfwd-B 交接」（tier `port-forwarding` spec 的
  SHALL 处于**已知不达标**）。
  **已收口（2026-10-08，Q-F-B 批）**：真监听器已实装（见下方「【Q-F-B 批】portfwd 真监听器」注记
  与「判据变更记录」同族行）——本注记的「未实装 / 已知不达标 / 假归因」不再成立（**保留历史原文 + 收口指针**）。
- **【Q-F-B 批，2026-10-08】portfwd 真监听器（含**偏离 Go** 的四处）**：每条映射在 `127.0.0.1:<listen>`
  （**地址硬编码，无配置面、无 `0.0.0.0` 退路** ⇒ 局域网不可达由绑定地址保证）真 bind + accept 线程
  （`poll(2)` ≤50ms，连接就绪即接）+ 入站连接经**裸拨**（`Client::connect_deadline`，**不复用
  `healing_dial`**——目标常态拒绝不许触发 R2 恢复阶梯；恢复仍由 `patrol` 独立驱动）与双向泵
  （复用 `bridge_host::pump`，EOF 零日志对齐 Go `pkg/netpipe`）转发到目标。守卫：规则 **≤8**、
  并发流阀 **256**、拨号期限 = `dialMs`（缺省 15s）、每连接线程显式 **256 KiB** 栈
  （**实现注记（代码门 r27 P6）：设计 v3 写 128 KiB，实现取 256 KiB**——与本仓全部会话面线程同档
  〔`session`/`domain_eps`/`daemon/carriers/forward` 均 256K〕，pf 的 conn/泵线程跑的就是同一批
  `SessionStream` 代码；Rust 栈溢出 = **进程 abort**，「省虚拟内存」的收益可忽略）；`SO_REUSEADDR`
  显式（Go `net.Listen` 默认）、backlog 固定 **128**（Go Linux=`somaxconn`/darwin=128）、
  `SOCK_CLOEXEC` 经 `sysfd`；拨号失败 **RST 收口**复用 `sysfd::rst_close_tcp`（单源上移，daemon 三处
  调用点零改动）；accept 瞬态错误退避重试（复用 Q-E F5 分类件）。
  **偏离 Go 四处**：① **阀值 256 vs Go `maxTCPFlows=4096`**——口径 = 手机内存预算（引擎每连接
  2×1 MiB 缓冲、`stackb.rs` 的 `TCP_BUF` 无上限 ⇒ 256 ⇒ 最坏 ≈512 MiB；Go 的自证注释称「与 gVisor
  流共用」**已陈旧**，其读写只在端口转发面）；② **accept 致命错误 ⇒ 记行 + 该条状态转 `failed`**（空码 +
  精确 err）——Go 留 `listening` 且**不关 fd**（端口仍绑、只是无人 accept）；Rust 的 fd 单属 accept
  线程 ⇒ 退工即释放端口，留 `listening` 就是「监听中但连不上」的谎报（D15）；③ **装配期破损配置不 bind**
  （见下注记）；④ **收工序**：Rust 取 **pf 先停、桥后停**（Go 的 defer LIFO 实为桥先停——两序都在
  client 关闭之前；Rust 取更早释放端口）。**热替换**：install 两阶段（只 take 旧 `lns` + 旧 `states`
  原地保留至单次换入 ⇒ 读侧只见「全旧」或「全新」，Go 持 `pfMu` 的等价语义）+ 旧监听器**退出 ack**
  （共享 400ms；`Disconnected` = 线程已退 = fd 已关，不是超时）+ 未 ack 端口的 3×50ms 重试 bind
  （仍失败 ⇒ 双记行，**不许把内部竞态伪装成「端口被占用」**）；已建立连接不被替换打断（Go 同形）。
- **【Q-F-B 批，2026-10-08】portfwd 非常态面 + 残余（10 条）**：① **`tunConfig` 旁路破损配置**
  （`listen` 非法 / `target_ip` 非法 / 超条数 ⇒ **不 bind** + `failed` + **空码** + 精确 err；
  **表单路径不可达**——`parsePortForwards` 只丢 id/端口 0 的条目、`HostStore.updateForwards` 不重复
  校验 ⇒ **手改/损坏的持久化记录可达**；Go 对 `listen=0` 会绑随机端口 ⇒ **偏离 Go 的加固**）；
  ② 环回/`0.0.0.0` 目标映射为「出口本机」（`ExitPort`）——Go 经出口过境重拨到出口的 `127.0.0.1`
  （实现路径不同、语义同；本栈显式拒环回，不映射就是「监听中但连不上」）；③ **`target_port == 0`**
  旁路形态走 `dial_target` 折叠语义（IP 分支折 `listen`；**空 IP 分支 `ExitPort(0)`**——Go `pfDial`
  原样拨 0；NAPI 门拒 0 ⇒ 仅旁路可达）；④ 跨世代换代的同端口瞬时 `EADDRINUSE` 窄窗（Go 同形）；
  ⑤ **收工预算叠加**：`stop_all` ack ≤200ms（到点 detach 自退，fd 随线程退出关）+ 派生线程 join ≤2s +
  桥 ≤2s vs `STOP_WAIT`=3s ⇒ 极端形态走既有 -2 强制放锁路径（Q-F §7-5 同族，挂 Q-G）；
  ⑥ 已建立连接无空闲回收/无速率整形（Go 同形）；⑦ **无鉴权**（本机任意进程可连，Go 同形）；
  ⑧ 引擎侧连接表无独立上限（阀是唯一界；`stackb::DialError::TooManyConns` 是死变体，本批不接线）；
  ⑨ **出口配额全局共享**：`intercept::MAX_CONNS=1024` 覆盖所有客户端与所有腿 ⇒ pf 达阀会挤压
  桥/files/term 与其它客户端（客户端阀 256 已低于该配额一个量级）。⑩ **`install` 中途 panic 的孤儿
  窗口收口**：阶段 3 已 spawn 但未换入 `inner` 的监听器由**暂存守卫（RAII）**在 drop 时置停止位自退
  ⇒ 孤儿窗口为 0；**设计 v3 §1-F1-3 的「万一仍 panic…由 `Finish::drop → stop_all` 清 states 收口」
  该句据此订正**（实际收口机制 = 暂存守卫 + 世代停止位；代码门 r27 P3，实现注记见
  `docs/reviews/QFB.md`）。**另**：`stats_loop` 的 tick 下限 5s 不变（`stats_line` 纯函数解决可测性）；
  accept 线程/conn 线程/泵线程 spawn 失败按「只记行 + 该条/该连接收口」处置（不新增
  `unhealthyReason` 面）；accept 线程**异常退出（panic 展开）**由退出守卫置迟到失败（failed + 空码）
  ——与「致命 accept 错误」同族（代码门 r27 P2）；世代停止位（`gen_stop`）让 accept 线程 ≤1 拍自退并
  关 fd，而状态表在 `stop_all` 之前仍是旧表（Go「`stopPortForwards` 之前不动作」同义；窗口 = 世代主线程
  ≤200ms 的停止位轮询）——**不**在此期间打 `failed`（免正常收工闪失败文案）。
- **【Q-F 批，2026-10-08】桥状态诚实性（F7b，本批加固 ≠ Go 同形）**：桥的 accept 线程 spawn 失败
  或 `listen_path` 重试耗尽时，`bridgeFilesSock/bridgeTermSock/bridgeSpeedSock` 由「上报路径」改为
  **空串**——**Go 的 `sockJSON` 不这么做**（只要 token 在就返回路径，`app_bridge.go:388-395`）⇒
  **偏离 Go 的加固**；`bridgeAuth` 不变；`start()` 到 listen 结论之间的短暂非空窗口为已知形态。
- **【Q-F 批，2026-10-08】服务会话暖机硬失败（F2，行为差异）**：`Session::start` 返回 Ok 但快照
  `state=failed` 时：① rc 面不再「幂等返 0」（域态写 Failed ⇒ 下次 `service_start` 走新受理）；
  ② 失败实例**保留**在运行槽（`service_status` 继续报 `failed`+原因；tier `bridgeHostOf` 的
  `SVC_FAILED → BRIDGE_MODE_SERVICE/BRIDGE_HEALTH_FAILED` 保持一致）；③ `fully_stopped` 置位后
  `start` 可**替换**该实例（Go `svcStateFailed && isDone()` 同形）。此前行为 = rc 面谎报 +
  不可重建 + 无巡检。**实现注记**：`Session::stop()` 的「failed 终态保留」此前被
  `set_state(Stopping)` 覆盖（分支恒不可达）⇒ 本批先读后写（`was_failed` 判定移到写 Stopping
  之前）——该分支由死代码转活，失败原因在收工后仍可读（C17 行文不变）。**F2-3 小修（已采）**：
  槽空 + 域 `Failed` 时 `service_status` 由 `{"state":"idle"}` 改为 `{"state":"failed","reason":…}`
  （覆盖 `Session::start` 返 `Err` 的清槽路径——此前状态面与 rc 门自相矛盾）。**tier 可见后果**：
  `ServiceSession.ets` 把 `state=failed` 映射为 `mode=SERVICE/health=FAILED`，`BridgeRules.ets`
  对 `SERVICE+FAILED` 返回 `BRIDGE_ACTION_HEAL_HOST`（请求自愈拉起）⇒ 一次暖机失败的
  `service_start` 之后 App 会**自动再拉起一次服务会话**（此前 idle ⇒ `NONE/ABSENT`、不拉起）。
  这是有意为之（状态面与 rc 门一致；再拉起受 App 侧既有预算约束），如需改变表达另开批。
- **【Q-F 批，2026-10-08】桥拨号期限（F3，本批加固 ≠ Go 同形）**：带预算的桥拨号把**恢复阶梯与
  阶梯等待**计入同一预算（此前可越界 ≈4×）；**Go 的阶梯与闸等待同样不受调用方预算约束** ⇒
  偏离式加固。两域巡检与 NAPI `tun_recover` 不受影响（`deadline=None`，行为逐字不变）；
  `LadderRc::Deadline` 对外映射 `-1`（不是 -3——tier 对 -3 渲染「本机网络栈没准备好」= 错误归因）
  且不计入耗尽；残余越界上界 = 一个动作预算（`ACTION`=2s，动作不可中断）。
- **【Q-F 批，2026-10-08】收工等待（F6，行为差异 + 残余）**：服务会话 `stop()` 的**五段**（巡检
  join / hint join / 缓存落盘线程 join / 缓存终写（`try_lock` 快跳）/ `Client::stop_within`）共用
  一个 6s 预算；到点放行自退 + per-thread 新行（§5.1 登记）。**范围声明**：本预算只覆盖
  `session::Session::stop()`——不含隧道域 `Finish::drop`/`request_stop` 的 `c.stop()` 与
  `rebuild_session→old.stop()`（残余登记，设计 §7-4/§7-5）。**残余**：`stop_within` 到点 detach 后
  引擎线程可能存活到自行退出（由收割线程 `hw-engine-reap` join 后关 wake fd；收割线程起不来时
  fd 泄漏一枚、保持打开——提前 close 会让驱动 `poll` 忙转）；`fully_stopped` 因此**弱于** Go 的
  `isDone()` ⇒ `start` 替换窗口内旧引擎线程可能仍在（缓解：`Cmd::Stop` 已投递、新引擎新 UDP 口、
  出口按 `peer_id` 覆盖注册）。锁纪律：session/recover/tun_exec/`wgcore::Client` 面一律走
  `crate::syncutil::lock_unpoison`（Drop 链零 panic；分配失败 = abort 为明示 carve-out）。
  **段④残余（代码门 ②-2 订正）**：`try_lock` 只保证「不等锁」——拿到锁后的落盘 I/O
  （`EndpointCache::save` 的读盘/建目录/写/rename，无 fsync、raw 未变时早退）仍**无期限**；
  锁空闲 + 卡文件系统形态下 6s 仍可被击穿（登记为残余，不宣称「不再有任何无界段」）。
- **【Q-F 批，2026-10-08】域名解析并发上限（F8e，本批加固）**：`lookup_host` 的**在飞解析**分档
  上限（critical 4 / background 4；额度随 worker 生命周期——**调用方等待超时返回不归还额度**，
  卡在 `getaddrinfo` 里的 detach 线程继续占用 ⇒ 到上限后该档**有界地失败**：第 N+1 个调用等到
  调用方预算耗尽即回 TimedOut）；阻塞获取的等待计调用方预算，获取耗时同计（`budget - 获取耗时`
  才是 worker 期限，总耗时不得 ≈2× 预算）；**Go 无上限**（每调用新 goroutine）。
  `resolve_domains` 对每个域名条目各用整份预算（N×budget）的既有形态不变。
- **【Q-G 批，2026-10-08】TUN fd 失效判据（F2）**：`poll_fd` 现在区分「可读/可写/HUP/ERR/NVAL」并
  **把 HUP/ERR/NVAL 当异常**（判死条件**只看 `hup||err||nval`**——不依赖「readable 为假」：darwin 上
  管道 EOF 恒返 `POLLIN|POLLHUP`，实测见设计 §0.4）；**超时不判死**；读侧**可读优先**（不丢最后一包）、
  判死前**确认一拍（真睡眠 50ms 后复 poll）**；`n==0` 的**非判死**路径在「poll 立返可读但空」形态下加
  **地板睡眠**（`DEAD_CONFIRM_DELAY`，代码门③——防该形态 100% CPU 热自旋；超时/HUP 形态不走此路）。
  Go 基线（`tunfd_unix.go`）在 EAGAIN 分支同样不看 revents ⇒ 本项为**偏离 Go 的加固**；可见差异仅在
  「fd 已死」的极端形态（Go 静默转，Rust 上报并触发重建）。残余：OHOS VPN fd 的精确内核语义本仓不可
  取证（capi 只收 App 传入的裸 fd）；**持续型 HUP** 的健康 fd（若存在）会被确认拍放行误判——如实登记。
- **【Q-G 批，2026-10-08】fd 继承纪律（F1）**：全仓**自建** fd 一律 CLOEXEC（`sysfd` 单源：linux/OHOS
  走 `SOCK_CLOEXEC`/`pipe2(O_CLOEXEC)` 原子位，darwin 建后立即 fcntl）；**App 传入的 tun fd 不改 flags**
  （所有权在扩展）。真实继承面 = 3 处 std `Command` exec（`ps` / `dscl` / daemon 自 exec；已实测证实 std 会
  继承未设 CLOEXEC 的 fd）；PTY shell 由 portable-pty 的 `close_random_fds()` 净化（库行为，依赖 `/dev/fd`，
  **非本仓保证**——本仓不再把它当豁免理由）。残余：darwin「创建→fcntl」窗口（无原子位可用；窗口 ≈ 数十
  纳秒）；linux 原子路径无本机运行期验证（三目标编译探针 + 交叉 check 为准）。
- **【Q-G 批，2026-10-08】权限口径（F4）**：普通文件「**创建即 0600**（`.mode`）+ 拿到 handle 后 fchmod
  归一（防 umask 掩码）+ 失败告警」——与 Go 的原子 `os.WriteFile(...,0o600)` / `os.OpenFile(...,0o600)`
  同义（`server/state.rs` 的身份私钥与 token 台账是**移植回退**修复：旧形态 chmod 失败会让私钥永久 0644
  且无告警）；目录「`DirBuilder::mode(0o700)` + create 后无条件 chmod 0700 + 失败告警」（mkdir 同样受 umask
  掩码）；UDS 因 `bind()` 无 mode 参数，采用「**目录先 0700（bind 之前）** + bind→chmod 0600 + 失败告警」，
  **同用户窗口为已知残余**（跨用户暴露面由目录权限关闭；Go `chmodTighten` 同为 bind 后 chmod + 告警）。
  **权限面定性 = 修缺陷（对齐 Go）⇒ 不登记为判据变更**（本节仅记口径）。

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
| CA11 | dialControlSpawn 按需拉起：`--no-spawn` → `守护进程未运行且 --no-spawn 已给定（不拉起）`（fail-fast）；无 no-spawn → `守护进程未运行（launchd 代理 me.zhaozhe.homeway-exit 在册）——等 KeepAlive 重拉…` → `KeepAlive 4s 内未重拉——改为自行拉起` → `守护进程未运行，已启动 pid=N（state=…）` → 命令照常完成；子进程 stdio 落 `<state>/cache/spawn.log`（tail 见统一进程就绪行）。**Q-J F6**：等待分支（等 KeepAlive）的触发集改为「plist 内容提及该 state」；**非相关分支文案改**（`（未在册 launchd 代理提及 state=…——不等 launchd KeepAlive，直接拉起）`）——见「判据变更记录」 |
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
| 2026-10-08（Q-F 批落地） | **服务会话巡检失败行**（`巡检失败（连续 %d）：探测超时`——**非 C1–C17 判据行**） | `巡检失败（连续 %d）：探测超时`（**恒写「探测超时」**）→ `巡检失败（连续 %d）：<探测的真实错误>`（`ConnErr` 的 Display 原文；超时形态实渲染 `巡检失败（连续 2）：连接超时`——与隧道域同族行 `对端巡检失败 2/3: 连接超时` 同串形态） | F8c：归因写死 = 报错信息失真（`probe_ok` 在 `session/mod.rs` 由 `.is_ok()` 丢弃了错误） | `session/mod.rs` 巡检失败行、grep 该行排障的脚本；**成功拍/门控行不变**（`巡检失败被门控拦下（…）`、`巡检恢复：门控态结束（成功拍清零）` 等逐字保留） |
| 2026-10-08（Q-F 批落地） | **服务会话收工等待行**（非编号判据行；**既有行文变更**） | `收工等待巡检线程超时（STOP_WAIT）——放行自退` → per-thread 形态（`收工等待 <线程名> 超时（STOP_WAIT）——放行自退`；五段覆盖：巡检线程 / hint 线程 / 缓存落盘线程 / 缓存终写跳过（`收工缓存终写跳过（锁被在途落盘占用）——去抖线程近期写已在盘上`）/ client 线程（尾缀 `（引擎线程由收割线程收口）`）） | F6b：五段共用一个 6s 预算（此前仅巡检有界——hint/save/client 的 join 无上界） | `session/mod.rs`；C17「已收工（state=%s）」的 **idle 形态逐字不变**（`已收工（state=idle）`）；`已收工（state=failed 终态保留）` 是失败终态的另一条行（HEAD 同串，本批由死代码转为活路径——见注记） |
| 2026-10-08（Q-F 批落地） | **`portForwards[]` 状态文案与 `ClientCoreTunSetPortForwards` 返回码**（**契约面行为变更**，非编号判据行） | ① `state`：恒 `"listening"` → `"failed"`；② `err`：空 → `"手机核未提供端口转发监听（127.0.0.1:<listen> 未监听）——该映射在当前版本不可用，不影响隧道"`；③ `code`：空（不变）+ **失败映射带枚举 code 的 spec MUST 被有意偏离**（空码走 App 登记的空码兜底，`bind_failed` 是假归因）；④ `target`：`":0"`/`":port"` → `pf_target_text` 四形态（`主机（同端口）`/`主机:N`/`IP:<listen>`/`IP:N`）；⑤ rc：`0` → `-1` | F1（P0 假成功 + 假状态）：本核无监听器 ⇒ 原 `listening`+rc 0 是谎报；对照 Go 真 bind 与 tier spec（SHALL 监听）——**功能缺口挂账见 `docs/reviews/QF.md`「portfwd-B 交接」** | `tier:openspec/specs/port-forwarding`（**已知不达标**）、`tier:pages/PortForwardsPage.ets`（`:484` 提示失义）、`tier:…/TierVpnExtensionAbility.ets:1010`（rc 日志文案失义）、`facade/tun_exec.rs`/`facade/portfwd.rs` 单测；**`fixtures/` 无 portForwards 夹具 ⇒ 无字节夹具变更**；`tools/check-vocab.sh` 不受影响（`bind_failed` 仍声明） |
| 2026-10-08（Q-G 批落地） | **`sun_path` 上限**（三处错误串：`路径超长（%d 字节 ≥ 100，sun_path 上限）` / `socket 路径超长（… ≥ 100 …）` / `桥路径超长（%d ≥ 100 字节…）`） | 判据 `len >= 100`（Go 同形）→ **`len > SUN_PATH_MAX`**（= 拒 104+/108+，**放行 103/107**；`SUN_PATH_MAX` = darwin 103 / linux·OHOS 107，编译期单源 `size_of − offset_of − 1`）；文案「≥ 100」→「> {SUN_PATH_MAX}（sun_path 上限〔平台值〕）」；量法统一为 `as_os_str().len()` 字节口径（旧 `display()` 是 lossy 字符串） | F4.3：深 state 路径被**误拒**（审计 P2；darwin 实测 104+ 才真超限；tier `transport.cpp:268` 的阈值 `>= sizeof(addr.sun_path)` 与本条同义——允许 103） | `daemon/listen.rs`（判定 + 文案）、`files_server.rs::listen_local_service`、`facade/bridge_host.rs`（`MAX_UNIX_SOCKET_PATH` = `sysfd::SUN_PATH_MAX`，用点两处）+ 两处「已平台正确」点注明同源（`bridge_host.rs::connect_budget`、`intercept/mod.rs` 的 UDS 打包——**不字面改调**，避免 off-by-one 静默收紧）；单测 `listen_control_path_limit_boundary`；**已核无脚本/文档消费方**；**行为放宽**（Go 仍拒 100–107） |
| 2026-10-08（Q-G 批落地） | **`relay stop` 后 `relay start` 的 broken 路径行消失**（`role relay: 失败（relay run 线程退出（异常终结））——退避 … 后进程内重建`） | 修前：`relay start` 后 run 立即自退 ⇒ 该行**无限刷**（500ms/1s/5s/30s…，设计 §0.5 实测）→ 修后：不出现（重启后正常驻留；手工脚本复跑 `state=running`、该行计数 0） | F3：`STOP_PIPE` 单例复用已关写端（停管道改 per-proc `StopPipe`，两端归 `RelayProc`，幂等 `shutdown`） | `relay_cli.rs`/`unified_cli.rs`（supervisor）；排障脚本若 grep 该行需知它「不是常态」；Q-H 条目「`relay stop` 后 `relay start` 不可恢复」**由本批 F3 修根因**（Q-H 收口时勾选，防重开同一修——见 `docs/reviews/QG.md`） |

| 2026-10-08（Q-H 批落地） | **非法 config 处理路径**（`config.toml` 非法值的后果与文案） | 控制面 handler 线程内 `process::exit(1)`（**打崩统一进程**；客户端只见 `等待应答超时`）→ ① 启动期：**拒启**（exit 1 + `config.toml` 路径 + 字段 + 值域的可行动文案；值域错误形态 = `路径: 字段：值域`〔Go `Error.String` 同形〕，TOML 语法/类型错误 = Rust toml crate 原文〔含行号 + 源码片段〕——代码门 A3 口径订正）；② op 触发（`serve/relay start\|stop\|restart`）：**拒绝 + 零副作用**（rc=1 + 可行动文案；内存/文件均不变；Go `lifecycleStart→Update` 同形）；③ 装配期失败（合法 config 但绑定失败等）：`failed` + reason + 退避重建（Go `failedRole` 同形） | F1：同一份 config 被弱表（`unified_cli` 的 `Option<toml::Value>` + 分节私表）/严格表（`serve_cli`）两套判定，弱表放行的类型/值域非法在严格表 exit ⇒ 控制面线程带走整个进程（审计实测两形态）。实现注记：`restart` 不变期望态（Go `lifecycleRestart` 不写 config）故只做严格读、不回写 | `serve_cli.rs`（唯一表 + `load_config_strict`/`serve_config_of`/`assemble_result`）、`unified_cli.rs`（启动期拒启 + 六 op 顺序 + 写回走严格表）、`relay_cli.rs`（去分节私表）；新增 E2E `crates/homeway-cli/tests/qh_config_failfast.rs`（形态①②③）；任何按「非法 config ⇒ 进程消失」写断言的脚本须改 |
| 2026-10-08（Q-H 批落地） | **config 值域校验补齐（= Go `validateFile` 全量）** | Rust 严格表只查 `peer_ttl`/`ddns.domain`/未知键 → **补** `serve.listen ∈ 1–65535`（0 拒）、`bind_interface`（含 `: / \t` 与空/auto/none/off/no 枚举）、`public_endpoint`（逗号分隔 `ip:port`）、`serve.relay`（rl1 可解码 或 `ip:port`；域名不支持）、`relay.listen`（`[host:]port` + 端口 1–65535） | F1：`serve.listen=0` 现状会绑随机端口、`serve.relay` 垃圾串晚失败、`relay.listen` 非法只在装配期失败——Go 一律启动期拒 | `serve_cli.rs`/`relay_cli.rs`；写回面（`nodeconfig.Update` 同形：坏 config 拒写）；**存量「能跑但违规」的手编 config 会被新拒**——见 `docs/reviews/QH.md`「行为收窄提示」（生产预检结论：本机两 state 均通过） |
| 2026-10-08（Q-H 批落地） | **CA13 扩展**（`--help` 短路覆盖面） | `--help` 仅在 speedtest/carriers 族短路 → **`host`/`status`/`serve`/`relay`/`serve token` 命令组（动词形 + 裸名词）与 `serve`/`relay` 前台形态同样短路**（用法 + exit 0；`serve stop --help` 不再真停出口）；`connect`/`files`/`dnstest`/`portfwd`/`speedtest`（直连形态）/`token` 的 args[0] `--help` 同批收口 | F8：审计实测 `serve stop --help` 真写 `serve.enabled=false` | CA13 行（行文不变、覆盖面注明）、`daemon_cli.rs`/`serve_cli.rs`/`relay_cli.rs`/`main.rs`；E2E `serve_stop_help_does_not_touch_config` |
| 2026-10-08（Q-H 批落地） | **CA13 形态扩展**（离线 `serve/relay status` 坏 config） | `config serve.enabled=<默认 true>`（静默降级）→ `config 读取失败：<err>`（Go `degradedServe` 同形；relay 同款） | F1 附带：Go `servegroup_cli.go:410-427` 如实报读失败 | CA13、`daemon_cli.rs` 离线分支、`unified_cli::config_role_enabled → Result` |
| 2026-10-08（Q-H 批落地） | **C14 首次实装**（客户端「出口能力」行） | 未实装（Rust 客户端零打行）→ 实装于服务会话启动序列 C13 之后：成功行逐字 Go 同串（`出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v`）；失败行 `出口能力：参照点探测失败（%v）—— 本机网络到出口的 UDP 不通或出口未应答`（`%v` = Rust io 文案）；探测目标 = token 序**首个已解析候选**（`candidates.first()`，Go `cands[0]` 同义）；探测参数 = `ping_ex(addr, 16, 5s)` | F17：GAP-AUDIT P1-4 取证裁定 = 做（C14 缺失实锤） | C14 行、`session/mod.rs`（`format_outbound_caps_line`/`format_outbound_caps_fail` + 一次性线程）、单测 4 例（纯函数/本地 UDP 桩/失败形态/目标选取）；**行内 `%v` 平台文案差异注记**（与 E20a 同类） |
| 2026-10-08（Q-H 批落地） | **取值 flag 纪律（`--state` 族 + 泛化）** | 缺值/吞下一个 flag/空值/`--state=` 等号形「静默取值或落 CWD」→ **全形态 fail-fast**（exit 2 + 可行动文案；等号形全站点生效；`serve token list\|revoke` 收下等号形）；**泛化到全部取值 flag**（`--token/--host/--listen/--stun/--stun6/--bind-interface/--files-root/--public-endpoint/--relay/--advertise/--ddns/--identity-dir/--endpoint-cache-dir/--inject/--timeout/--name/--mode/--map/--rate-limit/--reason/--file/--agent/-o` …；值以 `-` 开头 ⇒ 缺值报错）；**唯一 carve-out = `--recover-cause`**（自由文本）；**term 两站点从「硬报错未知 flag」变为「接受等号形」**（放宽）；`--state` 内联（`=`）形态不收 `-` 开头值（用户显式绑定） | F2：审计实测 4 形态（含 `serve token list --state=DIR` 落 CWD）+ N2（`homeway export --state=` 会把 `--state=` 当目标文件名） | `cli_flags.rs`（新，唯一取值器）+ 8 个 parser 站点 + `daemon_cli` 工件族三动词；E2E `state_flag_forms_failfast_and_eq_form_accepted`；CA11/CA13 文案不变（新增非法形态文案） |
| 2026-10-08（Q-H 批落地） | **flag 非法值/布尔显式赋值纪律** | `--hold/--probe/--recover-from/--recover-delay/--rounds/--listen(socks on)` 静默回退默认；`--upnp=maybe` 与 `--json=false/--yes=false/--force=false/--no-spawn=false/--verbose=false/--watch=false/--hold=false` 等**布尔显式值被忽略（恒 true）** → **fail-fast（非法值 exit 2）/ 显式值真生效（Go `strconv.ParseBool` 值集：1/t/T/true/TRUE/True/0/f/F/FALSE/False）** | F7a/F7b：与 Go `flag` 包语义对齐 + 审计 P2（设计门 B1/B2 扩面） | `cli_flags.rs`（`take_bool`/`take_num_or_exit`）、`main.rs`/`carriers_cli.rs`/`serve_cli.rs`/`daemon_cli.rs`/`relay_cli.rs`/`unified_cli.rs`/`term_cli.rs`；E2E `numeric_and_bool_flags_failfast` |
| 2026-10-08（Q-H 批落地） | **N1/L7 默认 state** | 前台 `serve`/`relay`/`serve token list\|revoke` 默认 `.`（CWD）→ `default_state_dir()`（`~/.config/homeway`） | F15：GAP-AUDIT P0-1 余项；Go 前台默认同为 `$HOME/.config/homeway`（`internal/server/cli.go:30`、`relay/cli.go:33` 各自的 `defaultStateDir()`；**非** `cliutil` 单源——设计门 C-注记订正） | `serve_cli.rs`/`relay_cli.rs`；「全形态共用锁」在默认取值下成立；单测 `default_state_matches_unified` |
| 2026-10-08（Q-H 批落地） | **DAEMON 资源上限/期限（Rust 加固，接口零变更）** | 控制面连接无上限/握手无期限/joiner 句柄只增 → 上限 64（`DEFAULT_MAX_CONTROL_CONNS`，超限接受后立即关闭 + 新日志行 `control: 连接拒绝（并发上限 64）`）+ 握手 10s（`DEFAULT_HANDSHAKE_DEADLINE`，超时断开 + 行 `control: 连接握手超时（10s 未 hello）——断开`）+ accept 前回收已结束句柄；SOCKS accept 烧尽后 `status.on` 由「谎报 true」→「false + 可行动 err」（`socks on` 可重建——`dead` 落账） | F5/F4：审计 P1/P2；Go 无上限（goroutine）⇒ 显式登记为加固 | `daemon/server.rs`（`ServerConfig{max_conns,handshake_deadline}` + 3 单测）、`daemon/carriers/socks_srv.rs`（`mark_dead` + 注入缝）、`socksmgr` 单测 `socks_dead_is_recorded_and_rebuildable`；新日志行（非判据行）；CA4/CA6 正常路径不变 |
| 2026-10-08（Q-H 批落地） | **CA11 形态扩展（launchd）** | 任何 state 都先等 launchd KeepAlive 4s → **仅默认 state** 等；非默认 state 直接自拉起 + 提示行 `（state=<dir> 非默认 state——不等 launchd KeepAlive，直接拉起）` | F14：Q-G 移交（临时 state 会等生产出口的 launchd，KeepAlive 触发会把生产出口拉起） | CA11、`daemon_cli.rs`（`launchd_relaunch_relevant` 纯函数 + 单测）；**边界登记**：launchd 托管在自定义 state 的部署会退化为自拉起——`docs/reviews/QH.md` §残余（本机 Mac 生产形态即此类，见该文） |
| 2026-10-08（Q-H 批落地） | **CA12 同族：`resolve_host` 全长 hex 就地校验** | 64-hex 直用（不查表、不 canonical 化；打错 id 由 daemon 回 `no_host`）→ **解码 → 小写 canonical → 表内命中返回表内 id；表外/未命中 = CLI 就地报错**（`没有匹配 … 的主机（host list 看全表）`） | F10：Go `host_cli.go:419-427` 全长 64 hex 必须精确命中表内 ID（未命中 `主机 %s 不存在`）；Rust 直用是分叉，且是大写级联漏删的入口 | CA12 同族、`daemon_cli.rs`、`carriers_cli.rs`（forward/socks/speedtest 的 host 解析共用同一 `resolve_host`）；单测 `resolve_host_full_hex_forms` |
| 2026-10-08（Q-H 批落地） | **承载面 host 串 canonical 化（大小写等价）** | 大写 hex 能过成员检查却按原串存表 → `remove_host` 级联按小写找不到（**级联漏删：转发规则 + 监听器残留**）；改为入口 `decode→canonical 小写` 后再委托（**只规范化，不改错误分类**——add/on 仍 NoHost、remove/off 仍 NoRule）（speedtest 三面同批） | F11：`daemon/mod.rs` 只规范化级联一处，`add/remove/off` 原样（设计门复核确认根因成立） | `carriers/mod.rs`（`canonical_host_hex`）；CA3/CA6 成功文案不变；控制面直连客户端（App/矩阵）的输入等价化；单测 `uppercase_host_canonicalized_full_chain` |
| 2026-10-08（Q-H 批落地） | **session 锁 IO 失败 fail-fast** | 只读/异常 identity 目录下 `LockError::Io` 仅告警继续（**互踢 keypair 防线静默消失**）→ 可行动错误 + exit 1（`--no-session-lock` 仍为逃生口；`Held` 文案不变） | F9：审计 P2；Rust 独有防线的静默降级 | `main.rs`（connect/files/speedtest/dnstest/portfwd 五调用点）；只读环境从「降级继续」变「拒跑」；单测 `unwritable_dir_is_lock_io_with_path` |
| 2026-10-08（Q-H 批落地） | **短串截断面（非判据行，行为注记）** | `short_host` 由**字节**切片（`&host[..8]`——非法输入〔非 ASCII〕panic）→ **字符**截断（不 panic；非法输入返回整串） | F6：外部可触发的 dispatcher panic 面（`forward remove`/`socks off` 的 `NoRule(short_host(host))`；`server.rs` 只查非空不做 hex 校验） | 「与 Go 同串」**只对合法 hex 成立**（逐字节不变；Go `shortHost` 是字节截断，非法输入会产出非法 UTF-8 前缀——Rust 取自定义语义）；单测 `short_host_char_safe`；同时 F11 从入口消除该形态 |
| 2026-10-08（Q-I 尾段批落地） | **取值 flag 纪律的局部回退：空值 carve-out（`--stun`/`--stun6`/`--relay`/`--ddns`）**（对照 Q-H「取值 flag 纪律」行） | Q-H 口径「`--flag=`/`--flag ""` 空值 ⇒ fail-fast（唯一 carve-out `--recover-cause`）」→ **追加四 flag 的空值 carve-out**：`--stun`/`--stun6`（空 = 关公网观测）、`--relay`（空 = 关注册腿）、**`--ddns`（空 = 清空 config 全部条目；代码门 M1 扩面——`serve_cli.rs` 原本已有该分支但不可达）** 接受空值并按语义处理；**`--public-endpoint=` 仍拒**（Go 基线实测同拒：`homeway: --public-endpoint "" 非法（not an ip:port…）` rc=1）；其余取值 flag 纪律不变（缺值/吞 flag 仍 fail-fast） | **F0（Q-I 尾段）**：Q-H 泛化纪律把 Go 文档化「空 = 关」（`baseline/homeway/internal/server/cli.go:36-37` + `explicit["stun"]` 分支；`--ddns` 见 `:38/55/127-135` `explicit["ddns"]` ∧ 空 ⇒ nil）拒绝 ⇒ `tools/local-rust-exit.sh`/`perf-ab.sh`/`matrix.sh` 的 Rust 出口**全部起不来**（R5 起本地测量面全瘫）；`bin/homeway-go` 实跑四 flag 空值形态正常起服（Rust 侧属对 Go 的回归） | `cli_flags.rs`（新 `take_value_empty_ok_or_exit`）、`serve_cli.rs`（stun/stun6/relay/ddns 四站点 + 用法行）；单测 `empty_value_carveout_forms`/`empty_value_carveout_flags_accept_empty`；E2E 冒烟（`local-rust-exit.sh start 9` + Go 客户端「就绪（会话在位）」）；**已知残留（非 carve-out）**：`--bind-interface=` 空值 Go = auto（`ResolveBind("")`）而 Rust 拒——有等价写法（`--bind-interface auto`）⇒ 登记不扩；排障脚本若按「空值一律 exit 2」写断言须改 |
| 2026-10-08（Q-I 尾段批落地） | **DNS 面缓冲复用（行为注记，非判据行）** | 无行文变化；`DnsFaces` UDP 收包缓冲改字段复用（F1）、DNS-TCP 连接读缓冲穿参复用（F3）、`TcpConn.rx/tx` 改 `VecDequeLite` 前缀偏移（F4）、dnsproxy 上游列表改 `Arc` 快照 + worker 懒分配读缓冲（F5） | Q-I 尾段性能项：每拍/每查询固定税消除（E22/E4 行文与数值语义零变化） | 无判据行/夹具变更；`DnsFaces` UDP 收包缓冲**复用不残留**（`recv_slice` 只写 `0..n` + 下游 `to_vec()`；新增同拍多包回归断言）；**F4 内存包络（代码门 M2 订正：rx/tx 同形）**：DNS-TCP 单连接 rx backing 最坏 128KiB → ≈256KiB（×64 ⇒ ≤+8MiB），**tx backing 同形 2× `CONN_TX_CAP`(256KiB) ⇒ 相对旧形态增量 ≤256KiB/连接（×64 ⇒ ≤+16MiB 最坏；实测未现）**，RSS 判据（≤A×1.1）已核 |
| 2026-10-08（Q-J 批落地） | **E22 新增 `fakeip=%d`**（DNS 代答计数行尾字段） | `dns: q=… aaaa-mixed=%d`（无 fakeip）→ 末尾追加 ` fakeip=%d` | F3：交付应答（`clamp_ttl`/`truncate` **之后**）答案段 A 记录命中 `198.18.0.0/15` 的**应答数**（每应答 ≤+1；截断掉的记录不计）。**不改应答一个字节**（tier `wg-native-dns:50` MUST「与主 nameserver 一致（含 fake-ip 地址）」）；fake-IP **不触发**换上游/兜底。数值语义 = 单调累计、**fake-ip 主机上常态增长**（不区分「正常透传」与「上游被污染」）；配 `dns_upstream` 覆盖后 `fakeip=0` **不代表环境干净**（与覆盖告警同源） | E22 行文（本表 E22 行已改）、`dnsproxy.rs`（`has_fake_a`/`fakeip`/`fakeip_once` 一次性告警）、单测 `fakeip_counted_on_delivered_response_only`/`fakeip_counts_only_surviving_records`/`fakeip_guard_single_source`；tier `wg-native-dns` 代答可观测性段的统计行描述（**上报项**） |
| 2026-10-08（Q-J 批落地） | **term 键编码平台口径**（新 caps 位 + 声明后行为差异；**取代 D-10「按出口编译宿主」**） | 口径由「出口**编译宿主**（`IS_DARWIN`）恒选」→「**客户端 HELLO caps 声明**；未声明/歧义 = **宿主推断**（= 今日逐字节同值）」：新 caps 位 `1<<2 KEY_ALT_NO_ESC_PREFIX`（darwin 分支语义）/ `1<<3 KEY_ALT_ESC_PREFIX`（非 darwin 分支语义）；**两位同置/裸 ID 形态（caps=0x7F 恰含两位）⇒ fail-soft = 未声明 + 计数 + 一次性告警，绝不拒腿**；声明生效时 macOS 出口上 `alt+文本` 由 `text` → `ESC text`（mok2 alt 位/kitty 关联文本/super 抑制文本同批随口径）。**旧对端策略**：旧客户端不发新位 ⇒ 宿主推断（逐字节不变）；旧出口（< 本批）不认识新位 ⇒ 静默忽略 ⇒ 宿主推断（矩阵见 `docs/reviews/QJ-design.md` §2.2）。**非 darwin 口径无夹具**（向量由 darwin 宿主产出）——真源 = vendored ghostty 源码行级 + `keyenc.rs` 期望表 | F1：出口按**编译宿主**选平台分支 ⇒ 同一手机连 Mac / 连 Linux（阿里云）出口时 alt+文本键字节分叉（审计 P1 核心） | `term/frames.rs`（caps 位）、`term/keyenc.rs`（`KeyFlavor`/`host_default` 唯一 `cfg!` 残留点/`encode_key` 必收参）、`term/service.rs`（serve_hello 判定 + 每腿 `LegRt.key_flavor` + `handle_input` 查不到腿丢弃计数，**不回落 host_default**）、`term/vt.rs`；单测 5 例（`keyenc_parity_with_go_vectors` 去 `#[cfg(macos)]` 全平台跑、`non_darwin_flavor_expectations` 8 案、`hello_caps_flavor_ambiguity_is_fail_soft` 五态、`alt_flavor_declared_per_leg` 两腿分叉、`input_for_missing_leg_dropped_with_count`）；**D-10 条目被本条取代**（宿主推断降为缺省）；tier 侧 App 需置 `KEY_ALT_ESC_PREFIX`（**上报项**） |
| 2026-10-08（Q-J 批落地） | **CA11（launchd）触发集 + 非相关分支文案** | 判定输入集：「仅默认 state 等 KeepAlive」→「plist **内容**提及该 state（`--state DIR`/`--state=DIR`，两侧同一 normalize）⇒ 等；零参形态 + 默认 state ⇒ 等（Q-H F14 旧判据保留，**不引入 argv[0] 谓词**）；未知形态 ⇒ 保守退化 = Q-H 行为；**仅提别的 state ⇒ 不等（默认 state 亦然——同一格，代码门 L3 补记）**）」；非相关分支文案 `（state=<dir> 非默认 state——不等 launchd KeepAlive，直接拉起）` → **`（未在册 launchd 代理提及 state=<dir>——不等 launchd KeepAlive，直接拉起）`**（旧文案在「默认 state 但无相关 plist」时说假话；**该行仅 darwin 打**——Linux 无 launchd，Q-H 形态此情形无输出，代码门 L4）；等待分支文案不变 | F6：Q-H F14 落地后「KeepAlive 仅默认 state」的假设与实机不符——生产 plist 实为 `--state ~/.config/homeway-rs`（自定义 state）⇒ 该 state 下 CLI 不再等 KeepAlive 而直接自拉起 | CA11 行（本表已注记）、`daemon_cli.rs`（`detect_launchd_agent_for_state`/`detect_in_dirs`/`parse_program_arguments`/`normalize_state`）、单测 3 例（四态 / 未知形态两态 / 分隔与 normalize + 生产夹具逐字）；**与 tier `role-management:184-188`「或本机任意 homeway 代理 plist」检测集合有 spec 张力**（上报项） |
| 2026-10-08（Q-J 批落地） | **UPnP 协议面（F4）——与 Go 的四条有意分歧 + E20 族数值语义** | ① SSDP 钉卡失败：硬失败终止候选 → **降级继续**（记行；发送已由 IP_MULTICAST_IF 钉在源卡，接收可能被默认路由/TUN 抢走）；② M-SEARCH ST：单发 `IGD:1` → **每轮固定三发 `IGD:1`/`IGD:2`/`upnp:rootdevice`**（MX=2 不变）；③ 应答采纳：「首应答即信（不看 ST/USN）」→ **判定表**（三条件 + ST/USN 精确段匹配 IGD 型 ⇒ 立即采信；非 IGD 型 ⇒ 待定，**只在 SSDP 腿期限到点后**回退采信）；④ 新增 **`AddAnyPortMapping` 末位兜底**（显式候选 prefer→internal→+1…+9 全失败后：期望端口交路由器改派、`NewReservedPort` 可解析且 ≠0 才算成功、租期 3600→0 回退；成功 ⇒ 外部端口由路由器选择——修前该形态 `NoPortAvailable`）。**既有成功路径零变化**；新增两行 additive 告警（钉卡降级 / A-any 成功）。**spec 偏离补记（代码门 M2）**：A-any 的触发条件与 tier `exit-upnp-port-mapping:32-35` 的 MUST（「候选全被占用/拒绝时 MUST 走映射失败路径 + 打『未取得端口映射』行」）WHEN **完全重合**——A-any **成功**时不再走该失败路径、外部端口改由路由器决定 ⇒ 定性与 `dns_upstream`/`dns_fallback` 同为「**opt-in 等价偏离**」（仅当路由器支持该动作时生效；不支持/失败 ⇒ 原失败路径与计数不变）；与 F6↔`role-management:184-188` 的 spec 张力同批上报（`docs/reviews/QJ.md` 上报项） | F4（超 Go 加固，逐条登记）：Go 基线 `internal/server/upnp.go:29/138-139/161-163/122-131` 同形旧形态 | E20 族数值语义（A-any 成功形态）、`upnp.rs`（ST 三态/`ssdp_is_igd_type`/`ssdp_location_with` 待定回退/`add_any_port_mapping`/`pin_multicast_with` 降级）、单测 `msearch_shape_three_states`/`ssdp_igd_type_matching`/`add_any_port_mapping_last_resort`/`shrink_path_unaffected_by_any_mapping`/`multicast_pin_failure_degrades`；**失效形态影响**：多候选/混答 LAN 与 A-any 消耗共享 40s 预算（`QJ-design.md` F4 风险②③） |
| 2026-10-08（Q-J 批落地） | **F2 五配置键（additive）+ 两处 spec opt-in 偏离 + config.toml 对 Go 单向** | 无 → 有（`[serve]` 五键，默认 = 修前硬编值逐值不变）：`dns_upstream`（显式上游覆盖；空 = 跟随 `/etc/resolv.conf`）、`dns_fallback`（缺省 `223.5.5.5`；**空串 = 拒启**，不是「关兜底」）、`ddns_resolver`（缺省 `223.5.5.5:53`/`119.29.29.29:53`）、`dns_probe_target`（缺省 `223.5.5.5:53`/`1.1.1.1:53`；**一键喂挑卡/健康探针/udpcap 三路 = 显式登记耦合**）、`stun_probe_target`（缺省 CF/Google；**须显式带端口**）。**两处 opt-in 偏离**（默认面不动）：`dns_upstream` ↔ tier `wg-native-dns:40` MUST（系统解析配置跟随）；`dns_fallback` ↔ `:77` SHALL（223.5.5.5）——仅显式配置时偏离。**env 缝纪律**：`HOMEWAY_BINDWATCH_PROBE` 只影响健康探针，挑卡恒吃 config/默认。`dns_upstream` 覆盖生效行（进 E4 取值）另告警**恰一次**（`DnsProxy::spawn`：fake-ip 主机上手机拿真实 IP、域名规则失效——代码门 M1 补齐，含测试断言）；**五键端口 0 一律拒启**（值域，代码门 L2）。**不加 CLI flag**（配置面足够，登记「不加」）。新键使 config.toml 对 Go 侧（`Undecoded()` 检查）**单向不兼容**（Go 已退役，仅影响回滚/对照） | F2（P1：DNS 上游/探针目标硬编码 CN 段 + fake-IP 无卫兵；「统一」落点按 tier spec 收窄——dnsproxy 侧**不得**做拒绝式卫兵，见 F3） | `serve_cli.rs`（五键 + 值域 + 边界解析）、`engine.rs`（`ServeConfig` 五字段 + 装配）、`dnsproxy.rs`（`Upstreams::with_static` 静态覆盖不做 mtime 跟随）、`ddnscheck.rs`（`resolvers` 穿参）、`egress.rs`（`preferred_iface(route_probe)`）、`bindwatch.rs`（`pick_targets`/`health_probe_targets_from_env`）、`nodestate.rs`（模板键表）；单测 `f2_keys_defaults_unchanged`/`f2_keys_take_effect`/`static_upstream_override_no_follow`/`fallback_from_config`/`defaults_unchanged_by_new_keys`/`pick_targets_ignore_env_seam`；**上报项**：两处 spec 偏离需 tier 知会/修订 |
| 2026-10-08（Q-I 尾段批落地） | **F2（reactor 并入引擎 poll）尝试后回退——零残留（登记留痕）** | 曾实现「reactor 兴趣集并入引擎唯一 poll（快照直派）+ `intercept: reactor 观测 … 兜底=N` 字段 + 两条唤醒时点行为变更」→ **实测负收益后整条回退**（代码与判据面回到 Q-H 形态；`兜底=` 字段与两条行为变更**未落地、不登记**） | F2 止损闸门（设计 §4.3）：两项 poll 样本合计 **+18%**（不降反升）、进程 CPU **+14.5%**、up 吞吐 **−5.9%**；机制 = 上游 fd 就绪成为引擎唤醒源 ⇒ 拍频 13.2k→21.4k/s、每拍全量 pump 的固定成本放大 | **本行不涉任何判据行/wire/夹具变更**；证据与数字见 `docs/reviews/QIt.md` 性能节 + `docs/PERF-AB.md`；`server/engine.rs`/`server/intercept/mod.rs` 内注释留痕（`reactor_turn` 文档注释） |
| 2026-10-09（M1 S3/S4 批落地；含 S1a–S2b 前序交下条目） | **C2（隧道侧就绪行）——QUIC 档新增前缀行** | `wgcore: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 B 拨隧道 IP）` → **新增同义行** `quic: 隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v，核心自连经 WG 拨隧道 IP）`（**WG 档原串保留**） | M1 的 L3 承载在 quic 档由 QUIC 岛承担；末句「经 B」在 quic 档写成「经 WG」（M1 的核自连仍在 WG/栈 B 上，M3 才退役栈 B）。**实施期订正**：`wgcore:` 串在 quic 档**仍会打出**（同世代两条传输并存：WG 客户端照常承载服务面/核自连——设计 §2.6）⇒ 验收口径 = 「两串都合法」，quic 档 `quic:` 串**必在**、`wgcore:` 串「在不在都不算违约」 | `docs/INTEROP-CRITERIA.md` C2 行、`crates/homeway-core/src/facade/tun_exec.rs`（gen_loop）、tier（App 核日志消费方）；证据 = `tools/quic-island-e2e.sh` 的 `generation_l3_rides_quic_datagram_against_local_exit`（C2' 在场）+ `tools/quic-wg-e2e.sh`（wg 档原串在场） |
| 2026-10-09（同上） | **C8 值域扩展（暖机判据位）** | `warmup pong: 就绪（判据=wg）` → **值域 `{wg, quic}`**：quic 档 = `warmup pong: 就绪（判据=quic）`（依据位 `facade/tun_exec.rs` 的暖机段；`readyBy` 字段同源 = `quic`） | 暖机判据位在 quic 档由岛 `Cmd::Probe`（QUIC STREAM 回显）满足；`判据=wg` 在 quic 档是假话。**CLI 形态不动**（`homeway-cli` 不走 quic 档 ⇒ 恒 `wg`） | C8 行、`facade/tun_exec.rs`、`crates/homeway-cli/src/main.rs`（不动）；按 `判据 ∈ {wg,quic}` 写断言的脚本须放宽（此前按 `判据=wg` 严格匹配） |
| 2026-10-09（同上） | **C15（注册刷新行）——QUIC 档新增前缀行** | `RREG 注册刷新 → %v（dev=%s，中继=%v）` → **新增** `quic: 注册刷新 → %v（dev=%s，中继=%v）`（WG 档原串保留） | quic 档的登记面 = QUIC 控制流上的 reg 刷新（无 WG 的 RREG 语义；节拍沿用 60s 巡检）；代层的 WG 补注册在 quic 档**不发**（避免双登记） | C15 行、`wtransport/bind.rs`（WG 档不动）、岛侧 `client/register.rs`、`facade/tun_exec.rs`（quic 档跳过 `refresh_reg_result_bounded`）；证据 = 岛侧单测 + S2b 中继 E2E |
| 2026-10-09（同上） | **新增 C 族行 N-a…N-d（additive）** | 无 → 有：`quic: 端点就绪（本地 %v，MTU %d，max_datagram_size=%d）` / `quic: 迁移完成（%v → %v，耗时 %v）` / `quic: 丢弃 超限=%d 发送缓冲满=%d 回程队列满=%d 未登记=%d（明细分行：首次 + 每 100 次）` / **N-d** `transport: 本世代 L3 承载 = %s（A/B 开关：env HOMEWAY_TRANSPORT / tunConfig.transport；回退 = wg）` | quic 档需要自己的就绪/迁移/丢弃/A-B 观测面（「丢弃可观测」与「迁移保持」两个 M1 判据的落点）；N-d = 观测面一致性的锚（世代装配时一行） | 新行读者 = 排障脚本 / M1 判据证据；不影响既有读者。落点：`crates/homeway-quic/src/driver.rs`（N-a/N-b/N-c）、`crates/homeway-core/src/facade/tun_exec.rs`（N-d） |
| 2026-10-09（同上） | **新增 E 族行 E-q1…E-q4（additive）** | 无 → 有：`quic: 端点就绪（%v，migration=%v，initial_mtu=%d，datagram 缓冲 %dB）` / `quic: 连接采纳 dev=%s tun=%v ← %v` 与 `quic: 路径变更 dev=%s %v → %v` / `quic: 丢弃 超限=%d 发送缓冲满=%d 未登记=%d 源校验拒=%d` / `UPnP：QUIC 端口 %d 映射 %s`（成/败都打，fail-visible） | 出口侧 QUIC 面（§1.1/§1.7）的可观测面；E-q2 是「迁移不新增设备表条目」的出口侧证据行，E-q4 是公网 QUIC 端点的可达性判据 | 出口排障读者（新行不进状态 JSON 既有字段）。落点：`crates/homeway-quic/src/exit/mod.rs`（E-q1/E-q2/E-q3）、`crates/homeway-core/src/server/engine.rs`（E-q4） |
| 2026-10-09（同上） | **E3 取值来源（token 端点表）——新增 QUIC 类端点** | 端点表只有 WG/中继/域名类 → **新增一类**（`<host>:<quic_port>`，后缀 `（QUIC 内网/QUIC 公网）`）；wire `type=2`（`EndpointKind::Quic`，additive；未知值宽松按 Direct 收的既有语义不变） | QUIC 端口与 WG 端口不同（设计 §1.1）⇒ 客户端必须从 token 学到它；WG 档候选过滤（`token::wg_endpoint_refs`）保证两族候选不互相投喂 | E3 行、`crates/homeway-core/src/token.rs`、`server/{state,engine}.rs`、`tools/local-*.sh` 的 token 断言；tier App **不改**（只透传字符串） |
| 2026-10-09（同上） | **token 新增「服务端 RPK 公钥」尾字段（additive）** | token 载荷 `… ‖ epCount ‖ [type+len+addr]*` → `… ‖ [rpk(32B)]? ‖ crc32`（**可选尾字段**，不带 = 逐字节同 M1 前） | 客户端钉定出口身份（RFC 7250 RPK；设计 §1.3/§12-②）必须从 token 拿到公钥；`serve.quic=false` 时**不打**该字段 ⇒ token 串与 M1 前逐字节相同 | `token.rs`（encode/decode/`TokenSpec.rpk`）、`serve` 铸造与 `serve token` 渲染、`crates/homeway-quic/src/exit/rpk.rs`（出口身份源）；证据 = `fixtures/vectors/token.json`（Go 冻结向量，逐字节对账）+ `tests/quic_wg_e2e.rs::serve_quic_false_token_is_byte_identical_to_pre_m1`（再编码逐字节等于原串） |
| 2026-10-09（同上） | **准入绑定口径（非编号判据行；安全面登记）** | 「token reg2 MAC（`"H2"`/`hr-reg2`）+ WG 握手绑定 pubkey」→ **`hr-reg3`：MAC 混入 TLS exporter 连接绑定**（`mac = HMAC(secret, "hr-reg3" ‖ pubkey ‖ devTag ‖ ts ‖ export_keying_material(out,b"hw-quic-reg",b"")[:32])[:16]`），走 QUIC 的第一条双向控制流；**服务端身份** = Ed25519 RPK（公钥进 token + 客户端钉定，M2 交付物的服务端半边提前到 M1）；客户端证明半边（Hello/Challenge/Proof/抗放大）仍留 M2 | ①去掉「pubkey 必须完成 WG 握手」后，仅凭 MAC 的 reg 帧在 ±90s 内可被**重放**成一条可用隧道连接（今日重放只能污染设备表）⇒ 必须连接绑定；②客户端必须能验证服务端（产品面禁 `dangerous()`/SkipVerify） | `crates/homeway-quic/src/reg3.rs`、出口 TLS 配置（`exit/rpk.rs`）、岛侧登记路径（`client/register.rs`）、**威胁模型（M2 必含条目）**；tier 侧无需改代码；reg2/reg3 版本混用在 M2 前台并存或按用户拍板①（无兼容包袱）直接切 reg3 |
| 2026-10-09（同上） | **隔离门第 ⑤ 条口径变更（`dangerous()` 允许面）** | 「`crates/` 内 `dangerous()`/`with_custom_certificate_verifier` **零命中**」→ **收窄到单文件**（`crates/homeway-quic/src/exit/rpk.rs`），且该文件**必须**出现 `verify_tls13_signature_with_raw_key`（钉定 ≠ 跳过验证）；其余 `crates/` 仍零命中 | rustls 0.23 公开面里装自定义 verifier **只能**经 `dangerous().with_custom_certificate_verifier(...)`（`with_webpki_verifier` 只收 `WebPkiServerVerifier` 具体类型）⇒ 原口径与「客户端 RPK 钉定」不能同时成立（设计 §12.6-1 主会话裁定） | `tools/check-quic-isolation.sh` 第 ⑤ 条（白名单 + 同文件自证，fail-closed）；语义 = 「产品面不得出现跳过验证」这条纪律**没有放松**，放松的只是「哪个 API 能承载钉定」 |
| 2026-10-09（同上） | **配置新键（additive，六个）** | 无 → 有：出口 `serve.quic`（缺省 **true**；false = 不监听 QUIC 端口 + 不打 E-q1/E-q4 + token 不带 QUIC 端点/RPK）与 `serve.quic_listen`（缺省 `serve.listen+1`，退让语义同 WG）、`--quic[=bool]` flag；客户端 `tunConfig.transport`（`quic` 缺省 \| `wg`）与 `tunConfig.quicMtuCap`（缺省 1400；有效区间 [1320,1400]）；env `HOMEWAY_TRANSPORT`（quic\|wg\|1\|0；**优先级高于 config**）与 `HOMEWAY_QUIC_MTU`（同上，优先级高于 config） | A/B 开关双面（env = 进程级一键回退；config = 世代级 App 可控，设计 §4.1）+ MTU 上限旋钮（设计 §12-① 候选 A）。非法值一律「记行 + 按缺省」：开关落 quic、MTU 落 1400（越界不夹取——意图不明按缺省最不易静默劣化） | `crates/homeway-cli/src/serve_cli.rs`（键/flag/值域）、`server/engine.rs`（`ServeConfig.quic`）、`facade/mod.rs`（`TunConfigJson` 两键）、`nodestate.rs`（模板键表）、`crates/homeway-core/src/envflag.rs`；config.toml 对 Go 侧单向不兼容（Go 已退役，仅影响回滚/对照）；单测 `quic_switch_config_key_and_flag` / `bearer_switch_parses_values_and_defaults_on_garbage` / `mtu_cap_resolution_clamps_by_default_policy` |
| 2026-10-09（同上） | **S3-1 世代装配新增行（quic 档，additive）** | 无 → 有：`quic: 岛已建连（候选 %d 个，胜出 %s %v，耗时 %dms）—— L3 承载 = 岛` / `quic: 岛未就用（%s）——本世代回落 WG 承载（L3 与判据行按 WG 档；下一世代重试）` / `quic: 本世代 L3 回落 WG 承载（判据行/观测面随档位切换）` / `quic: 面未启用（serve.quic=false）—— 不监听 QUIC 端口、token 不带 QUIC 端点（客户端将回落 WG）` | 世代级装配的**归因面**：岛成/不成、回落 WG、出口侧 QUIC 面关闭都要看得见（不静默）；「配置不一致形态」（`serve.quic=false` × `transport=quic`）的验收落点 = `岛未就用（…候选为空）` + `本世代回落 WG 承载` | `facade/tun_exec.rs`、`server/engine.rs`；证据 = 单测 `wg_mode_has_zero_quic_lines_and_quic_mode_falls_back_without_endpoints` / `quic_mode_constructs_island_and_reports_race_failure` + `tools/quic-wg-e2e.sh` 的 quic-off 断言 |
| 2026-10-09（同上） | **S3-1「Rebind 优先」处置行（quic 档，additive）** | 无 → 有：`quic: 迁移未确认 ⇒ 先试 Rebind（换本地 socket 保连接；设计 §2.3 的新增动作）` / `quic: Rebind 完成（→ %v），等对端回包证路径` / `quic: Rebind 后探活通过 —— 连接保持（不拆世代）` / `quic: Rebind 失败（…）—— 回落重连/重赛跑` / `quic: Rebind 后 %s 仍探不活（…）—— 回落重连/重赛跑（交扩展重建）` / `quic: %s（连接不在/迁移未确认位未置）—— Rebind 无可保之物，交扩展重建` | 设计 §2.3：网络路径变化时 **`Rebind`（保连接）优先于重连/重赛跑**。动作面：岛判死（`patrol`）或巡检 3 连败时先试换本地 socket + 探活确认；不成则**回落既有路径**（M1 不重写阶梯 ⇒ `mark_unhealthy_if_current(gen,"patrol")` 交 App 重建）。**单飞**：并发信号合并（`InFlight` 不重复动作） | `facade/tun_exec.rs`（`quic_rebind_first`/`quic_unhealthy_signal`/巡检查分支）；不新增 unhealthyReason 取值（仍 ∈ `{patrol,fd,panic,stop}`）；真机换网的 N 拍确认口径归 M6 |
| 2026-10-09（同上） | **岛侧行族（S2a/S2b 落地，本批一并登记）** | 无 → 有：`quic: 端点就绪（…）` / `quic: 赛跑投出 %d 个候选（直连 %d / 中继 %d；本行每轮限 3 条）`（C4'）/ `quic: 赛跑结算：胜出 %s %v（候选 %d 个，耗时 %v）；完成=<端点清单>；未完成=<端点清单>`（C5'）/ `quic: 路径确立：%s %v（首个完成握手）`（C6'）/ `quic: 岛已建连…`（S3-1）/ `quic: 注册刷新 → %v（dev=%s，中继=%v）`（C15'）/ `quic: 迁移完成（…）`（N-b）/ `quic: 迁移未确认（…）` / `quic: 忽略非 kind=5 腿帧（…）` / `quic: 窄路径不可用 —— max_datagram_size=%dB < 内层 MTU=%dB…` / `quic: 隧道面已附加（fd=%d, mtu=%d；…）` / `quic: 到点 detach —— 老世代仍持 UDP 源端口 %v、连接 %d 条…` / `quic: 岛收工（连接面随端点关闭）` / `quic: 连接已断 —— 等上层重连/重赛跑（阶梯接线 = 世代层）` / `quic: 替换旧连接（旧连接已 CONNECTION_CLOSE）` | C4'/C5'/C6' = QUIC 档的赛跑族（语义保留、行文改写；**WG 域 C4/C5/C6 原样保留**——服务会话仍走 `wtransport::Bind`）；其余 = 数据面/生命周期/巡检的可观测面（S2b 的判据落点） | 新行读者 = 排障脚本 / `docs/reviews/M1.md`；`crates/homeway-quic/src/{driver.rs,client/**}`；证据 = `tools/quic-island-e2e.sh`（三条用例）+ `crates/homeway-quic` 单测 |
| 2026-10-09（同上） | **实现口径偏离（S1c 三条 + S2b 一条，一并登记）** | ①腿 `WouldBlock`：丢 + 计数（**不**返 `WouldBlock` 给 quinn）；②摘腿不回落直连（腿摘除是中继面的事，直连 socket 另一条路）；③`may_fragment() = true` 等价（不承诺分片能力位）；④`Cmd::TunPacket` 在途闸 = 原子计数 4096（**非**有界通道类型；满 ⇒ 丢新，丢弃归 `未登记`） | 前三条 = 自定义 socket 的语义选择（保持「不静默」与既有腿表语义）；第四条 = 设计 §6.4「TUN 读线程 → 岛：有界通道 4096 条」的**形态偏离、语义等价**（不动控制面命令通道的前提下等价实现；严格复刻有界通道类型需把控制面命令拆第二条通道） | `crates/homeway-quic/src/{exit/socket.rs,client/relay_sock.rs,tun.rs}`；残余 = 尾批 ≤63 条丢弃在岛收工后无人可报（登记在案）；单测 `tun_inflight_is_bounded_and_reopens_after_release` |
| 2026-10-09（同上） | **`quic` 段（状态 JSON）/ `migrations`+`migration_unconfirmed` 新字段 / `link` 来源切换** | ①`tunStatusJSON`：新增**平级 additive 段** `quic{mtu,current_mtu,lost_packets,congestion_events,migrations,migration_unconfirmed,drops{too_large,send_buffer_full,return_queue_full,unregistered},via,ep,rtt_ms,packets_in,packets_out,local,connections,relay_tx,rx_ignored,candidates,mirrors}`（岛不在 = 整段缺席）；②`link{via,ep,rttMs,at}` **键序/形态不变**，`via` 的**来源**在 quic 档改为岛快照（词表仍 `direct|relay|none`）；③`demand.localErr*` 两键在 quic 档缺席（无等价物，additive 兼容） | 设计 §4.3/§12-①④：可观测面与 MTU 旋钮的状态暴露（`maxDatagramSize` = `mtu`、`currentMtu` = `current_mtu`）；「丢弃可观测」既可 grep（N-c 行）又可程序读（`drops`）——**同源**（同一份 `IslandSnapshot::drops`） | `facade/tun_status.rs`（`QuicIn`/`QuicDropsIn` + JSON 渲染）、`facade/{mod,tun_exec}.rs`、tier App（只校验既有键存在 ⇒ 零影响）；单测 `quic_section_is_additive_and_complete`/`quic_section_maps_drops_from_island_snapshot`/`quic_section_requires_quic_bearer_and_island`；证据 = `generation_l3_rides_quic_datagram_against_local_exit`（真世代 JSON 读数） |
| 2026-10-09（同上） | **已知 flake 登记：`wtransport::bind::tests` 的实 socket 时序族** | 无 → 有（登记为**已知 flake**，非契约变更） | S2b 实测 `mirror_then_adopt_then_single_send` 一次、M1 S3/S4 批全量并行跑实测 `relay_envelope_and_adoption` 一次（均隔离复跑绿）；同批全量并行跑另有既登记族两例（`daemon::tests::handshake_deadline_beats_slow_drip`、`term::service::tests::attach_size_applies_to_pty`）——**三例 `--test-threads=1` 隔离复跑全绿**；该文件 = M1 红线面零改动；flake 口径照 M0 §9.2 ④（红了先隔离单跑再判回归） | 判据行/wire/夹具**零变更**；CI 复跑若再现按 flake 口径处置（不静默重跑，登记在案） |
| 2026-10-09（M1 S6 代码门补登） | **Q-O 资源上限三闸的拒绝行（additive，补齐登记）** | 无 → 有：`quic: 拒新连接（连接总数 {held}/{conn_cap} 超限，来自 {peer}；第 {n} 次）` / `quic: 拒新连接（并发握手 {in_flight}/{cap} 超限，来自 {peer}；第 {n} 次）` / `quic: 握手期限（{peer} 未在 {deadline:?} 内完成，已弃；第 {n} 次）` | 设计 §9.3 Q-O 明确要求「超限**记行**」——Q-O 的资源上限是 M1 的**安全面/资源面判据**，其证据面就是这三行（S1a 已落地但 S4 登记遗漏，代码门 r13 的 D1 点出）| 出口排障读者；落点 `crates/homeway-quic/src/exit/mod.rs`；计数 = `ExitQuicSnapshot::{conn_refused,handshake_refused,handshake_timeouts}`（三条原子计数同源）；单测 `connection_cap_refuses_beyond_two_max_devices`/`handshake_cap_refuses_extra_in_flight`/`handshake_deadline_drops_stalled_handshake` |
| 2026-10-09（M1 S6 代码门补登） | **准入被拒行（additive，补齐登记）** | 无 → 有：`quic: 准入被拒（dev={dev} ← {peer}；{why}；第 {n} 次）`（岛侧）= `exit/conn.rs::reject` 的唯一出口；`quic: 准入被拒（dev={dev} pub={pub}；hr-reg3 MAC 不符——含换连接重放）`（出口侧，`server/engine.rs` 的引擎裁决面） | 准入面（`hr-reg3` + TLS exporter 连接绑定）是 M1 的**安全面判据**，其拒绝必须 fail-visible（设计 §1.3）；S4 登记遗漏（代码门 r13 的 D1）| 安全面排障读者；落点 `crates/homeway-quic/src/exit/conn.rs`、`crates/homeway-core/src/server/engine.rs`；计数 = `regs_rejected`（岛/出口两侧同源语义）；单测 `reg3_bad_mac_is_rejected`/`reg3_replay_on_another_connection_is_rejected` |
| 2026-10-09（M1 S6 代码门补登） | **装配 / 生命周期 / 数据面归因行族（additive，补齐登记）** | 无 → 有：①岛侧 `quic: 赛跑小结：无胜者（…）`（D1 点名的清单项）/ `quic: 赛跑未成（{e}）` / `quic: 候选清单已更新（{n} 条）` / `quic: 本地 socket 已换绑（{from} → {to}）` / `quic: 换绑失败（{from} → …；{e}）` / `quic: 岛内 panic（…）` / `quic: 岛线程 panic（…）`；②岛面登记 `quic: 登记已发（dev={dev}，{n}B；等准入窗 {w}）`；③出口面 `quic: 拆连接（dev={dev} 已从设备表摘除/轮换）` / `quic: 替换旧连接（dev={dev}；旧连接已 CONNECTION_CLOSE）` / `quic: 路径变更（未登记连接）{from} → {now}`（E-q2 的变体）/ `quic: {EXIT_THREAD} 线程 panic —— 本世代 QUIC 面已死` / `quic: 出口 QUIC 面 panic（{msg}）—— 判不健康，线程退出（不复用该线程）`；④装配面 `quic: 端点未起（三形态）` / `quic: MTU 上限取值 {raw}（{from}）非法或越界…按缺省 {default} 走` / `quic: 岛附加失败（{note}）—— 尝试 WG 兜底` / `quic: 出口 QUIC 面线程已退出 —— 后续出站回落 WG 原样、入站停止…` / `quic: 出口 QUIC 面未在收工预算内退出 —— 已 detach`；⑤腿面 `quic: 腿（→ {remote}）发送句柄克隆失败…` / `quic: 腿上的 QUIC 报文无法投递（出口 QUIC 面不可用…）——已丢 {c} 个` | 「丢弃/失败可观测、不静默」是 M1 的硬口径（设计 §6.4）；这批行是「排障要看得见」的归因面，但 S4 登记遗漏（代码门 r13 的 D1）| 排障脚本 / `docs/reviews/M1.md` 读者；落点 `crates/homeway-quic/src/{driver.rs,client/register.rs,exit/bridge.rs}`、`crates/homeway-core/src/{facade/tun_exec.rs,server/engine.rs,server/bind.rs}`；**均为 additive 新行，WG 族行零改动** |
| 2026-10-09（M1 S6 代码门补登） | **N-c 明细行节流口径（登记文案订正）** | 原登记文案「（明细分行：**首次** + 每 100 次）」→ **实际实现 = 「首 3 + 每 100」**（`driver.rs` 的 `note_drop` 与 `exit/bridge.rs` 同值，与设计 §3.3 N-c 原文一致） | 登记文案抄写误差（代码/设计一致，登记不一致）；代码门 r13 的 D3-a | 仅登记文案；N-c 行的**字段名与顺序不变**（`超限/发送缓冲满/回程队列满/未登记`）；证据 = `crates/homeway-quic/src/driver.rs` 的 N-c 行文案「计数行首 3 + 每 100」 |
| 2026-10-09（M1 S6 代码门补登） | **`L3Bearer::parse` 的接受集（登记补充）** | 设计 §4.1 写的合法集 `quic|wg|1|0` → **实现额外接受 `true`/`false`**（同义别名，大小写不敏感归一到同一枚举） | 代码门 r13 的 L2：实现宽于登记的接受集**必须写明**（否则验收方按登记子集写断言时漏掉别名形态）| `tunConfig.transport` 的所有消费方；落点 `crates/homeway-core/src/facade/tun_exec.rs`（`L3Bearer::parse`）；单测 `bearer_switch_parses_values_and_defaults_on_garbage` |
| 2026-10-09（M1 S6 代码门补登） | **隔离门断言面扩展（第 ⑥–⑩ 条 + 第 ② 条口径收窄）** | ①新增 ⑥裸 `send_datagram(` 零命中（作用域 = `crates/**`，只许在两处 `send_datagram_checked` 函数体内；测试文件除外）；⑦`send_datagram_wait` 零命中（同作用域）；⑧`relay/**` + `relaywire.rs` **整文件**剥注释后 `quinn|tokio|rustls` 零命中；⑨数据面单线程前提三条可判定事实（零 `new_multi_thread` / tokio 未开 `rt-multi-thread` / 两处包装为非 async 且体内零 `.await`）；⑤harness 侧 `dangerous()` 必顶 `SECURITY: harness-only` 标记。②第 ② 条白名单由**目录前缀**（`client/**`+`exit/**`）改为**显式文件清单**（新增即改门）；③注释剥离升级为六态状态机（字符串里的 `//` 不再截断本行——旧 `sed` 口径存在「一行内藏 `quinn::`」的真绕过面），并支持 `/* */`（旧口径对块注释里的 crate 名会假红） | 代码门 r13 的专项②（B1–B6）：四条离析 grep 面方向正确但**作用域/可表达性**需改口径，否则会出现「假红 + 漏检」两种失效 | `tools/check-quic-isolation.sh`（九条断言；失败即非 0）；语义 = 「产品面不得裸发 DATAGRAM / 不得阻塞等待 / 中继不得沾异步栈 / 单线程前提可判定」，**不是**新增行为约束 |
| 2026-10-09（M1 S6 代码门补登） | **实现口径偏离（S6 整改两条，additive）** | ①**quic 档的环境噪声源按档分流**：`patrol_loop` 的噪声第二路（`last_local_send_err` 15s 尾窗）由「quic 档也读」→ **只在 WG 承载下读**（quic 档该信号恒 false）——此前 quic 档会拿 **WG 腿**的本地发送错误把 QUIC 的巡检失败门控成「环境噪声」（`fail_streak` 清零，最长压到 `NOISE_ESCALATE_AFTER=180s`），与设计 §2.5「只以**当前承载**的探活结论驱动处置，另一条腿的失败只记行」相抵；修复后与同文件 `pusher_loop` 的 `has_fresh_local_err`（已带 `!l3_on_island()`）同构。②**回程丢弃的归因细化（非判据行、计数字段不变）**：TUN 写线程已退（消费者消失）时，回程泵不再把丢弃计/打成 `回程队列满`，而是计 `未登记` + 一次性记行 `回程面已终止（TUN 写线程已退）—— 回程泵收口` 并退出（此前为「同归队列满」（旧注释「丢弃语义相同」）——队列没满而是没有消费者，排障会误判） | ①代码门 r13 的 H1（高，建议阻塞：与 M1 判据「断线恢复 ≤3.5s」直接冲突）；②代码门 r13 的 A3（中：归因与事实不符）| ①`crates/homeway-core/src/facade/tun_exec.rs`（`patrol_loop` 的 `local_noise` 第二路）；②`crates/homeway-quic/src/{tun.rs,client/dataplane.rs}`（`PushOutcome::{Pushed,Full,Gone}`）；证据 = 单测 `return_push_reports_gone_when_consumer_exits`/`return_pump_reports_write_thread_gone_and_stops`/`return_pump_restarts_after_connection_replacement`（后者在旧语义下**确定性红**） |
| 2026-10-09（**M2 S1–S3 落地；S4 登记**） | **准入协议版本（`hr-reg3` → `hr-reg4`）**（非编号判据行；**安全面 + 线协议面登记**） | `H3` 单帧（`mac = HMAC(secret,"hr-reg3"‖pubkey‖devTag‖ts‖exporter32)[:16]`；`reg3.rs` 整文件退役）→ **`H4/C4/P4/A4` 四帧**（Hello 50B / Challenge 18B / Proof 82B / Accept 2B；`mac = HMAC(secret,"hr-reg4"‖pubkey‖devTag‖ts‖nonce‖exporter32)[:16]`）+ **刷新帧 `R4`**（66B；域标签 `hr-reg4-refresh`——与 Proof **不可互冒**）；`exporter32` 标签沿用 `hw-quic-reg`（**不是**帧版本号）；**`H2/H3` 起被拒**（归因串 `帧版本不符（H2/H3——旧核或垃圾包）`） | 设计 §1.1/§1.2（**替换不并存**：无兼容包袱 + 不翻倍未认证生命周期）；nonce 一次性 + 未认证状态有界（`ADMIT_DEADLINE`/`NONCE_TTL`）+ 确定性 `A4` 回执替代经验窗 `REG_SETTLE=400ms`。**准确口径（r14 F2/F6 订正，不得夸大）**：nonce **不**提供对「未持 secret 者」的新抵抗力（唯一密码学加分 = 「连接密钥泄露但 secret 未泄露」档的纵深）；Hello 的收益 = 「MAC 试秘**之前** + 不投引擎 + 不触设备表」的廉价拒绝面 | `crates/homeway-quic/src/reg3.rs`（删除）→ `reg4.rs`（新增：帧常量/域标签/`FrameHead::legacy_why`）、`crates/homeway-quic/src/exit/{conn,bridge}.rs`、`client/{register,race}.rs`、`crates/homeway-core/src/server/table.rs`（`match_reg3` → `match_proof`；**其余零 diff**）、`server/engine.rs`（`admit_reg3` → `admit_reg4`）；**tier 零代码**；旧核/新出口（或反之）**不互操作**；`fixtures/` 无该帧的字节夹具（M1 的 reg3 亦无） |
| 2026-10-09（同上） | **岛侧准入行改写 + 新增（行文改写之一）** | `quic: 登记已发（dev=%s，%dB；等准入窗 %s）` → **`quic: 准入已发起（dev=%s，Hello %dB；等挑战/回执）`**（旧串在产品代码/工具/测试中**零命中**；`REG_SETTLE` 字面量全仓零命中 = S6-4 的判定式）；**新增** `quic: 准入完成（dev=%s，耗时 %s）`（收 `A4` 后一次性行；**无「挑战 %d 次」字段**——r14 F22 删该分支）；**新增**（S2 交下）`quic: 准入失败（{why}；预算 {b}）—— 连接已显式关闭（不留悬挂）`（失败/到点 ⇒ `conn.close` + 记行——设计 §1.7-③ / r14 F8：「失败后不得留悬挂连接」的可观测落点；`{b}` = 实际预算，含 `max(剩余, ADMIT_MIN=2s)` 形态） | 四帧流程 + `REG_SETTLE` 退役（§1.7）；「准入完成」= 确定性回执到达；「准入失败」= 岛侧未认证状态的有界收口 | `crates/homeway-quic/src/client/{register,race,mod}.rs`（`close_on_failed_admission`）；排障脚本若 grep `登记已发` 须改；证据 = `crates/homeway-quic/src/client/tests.rs` 四帧序用例 + `tools/quic-island-e2e.sh`（`准入已发起` 行在场） |
| 2026-10-09（同上） | **出口侧准入/抗放大行族（additive；九串）** | 无 → 有（①–⑤ 节流照仓内「首 3 + 每 100」；⑥⑦ 为启动期各一次）：① `quic: 准入挑战已发（%v；在途未认证 %d/%d；第 %d 次）`；② `quic: 认证超时（%v 未在 %s 内完成证明——已弃；第 %d 次）`；③ `quic: 地址校验挑战（%v；在途未认证 %d/%d；第 %d 次）`；④ `quic: 握手洪泛拒绝（%v 在 %s 内第 %d 次尝试——已拒；第 %d 次）`；⑤ `quic: 证明失败闸（dev=%s 在 %s 内失败 %d 次——冷却 %s）`；⑥ `quic: 抗放大面（retry=%s；retry_token_lifetime=%s；每源 %d/%s；证明失败闸 %s）`；⑦ `⚠️ quic: retry_policy=always —— 常态每次建连/重连 +1 RTT（真机 LTE ≈50ms；§3.1 登记的代价，仅排障用）`；⑧ `quic: 抗放大策略被 env 覆盖（HOMEWAY_QUIC_ADMIT_RETRY=%s 覆盖配置 %s）`；⑨ `⚠️ quic: HOMEWAY_QUIC_ADMIT_RETRY=%s 非法（合法取值 %s）—— 记行后按 %s 走（不 fail-fast）` | 「未认证连接不占额度」「重连洪泛有界」「抗放大」三条判据的可观测落点（§1.6/§3.3）；⑥⑦ 把生效值与 `always` 档代价钉在启动日志；⑧⑨ 是 env 纪律（同 M1 的 `HOMEWAY_TRANSPORT`：非法 ⇒ 记行 + 缺省，**不 fail-fast**） | `crates/homeway-quic/src/exit/{mod,conn}.rs`、`crates/homeway-core/src/server/quic_admit.rs`；出口排障读者；**均 additive**；**行内分母 = `conn_cap`（`2×max_devices`）**——设计只写 `%d/%d`，此为实施期选择（设计 §13-5） |
| 2026-10-09（同上） | **`quic: 准入被拒（dev=%s ← %v；%s；第 %d 次）` 的归因集扩展（行文主字段不变）** | `why ∈ {帧格式非法（魔数/长度）, TLS exporter 不可得, 引擎裁决拒绝（见引擎侧归因行）}` → **实装取值集（逐串；`exit/conn.rs::reject` = 准入拒绝唯一出口）**：`帧格式非法（魔数/长度）` / `帧版本不符（H2/H3——旧核或垃圾包）` / `帧格式非法（首帧必须是 Hello）` / `帧格式非法（Hello 之后必须是 Proof）` / `帧格式非法（已绑定连接只收刷新帧）` / `重复 Hello（一个连接只接受一个 Hello）` / `已绑定连接的再准入` / `连接未绑定（绑定已摘）` / `刷新帧但连接未绑定` / `刷新帧与绑定身份不符` / `nonce 缺失/过期/已消费` / `TLS exporter 不可得` / `hr-reg4 MAC 不符——含换连接重放` / `引擎裁决拒绝（见引擎侧归因行）` / `刷新帧但设备不在册（已淘汰，不 resurrect）` / `证明失败闸冷却中（同 dev 短时多次 nonce/MAC 类失败——暂不发挑战）` / `挑战不可发（{e}）` | 四帧流程的失败面必须可归因：r14 F7（MAC 类 `why` 由引擎 verdict 携带——试秘在引擎侧）、r14 F1（`ts` 超 ±90s 窗 ⇒ 走 `引擎裁决拒绝` + 表内 `peer: ! reject reason=no-token` 双面）、**S1 交下的 §13-4 四串**（`帧格式非法（首帧必须是 Hello）`/`重复 Hello`/`刷新帧与绑定身份不符`/`连接未绑定`——fail-visible 拆细）、**S2 交下第 5 串**（`刷新帧但设备不在册（已淘汰，不 resurrect）`）、**S3 交下第 6 串**（`证明失败闸冷却中…`） | `crates/homeway-quic/src/exit/{conn,bridge}.rs`、`crates/homeway-quic/src/reg4.rs`（`FrameHead::legacy_why`）；计数 `regs_rejected`/`challenges_refused`/`proof_rejected` 语义不变，只扩 `why` **取值集**；出口排障读者 |
| 2026-10-09（同上） | **`ExitQuicSnapshot` / `quic` JSON 段新增字段（additive）** | 无 → 有：① 出口快照（S1）`challenges_issued`/`challenges_refused`/`proof_rejected`/`pending_expired`/`admit_timeouts`；② （S3）`retry_sent`/`flood_refused`/`proof_cooldowns`；③ （**S4 §14-1④**）`handshake_peer_closed`（`handshake_failed` 的**子集**；只归因、**不放宽**闸的计数集）；④ **`quic` JSON 段**（岛快照面，S2-5）新增平级键 **`send_buffer_used`**（瞬时量；无连接 = 0，满 = 1 MiB——M1 交下项 **N8①**） | §1.6/§3.3 的可观测面（程序读、与行同源）；N8① 的黑洞期在途缓冲可观测（连接终结时被 quinn 静默丢的那批仍无逐包计数——残余登记） | `crates/homeway-quic/src/exit/mod.rs`（快照 + 原子计数）、`crates/homeway-core/src/facade/tun_status.rs`（`quic` 段渲染 + 单测 `quic_section_is_additive_and_complete`）、`facade/tun_exec.rs`；tier 只校验**既有**键 ⇒ 零影响；按键存在性写断言的消费方按新增键放宽（additive） |
| 2026-10-09（同上） | **引擎侧准入归因行的协议版本串（r14 F4 补登项；行文改写之二）** | `quic: 准入被拒（dev={dev} pub={pub}；hr-reg3 MAC 不符——含换连接重放）` → `… hr-reg4 MAC 不符——含换连接重放 …`（`crates/homeway-core/src/server/engine.rs`） | 该串是 **M1 S6 补登的判据行**（本表 2026-10-09「准入被拒行」条）⇒ 协议换版本时**必改**（换版本不改串 = 静默破坏对齐） | `server/engine.rs`；出口排障读者；**WG 档不受影响**（WG 档 reg2 族行文中无此串） |
| 2026-10-09（同上） | **E-q2（`quic: 连接采纳 dev=%s tun=%v ← %v`）的频率/输入集** | 「每次 `Accepted`（含 60s 刷新帧）都打」→ **「仅首次准入打；刷新成功不重绑、不打 E-q2」**（**行文逐字不变**） | r14 F11 / §1.8：M1 的控制循环对每次 `Accepted` 都 `bridge.bind` ⇒ 今天每 60s 一条采纳行；M2 明确刷新语义（只走 `table.register` 的 Refreshed + C15' 行） | `crates/homeway-quic/src/exit/{bridge,conn}.rs`、`client/register.rs`；按「每 60s 一条采纳行」写断言的排障脚本须改；证据 = `crates/homeway-core/tests/quic_island_e2e.rs::island_connects_registers_and_survives_rebind_against_local_exit`（刷新后 `quic: 连接采纳` 计数仍 = 1） |
| 2026-10-09（同上） | **E-q3 明细文本增 `src=%v`（行文主字段不变；行文改写之三 / 可选登记条）** | `quic: 丢弃 超限=%d 发送缓冲满=%d 未登记=%d 源校验拒=%d（本次：源校验拒 src ∉ {tunnel_ip,tun_ip}（dev=…）；…）` → 明细补**实际**源：`（本次：源校验拒 src=1.2.3.4 ∉ {tunnel_ip=…,tun_ip=…}（dev=…）；…）`（非 IPv4 形态 = `src=非 IPv4（NB）`；**四字段计数行逐字不变**） | M1 真机发现①：`quic: 源校验拒 ≥3 次`而核侧 `drops` 全 0 ⇒ 无实际 `src` **无法定性**（r14 F21 认同增强；明细是自由文本 ⇒ 登记面 = 四字段计数行） | `crates/homeway-quic/src/exit/conn.rs`（`src_text`）、`exit/bridge.rs`（`note_drop`）；出口排障读者；按整行逐字匹配明细的脚本须放宽 |
| 2026-10-09（同上） | **配置新增段 `serve.quic_admit`（additive，七键 + 一 env）** | 无 → 有：`retry_token_lifetime`（缺省 5s；1s..=60s）/ `per_src_fails`（缺省 **16**；1..=1000——**缺省值变更见本表 S4 条**）/ `per_src_window`（缺省 10s；1s..=1h）/ `nonce_ttl`（缺省 5s；1s..=30s）/ `admit_deadline`（缺省 10s；1s..=60s）/ `proof_fail_threshold`（缺省 10；0..=1000，**0 = 关该闸**）/ `retry_policy`（pressure\|always\|never，缺省 pressure）+ env `HOMEWAY_QUIC_ADMIT_RETRY`（同三值）。**config 值域非法 ⇒ 拒启**（`serve` 节严格表纪律，Q-H 同款）；**env 非法 ⇒ 记行 + 缺省（不 fail-fast）**——两套纪律并存（设计 §3.2 表末写明） | §3.2：抗放大与限流的可配面（缺省即可用；`always` 档代价由启动告警行承担） | `crates/homeway-cli/src/serve_cli.rs`（键/值域/拒启）、`crates/homeway-core/src/server/quic_admit.rs`（env 叠加 + 记行）、`nodestate.rs`（模板键表）、`server/engine.rs`（`ServeConfig` 搬运）；config.toml 对 Go 侧**单向不兼容**（Go 已退役，仅影响回滚/对照） |
| 2026-10-09（同上；**主会话裁定见设计 §13-1①**） | **token 载荷布局：本批未变更（登记留痕）** | `hmw1` 布局**保持不变**（M1 已登的 `type=2` QUIC 端点类 + 可选 `rpk(32B)` 尾字段照旧）；M2 **未**新增/未改任何 token 字节、未动 `PREFIX`；`rl1` 中继 token 不受影响 | §12-① 是**用户拍板项**，未获拍板 ⇒ 按设计兜底取**候选 A（本批不动 token）**；候选 B（`hmw2` 段容器）的窗口**未关闭**（前置条件 = 设计 §4.2.1 的 (a)(b)(c)，用户后续拍板 B 时执行） | `crates/homeway-core/src/token.rs` **零 diff**、`fixtures/vectors/token.json` **零改**、`tools/**` 的 `hmw1` 抽取面 **零改**（`tools/quic-wg-e2e.sh` 的 `_wg` 字节判据照旧成立） |
| 2026-10-09（同上） | **E6/E7/E18 保留原串（M2 零差异登记）** | E6（`peer 表：设备表就绪（cap=%d，ttl=%v，grace=%v；按 devTag 记账/刷新/轮换）`）/ E7（`peer: + dev=%s pub=%s ip=%v n=%d/%d`）/ E18（`凭证台账：… 吊销即时对新注册生效`）**行文与语义零改动**；E8/E9 **行文一字不改**，差异只走「计数输入集」节的 M2 两行（refresh 输入集 = `R4`；`no-token` 输入集 = {MAC 过、窗超}） | 设计 §2.3：M2 的设备表面是**语义等价**实现（`table.rs` 零 diff——只换 MAC 入口名 `match_reg3` → `match_proof`）；显式登记「无变更」防后续读成漏登 | `crates/homeway-core/src/server/table.rs`（**零 diff**）；E6/E7/E8/E9/E18 按与 M1 末**逐字同**验收 |
| 2026-10-09（**M2 S4 落地**；设计 §14-1② 裁定） | **每源闸阈值缺省 `serve.quic_admit.per_src_fails` 10 → 16（窗保持 10s）** | 缺省 `per_src_fails = 10` → **`16`**（值域 `1..=1000` 不变；`per_src_window` 缺省 10s 不变；**计数集不变**——仍计「同源未完成/被拒」） | **设计-实测冲突已裁（设计 §14-1②）**：3 候选同址赛跑在「客户端 abort 未完成候选」形态下单轮消耗 `(N−1)=2` 次失败预算 ⇒ 旧缺省 S3 实测第 4 轮 `flood_refused=5` + 岛侧 `NoCandidate`。**不放宽计数集**（攻击者同样能 abort ⇒ 豁免 = 逃逸面），改为抬高预算：16 允许约 5 次 3 候选赛跑/窗（覆盖断网抖动期恢复节奏），同时仍把攻击者束在 **≤16 次握手/10s/源**（有界性不变）。**待真机标定**（值可配） | `crates/homeway-quic/src/exit/admit.rs`（`PER_SRC_FAILS_DEFAULT`）、`exit/mod.rs`（缺省装配 + 启动行「抗放大面 … 每源 16/10s」取值）、`crates/homeway-core/src/nodestate.rs`（模板注释）、`crates/homeway-cli/src/serve_cli.rs`（缺省对照用例）；**按「每源 10/10s」写断言的读者须改**；证据 = `exit::tests::race_abort_form_stays_within_gate_budget`（3 候选 × 5 轮 ⇒ `flood_refused = 0`）+ 对照臂 `exit::tests::old_gate_budget_trips_on_next_attempt`（旧缺省下第 16 条尝试被拒） |
| 2026-10-09（**M2 S4 落地**；设计 §14-1③ 裁定） | **「正常赛跑」判据措辞改写（设计 §3.3-6 / S3-2 的原判据）** | 「正常赛跑 ⇒ `retry_sent = 0` **且** `flood_refused = 0`」→ **「正常赛跑在阈值内 ⇒ 不触发闸（`retry_sent` 可为 0；`flood_refused` 只有在超过 `per_src_fails` 后才增长）」** | 设计 §3.1-② 的「正常赛跑会完成 ⇒ 不计数」只对「**完成后再被收掉**」的候选成立；「客户端在完成前 abort 输家」（`race::run` 的 `abort_all`）在出口侧落 `HandshakeOutcome::Failed` ⇒ 计入每源闸。计数语义按 §14-1① **保持**（改的是**判据措辞**，不是计数） | `crates/homeway-quic/src/exit/tests.rs`（`normal_races_do_not_trigger_retry_or_gate` 的形态注记改写 + 新增 abort 形态正面用例）；`docs/reviews/M2.md` 的差异登记（**S6 落**——该文件开工时不存在，S4 转交）；设计文档 §14-1 |
| 2026-10-09（**M2 S4 落地**；设计 §14-1④ 承接） | **出口侧握手失败归因位（additive）** | 无 → 有：`ExitQuicSnapshot.handshake_peer_closed`（`handshake_failed` 的**子集**）。口径写死：`ConnectionClosed`/`ApplicationClosed`（收到对端关闭帧——含 TLS alert 类 crypto 错误）计入；`TimedOut`/`Reset`/`VersionMismatch`/`TransportError`/`LocallyClosed` **不计**（`TransportError` 无法反推对端是否发过帧 ⇒ 统一归「非主动关闭」，**如实注记**） | §14-1④：「出口侧能否区分『对端主动关闭』与『对端静默』」——**能**（以 quinn 的 `ConnectionError` 变体为据；此前 `Ok(Err(_e))` 丢弃错误值 ⇒ 不可归因）。**只作归因——两个闸的输入集不变**（§14-1①）。另订正 `client/race.rs` 模块头的旧口径（「输家 drop = 主动关闭」**不成立**：`Connecting` 无 close API，drop 只停止等待；实测 3 候选 abort 后出口侧仍见 2 条完成） | `crates/homeway-quic/src/exit/mod.rs`（`FailureKind`/`failure_kind` + 计数）、`client/race.rs`（注释订正，**零行为改动**；已建立输家的显式 close 不变）；证据 = `exit::tests::race_abort_form_stays_within_gate_budget`（错 pin 中止 ⇒ `handshake_peer_closed ≥ 8`）+ `exit::tests::rpk_pin_mismatch_aborts_handshake`（≥1）；该快照为**进程内**读面（未进 `quic` JSON 段） |
| 2026-10-09（**M3 S1–S5 落地；S7 登记**） | **E14 行文改写**（`files 就绪`） | `files 就绪：root=%s (rw) sock=%s（隧道IP:%d 经拦截层转投）` → **`files 就绪：root=%s (rw) sock=%s（服务流 tag=1；QUIC STREAM 承载；本机 UDS 仍为 WG 服务腿入口）`** | M3 服务流入口改 STREAM tag 分发（应用层帧逐字节不变）；D1 下 UDS 仍是 WG 服务腿入口 ⇒ 新行文须同时说清两者（设计 §8.2-1） | `crates/homeway-core/src/server/engine.rs`（files 就绪行）；出口排障读者；按旧串写断言的脚本/文档须改。**实采（S6，2026-10-09 统一进程本地实例）**：`files 就绪：root=/Users/zhaozhe (rw) sock=/tmp/m3s6-fr2/serve/files.sock（服务流 tag=1；QUIC STREAM 承载；本机 UDS 仍为 WG 服务腿入口）` |
| 2026-10-09（同上） | **E17 行文改写**（`speedtest 就绪`） | `speedtest 就绪：sock=%s（隧道IP:%d 经拦截层转投；内存收发不落盘）` → **`speedtest 就绪：sock=%s（服务流 tag=3；QUIC STREAM 承载；内存收发不落盘）`** | 同 E14（设计 §8.2-2；要点不同故不转引「同上」） | `server/engine.rs`（speedtest 就绪行）；**E13 的模板与采样行（`INTEROP-CRITERIA.md` 正文 `:31` 与实采 `:110`）不含本串，不受影响**；按旧串 grep 的脚本须改 |
| 2026-10-09（同上） | **E10/E11 输入集变化（行文逐字不变）** | QUIC 档服务腿（files/term/speedtest 的 `kind=exempt` 行）**不再产生**；隧道 IP 上的**回环同端口豁免**仍产生；D1 下 **WG 服务腿（CLI 远程面）照旧产生**（设计 §8.2-3） | M3 服务流改 tag 分发（D1）；服务腿不再经 `intercept` 的 UDS 映射 | `server/intercept` 行读者；按「每命令一条 exempt 行」写断言的脚本须按承载分档。**证据**：`tools/quic-island-e2e.sh` 的 `[e2e5] exit.exempt_lines=0`（QUIC 档零 exempt 行）+ `tools/quic-wg-e2e.sh` 的 `intercept: tcp exempt 100.64.255.1:7724 ← …（dialok）` 在场（WG 档不变） |
| 2026-10-09（同上） | **E5 复核（无变更显式登记）** | `intercept: 过境拦截就绪（隧道IP %v；豁免=转投本机同端口；TCP 并发上限 %d）` **逐字不变** | 显式登记「无变更」防后续读成漏登（先例 E6/E7/E18）；UDS 映射在 D1 下保留（§2.3） | 无（读者/脚本零改）。**实采**：统一进程本地实例同日打出同串（`隧道IP 100.64.255.1；豁免=转投本机同端口；TCP 并发上限 1024`） |
| 2026-10-09（同上） | **E1 复核（无变更显式登记）** | `serve 就绪：… files=%d term=%d speedtest=%d …` 的**字段与值域不变**（虚拟端口号仍是配置面；M3 只是「隧道侧不再用它」） | 设计门 4-1 补（原稿漏）；`tools/local-exit.sh`/`local-rust-exit.sh`/`matrix.sh`/`qi-ab.sh` 全部 wait 该行 ⇒ 显式登记防读成漏登 | 工具 wait 面**全不变**（`tools/*.sh` 的 `serve 就绪` 等待点复核通过）。**实采（S6）**：`serve 就绪：wg=:45341（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=false tokens=1 key=…` |
| 2026-10-09（同上） | **E-q3 明细行文 + 源校验接受集收窄** | 接受集 `{tunnel_ip, tun_ip}` → **`{tun_ip}`**（QUIC 档）；明细同步打印**单元素集**：`quic: 丢弃 超限=%d 发送缓冲满=%d 未登记=%d 源校验拒=%d（本次：源校验拒 src=%v ∉ {tun_ip=%v}（dev=…）；…）`（**四字段计数行逐字不变**；`src=` 是 M2 已登项，本批只改集合元素） | `tunnel_ip` 在 QUIC 档无合法来源（设计 §6：源校验接受集收窄 = 安全面收紧）；QUIC 档 `tunnel_ip` 字段保留但不再参与源校验 | `crates/homeway-quic/src/exit/conn.rs`（`src_allowed`/`src_text`）、E-q3 读者、按双元素集写断言的脚本；**计数输入集变化另见**「计数输入集/数值语义变化」表。**证据**：`exit::tests::datagram_with_illegal_source_is_dropped_and_counted`（`tunnel_ip` 源现在被拒 + 明细不含 `tunnel_ip=`） |
| 2026-10-09（同上） | **新增 E-q5 服务流出口行族（additive）** | 无 → 有（**逐字照实装**）：① `quic: 服务流已受理（tag=%s dev=%s 第 %d 次）`；② `quic: 服务流拒（dev=%s tag=%s；%s（0x%02x）；第 %d 次）`——`%s` 实装取值集 = {`未知 tag`、`服务不可用（未启用）`、`服务不可用（socketpair 建不起来：%e）`、`入口队列满 %d/%d`、`连接未绑定`、`tag 读取超时`、`服务不可用（dial 目标 %v；M3 只定协议，M4 换轨）`、`服务不可用（dial 目标帧畸形；…）`、`服务不可用（dial 目标未读出；…）`}（**复位码位 `（0x%02x）` 是实装新增**，设计草案只写 `%s`）；③ `quic: 服务流结束（tag=%s，↑%dB ↓%dB，耗时 %v）`（**耗时字段是实装新增**）；④ `quic: 服务流泵起不来（tag=%s dev=%s：socketpair 进 runtime 失败 %e）——已关流` | M3 服务流面的唯一可观测面（设计 §8.2-7） | 出口排障读者；落点 `crates/homeway-quic/src/exit/{serve,pump}.rs`。**两条实装/设计差异须记住**：(a) **probe（tag=5）也打受理/结束行**（设计 §2.1 写「probe 不记受理行，只计快照计数」；实装 = S1 落地的同族行，且 **与 tag 1–3 共享「首 3 + 每 100」节流窗** ⇒ 每连接多 1 条 probe 流会占掉窗位，S4 起既有 e2e 断言按「受理**或**结束至少一条」口径对齐）；(b) 受理行是**入队即受理**（服务自身的在册闸回应用层 busy 不另计拒） |
| 2026-10-09（同上） | **C8 判据位的语义更正（quic 档；行文与值域不变）** | M1 登记原文（「quic 档的暖机判据位由岛 `Cmd::Probe`（**QUIC STREAM 回显**）满足」）**过度声明**（当时的实现是 `open_uni + reset` + 等 `udp_rx`，`client/mod.rs` 自陈「**不是**端到端回显」）→ **更正为**：M3 **前** = 连接级判据；**M3 起**才为 `STREAM[probe]`（tag=5）真回显 | M3 定型 probe 流（设计门 3-5）：`uni=0` 与「`Cmd::Probe` 换真回显」同切片落（§1.7-N13） | C8 读者（`warmup pong: 就绪（判据=quic）`）；实现 = `client::probe` 走 tag=5；政策允许对既登记条追加「后续」说明（见本表 M1 批的 C8 行） |
| 2026-10-09（同上） | **C11（RECOVER 族）的处置：保留 + QUIC 档不产生 + C18 替代关系** | C11 族（`RECOVER R1/R2/R3` + 恢复/走完行）**行文与语义对 WG 档逐字保留**；新增登记两条：①「**QUIC 档不再产生 C11 族行**」②「**C18 与 C11 是替代关系**（同一期同一承载不会两族并存；QUIC 档恢复时间线的唯一行族 = C18）」 | M3 阶梯重写（QUIC 档无 R1/R2/R3）；WG 档旧阶梯保留到 M5（设计 §3.4/§8.2-9） | C 族读者；按 C11 grep 的排障脚本须按承载分档。**证据**：`session/recover.rs` **零 diff**（`git diff c7765fd -- …/recover.rs` = 空）；`recover()` 全部调用点都在 `!l3_on_island()` 守卫下；`c18_line_family_texts_are_pinned_and_replaces_c11` 显式断言替代关系 |
| 2026-10-09（同上） | **新增 C18 链路恢复行族（additive；QUIC 档）** | 无 → 有（**逐字照实装**）：`quic: 链路快探失败（连续 %d，原因=%s）` / `quic: 链路探活抖动（%s，已复探）` / `quic: 链路重连中（原因=%s，第 %d 次）` / `quic: 链路重连完成（原因=%s，耗时 %v）` / `quic: 链路重连失败（原因=%s，第 %d 次）—— 交世代重建` / `quic: 世代重建（原因=%s；连续重连失败 %d）`；**三条伴随行**（实装新增，同族）：`quic: 链路动作选 %s（原因=%s；%s）`（`%s` ∈ {`M（换本地 socket）`、`R（新 QUIC 连接）`}）/ `quic: 交另一动作（M↔R；窗 %v / 门 %d 次未到）` / `quic: 换本地 socket 失败 —— 转 R（新 QUIC 连接）`；**抖动升格行**：`quic: 连续 %d 次抖动升格 —— 计入失败链（防 fail-silent）`；节流照仓内口径（首 3 + 每 100） | M3 阶梯重写（设计 §8.2-10）：QUIC 档无 R1/R2/R3，恢复时间线由本族承担 | C 族读者；`recover.rs`（WG 档保留）与新恢复面并存；`load` 类脚本。**证据**：`client/ladder.rs` 的 `c18_line_family_texts_are_pinned_and_replaces_c11`（六条逐字 + 「本模块零 RECOVER」）；`tools/quic-ladder-e2e.sh 2` 四用例实读（`T_recv` 两相位 2461/1312ms） |
| 2026-10-09（同上） | **新增 C19 服务流行族（additive；客户端）** | 无 → 有（**逐字照实装**）：`quic: 服务流已开（tag=%s；第 %d 条）` / `quic: 服务流已关（id=%d tag=%s，↑%dB ↓%dB；第 %d 条）` / `quic: 服务流失败（tag=%s；%s；本机在册 %d/%d；第 %d 次）`（额度耗尽时带在册读数；其余形态 `quic: 服务流失败（tag=%s；%s；第 %d 次）`）/ `quic: 服务流背压（id=%d tag=%s；待发队列满 %dB；第 %d 次——调用方走 Ok(0) 退避环）` / `quic: 服务流重试（tag=%s；首试 %s；余预算内再试一次）`（**S3 交下项**） | 服务流拨号缝换轨后的归因面（设计 §8.2-11）；`%s` 实装取值 = `StreamErr` 的 `text()`（§1.6 的 typed 归因） | App 排障读者 / 核日志；落点 `crates/homeway-quic/src/client/streams.rs` + `crates/homeway-core/src/facade/quic_stream.rs`。**实装 vs 草案**：草案写 `已开（tag=%s，耗时 %v）`（实装**无耗时**、带「第 %d 条」）；节流首 3 + 每 100 |
| 2026-10-09（同上） | **准入关闭码表 + 客户端映射（非编号判据行；安全面 + 归因面登记）** | 出口拒绝路径的 `CONNECTION_CLOSE` 码从恒 `0` → **`{0x11 凭证不被接受, 0x12 资源暂不可用（稍后重试）, 0x13 准入数据非法, 0x14 准入超时}`**（**仅准入窗内三处**：`exit/conn.rs` 的 `reject()` / `time_out()` / `ask_engine` 入境队列满）；客户端**限定未绑定态**映射 typed 归因（`IslandErr::AdmissionRejected{code}`）+ 一行 **`quic: 准入回执（code=0x%02x %s）——本世代回落 WG 承载`**（前缀刻意与出口 `quic: 准入被拒（` 区分，设计门 P3）+ 岛快照两字段（`admit_reject_code`/`admit_reject_text`）；**准入后**的同类关闭走 `IslandErr::SessionClosed{reason}` 独立分支、留码 `0`、单记 **`quic: 会话被对端关闭（%s）——会话级归因（非准入面）`**（行文为实装新增，设计只给分支） | M2 真机发现①（三种拒绝原因在设备侧不可见 + WG 回落使黑洞期不可见）；设计 §4/§8.2-13 | `crates/homeway-quic/src/admit_close.rs`（码表单源，**纯 std**）、`exit/conn.rs`、`client/register.rs`、`IslandErr`、岛快照、E-q 归因行读者；**安全面：两桶粗粒度、不发新帧、不改准入状态机、不放宽任何闸**；出口详细归因行（E-q 族）逐字不变。**证据**：`exit::tests::admission_close_codes_are_bucketed`（客户端侧读 `close_reason()` 逐档）+ `post_admission_close_carries_no_admission_code`（F4 负例）+ `client::tests::admission_rejection_carries_code_line_and_snapshot_fields` + `post_admission_session_close_is_not_reported_as_admission` |
| 2026-10-09（同上） | **新增服务流复位码表（0x21–0x27）+ `StreamErr` 取值集（additive，wire 面）** | 无 → 有：`0x21 TAG_UNKNOWN` / `0x22 SERVICE_DISABLED` / `0x23 INTAKE_FULL` / `0x24 UNBOUND` / `0x25 DIAL_REFUSED`（**M4**）/ `0x26 DIAL_TIMEOUT`（**M4**）/ `0x27 TAG_READ_TIMEOUT`；客户端读侧**白名单闭区间 = [0x21,0x27]**，其余（含 `0x00`/未知码/对端 FIN）⇒ `StreamErr::Closed`（= 今天的 EOF 语义，逐字保留）；`StreamErr` = `#[non_exhaustive]` thiserror enum（禁字符串错误）。**并登记 `ConnErr::Refused` 消费点的逐点迁移表（设计 §1.6 七行）与 `ConnErr::Refused` 的 Display 去 RST 化：`连接被拒（对端 RST）` → `连接被拒（对端无该服务）`** | 换轨后「服务不存在」不再以 RST 出现；`Refused` 的启发式（SynSent→Closed 且非本地 abort）不再成立（设计 §1.6/§8.2-14） | `crates/homeway-quic/src/stream.rs`（码表 + `StreamErr` + `from_reset_code` 白名单）、`exit/{serve,conn}.rs`、`client/streams.rs`、`facade/quic_stream.rs`（`stream_err_to_io` 含**新分支** `{Busy,Unbound,BadTag} ⇒ ErrorKind::Other`）、`facade/bridge_host.rs`（`is_refused_like` + 测速桥）、`facade/service_exec.rs`、`facade/tun_exec.rs`（`conn_err_to_io`）、`daemon/mod.rs`（两处）、`daemon/carriers/speedrun.rs`、`wgcore/mod.rs`（Display）。**可达性（照实装）**：出口写出 **5 码**（0x21/0x22/0x23/0x24/0x27——`exit/serve.rs` 的测试逐码钉「本文件写出的五个码都能被客户端识别成 typed 错误」；其中 **0x22 覆盖 tag=4（dial）的本期限定形态**与「服务未启用/入口建不起来」）；**0x25/0x26 在 M3 不可达**（出口不写，客户端白名单已备——同测试明写「M4 的两码本文件不得写出」）；**`0x27` 可达**（出口 `TAG_READ_BUDGET=5s` 未读到 tag ⇒ `reset(0x27)` + 拒行「tag 读取超时」）。**S2 前的暂态**：tag 1–4 在 S1 出口最小形态下一律 `0x22`（S2 接线后 tag 1–3 = 真服务、tag 4 = 本期限定 `0x22`） |
| 2026-10-09（同上） | **`quic` 段（客户端 `tunStatusJSON`）：实装键表 + 段存在判据变化** | ① 段出现判据由「quic 档 **且 L3 落在岛上**」→ **「quic 档 且 岛已构造」**（岛建连失败/准入被拒而回落 WG 的世代**也出现该段**——§4 的「App 可见为什么走了 WG」不可达的修复）；② **实装键表 = 25 键**（字典序，单测 `quic_section_is_additive_and_complete` 钉死）：`admit_reject_code`/`admit_reject_text`（**S5 新增**）/`candidates`/`congestion_events`/`connections`/`current_mtu`/`drops{return_queue_full,send_buffer_full,too_large,unregistered}`/`ep`/**`ladder_action`/`ladder_fail_streak`/`ladder_jitter_streak`/`ladder_probe_ok`（S4 新增）**/`local`/`lost_packets`/`migration_unconfirmed`/`migrations`/`mirrors`/`mtu`/`packets_in`/`packets_out`/`relay_tx`/`rtt_ms`/`rx_ignored`/`send_buffer_used`/`via` | M3 服务流面 + §4 归因 + §3.1 的 M/R 判别信号都要观测落点；行与快照同源（M2 §15-1 口径） | `tunStatusJSON` 读者（additive）；tier App 只校验既有键 ⇒ 零影响；按段缺席写断言的脚本须改（回落 WG 的世代现在也有段）。**实装 vs 设计草案（如实登记，防按草案写断言）**：§8.2-12 草案里的 `streams_open`/`streams_refused{…}`/`streams_active`/`stream_bytes{in,out}`/`stream_backpressure_events`/`sock_send_errs` **未进客户端 JSON 段**——它们的实装落点 = 岛 `IslandSnapshot` 的**进程内**读面（`streams_open`/`streams_active`/`streams_refused`/`stream_bytes_in`/`stream_bytes_out`/`stream_backpressure_events`/`sock_send_errs`/`sock_send_errs_local`/`sock_send_err_local_fresh`/`sock_send_err_last_errno`/`sock_send_err_age_ms`；e2e 与阶梯消费），出口侧对位计数进 `serve status --json` 的 `quic` 段（见下条） |
| 2026-10-09（同上） | **出口 `serve.status --json` 的 `quic` 段（M2 交下项 L4；additive）** | 无 → 有：平级 additive 段 `quic`，**面未起 = 整段缺席**（旧载荷/旧读者零影响）；**实装 28 键**（键名 = `ExitQuicSnapshot` 字段名的 camelCase；**单源 = `ServeQuicBits::from_snapshot`**，加字段即编译红）：`connections`/`admitted`/`pathChanges`/`handshakeFailed`/`handshakePeerClosed`/`handshakesInFlight`/`handshakeRefused`/`connRefused`/`handshakeTimeouts`/`regsAccepted`/`regsRejected`/`challengesIssued`/`challengesRefused`/`proofRejected`/`pendingExpired`/`admitTimeouts`/`retrySent`/`floodRefused`/`proofCooldowns`/`dropTooLarge`/`dropSendBufferFull`/`dropUnregistered`/`dropSrcRejected`/**`streamsOpen`/`streamRefused`/`streamsClosed`/`streamBytesIn`/`streamBytesOut`（S2 新增，服务流面）** | M2 设计 §15-1 交下（出口 QUIC 快照的外部只读面）；M3 S2 判据点名落地 | `crates/homeway-core/src/daemon/proto.rs`（`ServeQuicBits` + 反序列化 additive 兼容）+ `daemon/mod.rs` 装配；`serve status --json` 读者（App/CLI）。**证据**：`daemon::proto::tests::serve_status_quic_segment_is_additive_and_one_to_one`（键一对一 + 旧载荷零影响） |
| 2026-10-09（同上） | **`migration_unconfirmed` 语义收窄（落「计数输入集/数值语义变化」表；行文不变）** | 判定窗口 60s（巡检拍）→ **≤1 个快探预算（缺省 700ms）**；且 **M 的确认面改为「快探回显」**（不再是 `udp_rx` 增量）——该位由「慢变量告警」升为**动作前置条件**（置位 ⇒ 允许走 R）；**N-b 行文逐字不变**（`quic: 迁移完成（%v → %v，耗时 %v）` / `quic: 迁移未确认（…%v 内无对端回包 ⇒ 回落重连/重赛跑）`，窗口值随实参变） | M3 §3.1：`Rebind` 后一个预算内无回显 ⇒ 置位 ⇒ 允许走 R（相位 B 实测链） | 状态 JSON 读者（`quic.migration_unconfirmed`）、N-b 行读者、M1 真机「路径变更已验」的复现脚本。**实装补充**：`quic: 迁移未确认（一个快探预算内无对端回包 ⇒ 回落重连/重赛跑）`（driver.rs 的 M 确认失败形态） |
| 2026-10-09（同上） | **`patrol` 分类的触发源变化（安全/行为面）** | 「M1 = 连接死**即时**分类（`mark_unhealthy_if_current(gen,"patrol")`）」→ 「M3 S4 = 岛内阶梯走完 M/R（**B 门**：连续 2 次 R 失败 **且** 窗 ≥10s）才上报」；**值域不变**（`unhealthyReason` 仍 ∈ `{patrol,fd,panic,stop}`） | M3 §3.1：恢复动作面整体交岛内阶梯（世代层只留 B 的接收端） | `mark_unhealthy_if_current(gen,"patrol")` 的读者（扩展重建）、App 的世代重建时点；新增行 `quic: 快探阶梯走完 M/R（连续重连失败 %d，原因=%s）—— 上报不健康（交世代重建）` + `quic: 岛上报不健康（patrol）—— 岛内快探阶梯已走完 M/R（B 门），交世代重建` |
| 2026-10-09（同上） | **M1 登记的「S3-1『Rebind 优先』处置行族」整族删除（行不再产生）** | M1 登记的六条（`quic: 迁移未确认 ⇒ 先试 Rebind（…）` / `quic: Rebind 完成（→ %v），等对端回包证路径` / `quic: Rebind 后探活通过 —— 连接保持（不拆世代）` / `quic: Rebind 失败（…）—— 回落重连/重赛跑` / `quic: Rebind 后 %s 仍探不活（…）—— 回落重连/重赛跑（交扩展重建）` / `quic: %s（连接不在/迁移未确认位未置）—— Rebind 无可保之物，交扩展重建`）→ **全部零产出**（实装已无任何构造点；`quic_rebind_first`/`quic_rebind_body`/`HealOutcome`/`QUIC_HEAL_PROBE` 四个调用点随 M1 动作面一并删除） | M3 S4 偏离 1：阶梯落岛内（`client/ladder.rs`），单飞改**结构性**（`Ladder::inflight`/`pending`），世代层不再留第二真相面；M/R 的判据信号与动作全在岛内 | M1 登记面的读者（按这六串 grep 的排障脚本）；**替代** = C18 行族（见上）。**证据**：六个特征子串逐个 grep（`先试 Rebind` / `Rebind 完成` / `Rebind 后探活通过` / `Rebind 失败` / `Rebind 后 %s 仍探不活` / `Rebind 无可保之物`）在 `crates/` + `tools/` = **0 命中**（S6 扫查）；`docs/INTEROP-CRITERIA.md` 的 M1 批同族行**保留为历史**（历史条目不改写） |
| 2026-10-09（同上） | **M1 登记的一条岛侧行文改写（`连接已断` 行）** | `quic: 连接已断 —— 等上层重连/重赛跑（阶梯接线 = 世代层）` → **`quic: 连接已断 —— 交快探阶梯（M/R/B；§3.1）保世代重连，不再就地拆世代`**（旧串零命中） | M3 S4：连接死的处置由「交上层」改为「岛内阶梯」（设计 §3.1） | M1 登记面的读者；`crates/homeway-quic/src/driver.rs`；**另有同批新行**：`quic: 连接断 —— %d 条在册服务流按 EOF 收`、`quic: 换连接 —— 旧连接上的 %d 条服务流按 EOF 收（调用方按需重开）`、`quic: 流面参数（bidi=%d uni=%d recv_window=%dB conn_recv_window=%dB send_window=%dB 待发=%dB；有效服务流 %d；env 覆盖 %d 项）`（**代码门 r18 ③-2：`conn_recv_window` 字段为补登**）、`quic: 快探参数（首探 %v，拍间 %v，复探 ×%d，待机 %v，抖动阈值 %d，B 门 连续 %d/窗 %v，发送面新鲜度窗 %v，在用窗 %v；env 覆盖 %d 项）`、`quic: 上行发送面错误（%s；%e；第 %d 次；计数行首 3 + 每 100）`（S1 的 N5 计数点）、`quic: 巡检失败 —— 动作面在岛内快探阶梯（§3.1；本拍不重复动作）`、`quic: 对端连续 3 次巡检不可达（岛内阶梯未见自愈）—— 兜底上报不健康，交扩展重建`、`quic: 待发包下推 —— 岛内快探阶梯为准（§3.1；本拍不重复动作）`、`quic: 本地 socket 已换绑（%v → %v）—— M 动作（原因=%s）` |
| 2026-10-09（同上） | **配置与常量（additive；按实装写清「硬编常量 / env / config.toml 键」三类）** | 无 → 有：**硬编常量（两端共用，`exit/transport.rs`）** `max_concurrent_bidi_streams=64` / `max_concurrent_uni_streams=0` / `stream_receive_window=4 MiB`（**S9 整改：256 KiB → 4 MiB**）/ **`receive_window=8 MiB`（连接级聚合闸；S9 新增 `CONN_RECV_WINDOW`）** / **`send_window=2 MiB`（quinn 真名；连接级）**；**自记账域** = {控制流, probe 持久流, 服务流} ⇒ **有效服务流容量 = 上限 − `RESERVED_STREAMS(2)` = 62**；**客户端常量** `PROBE_FAST_BUDGET=700ms`（+复探 ×2）/ 在用档拍间 250ms / 待机 60s / 抖动阈值 3 / B 门（连续 2 + 窗 10s）/ `stream_pending_bytes=64 KiB`；**出口常量** `TAG_READ_BUDGET=5s` / intake 容量 **`{files: 20, speedtest: 16, term: 20}` = 各服务在册上限 + `INTAKE_K=4`**（【**行为变化**】term 今天**无连接级闸**（只有会话上限 `DEFAULT_MAX_SESSIONS=16`）⇒ 20 是**新引入的连接级上限**，理由 = 防「一条设备开满 bidi 流」把 term 线程数打成无界；**S9 代码门补实装**：accept 处原子占名额、超限收线（此前该「上限」只在 intake 队列容量上，服务侧不计数——见本表 term 闸条）；files 在册 16（`files_server::MAX_CONNS`）、speedtest 12（`speedtest_server::MAX_CONNS`））/ socketpair `SO_SNDBUF`/`SO_RCVBUF` = 64 KiB。**env 消融臂（全部 `HOMEWAY_QUIC_*`，非法 ⇒ 记行 + 按缺省，不 fail-fast）计 14 条**（S9：`STREAM_WINDOW` 上限 4→16 MiB、新增 `RECV_WINDOW`）（`tuning.rs` 的 `ENV_*` 常量为准）：`STREAMS`/`STREAM_WINDOW`/`SEND_WINDOW`/`STREAM_PENDING`（S1 四条 + S9 的 `RECV_WINDOW` 一条）+ `PROBE_BUDGET`/`PROBE_GAP`/`PROBE_REPROBE`/`PROBE_IDLE`/`JITTER_STREAK`/`RECONNECT_STREAK`/`REBUILD_WINDOW`/`SEND_ERR_FRESH`/`IN_USE_FRESH`（S4 九条）——**订正**：S4 交下的「连同 S1 七条共九条」计数与实装不符（实为 13 条；S4 交下文本把「S1 四条」误记为七条），按实装登记；**S9 复订正为 14 条**（见上）。**实装面：以上全部是硬编常量 + env，未进 `config.toml`** | quinn 默认（200 流 × 1.19 MiB ≈ 238 MB/连接）与产品内存预算不符；并发上限与快探节拍必须显式（设计 §1.7/§8.2-16） | `crates/homeway-quic/src/{tuning.rs,exit/transport.rs,stream.rs}`、内存门槛读数口径（`tools/quic-ab.sh`）、启动行 `quic: 流面参数（…）`/`quic: 快探参数（…）`（生效值可观测）；**标定授权** = 设计 §15-3（超设计值域须先登记再改） |
| 2026-10-09（同上） | **零差异登记（防漏登）** | E12 / E13 / E15 / E16（+E16a–d）/ E-q1 / E-q2 / E-q4 / C2'/C4'/C5'/C6'/C15' **显式登记「M3 零变更」** | 先例 E6/E7/E18；防后续读成漏登（设计 §8.2-17） | 无。**复核证据**：C2'（`quic: 隧道侧就绪（…核心自连经 WG 拨隧道 IP）`）/C4'/C5'/C6'/C15'（岛侧赛跑与刷新族）在 `tools/quic-island-e2e.sh` 与 `client/tests.rs` 逐串断言；E-q1/E-q2/E-q4 由 M1/M2 登记面覆盖（M3 未改其行文与频率） |
| 2026-10-09（同上） | **stackb / UDS 分支退役面登记（按 §12-① 裁决形态 = D1 收口）** | D1（**已裁定**）：记为「**QUIC 档消费点清零**（App 核服务流全走 STREAM）+ **余下消费者 = WG 承载**」+「`intercept::local_services` / `DialTarget::Unix` **保留**（唯一消费者 = WG 服务腿，随 M5 与 `wgcore` 同批删除）」；**净改动（本批实测）**：新增 `facade/quic_stream.rs`（418 行）+ `client/ladder.rs`（963 行）+ `exit/{serve,intake,pump}.rs` 等服务入口/泵面，删除 M1 的 Rebind 动作面（`quic_rebind_first`/`quic_rebind_body`/`HealOutcome`/`QUIC_HEAL_PROBE`/`GenRun::quic_heal` 五个符号）+ **A12 死变体 `DialError::TooManyConns` 删除**。**§5.1 消费点清单的身份订正（S3 交下，设计表第 5/6 行）**：`files.rs` 的动词面只被 **CLI 本地形态**消费（WG 承载）；`service_exec.rs` 是「服务会话」域（App 进程内、**无 TUN 无岛**）⇒ 这两项**在 M3 不适用**（不是漏做）——App 核的服务流实际走**隧道域**的桥（已换轨）；**12 处清单的实际归属** = App 核侧重轨区（桥拨号 ✅清零 / portfwd 裸拨留 M4 / 隧道域 healing_dial 2 处 = WG 档保留 / 岛流面）+ CLI/daemon 6 处（WG-only，留 M5）+ `path_probe`（WG 档巡检，留 M5）。**D3 未采纳** | 设计 §5.2 的事实链（CLI/daemon host 会话 = WG-only）+ §15-1 裁定 | 路线文件 M3 三条（判据/退出口/范围）已按 D1 落（设计 §12-1 的 D1 列）；`tools/check-quic-isolation.sh` 第 ⑪ 条 = 本条的机械面；**若将来改取 D3，需追加**：C11/C8/DC14/DC15/CA1/CA4/CA5 + CA7–CA10 停用条 + `tools/quic-wg-e2e.sh` 断言改写 + intercept 的 5 个 UDS 用例删除 |
| 2026-10-09（同上） | **DC14/DC15、CA1/CA4/CA5 复核（D1：不变）** | 五族的语义对照**不变**（WG 承载的 CLI 远程面：term/files/forward/socks/5300 解析腿）——D1 下这些路径**零改动**；**DC18 的数值口径另见计数输入集表**（收窄面） | 设计 §8.1 的 D1 列（复核）+ §15-1 裁定 | DC14/DC15/CA1/CA4/CA5 的实采样例继续有效；**证据**：`tools/quic-wg-e2e.sh` 复跑（WG 档 exempt 行在场）+ 本批对 `daemon/**`/`session/**` 零行为 diff |
| 2026-10-09（同上） | **CA7–CA10 复核（D1：不变）** | 四族**行文与语义不变**；**CA9 的哨兵本体**（`refused（7803 回 RST）→ not_supported`）在 QUIC 档的对位 = `0x22 ⇒ StreamErr::NotSupported ⇒ ConnectionRefused ⇒ not_supported`（S3 已实装 + 单测），CLI（WG）面逐字不变 | 设计 §8.1 的 D1 列 + 设计门 4-2（CA9 即 §1.6 哨兵本体，原稿漏列）；D3 未采纳 ⇒ 不停用 | CA7–CA10 读者；`crates/homeway-core/src/daemon/carriers/speedrun.rs`（refused ⇒ NotSupported 语义等价改写）；单测 `facade::quic_stream::tests::stream_errors_map_to_the_designed_io_kinds`（`NotSupported ⇒ ConnectionRefused`） |
| 2026-10-09（同上） | **A13 复核条（无变更显式登记）** | `facade/term_op.rs` 的「出口 7724 端口上不是终端服务（收到帧 0x%02x）」**零变更** | 它是 Go 真源（App 可见文本）的移植；QUIC 档虽不再拨端口，但该处端口是**服务身份**指代而非拨号目标（改字会脱钩基础词表） | `facade/term_op.rs`；排障读者零影响 |
| 2026-10-09（同上） | **公面类型清单变更（M0 层 1 面；非判据行）** | ① `IslandErr` 新增两变体：`AdmissionRejected{code}` / `SessionClosed{reason}`（载荷全 std）；② `IslandSnapshot` 新增字段：`streams_open`/`streams_active`/`streams_refused`/`stream_bytes_in`/`stream_bytes_out`/`stream_backpressure_events`/`sock_send_errs`/`sock_send_errs_local`/`sock_send_err_local_fresh`/`sock_send_err_last_errno`/`sock_send_err_age_ms`（S1）+ `ladder_probe_ok`/`ladder_fail_streak`/`ladder_jitter_streak`/`ladder_action`（S4）+ `admit_reject_code`/`admit_reject_text`（S5）；③ `RejectWhy::EngineRejected` 由单元变体改为携带 `EngineRejectClass{Credential,Resource}`；④ `ServeQuicBits`（28 键，出口状态面） | 层 1 面契约（跨 crate 的同步面类型）：`homeway-quic` → `homeway-core` 的可观测面扩展 | `crates/homeway-quic/src/{cmd.rs,admit_close.rs,exit/mod.rs}`、`crates/homeway-core/src/{facade/tun_status.rs,daemon/proto.rs,server/table.rs}`；单测 `tests::island_err_variants_are_pinned` |
| 2026-10-09（同上） | **A12 死变体删除（非判据行；代码面）** | `stackb::DialError::TooManyConns`（声明以来全仓零构造点）→ **删除** | M3 的「并发上限从无到有」失败态在 **QUIC 档**由 `max_concurrent_bidi_streams=64` + `StreamErr::Busy` 快速失败承担（§1.6）；WG 侧接线 = 改 WG 档行为且该文件 M5 即删 ⇒ 取删除（设计 §5.3-A12 的「顺手接线或删除」二选一） | `crates/homeway-core/src/wgcore/stackb.rs`（+ 注释说明）；**对 Q-F-B 登记条「本批不接线」的后续说明**（该条不再悬空） |
| 2026-10-09（同上） | **A11 跨承载标注（非判据行；口径注记）** | `docs/PERF-AB.md` 的「ACK 密度/段数/通告窗/在途估算」类读数标注为**跨承载不可比**（读数取自栈 B 的 ACK 时钟，随 M5 退场）；QUIC 档对位 = `exit/transport.rs` 的 `ACK_ELICITING_THRESHOLD=16`/`MAX_ACK_DELAY=5ms`（**代码门 r18 ①-2 订正：这两个常量由 `transport_config_with` 组装 ⇒ 客户端与出口**都**广告本值，不是「出口侧下发」**）；服务流吞吐对比须按 §7 的相对门槛（同刻同承载、同一服务操作 ≥0.95× WG/UDS 档）重采 | 设计 §5.3-A11/§7：ACK 整形换轨、旧读数无同义对位 | `docs/PERF-AB.md`（头部批注）；`docs/QUIC-BASELINE.md`/S8 的读数口径；**服务流吞吐相对门槛的读数归 S8** |
| 2026-10-09（同上） | **既有小缺陷修复（非判据行）：统一进程 `files_root = ""` 判负** | `config.toml` 模板缺省 `files_root = ""` 被装配成 `Some(PathBuf::from(""))` ⇒ `FilesServer::open` 的 `is_dir()` 判负 ⇒ **恒打**「⚠️ files 根目录不可用—— 文件管理会报错，其余功能不受影响」（独立 `serve` 形态不生成模板 ⇒ 不受影响）→ **空串视作未配置**（与 `None` 同义 ⇒ `$HOME`，Go `files.Open("")` 同源） | 配合 Go 语义（`pkg/files/server.go:56-62` 的 `rootDir == "" ⇒ os.UserHomeDir()`）+ 模板键表自陈「空=$HOME」；**只归一「空」**，非空但不存在仍 `NotFound` 判负 | `crates/homeway-core/src/files_server.rs`（唯一 choke point）、统一进程/前台 `serve` 的 files 服务根；新单测 `files_server::tests::empty_root_is_treated_as_unconfigured`；**实采（修后）**：`files 就绪：root=/Users/zhaozhe …`（修前同一实例打「根目录不可用」） |
| 2026-10-09（同上） | **M3 未新增 `config.toml` 键（显式登记）** | M3 **未新增/未改** `config.toml` 任何键；`serve.status` 的 `quic` 段与 `tunStatusJSON` 的 `quic` 段都是**只读输出面**；快探/流面参数全走硬编 + env（见配置常量条） | 设计 §12-2/§15-2/§15-3 的授权面（env 消融臂）已足够 S8 标定；不加 config 键 = 不动 `nodestate.rs` 模板与对 Go 单向兼容面 | `nodestate.rs`/`serve_cli.rs` **零 diff**（本批）；若后续要落 config 键，影响面须点 `nodestate.rs` + `serve_cli.rs` + 单向兼容说明（三条先例：`serve.quic*`/`tunConfig.*`） |
| 2026-10-09（同上） | **已知 flake 补充登记：`wgcore::tests::stop_within_detaches_and_reaper_closes_wake_fd`** | 无 → 有（登记为**已知 flake**，非契约变更；其代码注释自陈「真出现按 flake 记」） | M3 S6 首轮全量并行跑实测 1 例红（并行 66s 轮；单测自带 5s 收割期限 + 裸 fd 号探测）；`--test-threads=1` 隔离复跑 **绿（0.02-0.07s，两轮）**；同批另两例（`daemon::tests::handshake_deadline_beats_slow_drip`、`term::service::tests::attach_size_applies_to_pty`）与 `wtransport` 实 socket 时序族为**已登记**族 | 判据行/wire/夹具**零变更**；flake 口径照 M0 §9.2④（红了先隔离单跑再判回归）；CI 复跑若再现按 flake 口径处置（不静默重跑） |
| 2026-10-09（**M3 吞吐整改落地 + S9 代码门补登**；设计 §15-3「超设计值域须先登记再改」） | **流窗口整改：每流接收窗 `stream_receive_window` 256 KiB → 4 MiB + 新增连接级 `receive_window` = 8 MiB** | `64 bidi × 256 KiB`（隐含 16 MiB/连接）→ **每流 4 MiB + 连接级聚合闸 8 MiB**（新常量 `CONN_RECV_WINDOW`；`send_window` 2 MiB 不变） | S8 负面读数（服务流吞吐 0.464× WG/UDS）定位 = **每流接收窗与窗更新往返之比**（出口泵 94% 墙钟停在流控等窗，单次停等 ≈9.8 ms；`W/R_eff` 定量闭合）；同仪器改后 **1.347–1.366×**（门槛 ≥0.95×）。**内存面**：单连接接收面最坏从「16 MiB 隐含」改为「**8 MiB 显式**」（quinn 缺省 `receive_window = VarInt::MAX` 无界 ⇒ 必须显式给聚合闸）；出口 × `max_peers(32)` 最坏 ≈**256 MiB**（与稳态门槛 ≤+320 K 是两口径，禁止混读） | `crates/homeway-quic/src/tuning.rs:37/45`（值 + 依据注释）、`crates/homeway-quic/src/exit/transport.rs:94/98`（两端共用组装）；**读者** = 内存门槛读数口径 / 真机吞吐复测（M5）；M3 记录与设计正文里的旧值 `256 KiB` 以本行取代。**实采**：`quic: 流面参数（… recv_window=4194304B conn_recv_window=8388608B …）` |
| 2026-10-09（同上） | **env 消融臂：`HOMEWAY_QUIC_RECV_WINDOW`（新，256 KiB…64 MiB）+ `HOMEWAY_QUIC_STREAM_WINDOW` 值域上限 4 MiB → 16 MiB；臂数 13 → 14 条** | 臂集 13 条（S1 四条 + S4 九条）→ **14 条**（+ `HOMEWAY_QUIC_RECV_WINDOW`） | 缺省抬到 4 MiB 后旧上限 = 新缺省 ⇒ 消融臂失去上抬空间（负向对照臂仍可显式指定 `262144` 复现 S8 负面读数）；连接级窗必须可单独消融（M5 内存账/真机复测） | `crates/homeway-quic/src/tuning.rs:118/127/162`；配置与常量条的臂数以本行为准。**代码门 r18 ③-4 附带**：跨项关系「连接级接收窗 ≥ 每流接收窗」由 `apply_env` 末尾校验（违例 ⇒ **抬连接窗** + 一行说明；否则每流窗静默失效——连接窗才是真界） |
| 2026-10-09（同上） | **`quic: 流面参数（…）` 行文改写（additive 字段；岛侧 + 出口侧两条）** | 岛侧 `quic: 流面参数（bidi=%d uni=%d recv_window=%dB send_window=%dB 待发=%dB；有效服务流 %d；env 覆盖 %d 项）` / 出口侧 `quic: 流面参数（bidi=%d uni=%d recv_window=%dB send_window=%dB）` → **两条各插入 `conn_recv_window=%dB`**（紧随 `recv_window`） | 新增的连接级聚合闸是**内存上界**（§7 的单连接最坏面）⇒ 必须可从日志确认生效值（否则 M5 重算内存账只能读源码）；代码门 r18 ③-2 的整改 | 出口/岛启动行读者（排障脚本）；**模板变更 = 本行**；`crates/homeway-quic/src/driver.rs`（岛侧）+ `crates/homeway-quic/src/exit/mod.rs`（出口侧）。**实采**：`conn_recv_window=8388608B` |
| 2026-10-09（同上） | **term 连接级在册闸落地（行为变化；§1.7 设计门 2-4 的实装补齐）** | term 的 accept 循环**从不计数**（每条连接 spawn 一个 `term-conn` 线程；只有**会话**上限 16）→ **连接级在册闸**：容量 = `max_sessions + INTAKE_K(4)`（缺省 **20**），accept 处**原子占名额**、线程退出（含 panic）即归还；**超限 ⇒ 收线**（对端见 EOF）+ 新行 + 计数 | S2 只把 20 用作**出口 intake 队列容量**（取出即释放），登记里「20 = 新引入的连接级上限，防一条设备开满 bidi 流把 term 线程数打成无界」当时**无实装支撑**（代码门 r18 C2-1）⇒ 本批补齐、登记成立。行为变化：第 21 条并发 term 连接被拒（今天无上限） | `crates/homeway-core/src/term/service.rs`（`serve_one` / `conn_capacity`）；新行 `term: ⚠️ 连接超限（在册 %d/%d）—— 收线（连接级上限；M3 §1.7 设计门 2-4；第 %d 次）`（additive；节流首 3 + 每 100）；用例 `conn_level_cap_closes_over_capacity_and_releases_slots` |
| 2026-10-09（同上） | **阶梯整改（QUIC 档；**行文模板逐字不变**，只有 `%s` 取值集 / 数值语义变化）** | ①**确认探活不可执行 = 该动作失败**（`on_confirm_unavailable`：M 在途 ⇒ 迁移未确认 ⇒ R；R 在途 ⇒ 计入 R 失败链）——修「连接确定性死亡 + 首次 R 失败 + `rebind` 成功 ⇒ `inflight` 永真、动作链死绝」的活性洞（代码门 r18 ②-1，高）；②同步动作链 >8 步 ⇒ **按 B 收**（`force_rebuild`，走既有 `世代重建` 行族；防「候选为空 × M/R 轮转」自旋）；③**待机档首探**：自阶梯起算满 60s 必探（旧实现 `None => in_use` ⇒ 待机档 60s 巡检实质不存在；②-3）；④`链路重连完成（…耗时 %v）` 的耗时 = **R 发起 → 确认成功**（旧实现重设 `pending.at` = 完成时刻 ⇒ 恒 `0s`/`1ms`；②-6）；⑤读面 `ReadError::ConnectionLost` 归 **`Closed`/EOF**（与 `clear_on_connection_loss` 同面；①-4） | 取值集/数值面变化：`链路动作选 %s（原因=%s；%s）` 的 `%s` 新增「（无连接可确认）」「（无候选可拨）」「（无连接可保）」后缀；`世代重建（原因=%s；连续重连失败 %d）` 的 `%s` 可为「动作链过长（同步 %d 步…）」；`链路重连完成（…耗时 %v）` 的 `%v` 语义 = R 全周期 | `crates/homeway-quic/src/client/ladder.rs` + `crates/homeway-quic/src/driver.rs`（`ladder_step`）；用例 `confirm_unavailable_is_accounted_as_action_failure` / `chain_guard_forces_rebuild_and_stands_down` / `standby_first_probe_fires_after_one_idle_interval` / `reconnect_elapsed_covers_dispatch_to_confirm` / `classify_read_maps_connection_loss_to_eof`；**判据面读者** = `T_recv ≤3.5s` 的真机档（本修直接覆盖「出口停机相位里首次 R 失败」这条最坏链） |
| 2026-10-09（同上） | **tier `docs/agents/connection-lifecycle.md` §3 待修订（草案交付；非本表条目）** | tier 的「核内恢复阶梯」现文只描述 R1/R2/R3（WG 档） → **需按承载分档**（WG 档 = 旧三档原样；QUIC 档 = 快探 → 复探 → M/R → B 世代重建，门槛 `T_recv ≤ 3.5s`）；真源 = `docs/reviews/M3.md` 附录 A（§3 整节替换稿 + §9 常量追加 + §10 速查追加 + §11 待办两条） | M3 阶梯重写（QUIC 档无 R1/R2/R3；设计 §3.4/§8.1 的 tier 行） | tier 侧触点 = **用户**（M3 只出草案、不改 tier）；**未落地前该文档与实现短期不一致**（风险 §10-9；读到 `RECOVER` 行须先确认世代承载） |
| 2026-10-09（同上） | **代码门 r18 的差异登记（未做 / 降级 / 归属；防读成漏登）** | ①**A10 的「收工后出口侧会话与关闭行的时点」e2e 断言未落**（出口侧没有「每连接收线」判据行 ⇒ 时点无可锚行）⇒ 记为**未做**，归属 = M5 出口观测面（补行 + e2e 时点断言；本批不动产品行文面）；②**A13 复核网漏 `daemon/carriers/mod.rs:68` 的 `DialErr::Refused`（「连接被拒（对端 RST）」）**⇒ 保留原样并**在此显式登记**（D1 下该路径走 WG 承载，RST 仍是真实机制；`wgcore/mod.rs` 的去 RST 只覆盖 QUIC 档可达面）；③**A11 的 `ack_eliciting_threshold=16`/`max_ack_delay=5ms` 是两端共用**（`transport_config_with`）——原注释/登记里的「出口侧下发」措辞按此订正；④**出口进程不施加流面/快探 env**（`apply_stream_env` 只在岛侧调用）⇒ 上传方向（出口广告的接收窗）的 env 消融臂当前不可用，归属 M5 真机复测（届时在 `ExitQuicConfig` 装配点同批接线）；⑤**`send_window` 不动的论证只在回环成立**（`send_window` = 发送端本地「未确认保留字节」上界 ⇒ 吞吐 ≲ `send_window/RTT`；真机 30–100 ms 档需双向复测），如实登记为**回环口径**；⑥**残余共享段上限实测下界 = 73.04 MiB/s**（文档原写「74–76」，按实读订正；候选清单补「同连接多流共享 cwnd/pacer」与「同机两端 CPU 竞争」两条，仍未定论）；⑦**「异常序列对照矩阵」（设计 §1.2）只落了正常路径的 UDS↔STREAM 逐字节对照**（异常腿 = STREAM 侧单侧断言 + 三处既有单测）⇒ 登记为**部分落**；⑧**NAPI 下推入口 `ClientCoreTunRecover` 未分档**（S6 已登记）本棒裁定 = **接受现状 + 归属 M4 设计门**（改 rc 语义会驱动 tier 自愈，属设计面决策） | 本表读者以第三列的**事实**为准，不得按「设计要求 = 已落」读 | ①②③⑤⑥⑦ 落 `docs/reviews/M3.md` 的收口节 + 本行；④ 落 `ExitQuicConfig` 装配点（M5）；⑧ 落 M4 设计门 |
| 2026-10-09（**M4 S1–S5 落地；S6 登记**） | **E-q5 服务流出口行族——`tag=dial` 真产出 + why 取值集扩展**（判据行扩展；设计 §8.2 行 1） | `tag=dial` 的拒行 why 恒 `服务不可用（dial 目标 %v；M3 只定协议，M4 换轨）`（一律 `0x22`）→ **真拨号腿（`exit/dial.rs`）产出**：① `目标地址类不可拨（%v：%s）`（`0x25`；`%s` ∈ {`未指定 0.0.0.0` / `本网络 0/8` / `受限广播 255.255.255.255` / `组播 224/4` / `端口 0 不是可拨端口`}）② `目标拨号失败（%v：%e）`（`0x25`；`%e` = errno 原文）③ `目标拨号超时（%v；预算 10s）`（`0x26`）④ `目标帧未读出（对端提前收线，已得 %dB）` / `目标帧未读出（%e）` / `目标帧未读出（预算 5s 到点）`（`0x25`）⑤ 防御臂 `目标帧畸形（长度非 6B）`（`0x25`，不可达）；**受理行 `quic: 服务流已受理（tag=dial dev=%s 第 %d 次）` 的时点 = 1B 回执写成功之后**（其余 tag = 入队即受理）；结束行沿用 `quic: 服务流结束（tag=dial，↑%dB ↓%dB，耗时 %v）`；**节流 = 首 3 + 每 100**（`exit/mod.rs::log_due`；与 tag 1–3/probe **共享同一全局计数窗** ⇒ 断言出口行须落在新起出口的头三次额度内） | M4 S1 把 `exit/serve.rs` 的 `dial_refuse`（恒 `0x22` 拒）换成真拨号腿（A8 判定表 + 1B 回执 + 泛型泵复用） | 出口排障读者；`exit/{dial,serve}.rs` 单测（`a8_address_table_row_by_row` / `dial_failures_map_to_the_two_codes_and_why_texts` / `service_stream_dial_*`）；**既有「dial 一律 `0x22`」类断言须改**（`exit/serve.rs` 的七码同表自检已同批改；`:278-284` 的「M4 两码本文件不得写出」注记已删）；按拒行文本/节流窗写断言的脚本 |
| 2026-10-09（同上） | **C19 客户端服务流行族 + 复位码 `0x25/0x26`（`StreamErr::{Refused,Timeout}`）可达性**（设计 §8.2 行 2） | 两码在码表（`stream.rs` 的 `DIAL_REFUSED=0x25` / `DIAL_TIMEOUT=0x26`）与客户端白名单里但**恒不产出**（M3「备而未用」）；`StreamErr::{Refused,Timeout}` 的 `text()` = `目标拒绝`/`服务流超时`（不可达）→ **同一取值真可达**（由 dial 腿产出）；**`%s` 取值集与行文均不变（仅可达性变化）**；消费链不动：`Refused ⇒ ErrorKind::ConnectionRefused`、`Timeout ⇒ ErrorKind::TimedOut`（`facade/quic_stream.rs::stream_err_to_io`）⇒ C19 的 `quic: 服务流失败（tag=%s；%s；第 %d 次）` 在 pf 链上的实测字面 = `QUIC 服务流：目标拒绝（目标拒绝）` | M4 让 M3 备好的两码可达（码表与白名单**零改动**） | `stream.rs` 码表单测（`serve_only_emits_the_designed_codes` 已按七码改：白名单区间内每码有写侧归属）；`stream_err_to_io` 用例（两码由边角变**主路径**）；C19 行读者 |
| 2026-10-09（同上） | **dial 流的出口→客户端 1B 回执（新增 wire 元素；非编号判据行）**（设计 §8.2 行 3） | M3-design §1.2 表 dial 行「后 = 裸字节管」（双向）→ 客户端→出口方向**不变**（6B 目标帧后即裸字节）；**出口→客户端方向首字节 = `0x01`（`DIAL_OK`，`crates/homeway-quic/src/stream.rs` 单源常量）**，之后才是目标侧字节；失败路径**不写回执**（改 `reset(0x25/0x26)`）；客户端 seam 必须吃「**首块 > 1B**」（回执之后可能立刻跟目标数据 ⇒ 余量**必须**预置进读半缓冲，否则**丢一字节 = 应用层帧错位**） | 拨号发生在**出口**，客户端 `open_bi + 写 6B` 之后无从知道拨号成败；无回执则 Q-F-B 钉住的「失败 ⇒ 对端 `read` = `ConnectionReset` + `fails` + 行」退化成静默 EOF | **M3-design §1.2 该句的订正指针**（**不是**帧格式变更——客户端发的字节一个没变，是**新增方向面**）；`stream.rs`（`DIAL_OK`）、`exit/dial.rs`、`facade/quic_stream.rs`（`dial_target`/`read_dial_ack`/`with_pending`）；两侧单测（回执先于字节 / 余量保真） |
| 2026-10-09（同上） | **新增观测行族（additive）**（设计 §8.2 行 4） | 无 → 有：①（出口，dial 腿）`quic: 服务流目标侧中断（tag=dial，%s 方向：%e）`——**不节流**（每流至多 2 行 = 方向各一；节流会抹掉「目标反复自杀」的证据）②（客户端，NAPI 分档）`quic: 恢复下推（%s）——按承载分档（岛快探%s：%s）`（`%s` = cause ｜ `""`/`+复探` ｜ `通过`/`失败（首探…；复探…）`）③（出口）拒行四条 why = **行 1**（同族扩展，不重复登记） | ① 目标 RST 以 `finish()` 收口 ⇒ 客户端侧归 EOF（行 12），可观测面须回到出口（r19 L5）②S4 分档后岛档**不产 C11 族行** ⇒ 自己的归因行必在 | 出口/核日志读者；排障脚本（`docs/DEVICE-TEST-OHOS.md` §5 速查串可增补）；`exit/dial.rs`（`note_dir`）、`facade/tun_exec.rs`（`recover_downpush_on_island`） |
| 2026-10-09（同上） | **`pfFails` 输入集 + pf 拨号策略（行文与节流窗逐字不变）**（设计 §8.2 行 5 + §11-S2 判据） | 输入集 = 「WG 裸拨失败（`ConnErr` 归因）」→ 输入集 = 「**承载拨号失败**」：`0x25`（目标拒/地址类/帧问题）、`0x26`（超期）、**本端额度耗尽**（`StreamErr::Busy`，**无复位码**；`0x23=INTAKE_FULL` 只由 intake 路径写出，**dial 腿无 intake ⇒ 对 dial 不可达**）、**回执面异常**（回执读得 `Closed`/`ConnectionLost`，或首字节非 `0x01` ⇒ `InvalidData`）、防御面（`NotSupported`/`Unbound`/`BadTag`）；**策略面：`dial_target` 单次尝试、不重试**（**不复用** `dial_with` 的服务流重试策略——Q-F-B D11「pf 的常态拒绝不许触发恢复阶梯」）；行文 `port-forward: {listen} -> {target} 拨号失败 #{n}: {e}` 与节流窗 `n<=5 ∨ n%20==0` **逐字不变**，`{e}` 字面变 `QUIC 服务流：<StreamErr::text()>（<同文>）` | M4 换轨（判据原文：`STREAM[dial]` 与 Q-F-B 的阀/计数/热替换语义逐条对照） | `facade/portfwd.rs` 的计数单测（`dial_failure_rst_and_counters`——`{e}` 逐字类断言须改「前缀 + 非空」口径）；`stats.pfFails`/`stats:` 行两位/本机应用 RST（链 A 零改动）；「不重试」口径读者 |
| 2026-10-09（同上） | **`targetIp = 100.64.255.1` 字面拨差异 + `SERVER_TUNNEL_IP` 别名删除**（行为差异；非判据行）（设计 §8.2 行 6 + §15-2 裁定） | WG 档 = 经出口豁免臂 ⇒ **出口回环**该端口（`server/intercept/mod.rs::route_upstream` 的 `dst == tunnel_ip` 支路；`PfDialTarget::resolve()` 的 `ExitPort` 产物原为 `SERVER_TUNNEL_IP:p`）→ QUIC 档 = **字面拨 `100.64.255.1:port`**（出口侧通常不可达 ⇒ `0x25`）；`resolve()` 改 **`127.0.0.1:p`**；**`facade/portfwd.rs` 的 `use crate::wgcore::SERVER_TUNNEL_IP` 导入与哨兵用法删除**（§15-2 裁定「不保留别名」= 无兼容包袱口径；WG 档的「出口本机」改在 `tun_exec::wg_dial_addr` 内，见**行 16**） | 该常量是 WG 隧道地址（QUIC 档出口不持有该地址语义）；继续当哨兵 = 「为旧承载留隐含依赖」 | spec 无该条（用户面不可达形态）；排障口径统一；`portfwd.rs` 的 doc 注释 + `dial_target_semantics` 单测的 `resolve()` 断言（**唯一允许改的既有用例**：`ExitPort(8080) ⇒ 127.0.0.1:8080`）；隔离门 ⑪(c) 的「`quic_stream.rs` 零 WG 引用」面 |
| 2026-10-09（同上） | **出口虚拟端口 7802/7724/7803 作为 portfwd 目标**（行为差异；非判据行）（设计 §8.2 行 7） | WG 档 = 豁免臂命中 `local_services` ⇒ **UDS 服务**（Q-F-B 实测可达）→ QUIC 档 = `ECONNREFUSED` ⇒ `0x25`（`127.0.0.1:780x` 属 A8 **允许类** ⇒ 透传 OS 后无监听）——服务已改为**流 tag**，不再是 TCP 端口（M3 A13「虚拟端口退役」） | M3 的服务承载切换 | spec 无该条；排障口径（读到 `目标拨号失败（127.0.0.1:7802：Connection refused…）` 是**预期形态**）；实测锚点 = M4-design §12.5 |
| 2026-10-09（同上） | **`targetPort == 0` 旁路形态的 wire 目标**（行为差异；非判据行）（设计 §8.2 行 8 + r20-c 统一定稿） | `SERVER_TUNNEL_IP:0` ⇒ 出口豁免臂 ⇒ 回环拨 0（失败于 `connect` 的 errno）→ **`127.0.0.1:0`，且该目标在拨号前被地址类判定拒**（A8 行 11）⇒ `0x25` + why `目标地址类不可拨（127.0.0.1:0：端口 0 不是可拨端口）`；**可达向量 = `tunConfig.portForwards`**（serde 直读、不过 `validate_table`；NAPI 热替换路径被 `ZeroPort` 先拒 ⇒ **不可达**） | §1.3 哨兵更换 + §1.4 行 11（r20 统一三处机制：**以 A8 判定表为准**） | `portfwd.rs::dial_target_semantics`（`ExitPort(0) ⇒ 127.0.0.1:0` 断言）；`exit/dial.rs` 判定顺序单测（`127.0.0.1:0 ⇒ PortZero`、`0.0.0.0:0 ⇒ Unspecified`） |
| 2026-10-09（同上） | **C11（RECOVER 族）触发集订正——世代限定**（设计 §8.2 行 9 + §15-1 裁定） | M3 登记「QUIC 档零 C11 入口（内部触发点）」+ 同日订正「**NAPI 下推入口 `ClientCoreTunRecover` 未分档**」（⇒ QUIC 档该入口仍产 RECOVER 族行并做 WG 动作）→ **NAPI 下推入口已分档**（M4 S4；判据 = `l3_on_island()`，**不是** `bearer`）：**`l3_on_island()==true` 的世代**不产 C11 族行（改产**行 4 ②** 的 additive 行）+ 不做任何 WG 动作；**`bearer=Quic` 但岛未就的回落世代仍走 WG 原路**（C11 族行照旧）；WG 档逐字不变 | §5.3 的最小分档（现状 rc 与 QUIC 数据面**无关** = 错误证据源，不是「可用但不够好」） | `facade/{mod,tun_exec}.rs`；读到 `RECOVER` 行时**须先确认世代承载**；tier 文档 `docs/agents/connection-lifecycle.md` 修订稿（M3 附录 A §11 第二条由「未分档」→「已分档」，**tier 触点**） |
| 2026-10-09（同上） | **出口 `streams_open` / `stream_refused` / `stream_bytes_in`/`out` / `streams_closed` 的 tag=dial 输入集**（行文不变）（设计 §8.2 行 10） | dial 只走拒行计数（`stream_refused`）→ `streams_open` = 拨号成功数（**时点 = 1B 回执写成功之后、泵启动之前**）；`stream_refused` = 五支拒（帧畸形 / 帧未读出三形 / 地址类 / 拨号失败 / 拨号超时）；`stream_bytes_in/out` = **泵搬运字节（不含 1B 回执）**（`↑%dB ↓%dB` 与目标真实字节对齐）；`streams_closed` = 泵收（两方向 `join!` 之后） | §3.2/§3.3（计数口径写死；1B 回执是唯一不计入字节账的 wire 字节） | `serve status --json` 的 quic 段数值语义；出口排障；`exit/tests.rs` 断言（`stream_bytes_in/out = 12/12` 不含回执） |
| 2026-10-09（同上） | **并发与内存口径（Q-F-B 残余 9/10）**（口径变化；登记）（设计 §8.2 行 11） | 「阀 256 = 唯一界；引擎每连接 2×1 MiB ⇒ 最坏 512 MiB」→ QUIC 档 ①**实际生效并发上界 = 岛 bidi 额度 − 2（缺省 62）/连接**（阀 `MAX_PF_FLOWS=256` **保留**——行文与计数仍在，只是通常不可达）②dial 腿**不进** `intercept::MAX_CONNS(1024)` 账；fd/腿上界 = `conn_cap = 2 × max_devices`（缺省 **64**）× 62 ≈ **3968**（**强制态**口径；持续态 ≈ 每连接 62 × 在用设备数 ≤32 ≈ 1984）③接收面 = **每流窗 4 MiB + 连接级聚合闸 8 MiB**（`homeway-quic/src/tuning.rs` 的 S9 定值）+ 待发 64 KiB/流 ⇒ 「2 MiB/连接」「64 × 256 KiB」两条旧账**均作废** | 承载换代（M3 §1.7 + S9 整改 + 设计 §7A） | Q-F-B 残余 9/10 的注记；M5 容量复核；W1（62 上界）**本期只登记**（真机并发打点未做，见 `docs/reviews/M4.md` 收口记录） |
| 2026-10-09（同上） | **目标侧异常结束的可观测面**（非判据行）（设计 §8.2 行 12） | WG 档在某些 `ConnErr` 变体下，客户端 pf 泵记 `port-forward[down] 读错误（累计 …）` → QUIC 档目标 RST / 读错误以 `finish()` 收口 ⇒ 客户端统一读成 **EOF**（白名单外复位码/FIN 都归 `Closed`）⇒ **该行不再出现**；**出口侧新增**行 4 ① 的 `quic: 服务流目标侧中断（tag=dial，%s 方向：%e）`（additive，不节流） | M3 §1.3「正常收工不用 reset」沿用（r19 L5） | 客户端日志读法；`bridge_host::pump` 的 pf 无条件记行门槛（**行为不变**，只是触发面变小） |
| 2026-10-09（同上） | **`ClientCoreTunRecover` 的 rc 可达集与归因面**（非判据行但驱动 tier 行为面）（设计 §8.2 行 13 + S4 实测） | QUIC 档可达 `0/-1/-2/-3/-4`（跑 WG 阶梯 ⇒ 与 QUIC 数据面**无关**的值）→ **`l3_on_island()==true` 的世代**可达 **`0/-1/-2`**（岛快探 700ms + 一次复探 1.4s ⇒ 通过 `0` / 两次都败 `-1`；**`-3/-4` 在该世代不可达**）；**`bearer=Quic` 但岛未就的回落世代仍走 WG 原路**（`-1/-3/-4` 照旧可达）；`-2` 的构成 = 「无世代/陈旧」∪「岛不在/未 attach」；**rc 值域不变、可达集与归因面变**（**不**声称「零 rc 语义变更」）；tier 决策不变（只有 `rc===0` 跳过整套重建），但 tier 侧 `-2` 的日志文案（「当前无 attached 隧道」）在该形态下**不实** | §5.3 的最小分档（§15-1 裁定「本期修」） | `facade/{mod,tun_exec}.rs`（doc 已写 rc 可达集）；tier `TierVpnExtensionAbility.ets` 的日志与 `attribute()` 归因；tier 文档 `connection-lifecycle.md` 修订稿 |
| 2026-10-09（同上） | **岛档下推耗时上界：设计口径订正（实施期订正，点名）**（S4 交 S6） | 设计 §5.3 记「探段 2.1s + `QUIC_RPC_BUDGET(5s)` ⇒ 最坏 ≈ **7.1s**」（**只计了一次 RPC 余量**）→ 按代码每**次** `l3_probe` 的等待 = `budget + QUIC_RPC_BUDGET` ⇒ 真实最坏 = (0.7s+5s) + (1.4s+5s) = **12.1s**（只有「岛 RPC 两次都卡满」的错配形态可达；首探即通 = 5.7s；正常形态实测最坏 **2.109s**） | S4 实施期发现设计与代码矛盾 ⇒ 按设计 §11 硬约定③（**不得静默降级**）追加订正并上报主会话 | 设计 §5.3 的该数字（订正记录 = `docs/reviews/M4.md` §2.3 + 本行）；`recover_downpush_on_island` 函数头注释；预登记 falsify 指标（`-1` 误判率 / 零 RECOVER 行 / 耗时 ≤8s）**只对实测值设门** |
| 2026-10-09（同上） | **出口 dial 腿 `TCP_NODELAY`**（socket 选项面；新行为对齐）（S5 交 S6） | dial 腿拨号成功后**不设任何 socket 选项**（S1–S3 形态）→ 拨号成功后 `let _ = tcp.set_nodelay(true);`（**最佳努力、失败不致命**） | **同源先例** = WG 档出口 transit 腿（Go `SetDelayOption(false)` 同口径，`server/intercept/mod.rs:2233-2246`）⇒ 同一映射在两条承载下的小包时延语义不得有差异（Nagle 会把「请求—响应」型小包多压一个 ACK 往返） | 仅本机/真机的**小包时延**（握手/回执/泵路径与字节判据**零影响**；改动前后 `.so` 体积同值）；`exit/dial.rs` 注释；探针/字节判据测试零影响 |
| 2026-10-09（同上） | **`tun_exec::wg_dial_addr` = 双栈期临时物（M5 删；登记指针）**（S5 交 S6；设计 §10-W10） | 无 → 有：WG 腿的「出口本机」归一函数（`127/8 ⇒ SERVER_TUNNEL_IP:p`，其余**原样**；归一不加宽） | §1.3 裁决 D-1 的 WG 腿落点——A1①「客户端不得把 127/8 当隧道内目标」的约束**只在 WG 腿内**继承（wire 语义不外泄） | `facade/tun_exec.rs`（`session_connect_target`/`wg_dial_addr` + 单测 `wg_dial_addr_replaces_loopback_only`）；**M5 删 `session_connect_target` 时一并删**（那时「出口本机」只由 QUIC 档的 `127.0.0.1:p` 表达）——见 `docs/reviews/M4.md` §10 的 M5 输入清单 |
| 2026-10-09（同上） | **零改动复核（防漏登；本批逐条核实）**（设计 §8.3） | 无变更（显式登记）：`fixtures/` 零改动；`tools/check-vocab.sh` **PASS** 且四处声明零改动（`portfwd/err` 值域不变）；tier `port-forwarding` spec 零改动；`PfDialFn` 签名（`portfwd.rs:344-345`）与 `pf_target_text` 四形态零改动；`portForwards[]` 的 JSON 键与 `code` 值域零改动；NAPI 符号面零新增/改名 | 判据政策（未登记的改动 = 静默破坏对齐；本批逐条核实**无**改动） | 验收方按「本批无夹具/词表/NAPI 变更」核；WG 档判据行**一条不改**（证据 = `tools/quic-wg-e2e.sh` 复跑：`quic_lines=0`、与 M1 前逐字节同） |


> **上表 E12/decr_flow 两行 = 2026-10-07 Q-B 批落地登记**（Q-A 批预登记的占位条目已按本政策补全
> 「从 → 到」实际行文并去掉「占位」标注，同批 commit）；**其下两行 = 2026-10-07 Q-C 批落地登记**；
> **再下两行 = 2026-10-08 Q-D 批落地登记**（E16a/E16b 尺寸字段 + surface symLen 域）。
> **再下三行 = 2026-10-08 Q-F 批落地登记**（服务会话巡检失败行真因 / 收工等待行 per-thread / portfwd 状态与 rc 契约面）。
> **再下两行 = 2026-10-08 Q-G 批落地登记**（`sun_path` 上限放宽与文案 / relay broken 路径行消失）。
> **其下十四行 = 2026-10-08 Q-H 批落地登记**（非法 config 路径 / 值域补齐 / CA13 覆盖面与形态 / C14 实装 /
> 取值 flag 与布尔纪律 / N1·L7 默认 state / daemon 上限与 SOCKS dead / CA11 与 CA12 同族 / 承载面大小写等价 /
> session 锁 IO fail-fast / short_host 字符语义）；计数输入集表 = Q-H 两行（state 分布移动 + additive 日志）。
> **其下两行 = 2026-10-08 Q-I 尾段批落地登记**（F0 空值 carve-out 局部回退 / DNS 面缓冲复用行为注记 +
> F2 尝试后回退的零残留留痕行）；Q-I 尾段批**零编号判据行变更、零 wire 变更**（F2 的 `兜底=N` 行文变更与
> 两条唤醒时点行为变更**随 F2 回退而未落地**，不属生效登记）。
> **其下五行 = 2026-10-08 Q-J 批落地登记**（E22 `fakeip=` 新增字段 / term 键编码平台口径字段化〔取代 D-10〕/
> CA11 launchd 触发集与文案 / UPnP 四条与 Go 有意分歧 + E20 族数值语义 / F2 五配置键 + 两处 spec opt-in 偏离）；
> 计数输入集表 = Q-J 三行（E21 平台分档语义订正 / E4 取值来源扩展 / C14 + `UDP 默认路径` 取值来源变化）。
> Q-J 的**默认缺省面逐字节/逐值不变**（F1 宿主推断、F2 默认常量、F5 平台分档的 linux 放宽属纠偏）。
> **最新一行 = 2026-10-08 Q-F-B 批落地登记**（`portForwards[]` 状态文案与热替换 rc 的契约面收口——
> 接续 Q-F 同族行；**本批零编号判据行变更、零 wire/夹具变更**，词表门四处不动）；计数输入集表 = Q-F-B 四行
> （`pfAccepted`/`pfFails`、`portForwards[].conns`、`stats:` 行 pf 两位、additive 观测行 10 条）。
> **其下十七行 = 2026-10-09 M1 S3/S4 批落地登记**（WG → QUIC 传输层换代程序的第一批**新增为主**的判据变更；
> **含 S1a–S2b 前序切片交下的条目**——本批一次性落库）：
> ①C2 新增 `quic:` 前缀行（WG 档原串保留）②C8 值域 `{wg,quic}` ③C15 新增 `quic:` 前缀行
> ④新增 C 族 N-a…N-d ⑤新增 E 族 E-q1…E-q4 ⑥token 新增 QUIC 类端点（`type=2`）+ RPK 尾字段
> ⑦准入绑定口径（`hr-reg3` + TLS exporter；安全面）⑧隔离门第 ⑤ 条口径变更（`dangerous()` 单文件白名单）
> ⑨配置新键六个（`serve.quic`/`serve.quic_listen`/`tunConfig.transport`/`tunConfig.quicMtuCap`/
> `HOMEWAY_TRANSPORT`/`HOMEWAY_QUIC_MTU`）⑩S3-1 世代装配行（岛建连/回落 WG/出口面未启用）
> ⑪S3-1「Rebind 优先」处置行族 ⑫岛侧行族（S2a/S2b：C4'/C5'/C6'/C15'/N-a/N-b/窄路径/附加/detach/收工）
> ⑬实现口径偏离四条（S1c 三条 + S2b 一条）⑭`quic` JSON 段 + `migrations`/`migration_unconfirmed`
> ⑮已知 flake 登记（`wtransport::bind::tests` 实 socket 时序族两例 + 既登记族两例；隔离复跑全绿）。
> **本批的策略 = 「新增为主 + 取值来源变化 + 极少数行文改写」**（设计 §3.1）：WG 路径在 M1 仍活着
> （服务面自带连接走 WG、A/B 回退档亦然）⇒ WG 族行**一条不改**，QUIC 档一律**新前缀/新行**（additive）；
> 验收口径 = **两串都合法（取决于 `transport`）**；`_wg` 档全链 E2E 的 C2/C4/C5/C6/C10/C15 **原串**
> 证据见 `tools/quic-wg-e2e.sh`（读数 `/tmp/m1s3-res/`）。
> 计数输入集表 = M1 三行（C3/C13 计数含 QUIC 类 / C10 快照来源改岛 / C11 阶梯触发集 + E23 输入集）。
> **其下七行 = 2026-10-09 M1 S6 代码门补登**（代码门 r13 点出的「代码做了但没登记」补齐 + 登记文案订正 +
> 隔离门断言面扩展 + S6 整改两条实现偏离）：①Q-O 三闸拒绝行 ②准入被拒行 ③装配/生命周期/数据面归因行族
> ④N-c 节流文案订正（首 3 + 每 100）⑤`L3Bearer` 接受集含 `true`/`false` 别名 ⑥隔离门第 ⑥–⑩ 条 +
> 第 ② 条改显式清单 + 注释剥离升级 ⑦实现偏离两条（quic 档噪声源按档分流 = H1 修复；回程消费面已退归
> `未登记` 且泵收口 = A3）。**补登条目的行文与已登记条目同批可查；`未登记` 的输入集移动另记在
> 「计数输入集」节。**
> 登记生效后，E12 关闭行与 flows 计数按新行文验收（旧行文不再要求同串）；Q-D 的尺寸面按
> 「正常尺寸逐字节同串、极端输入按登记」验收；Q-G 的 UDS 路径面按「`> SUN_PATH_MAX`（平台值）」验收；
> Q-I 尾段的 `--stun=`/`--stun6=`/`--relay=`/`--ddns=` 空值形态按「接受并按 Go 语义处理」验收；
> `--public-endpoint=`/`--bind-interface=` 空值形态**不在** carve-out（前者 Go 同拒、后者登记为已知识别差异）。
> **其下十四行 = 2026-10-09 M2（WG → QUIC 传输层换代程序 **M2 身份、设备表与准入**，S1–S4）落地登记**：
> ①准入协议版本（`hr-reg3` → `hr-reg4` 四帧；安全面 + 线协议面）②岛侧准入行改写 + 新增
> ③出口侧准入/抗放大行族（九串 additive；含启动两行与 env 两行）④`why` 归因集扩展（**十七串**
> ——2026-10-09 S6 代码门 r15 的 G9③ 计数订正：原写「十六串」，逐串核对实为 17 条，行文与取值集不变）
> ⑤`ExitQuicSnapshot`/`quic` JSON 新增字段（含 S2-5 的 `send_buffer_used`）⑥引擎侧协议版本串（r14 F4）
> ⑦E-q2 频率/输入集（r14 F11）⑧E-q3 明细 `src=%v` ⑨`serve.quic_admit` 七键 + env
> ⑩token 未变更留痕（设计 §13-1① 取候选 A）⑪E6/E7/E18 保留原串（零差异登记；E8/E9 的差异走计数输入集条）
> ⑫**S4**：`per_src_fails` 缺省 10 → 16（设计 §14-1②）⑬**S4**：「正常赛跑」判据措辞改写（设计 §14-1③）
> ⑭**S4**：出口侧握手失败归因位 `handshake_peer_closed`（设计 §14-1④）。
> **本批的策略 = 「行文改写 3 处 + 取值/归因集扩展 + additive 新行/新键/新配置」**（设计 §6.1）：
> WG 族行**一条不改**；新增面向 tier 与排障脚本**均为 additive**（除点名的 3 处行文改写：
> 岛侧 `登记已发`→`准入已发起`、引擎侧 `hr-reg3`→`hr-reg4`、E-q3 明细分行补 `src=`）；
> `_wg` 档判据 = **与 M1 末逐字节同**（证据 = `tools/quic-wg-e2e.sh` 复跑，S4 读数见提交/回报）。
> **计数输入集表 = M2 五行**（E8 / E9 / `未登记` / `准入被拒` 两分档 / `flood_refused`+`retry_sent`）。
> **口径与设计文档对齐**：本批判据面变更逐条对应 `docs/reviews/M2-design.md` §6.2（草案）与
> §13/§14（实施期订正）；**未在上表出现的行文改动 = 静默破坏对齐**（判据政策）。
> **S4 的阈值裁决留痕（设计 §14-1）**：缺口径的不动 + 阈值 16 + 措辞改写三条已落表；其中
> ①的**差异登记**（设计 vs 实测）按裁定已落 `docs/reviews/M2.md` §1（该文件由 S4 立，
> **S6 续写** §2–§4 的代码门记录/威胁模型验证/判据行转交清单）。
>
> **其下三十二行 = 2026-10-09 M3（WG → QUIC 传输层换代程序 **M3 服务流迁移 + 阶梯重写 +
> 准入归因**，S1–S5 落地；S7 登记）**——登记面覆盖设计 §8.2 的 18 条草案 + 各切片「交 S7」清单，
> **逐条按代码里的实际串/实际键表登记**（S2/S4 都报过「登记须照实装串」）：
> ①E14 ②E17 ③E10/E11 输入集 ④E5 复核 ⑤E1 复核 ⑥E-q3（行文 + 接受集收窄）⑦新增 E-q5（四行）
> ⑧C8 语义更正 ⑨C11 保留 + 不产生 + C18 替代 ⑩新增 C18 六条 + 三条伴随行 + 抖动升格
> ⑪新增 C19 五条 ⑫准入关闭码 0x11–0x14 + 客户端映射 + 准入后分支 ⑬复位码 0x21–0x27 + `StreamErr`
> + `Refused` 迁移/去 RST ⑭客户端 `quic` 段（25 键 + 段存在判据）⑮出口 `serve.status` 的 `quic`
> 段（28 键）⑯`migration_unconfirmed` 收窄 ⑰`patrol` 触发源 ⑱M1「Rebind 优先」行族**整族删除**
> ⑲M1 的 `连接已断` 行改写 ⑳配置与常量（含 13 条 env、intake 取值表、term 连接级闸 = 新行为）
> ㉑零差异登记 ㉒stackb/UDS 退役（D1）+ §5.1 消费点身份订正 ㉓DC14/DC15/CA1/CA4/CA5 复核
> ㉔CA7–CA10 复核 ㉕A13 复核 ㉖公面类型清单 ㉗A12 死变体删除 ㉘A11 跨承载标注 ㉙`files_root=""`
> 小修 ㉚M3 无新增 config 键 ㉛已知 flake 补充登记（`wgcore::tests::stop_within_detaches_and_reaper_closes_wake_fd`）
> ㉜tier `connection-lifecycle.md` §3 待修订（草案交付；tier 侧触点 = 用户）。
> **同批订正两处**：①C11 触发集条追加「同日订正」——「QUIC 档零 C11 入口」只对内部触发点成立，
> NAPI 下推入口 `ClientCoreTunRecover` 未分档（缺口登记 + 归 S9 代码门/M4 设计门，见 `docs/reviews/M3.md` §5）；
> ②本批登记文内的 `ladde.rs` 笔误订正为 `client/ladder.rs`。
> **本批的策略 = 「QUIC 档新增为主 + 三处行文改写（E14/E17/E-q3 明细）+ 两处行删除
> （M1 的 Rebind 族、`连接已断` 旧串）+ 取值/键表变化」**（设计 §8.1）：
> WG 族行**一条不改**（`_wg` 档判据 = 与 M1/M2 末逐字节同，证据 = `tools/quic-wg-e2e.sh` 复跑）；
> **计数输入集表 = M3 五行**（E-q3 接受集 / `migration_unconfirmed` 窗 / DC18 输入集 /
> C11+C18 触发集 / `patrol` 触发源——见下节）。
> **实装 vs 草案的三处差异已如实登记**（不得按草案写断言）：①E-q5 的拒行带复位码位、结束行带
> 耗时、受理行**probe 也打**且与 tag 1–3 **共享节流窗**；②C19 的「已开」行是「第 %d 条」而非
> 「耗时 %v」；③§8.2-12 草案里的 `streams_*`/`stream_bytes`/`backpressure`/`sock_send_errs`
> **未进客户端 JSON 段**（落点是岛 `IslandSnapshot` 进程内读数）。
> **口径与设计文档对齐**：本批变更逐条对应 `docs/reviews/M3-design.md` §8（草案）与 §15
> （实施期订正 1–8）；**未在上表出现的行文改动 = 静默破坏对齐**（判据政策）。
> **S8/S9 承接项**（不在本表）：服务流吞吐相对门槛与 bulk/L3 共存读数（§7）、`T_recv` 真机两相位、
> 九→13 条 env 的消融标定、内存最坏值口径（设计 §14.4 的待标定项）。
>
> **其下十七行 = 2026-10-09 M4（WG → QUIC 传输层换代程序 **M4 portfwd 收口：拨号缝换轨 +
> spec 不回退核验**，S1–S5 落地；S6 登记）**——登记面 = `docs/reviews/M4-design.md` §8.2 的
> **13 条草案（§15-4 裁定「原样落」）** + 各切片「交 S6」补充清单：
> ①E-q5 扩展（why 取值集 + 受理时点）②C19/复位码 `0x25/0x26` 可达性 ③dial 流 **1B 回执**（新 wire 元素）
> ④新增观测行族 ⑤`pfFails` 输入集 + pf 不重试 ⑥`100.64.255.1` 字面拨 + `SERVER_TUNNEL_IP` 别名删除
> ⑦虚拟端口 7802/7724/7803 ⑧`targetPort=0` 旁路 ⑨C11 触发集订正（世代限定）⑩出口流计数输入集
> ⑪并发/内存口径 ⑫目标 RST→FIN 可观测面 ⑬rc 可达集与归因面 ⑭（S4）耗时上界订正 **7.1s → 12.1s**
> ⑮（S5）出口 dial 腿 `TCP_NODELAY` ⑯（S5）`wg_dial_addr` 双栈期临时物（M5 删）⑰零改动复核。
> **本批的策略 = 「判据面新增/可达性 + 两条行为差异（字面拨 / 虚拟端口）+ 一条 wire 增量（1B 回执）
> + 两条口径订正（并发内存 / 耗时上界）」**：WG 档判据行**一条不改**（证据 = `tools/quic-wg-e2e.sh`
> 复跑：`quic_lines=0` 且与 M1 前逐字节同）。
> **计数输入集表 = M4 三行**（`pfFails` / 出口 `streams_*` 的 tag=dial / C11 触发集世代限定——见下节）。
> **对 M3-design §1.2 的订正指针**：dial 行「后 = 裸字节管」只对**客户端→出口**方向成立；
> 出口→客户端 = 「**1B 回执 + 裸字节**」——**不是**帧格式变更（客户端发的字节一个没变，是**新增方向面**），见行 3。
> **口径与设计文档对齐**：本批变更逐条对应 `docs/reviews/M4-design.md` §8（草案）与 §15（主会话裁定）；
> **未在上表出现的行文/行为改动 = 静默破坏对齐**（判据政策）。
> **S6 收口承接项**（不在本表）：S4 真机三指标（需人工换网 WiFi↔蜂窝）、R3-S②/R1-S③ 的真机未取形态、
> W1 并发打点未做、热替换后状态列停在「启动中…」（**tier 侧**观察）——逐条落 `docs/reviews/M4.md`
> 的收口记录与差异登记（不冒充达标）。


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
| 2026-10-08（Q-F） | **`stats.pfAccepted`/`stats.pfFails`** 与 `stats:` 行的 `pf=a/f` | 值不变（恒 0）——**语义由「未接线的占位」改为「真值：无监听器 ⇒ 无 accept/失败」** | F1/N4：读数的人不得据此以为端口转发在跑（映射状态见 `portForwards[]` 的 failed + err） | `facade/tun_exec.rs::runner_of`、`stats_loop`；tier 无消费（只校验键存在） |
| 2026-10-08（Q-F） | **新增观测行（additive）** | 无 → 有：① 桥 `<桥名> accept 线程启动失败（{e}）—— 该桥本轮不可用`（accept/conn/pump 三面各一行）；② 隧道域/服务域各派生线程 `启动失败（{e}）—— …`（含「本世代失去自愈巡检」）；③ `服务会话暖机硬失败（原因=…）——不发布就绪`；④ `RECOVER 预算耗尽（起跑=%s，原因=%s，预算已用尽）—— 放弃等待`；⑤ `域名解析并发已达上限（N），本次等待超时`；⑥ `homeway-svc-reap 启动失败（{e}）—— 收尾线程起不来——本次 stop 就地同步收尾`；⑦ 服务域 `巡检线程 panic（已兜住）—— 本会话失去自愈巡检`；⑧ 服务域 `会话线程 panic（已兜住）—— 会话失败收工（清槽 + 域 Failed）` | F2/F3/F6/F7/F8e：静默路径改可观测（审计明文要求「spawn 失败落判据行」） | 各日志族读者；**非编号判据行** |
| 2026-10-08（Q-F） | **`unhealthyReason` 取值集不变** | 仍 = {patrol, fd, panic, stop}；**既有**巡检 3 连败 + 阶梯耗尽 ⇒ `mark_unhealthy_if_current(gen,"patrol")` **原样保留**；新增的 spawn 失败处置**不**新增该面（只记行 + 残余登记） | F7/D6：`unhealthy` 会经 App `FailGate` 触发整套重建，代价大于收益（两域数据面都不依赖 patrol） | `facade/tun_exec.rs`；tier `FailGate` 判据不变 |
| 2026-10-08（Q-F） | **`LadderRc::Deadline` 的记账语义** | `Deadline` **不计入** `LadderRc → exhausted`（`Deadline` 不是「阶梯走完未恢复」的证据，单源判据 `recover::exhausted_delta`）；等待方到点返回时**跳过** merge 后的重建决策（既有「merge 返回即本轮结束」的结构不变） | F3/D12（设计门 C4）：防「调用方预算紧张把会话推向 REBUILD」与「在途轮未完被 rebuild 掉 Client」 | `session/mod.rs`（`note_ladder_result` 与 `session_recover` 两处埋点）、`tun_exec.rs`（隧道域无 exhausted 面，仅 rc） |
| 2026-10-08（Q-F） | **`healing_dial_*` 所有调用方的最长等待** | 从「首试 + 尾试各受预算约束（阶梯可越界 ≈4×，实测 15s 预算 → 最坏 ≈64s）」→「首试 + 阶梯（含闸等待） + 尾试合计受同一预算约束」；残余越界上界 = 一个动作预算（2s）；预算表不变：服务桥/tun 桥 15s、`files.rs` 调用方预算、CLI `portfwd` 15s、daemon `budget.min(15s)` / 30s | F3（**偏离 Go 的加固**：Go 的阶梯与闸等待不受调用方预算约束） | daemon 拨号（Q-H 面，最长等待缩短）、files/term/speedtest 桥、CLI `portfwd` |
| 2026-10-08（Q-F） | **域名解析并发上限（F8e）** | 无上限 → **分档令牌池**（critical 4 = 建会话 + daemon `host reach` / background 4 = 巡检刷新）；**额度 = 在飞解析**（随 worker 生命周期，调用方超时不归还）⇒ 黑洞下该档**有界地失败**（第 N+1 个调用按预算 TimedOut），不是「不会失效」；获取等待计调用方预算；`resolve_domains` 对每个域名条目各用整份预算（N×budget）的既有形态不变 | F8e（两轮设计门 3.3/3.3'/C6）：黑洞下每分钟可漏 N 枚卡死线程；进程级单桶会让后台刷新饿死用户可见路径 | `wtransport/domain_eps.rs`、建会话路径、巡检刷新、daemon `host reach`（用户可见结论面） |
| 2026-10-08（Q-G） | **`unhealthyReason=fd` 触发集与时机**（取值集不变，仍 = {patrol, fd, panic, stop}） | 「读错误型（EBADF/EIO）」→ 新增 **HUP/ERR/NVAL 型**（TUN fd 失效/EOF 现在被**确认一拍**〔真睡眠 50ms 后复 poll〕后上报；判死**只看 hup/err/nval**，不看 readable——darwin EOF 恒带 POLLIN）；同一会话内**更早/更准**报。**残余（代码门④）**：确认拍只滤**瞬时** HUP——若某平台健康 fd **持续**报 HUP（OHOS VPN fd 语义本仓不可取证），两拍都判死 ⇒ 健康隧道被反复拆世代（如实登记） | F2：`poll_fd` 返回 revents 派生 `Ready` 掩码；读侧可读优先（不丢最后一包）+ 写侧 HUP/ERR/NVAL 立即出线 + 期限检查写死循环顶 | `facade/tun_exec.rs`（`on_error → mark_unhealthy_if_current("fd")`）、App `FailGate` **重建频率**（更早/更准，极端形态下也可能更频繁——如实写明）；`wgcore/mod.rs` 单测 `tun_read_loop_n0_with_hup_reports_dead`/`poll_fd_ready_masks` |
| 2026-10-08（Q-G） | **TUN 写侧预算语义**（`write_fd_all`，非判据行） | 修前：期限只在 `poll_fd` 入口检查 ⇒ 预算到点前**允许最后一次写入重试**；修后：**循环顶硬停** ⇒ 「恰好到点变可写」的那一次写入被放弃、直接 `TimedOut`（→ 卸源 + `unhealthyReason=fd` 重建） | F2/U1：期限检查落点写死循环顶（防「无睡眠死循环」退化） | 极端形态（长期不可写且无 HUP）下的重建时点前移一拍；正常形态（可写/超时）逐字不变 |
| 2026-10-08（Q-G） | **隧道域世代收尾等待上界** | 无界 → **≤2s/处**（`CLIENT_CLOSE_BUDGET`；五处：`request_stop` 的 Preparing 分支 / `Finish::drop` / `gen_loop` 装配窗口 / `rebuild_session` 的 new·old；`Drop for Client` 兜底**仍无界**，设计 §3-D3） | F5（Q-F 移交）：五处 `Client::stop()` 无界 join 会拖死 `tun_stop` 等待 | `facade/tun_exec.rs`/`session/mod.rs`；`tun_stop` 的 `-2 强制放锁` 频率；**注**：Q-F §7-5 的「4s 串行上界 > `STOP_WAIT=3s`」**只减不消** |
| 2026-10-08（Q-H） | **`serve.status`/`relay.status` 的 `state` 分布** | 非法 config 的 **op 触发**形态由「进程消失（统一进程 DEAD）」→ `stopped`（拒绝 + 零副作用；`enabled` = 文件真值）；`failed` 保留给**装配期失败**（合法 config 绑定失败等）与**运行期重建失败**（Go 同形）。五态语义不变、输入集移动 | F1：op 顺序「严格读 → 写回 → 改内存 → 装配」+ 零副作用 | `serve status`/`relay status` 的 state 字段消费方；`docs/reviews/QH.md` §3 |
| 2026-10-08（Q-H） | **C13** `候选端点（%d 条，标记·学习=…）` 的 `·学习` 判定集 | 判定「是否在 token 候选里」由 `static_cands`（只装 IP 字面量）→ **token 序已解析候选**（IP 字面量 + 域名首解；= Go `DescribeCandidatesWithLearned(list, cands)` 的 `cands`，`bind.go:208-226`） | 代码门 M3：域名 token 下域名首解候选被误打 `·学习`（Go 不打）——**行文不变、输入集修正** | C13 数值、`session/mod.rs`（C13 块 `known` 判据）、C14 目标选取测试（同源断言） |
| 2026-10-08（Q-H） | **控制面新增日志行（additive，非判据行）** | 无 → 有：`control: 连接拒绝（并发上限 64）`；`control: 连接握手超时（10s 未 hello）——断开`；非默认 state 的 `（state=… 非默认 state——不等 launchd KeepAlive，直接拉起）`；C14 的失败行/无候选行（见 C14 登记） | F5/F14/F17：拒绝/超时/跳过路径改可观测 | 各日志族读者；`reject`/`dialFail` 等既有计数不受影响（新行不进状态面） |
| 2026-10-08（Q-G） | **新增 additive 观测** | 无 → 有：`files` UDS `chmod 0600` 失败告警（Go 有、本仓原为静默 `let _ =`）；`state.rs` 的 key/台账/目录收紧失败告警 + **存量 key.bin 读路径归一告警**（原静默；代码门① 补强）；`nodestate` config 模板 / `carriers::save_json_atomic` / `daemon_cli` spawn 日志 / `endpoint_cache` 端点缓存 的收紧失败告警（代码门② 统一为告警不阻断）；F5 的 detach 记行（`等待 client 线程收工超时（CLIENT_CLOSE_BUDGET）——放行自退（引擎线程由收割线程收口）`） | F4/F5：静默失败改可观测 | 各日志族读者；**非编号判据行** |
| 2026-10-08（Q-F-B） | **`stats.pfAccepted`/`stats.pfFails`**（`tunStatusJSON.runner.stats` 两键 + `stats:` 日志行同源） | 「恒 0 的占位真值（无监听器 ⇒ 无 accept/失败）」→ **真计数**：`pfAccepted` = accept 准入数（阀拒绝不计、拨号失败仍计）、`pfFails` = 拨号失败数（RST 收口那些；阀拒绝与线程启动失败不计） | Q-F-B F1/F3：实装真监听器（Go `app_portfwd.go` 的 `pfAccepted`/`pfFails` 同语义） | `facade/tun_exec.rs::runner_of`、`facade/portfwd.rs::PfCounters`；tier 只校验键存在（无消费） |
| 2026-10-08（Q-F-B） | **`portForwards[].conns`** | 恒 0 → **真连接数**（accept 准入 +1；conn 线程与两枚泵线程全结束才 −1——RAII `FlowGuard` 随最后一枚 `Arc` 持有者 drop 回退） | 同上（Go `st.conns` 的 `defer` 同义） | tier「监听中 · N 条连接」真实可用；`install` 换表后旧表的在途连接不再计入新表（Go 同形——连接与快照同属一次装表） |
| 2026-10-08（Q-F-B） | **`stats:` 日志行**（`stats: fdReadBytes=%dB fdWriteBytes=%dB ｜ pf=a/f`） | 恒 `pf=0/0` → **真值**（**行文形态逐字不变**；抽 `stats_line` 纯函数） | 同上 | 世代日志读者；`facade/tun_exec.rs::stats_line` 单测 |
| 2026-10-08（Q-F-B） | **新增观测行（additive，非判据行）** | 无 → 有（10 类）：① `port-forward: 监听 127.0.0.1:{listen} 失败（{e}）——该条映射不可用，不影响隧道`（**Go 逐字**）；② `port-forward: 127.0.0.1:{listen} -> {target} 监听中`（**Go 逐字**）；③ `port-forward: 已停止全部监听器（{n} 个）`（**Go 逐字**）；④ `port-forward {listen}: 并发流已达上限 {max}，拒绝（累计拒绝 {r}）`（**Go 逐字**，节流 `<=5 ∥ %50`）；⑤ `port-forward: {listen} -> {target} 拨号失败 #{n}: {e}`（**Go 逐字**，节流 `<=5 ∥ %20`）；⑥ 装配期防御面三条（`listen` 非法 / 超条数 / `target_ip` 非法——err 原文直记）；⑦ 旧监听器 `未在预算内退出——已在后台自退`（install/stop_all detach）+ `{listen} 旧监听器未在预算内释放——已重试 bind 仍失败`（双记行）；⑧ accept 瞬态/致命（复用 Q-E F5 分类件行文 + **该条状态转 failed** 的置位行）；⑨ 监听线程/conn 线程/泵线程 spawn 失败行 + **转发流拆半失败 / 本地 fd 复制失败行** + **pf 泵的读错误/写失败行**（`port-forward[up\|down] …`——桥侧 `桥泵[up\|down]` 的行文与「有流量才记」门槛**逐字/逐条不变**；pf 侧读错误**无条件记行**〔零字节 RST 也留痕，对齐 Go `pkg/netpipe` 的错误口径〕、EOF 仍零日志）；⑩ 装配期装表后 stale 复查撤回行 | Q-F-B F1/F4/F8：监听器全生命周期可观测（Q-F「spawn 失败不静默」纪律延伸）；①–⑤ 与 Go 行文**逐字同形**（`app_portfwd.go` 的监听/停表/阀/拨号四行 + `pfAccept` 的失败行），其余为 Rust 侧新增面 | 世代日志读者；**非编号判据行**（无 E*/C*/R* 行文变更）；`facade/portfwd.rs`/`facade/tun_exec.rs` 单测逐字断言 |
| 2026-10-08（Q-J） | **E21** 绑卡族（行文与渲染形态**不变**） | ① linux 上 `index=0` **不再蕴含「不可钉」**（钉卡守卫改平台分档：darwin 拒 `index==0`〔`IP_BOUND_IF=0`=解绑〕，linux 按名 `SO_BINDTODEVICE`、index 不参与）；② darwin 候选面**排除** `!index_ok`（不可钉不参与挑卡），linux 不过滤；③ 三处 index 键面改 **name 优先**（`bindwatch::state_of_from` 纯函数 / 重挑比较 `iface_same` / `select_best` 的默认路由偏好）；`IfaceFingerprint` 仍保留 index 字段（「换 index ⇒ 重钉」信号不丢） | F5：`if_nametoindex` 失败静默 index=0 的完整语义（macOS 0=解绑但 `setsockopt` 成功 ⇒ 「已钉卡」判据反向；linux 按名绑定与 index 无关，旧硬拒属误伤） | E21 取值路径不变（`index=%d` 渲染形态不动）、`egress.rs`（`candidate_pinnable`/`iface_same`/`pin_socket_to_iface` 平台守卫）、`bindwatch.rs`（`state_of_from`）；单测 `if_nametoindex_zero_platform_split`/`candidate_filter_platform_split`/`iface_same_three_states`/`state_of_from_name_keyed`/`repick_same_name_with_index_flap_stays`；**linux 语义放宽**（旧硬拒不再发生）；**平台分档补门（代码门 L1）**：linux 空网卡名 ⇒ `SO_BINDTODEVICE(optlen=0)` 是内核级「解绑且成功」——与 darwin `IP_BOUND_IF=0` 同型，按名面补硬拒（现调用方不可达，属不变量结构化） |
| 2026-10-08（Q-J） | **E4** `dns 代答就绪：… upstream=%s`（行文**不变**） | 取值来源扩展：`serve.dns_upstream` 配置生效时 `upstream=` = **配置列表**（静态、**不做 mtime 跟随**）；缺省/空 = 今日的 `/etc/resolv.conf` nameserver 列表（跟随语义不动） | F2：显式上游覆盖（opt-in；macOS 出口的 resolv.conf 非真源，覆盖是等价能力收口） | E4 取值（行文不变）、`dnsproxy.rs`（`Upstreams::with_static`/`text()`）、单测 `static_upstream_override_no_follow` |
| 2026-10-08（Q-J） | **C14** + 未编号行 `UDP 默认路径：…`（行文**逐字不变**） | **取值来源变**：`serve.dns_probe_target`/`stun_probe_target` 配置生效时，udpcap 探针目标 = 配置值（修前硬编 `&[]` = 默认常量）；默认逐值不变。**耦合写明**：`dns_probe_target` 一键喂**挑卡 / 健康探针 / udpcap DNS:53 三路**——把该键指到诊断死地址会同时影响挑卡、健康探针与 C14 取值 | F2：探针目标可配置（默认 = 修前硬编值） | C14 取值（行文不变）、`engine.rs`（`probe_once(dns_targets, stun_targets)`）、`bindwatch.rs`（挑卡 `pick_targets` 不吃 env）、`serve_cli.rs` 值域；单测 `pick_targets_ignore_env_seam`/`f2_keys_take_effect` |
| 2026-10-09（M1 S3/S4） | **C3 / C13**（token 端点数与候选清单条数） | 计数输入集：WG/中继/域名类 → **含 QUIC 类端点**（`EndpointKind::Quic`）；`C13` 的 `·学习` 判定沿用（quic 档的候选来自 token，不走学习缓存——§2.7 收窄） | M1 token 新增端点类（E3 同批）；WG 档过滤（`token::wg_endpoint_refs`）保证 C3/C13 在 WG 档的数值与 M1 前一致 | C3/C13 行、`server/state.rs`（`tok_endpoint_refs`）、`facade/tun_exec.rs`（候选清单打印）；**行文逐字不变** |
| 2026-10-09（M1 S3/S4） | **C10**（`link:` 快照形态） | 取值来源：`via/ep/rtt` 由 `wgcore::Snapshot` → **quic 档由岛 `IslandSnapshot`**（`via ∈ {direct,relay,none}` 词表与 `link{via,ep,rttMs,at}` 键面**零变化**）；巡检形态（服务会话）不动 | 承载替换（设计 §4.3）；`rtt` = 岛快照的 `PathStats::rtt`（quic 档不再用巡检实测的墙钟差） | C10 行、`facade/tun_exec.rs`（patrol_loop 分支）、`facade/tun_status.rs`；证据 = `generation_l3_rides_quic_datagram_against_local_exit` 的 `link={"at":…,"ep":…,"rttMs":1,"via":"relay"}` 读数 |
| 2026-10-09（M1 S3/S4） | **C11 / E23**（阶梯触发集 / 入站新源行） | ①`C11` 触发集：含「换源（R2）」类 → quic 档由协议迁移吸收（quic 档的巡检失败面走「Rebind 优先 + `patrol` 分类」，**不再驱动 WG 阶梯**）；档位/时间窗**零改动**（`session/recover.rs` M1 不动）②`E23`：kind=5（QUIC 载荷）腿帧**显式跳过** `note_new_src` —— QUIC 档的「新源」由 **E-q2**（`quic: 路径变更`）承接 | ①设计 §2.3（迁移保连接 ⇒ 不再产生 R2 类触发；阶梯重写 = M3）②设计 §3.4 E23 行（否则 QUIC 腿会打出 WG 语义的「入站新源」行） | C11 行（行文不变）、`facade/tun_exec.rs`（quic 档失败面分支）、出口 `server/bind.rs`（kind=5 分支跳过）、`server/engine.rs`；真机恢复实录的解释口径随 M6 |
| 2026-10-09（M1 S6） | **N-c 的 `回程队列满` / `未登记`（两字段的行文与顺序不变）** | 输入集移动：TUN 写线程已退（消费者消失）时的回程丢弃由「计 `回程队列满`」→ **计 `未登记`**（并一次性记行 `回程面已终止（TUN 写线程已退）—— 回程泵收口`，泵随之退出）；`回程队列满` 自此**只**表示真队列满（丢新） | 代码门 r13 的 A3：队列没满而是没有消费者，旧口径把两回事归一类会让排障误判（旧注释自称「丢弃语义相同」）| N-c 行文与 `quic.drops` 四键（`too_large/send_buffer_full/return_queue_full/unregistered`）**零变化**；`crates/homeway-quic/src/{tun.rs,client/dataplane.rs}`；单测 `return_push_reports_gone_when_consumer_exits`/`return_pump_reports_write_thread_gone_and_stops`（后者断言 `return_queue_full == 0`） |
| 2026-10-09（**M2 S1–S3 落地；S4 登记**） | **E8** `peer: ~ dev=%s refresh (idle=%s) n=%d/%d` | QUIC 档 refresh 输入集 = **`R4` 刷新帧**（60s 节拍；M1 的 `hr-reg3` 刷新帧退役）；`idle=` = **距上次刷新帧**（节拍 ≤60s ⇒ 比 WG 档「距上次注册」规整）；**且刷新成功不再产生 E-q2 行**（r14 F11：刷新不重绑，只走 `table.register` 的 Refreshed + C15' 行） | **行文逐字不变**；数值语义随四帧/刷新面切换（设计 §2.3 E8 行） | E8 数值、`server/table.rs`（零改动）+ `client/register.rs`；`tools/quic-island-e2e.sh` 的刷新断言 |
| 2026-10-09（同上） | **E9** `peer: ! reject reason=…`（`no-token` 档） | QUIC 档 `no-token` 输入集 = **{MAC 已过、`ts` 超 ±90s 窗}**（`table.rs::verify` 把 `verify_reg` 的 `Expired` 归 `NoToken`）；**MAC/nonce 类拒绝不进本表**（在出口面计数，归 `quic: 准入被拒`）；`revoked`/`table-full`/`ip-conflict` 触发集不变 | **行文逐字不变**（r14 F1 订正了设计 v1 的「输入集为空」错论） | E9 数值、表内拒绝计数；双面呈现 = 表内 `reason=no-token` + 出口 `引擎裁决拒绝` |
| 2026-10-09（同上） | **`quic: 丢弃 … 未登记=%d`（E-q3 四字段之一）** | 输入集增「**未完成 Proof / 未绑定**连接发来的数据报」（未认证门禁 + 双期限把窗口收窄 ⇒ 数值语义更准）；`源校验拒` 只对**已绑定**连接的入站包计数 | **行文与四字段序不变**（S4 只改明细文本，见登记表 E-q3 条） | N-c/E-q3 数值、`exit/{conn,bridge}.rs`、`quic` JSON 段的 `drops.unregistered` |
| 2026-10-09（同上） | **`quic: 准入被拒`（`regs_rejected`）与两分档 `challenges_refused`/`proof_rejected`** | `regs_rejected` 输入集 = {帧非法 / 版本不符（H2/H3）/ 重复 Hello / 已绑定再准入 / 连接未绑定 / 刷新帧未绑定 / 刷新身份不符 / 刷新帧设备不在册 / nonce 类 / MAC 类 / 引擎裁决拒绝 / 冷却中拒 Hello}（逐串见登记表的 `why` 条）；`challenges_refused` = **未发 Challenge** 的拒绝，`proof_rejected` = **Proof 阶段**拒绝（nonce/MAC/引擎/身份不符） | r14 F7/F1 + 设计 §13-4 的拆细（fail-visible） | 出口排障读者；`exit/conn.rs` 的 `Counter::{BeforeChallenge,AtProof}` 分档 |
| 2026-10-09（**M2 S4 落地**） | **`flood_refused` / `retry_sent`（每源闸与 Retry 的计数）** | 输入集 = 每源闸的「**未完成/被拒**」尝试：① 过闸即记账、**完成即销账**（`SrcGate::completed`）；② 被拒的尝试**仍计数**（`flood_refused = K − F`）；③ **对端主动关闭与对端静默都在同一输入集内**（设计 §14-1① **不豁免**——出口侧可区分二者，但只进 `handshake_peer_closed` **归因位**）；**缺省预算 10 → 16**（§14-1②；窗 10s 不变） | 「重连洪泛有界」的计数口径（S3 落地 + S4 裁决） | `exit/{mod,admit}.rs`；快照 `retry_sent`/`flood_refused`/`handshake_peer_closed`；启动行「抗放大面」取值 |
| 2026-10-09（**M3 S1–S5 落地；S7 登记**） | **E-q3 的 `源校验拒` 输入集**（QUIC 档） | 内层包源 ∉ `{tunnel_ip, tun_ip}` → ∉ **`{tun_ip}`**（QUIC 档接受集收窄；E-q3 明细同步改单元素集） | 设计 §6：`tunnel_ip` 在 QUIC 档无合法来源（源校验接受集收紧 = 安全面收紧）；`tunnel_ip` 字段保留但不再参与校验 | E-q3 数值、`quic` JSON 段的 `drops` 面（`源校验拒` 不在 JSON 段；行面）；`exit/conn.rs`；单测 `datagram_with_illegal_source_is_dropped_and_counted` |
| 2026-10-09（同上） | **`migration_unconfirmed`（状态 JSON + N-b 行）** | 判定窗口 60s（巡检拍）→ **≤1 个快探预算（缺省 700ms）**；确认面由「`udp_rx` 增量」→ **「快探回显」**；语义由「慢变量告警」→ **动作前置条件**（置位 ⇒ 允许走 R） | 设计 §3.1（QUIC 档阶梯重写） | `quic.migration_unconfirmed` 读者、N-b 行读者、M1 真机「路径变更已验」复现脚本；`driver.rs`（M 确认失败行 `quic: 迁移未确认（一个快探预算内无对端回包 ⇒ 回落重连/重赛跑）`） |
| 2026-10-09（同上） | **DC18** `intercept：dialOk=N dialFail=N reject=N flows=N` | QUIC 档服务流（files/term/speedtest 经 STREAM tag 分发）**不再进拦截层** ⇒ 不再抬高 `dialOk`/`flows`（对应 WG 档服务腿仍照计——D1 下该路径不变） | M3 服务流改 tag 分发（D1） | DC18 数值、`serve.status` intercept 段；按「QUIC 档服务流也会 +dialOk」写断言的脚本须改。**证据**：`[e2e5] exit.exempt_lines=0`（QUIC 档零 exempt/dialok 行） |
| 2026-10-09（同上） | **C11 触发集（QUIC 档）** | M1 登记的「quic 档的巡检失败面走『Rebind 优先 + `patrol` 分类』、**不再驱动 WG 阶梯**」→ **M3 S4：QUIC 档零 C11 入口**（`recover()` 全部调用点都在 `!l3_on_island()` 守卫下）+ **C18 承担 QUIC 档恢复时间线**（替代关系） | 设计 §3.4/§8.2-9；阶梯重写 | C11 行读者（须按承载分档）；`session/recover.rs` **零 diff**；新增 `client/ladder.rs` 的 C18 族（见登记表）　**同日订正（S6 扫查）**：「QUIC 档零 C11 入口」只对**内部触发点**成立（挂起唤醒 / 巡检失败 / 3 连败 / 待发包下推四处均在 `!l3_on_island()` 守卫下）；**NAPI 下推入口 `ClientCoreTunRecover(from)` 未分档**（`facade/mod.rs` → `tun_exec.rs` 的隧道域 `recover` → WG 阶梯，全链无守卫）⇒ QUIC 档下该入口仍会产 `RECOVER` 族行并做 WG 动作。**缺口登记 + 归属**：homeway-rs `docs/reviews/M3.md` §5（S9 代码门复核 + M4 设计门定下推语义 + tier 触点）；读到 `RECOVER` 行时须先确认世代承载 |
| 2026-10-09（同上） | **`unhealthyReason=patrol` 的触发源** | 「M1 = 连接死**即时**分类」→ 「M3 S4 = 岛内阶梯走完 M/R（B 门：连续 2 次 R 失败 **且** 窗 ≥10s）才上报」；**值域不变**（`{patrol,fd,panic,stop}`）；另：QUIC 档的**环境噪声位**（M1 S6 为 quic 档关掉的 WG 腿 `last_local_send_err` 那一路）由岛侧 `sock_send_errs`（非 `WouldBlock` 的 errno 白名单 + 新鲜度窗 + rebind 清零）等价替换承担 | 设计 §3.1/§3.5（分档纪律：不读另一条腿的错误）+ S4 偏离 1（阶梯落岛内） | `facade/tun_exec.rs`（`quic_unhealthy_signal` 如实判不健康）、App 的世代重建时点（`FailGate`）；新增行 `quic: 快探阶梯走完 M/R…` / `quic: 岛上报不健康（patrol）…`；单测 `connection_death_goes_to_the_ladder_instead_of_unhealthy` |
| 2026-10-09（**S9 代码门补登**） | **`quic: 流面参数（…）` 的 `recv_window=` 读数 + 新增 `conn_recv_window=` 字段** | `recv_window=262144B` → **`recv_window=4194304B`**（格式另见主表的行文改写条：插入 `conn_recv_window=%dB`） | 每流接收窗 256 KiB → 4 MiB（S9 整改）+ 连接级聚合闸 8 MiB 显式化 | 出口/岛的启动行读者（排障脚本按读数写断言者须改）；落点 `crates/homeway-quic/src/driver.rs` + `crates/homeway-quic/src/exit/mod.rs` |
| 2026-10-09（同上） | **`链路重连完成（原因=%s，耗时 %v）` 的 `%v` 输入集（数值语义）** | 「确认探活耗时」（旧实装重设 `pending.at` = 完成时刻 ⇒ 落纸 `0s`/`1ms`）→ **「R 全周期：发起 → 确认成功」**（含握手 + 四帧准入 + 确认探活） | 代码门 r18 ②-6：与设计 §3.1 的该行语义（重连代价）对齐；排障读者不再误读为「重连只要 1ms」 | C18 行读者；落点 `crates/homeway-quic/src/client/ladder.rs`（`on_reconnect_result` 保留发起时刻）+ 用例 `reconnect_elapsed_covers_dispatch_to_confirm` |
| 2026-10-09（同上） | **读面 `ReadError::ConnectionLost` 的分类** | typed 错误（`StreamErr::ConnectionLost` ⇒ 消费侧 `ErrorKind::Other`）→ **`Closed`/EOF** | 代码门 r18 ①-4：与 `clear_on_connection_loss`（连接死 ⇒ 读回 EOF，§1.6/A9 的 EOF 同形性）统一，消除「同一连接死在 250ms 拍内给两种结论」的竞态窗 | 服务流读面消费者（`facade/quic_stream.rs`；两者都按「流结束」收 ⇒ 行为面不变）；落点 `crates/homeway-quic/src/client/streams.rs`（`classify_read`）+ 用例 `classify_read_maps_connection_loss_to_eof` |
| 2026-10-09（**M4 S1–S5 落地；S6 登记**） | **`stats.pfFails`**（`tunStatusJSON.runner.stats.pfFails` + `stats:` 行两位 + `port-forward… 拨号失败 #n` 行的触发集） | 输入集 = 「WG 裸拨失败（`ConnErr` 归因）」→ **「承载拨号失败」**：`0x25`/`0x26`（出口拒/超期）、本端额度耗尽（`StreamErr::Busy`，**无复位码**）、回执面异常（`Closed`/`ConnectionLost`/首字节非 `0x01`）、防御面（`NotSupported`/`Unbound`/`BadTag`）；**行文与节流窗（`<=5 ∥ %20`）逐字不变** | M4 拨号缝换轨（`PfDialFn` 签名与阀/计数/热替换逐字保留；承载差异止于闭包内） | `facade/portfwd.rs` 的 `dial_failure_rst_and_counters` 等计数用例；`stats:` 行读者；本机应用读 `ConnectionReset` 的时点后移一个 RTT（登记，W9） |
| 2026-10-09（同上） | **出口服务流计数（`streams_open` / `stream_refused` / `stream_bytes_in`/`out` / `streams_closed`）的 tag=dial 输入集** | dial 只计拒（`stream_refused`）→ `streams_open` = 拨号成功数（**时点 = 回执写成功之后**）；`stream_refused` = 五支拒；`stream_bytes_*` = **泵字节（不含 1B 回执）**；`streams_closed` = 泵收 | §3.2/§3.3 的计数口径写死（1B 回执是唯一不计入字节账的 wire 字节） | `serve status --json` 的 quic 段；`exit/tests.rs` 的 `stream_bytes_in/out = 12/12` 断言 |
| 2026-10-09（同上） | **C11 触发集（NAPI 下推入口，世代限定）** | M3 同日订正的「`ClientCoreTunRecover` 未分档 ⇒ QUIC 档该入口仍产 RECOVER 族行」→ **已分档**：`l3_on_island()==true` 的世代**零 C11 族行**（改产 `quic: 恢复下推` additive 行）；`bearer=Quic` 但岛未就的回落世代仍产 C11 族行（走 WG 原路） | M4 S4（§15-1 裁定「本期修」） | C11 行读者须按**世代承载**判定；`facade/tun_exec.rs::recover`；tier 文档修订稿 |

