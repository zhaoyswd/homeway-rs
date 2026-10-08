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
| 2026-10-08（Q-F-B 批落地） | **`portForwards[]` 状态文案与 `ClientCoreTunSetPortForwards` 返回码**（契约面行为变更，非编号判据行；**接续 Q-F 该条**——Q-F-B = Q-F 的 portfwd 挂账项收口批） | ① `state`：恒 `"failed"`（Q-F 诚实态）→ **`"listening"`（真 bind 成功且 accept 线程在位）/ `"failed"`（真 bind 失败 / 装配期破损配置 / accept 致命错误的迟到失败）**；② `err`：恒「手机核未提供端口转发监听（127.0.0.1:<listen> 未监听）——该映射在当前版本不可用，不影响隧道」→ **空串（成功）/ 真 bind 失败 `errno` 原文（失败）/ 装配期精确 err（破损配置）/ accept 致命错误原文（迟到失败）**；③ `code`：空 → **`bind_failed`（真值，tier 渲染「端口被占用」）**；空码**只剩非常态面**（装配期破损配置 / 迟到失败——spec 的空码兜底路径）；④ `conns`：恒 0 → **真连接数**（accept 准入 +1、连接结束 −1）；⑤ **未 attach / 未装表期 与 收工后**：`portForwards` 为空数组（tier 显示「启动中…」）——HEAD 是「失败 · 未提供…」；⑥ rc：恒 `-1` → **`0`（有承载：已受理并真装表）/ `-1`（无世代或世代已收口——改动随下次连接的 tunConfig 生效）/ `-2`（JSON/校验不过，含新增条数上限 8）** | Q-F-B：实装真监听器（Q-F 挂账项收口）。`bind_failed` 由「具体化假归因」转**真值**（真 `bind()` 失败），spec「失败原因 MUST 携带稳定枚举 code」由「有意偏离」转**达标**（限定语：除非常态空码面） | `tier:openspec/specs/port-forwarding`「端口映射的建立与访问」SHALL 由**已知不达标**转**达标**、「映射状态可见」失败码 MUST 转**达标**；tier 两处失义文案（`PortForwardsPage.ets` 的 `dirty` 兜底提示 / `TierVpnExtensionAbility.ets` 的 rc 日志）**随本批自愈**；`facade/portfwd.rs`（`PfRuntime`）/`facade/tun_exec.rs` 单测；**`fixtures/` 无 portForwards 夹具 ⇒ 无字节夹具变更**；`tools/check-vocab.sh` **零改动**（声明集/缺席表/manifest/tier 码表四处不动） |

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
> 登记生效后，E12 关闭行与 flows 计数按新行文验收（旧行文不再要求同串）；Q-D 的尺寸面按
> 「正常尺寸逐字节同串、极端输入按登记」验收；Q-G 的 UDS 路径面按「`> SUN_PATH_MAX`（平台值）」验收；
> Q-I 尾段的 `--stun=`/`--stun6=`/`--relay=`/`--ddns=` 空值形态按「接受并按 Go 语义处理」验收；
> `--public-endpoint=`/`--bind-interface=` 空值形态**不在** carve-out（前者 Go 同拒、后者登记为已知识别差异）。

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
