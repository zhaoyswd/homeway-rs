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
| E9 | `peer: - dev=%s reason=ttl (idle=%s) …` / `reason=stale` / `peer: ! reject reason=revoked|no-token|verify` | `pkg/servercore/peers.go:466` / `:509` / `:369-373` | （需 TTL 到期/表满/吊销注入，R2/R3 补采） |
| E10 | `intercept: tcp %s %v ← %v（dialok）`（kind=transit|exempt；**真凭据判据**） | `pkg/intercept/intercept.go:378` | speedtest 腿为 exempt 形态：`intercept: tcp exempt 127.0.0.1:7803 ← 100.64.56.143:36862（dialok）`；transit 形态需应用流量（R1 补采） |
| E11 | `intercept: tcp %s %v ← %v 关闭` | `pkg/intercept/intercept.go:389` | `intercept: tcp exempt 127.0.0.1:7803 ← 100.64.56.143:28709 关闭` |
| E12 | `udp intercept: 会话 #%d %s 建立（%v ← %v）` / `… 关闭（%v ← %v）` | `pkg/intercept/intercept.go:560` / `:641` | （需 UDP 应用流量，R1 补采） |
| E13 | `speedtest: 会话 #%d role=%s warmup=%s window=%s`（受理）/ `… role=recv bytes=%d（含预热 %d）用时=%dms` / `… role=send bytes=%d（含预热 %d）用时=%dms`（结算） | `pkg/speedtest/speedtest.go:209` / `:238` / `:294` | `speedtest: 会话 #8 role=send bytes=203355105（含预热 45677895）用时=10183ms` |
| E14 | `files 就绪：root=%s (rw) sock=%s（隧道IP:%d 经拦截层转投）` | `internal/server/serve.go:467` | `files 就绪：root=/Users/zhaozhe (rw) sock=/tmp/homeway-rs-exit-1/files.sock（隧道IP:7802 经拦截层转投）` |
| E15 | `term: 检测规则已加载 %d 份（覆盖目录 %s）` | `pkg/term/service.go:378` | `term: 检测规则已加载 22 份（覆盖目录 /tmp/homeway-rs-exit-1/agent-detection）` |
| E16 | `# Serving terminal sessions on sock=%s (shell=%s, history=%s, features=%s, vt=%s)` | `internal/server/serve.go:508` | `# Serving terminal sessions on sock=/tmp/homeway-rs-exit-1/term.sock (shell=/bin/zsh, history=1…` |
| E17 | `speedtest 就绪：sock=%s（隧道IP:%d 经拦截层转投；内存收发不落盘）` | `internal/server/serve.go:537` | （烟囱日志在档，措辞出处已核） |
| E18 | `凭证台账：%d 行记录 / %d 枚在用凭证（其中 %d 行已吊销；吊销即时对新注册生效）`（吊销/续期语义判据） | `internal/server/serve.go:239` | `凭证台账：1 行记录 / 1 枚在用凭证（其中 0 行已吊销；吊销即时对新注册生效）` |
| E19 | `后端身份：标签 %x ｜公钥 %x…` | `internal/server/role.go:97` | （烟囱日志在档） |
| E20 | `公网端点：已按 **--public-endpoint 配置**公布 %v（跳过 UPnP/STUN 推断；写进 %s）`（显式端点路径；同族另有 STUN/UPnP 推断各形态行 `publicendpoint.go:187-218`） | `internal/server/publicendpoint.go:129` | `公网端点：已按 **--public-endpoint 配置**公布 [127.0.0.1:42642]（跳过 UPnP/STUN 推断；写进 public_endpoint.txt）` |
| E21 | `绑卡：自动挑到 %s（%s）` / `绑卡看护：网卡 %s 探针失败…` / `…连续探不通，重新挑卡` | `internal/server/serve.go:263` / `internal/server/bindwatch.go:148` / `:152` | `绑卡：自动挑到 en0（index=6 up addrs=[192.168.3.12/24]）` |
| E22 | `dns: q=%d qtcp=%d resp=%d filter=%d trunc=%d fallback=%d fail=%d drop=%d malformed=%d aaaa-mixed=%d`（DNS 代答计数行，debug 级周期输出；与 E10 同族计数语义） | `pkg/dns/server.go:162`（`StatsLine`，判据行注记 `:159-161`） | （需 DNS 查询流量，R1 补采） |
| E23 | `入站新源：%v（%s，%d 字节）`（源学习/漫游跟随证据，debug 级） | `pkg/servercore/bind.go:799` | （需换源/新源场景，R2 补采） |

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
| C8 | `warmup pong: 就绪（判据=%s）`（**APP 核形态**暖机判据，判据=wg） | `clientcore/cmd/clientcore/tunmode.go:751` | （需 APP/两阶段形态，R1/R7 补采） |
| C9 | `attached（数据面已接管 fd=%d，L3 直通）`（**APP 核形态**数据面判据） | `clientcore/cmd/clientcore/tunmode.go:822` | （需 TUN fd，R7 补采） |
| C10 | `link: via=%s ep=%s rtt=%dms（新栈状态快照）` / `link: via=%s ep=%s rtt=%dms（服务会话巡检）` | `clientcore/cmd/clientcore/tunmode.go:982` / `clientcore/hostsession/service.go:640` | 巡检形态：`link: via=direct ep=127.0.0.1:42641 rtt=0ms（服务会话巡检）`；快照形态需 APP 核（R7 补采） |
| C11 | `RECOVER R1 重握手（原因=%s）：补注册 + 丢会话（保采纳）` / `RECOVER R2 换源（原因=%s）：换本地 socket（保采纳）` / `RECOVER R3 重赛跑（原因=%s）：清采纳，学习缓存候选兜底` / `RECOVER 恢复于 %s（原因=%s，起跑=%s，耗时 %v）` / `RECOVER 走完 R1→R3 仍未恢复（…）—— 交上层升级` | `clientcore/hostsession/recover.go:181` / `:188` / `:198` / `:201` / `:206`（同族：零档位恢复 `:151/:155`、动作生效复探 `:151`、本地动作失败 `:223`、R1 补注册未发出 `:168`、丢会话失败 `:175`） | （需链路故障注入，R2 补采；档位/节拍/时间窗真源 = `tier:docs/agents/connection-lifecycle.md`） |
| C12 | `启动（无 TUN 服务会话）`（服务会话启动形态） | `clientcore/hostsession/service.go:246` | `服务会话: 启动（无 TUN 服务会话）` |
| C13 | `候选端点（%d 条，标记·学习=来自巡检缓存/中继 hint）：%s` | `clientcore/hostsession/session.go:249` | `候选端点（2 条，标记·学习=来自巡检缓存/中继 hint）：192.168.3.12:42641(LAN)、127.0.0.1:42641(LAN)` |
| C14 | `出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v`（参照点探测判据；失败形态 `session.go:260`） | `clientcore/hostsession/session.go:280` | `出口能力：构建 homewayd-dev ｜ 默认路径 UDP：DNS:53 可用 / 通用（非 53）可用 / 实测 还没实测样本（这台出口还没转发过 UDP） ｜ 探测往返 0s` |
| C15 | `RREG 注册刷新 → %v（dev=%s，中继=%v）`（60s 巡检注册刷新） | `clientcore/internal/wtransport/bind.go:933` | `服务会话: RREG 注册刷新 → 192.168.3.12:42641（dev=9b8bf127，中继=false）`（失败形态 `:930`） |
| C16 | `就绪（会话在位%s）`（服务会话形态核心**就绪**判据；`local-exit.sh client-add` 的等待点） | `clientcore/hostsession/service.go:423` | `服务会话: 就绪（会话在位，无桥直通）` |
| C17 | `已收工（state=%s）`（会话收工） | `clientcore/hostsession/service.go:856` | （停机/断开场景，R2 补采） |

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

## 已知口径注记

- E10 的 `dialok` 计数**仅 TCP**（`pkg/intercept/stats.go:9-11`：flows 退役后 UDP 会话不再计
  dialok；UDP 观测走 E12 会话行）。Rust 侧计数语义必须同此口径。
- E1 打的是**配置端口**；实际端口（占用退让后）落 `<state>/serve/listen_port.txt`，token
  端点跟实际端口走（`serve.go:553` 注释）。
- E3 的 token 行有去重纪律：端点没变不重打（`serve.go:157` lastToken）。
