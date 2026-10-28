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
| E1 | `serve 就绪：quic=:%d（配置端口；被占用会自动退让）tunnel=%v files=%d term=%d speedtest=%d dns=%v tokens=%d key=%x…`（**M5 改**：`wg=:` → `quic=:`——该端口语义 = 出口唯一公共 UDP 端口（QUIC）；`files/term/speedtest` 三字段**保留原值 7802/7724/7803**（它们是**客户端侧「端口 → STREAM tag」映射的输入**，不是出口侧监听端口——口径注记见文末「M5 批」） | `internal/server/serve.go:552` | `serve 就绪：quic=:42641（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true tokens=1 key=7bab5077e7ea…` |
| E2 | `凭证：现有凭证全部不可用（首启或已吊销）——已铸出新凭证（客户端需重新粘贴新 token）`（首启铸出） | `internal/server/serve.go:226` | 同串（exit stdout.log） |
| E3 | `客户端 token（粘进 App 的「添加主机」即可；%d 个端点）：%s`（**行文零改**；M5 S5t 起 token 载体 = `hmw2` 段容器，见文末「M5 批」L-12） | `internal/server/publicendpoint.go:370` | `客户端 token（粘进 App 的「添加主机」即可；2 个端点）：hmw2e6tQd…`（端点变化时 `publicendpoint.go:374` 打「端点已变化」变体） |
| E4 | `dns 代答就绪：tunnel=%v:53（UDP+TCP）resolve=%v:%d（TCP）upstream=%s` | `internal/server/serve.go:310` | `dns 代答就绪：tunnel=100.64.255.1:53（UDP+TCP）resolve=100.64.255.1:5300（TCP）upstream=198.18…` |
| E5 | `intercept: 过境拦截就绪（隧道IP %v；DNS 代答腿 :53/:5300；TCP 并发上限 %d）`（**M5 改**：豁免面退役 ⇒ 原「豁免=转投本机同端口」半句删除） | `pkg/intercept/intercept.go:187` | `intercept: 过境拦截就绪（隧道IP 100.64.255.1；DNS 代答腿 :53/:5300；TCP 并发上限 1024）` |
| E6 | `peer 表：设备表就绪（cap=%d，ttl=%v，grace=%v；按 devTag 记账/刷新/轮换）`（debug 级） | `internal/server/serve.go:394` | `peer 表：设备表就绪（cap=32，ttl=168h0m0s，grace=10m0s；按 devTag 记账/刷新/轮换）` |
| E7 | `peer: + dev=%s pub=%s ip=%v n=%d/%d` | `pkg/servercore/peers.go:445` | `peer: + dev=37a8115c pub=d5cae8cf ip=100.64.56.143 n=1/32` |
| E8 | `peer: ~ dev=%s refresh (idle=%s) n=%d/%d`（重连刷新）/ `peer: ~ dev=%s rotate pub=%s→%s ip=%v→%v …`（换钥轮换） | `pkg/servercore/peers.go:387` / `:415` | refresh 采样例：`peer: ~ dev=37a8115c refresh (idle=0s) n=1/32` |
| E9 | `peer: - dev=%s reason=ttl (idle=%s) …` / `reason=stale` / `peer: ! reject reason=revoked|no-token|verify` | `pkg/servercore/peers.go:466` / `:509` / `:369-373` | **R2 已采（ttl 形态）**：`--peer-ttl 15s` 注入 + 客户端静默后 GC 10min 周期回收，实采行 `peer: - dev=6280f0b6 reason=ttl (idle=7m47s) n=1/32`、`peer: - dev=3e16ea16 reason=ttl (idle=2m53s) n=0/32`（2026-10-02 17:31）；stale/revoked 形态归 R3（表满/吊销注入） |
| E10 | `intercept: tcp transit %v ← %v（dialok）`（**M5 收窄**：kind 位 = **固定字面 `transit`**，原值域 `{transit,exempt}` 的 `exempt` 随豁免面退役；**真凭据判据**） | `pkg/intercept/intercept.go:378` | transit 形态 **R1 已采**（2026-10-02，Rust 客户端 `--dial 192.168.3.12:9999`）：`intercept: tcp transit 192.168.3.12:9999 ← 100.64.213.172:34321（dialok）` |
| E11 | `intercept: tcp transit %v ← %v 关闭`（**M5 收窄**：同 E10，kind 固定 `transit`） | `pkg/intercept/intercept.go:389` | `intercept: tcp transit 192.168.3.12:9999 ← 100.64.213.172:34321 关闭` |
| E12 | `udp intercept: 会话 #%d %s 建立（%v ← %v）` / `… 关闭（%v ← %v）`（**M5 收窄**：`%s ∈ {transit, dns}`——`exempt` 随豁免面退役） | `pkg/intercept/intercept.go:560` / `:641` | `udp intercept: 会话 #1 dns 建立（8.8.8.8:53 ← 100.64.132.135:46440）`（Rust 出口实采） |
| E13 | `speedtest: 会话 #%d role=%s warmup=%s window=%s`（受理）/ `… role=recv bytes=%d（含预热 %d）用时=%dms` / `… role=send bytes=%d（含预热 %d）用时=%dms`（结算） | `pkg/speedtest/speedtest.go:209` / `:238` / `:294` | `speedtest: 会话 #8 role=send bytes=203355105（含预热 45677895）用时=10183ms` |
| E14 | `files 就绪：root=%s (rw) sock=%s（STREAM tag=1 经服务入口转投）`（**M5 改**：服务面走 QUIC `STREAM[tag]`，出口侧不再于隧道 IP 上监听/转投） | `internal/server/serve.go:467` | `files 就绪：root=/Users/zhaozhe (rw) sock=/tmp/homeway-rs-rustexit-1/serve/files.sock（STREAM tag=1 经服务入口转投）` |
| E15 | `term: 检测规则已加载 %d 份（覆盖目录 %s）` | `pkg/term/service.go:378` | `term: 检测规则已加载 22 份（覆盖目录 /tmp/homeway-rs-exit-1/agent-detection）` |
| E16 | `# Serving terminal sessions on sock=%s (shell=%s, history=%s, features=%s, vt=%s)` | `internal/server/serve.go:508` | `# Serving terminal sessions on sock=/tmp/homeway-rs-exit-1/term.sock (shell=/bin/zsh, history=1…` |
| E17 | `speedtest 就绪：sock=%s（STREAM tag=3 经服务入口转投；内存收发不落盘）`（**M5 改**：同 E14） | `internal/server/serve.go:537` | `speedtest 就绪：sock=/tmp/homeway-rs-rustexit-1/serve/speedtest.sock（STREAM tag=3 经服务入口转投；内存收发不落盘）` |
| E18 | `凭证台账：%d 行记录 / %d 枚在用凭证（其中 %d 行已吊销；吊销即时对新注册生效）`（吊销/续期语义判据） | `internal/server/serve.go:239` | `凭证台账：1 行记录 / 1 枚在用凭证（其中 0 行已吊销；吊销即时对新注册生效）` |
| E19 | `后端身份：标签 %x ｜公钥 %x…` | `internal/server/role.go:97` | （烟囱日志在档） |
| E20 | `公网端点：已按 **--public-endpoint 配置**公布 %v（跳过 UPnP/STUN 推断；写进 %s）`（显式端点路径；同族另有 STUN/UPnP 推断各形态行 `publicendpoint.go:187-218`） | `internal/server/publicendpoint.go:129` | `公网端点：已按 **--public-endpoint 配置**公布 [127.0.0.1:42642]（跳过 UPnP/STUN 推断；写进 public_endpoint.txt）` |
| E21 | `绑卡：自动挑到 %s（%s）` / `绑卡看护：网卡 %s 探针失败…` / `…连续探不通，重新挑卡` / `…QUIC 端口 socket 钉在 %s（%s）`（**M5 改**：末者原为「WG socket 钉在」——M5 后该 socket 是 QUIC 公共端口） | `internal/server/serve.go:263` / `internal/server/bindwatch.go:148` / `:152` / `:156` | `绑卡：自动挑到 en0（index=6 up addrs=[192.168.3.12/24]）`；重钉成功形态 `绑卡看护：QUIC 端口 socket 钉在 en0（index=6 up addrs=[192.168.3.12/24]）` |
| E22 | `dns: q=%d qtcp=%d resp=%d filter=%d trunc=%d fallback=%d fail=%d drop=%d malformed=%d aaaa-mixed=%d fakeip=%d`（DNS 代答计数行，debug 级周期输出；与 E10 同族计数语义。**`fakeip=%d` = Q-J F3 追加**，见「判据变更记录」） | `pkg/dns/server.go:162`（`StatsLine`，判据行注记 `:159-161`） | **R1 已采**（2026-10-02，周期行）：`dns: q=0 qtcp=0 resp=1 filter=0 trunc=0 fallback=0 fail=0 drop=0 malformed=0 aaaa-mixed=0`（Q-J 前的历史形态，无 `fakeip=` 尾字段） |
| E23 | `入站新源：%v（%s，%d 字节）`（源学习/漫游跟随证据，debug 级；**M5 值域收窄**：`shape ∈ {STUN应答, 参照点探测, 畸形腿帧, 腿帧type=3, 腿帧type=N, 畸形容器/未知消息容忍, 非帧（首字节=0x..）, 非帧包}`——WG 载荷/注册/控制/批量（kind=0/1/2/4）形态退役。**后两形态 = E 棒代码门 M-8 补登**（`bind.rs` 的 `process_packet` 对「非腿帧包」按首字节打 `非帧（首字节=0x..）`／空包打 `非帧包`；`pubface.rs` 模块头同域——三处口径现已一致）） | `pkg/servercore/bind.go:799` | **R1 已采**：`入站新源：192.168.3.12:54242（容器数据，222 字节）`；Rust 出口实采：`入站新源：192.168.3.12:62535（参照点探测，213 字节）` |

| E25 | `出口收线（连接数 %d → 0，用时 %v）`（**M5 新增**；A10 的出口侧收线时点证据行——出口停止时 QUIC 面收工块打一次） | 本仓新增（无 Go 对应；`crates/homeway-quic/src/exit/mod.rs` 收工块） | **实测**（`tools/quic-island-e2e.sh 1`，2026-10-10）：`出口收线（连接数 2 → 0，用时 43.042µs）`（脚本断言 `exit.close_line=1`，缺席即 rc=1） |
| E-q6 | `出口 QUIC 面就绪（单承载；migration=%v，initial_mtu=%d）`（**M5 新增**；单承载语境的就绪行，打在 E-q1 之前一行） | 本仓新增（M1 additive 族延续；`crates/homeway-quic/src/exit/mod.rs`） | **实测**（本地出口 stdout.log，2026-10-10）：`出口 QUIC 面就绪（单承载；migration=true，initial_mtu=1400）` |

## 客户端侧（core / 服务会话）

| # | 判据行（模板） | 出处 | 真实样例 |
|---|---|---|---|
| C1 | `身份：新建（dev=%s pub=%s，目录 %s）` / `身份：复用（dev=%s pub=%s）` | `clientcore/hostsession/session.go:197` / `:199` | `服务会话: 身份：新建（dev=37a8115c pub=d5cae8cf，目录 /tmp/homeway-rs-client-1/client/identity）` |
| C2 | `隧道侧就绪（L3 直通；隧道地址 %v，后端隧道 IP %v）`（**M5 改**：`wgcore: ` 前缀删除（批量条目）+ 末半句「核心自连经 WG 拨隧道 IP」随 WG 面删除；**「两串都合法」的 M1 例外条款同批作废**） | `clientcore/internal/wgcore/core.go:143` | `隧道侧就绪（L3 直通；隧道地址 100.64.56.143，后端隧道 IP 100.64.255.1）` |
| C3 | `新栈会话已建立（token 端点 %d 个，后端隧道地址 %v）` | `clientcore/hostsession/session.go:237` | `新栈会话已建立（token 端点 2 个，后端隧道地址 100.64.56.143）` |
| ~~C4~~ | **已删除（M5）**：原 `MIRROR 镜像包#%d → %d 候选（…）` —— 消费者 = `wtransport::bind`（WG 赛跑）整件退役；岛侧 `赛跑投出 …`（原 C4'）**去前缀后升为主行**（见「M5 批」L-6/C20） | `clientcore/internal/wtransport/bind.go:697` | （历史样例保留：`MIRROR 镜像包#1 → 2 候选（直连优先：本次直连 2 / 中继 0；本行每轮限 3 条）`） |
| ~~C5~~ | **已删除（M5）**：同上（原 WG 赛跑结算）；岛侧 `赛跑结算：胜出 %s %v（候选 %d 个，耗时 %v）；完成=…；未完成=…`（原 C5'）去前缀后升为主行。**数值语义**：输入集 = 岛赛跑（见「计数输入集」表 M5 批） | `clientcore/internal/wtransport/bind.go:538` | （历史样例保留：`赛跑结算：胜出 直连 127.0.0.1:42641（镜像 2 包，耗时 5.025s）；响应过=…；未响应=…`） |
| ~~C6~~ | **已删除（M5）**：同上；岛侧 `路径确立：%s %v（首个完成握手）`（原 C6'）去前缀后升为主行 | `clientcore/internal/wtransport/bind.go:550` | （历史样例保留：`路径确立：直连 127.0.0.1:42641（首个回包来源）`） |
| C7 | `暖机就绪（出口可达，rtt=%dms）`（服务会话形态） | `clientcore/hostsession/service.go:405` | `暖机就绪（出口可达，rtt=5029ms）` |
| C8 | `warmup pong: 就绪（判据=quic）`（**M5 收窄**：`%s` 值域 `{wg,quic}` → `{quic}`；`判据=wg` 形态随 WG 面退役；**登记 = 本文件「判据变更记录」登记表 2026-10-10（M7 补登）条**） | `clientcore/cmd/clientcore/tunmode.go:751` | `warmup pong: 就绪（判据=quic）` |
| C9 | `attached（数据面已接管 fd=%d，L3 直通）`（**APP 核形态**数据面判据） | `clientcore/cmd/clientcore/tunmode.go:822` | （需 TUN fd，R7 补采） |
| C10 | `link: via=%s ep=%s rtt=%dms（新栈状态快照）` / `link: via=%s ep=%s rtt=%dms（服务会话巡检）` | `clientcore/cmd/clientcore/tunmode.go:982` / `clientcore/hostsession/service.go:640` | 巡检形态：`服务会话: link: via=direct ep=192.168.3.12:42641 rtt=1ms（服务会话巡检）`（**R2 前缀订正**：服务会话 logger = WithPrefix("服务会话: ")，全部 hostsession 族行带前缀——R1 样例的无前缀形态系当时采样口径遗漏）；快照形态需 APP 核（R7 补采） |
| ~~C11~~ | **已删除（M5，整族）**：原 `RECOVER R1 重握手（原因=%s）：补注册 + 丢会话（保采纳）` / `RECOVER R2 换源（原因=%s）：换本地 socket（保采纳）` / `RECOVER R3 重赛跑（原因=%s）：清采纳，学习缓存候选兜底` / `RECOVER 恢复于 %s（原因=%s，起跑=%s，耗时 %v）` / `RECOVER 走完 R1→R3 仍未恢复（…）—— 交上层升级` | `clientcore/hostsession/recover.go:181` / `:188` / `:198` / `:201` / `:206`（同族：零档位恢复 `:151/:155`、动作生效复探 `:151`、本地动作失败 `:223`、R1 补注册未发出 `:168`、丢会话失败 `:175`） | **R2 已采全族**（故障注入四档实测：出口重启 R1 命中 3.126s / poison-socket R2 命中 18.4s / 换端口 R3 命中 38.7s / 停机最坏 39.8s 走完——行样例见 docs/reviews/R2.md）；档位/节拍/时间窗真源 = `tier:docs/agents/connection-lifecycle.md` |
| C12 | `启动（无 TUN 服务会话）`（服务会话启动形态） | `clientcore/hostsession/service.go:246` | `服务会话: 启动（无 TUN 服务会话）` |
| C13 | `候选端点（%d 条）：%s`（**M5 改**：「标记·学习」面退役——端点学习缓存 + hint 整面退役（G-1/G-8 登记）；来源 = 岛候选（token 端点 + 域名端点）） | `clientcore/hostsession/session.go:249` | `候选端点（1 条）：192.168.3.12:42652（直连）` |
| C14 | `出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v`（参照点探测判据；失败形态 `session.go:260`） | `clientcore/hostsession/session.go:280` | `出口能力：构建 homewayd-dev ｜ 默认路径 UDP：DNS:53 可用 / 通用（非 53）可用 / 实测 还没实测样本（这台出口还没转发过 UDP） ｜ 探测往返 0s` |
| C15 | `注册刷新 → %v（dev=%s，中继=%v）`（**M5 改**：`RREG ` 语汇随 WG 面退役 + `quic: ` 前缀删除；节拍沿用 60s 巡检） | `clientcore/internal/wtransport/bind.go:933` | `注册刷新 → 192.168.3.12:42652（dev=9b8bf127，中继=false）`（失败形态 `:930`） |
| C16 | `就绪（会话在位%s）`（服务会话形态核心**就绪**判据；`local-exit.sh client-add` 的等待点） | `clientcore/hostsession/service.go:423` | `服务会话: 就绪（会话在位，无桥直通）` |
| C17 | `已收工（state=%s）`（会话收工） | `clientcore/hostsession/service.go:856` | **R1 已采**（Go 客户端 client-stop）：`服务会话: 已收工（state=idle）`（2026-10-02） |

| C20 | **岛侧行族（M1 additive 组；M5 去前缀后升为主行）逐行列名**：`端点就绪（本地 %v，MTU %d，max_datagram_size=%d）` / `赛跑投出 %d 个候选（直连 %d / 中继 %d；本行每轮限 3 条）`（原 C4'）/ `赛跑结算：胜出 %s %v（候选 %d 个，耗时 %v）；完成=…；未完成=…`（原 C5'）/ `路径确立：%s %v（首个完成握手）`（原 C6'）/ `岛已建连（候选 %d 个，胜出 %s %v，耗时 %dms）—— L3 承载 = 岛` / `注册刷新 → %v（dev=%s，中继=%v）`（原 C15'）/ `迁移完成（%v → %v，耗时 %v）` / `迁移未确认（%v → %v，%v 内无对端回包 ⇒ 回落重连/重赛跑）` / `忽略非 kind=5 腿帧（…）`（**行文改写**：腿帧只可能是 kind=5——单承载后语义变了） / `窄路径不可用 —— max_datagram_size=%dB < 内层 MTU=%dB，1280 内层包将全部被丢（丢 + 计数不静默；MTU 降级旋钮 = HOMEWAY_QUIC_MTU）` / `隧道面已附加（fd=%d, mtu=%d；数据面已接线：读线程 + 回程队列 %d 条 + 写线程；fd 所有权在扩展，岛从不 close）` / `到点 detach —— 老世代仍持 UDP 源端口 %v、连接 %d 条（可能继续对出口发包；M0 §8.1 残余）` / `岛收工（连接面随端点关闭）` / `连接已断 —— 交快探阶梯（M/R/B；§3.1）保世代重连，不再就地拆世代` / `替换旧连接（旧连接已 CONNECTION_CLOSE）` | 本仓新增（M1 S3-1/S2a/S2b 落地；M5 去前缀升主行） | 岛侧就绪/赛跑/数据面/生命周期观测面（M1 判据证据） |
| C21 | **装配 / 生命周期归因族（M1 additive 组；装配面 + 腿面五子族）逐行核**：`赛跑小结：无胜者（…）` / `赛跑未成（{e}）` / `候选清单已更新（{n} 条）` / `本地 socket 已换绑（{from} → {to}）` / `换绑失败（{from} → …；{e}）` / `岛内 panic（…）` / `岛线程 panic（…）` / `准入已发起（dev=%s，Hello %dB；等挑战/回执）` / `准入完成（dev=%s，耗时 %s）` / `准入失败（{why}；预算 {b}）—— 连接已显式关闭（不留悬挂）` / `拆连接（dev={dev} 已从设备表摘除/轮换）` / `路径变更（未登记连接）{from} → {now}` / `{EXIT_THREAD} 线程 panic —— 本世代 QUIC 面已死` / `出口 QUIC 面 panic（{msg}）—— 判不健康，线程退出（不复用该线程）` / `端点未起（三形态）` / `MTU 上限取值 {raw}（{from}）非法或越界…按缺省 {default} 走` / `腿（→ {remote}）发送句柄克隆失败…` / `腿上的 QUIC 报文无法投递（出口 QUIC 面不可用…）——已丢 {c} 个` / `出口 QUIC 面未在收工预算内退出 —— 已 detach`；**四条 WG 兜底措辞同批改写**（`岛附加失败 —— 尝试 WG 兜底` / `出口 QUIC 面线程已退出 —— 后续出站回落 WG 原样、入站停止` / `准入回执…——本世代回落 WG 承载` / `按承载分档`），其中 2 条属批量条目的 4 串例外 | 同上（M1 S6 补登 + M5 去前缀） | 装配/失败归因面（「不静默」硬口径）；改写面见批量条目①的 4 串例外 |
| N-e | `内层 MTU 上限 %d（来源=%s）`（**M5 新增**；世代装配一次——生效值 + 来源；来源 ∈ `HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap` / `缺省` / `缺省（非法值回退）`） | 本仓新增（`crates/homeway-core/src/facade/tun_exec.rs::resolve_mtu_cap`） | 单测实测（`mtu_cap_resolution_clamps_by_default_policy`，5 分支）：`内层 MTU 上限 1400（来源=缺省）` / `…1400（来源=tunConfig.quicMtuCap）` / `…1400（来源=缺省（非法值回退））` |
| N-f | `TUN 读面形态：%s（M6.5 逐段成本整改：阻塞读消「read+poll」系统调用对）`（`%s` ∈ `阻塞读（每包一次 read）` / `非阻塞读 + poll（回退形态）`；每世代装配一次）+ 回退行族 `TUN fd 读阻塞化失败（F_GETFL: %s）—— 保持非阻塞读 + poll 形态` / `TUN fd 读阻塞化失败（F_SETFL: %s）—— 保持非阻塞读 + poll 形态`（**M7 补登**；登记 = 本文件「判据变更记录」登记表 2026-10-10 条） | 本仓新增（`crates/homeway-quic/src/tun.rs`：读线程形态行 + `set_blocking_read`） | 本机实读（M6.5 起 `.so` 世代日志；`tools/quic-island-e2e.sh` 驱动）：`TUN 读面形态：阻塞读（每包一次 read）（…）` |
| N-g | `岛 socket 缓冲（SO_RCVBUF（下行接收）\|SO_SNDBUF（上行发送））：设定 %dB，读回 %dB` + 可选尾半句 `（setsockopt 失败——沿用内核缺省）`（每 socket 两行；设定值 = `SO_RCVBUF` 2 MiB / `SO_SNDBUF` 1 MiB）（**M7 补登**；登记 = 同上登记表 2026-10-10 条） | 本仓新增（`crates/homeway-quic/src/client/relay_sock.rs::ClientSock::open`） | 本机实读（`tools/quic-island-e2e.sh` 的岛侧世代日志）：`岛 socket 缓冲（SO_RCVBUF（下行接收））：设定 2097152B，读回 2097152B` |

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
| R1 | `中继就绪：[::]:42781（token 模式（中继 ID 89174bf28aa1）；分配回收 1m30s，注册腿过期 1m30s，每源限速 200 pps，每后端最多 32 条分配，腿总数上限 256）`（**M5 B 棒 Q2 改**：`%s` 形态由单栈 v4（`127.0.0.1:`/`0.0.0.0:`）→ **双栈 `[::]:`**（`listen_with_fallback` 改走 `udpbatch::bind_dual_stack`）；v4 映射地址仍可连。登记 = L-9） |
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
| X1 | exit 侧（`--relay`）：`中继：注册腿开跑（中继 127.0.0.1:42781，token 模式（rl1 凭据））` / `中继：注册成功（腿 42645 → 127.0.0.1:42781）—— 客户端可经它到达本机` / `中继控制面：中继身份已认证（OK-MAC 通过）` / `中继控制面已连（127.0.0.1:42781）—— 已清腿表，等待会话重放` / `中继控制面：会话 #1 已拨腿 → 127.0.0.1:62614（认证腿）` / `中继控制面：会话 #1 已拆腿（RELEASE）` |（**M5 S4 删一条**：原第 7 条 `中继：收到对端地址线索 %v → 盲打 %d 包（开自己 NAT 过滤；能否直连仍由 WG 握手决定）` 随 WG 打洞面退役整条删除——`LegEvent::Hint` 自 S3b 起零构造点 = 不可达代码；登记 = L-14。**其余六条零改动**）
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
  **⚠️【Q-K 批，2026-10-08 收口】本条已注销**——见下条 Q-K 条目：出口侧已实装**有界重组**，
  该「接受的差异」转为「已重组交付」。
- **【Q-K 批，2026-10-08】IPv4 分片处置（F1/F4/F5-a~d，**取代**上条 Q-B F7 的「接受的差异」）**：
  ① 拦截层在建会话之前对入站 IPv4 分片做**有界重组**（30s 超时〔按 `created` 计、**不续期**〕/
  64 上下文 / 每源 4 / 64 片 / 4 MiB / 单报文 ≤65535 字节），重组成功的报文走**与正常包完全
  相同**的 `route_plain` 路径（结构上无分片旁路；**分片自身不建会话**——Q-B F7 的性质保持，
  `route_plain` 内的原 F7 分支改 `debug_assert` 绊线）。② 非法（偏移越界 / 非末片非 8 倍 /
  空片）、部分重叠、**内含重叠**、同区间不同字节（冲突）、越过末片边界 ⇒ **整条丢弃**
  （末尾两项比 gVisor 严、与 Linux 一致——**有意更严**）。③ 同区间**同字节**重复 ⇒ 静默忽略
  （不计数，与 Go 一致）。④ 超时且**首片在位** ⇒ 回 ICMP type 11 code 1（载荷 8B =
  原 IP 头 + 前 8 字节，**与 gVisor 的 RFC 1812 ≤548B 有意不同**）；抑制集 = 源 `0.0.0.0` /
  目的组播 / `255.255.255.255`。⑤ **出口 TX 侧反重写新增分片感知**（F5-d）：非首片只改 IP 源 +
  IP 校验和；首片用 **RFC 1624 增量更新** L4 校验和（UDP 原值 0 保持 0）；TX 分片表
  `(dst, ident, proto)` 末片精确回收 + 10s TTL + 上限 64，**表未命中 ⇒ 丢片 + 计 `txFragDrop`**
  （不发坏片）。⑥ 客户端侧：`fragmentation-buffer-size-65536`（恢复 Go 能力——修前 >1472
  载荷在客户端 TX **静默丢**）+ `reassembly-buffer-count-8`（并发重组槽 8）+ `udp_send` 对
  `> 65507` 载荷回 `ConnErr::DatagramTooLarge`（`(1253, 65507]` 不报错——那是正常可发）。
  **负面事实（必须知悉）**：本仓 Rust 客户端**看不到**上述 ICMP（smoltcp 的 `process_icmpv4`
  只处理 Echo、其余丢弃；客户端核无 `icmp` socket）——该 ICMP 的价值是**出口侧可观测**
  （tcpdump/日志）+ 与历史 Go 行为同形，**不是**「客户端大 UDP 失败现在可观测了」。
  **前提假设**：片数上限 64 以「对端 IP MTU = 1280」为前提（合法最坏 53 片）——异种 MTU
  对端（如 576 ⇒ 123 片）会被 `fragLimit` 拒（**有意更严**，行文带片数维度可观测）。
  **残余**：客户端入站重组超时仍是 smoltcp 的 60s（F6，不改）；内存压力淘汰**无低水位滞回**
  （逐条淘汰，gVisor/Linux 是 4MiB→3MiB 滞回）；ICMP 只在**超时**发（内存压力淘汰不发）。
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
- **【Q-L 批，2026-10-08】备用屏 47/1047 的四条登记差异（L2）**：① **重入清空**——alacritty 的
  `swap_alt` 在**进入时**复位 alt 内容（`term/mod.rs:723` `reset_region`）⇒ `47h→47l→47h` 时
  ghostty **保留旧内容**、本实现**清空**（ghostty 的 47 从不 saveCursor/清屏）；② **1047 退出清屏的
  等价性依赖**——ghostty 在 1047 退出前于 alt 上 `eraseDisplay(complete)`，本实现不显式清（改由
  「下次进入复位」等价覆盖可观察面）——**该等价性依赖差异 ① 的「进入即复位」**（若上游复位行为变化
  则等价失效）；ghostty 的 ED2 另清 kitty 图像，本仓无对应物。③ **混合形态终点**（`1049h → 47l → 1049l`，
  代码门 低3 登记）——47l 现在真切回主屏后，随后的 `1049l` 落到既有 D-15「未配对 1049l」分支
  （SGR 复位 + DECOM 清 + `goto(0,0)`），而 ghostty 是 `restoreCursor()`（恢复 1049h 保存的槽）；
  同时 `dec_modes[1049]` 仍记「set」而屏已 primary（DECRQM `?1049$p` 与屏态不一致）。触发条件矛盾
  （用 1049 进、用 47 出）且软件极少如此，**登记不改**（低危）。④ **grow 位移丢失**（代码门复核轮 中，
  2026-10-08）——alacritty 的 `grow_lines` 会把 `saved_cursor.line += from_history`（内容锚定，把槽
  随回滚拉入视口）；本实现的捕获是 resize **前**的克隆，只做上界钳制 ⇒ 「`47h` + 放大跨回滚 +
  `47l` + `ESC 8`」的槽位置比 ghostty/HEAD 少位移（实测对照：HEAD/grow 路径 `(2,7)` vs 本实现
  `(2,1)`；不 panic、位置仍在界内）。**登记不改**（补偿 = 镜像 alacritty 的 grow/shrink 位移规则，
  需跟踪主屏 history 跨 resize 的增减，风险 > 收益；列为 term 面候选，若将来有实测靶点再开）。
  **1049 路径本身不受影响**（保存/恢复语义由 alacritty `Named` 分支覆盖，本批未改动；session-styles
  golden 与 `vt_alt1049_path_unaffected_by_l2` 双钉）。**光标面**：进出两方向都拷**整个** `Cursor`
  （含 SGR 样式；ghostty `cursorCopy` 除 hyperlink）；主屏 DECSC 槽由 `alt47_saved_decsc` 进入前捕获、
  退出后还原（alacritty `swap_alt` 进入时会覆盖它——那是 1049 的实现痕迹）。**还原前按当前尺寸钳制**
  （代码门 高1 整改）：捕获是跨 resize 的克隆，而 alacritty 的 `Grid::resize` 只钳制 grid 内的
  `cursor`/`saved_cursor` ⇒ 缩小后直接写回会让后续 `ESC 8`（`restore_cursor_position` → `damage_cursor`）
  越界 panic（会话被终结）；ghostty `restoreCursor` 同为钳制口径（`Terminal.zig:2045-2048`）。
  **混合形态的 1049 退出还原**：若曾以 47h 进入、未走 47l 就经 1049 退出，捕获在该出口还原
  （alacritty 的 1049 退出只换屏、不读槽）；按不变量（捕获存在 ⇔ 处于 alt）正常 1049 会话捕获为
  `None` ⇒ 零影响（钉子测试 `vt_alt47_capture_not_stale_across_1049`）。

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
| CA5 | socks 域名目标（远程解析腿）：`curl --socks5-hostname 127.0.0.1:42702 http://example.com/` → `HTTP 200`（DNS-over-TCP → **`STREAM[tag=6]`（dns-resolve）→ 出口代答** → 应答候选按序拨）；出口 `dns: … qtcp=N` 计数增长。**⚠️ 2026-10-12（DNS-STREAM 承接批）**：M5 的降级注（2026-10-10「宿主会话无 tag」）**已作废**——用户裁「承接」并落地 `STREAM[tag=6]`，本行**恢复可验收**；机制面从「隧道 IP:5300 经拦截层回环豁免」改述为「QUIC 服务流经出口服务入口转投 + `dnsproxy` 代答」（豁免面随 M5 退役，不再复活）；登记 = 「判据变更记录」2026-10-12 条 |
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

- **【M5 批，2026-10-10】token 载体换代（`hmw1` → `hmw2` 段容器）**：①**存量 `hmw1` token 一律失效**
  （解析报 `homeway/token: 不支持的 token 版本: hmw1`——可行动文案；无兼容包袱口径，用户 2026-10-09 常设）；
  ②`serve token` / `E3` 行文**零改**（只有串内容变 `hmw2…`）；③`fixtures/vectors/token.json`（Go 冻结向量）
  **退役为历史参照**，新向量 = **本仓自产** `fixtures/vectors/token_hmw2.json`（生成器 =
  `crates/homeway-core/tests/token_vectors.rs::bless_token_hmw2_vectors`，`#[ignore]`；常规用例断言
  「编码器 reproduces 入库字节 + 解析逐字段 + 哨兵码」）；④**中继 `rl1` wire 零变化**（rl1 body 冻结在
  v1 布局：`token::encode_v1_body`/`parse_v1_body` = 冻结单源）；**红线记账的准确写法**（E 棒代码门 H-3 勘误：
  原写「`relay/**` 零改动」与事实相反——`relay/rltoken.rs` 有 **+18/−13 行**的**调用面换源**：encode/decode
  改走 `encode_v1_body`/`decode_v1_body` + 自带 base64 引擎，删一条恒不可达分支）：**`relay/**` 的实际改动面 =
  `rltoken.rs` 一个文件的调用面**；`relaywire.rs` 与 `relay/` 其余文件**零改动**；**中继 wire 逐字节不变**
  （代码门独立用旧算法重实现比对两形态 hex 相等）；⑤tier App 零代码（只透传）。
- **【M5 批，2026-10-10】E1 的 `files/term/speedtest` 三字段口径注记**：三数（7802/7724/7803）**不是**出口侧
  监听端口——M5 后出口**不再**于隧道 IP 上监听这三端口（豁免面退役，见 L-3/L-4）。它们 = **客户端
  「端口 → STREAM tag」映射的输入**（`quic_stream::tag_for_port`），保留原值使映射可见；出口侧服务入口 =
  UDS（调试面）+ QUIC `STREAM[tag]`。
- **【M5 批，2026-10-10】`quic: ` 前缀批量删除（L-6）的判据面影响**：所有带前缀行的**正文逐字不变**
  （4 条已登记串例外见批量条目①）；**排障脚本凡 `grep 'quic: '` 者已同批改**（`tools/quic-{island,wg}-e2e.sh`
  + 10 个 matrix/perf/ab 脚本的抽取式）；`tunStatusJSON` 的 `quic{…}` **段名保留**（结构键 ≠ 日志前缀）。
- **【M5 批，2026-10-10】`transport.localErr*` 两键恒缺席（观测面收窄）**：该两键源 = WG bind 的全候选发送
  统计（随 WG 面退役）；岛无该面 ⇒ `demand.localErr*` **恒缺席**（additive 兼容：旧 App 读到 `None`/缺键）；
  `transport` 段其余键与 `quic` 段键面**零改动**（`fixtures/vectors/tun_status.jsonl` 同样零改——它的可达
  形态是阶段机向量，不含这两段）。
- **【M5 批，2026-10-10】测试里的「负向断言」与门的关系（口径）**：单承载后若测试要证明「**不得**出现
  `回落 WG`/`A/B 开关`/`按承载分档`」这类串，它**必须**在断言里写出该串——`tools/check-wg-removed.sh`
  的 ③ 条按「断言上下文」白名单放行（显式 4 文件清单 + `assert|contains|必须已删除|不得|banned` 行级判定），
  **产品行照旧击红**。
- **【M5 批，2026-10-10】历史文档不追改**：`docs/reviews/M1–M4.md`、`docs/PERF-AB.md`、
  `docs/E2E-APP-RUST-*.md`、`docs/DEPLOY-RUST-EXIT.md`、`docs/DEVICE-TEST-OHOS.md` 里的旧判据串
  （`quic: ` 前缀 / `hmw1` / `tcp exempt` / `wg=:`）**按历史原样保留**（登记面 = 本表；验收一律以本表为准）；
  `docs/QUIC-ROADMAP.md` 的门槛表体积行/CPU 相对列由**主会话**同批改（设计 §8.5-L-1 的指定触点位）。
  **【M7 批，2026-10-10】例外（局部划出，见登记表 2026-10-10「DEVICE-TEST-OHOS 现行口径节」条）**：
  `docs/DEVICE-TEST-OHOS.md` **新增 §8「现行口径（M7 起）」**并在 §3/§5 各加一行就地警示
  （「本节为 M1/M5 历史实录，现行口径见 §8」）+ §1 的 worktree 路径改 main 主检出——
  这三处属**现行口径面**（操作手册类），**历史正文（§2–§7 的实录行）仍不追改**；
  `docs/DEPLOY-RUST-EXIT.md` 同理：§0–§11 不动，**新增 §12**（v0.3.0 切换手册/实录位）为现行口径面。

## 判据变更记录（政策 + 登记表）

### 2026-10-28：PTY 强杀与收尸期限

- 从「宽限内轮询后调用组 SIGKILL、以 portable-pty 的 cloned killer 作单进程回退，再无期限 wait」改为「控制面锁内确认 `Child::try_wait() == Ok(None)` 后发组 SIGKILL 与真实单进程 SIGKILL；原宽限后最多再轮询 2 秒，返回真实退出状态或类型化收尸错误」。cloned killer 在 Unix 仅发 SIGHUP，不再把它当作 SIGKILL 回退。IO/ECHILD 不视作仍活进程，非正 PID 拒绝发信号；已收尸 PID 不再补发信号。
- 新增预算真源为 `term/pty.rs::FORCE_REAP_WAIT=2s`；原调用方宽限、`KILL_GRACE=500ms` 与测试硬期限 60 秒不变。争用控制面锁的强杀须等待当前有界收尸轮释放锁，不再锁外向可能复用的组 PID 发信号。业务收尸不调用无期限 `Child::wait`，不创建脱离的 wait 线程。
- 未取得真实退出状态时保留 Child 和运行态；正常 EOF、pump panic 与原 spawn 失败清理拥有者在既有线程内释放 core 锁后每 20ms 重试有界收尸，诊断首轮及每 16 轮节流。服务关停一次有界尝试失败会记「收尸未完成……保留运行态，由既有读泵继续回收」；只有真实收尸后才移除注册表，最后写者不能提前释放历史。D-19 的信号死 `-1`、KILL 的 `-2` 与既有 ENDED 词面/帧格式不变。
- 依据为本轮私有 workspace 测试采样：`attach_size_applies_to_pty` 已通过尺寸断言，在 `svc.close → stop_and_wait → wait_bounded → wait → wait4` 挂起，pump 等待同一 core 锁；该次没有取得 shell PGID 或信号返回码，故不把组信号失效写成该轮已实测事实。新增自有 HUP 忽略且 PGID 与 PID 不同的进程回归验证单 PID 升级，另以 pending Child 验证强杀后期限错误与句柄保留；既有收尾/信号码回归继续适用。无 wire/NAPI 变更。

### 2026-10-28：默认 shell 的真实工作目录

- LIST 可选 `cwd` 从「仅有服务端 VT 且收到有效 OSC 7 才返回」改为「有效 OSC 7 优先，否则使用当前会话 PTY shell PID 最近成功采样的真实 OS 工作目录」。覆盖初始默认 shell 以及普通 `cd`，不要求用户安装 shell hook；raw-only 无 VT 会话也能返回该真实目录。没有保存过的 launch cwd hint，不把服务进程的目录、标题、命令行或固定 HOME 猜成当前目录。
- macOS 使用已有 libc 的 `proc_pidinfo(PROC_PIDVNODEPATHINFO)` 中 `pvi_cdir.vip_path`，验证完整结构长度与有界 NUL；Linux 使用 `/proc/<pid>/cwd`。保留 OS 返回的物理路径（含合法非 UTF-8 路径的展示转换），不注入 OSC 7、不发 `pwd` 命令、不新增依赖。其它平台原生查询不可用时保留原 OSC 7 路径。
- 复用既有 `SAMPLE_PERIOD=1s` 线程，先取 `(name, session generation, PTY PID)` 快照、锁外查询 OS，再在锁内核验同名同代同 PID 且存活后更新缓存。元数据采样不受 `HOMEWAY_TERM_DETECT` 开关影响；agent 检测仍遵守该开关，状态检测冻结不冻结目录元数据。LIST 只读缓存，不做 OS I/O；查询失败保留本代上次成功的真实目录，不把缺测伪装成新目录。初始目录在首个成功采样后出现，再由客户端原有刷新读取。
- 影响面为已有 LIST `cwd` 值及展示：raw-only 的旧恒空行为在本条明确变更；可选字段名/类型、JSON 声明顺序、帧封装与版本、surface/agent 枚举和网络准入不变，固定 wire 夹具无新字节。真实默认 shell 的初始目录/带空格 `cd`、检测关闭、raw-only 与 OSC 7 优先验证使用私有实例；本条是源码判据，不授权重启现役服务。

### 2026-10-27 TUI 权威 surface 与观察接入

- **显式可协商扩展**：GREETING features 新增 `OBSERVER=1<<7`（全开位集从 `0x7f`→`0xff`），HELLO flags 新增 `OBSERVER=1<<3`，仅允许 SURFACE 且不得同时 TAKEOVER。协议版本、帧格式、已有能力位不变。旧 Rust 与 App 只按所需位掩码判断，忽略新 feature；新 TUI 遇缺能力的旧出口明确要求升级，在发送 HELLO 前退出，禁止 raw 降级造成隐式抢占。同步 greeting golden、MANIFEST/SHA256SUMS 与派生 fuzz 种子摘要。
- **活动与尺寸**：从所有接入/RESIZE 均成为活动腿，增加显式 observer 的例外：接入只观察，未活动 RESIZE 只记该腿偏好；活动 observer 的真实窗口变化照常应用。首次真实按键 press/repeat、非空文本/粘贴、鼠标按钮按下（含应用上行滚轮）沿原活动策略接管，先应用该腿尺寸再用共享 VT 模式编码 INPUT。focus、keyup、motion、纯终端应答与 THEME 不选举；非活动 observer 的非粘贴纯应答 Text 与 DATA 均丢弃，避免污染活动程序；本地历史滚轮由客户端处理，不发 INPUT。未使用 observer 在活动腿退出时也不自动接管。已摘腿在途输入在记活动前拒绝，不得更改 owner/几何或活动序号；其余未置 flag 的 CLI/App 正常路径保持原行为。
- **权威内容与历史**：TUI 从 raw 回放后重新解析 ANSI 改为消费已有 SURFACE SNAPSHOT/DIFF/FETCH-ROWS 的网格、真实 cols/rows、revision/seq/history epoch，面板只裁剪而不重新折行；STATE 不增加尺寸。观察接入的 ATTACHED 尺寸取当前共享 VT，首个完整快照才可交互，客户端不向 PTY 回答历史查询。LIST 的既有 pid 用于本机真实祖先链识别承载当前 TUI 的会话，远端同名或同号 PID 不据此禁选；不新增身份协议字段。
- 原因：窄面板查看另一客户端会话不应改变它的 PTY 几何，回滚应包含接入前服务端历史；防止远程 shell 内 TUI 自嵌套到承载自身的终端。影响新协商 observer、TUI 及已摘腿在途活动拒绝；有效旧默认接入/应答路由及已有 surface 编码保持不变。验证与评审见 `docs/reviews/2026-10-27-tui-usability.md`。

### 2026-10-27 TUI 与统一进程本地终端

- **raw接入输入模式（评审r2高1）**：从「ATTACHED仅含8个legacy位，raw回放只恢复屏幕，启用序列若已离环则新客户端丢失编码模式」改为「同一锁内捕获replay_end与服务端VT输入模式检查点，回放完成、REPLAY_DONE之前用已有DATA帧补发标准ANSI输入模式恢复序列」。覆盖DECCKM/DECKPAM/DECBKM/1035/1036、bracketed paste、focus、鼠标tracking/format（保留last-set权威值）、modifyOtherKeys与kitty当前flags；**不含切屏/清屏/光标/历史/尺寸指令**，不入输出环、不计replayed，live字节在后。原因：长会话重进不能把本应受保护的粘贴按裸文本执行。影响TUI与原raw CLI消费者，op/封装/版本/PTY实时输出原样不变；无服务端VT的raw-only保持原形。回归=`raw_truncated_replay_restores_input_modes_for_existing_raw_consumers`、`input_checkpoint_restores_encoding_without_touching_screen_or_cursor`。
- **预算入口（评审r3中1）**：新增仅显式调用的`IpcStream::connect_timeout`、`ControlClient::dial_with_timeout/request_deadline`供TUI贯穿连接/HELLO/WELCOME/写锁/帧写/应答总截止，旧`connect/dial/request`默认不变。AF_UNIX EAGAIN表示未排队，丢弃socket并在同截止内每至多2ms重建重试；EINPROGRESS以poll+SO_ERROR+peer_addr确认，不以未连接socket的SO_ERROR=0冒充成功。原因：满backlog时本机连接也不能无限阻塞刷新lane；影响新TUI deadline API及本地实际连接回退，不改线上帧/服务端节拍。

- 本地终端从「仅出口角色持有 `<state>/serve/term.sock`，停出口同时结束会话」改为「非 Windows 统一进程持有一份 TermService，0600 的 `<state>/local-term.sock` 恒开；出口运行时借用同一注册表提供已有 UDS/QUIC 入口」。`serve stop/restart` 撤出口网络入口并保留会话，本地与远端同名会话完全互见；统一进程收工才结束全部会话。前台单角色 `homeway serve` 仍独占并在出口停机时收工。`HOMEWAY_TERM=off` 仍禁用终端；不因建立本地会话打开网络角色。本地 UDS 装配失败发 `local-term: 本地终端监听失败` 告警后降级，其余角色继续运行，出口仍可按既有方式独占终端服务。
- `homeway term` 本地寻址从「term.sock / serve/term.sock 且出口 enabled」增加优先存活的 `local-term.sock`（无需出口启用），实际连接失败按local→flat→serve回退。TUI 不会自动重启旧守护，缺少本地服务时给出启动新版统一进程的提示。
- LIST JSON 增加可选 `agentLabel` 字符串，来自本拍前台进程命中的现有 manifest 名称，用于保留 pi 等固定枚举之外的身份。每拍独立更新，退出进程不沿用上次扫屏缓存；既有 `agent` 字段、`STATE/ATTACHED` 的 u8 枚举、帧封装与协议版本均不变（LIST体因可选字段而扩展）。旧消费者忽略新增字段，新 TUI 在缺少字段时回退既有 `agent`。
- TUI 控制面继续使用既有角色/host/SOCKS/term 操作与帧；刷新整轮/列表查询各3s、角色操作与终端接入/创建/关闭10s、添加主机20s预算对齐既有CLI，终端一次请求的open/GREETING/应答共享绝对截止。刷新和列表各用独立单槽队列，刷新不占修改worker，快照由mutation epoch在完成和消费两侧过滤；列表携带query epoch，连续切换只保留最新待查询目标。提交成功、后续取值/复制、刷新错误分别提示，不因后续失败抹掉已成功事实。修改命令队列16槽/事件32槽、attach输入64槽×16KiB和下行128槽×64KiB为本地界面资源策略，不改现有服务端节拍、恢复阶梯或 wire 判据。验证与两道门见 `docs/reviews/2026-10-27-tui.md`。本条登记源码行为，不代表发布或部署。

### 2026-10-22 出口质量历史（本地源码）

- 从「累计计数/分散日志，无法指定时间回看」新增为「认证设备路径 + 过境目标流按窗观测，异步有界历史 + `homeway serve quality` 本机查询」。默认每5秒采集，结束流队列4096、结束连接队列256、落盘通道4槽、磁盘4×8MiB；均有显式缺测口径。
- 新增故障判据前缀 `quality: 历史写入失败`、`quality: 观测线程创建失败`、`quality: 采样队列满或已关闭`、`quality: 历史收尾超时`、`quality: 观测线程异常退出`；正常收工落盘等待上限200ms。报告提供丢样本/坏行/半尾/覆盖/重启信息。既有 dialok、stats、peer 与连接恢复日志模板及语义不变。
- 发送端判丢只对应出口发送方向，连接首上游等待不是HTTP TTFB，UDP无回包不算请求失败；不把低吞吐当带宽瓶颈。目标TCP系统读数按平台能力出值，不支持保持不可用。
- 无wire/NAPI/词表、探测节拍或恢复策略变化；详见 `docs/QUALITY.md` 与 `docs/reviews/2026-10-22-quality-*.md`。源码验证不代表已发布/部署，也不补采升级前历史。

### 2026-10-22 raw 终端回放恢复主/备用屏状态

- 从「清屏后直接回放有限历史」改为「按回放起点的主/备用屏状态切屏、清屏，再原样回放」。
  输出环裁掉早期的 `47/1047/1049` 进入指令后，后续 attach/自动重连仍须进入备用屏，
  防止全屏 TUI 重绘到主屏而可滚动到启动前的内容。初始化使用回放起点状态，不能把
  `ATTACHED` 的回放末尾模式当起点，避免历史中的进出屏序列改变还原结果。
- 输出环保留裁剪边界的解析检查点；完整未截断历史不再无条件丢弃第一行，截断历史仍
  丢弃不完整首行。实时 DATA 保持字节透传；不按应用名称切屏，主屏/inline 程序保留历史。
- 模式扫描器忽略 DCS/SOS/PM/APC 字符串中的伪切屏指令，RIS 复位已记录模式；检查点
  携带跨块解析状态，不能把裁剪边界后的 CSI 尾部误当普通文本或遗漏切屏。
- 新增初始化字节只用于 raw 握手，不写入会话输出环；帧格式/协议版本、surface/NAPI 导出
  与既有日志模板均不变。两道门与验证见 `docs/reviews/2026-10-22-term-replay-screen.md`。
  此条登记源码行为，不代表已经发布或换装生产出口。

### 2026-10-22 终端断腿 SIGPIPE 隔离

- term 帧发送由裸 `libc::write` → 复用 `sysock::send` 的 MSG_NOSIGNAL 单次发送；
  保持 poll 绝对期限、短写进度、断尾续写及 Timeout/Hard 分派。对端退出只能终结该腿，
  不能因主函数的默认 SIGPIPE 处置杀死统一进程；CLI stdout 管道原有 SIGPIPE 语义不改。
- wire 帧/版本/错误码/常量与日志模板不变。新增默认 SIGPIPE 子进程关闭对端回归，
  私有 Linux 零参双角色实例执行立即断开压力与当前协议完整 PTY 冒烟。
- 根因、前红后绿及两道门见 `docs/reviews/2026-10-22-terminal-sigpipe.md`。

### 2026-10-21 终端仅保留当前版本

- 用户确认本批尚未发布，不保留旧APP/旧出口兼容。协议版本数值不变，GREETING必须
  恰5B且当前版本，各模式要求自身必需features（额外位允许，不强求raw具备surface）。
- HELLO由可选caps/版本/ID、宿主平台推断 → 必须capLen=1，SURFACE/RAW_TERMINAL恰一位，
  PROTO_VER声明及当前版本字节必有，KEY_ALT平台恰一位，ID为1..64B合法UTF-8。
  旧空/缺cap、多字节OR块、裸ID、缺版本/ID及歧义平台均bad_capability；完整结构错版仍
  term_version。删除原歧义“按未声明处理、不拒腿”告警与计数，不新增错误码。
- Legacy旧APP腿与JSON kind=legacy删除，仅App/Host；注册表持明确类型，不按缺位猜raw。
  LIST clients始终数组（含空数组）、stateV2始终存在；App/CLI端按当前必需字段严格消费。
- keyenc宿主默认helper删除，按客户端明确平台编码；ANSI/DEC标准终端模式不删。
  HOMEWAY_TERM_VT=off的合法raw-only故障路径保留，日志“仅legacy原始字节模式”改为
  “仅raw原始字节模式”；不存在的日志腿分类改detached，不伪装旧APP。
- core一次性LIST/KILL客户端不发HELLO；它要求当前GREETING与LIST|PROTO_VER，不再接受
  缺位旧出口。无应答诊断去“官方版/旧版tailcat、升级增强版tailcat”历史文案，归因码不变。
- 当前HELLO与surface夹具显式迁移，禁止以运行期/测试前偷偷补字段保留旧wire。影响面与
  两道门见 `docs/reviews/terminal-current-only-2026-10-21.md`；不发版/装机/部署。
- 删除旧Go样式摘要的空白宽度归零：样式空格仍占1列，cell2两端均要求skip当且仅当width=0，
  非skip仅1/2；当前夹具、样式/行首组合簇回归同步，不保留v4/cell1自动升级。

### 2026-10-21 终端 APP/CLI 整改

- surface 体 v4 / cell v1 → **v5 / v2**：公共头在 cols/rows 后新增
  `seq:u64/historyEpoch:u64/historyBase:u64`；行头增加 wraps:u8，完整格 symbol 后增加
  width:u8。FETCH 请求由 from/count → epoch/from/count（18B）。旧版 surface 客户端
  不可与新版服务混用，须协调升级；raw CLI 帧族不变。冻结 v4 夹具与 baseline 不改，
  新字节基准见 `fixtures/vectors/surface_v5.json` 与 MANIFEST。
- 历史身份：容量满后固定物理下标 → epoch 内单调绝对行号，historyBase 随真实裁剪推进；
  resize/reflow、ED3、RIS、DECCOLM 等破坏性变化换 epoch，备用屏使用独立 epoch。
  不按旧存活区猜测退出行，FETCH 仅从当前 VT 历史取材；epoch 失配要求新快照。
- 帧连续性：仅 revision 基线 → 成功入队的 SNAP/DIFF 才推进 seq，FETCH 不消耗 seq；
  安全淘汰完整待发 surface 组后用快照恢复，不将缺失差分视为连续。
- 响应归属：只要存在 raw 腿即禁用服务应答 → 仅 active 的纯 raw 腿拥有应答权；
  active 为 surface 时服务端应答，旁观 raw 的自动应答被丢弃。OSC52 load 同遵归属，
  surface focus-out、无按钮 motion、key release 不抢 active/尺寸。
- 同步输出：等待后续 PTY 字节才结束超时事务 → 宿主定时驱动 VTE 的 **150ms**
  同步更新超时，空闲也提交；150ms 为既有 VTE 常量，不另造第二套超时。
- 分片粘贴：每片直接写 PTY → **1MiB** 有界、单腿所有、末片一次提交；首片捕获 bracketed
  模式，**10s** 未完成、超限或断腿取消未提交数据，多腿输入不能交错进入事务。
- 下发预算：FETCH 按最多512行但材质无字节约束 → 同受 pending_cap/queue_bytes 预算；
  镜像按实际编码大小从近到远缩窗，单行过大缩至0。视口本身或单份压缩快照仍超预算时
  明确 `ERROR(surface_unavailable)` + ENDED，不无限重试。慢写不再放大全量；surface
  writer 使用 HOMEWAY_TERM_WRITE_TIMEOUT_MS 配置，与 raw 一致。
- 几何/解码材质：1000×500 单轴上限 → 保留列/行上限，新增 **128×1024 格**乘积硬门；
  HELLO 乘积超限明确 bad_hello、RESIZE 明确 bad_resize，不改当前会话尺寸。
  视口+快照镜像合计、单次 FETCH 展开分别最多128Ki格，防空白RLE字节虽小而解码膨胀；
  VT 组件 new/resize 同门，内部 Size 归一也保证乘积不变量。客户端缓存32MiB/10000行，
  可见窗口保留，不用缓存不住的大窗口触发淘汰/重取循环。
- APP上行预算按 `sizeof(PtyItem)+payload` 及重复键实际展开量记账（每次20B），而非只数
  payload；重复次数限1..1024，整批受理或整批拒绝，失败走type3本地toast，不伪装成功。
  本地错误合并最新值，App toast 至多1秒一次，避免背压下重复按键反过来挤爆UI反馈。
- 渲染语义：宽度靠客户端猜测、调色板动态变化不传 → 明确列宽/wraps，OSC颜色覆盖转换
  为 RGB 后随 damage 下发，复位恢复符号颜色；宿主主题不覆盖程序 OSC10/11 设置。
- 动态颜色的深历史材质：只刷新当前视口 → 实际OSC override变化或复位同时推进材质epoch，
  保留historyBase与绝对行号，令旧历史缓存失效。备用屏改共享调色板也使主屏历史epoch换代；
  重复同值设置/重复无override复位不换代。OSC110/111恢复宿主主题查询值与符号默认色，
  OSC104恢复符号palette；不因颜色变化重用或回退已裁剪的绝对行号。
- CLI：无界 channel/阻塞 stdout/无限排空 → 上下行均 **1MiB / 256帧** 有界；输出不丢
  raw 字节，满队列向上游背压；主循环每拍最多 **32帧/64KiB**、单次 stdout 写最多
  **16KiB**、poll **50ms**，非阻塞 stdout 保留部分写，输入/信号/退出可并行推进。
  取消先关队列再关流并 join，管道满不能丢退出；满 stdout/上行背压退出测试门槛 **2s**。
- 影响面：服务端、APP surface 解码/历史缓存/渲染、raw CLI 调度与专项测试；连接策略、
  token、QUIC 承载、终端外帧族与生产部署均不变。本批只改本地源码，不发版/推送/装机。

### 2026-10-21 终端无腿收尾回收

- 无活跃腿的首次 finish：误按空 ENDED 集提前返回、保留 PTY runtime →
  仍完成收尸与注册/运行态回收。幂等依据会话是否已结束；有写者时仍等待排空后回收。
- wire、ENDED码/发送顺序、信号宽限、连接/重试策略和判据行均不变。


### 2026-10-21 中继 TCP 快速重启

- Unix TCP 监听：旧连接主动关闭后 TIME_WAIT 可导致重绑失败、纯 UDP 降级 →
  SO_REUSEADDR 恢复同一监听地址/端口的立即重启。该标志不允许两个 live listener
  共享端口，不启用 SO_REUSEPORT；Windows 保持原路径。
- 既有就绪/降级判据行、监听范围、wire、token、配额与连接策略常量均不变。

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
| 2026-10-08（Q-I 尾段批落地） | **取值 flag 纪律的局部回退：空值 carve-out（`--stun`/`--stun6`/`--relay`/`--ddns`）**（对照 Q-H「取值 flag 纪律」行） | Q-H 口径「`--flag=`/`--flag ""` 空值 ⇒ fail-fast（唯一 carve-out `--recover-cause`）」→ **追加四 flag 的空值 carve-out**：`--stun`/`--stun6`（空 = 关公网观测）、`--relay`（空 = 关注册腿）、**`--ddns`（空 = 清空 config 全部条目；代码门 M1 扩面——`serve_cli.rs` 原本已有该分支但不可达）** 接受空值并按语义处理；**`--public-endpoint=` 仍拒**（Go 基线实测同拒：`homeway: --public-endpoint "" 非法（not an ip:port…）` rc=1）；其余取值 flag 纪律不变（缺值/吞 flag 仍 fail-fast） | **F0（Q-I 尾段）**：Q-H 泛化纪律把 Go 文档化「空 = 关」（`baseline/homeway/internal/server/cli.go:36-37` + `explicit["stun"]` 分支；`--ddns` 见 `:38/55/127-135` `explicit["ddns"]` ∧ 空 ⇒ nil）拒绝 ⇒ `tools/local-rust-exit.sh`/`perf-ab.sh`/`matrix.sh` 的 Rust 出口**全部起不来**（R5 起本地测量面全瘫）；`bin/homeway-go` 实跑四 flag 空值形态正常起服（Rust 侧属对 Go 的回归） | `cli_flags.rs`（新 `take_value_empty_ok_or_exit`）、`serve_cli.rs`（stun/stun6/relay/ddns 四站点 + 用法行）；单测 `empty_value_carveout_forms`/`empty_value_carveout_flags_accept_empty`；E2E 冒烟（`local-rust-exit.sh start 9` + Go 客户端「就绪（会话在位）」）；**已知残留（非 carve-out）**：`--bind-interface=` 空值 Go = auto（`ResolveBind("")`）而 Rust 拒——有等价写法（`--bind-interface auto`）⇒ 登记不扩；排障脚本若按「空值一律 exit 2」写断言须改。**后续（2026-10-08 Q-L L3）**：本条「已知残留」已消除——`--bind-interface=` 空值**已进 carve-out**（见本表 Q-L 行），四→五站点；本条其余部分（四 flag 空值语义）不变 |
| 2026-10-08（Q-I 尾段批落地） | **DNS 面缓冲复用（行为注记，非判据行）** | 无行文变化；`DnsFaces` UDP 收包缓冲改字段复用（F1）、DNS-TCP 连接读缓冲穿参复用（F3）、`TcpConn.rx/tx` 改 `VecDequeLite` 前缀偏移（F4）、dnsproxy 上游列表改 `Arc` 快照 + worker 懒分配读缓冲（F5） | Q-I 尾段性能项：每拍/每查询固定税消除（E22/E4 行文与数值语义零变化） | 无判据行/夹具变更；`DnsFaces` UDP 收包缓冲**复用不残留**（`recv_slice` 只写 `0..n` + 下游 `to_vec()`；新增同拍多包回归断言）；**F4 内存包络（代码门 M2 订正：rx/tx 同形）**：DNS-TCP 单连接 rx backing 最坏 128KiB → ≈256KiB（×64 ⇒ ≤+8MiB），**tx backing 同形 2× `CONN_TX_CAP`(256KiB) ⇒ 相对旧形态增量 ≤256KiB/连接（×64 ⇒ ≤+16MiB 最坏；实测未现）**，RSS 判据（≤A×1.1）已核 |
| 2026-10-08（Q-J 批落地） | **E22 新增 `fakeip=%d`**（DNS 代答计数行尾字段） | `dns: q=… aaaa-mixed=%d`（无 fakeip）→ 末尾追加 ` fakeip=%d` | F3：交付应答（`clamp_ttl`/`truncate` **之后**）答案段 A 记录命中 `198.18.0.0/15` 的**应答数**（每应答 ≤+1；截断掉的记录不计）。**不改应答一个字节**（tier `wg-native-dns:50` MUST「与主 nameserver 一致（含 fake-ip 地址）」）；fake-IP **不触发**换上游/兜底。数值语义 = 单调累计、**fake-ip 主机上常态增长**（不区分「正常透传」与「上游被污染」）；配 `dns_upstream` 覆盖后 `fakeip=0` **不代表环境干净**（与覆盖告警同源） | E22 行文（本表 E22 行已改）、`dnsproxy.rs`（`has_fake_a`/`fakeip`/`fakeip_once` 一次性告警）、单测 `fakeip_counted_on_delivered_response_only`/`fakeip_counts_only_surviving_records`/`fakeip_guard_single_source`；tier `wg-native-dns` 代答可观测性段的统计行描述（**上报项**） |
| 2026-10-08（Q-J 批落地） | **term 键编码平台口径**（新 caps 位 + 声明后行为差异；**取代 D-10「按出口编译宿主」**） | 口径由「出口**编译宿主**（`IS_DARWIN`）恒选」→「**客户端 HELLO caps 声明**；未声明/歧义 = **宿主推断**（= 今日逐字节同值）」：新 caps 位 `1<<2 KEY_ALT_NO_ESC_PREFIX`（darwin 分支语义）/ `1<<3 KEY_ALT_ESC_PREFIX`（非 darwin 分支语义）；**两位同置/裸 ID 形态（caps=0x7F 恰含两位）⇒ fail-soft = 未声明 + 计数 + 一次性告警，绝不拒腿**；声明生效时 macOS 出口上 `alt+文本` 由 `text` → `ESC text`（mok2 alt 位/kitty 关联文本/super 抑制文本同批随口径）。**旧对端策略**：旧客户端不发新位 ⇒ 宿主推断（逐字节不变）；旧出口（< 本批）不认识新位 ⇒ 静默忽略 ⇒ 宿主推断（矩阵见 `docs/reviews/QJ-design.md` §2.2）。**非 darwin 口径无夹具**（向量由 darwin 宿主产出）——真源 = vendored ghostty 源码行级 + `keyenc.rs` 期望表 | F1：出口按**编译宿主**选平台分支 ⇒ 同一手机连 Mac / 连 Linux（阿里云）出口时 alt+文本键字节分叉（审计 P1 核心） | `term/frames.rs`（caps 位）、`term/keyenc.rs`（`KeyFlavor`/`host_default` 唯一 `cfg!` 残留点/`encode_key` 必收参）、`term/service.rs`（serve_hello 判定 + 每腿 `LegRt.key_flavor` + `handle_input` 查不到腿丢弃计数，**不回落 host_default**）、`term/vt.rs`；单测 5 例（`keyenc_parity_with_go_vectors` 去 `#[cfg(macos)]` 全平台跑、`non_darwin_flavor_expectations` 8 案、`hello_caps_flavor_ambiguity_is_fail_soft` 五态、`alt_flavor_declared_per_leg` 两腿分叉、`input_for_missing_leg_dropped_with_count`）；**D-10 条目被本条取代**（宿主推断降为缺省）；tier 侧 App 需置 `KEY_ALT_ESC_PREFIX`（**上报项**） |
| 2026-10-08（Q-J 批落地） | **CA11（launchd）触发集 + 非相关分支文案** | 判定输入集：「仅默认 state 等 KeepAlive」→「plist **内容**提及该 state（`--state DIR`/`--state=DIR`，两侧同一 normalize）⇒ 等；零参形态 + 默认 state ⇒ 等（Q-H F14 旧判据保留，**不引入 argv[0] 谓词**）；未知形态 ⇒ 保守退化 = Q-H 行为；**仅提别的 state ⇒ 不等（默认 state 亦然——同一格，代码门 L3 补记）**）」；非相关分支文案 `（state=<dir> 非默认 state——不等 launchd KeepAlive，直接拉起）` → **`（未在册 launchd 代理提及 state=<dir>——不等 launchd KeepAlive，直接拉起）`**（旧文案在「默认 state 但无相关 plist」时说假话；**该行仅 darwin 打**——Linux 无 launchd，Q-H 形态此情形无输出，代码门 L4）；等待分支文案不变 | F6：Q-H F14 落地后「KeepAlive 仅默认 state」的假设与实机不符——生产 plist 实为 `--state ~/.config/homeway-rs`（自定义 state）⇒ 该 state 下 CLI 不再等 KeepAlive 而直接自拉起 | CA11 行（本表已注记）、`daemon_cli.rs`（`detect_launchd_agent_for_state`/`detect_in_dirs`/`parse_program_arguments`/`normalize_state`）、单测 3 例（四态 / 未知形态两态 / 分隔与 normalize + 生产夹具逐字）；**与 tier `role-management:184-188`「或本机任意 homeway 代理 plist」检测集合有 spec 张力**（上报项） |
| 2026-10-08（Q-J 批落地） | **UPnP 协议面（F4）——与 Go 的四条有意分歧 + E20 族数值语义** | ① SSDP 钉卡失败：硬失败终止候选 → **降级继续**（记行；发送已由 IP_MULTICAST_IF 钉在源卡，接收可能被默认路由/TUN 抢走）；② M-SEARCH ST：单发 `IGD:1` → **每轮固定三发 `IGD:1`/`IGD:2`/`upnp:rootdevice`**（MX=2 不变）；③ 应答采纳：「首应答即信（不看 ST/USN）」→ **判定表**（三条件 + ST/USN 精确段匹配 IGD 型 ⇒ 立即采信；非 IGD 型 ⇒ 待定，**只在 SSDP 腿期限到点后**回退采信）；④ 新增 **`AddAnyPortMapping` 末位兜底**（显式候选 prefer→internal→+1…+9 全失败后：期望端口交路由器改派、`NewReservedPort` 可解析且 ≠0 才算成功、租期 3600→0 回退；成功 ⇒ 外部端口由路由器选择——修前该形态 `NoPortAvailable`）。**既有成功路径零变化**；新增两行 additive 告警（钉卡降级 / A-any 成功）。**spec 偏离补记（代码门 M2）**：A-any 的触发条件与 tier `exit-upnp-port-mapping:32-35` 的 MUST（「候选全被占用/拒绝时 MUST 走映射失败路径 + 打『未取得端口映射』行」）WHEN **完全重合**——A-any **成功**时不再走该失败路径、外部端口改由路由器决定 ⇒ 定性与 `dns_upstream`/`dns_fallback` 同为「**opt-in 等价偏离**」（仅当路由器支持该动作时生效；不支持/失败 ⇒ 原失败路径与计数不变）；与 F6↔`role-management:184-188` 的 spec 张力同批上报（`docs/reviews/QJ.md` 上报项） | F4（超 Go 加固，逐条登记）：Go 基线 `internal/server/upnp.go:29/138-139/161-163/122-131` 同形旧形态 | E20 族数值语义（A-any 成功形态）、`upnp.rs`（ST 三态/`ssdp_is_igd_type`/`ssdp_location_with` 待定回退/`add_any_port_mapping`/`pin_multicast_with` 降级）、单测 `msearch_shape_three_states`/`ssdp_igd_type_matching`/`add_any_port_mapping_last_resort`/`shrink_path_unaffected_by_any_mapping`/`multicast_pin_failure_degrades`；**失效形态影响**：多候选/混答 LAN 与 A-any 消耗共享 40s 预算（`QJ-design.md` F4 风险②③） |
| 2026-10-08（Q-J 批落地） | **F2 五配置键（additive）+ 两处 spec opt-in 偏离 + config.toml 对 Go 单向** | 无 → 有（`[serve]` 五键，默认 = 修前硬编值逐值不变）：`dns_upstream`（显式上游覆盖；空 = 跟随 `/etc/resolv.conf`）、`dns_fallback`（缺省 `223.5.5.5`；**空串 = 拒启**，不是「关兜底」）、`ddns_resolver`（缺省 `223.5.5.5:53`/`119.29.29.29:53`）、`dns_probe_target`（缺省 `223.5.5.5:53`/`1.1.1.1:53`；**一键喂挑卡/健康探针/udpcap 三路 = 显式登记耦合**）、`stun_probe_target`（缺省 CF/Google；**须显式带端口**）。**两处 opt-in 偏离**（默认面不动）：`dns_upstream` ↔ tier `wg-native-dns:40` MUST（系统解析配置跟随）；`dns_fallback` ↔ `:77` SHALL（223.5.5.5）——仅显式配置时偏离。**env 缝纪律**：`HOMEWAY_BINDWATCH_PROBE` 只影响健康探针，挑卡恒吃 config/默认。`dns_upstream` 覆盖生效行（进 E4 取值）另告警**恰一次**（`DnsProxy::spawn`：fake-ip 主机上手机拿真实 IP、域名规则失效——代码门 M1 补齐，含测试断言）；**五键端口 0 一律拒启**（值域，代码门 L2）。**不加 CLI flag**（配置面足够，登记「不加」）。新键使 config.toml 对 Go 侧（`Undecoded()` 检查）**单向不兼容**（Go 已退役，仅影响回滚/对照） | F2（P1：DNS 上游/探针目标硬编码 CN 段 + fake-IP 无卫兵；「统一」落点按 tier spec 收窄——dnsproxy 侧**不得**做拒绝式卫兵，见 F3） | `serve_cli.rs`（五键 + 值域 + 边界解析）、`engine.rs`（`ServeConfig` 五字段 + 装配）、`dnsproxy.rs`（`Upstreams::with_static` 静态覆盖不做 mtime 跟随）、`ddnscheck.rs`（`resolvers` 穿参）、`egress.rs`（`preferred_iface(route_probe)`）、`bindwatch.rs`（`pick_targets`/`health_probe_targets_from_env`）、`nodestate.rs`（模板键表）；单测 `f2_keys_defaults_unchanged`/`f2_keys_take_effect`/`static_upstream_override_no_follow`/`fallback_from_config`/`defaults_unchanged_by_new_keys`/`pick_targets_ignore_env_seam`；**上报项**：两处 spec 偏离需 tier 知会/修订 |
| 2026-10-08（Q-I 尾段批落地） | **F2（reactor 并入引擎 poll）尝试后回退——零残留（登记留痕）** | 曾实现「reactor 兴趣集并入引擎唯一 poll（快照直派）+ `intercept: reactor 观测 … 兜底=N` 字段 + 两条唤醒时点行为变更」→ **实测负收益后整条回退**（代码与判据面回到 Q-H 形态；`兜底=` 字段与两条行为变更**未落地、不登记**） | F2 止损闸门（设计 §4.3）：两项 poll 样本合计 **+18%**（不降反升）、进程 CPU **+14.5%**、up 吞吐 **−5.9%**；机制 = 上游 fd 就绪成为引擎唤醒源 ⇒ 拍频 13.2k→21.4k/s、每拍全量 pump 的固定成本放大 | **本行不涉任何判据行/wire/夹具变更**；证据与数字见 `docs/reviews/QIt.md` 性能节 + `docs/PERF-AB.md`；`server/engine.rs`/`server/intercept/mod.rs` 内注释留痕（`reactor_turn` 文档注释） |
| 2026-10-08（Q-F-B 批落地） | **`portForwards[]` 状态文案与 `ClientCoreTunSetPortForwards` 返回码**（契约面行为变更，非编号判据行；**接续 Q-F 该条**——Q-F-B = Q-F 的 portfwd 挂账项收口批） | ① `state`：恒 `"failed"`（Q-F 诚实态）→ **`"listening"`（真 bind 成功且 accept 线程在位）/ `"failed"`（真 bind 失败 / 装配期破损配置 / accept 致命错误的迟到失败）**；② `err`：恒「手机核未提供端口转发监听（127.0.0.1:<listen> 未监听）——该映射在当前版本不可用，不影响隧道」→ **空串（成功）/ 真 bind 失败 `errno` 原文（失败）/ 装配期精确 err（破损配置）/ accept 致命错误原文（迟到失败）**；③ `code`：空 → **`bind_failed`（真值，tier 渲染「端口被占用」）**；空码**只剩非常态面**（装配期破损配置 / 迟到失败——spec 的空码兜底路径）；④ `conns`：恒 0 → **真连接数**（accept 准入 +1、连接结束 −1）；⑤ **未 attach / 未装表期 与 收工后**：`portForwards` 为空数组（tier 显示「启动中…」）——HEAD 是「失败 · 未提供…」；⑥ rc：恒 `-1` → **`0`（有承载：已受理并真装表）/ `-1`（无世代或世代已收口——改动随下次连接的 tunConfig 生效）/ `-2`（JSON/校验不过，含新增条数上限 8）** | Q-F-B：实装真监听器（Q-F 挂账项收口）。`bind_failed` 由「具体化假归因」转**真值**（真 `bind()` 失败），spec「失败原因 MUST 携带稳定枚举 code」由「有意偏离」转**达标**（限定语：除非常态空码面） | `tier:openspec/specs/port-forwarding`「端口映射的建立与访问」SHALL 由**已知不达标**转**达标**、「映射状态可见」失败码 MUST 转**达标**；tier 两处失义文案（`PortForwardsPage.ets` 的 `dirty` 兜底提示 / `TierVpnExtensionAbility.ets` 的 rc 日志）**随本批自愈**；`facade/portfwd.rs`（`PfRuntime`）/`facade/tun_exec.rs` 单测；**`fixtures/` 无 portForwards 夹具 ⇒ 无字节夹具变更**；`tools/check-vocab.sh` **零改动**（声明集/缺席表/manifest/tier 码表四处不动） |
| 2026-10-08（Q-K 批落地） | **IPv4 分片处置**（Q-B F7 的「接受的差异」条目；**非编号判据行**——DC18/E12/E22 行文与数值语义均不动） | 「拦截层丢弃入站 IPv4 分片（非首片 `frag_off>0` 或 MF 置位）并计 `fragDrop`——**拒绝 Go/gVisor 会重组的形态**；出口侧分片重组未实现」 → 「① 拦截层在建会话之前对入站 IPv4 分片做**有界重组**（30s 超时〔按 `created` 计、不续期〕/ 64 上下文 / 每源 4 / 64 片 / 4 MiB / 单报文 ≤65535），重组成功的报文走**与正常包完全相同**的 `route_plain` 路径（分片自身**不建会话**——F7 性质保持）；② 部分重叠、**内含重叠**、同区间不同字节、越界、非 8 倍、空片 ⇒ **整条丢弃**（两处比 Go 严、与 Linux 一致）；③ 超时且首片在位 ⇒ 回 ICMP type 11 code 1（**载荷 8B，与 gVisor 的 RFC 1812 ≤548B 有意不同**；本仓客户端**看不到**它）；④ **出口 TX 侧反重写新增分片感知**（非首片只改 IP 源；首片用 RFC 1624 增量更新 L4 校验和）」 | Q-K：恢复 Go/gVisor 的重组能力（对齐），同时保持 F7 修掉的「不占会话表 / 不污染载荷」性质；并修复设计门查出的**对称缺陷**（TX 侧 `on_tx` 无分片门 ⇒ 首片 UDP 校验和被按首片长度重算覆盖、非首片概率性改写另一条流的 IP 源与载荷前 2 字节——**返向 1253–1472 大 UDP 修前根本不通**） | `crates/homeway-core/src/server/intercept/{mod.rs,reasm.rs,nat.rs}`、`crates/homeway-core/src/wgcore/mod.rs`（F5-b 门）、根 `Cargo.toml`（smoltcp features）；`docs/INTEROP-CRITERIA.md` §已知口径注记的 Q-B F7 条目已**注销指针**（历史记录保留）；Q-B 的 `docs/reviews/QB.md`/`QB-design.md` 不追改（历史记录）；单测 T1–T38（`reasm.rs`/`intercept::tests`/`wgcore::tests`/`daemon::proto::tests`） |
| 2026-10-08（Q-L 批落地） | **备用屏族 47/1047 内容面**（`CSI ? 47 h/l` / `CSI ? 1047 h/l`；**非编号判据行**——E*/C*/R* 行文与数值语义均不动） | 「屏不切换（47/1047 落 vte 表外 `PrivateMode::Unknown`、被 alacritty 忽略）——wire Alt 位与 DECRQM 报告面已记账，屏内容面不可达（R6-gate1 §B6 / `QD.md` §6.1 挂空）」 → 「**真切屏** + 进出**双向整光标拷贝**（含 SGR 样式）+ 主屏 DECSC 槽不动（进入前捕获、退出后还原）；`47` 不清屏；`1047` 的「退出前在 alt 上 `eraseDisplay(complete)`」由 alacritty 下次进入复位 alt 内容**等价覆盖**（依赖登记差异，见「已知口径注记」Q-L 条）；语义真源 = ghostty `Terminal.zig:4800-4869`」 | Q-L L2（挂空项收口；设计门 A2 **翻案**：alacritty 0.26 有**公开** `Term::swap_alt()`（`term/mod.rs:714`）与公开 `grid()/grid_mut()` ⇒ `vt.rs` 原注释「alacritty 无公开 swap-alt API」不成立，已改写） | `crates/homeway-core/src/term/vt.rs`（`set_private_mode`/`unset_private_mode` 的 `Unknown` 分支 + `alt47_saved_decsc` 捕获槽 + RIS 清槽）；单测 8 组（`vt_alt47_switch_and_content`/`vt_alt47_cursor_copy_both_ways`/`vt_alt47_idempotent_guards`/`vt_alt47_preserves_primary_decsc_slot`/`vt_alt1047_same_and_reentry_clears`/`vt_alt1049_path_unaffected_by_l2`/`vt_alt47_capture_not_stale_across_1049`/`vt_alt47_decsc_restore_clamped_after_resize_shrink`）；**代码门整改（高1）**：还原捕获的 DECSC 槽前**按当前尺寸钳制**（`restore_alt47_saved_decsc`）——捕获跨 resize 缩小后未钳制的点会让后续 `ESC 8` 越界 panic（修前实测红：`len 5 / index 23`；ghostty `restoreCursor` 同为钳制口径）；**混合形态 1049 退出还原**（曾以 47h 进入、未走 47l 就经 1049 退出时还原；正常 1049 会话捕获为 None ⇒ 零影响）；**fixtures 不动**（`surface_codec.json` 不含 47/1047/1049，已 grep 证）；1049 路径与 session-styles golden 逐字节不变；K-15「47 备用屏」一条由本条收口（残余 = 重入清空差异） |
| 2026-10-08（Q-L 批落地） | **取值 flag 纪律：空值 carve-out 四 → 五（`--bind-interface=`）**（对照 Q-I 尾段「取值 flag 纪律的局部回退」行） | Q-I 尾段口径「四 flag 空值 carve-out（`--stun`/`--stun6`/`--relay`/`--ddns`）；**`--bind-interface=` 空值 Go = auto 而 Rust 拒**（有等价写法 ⇒ 登记不扩）」 → **`--bind-interface=`/`--bind-interface ""` 空值接受，按 Go `ResolveBind("")`（`cli.go:190-198` TrimSpace 后判空）⇒ `BindMode::Auto`**；同时**枚举形态 trim + 大小写不敏感**（`" AUTO "`/`NONE`/`Off` ⇒ Auto/Off——修前漂到 `Explicit(" AUTO ")`，engine 打伪告警「找不到 → 退回 auto」）、**网卡名保原大小写**（只去首尾空白——Go `net.InterfaceByName(v)` 吃 trim 后的原串）；缺值/吞 flag 纪律不变 | Q-L L3（`QIt.md` §7.3 末行登记差异收口；纯取值纪律，传输无关——与 Q-I 尾段 F0 同族） | `crates/homeway-cli/src/serve_cli.rs`（flag 站点改 `take_value_empty_ok_or_exit` + `parse_bind_iface` 重写 + 用法行）、`crates/homeway-cli/src/cli_flags.rs`（carve-out 边界表扩第五站点）；单测 `bind_interface_empty_and_enum_forms`（flag/config 两形态 + 枚举/网卡名/IP 五组形态）/`empty_value_carveout_forms`；**`--public-endpoint=` 仍拒**（Go 同拒，不属 carve-out）；config `bind_interface = ""` 同 parse 层 ⇒ 不再走伪告警路径 |
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
| 2026-10-09（**M4 S6 代码门 r21 更正**；登记只可追加） | **对上面 M4 第 1 / 13 / 14 行的更正（`-2` 构成 / 节流窗措辞 / 舍入）** | ①**`-2` 的构成订正**：第 13 行原写「`-2` 的构成 = 「无世代/陈旧」∪「岛不在/未 attach」」（转抄设计 §5.3 ③）**与代码相抵**——`facade/tun_exec.rs::recover` 的 `-2` **只**由两条既有前置产出（无世代 / 陈世代）；「岛不在/未 attach」的**在世世代** `l3_on_island()==false` ⇒ 走 **WG 原路**、可达 **`-1/-3/-4`**（设计 §5.3 表第 2 行自述一致；`session/recover.rs::as_rc` 永不产 `-2`）。⇒ 岛档「只产 `0/-1/-2`」成立，但 `-2` **不含**岛缺失；连带第 13 行影响面里「tier 的 `-2` 文案（当前无 attached 隧道）在该形态下不实」**不成立**（该形态下确实没有 attached 世代）。②**节流窗措辞订正**：第 1 行的「与 tag 1–3/probe 共享同一**全局**计数窗」读作「**两个独立计数窗**（受理/结束行共用 `streams_open`；拒行共用 `stream_refused`），各自首 3 + 每 100，跨 tag 共享」。③**舍入订正**：第 14 行与 `M4.md` 的「实测最坏 2.109s」原值 = **2.107914625s**（`/tmp/m4s5-res/pf-s4-recover.log`）⇒ 按 **2.108s** 读（判据门 ≤8s 不受影响） | 代码门 r21 的 F1/F7/F11（评审回源核对 `tun_exec.rs` 的两条前置 + `as_rc` 的取值集）⇒ 按判据政策**追加更正**（既有条目不改） | 本表第 1/13/14 行的读法以本行为准；`facade/mod.rs::tun_recover` 的 doc 已同批订正（代码注释可直接改）；设计 §5.3 ③ 的该句**待主会话**追加「实施期订正（二）」（本切片不改设计文件）；`docs/reviews/M4.md` 收口记录的差异登记含此三条 |

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
> **再下一行 = 2026-10-08 Q-K 批落地登记**（**IPv4 分片处置**——Q-B F7 的「接受的差异」由「拦截层一律
> 丢弃」转「有界重组交付 + TX 分片感知」；**本批零编号判据行变更、零 wire 夹具变更**——线上包形态变化
> 〔1 报文变 N 包〕属计数输入集表登记项，见下；词表门零改动）；计数输入集表 = Q-K 三行
> （`fragDrop` 重定义 + 6 新计数 + 出口→客户端线上包形态）；**additive 观测行 2 条**（非编号判据行）：
> `intercept: 分片重组超上限（上下文 …，源 …）——新报文暂不可重组（累计丢 N 片）` 与
> `intercept: 分片重组超时（N 片，30s）——整条丢弃（累计 M）`——**键 = 纯 kind（不含 `src`；
> `src` 只进行文）**、两个封闭具名字段（无 HashMap）、节流 = 首行 + 每 100 次一条；
> **重叠/冲突/非法不记行**（可被对端逐包诱发，纯计数——有意取舍）。
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
> **再下两行 = 2026-10-08 Q-L 批落地登记**（**备用屏族 47/1047 内容面**——Q-D/R6-gate1 B6 的「屏内容面
> 不可达」挂空收口，真切屏 + 双向整光标拷贝 + 主屏 DECSC 槽保护；**取值 flag 空值 carve-out 四 → 五**
> ——`--bind-interface=` 进 carve-out〔Go `ResolveBind("")` ⇒ auto〕，并顺带修 `" AUTO "` 等枚举形态的
> 伪告警路径）；Q-L 批**零编号判据行变更、零 wire/夹具变更**，词表门零改动。
> **Q-L 对上一段收束句的失效声明（追加式，不改历史）**：本段上方「`--public-endpoint=`/`--bind-interface=`
> 空值形态**不在** carve-out」中的 **`--bind-interface=` 半边自此失效**（Q-L 已扩，见上两行）；
> **`--public-endpoint=` 仍不在 carve-out**（Go 同拒）。验收口径：`--bind-interface=`/`--bind-interface ""`
> 按「接受并按 Go 语义（auto）处理」验收；`--stun=`/`--stun6=`/`--relay=`/`--ddns=` 仍按 Q-I 尾段口径验收。
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
> **其后再追加 1 条更正行**（对上面第 1 / 13 / 14 行的**自查订正**：节流窗措辞 / `-2` 构成 / 最坏耗时口径
> ——代码门 r21 的 F1/F7/F11；正文 17 行不改，读法以该更正行为准）。
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

> **M5 批（WG → QUIC 传输层换代程序「M5 WG 路径删除与收束」；S4/S5/S5t 落地）——本表其下 14 行**
> ——登记面 = `docs/reviews/M5-design.md` §8.1 全表（逐行处置）+ §8.2 的三条**批量签名条目** +
> §8.5 的 L-1…L-11 五字段草案（**指针化条目已按设计门 r22 H18 展开成字面串**）+ 本棒新增的
> L-12…L-14（token 段容器 / E21 措辞 / X1 中继 hint 行）。
>
> **批量签名条目（三条；不再逐行展开）**：
> ① **`quic: ` 前缀批量删除**：`quic: <正文>` → `<正文>`（只删前缀 4 字符；**例外**：4 条已登记串的
>    正文本身要改——`admit_close` 的「本世代回落 WG 承载」、`tun_exec` 的「尝试 WG 兜底」、
>    `engine` 的「回落 WG 原样」、`按承载分档`——C3/C4 已改）。受影响面 = §8.1 表内全部带前缀行
>    （C2/C4'/C5'/C6'/C15'/N-a…N-d/E-q1…E-q4 及岛侧行族）+ `crates/homeway-quic/src/**`、
>    `facade/tun_exec.rs`、`crates/homeway-core/tests/quic_*_e2e.rs`、`tools/quic-*.sh`（10 脚本）；
>    **例外保留**：`tunStatusJSON` 的 `quic{…}` **段名**（结构键，非日志前缀）。
> ② **WG 档行族整体删除**：C4/C5/C6/**C11 全族**/C15（WG 串）/`MIRROR`/`RECOVER`/`RELAY-UPGRADE`/`RARM`/
>    「两串都合法」例外条款；消费者（`wgcore`/`wtransport`/`session/recover`/`server/bind` 的 WG 面）
>    整体退役。影响面 = 排障脚本 / `tools/matrix.sh` / 单测（C3 已同批）。
> ③ **承载开关三键退役**：`HOMEWAY_TRANSPORT` / `serve.quic` / `tunConfig.transport` → 无
>    （非法值「记行 + 缺省」面同批删）；保留 `HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap` /
>    `serve.quic_listen`。M1 登记的「配置新键（additive，六个）」条目**部分作废**（六键里三条退役）。
>
> ④ **远程解析腿 / `dnstest` 退役登记（E 棒代码门 H-2 钉死实况）**：`dnstest` 动词**退役**（岛无 UDP
>    socket 服务面；动词面无 dispatch、用法行除名）；**socks 域名目标（CA5）在宿主会话面不可用**——
>    `tag_for_port(5300)` = `None` ⇒ `daemon/carriers::DialErr::NoService(5300)`，归因文案 =
>    `出口未提供该服务腿（端口 5300 在 QUIC 档无 STREAM tag）`（**此前误报「连接被拒（对端 RST）」**，
>    代码门 H-2 整改）；**无「本机解析」兜底**（上一版模块头注释「降级为本机解析」失实，已订正）。
>    **E4 行文零改**（出口侧 `resolve=` 监听仍在、逐字同串）但其 `resolve=` 腿**在宿主会话面零消费者**
>    ——入口集注记见「计数输入集/数值语义」表；**CA5 = 降级**（daemon/socks 面不可用；**带 L3 的
>    客户端面（App TUN / `connect`）经隧道 TCP 到 `tunnel_ip:5300` 仍可达**——是「宿主会话面没有这条
>    路」，不是出口能力消失）。
>    **⚠️ 设计 §12-C-8 的「承接」选项（新增 `STREAM[tag=6]`）未做 = 待用户裁决项**（见 `docs/reviews/M5.md`
>    的收口材料；若用户选「承接」，本条目随之作废并改回原判据面）。
>    **【2026-10-12 状态更新】**：用户已裁「承接」并落地（DNS-STREAM 批）⇒ **本条目 ④ 的「退役执行」
>    部分作废**（CA5 恢复，机制面改述为 `STREAM[tag=6]`；`dnstest` 退役维持）；登记见「判据变更记录」
>    2026-10-12 条。**保留为真的半句**：栈内 `:5300`（`resolve=`）腿在宿主会话面**仍然零消费者**
>    ——daemon 走 tag=6，不经它。
>
> ⑤ **E10/E11 的证据改锚（Rust 客户端链路）**：`tools/matrix.sh` 对 **Rust 客户端**链路
>    （`C=rust`）的 **E10** 判据从「出口 `intercept: tcp transit …（dialok）` 行」改为
>    **客户端往返行**（`transit: 经隧道拨`——QUIC dial 腿 `STREAM[dial]` 不再经出口 intercept
>    transit 面）；**E11** 从 PASS 降 **WARN**（QUIC dial 腿的收口行受出口 #17 节流
>    〔首 3 + 每 100〕，独立证据 = M4 `quic-pf-e2e` 的流收口/泄漏判据）。**判据行本体零改**
>    （E10/E11 的行文与语义未动）——改的是**验收证据的锚点**，故记在批量条目而非行级（E 棒代码门 M-7 指出）。
>    **同批**：矩阵 `DNS` 行从 `PASS` 改 `SKIP`（未跑的检查不得记 PASS；`SKIP` 档语义 = 本轮形态不适用）。
>
> **ID 说明**：M1 的两族 additive 行按设计改 **`C20`/`C21`**（避与 M3 的 `C18`/`C19` 撞名）；
> 新增 **`E-q6` / `E25` / `N-e`**（落登记前经真源词边界 grep 实测为空位；门断言见
> `tools/check-wg-removed.sh` ⑩——**与设计 §7.1-⑩ 的差异**：设计写「新 ID 全表 grep = 0」是
> **落登记前**的空位核查，登记落地后其常驻不变式 = 「各恰一条登记行」，否则门在登记后恒红）。
>
> **计数输入集表 = M5 六行**（E22 / `udpNoReply`+`udpDrop`+`shapeDrop`+`txFragDrop` / C5+C6 岛赛跑 /
> DC18 / `transport.localErr*` 两键恒缺席 / E10–E12+E23 的 WG 档限定语失效——见下节）。
> **fixtures**：`vectors/token.json` 退役（L-12）；`identity/psk/reg` 三件**保留原样**（生产消费者
> 实在，见 L-8 的「到」列）+ **`vectors/endpointcache.json` 于 E 棒退役**（E 棒代码门 M-9：
> 该件**零消费者**——L-8 原登记的「四件生产消费者仍在」对它不成立，已订正；处置 = 文件删 +
> 生成器停产出 + `SHA256SUMS` 去行）。
> **tier 触点（只出草案，不改 tier）**：`exit-transit-intercept` 的回环同端口豁免 MUST 退役 +
> 健康判据换源（设计 §12-C-7，须用户点头）；`connection-lifecycle.md` 修订稿随本轮交付（M3 先例）。
>
> **本批的策略 = 「行文改写/删行 + 值域收窄 + additive 三新行 + token 格式换代」**；
> **未在上表出现的行文改动 = 静默破坏对齐**（判据政策）。

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-10（M5 S4/S5 落地） | **内存门槛「单连接」格：≤+320K → ≤+640K** | `单连接 ≤ +320K`（M1 登记；M1 实测 +496K/+608K = **未过**，M2 后 +3.2%、无恶化）。**【2026-10-12 补登】**——本条此前只落在门槛表括注与 M5-design §9.3/§12-C-4，**未入本表**，按判据政策补齐 | M5 设计 §9.3/§12-C-4 证据链：①原 256K/64K 锚来自附录 A **手抄 37.6K、无原始证据链**；②实测五点拟合 81.6K / 三点 96.0K；③QUIC 单连接内存主体 = quinn 连接状态 + 流窗（每流 `stream_receive_window=4 MiB` + 连接级 `receive_window=8 MiB` 聚合闸）——**降窗是唯一有效杠杆，而 S9 实测窗是吞吐瓶颈**（256 KiB 档 0.464×）⇒「达标即降吞吐」。**修订 = 事后拟合（非实测新基线），2026-10-10 用户批准** | →「**单连接 ≤ +640K**」；**falsify 口径** = 实测上界 608K × 1.05。**失效条件（写死）**：若后续帧尺寸/连接模型变化（如内层 MTU 调整、回程队列默认值变更）⇒ 该门失效、须重测重订。影响面 = `docs/QUIC-ROADMAP.md` 门槛表内存行（同批改，含「2026-10-10 用户批准再修订」括注）、`tools/quic-ab.sh mem` 臂判读口径、M5 终值判定（该格自 M1 起的历史读数 +496K/+608K 按新门槛**转达标**）；**与 M6.7 条的「门槛口径写死」配套**（判定面 = lab 探针口径、不含 `ClientSock` 显式缓冲；产品侧口径 = 真机 App Pss） |
| 2026-10-10（M5 S4/S5 落地） | **L-1 体积判据的档位口径** | 「现役 `.so` 2,213,744 B / QUIC 净增 ≤ +1.5MB」（无 `[profile.release]` 档） | M5 引入 `lto=true` + `codegen-units=1`（设计 §0.4：不改档位则删完仍 4.39–4.54MB ⇒ 判据不可达） | →「现役 = 1,685,144 B（WG-only + LTO）；双栈 = 3,556,520 B；判据 ≤ 3,800,000 B（十进制）**按同档判、跨档不得互引**」。影响面 = 门槛表（体积行，路线文件=主会话触点）、`docs/QUIC-BASELINE.md` 口径、`tools/quic-ab.sh size` 臂、M6 报告口径行 |
| 同上 | **L-2 E1 的 `wg=` 字段** | `serve 就绪：wg=:%d（配置端口；被占用会自动退让）tunnel=%v files=%d term=%d speedtest=%d …` | WG 端口退役（QUIC 单 UDP 端口接管该位；设计 §11-S3a「`exit/socket.rs` 成为唯一 UDP 面」） | → `serve 就绪：quic=:%d（…）…`（**其余字段逐字不变**；`files/term/speedtest` 三数=客户端「端口→STREAM tag」映射输入，**出口不再于隧道 IP 上监听这三端口**——口径注记见「已知口径注记」M5 条）。影响面 = `tools/local-*.sh` 的 E1 等待点、`tools/matrix.sh`、`serve_cli.rs` |
| 同上 | **L-3 E10/E11/E12 的 kind 值域** | E10/E11：`%s ∈ {transit,exempt}`；E12：`{transit,exempt,dns}` | 豁免面整体退役（设计 §3.2/§3.3：`local_services`/`DialTarget::Unix`/`Kind::Exempt` 全删） | → E10/E11：固定字面 `transit`；E12：`{transit,dns}`。影响面 = 出口排障脚本、`intercept` 单测（原 `exempt_flow_end_to_end`/`uds_exempt_flow_end_to_end` 两例改为 `transit_flow_end_to_end` 一例 + 删一例） |
| 同上 | **L-4 E5 / E14 / E17 行文** | E5 `intercept: 过境拦截就绪（隧道IP %v；豁免=转投本机同端口；TCP 并发上限 %d）`；E14 `files 就绪：root=%s (rw) sock=%s（隧道IP:%d 经拦截层转投）`；E17 `speedtest 就绪：sock=%s（隧道IP:%d 经拦截层转投；内存收发不落盘）` | 同上（豁免面退役 + 服务面走 QUIC `STREAM[tag]`） | → E5 `（隧道IP %v；DNS 代答腿 :53/:5300；TCP 并发上限 %d）`；E14 `（STREAM tag=1 经服务入口转投）`；E17 `（STREAM tag=3 经服务入口转投；内存收发不落盘）`。影响面 = 出口排障脚本、tier 文档触点、files/speedtest 装配单测 |
| 同上 | **L-5 WG 档行族删除 + E23 值域收窄** | C4/C5/C6/**C11 全族**/C15（WG 串）/U1/U2（RELAY-UPGRADE/RARM）**删除**；E23 `入站新源：%v（%s，%d 字节）` 的 `shape` 含 WG 载荷/注册/控制/批量形态 | 消费者整体退役（`wgcore`/`wtransport`/`session/recover`/`server/bind` 的 WG 面）；**E23 例外**：`note_new_src` 的多数调用点非 WG（STUN 应答/参照点探测/畸形腿帧/腿帧 type=3）⇒ **行保留、只收窄值域** | → 删除面见批量条目①②；E23 新值域 = `{STUN应答, 参照点探测, 畸形腿帧, 腿帧type=3, 腿帧type=N, 畸形容器/未知消息容忍}`（退役 = kind=0/1/2/4）。影响面 = 排障脚本 / matrix / 历史 review 文档（不追改） |
| 同上 | **L-6 `quic: ` 前缀批量删除** | `quic: <正文>` | 单一承载 ⇒ 前缀退化为噪声（设计 §4.4）；「剩下的前缀才是信息」（`intercept:`/`peer:`/`term:`/`serve:`） | → `<正文>`（**只删前缀 4 字符**，4 条已登记串例外见批量条目①）。影响面 = `crates/homeway-quic/src/**`（driver/client/exit/admit_close/tuning）、`facade/tun_exec.rs`、`server/{bind,engine,quic_admit}.rs`、`crates/homeway-core/tests/quic_*.rs`、`tools/quic-{island,wg}-e2e.sh`；**例外保留**：`tunStatusJSON` 的 `quic` **段名** |
| 同上 | **L-7 承载三键退役** | `HOMEWAY_TRANSPORT`（env）/ `serve.quic`（出口 config 键 + `--quic` flag）/ `tunConfig.transport`（App 世代键） | 单一承载 = 死旋钮（设计 §4.2/§15-5「三个全删」）；保留 `HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap` / `serve.quic_listen` | → 无（非法值「记行 + 缺省」面同批删）。影响面 = M1 登记条「配置新键（additive，六个）」**部分作废**；tier 侧停发 `transport`（**`TunConfigJson` 非 deny_unknown_fields ⇒ 不报错**，设计 §4.3 已核）；`[serve]` 是严格表 ⇒ 旧 config 含 `quic = false` **拒启**（升级须删该行——升级注意，已登记） |
| 同上 | **L-8 fixtures 处置** | 设计草案：`identity/psk/reg/endpointcache` 四件退役 + `tunnel_addr` 缩样本 + `tun_status.jsonl` 重写 | 设计 §8.3 的预判；**实施期实测与草案不同**：`identity/psk`/`reg` 三件的**生产消费者仍在**（identity/psk → RPK seed 派生面、reg → `table` 邻域 codec；`tunnel_addr` → `table.rs`/`tun_exec` 生产用），`tun_status.jsonl` 的可达形态（阶段机向量）**不含** `bearer`/`quic` 段 ⇒ 无需重写 | → **D 棒本批实际 = 零件退役**（`token.json` 见 L-12；`identity`/`psk`/`reg`/`tunnel_addr` 保留原样、`tun_status.jsonl` 零改）；**E 棒追加（代码门 M-9 订正）**：`endpointcache.json` **零消费者**（唯一引用 = `r2_vectors.rs` 的注释）⇒ **本批退役**（文件删 + `tools/vector-gen` 停产出 + `SHA256SUMS` 去行）——原登记的「四件生产消费者仍在」括注**对 `endpointcache` 不成立，已订正**（设计门 r22 B4 的「生产消费者与向量退役解耦」原则方向不变） |
| 同上 | **L-9（M5 B 棒 Q2）中继 listen 地址族** | `中继就绪：127.0.0.1:42781（…）` / `0.0.0.0:…`（单栈 v4 形态） | Go 双栈对齐（设计 §6-Q2 显式立条；`listen_with_fallback` 改走 `udpbatch::bind_dual_stack`） | → `中继就绪：[::]:42781（…）`（双栈形态；v4 映射地址仍可连）。影响面 = `R1` 行、`tools/local-rust-relay.sh` 断言、`relay_cli.rs` 用例 |
| 同上 | **L-10（M5 B 棒 Q3/Q4/Q5）新增告警行 + 一处致命→非致命** | `listen_port.txt` 写失败 = 致命（`?`）；`public_endpoint.txt` 两处写失败**静默**（`let _ =`） | Go 对齐（Q3/Q4 显式立条：Go 侧三处皆非致命 + 告警） | → 三处均**非致命 + 告警行（additive）**：`⚠️ 公网端点写盘失败（{path}）：{e}` ×2 / `⚠️ 监听端口落盘失败（{path}）：{e}——服务照常就绪`；既有成功行文逐字不变。另 Q5：`--public-endpoint` 每段值域校验（非法 ⇒ exit 2）。影响面 = 出口启停语义、E20 族（additive 告警） |
| 同上 | **L-11 新增行 `N-e` / `E-q6` / `E25`** | 无 → 有 | 单承载语料补全（N-e = 唯一的窄路径旋钮生效值；E-q6 = 无「回落」语境的就绪行）+ A10（M3 交下的出口侧收线时点） | → `N-e` `内层 MTU 上限 %d（来源=%s）`（世代装配一次；`tun_exec.rs`）；`E-q6` `出口 QUIC 面就绪（单承载；migration=%v，initial_mtu=%d）`（`exit/mod.rs`，E-q1 前一行）；`E25` `出口收线（连接数 %d → 0，用时 %v）`（QUIC 面收工块）。影响面 = 出口/客户端排障读者；`tools/quic-island-e2e.sh` 的 A10 断言（E25 缺席即 rc=1）；**ID 空位复核 = S5 的门断言（§7.1-⑩ 口径）** |
| 同上 | **L-12 token 格式换代：`hmw1` → `hmw2` 段容器**（M2 候选 B；主会话裁决 §15-3「本期做」） | `hmw1` ‖ base64url( peerId(32) ‖ secret(32) ‖ epCount(1) ‖ [type+len+addr]* ‖ [rpk(32)]? ‖ crc(4) )（定长顺序布局 + 按尾长猜字段） | 趁「无兼容包袱」窗口把「按长度猜字段」换成**自描述段容器**（`segCount` + `[segType+len(2BE)+body]*`，critical 位 = 高位置 1；不认识 critical ⇒ 整串拒、info ⇒ 跳过）；M3+ 加字段不再改布局（M2 设计 §4.2/§4.4） | → `hmw2` ‖ base64url( segCount(1) ‖ [segType(1) ‖ len(2 BE) ‖ body]* ‖ crc(4) )；段 = `peerId`/`secret`/`epList`/`rpk(info)`；**字段与字节含义零变化**（`epList` 段体沿用 `epCount‖[type+len+addr]*`，type 2=QUIC 照旧）；**存量 `hmw1` token 一律失效**（明确版本拒 + 可行动文案）。影响面 = `token.rs`（encode/decode/parse_body/新 `encode_v1_body`/`parse_v1_body`）、`serve token` 渲染面**行文零改**、`tools/**` 的抽取正则（`hmw1`→`hmw2`，10+ 脚本）、CLI 用法文案、`crates/homeway-core/tests/token_vectors.rs`（改自产）、`fixtures/vectors/token_hmw2.json`（新）+ `token.json`（退役）；**中继 wire 零变化**（`rl1` body 冻结在 v1 布局：`rl1` 自带前缀 + `encode_v1_body` 单源，`relay/**` 零改动）；**tier App 零代码**（只透传） |
| 同上 | **L-13 E21 的「WG socket」措辞** | `绑卡看护：WG socket 钉在 %s（%s）` | M5 后该 socket 是 **QUIC 公共端口**（唯一 UDP 面）；措辞失真 = 排障误导 | → `绑卡看护：QUIC 端口 socket 钉在 %s（%s）`。影响面 = `bindwatch.rs`（行 + 3 处单测断言）、E21 行读者；**同族其余行（探针失败/连续探不通/重钉失败/指纹变化）逐字不变** |
| 同上 | **L-14 X1 族删一条：中继 hint 盲打行** | `中继：收到对端地址线索 %v → 盲打 3 包（开自己 NAT 过滤；能否直连仍由 WG 握手决定）`（X1 七条之一） | hint 的消费者 = WG 打洞（`bind.set_on_hint` 删于 S3b）⇒ 本面自那时起**零构造点** = 不可达代码；单承载下「中继 → 直连升级」由岛的迁移/重赛跑承接（G8 登记） | → **删除**（`LegEvent::Hint`/`parse_hint_addr`/`PUNCH_*`/`run_punch_worker` 同批删）。影响面 = X1 行族（7 → 6 条）、`server/relayleg.rs`（净 −98 行）、中继排障读者；**X1 其余六条 + `relay/**` 零改动**（红线守住） |
| 2026-10-10（M5 E 棒代码门 H-2 整改） | **CA5 降级 + `dnstest` 退役（G6/G7 实况钉死）**〔**2026-10-12 已随 DNS-STREAM 承接批作废**——见本表 2026-10-12 条；`dnstest` 退役维持〕 | CA5：`curl --socks5-hostname … → HTTP 200`（DNS-over-TCP 经隧道 IP `:5300` 出口代答）→ **宿主会话面退役**：`tag_for_port(5300)` = `None` ⇒ `daemon/carriers::DialErr::NoService(5300)`，文案 = `出口未提供该服务腿（端口 5300 在 QUIC 档无 STREAM tag）`（此前误报「连接被拒（对端 RST）」）；**E4 行文零改**（出口侧 `resolve=` 监听仍在）但其 `resolve=` 腿在宿主会话面零消费者；`dnstest` 动词退役（无 dispatch + 用法行除名） | 设计 §2.6-G7 的「承接」选项（新增 `STREAM[tag=6]`）**未做**（§12-C-8 待用户裁决 ⇒ 实现按「退役」执行，见「M5 批」④）；代码门 H-2 指出三处不一致：模块头注释「降级为本机解析」**失实**、判据真源零改动、失败归因误导 | `docs/INTEROP-CRITERIA.md` CA5（E4 加注）、`crates/homeway-core/src/facade/host_session.rs`（模块头订正）、`crates/homeway-core/src/daemon/{mod.rs,carriers/mod.rs,carriers/speedrun.rs}`（`DialErr::NoService`/`Busy` + 归因分工）、`crates/homeway-cli/src/unified_cli.rs`（用法行）；**验收含义**：daemon `socks` 的域名目标**不再作为验收项**（IP 目标 CA4 不受影响）；**若用户裁「承接」⇒ 本条作废并恢复原 CA5 判据面** |
| 2026-10-10（M5 E 棒代码门 M-7 整改） | **E10/E11 的验收证据锚（矩阵 Rust 客户端链路）+ 矩阵 `DNS` 档位** | E10 证据：出口 `intercept: tcp transit …（dialok）` → **客户端往返行**（`transit: 经隧道拨`）；E11：`PASS` → `WARN`（收口行受出口 #17 节流，独立证据 = M4 `quic-pf-e2e`）；矩阵 `DNS`：`PASS` → `SKIP` | M5 S4 的 intercept 收窄（豁免面退役 + QUIC dial 腿不再走出口 intercept transit）+ 出口服务流行节流 ⇒ 原证据锚**不再产出**；**Rust 客户端链路**适用；**Go 客户端链路（L1）证据锚不变**（仍按出口 transit 行） | `tools/matrix.sh`（`base_segment` 的 E10/E11 分支 + DNS 行）、`docs/reviews/M5.md`（矩阵读数）；**判据行 E10/E11/DNS 本体零改**（只改验收证据的取行位置） |
| **2026-10-10（M7 补登：M6.5 批，责任期 M6.5）** | **M6.5 TUN 读面形态行（新增观测行两条）+ 写路径语义偏离** | 无 → 有：`TUN 读面形态：{阻塞读（每包一次 read）\|非阻塞读 + poll（回退形态）}（M6.5 逐段成本整改：阻塞读消「read+poll」系统调用对）`（**每世代装配一次**）+ 回退行 `TUN fd 读阻塞化失败（F_GETFL: {e}）/（F_SETFL: {e}）—— 保持非阻塞读 + poll 形态`；**写路径语义偏离**：tun 队列满时写线程由「`WouldBlock` → `POLLOUT` 有界等待」改为**阻塞在 `write` 内** | M6.5 逐段成本整改（真机逐段表：读线程 CPU 几乎全在「非阻塞 read 返 EAGAIN + poll」的调用对上 ⇒ 阻塞读把等待收回内核等待队列，6.11 → 2.29 s/轮） | `crates/homeway-quic/src/tun.rs`（`set_blocking_read` + 读面形态行 + `write_fd_all` 的注释自陈）；读面/写面排障读者；**回退条件 = `F_SETFL` 清 `O_NONBLOCK` 失败 ⇒ 自动回退支**（单行可逆，M6.5 §12.5-2）；stderr/世代日志读者按新行名取样 | **【M7 代码门 r31 后续（追加式）】**：本行两条观测行**立 ID = `N-f`**（主表行式登记见本文件 N-f 行）；ID 空间门（`tools/check-wg-removed.sh` ⑩）的 `NEW_IDS` 已同批加入。
| **2026-10-10（M7 补登：M5 批，责任期 M5）** | **C8 `warmup pong: 就绪（判据=%s）` 值域收窄** | `%s ∈ {wg, quic}`（M1 登记条 2026-10-09 扩域）→ **`%s = quic`**（`判据=wg` 形态随 WG 面退役不再产出） | M5 单承载（`l3_on_island()` 恒真；WG 档与 `wgcore` 同批删除）⇒ 值域只剩一值；**此前仅主表就地注记**（本文件 `:57` 的「M5 收窄」），违反「变更须入登记表」政策（设计门 r30 高-3 点名） | C8 行读者：`crates/homeway-core/src/facade/tun_exec.rs`（世代暖机段，恒 `quic`）、`crates/homeway-cli/src/main.rs`（CLI 直连形态两条，恒 `quic`）、`crates/homeway-core/tests/quic_island_e2e.rs` 断言；`tools/matrix.sh` / `local-rust-exit.sh` 的按 `判据=` 取样须按 `quic` 严格匹配（旧 `wg` 形态不再存在） |
| **2026-10-10（M7 补登：M5 批，责任期 M5）** | **`SessionSnapshot::stats` 来源换岛**（数值语义） | 同键位 `(rx, tx)`：来源 = WG 会话统计（`Client::snapshot()` 的 rx/tx）→ **岛的 QUIC 服务流字节面**（`island_stats()`） | M5 单承载后无 WG 会话可报；`facade/host_session.rs` 模块头自陈「**同键位、异来源**」（`M5.md` §5/§8-2 自陈交 D 棒 S5，未落表） | `tunStatusJSON` 的 stats 读者（`runner.stats` 两键 / `stats:` 行同源口径）；`crates/homeway-core/src/facade/host_session.rs`（`SessionSnapshot::stats` + `island_stats`）；**语义边界**：只统计**服务流**字节（宿主会话无 L3 ⇒ 不再含隧道转发量） |
| **2026-10-10（M7 补登：M5 批，责任期 M5）** | **`--dead-direct` / `--loopback-only` 语义升承载无关**（注入缝；CLI 改写面） | 「只改 `Direct`（WG 类）端点」→ **「`Direct ∪ Quic` 两族同改」**（`--dead-direct`：非中继端点一律改指 `127.0.0.1:1`；`--loopback-only`：非中继 v4 端点改指本机回环同端口） | M5 C2 承载换 QUIC 后，只改 `Direct` 会让矩阵中继段/同机 perf 形态的注入**静默失效**（注入全落在已退役类别上）；`M5.md` §2-4 自陈「登记在案」但**全表零条目**（此前仅 `:200` 一处顺带提及） | `crates/homeway-cli/src/main.rs`（`kill_direct_endpoints` / `loopback_endpoints`，含「承载无关」注释）；`tools/matrix.sh` 的中继段/直连段注入读者；**用法行零改**（flag 名不变，语义扩到两类端点） |
| **2026-10-10（M7 补登：M5 批，责任期 M5）** | **`daemon::StreamConn` 写停滞上界 30s → 10s**（数值语义） | 控制面**流腿**写停滞窗：30s（WG 档 `TunnelConn`）→ **10s**（`HostStream` 的 `write_with_backoff`：`n=0` 分级退避环 + 10s 无进展 ⇒ `ErrorKind::TimedOut`） | M5 换承载后的实现形态（QUIC 服务流待发队列的「无进展」判据比 WG 的写阻塞更早可见）；`M5.md` §2-7 自陈「登记（口径变化）」未落表 | 控制面流腿排障读者（`control: 流 %d 上行停滞（后端不读）——收流（gone）` 的触发时点提前）；`crates/homeway-core/src/facade/host_session.rs`（`write_with_backoff`）；**连接级**写停滞看门狗（socket 写超时 30s，`daemon/server.rs`）**不变**——本条只关流腿 |
| **2026-10-10（M7 补登：M6.7 批，责任期 M6.7）** | **M6.7 岛侧 UDP socket 缓冲（常量 + 新增观测行 + 内存账）** | 无 → 有：`ClientSock::open` 显式 **`SO_RCVBUF=2 MiB`（下行接收）/ `SO_SNDBUF=1 MiB`（上行发送）**（`setsockopt` 失败**不致命**：沿用内核缺省 + 行内标注）；新增观测行 `岛 socket 缓冲（SO_RCVBUF（下行接收）\|SO_SNDBUF（上行发送））：设定 {want}B，读回 {got}B{（setsockopt 失败——沿用内核缺省）?}`（**每 socket 两行**，装配期）；**内存账 +3 MiB 设定值**（内核口径 ×2 ⇒ 记账 `+6 MiB` 上界） | M6.7 真机根因修复：内核接收缓冲突发溢出（`/proc/net/udp` 的 `drops` 逐轮 +17…+32，与 `Udp: RcvbufErrors` 同源）⇒ DATAGRAM 丢包被内层 CUBIC 当拥塞 ⇒ 包率腰斩（T2 热 11.27 → 20.14 MB/s） | `crates/homeway-quic/src/client/relay_sock.rs`（`RCVBUF`/`SNDBUF` 常量 + 记行）；**内存矩阵/门槛表**（单连接 ≤+640K / 每设备 ≤96K / 32 设备 ≤+3.1 MiB 的判定须含本条显式缓冲 —— 见 `docs/QUIC-BASELINE.md` §4 与 `docs/QUIC-ROADMAP.md` 门槛表）；T9 内存复跑读数（M7 S3）；`docs/PERF-AB.md` §9.20.11 | **【M7 代码门 r31 后续（追加式）】**：①本行观测行**立 ID = `N-g`**（主表行式登记见本文件 N-g 行）；②**门槛口径写死（防误读）**：`≤+640K / ≤96K / ≤+3.1MiB` 的判定面 = **lab 探针口径**（`tools/quic-ab/arms/`，**不含**本条的 `ClientSock` 显式缓冲）；**产品侧口径 = 真机 App Pss**（M6 基线 33,823 KB 参照，U2/U3 观察期复核，超 +3 MiB 即回看本条）。
| **2026-10-10（M7 落地：S1 批）** | **C 族新增归因：存量 token（版本不符）的换代归因**（additive；`TokenError::user_message()`） | 无 → 有：`UnsupportedVersion`（`{seen}` = `hmw1`/其它旧前缀）在**用户可见面**追加一条可行动归因 —— `homeway/token: 不支持的 token 版本: hmw1` + 「（存量 token 已失效（WG→QUIC 换代：本代只铸/只认 hmw2）——请重新向出口索取 token（`serve token`）后重新粘贴）」；落点 = 世代 failed `reason`（`tunStatusJSON`，**App 用户可见**）/ CLI stderr（`connect`/`speedtest`/`files`/`portfwd`/`token`/`host add`）/ 服务会话域 reason | M6 `§12-1` 交下（M7 设计 §10-5）：拿上一代 token 撞本代二进制是**换代的必然结果**，必须给可行动归因（此前只有一句哨兵，用户不知去哪取新串）；`Corrupted`/`Malformed`（真坏串）**不**追加（防误归因） | **哨兵 `Display` 字节零改**（`fixtures/vectors/token_hmw2.json` 的 `sentinels` 仍逐字钉住；Go 对齐面不动）——本条只是**追加**：`crates/homeway-core/src/token.rs`（`changeover_attribution`/`user_message`）、`facade/tun_exec.rs`（`warmup`）、`facade/service_exec.rs`、`homeway-cli`（`main.rs` 的 `token_or_exit` + `daemon_cli.rs` 的 `host add`）；单测 `unsupported_version_carries_actionable_changeover_attribution` + e2e `legacy_hmw1_token_surfaces_actionable_changeover_attribution`（`tools/quic-wg-e2e.sh` 三条断言含 CLI 面）；**出口侧不动**（wire 上无 token 版本信息 ⇒ 出口无从归因；设计原文「必要时」= 判定不必要，见 `docs/reviews/M7.md`） |
| **2026-10-10（M7 落地：S5）** | **历史文档的局部「现行口径」面划出（操作手册类；`DEVICE-TEST-OHOS.md` + `DEPLOY-RUST-EXIT.md`）** | 无 → 有：`docs/DEVICE-TEST-OHOS.md` **新增 §8「现行口径（M7 起）」**（新串：`端点就绪（…）`**无 `quic: ` 前缀** / E1 = `quic=:` / **公共端口 = QUIC 端口 = 42651+n** / 单承载行集 `传输：新栈（QUIC 岛）`·`岛已建连`·`L3 承载 = 岛`·`判据=quic`·`岛收工`；**删去已退役串** `transport: 本世代 L3 承载 = quic`——该串已是 e2e 禁串）×1 节 + §3/§5 就地警示行 ×2 + §1 的 worktree 路径改 **main 主检出**（`/…/homeway-rs`）；`docs/DEPLOY-RUST-EXIT.md` **新增 §12**（v0.3.0 切换手册：三阶段/代际矩阵/回滚三件套/`quic_listen`/台账跨代可读探针/取 token 两态与 `中继就绪 [::]` 现行串） | M5 的「历史文档不追改」条款点名了这两个文件（本文件 `:587-590`），但 M7 的操作面（真机手册 / 部署手册）若不带现行串，用户会按**已退役**的串排障（`transport: 本世代 L3 承载 = quic` 更是 e2e 显式禁串）⇒ 把「操作手册类文件的**新增现行节**」从「不追改」范围局部划出 | `docs/DEVICE-TEST-OHOS.md`（§8 新节 + §3/§5 警示 + §1 路径）、`docs/DEPLOY-RUST-EXIT.md`（§12 新节，§0–§11 历史正文零改）；**历史正文（各节实录行）仍不追改**——本条约定的例外仅限「新增节 + 就地警示行 + 路径订正」三态；真机判据行读者（`tailcat-tun.log` 排障）按 §8 的新串取样 |
| **2026-10-12（DNS-STREAM 承接批）** | **`STREAM[tag=6]`（dns-resolve）新增 + CA5 恢复 + 两族 `tag=%s` 值域扩展** | ① 服务流 tag 表 = `1..5`（files/term/speedtest/dial/probe）→ **`1..6`**（新增 `6 = dns`，文本 `dns`）；② CA5 = **降级**（宿主会话面不可用；`tag_for_port(5300)` = `None` ⇒ `DialErr::NoService(5300)`；机制注 = 「隧道 IP:5300 经拦截层回环豁免」）→ **恢复**（机制面改述 = DNS-over-TCP → **`STREAM[tag=6]`** → 出口代答 → 应答候选按序拨；`qtcp` 增长不变）；③ **E-q5**（出口服务流行族）的 `tag=%s` 值域 `{files,term,speedtest,dial,probe}` → **+`dns`**；④ **C19**（客户端服务流行族）同款扩展；⑤ 失败归因新增 `DialErr::ServiceDisabled(port)`（`reset(0x22)` 与真拒分档，保可行动文案——承接后 **dns 腿**的 `0x22` 来源 = 出口 `dns_port=0`（其余服务未装配——files/speedtest 监听失败、`HOMEWAY_TERM=off`——同产 `0x22`，本批同档归因；代码门 r34 L1 订正「唯一」过强）） | 用户 2026-10-12 裁「**承接**」（= M5 设计 §12-C-8 的推荐选项；理由：socks 域名面是产品能力、CA5/E4 以它为前置）。M5 当时因「删码批不擅动出口 wire（增项）」而按退役执行并上报裁决 | **wire 增项（additive）**：`crates/homeway-quic/src/stream.rs`（`StreamTag::DnsResolve` + `ALL` + pin 测试）、`exit/{serve,intake}.rs`（分派/槽位）、新增 `crates/homeway-core/src/server/dns_stream.rs`（在册闸 MAX_CONNS=64 / 每帧绝对期限 / 长度域收线）、`dnsproxy.rs`（`answer_stream` + `admit_and_send` 单源）、`facade/{quic_stream,host_session}.rs`（`tag_for_port(5300)`/模块头）、`daemon/{mod,carriers/{mod,dnsq,speedrun}}.rs`、`speedtest.rs`（归因分档）、`server/engine.rs`（装配 + 新行 `dns 流腿就绪：STREAM tag=6（客户端虚拟端口 5300；代答复用 :53 同一上游 dnsproxy）`——**additive 非判据行，不立 ID**）；**E4/E5 行文零改**（出口侧 `resolve=` 监听对 L3 客户端面照旧服务——「`resolve=` 腿在宿主会话面零消费者」在承接后**仍为真**：daemon 走 tag=6，不经栈内 `:5300`）；`dnstest` 退役维持（G6）。影响面 = 排障读者（`服务流已受理（tag=dns …）`）、`tools/check-*` 三门零改（词表不含 tag 文本 / 隔离门真扫描面已核 / ID 空间门不新增 ID）、**计数输入集两行见下节**；CA5 的验收证据锚 = **进程内机制面单测**（三条：`socksmgr::resolver_for` 全链 / `dns_stream` 帧面 / `dnsproxy::answer_stream`）+ **出口侧 tag=6 分派用例**（已装配逐字往返 + 未装配 0x22）+ **装配断言**（`engine::tests::dns_intake_assembles_only_when_dns_is_enabled`）+ 人工 `curl --socks5-hostname` 轮（字面全链 e2e 未落，见 `docs/reviews/DNS-STREAM.md`「验收与残余」；代码门 r34 M2 订正措辞） |
| **2026-10-13（直连优先改裁）** | **新增两行（additive）**：`赛跑：直连窗口 %v 到点仍无胜者 → 解锁中继候选 %d 个` / `赛跑：直连候选全部失败 → 提前解锁中继候选 %d 个` | 无 → 有 | 用户指令 2026-10-13「改为之前的逻辑，直连优先」＝回归老 `DirectFirst`（口径真源 `tier:openspec/project.md`「直连优先、中继纯后备（直连窗口默认 2s）」）。**两个解锁原因必须不静默**（照老 Go 行 `MIRROR 直连窗口 %v 内无响应 → 解锁中继候选 %d 个并补发一次` 的语义，去 MIRROR/补发构件） | 排障读者（`tools/matrix.sh` 的 C-via 落中继时按解锁行复核原因）、`docs/QUIC-ROADMAP.md`「C4 赛跑语义再改裁」节；落点 `crates/homeway-quic/src/client/race.rs`（解锁点就地记行）；证据 = 单测 `direct_first_unlocks_relay_after_window_expiry`（窗口档）/ `direct_first_unlocks_early_when_all_direct_fail`（提前档）；**未立 ID**（照 C20 行族先例：additive 归因行入行族/登记行，不占 ID 空间） |
| **2026-10-13（同上）** | **C4' `赛跑投出 %d 个候选（直连 %d / 中继 %d；本行每轮限 3 条）` 的数值语义**（**行文逐字不变**） | 语义 = 本轮**实际投出**的候选 → 本轮**候选名册**（直连优先：窗口内只投直连，中继的实际投出时点 = 解锁行；**无解锁行 = 中继未投出**） | 直连优先改裁——名册与投出集在窗口形态下不再同一件事 | C4' 行读者（按「行出 = 全部候选已发起握手」理解的排障脚本须改）；落点 `crates/homeway-quic/src/client/race.rs`；证据 = 单测 `direct_first_wins_with_direct_before_unlock_and_relay_unused`（名册 2 条 / 无解锁行 / 中继零上行帧） |
| **2026-10-13（同上）** | **C5' 两清单（`完成=`/`未完成=`）输入集**（**行文逐字不变**） | 输入集 = 全部候选 → **本轮实际投出的候选**：窗口未解锁就被直连赢下的中继候选**不入**两清单（与「调用面拒绝」同档）；解锁后投出的中继**投出即入**「未完成」、完成/失败再摘除 | 同上（「两清单并集 = 投出集」的不变量在窗口形态下须重述才成立） | C5' 数值读者（主表 C5/C6 行 + `RaceOutcome` 字段文档）；落点 `crates/homeway-quic/src/{cmd.rs,client/race.rs}`；证据 = `direct_first_wins_with_direct_before_unlock_and_relay_unused`（未投出 ⇒ 两清单皆空）/ `unlocked_relay_enters_unfinished_when_it_never_completes`（投出 ⇒ 未完成含两条） |
| **2026-10-13（v0.3.4 补丁）** | **CLI 本地 term 面的 socket 解析：新增 `<state>/serve/term.sock` 回退**（**错误文案与全部判据行零改**） | 解析 = `<state>/term.sock`（早期平坦 state 布局口径）→ **扁平优先 + serve 子目录回退**：①`<state>/term.sock` 存在 ⇒ 用它（`--state` 直指 serve 目录的老用法照旧）②否则 `<state>/serve/term.sock` 存在 ⇒ 用它（统一根 / 零参形态）③都不在 ⇒ 返回扁平路径（错误文案逐字不变，仍如实指向用户给的那个 state） | 用户实测缺口（2026-10-13）：出口角色的本地服务 socket 建在 `<state>/serve/` 子目录（E16 实采串即 `sock=…/serve/term.sock`），命令面按旧平坦口径拨 `<state>/term.sock` ⇒ 「零参正式形态」下在出口机上 `homeway term list` 拨不通 | CLI 本地面读者（`homeway term list/new/attach/delete/explain` 本机形态）；远程 `--host` 形态零改；落点 `crates/homeway-cli/src/term_cli.rs`（`local_term_sock`）；证据 = 单测 `term_cli::tests::local_term_sock_flat_then_serve_fallback` + 出口机零参实测（列出 2 会话） |
| **2026-10-13（v0.3.5，用户指令）** | **CLI 主机寻址统一：`files` / `term` / `forward` 的 `--host` 可选化 + 新增两条可行动文案**（**判据行本体零改**） | ①三命令的 `--host` 缺省 = **默认主机寻址**：0 台 ⇒ 新文案 `还没有添加主机——先在出口机上执行 `homeway serve token` 取 token，再在本机执行 `homeway host add <token>`…`（exit 1）；1 台 ⇒ 直接用；≥2 台 ⇒ 新文案 `本机有 N 台主机——用 `--host <name|id>` 指定…`（exit 1）。`term` 的缺省优先级 = 本机出口面（`term.sock` 在）> 默认主机；②`files` / `portfwd` 的**用法/帮助串**改为 host 形态（`--token` 直连形态降为**自动化缝**：仍接受、不再进帮助——matrix / perf-ab / qi-ab / rekey-check / quic-pf-e2e 与 `qh_config_failfast` 用例依赖它） | 用户指令（2026-10-13）：普通用户不需要知道 token/state 这类参数；file/term/portfwd 统一 host 风格（单主机默认、无主机提示先加主机） | CLI 帮助/文案读者、`tools/*.sh` 的 `--token` 调用点（**零改**，缝保留）；落点 `crates/homeway-cli/src/{daemon_cli.rs（`default_host_ref`）,main.rs,term_cli.rs,carriers_cli.rs}`；证据 = 真机三形态实测（无主机提示 / 单主机 `files list` 通 / `forward add-list-delete` 通 / `term new -d` 通） |
| **2026-10-15（event-driven-heal §1，未提交批）** | **TUN fd 失效判据（Q-G 批 F2 口径注记的演进）** | 判死条件「只看 `hup\|\|err\|\|nval`」→ **三类 + 僵死形态双阈值**：「poll 恒报可读而 read 恒返空/EAGAIN」连拍 ≥100 拍 ∧ 墙钟 ≥1s ⇒ 同款 `TunFdDead` 判死（归因行 `tun fd 僵死：poll 恒可读而 read 恒空（N 拍/历时，双阈值判死…）`——与「对端不可达（patrol）」可区分）；并订正 Q-G 注记的「`n==0` 非判死路径加**地板睡眠**防该形态 100% CPU 热自旋」：该小片对恒可读 fd 是**电平触发立即返回、不构成等待**（2026-10-15 真机事故实证：恒约 7% 单核零日志自旋 107 分钟） | 事故修复（2026-10-15 晚：半死态误报重装引入僵死 fd，resource-ext.log 实测 0.0696–0.0719 窄带 ≈ 单核 7.1%）；判死前至多一次 ≤1s 满核窗口（一次性）、「慢形态」≤50s 判死时延（期间睡在 poll 里不烧 CPU）；健康包流与**静默片**零影响（静默片 = `Ok(Ready::default())` 不计拍——评审曾实测误计致空闲 fd ~50s 被判死，已修并加回归钉 `silent_poll_slice_is_not_a_beat`） | `crates/homeway-quic/src/tun.rs`（`StuckReadGuard`/`STUCK_READ_MAX`/`STUCK_DEAD_AFTER`/`poll_reports_event` + 单测）；口径真源 = tier `openspec/changes/event-driven-heal`（wg-native-transport「TUN fd 僵死形态的防自旋」+ design D4）；tier 验收 4.5；按「防自旋已闭环」理解的排障读者 |
| **2026-10-18（连接策略收口：拍间与服务会话恢复模型）** | **在用档快探拍间 250ms → 1s；岛内拍统一 1s；服务会话恢复改事件驱动一次性重建** | ①客户端 `FAST_GAP` = 1s（env `HOMEWAY_QUIC_PROBE_GAP` 值域仍 0..=1000ms）；`T_detect` 最坏 = 0.7s + 1.4s + 拍相位 ≤1s ≈ **3.1s**（≤3.5s 判据不变；250ms 拍时代实测 ~2.5s）。②`driver::TICK` = 1s（删 `TICK_IDLE`；在用/待机同拍，旧「连接在位 250ms」不再）。③`host_session` 退役自驱 `rebuild_loop` + `REBUILD_BACKOFF_MIN/MAX`（1→30s 指数退避）；改 `RebuildGate`（bit0 单飞 / bit1 边沿 pending / demand `REBUILD_MIN_GAP=30s`）+ 短命 worker：B 门（边沿）、服务拨号失败（`NoFace`/`Closed`）、巡检 `NoFace`/连续 3 拍超时（demand）三源触发一次全名册 `Cmd::Connect`；失败不自动重试、等下一次事件。④新行族：`一次性重建：起跑（源=%s，候选 %d 个）` / `…恢复（源=%s，胜出 %s %s，耗时 %dms）—— 会话回 ready` / `…失败（源=%s，原因=%s）—— 不自动重试，等下一次事件（拨号/巡检/B门）` / `…停止（源=%s，岛本体已不可用：%s）—— 会话保持 failed` / `…放弃（源=%s，岛已收回）` / `…panic（已兜住——释放单飞位，等下一次事件）`；`宿主会话不健康` 行改写为 `⚠️ 宿主会话不健康（%s）—— 岛内阶梯已尽力；置 failed 并触发一次性重建`；巡检无连接面新增 `巡检无连接面：触发一次性重建（岛内阶梯也在动作）`。**2026-10-18 真机复测补正**：ladder 内部仍产出 `链路重连失败（…）—— 交世代重建` / `世代重建（原因=…；连续重连失败 N）` 两行（此前「五行零产出」不成立）——本批改写为 `…—— 交一次性重连` / `B 门上报（原因=…；连续重连失败 N）`，旧串零产出（单测逐字钉住） | 用户指令（2026-10-18）：降低在用期 4Hz 探针/唤醒；把服务会话的「自驱重试循环」收敛到与隧道域相同的两点式模型（活连接岛内保活 / 死连接等事件触发一次性重建），消掉两套恢复语义并存的维护债 | `crates/homeway-quic/src/{tuning.rs,driver.rs,client/ladder.rs}`（拍间/拍长/单测）；`crates/homeway-core/src/facade/host_session.rs`（`RebuildGate`/`RebuildSource`/worker + 拨号/巡检触发 + 单测 `rebuild_gate_edges_bypass_demand_throttle` / `unhealthy_marks_failed_and_rebuild_worker_releases`）；tier 真源 `docs/agents/connection-lifecycle.md` §3/§7/§8 + `AGENTS.md` 时延行；**同批（2026-10-18 真机后）**：①L3 上行岛侧按 `TUN_BATCH_MAX=64` 抽干（只取已就绪，一批一次 housekeeping）；②`carrier=false` 时 TUN 读面短路丢包（不分配/不投岛，按 `DROP_REPORT_BATCH=64` 批量上报；需求回调照发）——两者都不改 wire/NAPI；`homeway-quic` 258 绿、`homeway-core` 762 绿、clippy 零告警 |
| **2026-10-18（服务会话「在用档」补开流信号）** | **「在用档」判据的 STREAM 面输入集**（**判据行零改**；行为面 = 恢复时延档位） | 服务流需求 = **仅写出推进**（`Cmd::StreamWrite` 回执 `n > 0`，字段 `last_stream_out_at`）→ **开流成功 ∪ 写出推进**（`Cmd::StreamOpen` 成功即置，字段更名 `last_stream_use_at`） | 真机复报（2026-10-18，会话列表页 loading 数十秒）：回前台后 carrier 被系统半死（`上行发送面错误（Other；Connection aborted (os error 103)）`），term LIST 的桥拨号**开流成功**但「对端先说话」的 `GREETING` 永不到 ⇒ 客户端全程 0 字节写出 ⇒ 阶梯恒待机档，页面每次 15s 读超时重试，直到 **60s 待机拍**才探活重连。两次真机复现：开流→检测滞后 **42–47s**；对照 10-13/14 同形态现场（有写出 ⇒ 在用档）检测仅 **2.1s** | `crates/homeway-quic/src/driver.rs`（`last_stream_use_at` + `Cmd::StreamOpen` 臂置位 + `in_use_now` 注释）；`crates/homeway-core/src/facade/host_session.rs` 模块头（信号名）；单测 `client::tests::stream_capacity_is_max_bidi_minus_two_at_design_defaults` 尾段口径改写（桩出口不 accept 双向流 ⇒ 对端不发 `MAX_STREAMS`；probe 流在场后「62 服务流 + probe + 控制 = 64」满额，旧「关一条立刻能再开」的断言只成立在 probe 未开时，改为断言自记账槽位即时回收）；tier 真源 `docs/agents/connection-lifecycle.md` §3 快探节拍 + 常量表、`docs/agents/debugging.md` 故障表 |

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
| 2026-10-08（Q-K） | **`fragDrop`**（intercept 计数；Q-B 起既有键） | 「入站分片被**整包丢弃**的片数（唯一来源 = F7 的 `return`）」→「被丢弃的分片包**总数**：非法（越界/非 8 倍/空片）+ 重叠冲突连带 + 超时连带 + 超限拒绝/淘汰」；恒等式 **`fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`**（**四项全部可独立读**，不需作差反推）；单位仍是**包**（向下兼容） | Q-K F1/F2：分片不再一律丢，`fragDrop` 需覆盖重组的各条失败路径 | `serve.status` intercept 段、`EngineInterceptBits`、`homeway-cli` 取值点；在「已能走通」的流量上数值**降为 0**（此前该流量根本走不通）；键序与 `snapshot()` 索引 `[8]` 不变 |
| 2026-10-08（Q-K） | **新增计数** `fragReasm` / `fragBad` / `fragOverlap` / `fragTimeout` / `fragLimit` / `txFragDrop`（`Stats::snapshot()` **追加末位**，数组 9 → 15） | 无 → 有 | Q-K F1/F2/F5-d：重组的成功 / 畸形 / 攻击 / 丢包 / 资源五类信号 + TX 侧无首片丢片，全部可观测 | additive：① `snapshot()` 索引 `[0..=8]` 与键查找语义不变；② 经 `serve.status` 载荷 `ServeInterceptBits` 暴露（6 键，serde `default` 兼容旧载荷）；**DC18 人读行文不变**（`daemon_cli` 仍只渲染 dialOk/dialFail/reject/flows）；③ **单位差异**：`fragReasm` = **报文数**（datagram），其余五者 = **分片包数**；④ `fragDrop` 是 **RX** 侧总数、`txFragDrop` 是 **TX** 侧总数（两面独立、不可混加） |
| 2026-10-08（Q-K） | **出口→客户端方向的线上包形态** + `tx_deferred`/`shape_drop`/`tx_win_released` 的输入集 | ① 出口栈对大 UDP 回复：1253–1472 载荷 ⇒ 分成 2 片发出，但**返向实际不可用**（首片 UDP 校验和被 `on_tx` 写坏 ⇒ 客户端静默丢）；≥1473 载荷 ⇒ 出口 TX **静默丢弃**（分片缓冲 1500 字节钳制）。② **`on_tx` 分片感知 + `fragmentation-buffer-size-65536`** 之后：1253–1472 正常发 2 片、**≥1473 现在真的分片发出**（**1 报文变 N 包**）；③ 按包计数的面（`tx_win_released`/`tx_win_pumps` 等）随之变化：1 报文 ≠ 1 包 | Q-K F5-a + F5-d：恢复返向大 UDP（对齐 Go），并修复设计门查出的校验和破坏缺陷 | 出口整形面（`TX_DEFER_MAX_BYTES` 按字节，口径不变；`tx_win_released` 按包，数值变大）、`tx_deferred` 深度（同字节量下包数变多）；**客户端入站重组并发 = 8**（`reassembly-buffer-count-8`，F5-c；修前为 1）⇒ 并发大回复不再互斥；客户端侧 `udp_send` 对 >65507 载荷新回 `ConnErr::DatagramTooLarge`（`(1253, 65507]` 不报错）；新增计数 `txFragDrop`（F5-d 的无首片丢片） |
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

| 2026-10-10（**M5 S4 落地**） | **E22** `dns: q=… qtcp=… resp=…` | 入口链换 DATAGRAM：`resp` 的入口由「WG 解密包 → 拦截层」→ **「QUIC DATAGRAM → 拦截层」**（计数语义与判定不变）；`upstream=` 解析面不变 | M5 单承载（设计 §8.1 的「计数输入集专段」点名行） | E22 数值读者；`server/dnsproxy` + 岛 DATAGRAM 入口 |
| 2026-10-10（同上） | **`udpNoReply` / `udpDrop` / `shapeDrop` / `txFragDrop`**（intercept 四计数） | 原「WG 上行门」形态退役：`udpNoReply` 的 WG 侧来源（服务腿/核自连 TCP-over-UDP 会话）消失 ⇒ **只余过境 UDP 与 DNS 腿**；四计数**行文与键名零改**（`Stats::snapshot()` 索引语义不变） | WG 侧 UDP 会话面退役（服务面走 QUIC STREAM） | E12/udpcap 观测、`serve.status` intercept 段 |
| 2026-10-10（同上） | **C5/C6**（赛跑结算 / 路径确立） | 输入集：WG 全候选镜像赛跑 → **岛赛跑**（候选 = token 端点；「镜像包」概念消失，结算字段面 = 候选/完成/未完成）；**行文已在主表改为岛侧原 C5'/C6' 升主行** | 岛承接赛跑（M1 已登记；M5 删 WG 侧同义行） | C5/C6 数值（排障脚本按新行文）、`tools/quic-island-e2e.sh` 读数 |
| 2026-10-10（同上） | **DC18** `intercept：dialOk=N dialFail=N reject=N flows=N` | 输入集再收一次：`dialOk` 不再含**豁免腿**（files/term/speedtest 的 UDS 转投）——M5 后这三条腿不存在；保留面 = 过境 TCP + DNS 腿（后者不计 dialok，口径注记同下）。`dialFail` 不再含「隧道 IP 未登记端口 → 回环拨号」形态（该径改落**过境**，拨的是 `tunnel_ip:port` 本身） | 豁免面退役（设计 §3.2） | DC18 数值、`serve.status` intercept 段、`tools/matrix.sh` 的相关断言 |
| 2026-10-10（同上） | **E10–E12 + E23 的「WG 档」限定语失效** | 原：`transport.localErr*` / `E10-E12` 等行带「WG 档不动/WG 档照旧」限定语（`:888/891/899/900/907`）→ **限定语随 WG 档不存在而失效**：M5 后只有单承载，脚本/文档凡按「WG 档 vs QUIC 档」分档读数的断言**按单档读** | 单承载（设计 §4.2） | 排障脚本的分档分支；`docs/reviews/M1–M4.md`（历史不追改，读法以本表为准） |
| 2026-10-10（同上） | **`transport.localErr*` 两键恒缺席** | 无 → **恒缺席**（`demand.localErrAgree`/`localErrAll` 不再出现；旧读方读到缺键 = `None`） | WG bind 的全候选发送统计退役（岛无该面） | `tunStatusJSON.demand` 键面（additive 兼容）、App 读键面（tier 侧无需改） |
| 2026-10-11（测试面精简批） | **证据指针改名（判据行本身零变化——登记留痕）** | 本表历史条目引用的证据脚本/用例名 `tools/quic-wg-e2e.sh` → **`tools/quic-single-bearer-e2e.sh`**；测试目标 `crates/homeway-core/tests/quic_wg_e2e.rs` → **`quic_single_bearer_e2e.rs`**（`#[ignore]` 用例名与判据断言逐字不变；环境契约 `HOMEWAY_WG_E2E_TOKEN`/`_EXIT_LOG` → `HOMEWAY_SB_E2E_TOKEN`/`_EXIT_LOG`）；WG 残留门 ③⑤⑨ 与 NEG_ASSERT 白名单同名同步 | 旧名 `wg` 是 WG 退役前的历史叙事（M5 C4 改写后内容早已是单承载 QUIC-only 判据）——`tools/check-wg-removed.sh` ⑤ 的 `_wg` 哨兵白名单长期靠「文件名豁免」立足，改名为消除误导留痕 | 历史条目里的旧名指针按本条对应到新名（文档不逐条追改）；判据行（E-q1/E-q2/E3/C2 等）字节零变化，对齐验收口径不变 |
| 2026-10-12（DNS-STREAM 承接批） | **E22 `qtcp` 输入集 + 出口服务流计数四键** | ① `qtcp` 输入集：{隧道栈内 `:5300` TCP 面 + 拦截层 TCP 腿（`DnsRoute::TcpFlow`，两者都走 `submit_tcp`；代码门 r34 L2 补全既有两面）} → **{同上 ∪ `STREAM[tag=6]` 流腿（`answer_stream`）}**；计入口径 = **收到即计**（在途门**之前**，与 `submit_tcp` 同）——**畸形（len<12）计入**（随后 `malformed` +1 且不回帧）、`len=0` **不计**（读帧后即收线）、在途门/入队丢弃**计入**（同时 `drop` +1）。② 出口服务流计数五键（`streams_open`/`stream_refused`/`stream_bytes_in`/`stream_bytes_out`/`streams_closed`）输入集：{tag 1/2/3/4/**5**（probe 回显任务同样自增受理/字节/收线计数，`exit/serve.rs` 的 `echo`；代码门 r34 L2 补全）} → **{∪ tag=6}**（`service_capacity` 的每连接流额度与「受理时点」语义不变） | DNS-STREAM 批：`STREAM[tag=6]`（dns-resolve）新增——代答复用同一 dnsproxy 管线（CA5 的 `qtcp` 增长判据面从栈内 TCP 面迁到流腿面） | E22 数值读者（`dns:` 行/`serve status`）、出口服务流计数读者（`quic` 段 28 键）；**行文零改**（E22/E-q5/C19 模板不动，值域扩展走「判据变更记录」2026-10-12 条）；单测 = `server::dnsproxy::tests::answer_stream_counts_qtcp_before_gate_and_gates` / `server::dns_stream::tests::stream_serves_frames_closes_on_len0_and_keeps_big_answers` |
| 2026-10-13（WIN 批） | **CLI 调用名 `homeway-cli` → `homeway`（判据行输出串零变化——登记留痕）** | 二进制/产物名：`homeway-cli` → **`homeway`**（不带后缀；`crates/homeway-cli` 包名与 `-p homeway-cli` 构建契约不动）；版本横幅 `homeway-cli <ver>` → **`homeway <ver>`**；用法/可行动文案里的命令形（如「执行 `homeway serve restart`」「重新接入：homeway term attach …」）随调用名同步。**一切判据行的输出串（E\*/DC\*/C\* 行文）逐字不变**——历史条目与正文里的 `homeway-cli` 调用形一律按 `homeway` 读（文档不逐条追改，读法以本条为准） | 用户指令（2026-10-13）：发布二进制（本地 + CI）统一叫 `homeway`，不带后缀 | 调用形态记载处（本文件历史条目/实采节、README、tools/\*.sh 的二进制路径与进程匹配形、release 产物名 `homeway-<tag>-<平台>.tar.gz`）；`crates/homeway-cli/tests/qh_config_failfast.rs` 的 `CARGO_BIN_EXE_` 键名随 bin 名走 |
| 2026-10-13（直连优先改裁） | **出口侧每源闸 / `flood_refused` / `retry_sent` 的单轮握手尝试数** | 单轮 = 名册全部候选都发起握手 → **窗口形态下可少于名册**（直连在窗口内胜出 ⇒ 中继候选**不发起**握手；直连快失败 ⇒ 提前解锁，尝试数同改裁前；直连胜者产生时未完成直连照旧 abort = 照旧计失败） | 直连优先（窗口内只投直连）——M2 S4 的「3 候选同址赛跑单轮消耗 (N−1)=2 次失败预算」**最坏面上界不变**（名册仍 ≤N），实际消耗只降不升 | 出口侧 `flood_refused`/`retry_sent`/每源闸读数（`exit/{admit,mod}.rs` **零改**）；`tools/matrix.sh` 的闸相关读数 |
| 2026-10-13（term 清屏修复） | **raw 腿 DATA 流新增「几何真变清屏序」**（无既有判据行——登记留痕 + 新行为面立册） | 会话几何真变（apply_size）时 raw 腿只透传 PTY 重画输出 → **上报几何 ≠ 新会话几何的 raw 腿先收一条 DATA(`\x1b[2J\x1b[H`) 再收重画**（上报几何 == 新几何的腿含触发腿**不收**；不含 `3J`——回滚条是历史不是残影；与 attach「换屏前序」`\x1b[3J\x1b[2J\x1b[H` 同族不同串） | 修复型变更：多 attach 异宽收窄后，宽腿窄宽外残留上一帧画面（PTY 内程序按新宽重画只覆盖新宽度，本地更宽的腿无人清屏——Go 基线同缺口） | raw 腿 DATA 消费方（CLI `term attach` 本地直写 stdout、tier rawCapable 腿）；单测 = `term::service::tests::{shrink_clears_wider_raw_leg, own_resize_does_not_clear}`；wire 帧格式与回放/环内容零变化（清屏序走控制帧通道、不进环——后续 attach 的回放不含它） |
| 2026-10-13（hover 不搬几何） | **raw 腿输入活动面收窄（尺寸同步契约「输入」定义）**——spec「窗口尺寸同步与重绘」的活动 = 接入/上报尺寸/输入 | 活动输入集：一切上行 DATA 帧 → **整帧只含 SGR 无键鼠标移动序列（`CSI < 35 ; col ; row M`，`?1003` hover 流）的 DATA 帧不记活动**（不搬几何、不动 active/主题/剪贴板；字节照常透传 PTY——程序 hover 高亮不受影响）。判定保守：键盘/带键拖动（`32|0/1/2`）/滚轮（64/65）/焦点序列（`?1004`）/X10 legacy 鼠标编码/跨帧截断半截序列/空帧 ⇒ 一律按真输入记活动（误判只会多记 = 现状行为，不破坏「一敲即回本机网格」）。surface 腿 INPUT 面不动（App 触摸无 hover 场景） | 用户观察（2026-10-13）：opencode 开 `?1003` 后鼠标**划过**窗口（不点击）即持续产生 hover 流，每次都把会话几何搬到该腿（最近活动者赢），两窗口异宽时纯浏览即反复搬几何 + 对面腿反复清屏重排——把无点击移动从「使用」中摘出 | 尺寸同步契约读者（FIX-26 固有代价注记的适用面收窄：交替**输入**仍每次重排，交替**悬停**不再重排）；单测 = `term::service::tests::{pure_mouse_motion_boundaries, mouse_motion_does_not_steal_geometry}`；Go 基线无此过滤（基线同场景同样被 hover 搬几何——超出基线的改进行为，对齐口径以本条为准） |
| 2026-10-13（应答不搬几何） | **raw 腿输入活动面再收窄：终端应答（同上「输入」定义条目的续条）** | 活动输入集（承接上条）：{一切上行 DATA 帧 − hover 流} → **再减：整帧只含终端应答序列的 DATA 帧**——本地终端对程序查询（DA1/DA2/OSC 10/11/DCS/CPR/kitty 探测/DECRPM/modifyOtherKeys）的自动回投。认的形态：OSC（`ESC ] … BEL|ST`）/DCS（`ESC P … ST`）/CSI（参数 `?`/`>` 开头 + final ∈ {c,n,q,R,u,y,m}，或纯数字 + final ∈ {R,n}；`m` 只认 `?`/`>` 开头形——输入侧鼠标释放带 `<` 前缀，无冲突）。保守面：键盘、kitty 键盘**输入**编码（`CSI <code>;<mods>u` 纯数字参数）、方向/功能键、鼠标（final M/m）、截断半截、空帧 ⇒ 按真输入记活动 | 修复型变更（反馈环）：双 raw 腿下程序发查询 → 透传双腿 → 两个本地终端各自应答 → 应答作为 DATA 上行各自记活动 → 几何随应答到达顺序在两腿间反复搬 + TUI 重画再发新查询——用户实测 opencode 在窄腿打字后 UI 宽↔窄连跳多次（单测确定性复现：`reply_feedback_does_not_flip_geometry` 修前红） | 尺寸同步契约读者（「输入」= 用户操作，自动回投不算）；单测 = `term::service::tests::{pure_terminal_reply_boundaries, reply_feedback_does_not_flip_geometry}`；Go 基线无此过滤（基线同场景同样成环——超出基线的改进行为） |
| 2026-10-14（focus-out 不搬几何 + 几何切换观测行） | **raw 腿输入活动面三收窄：焦点失去（续条）+ 新增运维观测行** | 活动输入集（承接上条）：{… − 终端应答} → **再减：整帧只含焦点失去事件（`CSI O`，`?1004`）的 DATA 帧**。焦点**获得**（`CSI I`）仍算输入（点它 = 用它）。另新增几何切换观测行（非判据行——debug 级日志）：`term: 会话 <名> 几何切换 → <WxH>（腿 <key> 输入 <n>B: <hex前48>）` / `（… RESIZE 上报）` / `（… 接入）`——三条入径（DATA/RESIZE/attach）全覆盖，多腿异宽排查时日志直接指认搬几何者 | 修复型变更（焦点乱序环）：macOS 焦点切换 = 旧窗口 focus-out + 新窗口 focus-in，两条腿分别上行、到达顺序不保证——focus-out 记活动时乱序会把几何翻回用户刚离开的窗口（切窗口打字时 UI 再跳一次）。观测行：应答/hover 修复后用户仍报跳变，需字节级证据定位 | 尺寸同步契约读者；单测 = `term::service::tests::late_focus_out_does_not_flip_geometry`（+ pure_focus_out 边界并入 pure_terminal_reply_boundaries）；日志读者 = `<state>/cache/debug.log` 的 `几何切换` 行 |
| 2026-10-14（XTWINOPS 应答收尾） | **终端应答 final 集补 `t`（前「应答不搬几何」条目的续修）** | CSI 应答 final ∈ {c,n,q,R,u,y,m} → **{… + t}**（纯数字参数 + `?`/`>` 开头两形都认）——`t` = XTWINOPS 窗口操作应答（像素尺寸 `CSI 4;<h>;<w>t` / 字符尺寸 `CSI 8;<h>;<w>t` / 位置 `CSI 3;<x>;<y>t`）。输入侧无 final t 的键编码（方向键 A-H、~ 家族、鼠标 M/m、kitty u——均不含 t） | 观测行实证收口：hover/应答/focus-out 三修后用户仍报「窄窗打字 UI 先窄后宽反复跳」；`几何切换` 日志指认 = 双窗口本地终端各自回的像素尺寸应答 `\x1b[4;353;797t`（宽）/`\x1b[4;353;580t`（窄）——TUI 每次重画发像素查询，`t` 漏网使每敲一键几何被两条应答各搬一次。此为该振荡链的最后一环 | 尺寸同步契约读者；单测 = `pure_terminal_reply_boundaries` 补 4 条 t 形（日志实证字节原样入测）；观测行本身不变 |
| 2026-10-14（清屏跳环尾） | **「几何真变清屏序」条目的续修：清屏腿同时跳环尾** | 清屏帧（控制帧通道、优先发）之后，raw 写者继续按环游标发**清屏前积压**的旧几何画面字节——旧画面在清屏后被重新画上（残留复发，积压大时肉眼可见）。修复：清屏 enqueue 的同一锁内把该腿 `skip_to` 抬到当前环尾；`peek_ring_chunk` 先于追赶判断应用低水位——清屏后直接接 resize 之后的新输出。同锁互斥面：pump 写环持同一把锁（线性化 ✓）；**登记残余**：pump 锁外 read 的在途旧字节 ≤32KiB 可落入 skip 位之后（闪现一瞬，通常被 TUI 全屏重画覆盖） | 用户复测（2026-10-14）：四环（hover/应答/focus-out/像素应答）修后宽窗内容变窄后**仍有残留**——机制 = 清屏帧与环积压的通道竞态（修前单测确定性复现：清屏后涌出 23943 字节积压旧画面） | raw 腿实时流语义（清屏后不重放旧几何画面）；单测 = `clear_screen_skips_backlog`（全速相制造积压 + TRAPWINCH 标记新纪元 + 清屏与标记间旧字节计数；修前红 23943 > 100）；Go 基线无此处理（基线同场景残留同样复发——超出基线的改进行为） |
| 2026-10-14（应答让位收窄到 active 腿） | **终端应答的路由语义：从「透传全部」改为「只透传 active 腿应答、丢弃非 active 腿的纯应答帧」**（前「应答不搬几何」条目的路由面增强——用户拍板落地） | 多 raw 腿在场：程序的每次查询 → 全部腿的本地终端各自应答、**全部透传 PTY** → **只透传 active 腿的**；非 active 腿的纯应答帧（判定命中 `pure_terminal_reply`）不写 PTY（计数 `replies_dropped` + 节流日志 `term: 会话 … 丢弃非活动腿应答 N 次（多腿应答竞态防护）`）。hover 流与 focus-out **不在丢弃面**（程序需要焦点/悬停事件）。已知代价：查询与应答间 active 切换时该轮查询程序侧超时（TUI 有降级，登记可接受）；粘贴纯应答形态文本且程序未开 bracketed paste 的理论误丢窗口（TUI 均开 bracketed paste，天然豁免） | 多路应答本身是歧义：程序的每次查询收到多个矛盾答案（两套颜色/像素尺寸/能力表，程序取哪个取决于到达顺序）——会话的「真终端」语义上只有 active 腿；且应答让位机制本就从「服务端代答」让位给 raw 腿，本次把让位对象从「会话级全部 raw 腿」收窄到「active raw 腿」 | 尺寸同步契约读者 + raw 腿应答路由；单测 = `reply_only_from_active_leg_reaches_pty`（程序 = stty raw + 后台周期 DA 查询 + 前台 perl 逐字节 hexdump 上行；修前 4 份/2 轮 → 修后 2 份/2 轮）；观测 = `replies_dropped` 计数与节流日志 |

| 2026-10-14（出口须显式启用 + 纯读不拉守护 + term 本机面判定） | **① `serve.enabled` 整节/文件缺省 true → false**（`DEFAULT_CONFIG_TOML` 模板 + `FileServe::default()`；手编 `[serve]` 节省略键仍 = true，Go 字段级同义不变）；**② CA11 拉起触发集收窄**：`default_host_ref`（`--host` 缺省寻址）在 `<state>/client/hosts.json` **不存在**时直接返回「还没有添加主机…」提示、**不拨/不拉守护**（文件在 ⇒ 仍经守护，主机表真源不变；CA11 的「守护进程未运行，已启动 pid=N」行在该前置分支不再出现）；**③ `term` 本机面成立条件**：`<state>/term.sock` 在 → **且 config `serve.enabled` = true**（config 缺失/坏 ⇒ 不走本机面，降级到默认主机规则） | 用户实测（2026-10-14，新客户端机）：第一次 `term list` 报「守护进程未运行…已启动」+「还没有添加主机」，第二次却列「当前没有会话」；`host add` 后 `term new` 建在了本地而非远端。机制 = 纯读命令按需拉起守护 → 守护按缺省 `enabled = true` 装配出口（生成身份、签发 token、建 `serve/term.sock`、监听 41641）→ 本机面优先遮蔽主机规则。三条合修：客户端机不再意外成为出口（安全面：凭空多出带有效 token 的监听出口）、新机器两次行为一致（实测：两次均提示加主机、无守护、state 目录未建）、`host add` 后转发到远端（实测：私有出口的会话表） | 出口机首装流程 **多一步** `homeway serve start`（写 enabled=true + 装配；零参 launchd/服务随后按它起）——三台现有机器 config 已显式 `enabled = true`，不受影响；`tools/local-rust-exit.sh` 用前台 `homeway serve` 形态不看 enabled，不受影响。单测 = `serve_cli::tests::strict_load_missing_is_default`（整节缺省 false / 省略键 true 两面）、`term_cli::tests::local_mode_rejects_timeout`（sock×enabled 四象限 + 主机表缺不拉守护的文案区分）；README「部署」段与 DEPLOY 注记同步 |
| 2026-10-14（默认端口换区间） | **出口 / 中继对外端口缺省**（`DEFAULT_LISTEN_PORT`、`relay::DEFAULT_LISTEN`、`DEFAULT_CONFIG_TOML` 模板、CLI 用法与错误示例） | 出口 `41641` → **`38241`**（QUIC 缺省仍 = listen+1 派生语义不变）；中继 `:41741` → **`:38341`**（4 处硬编码收成 `homeway_core::relay::DEFAULT_LISTEN` 单常量）。本机服务端口（files 7802 / term 7724 / speedtest 7803 / dns 5300）**不动**——隧道内 App 侧按固定值访问，改了破 App 兼容 | 用户指令：旧端口沿袭自 tailcat，同机并存两套时撞端口 | 只影响**新装机器**（已部署三机 config 显式写了 `listen = 41641` / `":41741"`，零影响）；判据行里的端口**样例值**（`serve 就绪：quic=:41641`、`中继：注册成功（腿 41641 → …:41741）` 等）是 config 值的回显——行文模板不变，新装机器的读数为 38241/38341；README 示例、`tools/local-exit.sh`/`local-relay.sh`（Go 基线件，保留其自身缺省）不在影响面 |
| 2026-10-14（CLI 输出梳理） | **CA11 拉起路径的 darwin 括注行** + **`--host` 缺省提示文案** + **socks `--host` 可选化**（用户指令：输出简洁、少术语、提示直接说怎么做） | ① CA11 非相关分支括注 `（未在册 launchd 代理提及 state=…——不等 launchd KeepAlive，直接拉起）` → **`（无相关自启动项——直接拉起守护）`**（仍 darwin-only；launchd 在册分支的两行与「守护进程未运行，已启动 pid=N（state=…）」不变）；② 0 台主机提示 `还没有添加主机——先在出口机上执行 \`homeway serve token\` 取 token，再在本机执行 \`homeway host add <token>\`（加过之后 \`homeway <cmd>\` 默认就用它）` → **`还没有添加主机。先在出口机上执行 \`homeway serve token\` 取凭证，再在本机执行 \`homeway host add <凭证>\`，之后 \`homeway <cmd>\` 直接用。`**（speedtest 的同类行 `主机表为空——先 homeway host add <token> 添加后端，再测速` → `还没有添加主机。先 \`homeway host add <凭证>\`，再测速。`）；③ `socks on/off` 的 `--host` 从必填改**可省**（缺省走 `default_host_ref`，与 files/term/forward 同口径——v0.3.5 统一 host 风格时漏了 socks）。全部 `--help`/用法串重写为「一行用途 + 人话选项」，删 M5/F0/Q-L/carve-out/Go flag/矩阵/手机口径等内部引用；`connect`/`token`/`portfwd` 标「调试用」；`--no-hints` 从 relay 帮助移除（仍接受） | 用户指令（2026-10-14）：系统梳理 CLI 输出 | 帮助/用法串非判据行；CA11 括注与 0 台提示为登记行故在此登记；测试 `term_cli::tests::local_mode_rejects_timeout` 断言子串「还没有添加主机」不变 |
| 2026-10-14（岛恢复接棒：R 全名册 + B 后世代重建 + 一次性操作超时） | **① 阶梯 R 的候选集**（§3.1 行文「同端点、同本地 socket」）**② B 门新行 + 宿主会话不健康行文改 + 世代重建行族（新）③ CLI term 一次性操作读帧超时文案（新）+ `stream_refused` 文案改** | ① R 候选 = 现任端点 → **现任优先 + 名册其余去重**（`driver::reconnect_candidates`；直连优先窗口照旧；赛跑行的「候选 %d 个」在重连时 = 名册规模而非恒 1）。② B 前新行 `quic: 世代重建前收口 —— 现任连接已关、%d 条在册服务流按 EOF 收（调用方按需重开）`（半死连接显式关 ⇒ 后续 `StreamOpen` 立刻 `ConnectionLost`）；宿主会话 `⚠️ 宿主会话不健康（%s）——岛内阶梯已尽力；本会话状态置 failed` → **`…置 failed，起世代重建`**（显式装定的第二形态 `⚠️ 宿主会话不健康（%s）` 合并为同一串）；新行族 `服务会话: 世代重建：重赛跑全名册（第 %d 次，候选 %d 个，退避 %v）` / `世代重建成功（第 %d 次，胜出 %s %v，耗时 %dms）—— 会话回 ready` / `世代重建失败（第 %d 次，%s）—— %v 后再试` / `世代重建停止：岛本体已不可用（%s）—— 会话保持 failed` / `收工等待重建线程超时（STOP_WAIT）——放行自退`；`host status` 的 reason 串 `岛不健康（%s）；自动重连中` / `岛不健康；自动重连中（第 %d 次失败）` / `岛不可用（%s）`；退避 1s×2 封顶 30s、成功后 state failed → **ready**（此前 failed 为终态）。③ CLI：`出口 %v 内未应答——主机可能刚断线（守护进程会自动重连），稍后重试；--timeout 可加大预算`（GREETING/LIST/CREATE/KILL/EXPLAIN 的应答读受 `--timeout` 预算，缺省 10s）；`与主机的流打开被拒（stream_refused）——主机离线、隧道未通或主机会话不可用；用 homeway host status <name> 核对会话与链路态` → `连不上主机（stream_refused）——主机离线或刚断线；断线后守护进程会自动重连，稍等几秒重试；\`homeway host status\` 看会话与链路态` | MacBook Pro 实测（2026-10-14 21:15）：Wi-Fi 瞬断（发送面 EHOSTUNREACH）→ R 单候选连败 16 次 → B → 宿主会话置 failed 终态、岛 dormant、半死连接未关 ⇒ 网络 10 s 后恢复但 7 分钟零重连，期间远程 term 操作开流成功零字节、CLI 永久挂（`docs/DEPLOY-RUST-EXIT.md` §13.9 补） | 判据读者：C18/C20/C21 行文不变（候选数值变）；`QUIC-ROADMAP.md` C4 节「未动面」销账；单测 = `client::tests::ladder_exhaustion_then_connect_rebuilds_and_r_uses_roster`（三合一：R 候选 2 个 / B 后开流 ConnectionLost / B 后 Connect 重建）、`driver::reconnect_candidates_tests`、`host_session::tests::{rebuild_backoff_doubles_and_caps, unhealthy_marks_failed_with_rebuilding_reason_and_spawns_once}`、`term_cli::tests::one_shot_read_times_out_instead_of_hanging`；**登记残余**：半死连接（发送面报错而 QUIC 未判死）的 B 路径在桩上不可复现（桩无黑洞旋钮），`drop_zombie_live` 只有真网实测证据 |
| 2026-10-14（macOS 文件夹预授权 serve.grant） | **控制面新 op `serve.grant`** + **新日志行族 `tcc:`** + **CLI 首次提示行 + `serve grant` 子命令**（macOS TCC 面；无既有判据行——立册） | 出口守护首次访问「文稿/桌面/下载」时 TCC 把 `open/read_dir` 阻塞到用户在屏幕点弹窗（远端像卡死）。新面：① `serve.grant`（控制面词表 + dispatcher 分流独立线程）：**守护进程自己**逐个 `read_dir` 三个受保护根（每目录等用户 120s，串行——弹窗一次一个），回执 `{unsupported, results:[{name,path,status}]}`，status ∈ granted/denied/pending；非 macOS 回 `unsupported:true`。日志行 `tcc: <名>（<路径>）→ <status>…`。② CLI `homeway serve grant`：守护未运行**不自动拉起**（CLI 拉起的守护是终端子进程，TCC 授权会记到终端名下——引导先起 launchd 守护）；结果渲染 + 全 granted 写 `<state>/cache/grant-hinted`。③ 首次提示（macOS、标记未在时打一次）：`serve token` 的 token 之后与 `serve start` 输出之后两行「出口首次访问『文稿 / 桌面 / 下载』会弹系统授权窗…现在执行 homeway serve grant 可预授权」。④ install.sh：macOS 且配了自启动后询问「要现在预授权吗」（`--grant yes|no`；非交互打提示行） | 用户实测（2026-10-14）：新二进制在 Mac 上跑 term/files 碰受保护目录弹「"homeway"想访问"文稿"文件夹中的文件」，远端命令像卡死 | 控制面 op 读者（词表 +1）；单测 = `vocab::serve_grant_roundtrip`、`tcc::{probe_grants_accessible_dir, probe_maps_error_to_denied, protected_roots_shape}`；**登记残余**：`pending`（弹窗等待）路径只在真机可验（桩无法伪造 TCC）；TCC 对 adhoc 二进制按路径+签名记录，**升级换二进制后可能需要重新授权**（Developer ID 签名为根治，另批）；可移动卷/网络卷（`/Volumes`）是另外的授权类别，不在 grant 面（提示文案交待） |
| 2026-10-15（CLI 简化：portfwd 直连形态删除） | **CLI 命令面：`homeway portfwd` 删除**（本地 127.0.0.1 监听 → 隧道直拨的直连调试缝） | 用户指令（简化命令体系：除 host add 外 --token 形态无用户价值）。**承载面的 portfwd（forward 命令 / `portfwd/err` 词表 / STREAM[dial]）零变化**——删的只是 CLI 直连入口；正式用法 = `homeway forward`。依赖盘点：无工具依赖（quic-pf-e2e 走 Rust 测试、check-vocab 登记的是 wire 词表）——与之对照，`connect --token` / `speedtest --token` / `files --token` / `token 变换` 被 matrix / perf-ab / qi-ab 实际使用，删除需连带改造三工具（另批） | 用户指令 | `qh_config_failfast::numeric_and_bool_flags_failfast` 的 portfwd 段改为「已删命令 = 未知命令 exit 2 + 可用命令表」断言 |
| 2026-10-15（tun-death-gate：传输不通与 TUN 真死分离；tier change 同名） | **① 健康位拆分 + running 单点** ② C18 族新行（B 门先重赛跑）③ tunStatusJSON 顶层三新键 ④ 空窗强制换源新行 ⑤ 常量三条** | ① `TunShared` 拆 `device_dead`（fd/panic/stop + 未知值归设备——单向，仅新世代清）与 `transport_healthy`（patrol 可翻回：`try_recover_transport`——重赛跑证据不受最小间隔约束、巡检探测证据受 `TRANSPORT_RECOVER_MIN_GAP=30s`；写序仅清 patrol 不抹设备类文案）；`TunShared::running()` 单点 = `probe_running && !device_dead && Attached`——**patrol 不再把 running 拉零**（2026-10-15 00:23 事故的误杀链入口；capi 面与对账快照两处共用单点）。② B 门（连续 2 次 R 失败 + 窗 ≥10s）**先走统一重赛跑入口再上报**：`阶梯尽头重赛跑：胜出 直连 %s %v（耗时 %dms）——传输恢复，不拆接口（patrol 清除，证据=重赛跑，距上次判定 %v）` / 失败 `阶梯尽头重赛跑：未获对端证据（%s）——上报传输类不健康`；入口 = 宿主面（岛线程回调 spawn 线程）固定两连发 `Cmd::Rebind{local:None}`（换 socket——静默腐烂只有这步能解）→ `Cmd::Connect{全候选}`（`st.racing` 单飞；`RaceInFlight`=在飞非失败）；发起前查 `IslandSnapshot::ladder_inflight`（防打断在途确认探活）；三调用方（B 门/巡检续探/恢复下推）收敛同一入口；僵尸收口保持 B 门岛内既有行为（2026-10-14 修复不回退）。巡检 3 连败兜底**不再 return 退出**——幂等上报一次 + 需求门控续探（需求期 60s 节流/无需求期仅需求恢复沿或 10min 兜底再发起重赛跑）；patrol 态下 `tun_recover` 下推快探+复探失败后补走重赛跑；巡检探测成功翻回行 `传输恢复（patrol 清除，证据=探测，距上次判定 %v）`。④ 空窗（gap>2×待机节拍）后首次失败强制 M：`挂起空窗后快探失败 → 直接换源（不等发送面报错）`（静默腐烂无 errno 也换源；标志一拍性：快探成功清）。③ tunStatusJSON 顶层**恒在**三新键 `transportUnhealthy`/`deviceDead`（bool，从健康位映射）+ `serviceStreamsActive`（= IslandSnapshot.streams_active，探针流不计入；QuicIn 加同名字段为 Rust 输入面、**quic JSON 段键面不变**）；Go 向量对账测试对三新键剥除后比对（冻结产端无此键）。⑤ `RERACE_PATROL_THROTTLE=60s`/`RERACE_IDLE_FALLBACK=10min`/`TRANSPORT_RECOVER_MIN_GAP=30s`（硬编常量，无 env） | 2026-10-15 00:23 6HR 半死态事故：挂起风暴 → 岛快探连败（静默腐烂选 R 必败）→ B 门 patrol → 健康位一次为假永不翻转 → 扩展读 running()==0 误判「核心已退出」整套重建处决活核（旧接口计数器/优雅收尾/零 fd 错误三铁证）→ 重建落网络未就绪窗口留系统路由残局（详见 tier 仓 openspec/changes/tun-death-gate） | tier 侧消费：`clientCoreTunRunning` 返回值语义收窄（patrol 期恒 1——tier 侧消费者清单见该 change design §3.5）；`unhealthyReason` 值域不变、`{patrol,fd,panic,stop}` 语义细化为设备/传输分界。**证据**：`tun_shared::tests::{patrol_not_pull_running_zero, recover_keeps_device_class_why, recover_patrol_with_min_gap}`、`ladder::tests::{suspend_gap_forces_migrate_without_errno, no_gap_keeps_r_and_success_clears_flag}`、`tun_status::tests`（三新键进 base/full 键面断言 + Go 向量剥键对账）；真机判据行待 tier 侧联调实采 |
| 2026-10-16（event-driven-heal 评审批：自救轮次与让路三态） | **C18 族行文行为演进**：① B 门失败行的内层 `%s` 扩展新形态——`B 门重赛跑让路（在飞持续——非快照陈旧）——照失败上报并进自救（轮次吸收在飞态）`（r2-N1：让路不再静默丢弃上报；r4-1：让路先经 250ms 复读重跑一次 rerace——陈旧快照的让路由重跑的真实成败定论，仍在飞才落本形态。该形态**并非「未获对端证据」**——只是「恢复动作尚未定论」，读日志时按内层文案区分）② 让路行族两条：`重赛跑让路：阶梯/赛跑在飞（或快照陈旧）——本轮不发起，零副作用等待` / `重赛跑让路：候选赛跑已在飞——本入口合并等待`（后者 r2 起仅在 Rebind 前置守卫的 200ms 陈旧残余窗可达）③ 自救轮次行族（新）：`自救轮次未恢复（%s）—— 按节流等待下一轮` / `自救轮次：统一重赛跑恢复成功 —— 退出自救` / `自救轮次耗尽（3 轮失败）—— 上报宿主（熄灯门：扩展按本 episode 出站包记账处置）` ④ 传感观测行（改名）：`待发包下推：App 出站新鲜且接收静默 → 立即下推阶梯（不等巡检拍）` + `待发包下推 —— 岛内快探阶梯为准（§3.1；本拍不重复动作）` → 合并为 `传感观测：App 出站新鲜且接收静默（动作面在岛内快探阶梯，本拍不动作）`（r1-F14：不再用「下推」措辞冒充动作——世代层无动作）⑤ B 门失败直达自救：`阶梯尽头重赛跑：未获对端证据（…）——上报传输类不健康` 后**不再等巡检 3 连败**即进轮次（r1-F26；巡检兜底保留为幂等重入） | event-driven-heal 变更（5s 泵退役/自救轮次/熄灯门）+ 五轮外部评审（r1–r4 整改、r5 终审通过）：r1-F1 让路曾被当「恢复成功」短路轮次（真机停摆路径）；r2-N1 静默丢弃让路上报会带回 ~3min 空窗；r1-F14 世代层下推日志冒充动作 | tier 侧灯塌对账/熄灯门消费面（`rescueRoundsExhausted` 键）；单测 = `tun_shared::tests::{device_class_mark_disguised_as_recovery_rejected`（r3-M1a 钉：设备类 mark 改写分类槽后不得按恢复收口）`, `healthy_bit_is_true_before_first_mark_of_episode`（r4-1 不变量钉：B 门时刻健康位恒真——不得当在飞胜出证据）`}`；**登记残余**：B 门让路支路无行级单测（需真岛线程 harness）；真机判据行采自 da2f909（先于 r1–r4 整改），r4 让路新形态待下次实机验收重采（ACCEPT-2026-10-16 文件口径） |
| 2026-10-16（v3 需求订正：熄灯退役） | **核侧一行 + App 侧行族变更**：核 `自救轮次耗尽（3 轮失败）—— 上报宿主` 的括注 `（熄灯门：扩展按本 episode 出站包记账处置）` → `（扩展挂起等首包重武装）`（episode 记账已随 App 侧整链删除，括注如实化）。App 侧行族变更：①熄灯族停止产出——`熄灯（自救轮次耗尽）—— 拆系统 VPN、流量回到物理网络；重连 = 打开 App 或点按通知` 与 `熄灯完成（…）—— 零周期机器已停，等待用户事件重连` 随 HEAL_DARK/goDark 删除（历史日志读到即旧件）；②新呈现行（通知副标题档位，AppLog 面）：`呈现：⚠️ 链路异常：正在重连`（轮次在跑，原 r1-F8）与 `呈现：链路断开：有流量时自动重连`（耗尽挂起档——新）；③决策 holdoff 新值：`exhausted-pending`（轮次耗尽挂起）与 `budget-exhausted`（重建预算耗尽挂起）替换原 dark 动作 | 用户需求订正（2026-10-16）：「不重连」是时机优化不是终态——休眠/无请求不试；真实 VPN 流量/手机激活时再连；**拆 VPN = 进程死 = 无法被流量唤醒**（感知「后台静默死掉还要手动重开」）不可接受。VPN 生命周期只由用户控制；轮次耗尽挂起等首包重武装（核侧既有语义零改） | tier 侧消费面：HealPolicy v3（15 例）；核侧判据行一行括注变更（见本行第 2 格）；真机验证 = 杀出口→耗尽→VPN 保持→出口恢复→流量→自动重连（v3 验收记录） |
| 2026-10-16（v3.1 无感订正） | **App 侧呈现行退役**：①`呈现：⚠️ 链路异常：正在重连` / `呈现：链路断开：有流量时自动重连` / `呈现：⚠️ 链路异常：设备故障，正在重建` 三行随副标题分档机制整体删除（TunnelNotify 的 errorText 覆盖机制一并退役——`setErrorText`/`hasErrorText`/字段零外部消费者）；②出口告警后台支路的副标题文案 `⚠️ 链路失联：连不上出口` 不再写——改日志行 `[后台] …——（无感：通知不报警）`；③主机卡 linkHint 档与 StatusStore linkHint 键删除（卡面恒为最后已知快照） | 用户口径（2026-10-16）：自愈/挂起/重建**全过程用户无感**——通知副标题不承载「链路断开」类描述，恒为正常流量形态；如实呈现只在用户主动打开 App 时（failed 态失败页/浮层） | 通知副标题读者；观测面转日志（出口告警的后台行保留）；302/302 绿 |
| 2026-10-16（重建风暴三连环） | **App 侧行为常量**：①`REBUILD_SETTLE_MS=3s`（换网重绑失败的升级沉淀窗——holdoff 'net-settling'，窗内挂延时重入）②`REBUILD_MIN_INTERVAL_MS=15s`（整套重建节流——首建不拦）③`RECOVER_SURVIVE_MS=60s`（预算清零存活门：上一世代自 attach ≥60s 才清——attach 尾/HEAL_RESET/rebuild 成功尾三处） | 2026-10-16 13:02 真机事故（28 轮重建风暴/86s）：Wi-Fi→蜂窝切换瞬间重绑失败 2s 内升级重建 → 坏窗口创建的 TUN fd 系统级僵死（poll 恒可读/read 恒空 ~17 万拍/s）→ 每代秒死 + 确认即清预算 ⇒ 预算永不累积、每轮「第 1 次」⇒ 无人刹车；出口侧全程健康（每代 30-80ms 完成准入）。坑 66 | tier 侧自愈面（HealPolicy + TierVpnExtensionAbility）；单测 = `netSettleWindowDefersRebuildEscalation`；核侧零改 |
| 2026-10-16（v4：整套重建退役） | **App 侧行族与行为常量**：①`自愈重建（%s）第 N 次` / `重建节流：…` / `换网重绑未恢复（rc=…），改走整套重建` / `自愈重建完成（%s）` 停止产出（HEAL_REBUILD/rebuildTunnel/预算族 MAX_HEAL_ATTEMPTS·RECOVER_SURVIVE_MS·REBUILD_MIN_INTERVAL_MS·REBUILD_SETTLE_MS/decideAfterFailedRebind 删除——b55df80 的三连环随之退役）；②新 holdoff 值 `device-dead` / `iface-gone`（观测分支，无动作）；③新观测行：`设备死（fd/panic/stop 类，分类=%s）：整套重建已退役——不自动重建，等待用户重连。若此形态非重建/换网后自发出现，请导出日志上报（核侧「tun fd 僵死/写入失败」行是直接证据）` 与 `系统 VPN 接口消失（系统收回）：不自动重建（v4 退役）——等待用户重连`；④`换网重绑未恢复（rc=%d）——整套重建已退役，等核内阶梯与下一事件` | 13:02 风暴终局（用户拍板）：自发 TUN 死亡零实证 + 全部僵死 fd 都是重建自己造的（坑 66）+「VPN 开关必须用户触发」——重建在解一个只被自己制造的问题。核侧判据行零变化（fd 僵死/写入失败行保留 = 未来真自发的发现面） | tier 自愈面 298/298 绿；核零改（v0.3.9 不受影响） |
| 2026-10-16（term-attach-resilience） | **服务端行为常量（尺寸哨兵）**：attach 且几何未变路径的哨兵两跳从背靠背（0ms）→ **间隔 300ms**（`SENTINEL_GAP`，env `HOMEWAY_TERM_SENTINEL_GAP`〔毫秒〕可调；第一跳原位、第二跳延迟线程五重验让位）；几何真变路径（RESIZE/输入几何切换/end_leg 重选举/带几何应用的 attach）保持背靠背现状 | 背靠背双 SIGWINCH 被内核合并、opencode 类差分渲染 TUI 读到「尺寸未变」跳过重绘（真机实验：背靠背 0B / 间隔 300ms 全屏重绘 14732B；vim 无条件重绘不受影响）——attach 后画面不完整、手动 resize 才恢复 | `crates/homeway-core/src/term/service.rs`（sentinel_repaint 族）；e2e：回放窗无全帧时 re-attach 后 0.3s 内静态内容重现；service 单测 ×5 |
| 2026-10-16（term-attach-resilience） | **CLI 行为常量（attach 重连）**：新增远程面链路级断链自动重连——退避 `LINK_RETRY_BASE=0.5s` 起 ×2 封顶 `LINK_RETRY_CAP=5s`，无重试上限（用户信号/会话级错误为终点）；等待期孤立 Ctrl-C 字节视为退出（raw 下 cfmakeraw 关 ISIG，Ctrl-C 非信号） | 网络中断时 attach 进程直接退出、恢复后不自愈；服务端 self_reconnect 替换语义已备只差客户端重拨 | `crates/homeway-cli/src/term_cli.rs`（attach_session 循环）；真源常量注释已注记与既有三套退避（ROLE_BACKOFF/host 重建/files_server）的关系 |
| 2026-10-16（term-attach-resilience） | **CLI 行为常量（退出排空）**：新增 `DRAIN_ON_EXIT_MS=50`——所有退出路径在还原 termios 前统一一次 `poll(50ms) + tcflush(TCIFLUSH)` 排空排队输入（不区分形态） | 应答让位设计下查询直达真终端，断链/退出竞态里孤儿应答（DA1 `62;1;2;…c` 形态）漏给 shell 回显成数字乱码 | `crates/homeway-cli/src/term_cli.rs`（drain_terminal_input + AttachEnv::finish）；openpty 单测 ×3 |
| 2026-10-16（term-attach-resilience） | **新行为面立册（重连横幅）**：CLI 新串 `homeway: 与主机的连接断开，重连中…（Ctrl-C 退出）`（暗色、raw 模式 \r\n；恢复后服务端握手自带 3J/2J/H 清屏自愈）；本地面断链不重连（对端=出口进程本身，死亡=会话不在），走既有终局文案 | 无 Go 对应新行为面（先例：term 清屏修复的「登记留痕 + 新行为面立册」） | term CLI 用户可见面；e2e：杀 daemon 后 1s 横幅、12s 自动恢复 |
| 2026-10-16（term-attach-resilience） | **断链三态文案产出面收窄**：`TermReadErr::message`（Go remoteEndMessage 同串）的三态文案（closed/gone/conn）从「一切链路级终结」收窄为「终局面」（本地面断链 / 会话级错误后的终局打印）；远程面链路级终结改走「横幅+重连」不再产出该文案 | 重连循环接管了远程面链路终结的处置，文案保留给真正的终局路径 | term CLI 错误面；判定矩阵 link_failure_classify 真值表单测钉住 |
| 2026-10-17（reconnect-oneshot：重连收敛为两点式——事件触发一次性重连；tier change 同名） | **核侧重连机器整体换代**：①**新行族（一次性重连五行——四判据行 + panic 兜底行）**：`一次性重连：起跑（源=%s）`（run_round 入口落——worker 线程、含承接轮；评审 r1 P0-1 整改：原「并入 stats 行」的降级不可支撑 tasks 7.1 的「读面触发」验收——stats 的起跑=S 无源枚举）／`一次性重连：恢复（源=%s，证据=快探［，耗时 %dms］）`（证据 ∈ {快探, 重赛跑, 重赛跑（复读）, 恢复下推同步快探——后者在「传输恢复（patrol 清除…」证据字段面））／`一次性重连：失败（源=%s，原因=%s）——demand 源下次最早触发 %ds 后（边沿源随时）`／`一次性重连：让路（源=%s，在飞赛跑持续——5s 短窗退避）`／`一次性重连：panic（已兜住——pending 留字 %#x，释放恰一次；下一任意事件承接）`。②**stats 行扩展一段（不加 JSON 键）**：`stats: … ｜ 重连 入口=N 起跑=S 节流=K 载位假包=P 起跑失败=F`（原 `stats: fdReadBytes=%dB fdWriteBytes=%dB ｜ pf=%d/%d` 段不变——r3-R16 五计数：入口调用与实际起跑分离，「读面饿死 vs 无事件」由「P>0 而入口 N 停滞」可辨）。③**退役族（显式登记）**：自救轮次族（`自救轮次：统一重赛跑恢复成功 —— 退出自救`／`自救轮次未恢复（%s）—— 按节流等待下一轮`／`自救轮次耗尽（3 轮失败）—— 上报宿主…`——轮次状态机退役）、传感观测行（`传感观测：App 出站新鲜且接收静默…`——1s 拍线程退役）、轮次前活性门翻回行（`传输恢复（patrol 清除，证据=轮次前活性门…`）、巡检失败/恢复/空窗族（`对端巡检失败 %d/%d: %s`／`巡检失败被门控拦下…`／`需求恢复…巡检失败重新计入证据`／`巡检恢复：门控态结束`／`巡检空窗 %v（判为进程被挂起）→ 阶梯恢复`／`链路巡检退出…`／`链路巡检：App 回到前台…`／`对端连续 3 次巡检不可达…`——facade 巡检线程退役）、链路动作选 R 面（`岛上报不健康（patrol）—— …先走统一重赛跑（成功不上报）`→ 改写为 `岛上报不健康（patrol）—— 岛内快探阶梯已走完 M/R（B 门）：标记传输不健康并触发一次性重连`——B 门沿立即标记 + 触发统一入口（回调沿顺序 = 先标记、再触发、后落日志——评审 r1 P1-10）；同批改写 B 门前置行：`快探阶梯走完 M/R（连续重连失败 %d，原因=%s）—— 上报不健康（交世代重建）` → `…—— 上报传输不健康（交一次性重连）`（driver.rs Rebuild 臂——评审 r1 P1-11：世代重建已随 v4/v5 退役，「交世代重建」误导排障）；评审 r2 P2 同批改文 B 门僵尸收口行：`世代重建前收口 —— 现任连接已关、%d 条在册服务流按 EOF 收（调用方按需重开）` → `B 门僵尸收口 —— 现任连接已关、%d 条在册服务流按 EOF 收（交一次性重连；调用方按需重开）`（drop_zombie_live——v5 语义；2026-10-14 条目的历史行文照旧可查；评审 r2 B7 起有确定性单测 `b_gate_zombie_closure_marks_carrier_dead`——「桩上不可复现」的登记残余解除））、`阶梯尽头重赛跑：未获对端证据（%s）——上报传输类不健康`／`…——下推全链未恢复`（B 门重写后无此形态）、pusher 族（`homeway-demand-push` 线程整体退役）。④**link 行改观测节打**：`link: via=%s ep=%s rtt=%dms（新栈状态快照）`→`link: via=%s ep=%s rtt=%dms（观测快照）`——仅承载在位时由 stats 观测线程每 60s 打（at=最近一次成功探测；`恢复下推（%s）——同步快探：通过/失败（…）——移交一次性重连 worker` 为 recover 新归因行）。⑤**观测快探新行**：`观测快探失败 %d/3（%s；有出站包拍——计入证据）`／`观测快探连续 3 败（%s）—— 标记传输不健康（读面触发条件成立，事件触发重连）`。⑥**`rescueRoundsExhausted` 恒 false**（契约键保留——台账只增不减；tunStatusJSON 键集零变化）。⑦**行为常量**：`RECONNECT_MIN_GAP=30s`（demand 源限频；边沿源绕过）／`RECONNECT_YIELD_GAP=5s`（让路短窗）／观测连败门槛 3（独立 60s 出站包拍计数）——`RESCUE_ROUNDS_MAX`/`RESCUE_ROUND_GAP`/`RERACE_YIELD_BACKOFF`/`TRANSPORT_RECOVER_MIN_GAP`/`PUSHER_*` 常量族删除 | 2026-10-17 6HR 事故（挂起后 8.5h 无自愈）：恢复正确性押在「1s 拍决策线程必须按时拿到 CPU」被系统性打破（进程其余线程照常、决策线程零动作）；两点式模型 = 活连接岛内保活 / 死连接等事件触发一次性重连（六源：读面/服务流/换网/唤醒/回前台/B 门共用单飞槽位；跨线程协调只有原子位与原子时间戳——无锁状态机、无常驻决策拍线程） | tier 侧消费：`clientCoreTunRecover` rc 表（0/-1/-2/-9——见 tier change design D5）、`Index.d.ts` 注释面、看门狗/HealPolicy 订正；单测 = `tun_exec::tests` 的 reconnect-oneshot 反例映射表全绿（r8-a1-2/r8-a1-3/r9-C1/C2/C4/C5/C6/C7/C12/r10-C8/C9/C10/C11/r13-P2/P3/P4、r8-B2、r4-N1——tasks 1.3 映射表全量 18 行 barrier/钩子受控交错；评审 r1 整改追加：armed ② 真达顶部检查、r9-C5 在飞形态、r9-C12 同 bit 并发注入、takeover/panic 精确轮数、tun_stop×在飞 worker、steady 五门、判据行真断言〔起跑/让路/失败〕；评审 r2 整改追加：恢复行逐字断言 `criteria_recovery_line_verbatim`、B3 在飞设备死零恢复行 ×2、stats Busy/Timeout typed 门、tun_shared 跨代受控交错 ×2〔旧 finish×新 begin / strong_recover×mark_transport〕）+ `tun::tests` 读面触发 4 例 + `tun_shared::tests` 临界区 3 例；真机判据行待 7.x 验收实采 |
| 2026-10-18（测速可观测性批） | **下行归因行（非 E 编号；`speedtest: 归因 #<id> 下行逐秒MB=[…] 尾<N>片(<span>s)=<mbps>Mbps`）** | 逐秒片时长恒按 `<1s` 记录（`.min(1.0)`），行尾无窗口真相 → **片时长如实记录**（写阻塞时如实显示 `24.0/2s`）+ 行尾追加 ` 实际窗=<elapsed>s`，越界 >500ms 时追加 `（窗口越界 +<x>s）` | 修复型变更：`pump_data` 的截止只在两次 `write_all` 之间检查，链路拥塞时 10s 窗口实测被拖到 21–23s（2026-10-18 20:45:44 生产出口实测），旧实现把 2s 片标成 `/1s` ⇒ 逐秒序列与尾速率系统性失真 | 出口排障面（`grep 'speedtest: 归因'`）；tier `docs/agents/log-index.md`；**行前缀与 `role=recv` 结算行零改**（会话计数不受影响） |
| 2026-10-18（测速可观测性批） | **客户端引擎完成行（`speedtest: 完成（down=… up=… 用量=… 用时=…s）`）** | 尾部无相位时长 → 追加 ` 下行用=<x>s 上行用=<y>s`（窗口锚点→下行 join / 上行相位起→join） | 诊断型变更：窗口越界或客户端读侧被拖慢时，合计「用时」看不出是哪个相位被拖长（与上一条配对的客户端侧证据） | C 族验收（服务会话/CLI 判据）、tier log-index；`down=`/`up=`/`用量=` 三段逐字不变 |
| 2026-10-18（测速可观测性批） | **新增逐流判据行（客户端引擎）**：`speedtest: 逐流下行（#<i> 窗内=<B> 窗后=<B> 服务端=<B> 预热=<B>；…）` / `speedtest: 逐流上行（#<i> 泵出=<B> 服务端=<B> 墙钟=<ms>；…）` | 无（additive 新行） | 合计值掩盖两种完全不同的形态：`2 流快 2 流饿死`（流控/调度）与 `客户端读取被拖慢`（窗内 ≪ 服务端）；2026-10-18 真机坏轮就是前者（0.5/0.65MB/s vs 6–8MB/s） | tier log-index；本轮无既有断言依赖（纯新增） |
| 2026-10-18（测速可观测性批） | **App 形态引擎日志落盘面**（`speedtest: 开跑/逐流/对账/完成` 的出现面） | App 形态（`ClientCoreSpeedTestStart`）的 `logf` 是空实现 ⇒ 上述判据行**永不落盘**（只有 CLI/守护 stdout 可见）→ `SpeedTestStart` 新增 `log` 参数（App 侧传隧道日志 `core-tun.log`），行形态 = `[ts] tier-core: speedtest: …` | 诊断缺口：App 进程核无 stdout；2026-10-18「读数抖动」时客户端侧零证据，只能靠出口日志反推 | tier `core/Index.d.ts`（params 注释）、`SpeedTestClient.ets`（传 `tunnelLogPath()`）、日志页/诊断报告（读 core-tun.log）；CLI/守护形态不受影响（`log` 空 = 不落盘） |
| 2026-10-19（files-delete：App 文件管理删除） | **files wire 动词面新增 `remove`**（additive；无既有判据行变更） | 六动词 `list/stat/mkdir/read/download/write` → **七动词**（+ `remove`）：文件与符号链接走 `remove_file`（**删链接本身、不跟随目标**）、目录走 `remove_dir_all`（递归）；服务根本身拒删（`invalid_arg`）；父目录仍按根内沙箱逐分量复核，叶子按字面拼接后 `symlink_metadata` 判型；错误码复用既有词表 | App 文件管理长按菜单新增「删除」（tier change `files-delete`）；出口旧版（v0.3.10）对 `remove` 回 `invalid_arg`「未知操作」，App 侧给「升级出口」可行动文案 | `crates/homeway-core/src/files_server.rs`（分派 + `remove`）、`facade/files_op.rs`（`op_remove`）、`files.rs`（动词面注释）；单测 `remove_verb_semantics` + 两处 symlink 逃逸负例扩入 `remove`；**`ver` 不变**（问候帧语义未变）；`fixtures/` 无 files 夹具 ⇒ 无字节夹具变更；CLI 同批扩 `homeway files remove|rm <路径>`（本地面与 `--host` 远程面同实现，仅助记/输出） |
| 2026-10-19（term attach 退出归还终端模式） | **raw 腿退出路径新增本地终端模式重置**（无既有判据行；行为面立册） | 退出时只还原 termios（cfmakeraw 的逆操作）→ **追加输入类模式重置串**再还原：鼠标上报（9/1000/1001/1002/1003/1005/1006/1007/1015）、焦点 1004、括号粘贴 2004、同步输出 2026、DECCKM ?1、application keypad（?66 + `ESC >`）、kitty 键盘（`CSI = 0 u`）、modifyOtherKeys（`CSI > 4 ; 0 m`）、备用屏（47/1047/1049）、光标可见 ?25h、SGR 归零。顺序 = 关模式 → 50ms 排空 → 还原 termios；stdout 非 tty 不写 | 用户实测（2026-10-19）：出口服务退出、attach 打出「出口服务正在退出，会话 … 已结束」后，鼠标移动持续在本地 shell 刷 `ESC[<…M` 乱码——raw 腿是字节透传，远端程序开的鼠标模式从没人关（与 term-attach-resilience D3 的「查询应答漏给 shell」同族：退出归还面不完整） | `crates/homeway-cli/src/term_cli.rs`（`LOCAL_TERM_RESET` / `reset_local_terminal` / `AttachEnv::finish` / 两条早退路径）；单测 `local_term_reset_covers_input_modes`；pty 实测两形态（Ctrl-b d 分离 / 杀出口 →「出口服务正在退出，会话 … 已结束」）输出尾均含完整重置串；**非 tty 与 App surface 腿不受影响** |
| 2026-10-20（恢复面评审整改） | **替换风暴熔断行文 + 宿主会话恢复族新行** | ① 替换风暴熔断：`替换风暴熔断中 —— 阶梯休眠（原因=%s），等换网/唤醒/回前台或新连接采纳` → `替换风暴熔断中 —— 阶梯休眠（原因=%s），%v 后自动恢复周期探活（或新连接采纳）`（`Ladder::park_storm` 新增冷却截止 `storm_until`；`due` 到期自行复位）；② 新增/改写：`一次性重建：让路（源=%s，岛内阶梯 R 在赛中）—— 等岛内结果/下一次事件`、`宿主会话：岛内阶梯采纳新连接 —— 状态由 failed 回 ready`、`暖机硬失败：无连接面（出口不可达）——仍启动巡检，由巡检事件重连`、`替换交接 —— 旧连接上的 %d 条服务流按 EOF 收`、`注册刷新失败 —— %d 条在册服务流按 EOF 收` | 修复型变更（2026-10-20 两仓评审）：风暴冷却到期后阶梯永久休眠；服务会话岛内恢复不回写状态；交接/刷新失败路径漏清服务流 | C18/C20/C21 与 tunnel 域行文零改；读者 = 出口/核日志排障（tier `log-index.md` 重新生成）；单测 = `client::ladder::tests::storm_park_rearms_after_cooldown`、`driver::storm_replay_tests`、`homeway-core::facade::host_session::{rebuild_gate_edges_bypass_demand_throttle, unhealthy_marks_failed_and_rebuild_worker_releases}` |
| 2026-10-20（v0.3.13：观测腿抗代理污染） | **E20 的 STUN 观测行 `%s` 语义 + DoH 新行族** | ① `STUN：监听 socket（本地 %s）在 %s 眼里是 %v` 的第二个 `%s`：整串候选配置 → **胜出的单个候选**；② 新增 `DoH：%s 查询失败（%v）…`、`DoH：%s 应答 %s 全部落在污染段（fake-IP/私网）——丢弃`、`DoH：%s 应答 %s 无 A/AAAA 记录（NODATA）`、`DoH：候选串有 %d 项无法解析…`、`STUN：%s（%v）无应答——换下一个候选`、`STUN：%s 观测到 %v 落污染段（fake-IP/私网）——丢弃`；③ `公网端点：暂不公布（UPnP=%v STUN=%v…）` 形态不变 | 外网手机落中继排障：系统解析器被 fake-IP 污染使 STUN 整条腿作废——STUN 换国内多候选 + 域名经 DoH（钉物理网卡 TCP）解析，解析/观测结果过污染闸；判据行变更随 v0.3.13（tag fbfd421）+ 评审整改（ee4a42d 及后续未发布批次） | 读者 = 出口/核日志排障（tier `log-index.md` 已重生成）；验收方 = `server::doh::tests`（含 reader 级 read_http_response 用例）+ 真网 ignore 用例 `doh_resolves_for_real_on_pinned_iface`；文档 = `docs/DEPLOY-RUST-EXIT.md` §配置示例、tier `docs/agents/exits.md` §6 |
| 2026-10-20（登记项：Windows 钉卡运行面） | **非判据行变更——验证登记** | `egress::pin_socket_to_iface` 的 Windows 臂：`IPV6_UNICAST_IF` 值从网络序改**主机序**（WireGuard windows 参考口径；v4 仍网络序）；该函数文档从「UDP socket」放宽为「socket（UDP/TCP）」 | r2 评审 #2：DoH 的 TCP 钉卡接到该路径，发现存量 v6 字节序缺陷；**Windows 上 TCP 钉卡与 v6 字节序修正均无真机运行面证据**（CI 只 cargo check）——待 Windows + TUN/fake-IP 环境实测 v4/v6 DoH 各一后销项 | 影响面 = Windows 出口的 DoH 解析腿（当前两台生产出口为 darwin/linux，不受影响）；登记于 `egress.rs` 函数文档与本条 |
| 2026-10-21（r5 全项整改） | **E20/E20a/STUN6 的预算、地址族及端口语义** | DoH 的外层 deadline → TLS 底层每次 IO 前后共享 3s 绝对期限（50ms 取消轮询）；公网推断无整轮上界 → 60s；首轮 token 等探测完成 → 最晚 15s 进入安全兜底；STUN6 mapped 字面量/错误族回包可提前结束 → 非 mapped IPv6 才采纳，否则继续后备；IPv6 公布端口取 v4 UPnP 外口 → 真实 QUIC 监听口 | F1/F3 及既存相邻问题；不改 wire 字节或客户端恢复档位 | 保留 `STUN6：%s（%v）无应答——换下一个候选` / `STUN6：%s 观测到 %v 落污染段——丢弃`；新增 `STUN6：%s 观测到 %v 地址族不符——换下一个候选`。E20a `IPv6 路径可用（STUN 看到 …），公布 …` 行文不变，端口数值更正。测试与限制见 r5 整改记录 |
| 同上 | **观测停止/缩租/中继端口 owner** | 停止位调用前采样、detached 观测及 relay 线程 → 发布门保护 cache/token 提交，停止观察后等待线程退出，再缩租；UDP/TCP relay 线程由 owned worker 回收，所有外部 shutdown 调用等待同一完成结果 | F2/F4：防旧世代覆盖 token、独立 UDP dup 引发重启退让；调用方从观察日志回调重入仅请求停止，不能等待自己 | 正常注册/重试/保活节拍不变；不新增生产正常日志。UPnP SSDP 与 HTTP 每 50ms 可轮询取消；TCP connect 最长本次剩余预算且不超过 3s；DDNS 整拍共享 15s 并可取消 |
| 同上 | **DoH HTTP framing / 配置错误 / 系统 DNS 与 UPnP hostname** | chunked/无 CL 一律拒绝 → 支持 chunked（含 trailer）与 EOF 定界，头+编码体+trailer 合计≤64KiB，拒 framing 歧义；全坏显式配置回退系统 → 明确 fail-closed；显式空仍用系统解析，但调用方经唯一进程 worker + 队列容量1有界等待；UPnP getaddrinfo 同步无界 → 同 LOCATION hostname 可复用 SSDP 来源，其它 hostname 用有界共享解析 | 消除协议兼容与取消缺口，保留 TLS 根校验和空配置原语义；默认 DoH 运营方不变，未改为隐式系统回退 | 新增可见 `DoH 未配置，系统解析 %s 失败（%v）——无可用结果` / `DoH：%s 查询候选耗尽或总预算/停止门触发——无可用结果` / `⚠️ DoH：全部候选无效——不回退系统解析器（显式配置 fail-closed），请检查 serve.doh 配置`；底层 getaddrinfo 不可取消但进程最多一个解析 worker、一个排队请求，不持出口/发布权。Windows 真实钉卡证据仍按前项登记待补 |
| 同上 | **同步 TLS 与 QUIC 隔离源码门（非运行判据行）** | 三个同步 manifest 一律禁 rustls（前批同步 DoH 已使门失败）→ 唯一例外为 core 精确 `rustls.workspace=true` 与 `server/doh.rs` 消费；新增所有同步 Rust 文件的反向扫描，禁止例外扩散/async 关键字块/await/quinn/tokio；异步 panic 测试 std sleep → tokio sleep | 使门禁匹配已引入的同步 DoH 能力，保持 runtime 与 QUIC 状态在岛内；不是全 crate 豁免、不是跳过证书验证 | 其它十条门不放宽；同步 TLS 例外自校准覆盖异文件 rustls 拒绝、DoH 同步 TLS 放行、async fn/块拒绝及 await_quota 普通方法放行；原 M0 文件为历史设计，当前例外以本登记及 r5 整改记录为准 |


### 2026-10-21：中继准入、持续直连与候选学习整改

- 路径首胜判据由 `路径确立：…（首个完成握手）` 改为
  `路径确立：…（首个完成准入）`；增加 `赛跑：候选准入未成 → 保留后备继续尝试`。
  原因：TLS 首胜不代表可用路径；注册准入串行，每项至多 2s，整轮硬上限为原赛跑预算 +2s。
- 新增 `中继升级直连：已确认 …（同连接，服务流保持）` / `中继升级直连：本轮未确认，保留中继`。
  每 30s 最多 3 个候选，每项 700ms 共享 TLS-only 预筛与旧连接随机 nonce 验证期限；
  不触发新设备准入，支持跨族和初始单族。中继 hint 仅接受配置中继源及公网单播目标。
- `中继就绪` 的 `每源限速 … pps` 改为 `每源准入限速 … pps`：200pps 仅针对新建/控制，
  正式数据按每会话 16MiB/s 字节桶计；避免共享 NAT 与大包吞吐被控制面阈值误限。
- 新增 `中继：会话 #… 客户端准入已确认（正式配额 …/…）`，仅真实出口客户端准入后首次提升打印。
  新 UDP wire：`CLADM` 45B（sid+cookie+域分离 HMAC），`UDP_SESSION` 0x12/43B
  （SESSION 内容加域分离 HMAC）；独立 HMAC golden 见 `fixtures/relay-admission.json`。
  确认 ACK/通告丢包有界重发；LEGUP 仅证明出口，不占客户端正式额度。
  新中继要求新出口发 CLADM；部署顺序必须先出口后中继，旧出口配新中继仅有临时窗口。
- TCP 就绪行的 `0.0.0.0` 改为实际配置监听地址；显式地址不扩大监听，通配分别建 v4/v6-only，
  任一族冲突则整体 TCP 降级，握手全读写共享绝对期限。
- 候选表最多 64 项，文件 64KiB；hint/探测列表 300s、DNS 600s、已验证 7d，失败冷却
  2–64s。启动/巡检/恢复 DNS 异步解析，赛跑每 100ms 读取新候选；成功验证后才落已验证缓存。
  token 更新替换目录；认证带内广播生产者未实现。tier 连接生命周期文档同步为现行口径。

- 交叉复核收口：客户端准入证据额外绑定该 QUIC 连接首个 Initial 的 sid/cookie 代际，
  不按处理请求时的 remote 当前值推断。代际表最多 1024 项/20s，失去证据不提升正式额度。
  同代控制重放需同步替换发送 socket；首次 LEGUP 具绝对窗口内的有界重发。
- 首次纯域名解析继续走 Critical 配额，恢复走 Background；全局工作额度短暂占满时，
  首次解析在原固定预算内争取额度，不能把瞬时繁忙直接等同 token 没有端点。
  明文参照点探测每次 recv 重算剩余期限并核验实际目标源，垃圾应答不能延长 worker 寿命。

- 旁路地址列表采样与既有 patrol 同源，健康直连也触发；每轮最多 2 个有效直连候选公平轮转，
  共 250ms、按剩余目标分配期限，失败不计健康/恢复证据。Probe/Hint 学习统一拒绝私网、
  CGNAT、fake-IP、保留及 mapped 绕过，显式 token/DNS 内网候选不受此线索卫兵限制。
- 中继预池 256/每源 IP 4；LEGUP 后独立在途保护池 64/每 IP 2、仍受原 10s 绝对准入窗。
  新分配首包须有 QUIC v1/v2 Initial 形状；形状不代表认证，不可直接提升正式额度。
  有限资源池不承诺在饱和的有效 Initial 攻击下无限接入；正式会话隔离和资源上界始终保留。


### 2026-10-21：Linux PTY 背压写入取消

- 原实现把 SIGHUP 杀死子进程等同于解除 PTY master 的阻塞 `write_all`；Linux 实跑不成立。
  改为 master `O_NONBLOCK` + 短写/POLLOUT，新增输入取消检查粒度 50ms；正常输入不设总超时，
  不丢弃未结束会话的背压数据。kill/finish/收尸先取消输入，不依赖进程退出或跨线程 close fd。
- dup 共享 O_NONBLOCK，输出端同步以 POLLIN 等待保证原阻塞读取语义，HUP 时继续排空输出，
  Linux EIO 映射 EOF，空读不当 EOF；写端 HUP/ERR 立即返回失败，不空转。
- wire 字节、日志判据、ENDED 退出码（信号死=-1）、KILL_GRACE=500ms、NAPI 导出均不变。
  新回归是真 PTY 下取消背压写/正常退出/输出延迟与逐字节顺序，不修改原失败用例的通过判据。

### 2026-10-22：surface 接入与共享 VT 尺寸一致性

- 从「surface 接入/活动几何切换/RESIZE/退出重选举均额外注入尺寸哨兵」改为直接下发权威
  VT 快照、真实尺寸只应用目标值。旧要求使 shell 经人工中间列宽重画，并把历史按错误几何写坏。
- raw 催重绘哨兵保留，但每一跳都在服务锁内同步 PTY 与共享 VT，标记全部 surface 腿全量；
  快照与 FETCH-ROWS 几何取 VT 实际尺寸。注册表/ring 的基准不跟临时尺寸走，恢复仍按会话
  世代/存活/基准尺寸重验，真实 resize 优先；混合腿允许看到临时尺寸快照及历史 epoch 换代。
- 不按 Codex 名字删除正常主屏/inline 历史；旧会话已经写坏的历史不能据此逆向恢复。
  wire/surface/NAPI 与日志判据串不变。两道门与真实 PTY 红→绿证据见
  `docs/reviews/2026-10-22-term-surface-attach.md`；尚未发布/部署或在用户终端现场验收。
