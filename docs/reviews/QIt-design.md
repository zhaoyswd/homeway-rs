# Q-I 尾段（性能细节批 · 尾段）设计文档

> 批次：Q 批整改（`docs/REVIEW-ROADMAP.md` §Q-I 尾段；2026-10-08 「Q-E 之后」落点）。第 1 棒（设计）产出。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`「Q-I 性能细节批」节 + `docs/REVIEW-ROADMAP.md`「依赖与顺序建议」+
> 前段交接（`docs/reviews/QI.md` §6「残余与下一批靶点」）+ Q-E 交接（`docs/reviews/QE.md` §6.1「DNS 面 / files 面」）。
> 基线：`git HEAD = 713f963`（2026-10-08 13:11）。**行号均为复验时（本 HEAD）实测值**，实现以符号定位为准。
> 范围边界：只做「每拍固定税 + DNS 面 + files 面」；两台生产出口、`tier`/`homeway` 两只读仓、`baseline/`
> 全程不碰（测量只用 `tools/` 起的本地私有实例）。
> **状态：v2**——设计门（dsh 外部评审，轮次 `/tmp/dsh-review/r21.NQMZfM`，exit=0）已过，评审 9 组意见
> 逐条处置见 §5；v2 已并入全部高危/中危修订（F2 收益模型与判据、F2 形态改「快照直派」、F3 方案重写、
> F0 最小修法、测量计划五处、附录 A 登记口径）。

---

## 0. 复验方法与现状实测（证据先行）

### 0.1 复验方法

逐条回源码重定位（前段/Q-E 之后行号已漂），🔎 项按性能问题复核真伪；对前段代码门 M3 登记的第一靶点
做**独立复算**（不采信前段结论，直接读 `sample` 产物 + 回源码）。另跑**一次 HEAD 现状臂**（只读测量）
给本批靶点定量。引用 Go/tier 语义处一并核对只读真源。

### 0.2 本轮实测（HEAD `713f963`；构建二进制 sha256 `f65b4c52…`；2026-10-08 13:27）

**口径**：与 Q-I 前段 §4.4 同形，唯一差异 = 出口启动形态（见 §0.3）：

- 出口 = 本地私有实例 `serve --state /tmp/qit-exit9 --listen 42659 --bind-interface none
  --public-endpoint 127.0.0.1:42659`（**不带任何 stun flag**，见 §0.3 订正后结论）。
- 客户端 = 同二进制统一进程（state `/tmp/qit-client`，serve/relay 双关）+ token `--loopback-only` ⇒ 采纳 `127.0.0.1:42659`（lo0）。
- 负载 = `speedtest --down 15s --up 15s --streams 4`；测量期 `sample <exit_pid> 20`。
- **loadavg 表头**：轮首 `2.68/2.48/2.42`，轮末 `2.88/2.42/2.38`（1/5/15 分钟；1Hz 采样落盘）。
- 结果：`down 89.25MB/s（≈714Mbps，rtt 2ms）/ up 126.38MB/s`，`via=direct`，wall 34.1s；
  出口进程累计 CPU `0:00.06 → 0:41.62`（**41.56s/35s = 118.7%**）；**s/GB = 12.85**。
- 原始产物：`/tmp/qit-ab/{sample.txt,speedtest.json,cpu.txt,loadavg-1hz.tsv,exit.log,bin.sha256}`（不入库）。

**出口驱动线程（`homeway-serve-drv`，14248 采样点；20s 窗口跨 down 尾 + up 头——与前段同窗口形态）**：

| 叶帧（sample 调用链） | 样本 | 占线程 | 归属 |
|---|---|---|---|
| `reactor_turn → poll`（**每拍零超时 poll syscall，从不阻塞 ⇒ 采样点全为 CPU**） | **1787** | **12.54%** | **F2** |
| `driver_loop → poll`（引擎阻塞等待，1/5ms 档，**含等待时间**） | 849 | 5.96% | 见 §2 F2 收益模型 |
| `DnsFaces::service → __bzero`（**每拍 64KB 零初始化**） | **278** | **1.95%** | **F1** |
| `__bzero` 族线程合计（`__bzero` 278+41+6，另 `DYLD-STUB$$_platform_bzero` 5+1） | ≈331 | 2.32% | F1 占九成 |
| `flush_out → __sendto`（upstream 写） | 713 | 5.00% | 系统税 |
| `Socket::recv_from_with_flags → __recvfrom` | 1218 | 8.55% | 系统税 |
| boringtun chacha 族（encrypt/decrypt ×5 叶帧） | ≈5100 | ≈36% | 组件（非本批） |
| `RandomState::hash_one` + `SipHashRounds::write` | ≈439 | ≈3.1% | 观察（§6 登记） |
| **前段靶点残留复核**：`flush_backlog → memmove` / `consume_step → __bzero` / `service_sockets → __bzero` / `getenv` | **67（0.47%）/ 0 / 0 / 0** | — | **前段 F1–F4 在 HEAD 仍成立**（未回归） |

**reactor 剂量（出口 5s 观测行，`exit.log`）**：`pump=31519–86160/5s`（**6.3k–17.2k 拍/s**；
采样窗内 4 个 5s 窗 = 86160/34687/31519/43631 ⇒ 均值 **≈9.8k 拍/s**），`名下fd峰=4`，
`单拍峰=405–3729µs`（调度停顿，非本批靶点）。

**sample 口径注记（本轮新核，影响全部占比解读）**：`sample` 给**每个线程**的采样点数相同
（本轮 81 条线程全部 14248）⇒ 叶帧计数是「该帧出现在采样点的比例」，**不是**线程的 CPU 占比：
- timeout=0 的 `reactor_turn → poll` 帧从不阻塞 ⇒ 其 12.54% **是 CPU（内核态）**；
- 阻塞式 `driver_loop → poll` 帧含**等待时间**（847/849 样本里相当部分是 1/5ms 档的空等）
  ⇒ **两项不能相加当作「可省 CPU」**（§2 F2 收益模型按此订正）。

**判读**：

1. **两个「每拍固定税」在 HEAD 上仍是同线程第一、第四叶帧**（12.54% + 1.95%），且 6.3k–17.2k 拍/s
   的拍频说明二者按拍计价（poll：≈10k–17k syscall/s；64KB 零初始化：≈0.6–1.1GB/s @满拍频）。
2. 前段结论「驱动循环 = 事件驱动 + 每拍固定税」在本轮复现；`dnsface` 的 64KB **只在 `udp53` 存在时**
   发生（生产恒存在）——前段 M3「每调用 64KB」成立，A→B 倍率上升机制 = 拍数变多，**修它同时压住
   「拍数放大」那一半**。
3. **F2 的收益不能按 12.54% 整块计**：合并后每拍仍要**扫同一批 fd**（这是每个 poll 的固有工作），
   真正消失的是**一次 syscall 入口**；模型见 §2 F2（≈2.6–4.6 线程点）。

### 0.3 跨批发现（阻塞本批测量）：Q-H 取值纪律把 `--stun=`/`--stun6=` 空值拒了 ⇒ 本地 harness 全部起不来

**现象（本轮实测复现）**：

```
$ target/release/homeway-cli serve --state … --listen 45999 --bind-interface none --upnp=false --stun= …
--stun 空值（`=` 后为空）——取值不会吞下一个参数（值以 `-` 开头请用 --stun=<值> 形态）   # exit 2
$ tools/local-rust-exit.sh start 9      # ⇒ 「25s 内未见 serve 就绪」，实际是该行 exit 2
```

**归因（回源码）**：Q-H F2/F7 的取值纪律（`crates/homeway-cli/src/cli_flags.rs::take_value_or_exit`）
把「等号形/空格形空值」一律判 `Empty` → exit 2（carve-out 只有 `--recover-cause`）。但空串对若干 flag
是**有语义的值**：
- `--stun`/`--stun6`：`serve_cli.rs:530` 注释「`--stun6 '' = 关 v6 校验（Go flag 空串同义）」，
  消费点 `engine.rs:646/1481/1543` 全按 `is_empty()` 判「关」；Go 基线 flag 帮助文本亦写「空 = 关」
  （设计门评审独立核对 `baseline/homeway/internal/server/cli.go:36-37` 与 `explicit["stun"]` 分支）；
- **同类还有 `--relay=`（空 = 未配中继；config 模板默认 `relay = ""`）与 `--public-endpoint=`
  （空 = 不公布/走推断）**——作为 config 覆盖形态有意义（config 开了想用 CLI 关掉时无路可走），
  实测同样 rc=2（评审独立复现）。`--bind-interface=`/`--files-root=`/`--ddns=`/`--peer-ttl=` 亦 rc=2，
  但其空值本身无意义，不属缺陷类。

**定性**：**产品 CLI 缺陷（对 Go 文档化语义的回归）**，不是单纯工具过期。

**影响面（已核）**：`tools/local-rust-exit.sh:69`、`tools/perf-ab.sh:89/93`、`tools/matrix.sh:192`
（Rust 出口三处；Go 出口 `local-exit.sh:100`/`matrix.sh:202` 走 Go flag 包不受影响）——即 **R5 起的全部
Rust 本地出口 harness 在 Q-H 之后全部起不来**。本条**不是本批范围**（Q-H 面），但它**阻塞本批的一切
测量**，故立 **F0（前置修复）**，修法见 §2 F0（**评审订正：最小修法 = 直接去掉两个 flag，不写 config**），
并把「空值 carve-out 的归属」列为**需上报项**（§6.3）。

---

## 1. 复验结果表（逐条）

| # | 派单条目 | 真伪 | 现行源码位置（HEAD 713f963） | 结论 |
|---|---|---|---|---|
| **1** | `dnsface.rs` `DnsFaces::service` 每调用 64KB 零初始化（前段 M3「下一批第一靶点」+ Q-E 交接） | ✅ 成立（**实测 1.95%**） | `server/intercept/dnsface.rs:197`（`service`）；`:199` `let mut buf = [0u8; UDP_RX]`；`UDP_RX = 64*1024` `:36`；`service_dns` 每拍调用 `intercept/mod.rs:1880-1888`（`pump` 序 `:1555`） | 生产恒走该路径（`udp53` 恒在）；每拍一次 64KB 栈零初始化。**成立**；A/B 前段口径 0.46%→2.92%、本批 A 臂 1.95% 见 §0.2 |
| **2** | `reactor_turn` 零超时 `poll` 每拍 syscall（前段复测 12.2%→18.7%） | ✅ 成立（**实测 12.54%**，且为纯 CPU） | `intercept/mod.rs:1914`（`reactor_turn`）；兴趣集构建 `:1932-1943`；`unsafe libc::poll(..., 0)` `:1946`；每拍调用点 = `pump` `:1540` / `pump_hold` `:1580` | 每拍一次 `poll(4 fd, timeout=0)`；6.3k–17.2k 拍/s。**成立**；但**净收益 ≠ 该 12.54% 整块**（§2 F2 收益模型） |
| **3a** | 同物种残留：`dnsface.rs:228/333` 的 `Vec+drain` | ✅ 成立（冷路径） | `dnsface.rs:245`（`c.tx.drain(..w)` 续写）、`:295`（`c.rx.drain(..2+m.len())` 分帧）、`:350`（`deliver_tcp` 的 `c.tx.drain(..w)`）——前段行号 `:228/333` 已漂 | 与 F1（前段 `tx_backlog`→`VecDequeLite`）同物种：尾部整搬。**DNS-TCP 腿为冷路径** ⇒ 结构性收敛，**不计入收益口径** |
| **3b** | 同物种残留：`server/bind.rs:757` 每包 `Vec` | ✅ 成立（低位） | `server/bind.rs:747`（`send_wire_queued`）`:757` `Vec::with_capacity(wg.len()+8)` 每包一次（注释自估「39k 包/s ≈0.3% CPU」） | **成立**；回收环需跨线程所有权改造（发送线程消费后归还），风险 > 收益 ⇒ 本轮**登记不做**（§6） |
| **3c** | 同物种残留：`wtransport/bind.rs:287` 每包 `Vec` | ✅ 成立（低位） | `wtransport/bind.rs:280`（`send_wg`）`:287` per-send `Vec` | **手机核侧**（客户端出站编码），本地无判据面 ⇒ **登记不做**（§6） |
| **4** | 测量 harness 改进：轮末 loadavg 落盘 / RSS 合成多流 / en0 端点逐轮核实 | ✅ 成立（三条都真有缺口） | 前段 `QI.md` §6 登记；本轮复现缺口（loadavg 无时间戳/无轮末标记、en0 端点未逐轮核实、RSS 只跑 4 流常态） | 三条并入 **F7**（`tools/qi-ab.sh` + 配方），见 §2 |
| **5** | DNS TTL 缓存（原尾段范围头条） | ⚠️ **评估后不做**（见下） | 现状无缓存：`dnsproxy.rs:574`（`respond`）每查询直通 `forward`；`Upstreams::list` `:256-276` | **裁决：本轮不做，登记 + 设计骨架留档（附录 A）**。理由：① tier 需求真源把出口代答定义为 **raw forwarder**（`tier:openspec/specs/wg-native-dns/spec.md:5`，且 `:11` 的 MUST NOT 名单含「域名缓存」——字面主语是手机核，出口侧缓存仍属**产品语义新增/跨仓规格面**，偏离 = 需用户裁决的「已知不达标」，同 portfwd 先例）；② **缓存会削弱 tier 明文的「上游跟随主机解析（秒级）」可观测性**（命中窗口 ≤60s 内 resolv.conf 变更/代理开关不被跟随）——设计门评审补充的第三条理由；③ 本批**无可执行的端到端判据**（无带时延的隧道内 DNS 探针、上游查询数无观测面），与批次纪律冲突；④ 风险最高项 vs 不可证收益。**若主会话/用户裁决要做**：按附录 A 骨架另起小批（含判据登记清单）；**收口时须同步改 `REVIEW-ROADMAP.md` §Q-I 尾段文字**，否则账实不符 |
| **6** | DNS socket/缓冲复用（`dnsproxy.rs`：每查询新 socket / 64KB 全零 / 上游列表整表 clone） | ✅ 成立（量级小） | `dnsproxy.rs:668-669`（每查询 `UdpSocket::bind`+`connect`）；`:673` `vec![0u8; UDP_BUF]`（64KB/**每查询每腿**）；`:256-276` `Upstreams::list()` 每查询 `Vec<String>` clone | 「每查询新 socket」= **防投毒属性 + Go 同形 ⇒ 不做**（§6 登记）；「64KB 全零」+「整表 clone」= **F5 做**（结构性，量级估算 ≤0.05% CPU @100qps，不设墙钟判据） |
| **7** | files 客户端/服务端拷贝与分配（`files.rs:438-456`、`files_server.rs:390/499`，审计行号已漂） | ✅ 成立 | 客户端：`files.rs:434-454`（`read_frame`：`to_vec()` + `buf.drain(..4+n)`）、`:457-462`（`write_frame` alloc+copy）、`:392-396`（`write_all` 再 `to_vec()` 交 RPC）、`:327-342`（`read_line_opt` 共用同一 `buf`）；服务端：`files_server.rs:833-847`（`read_frame` 每帧 `vec![0u8; n]` 零填充）、`:828-831`（`write_frame` 两次 `write_all`）、`:613`（`download` 读缓冲**已循环外提**，无每帧分配） | 成立；**F6 做**（逐条方案见 §2）；「审计 `files_server.rs:390/499`」两处行号实为 `dispatch` 与 `resolve_in_root`（Q-E 改过），真实拷贝点以上表为准（**订正 1**） |
| **8** | Q-E 登记的 DNS 面残余（评估，可登记不做）：全死上游≈25.6qps / smoltcp rx 溢出无观测面 / UPnP 域名解析无可取消面 | ✅ 三条都成立 | `dnsproxy.rs:43`（`DEFAULT_WORKERS=64`，全死上游下 64/2.5s≈25.6qps）；`dnsface.rs:43-51`（rx 溢出 = smoltcp `net_trace!` 静默丢弃，无公共 API）；`upnp.rs`（`to_socket_addrs` 无可取消面，Q-E M3 已登记） | **三条均登记不做**（理由见 §6.2），实现期不扩面 |

### 1.2 误报 / 订正记录

- **无整条误报剔除**：8 组派单条目全部回源码复验成立（含 🔎 类）。
- **订正 1（行号/指针）**：条目 7 的审计行号 `files_server.rs:390/499` 落在 `dispatch` 与
  `resolve_in_root`（皆非拷贝点）；真实拷贝/分配点已按 HEAD 重定位（见上表）——设计门评审独立复核
  确认该订正准确。
- **订正 2（定性收紧）**：条目 3 的三处「同物种残留」都**不是**热路径收益项——`dnsface` 的
  `Vec+drain` 在 DNS-TCP 腿（冷），`bind.rs:757` 已有「回收环登记后续」注释（0.3% 量级），
  `wtransport/bind.rs:287` 在手机核侧（本地无判据）。逐条按「结构性 vs 收益项」分档，**不打包进收益口径**。
- **订正 3（口径）**：条目 1 的「A 0.46% → B 2.92%」是**前段两臂**的数；本轮 HEAD 单臂实测 **1.95%**。
  本设计一律以「本批 A 臂实测值」为基线口径。
- **订正 4（新增量级事实）**：本轮实测 **SipHash 族 ≈3.1% 驱动线程**（前段登记「散布各分支 ≤0.5%」）
  ——数值上调，仍**不做**（换 hasher 是全局决策 + 五元组键用户可达 ⇒ HashDoS 面，见 §6）。
- **订正 5（sample 口径）**：见 §0.2「sample 口径注记」——线程级叶帧占比 ≠ CPU 占比；阻塞帧与
  零超时帧不可混加（本批 F2 的收益模型已按此重算）。

---

## 2. 修复清单

> **实现顺序 = F0（解锁）→ F1 → F2 →（F2 止损闸门）→ F3 → F4 → F5 → F6 → F7**，每项一 commit
> （可独立回退）。共同约束：**wire 字节零变化、编号判据行零变化、不引入新 `unsafe`**
> （F2 沿用既有 `libc::poll` 调用点，不新增 unsafe 面）。

### F0（前置/阻塞）本地出口 harness 解锁：`--stun=` 空值被拒

- **现状**：§0.3。
- **方案（评审订正后的**最小**修法）**：三处工具**直接删掉 `--stun= --stun6=` 两个 token**——实测
  `serve --state … --bind-interface none --upnp=false --public-endpoint 127.0.0.1:P --verbose`
  **不带任何 stun flag 就能起**，且 `refresh_public_endpoint`（`engine.rs:1424-1441`）在
  `public_endpoint` 非空时**短路返回**（写文件 + 打 E20 行 + 打 token），**UPnP/STUN 推断根本不会被调用**
  ⇒ 与 `--stun=` 关的形态**等价**。（设计 v1 的「config 预置」虽然也能跑，但**没有必要**：
  一是 `nodestate.rs:271-273` 默认模板自带 `stun = "stun.cloudflare.com:3478"`，state 已有 config 时
  「仅在无 config.toml 时写入」会被跳过 ⇒ 等价性不成立；二是多引入一个「工具改用户的 state 目录」的面。）
  - **口径注记（本轮实测）**：该形态下出口日志仍会出现 `UDP 默认路径：… 通用 UDP（STUN:3478）…`
    一行——那是 **udpcap 的能力探测**（独立机制，Q-I 前段 F7 面），与 `--stun` 取值无关、两臂一致；
    本项消除的是**公网端点推断**（UPnP 映射 + 同 socket STUN 观测两条腿），不是全部对外探测。
- **涉及文件**：`tools/local-rust-exit.sh:69`、`tools/perf-ab.sh:89/93`、`tools/matrix.sh:192`
  （**工具面，非产品面**）。
- **风险**：极低（去掉两个 flag；已实测）。
- **测试计划**：`tools/local-rust-exit.sh wipe 9 && start 9` 起得来 + `status 9` 判据行可采；跑一次
  最小互操作冒烟（Rust 出口 + Go 客户端 `host add` ⇒「就绪（会话在位）」）；`perf-ab.sh`/`matrix.sh`
  各跑一次最小臂（若时间不允许，至少静态核对改后行文与 `serve` 用法一致）。
- **残余/上报**：`--stun ''`/`--relay=`/`--public-endpoint=` 等**空值覆盖形态**仍被 Q-H 拒（产品 CLI
  缺陷）⇒ **需上报项**（§6.3），本轮不动产品语义。
- **判据行影响**：无（工具面）。

### F1 `DnsFaces::service` 每拍 64KB 零初始化 → 缓冲提为字段（**第二批一号的一半**）

- **条目**：1（前段 M3 登记 + Q-E 明确交接）。
- **方案**：`DnsFaces` 加 `udp_rx: Vec<u8>`（`attach` 时 `vec![0u8; UDP_RX]`，一次分配），
  `service` 的 UDP 读循环改用 `sockets.get_mut::<UdpSocket>(h).recv_slice(&mut self.udp_rx[..])`。
  - **借用面**：`self.udp_rx` 与 `self.pending`/`self.next_tag`/`self.udp53` 是**不相交字段**；
    循环体内 `self.route_tag(...)`/`self.take_route(...)`（`&mut self` 方法）在 edition 2021 下
    **可编译**——设计门评审用等价最小样例 `rustc --edition 2021` 独立实测通过（NLL 对无 Drop 的
    `&mut [u8]` 临时量不延长到循环体）；本仓亦有同形先例（`intercept/mod.rs:2591-2634`）。
    若个别编译器版本拒绝，就地展开这两个 3 行小方法为字段访问即可（**不作为主方案**）。
  - 不作为：不改 `UDP_RX` 值、不改收包语义、不改 `pending` 登记序。
- **涉及文件**：`crates/homeway-core/src/server/intercept/dnsface.rs`（结构体 + `attach` + `service`）。
- **风险**：低。唯一语义风险 = 复用缓冲导致「上一包数据残尾」——`recv_slice` 只写 `0..n` 且下游一律
  `buf[..n]`/`to_vec()`，无别名面（驱动线程独占）。
- **测试计划**：① 现有 `dnsface` 单测（容量/长度域/续写）+ `intercept` DNS 全套单测；② 新增「同一拍多包
  内容独立」回归（连投两条不同载荷的 UDP 查询，断言各自应答正确）；③ lo0 臂 `sample`：
  `DnsFaces::service → __bzero` 叶帧 = 0。
- **预期收益**：`DnsFaces::service → __bzero` **278 样本（1.95%）→ 0**（实测口径 ≈19.5ms/s @本窗口；
  按满拍频 17.2k/s 的理论上界 ≈70ms/s，**以实测为准**）。
- **测量臂**：§4 的 lo0 判别臂（叶帧）+ en0 臂。
- **判据行影响**：无。

### F2 引擎 poll 吸收 reactor 兴趣集（消每拍零超时 poll syscall）——**本批一号**（收益模型见下，已订正）

- **条目**：2（前段复测 12.2%→18.7%；本轮 HEAD 实测 **12.54% 且为纯 CPU**）。
- **现状机制**：`driver_loop` 每拍先对 `[udp_fd, leg_fds…]` 做**阻塞** poll（1ms/5ms 档，`engine.rs:1079-1090`），
  随后 `intercept.pump()` 在 `reactor_turn` 里对**上游流 fd** 再做一次 `poll(timeout=0)`（`intercept/mod.rs:1946`）。
  两个 poll 服务**不相交**的 fd 集、都在同一拍、同一线程。

- **收益模型（v2 订正：不是 12.54% 整块）**
  - 合并后每拍只剩一次 poll，**但它仍要扫同一批 fd**（udp/腿 + 上游流 = 5–6 个 fd）——fd 扫描是每个
    poll 的固有工作，不因合并而消失；真正消失的是**一次 syscall 入口**。
  - 用本轮实测反推：`reactor_turn→poll` ≈1787 样本 / ≈196k 次调用 ≈ **9.1µs/次**（4 fd）；
    `driver_loop→poll` ≈849 / 196k ≈ **4.3µs/次**（1 fd，含等待时间沿不确定）。按「入口 + 每 fd 边际」
    线性模型反解：入口 ≈2.7µs、每 fd ≈1.6µs ⇒ 合并后（5 fd）≈10.7µs/拍 vs 现状合计 ≈13.4µs/拍
    ⇒ **净省 ≈2.7µs/拍 = 一次 syscall 入口**。
  - 折算线程样本：按采样窗平均拍频 ≈9.8k/s ⇒ ≈**2.6 点**；按峰值 17.2k/s ⇒ ≈**4.6 点**。
    ⇒ **F2 预期收益 ≈2.6–4.6 线程点（估算，待 A/B 取证）**，而非 12.54 点。
  - 附带的**第二通道**：合并后上游 fd 就绪会让阻塞 poll **立即返回**（旧形态最晚 = 一个超时拍 1/5ms）
    ⇒ 唤醒时延改善可能是端到端收益的真正来源（拍频与吞吐都可能在 B 臂上升）；**拍频变化列为必报量**。

- **方案（v2：评审推荐的「快照直派」形态，替代 v1 的「重建+比较+兜底」三件套）**：
  1. engine 侧（`driver_loop`）：把 reactor 兴趣集**并入本拍唯一的 poll**：
     ```rust
     let mut pollfds = Vec::with_capacity(1 + leg_fds.len() + intercept.reactor_fd_capacity());
     pollfds.push(pollfd{fd: udp_fd, events: POLLIN, revents: 0});
     for fd in &leg_fds { pollfds.push(...); }
     let leg_end = pollfds.len();                       // ← 腿切片右界（见下"腿消费收窄"）
     intercept.append_reactor_pollfds(&mut pollfds);    // 新 API：按 interests_for 规则 append + 记流号快照
     let reactor_off = leg_end;
     …poll(pollfds, poll_ms)…
     let ready = if n >= 0 { Some(&pollfds[reactor_off..]) } else { None };
     //  n>0 = 有就绪；n==0 = 超时（revents 全 0 ⇒ 快路径空派发，零 syscall）；
     //  n<0（EINTR/错误）⇒ revents 未定义 ⇒ None（走兜底 poll(0)）
     let tx = if hold { intercept.pump_hold_with(ready) } else { intercept.pump_with(ready) };
     ```
  2. intercept 侧 `reactor_turn(ext: Option<&[libc::pollfd]>)`：
     - `Some(ext)`（**快路径**）：**不重建兴趣集、不 poll**，直接按 append 时记下的 `(flow_id, fd, events)`
       快照与 `ext[i].revents` 派发；派发前逐条校验「该 flow 仍在表内 ∧ `io.fd == 快照 fd`」（fd 复用/流
       已拆 ⇒ 跳过该条）。就绪集是**提示不是契约**（既有注释 `intercept/mod.rs:1911-1913`）：窗口内新
       建/拆掉的流由下一拍（≤1/5ms）发现，行为等价于今天的「漏看下拍重报」。
     - `None`（**兜底/测试面**）：走**今天的原路径**（重建兴趣集 + `poll(…, 0)`）——`pump()`/`pump_hold()`
       保留为 `pump_with(None)` 薄壳，≈13 处测试调用点不动；引擎 poll 出错（`n<=0`）也走这条。
  3. **腿消费收窄**（评审点名，v1 漏）：`engine.rs:1097` 的 `pollfds[1..]` 必须改为 `pollfds[1..leg_end]`，
     否则 reactor fd 会被当腿传进 `bind.leg_readable`（今天无害但每拍白扫 O(L)）——并加断言/单测
     「reactor fd 不得进入腿消费路径」。
- **与已批设计的关系（评审点名，v1 漏）**：`docs/reviews/reactor-design.md:22-40` 的选型表把
  「引擎把拦截层 fd 并进主 pollfd 数组」列为**弃用项**，理由之一正是「每拍 2 次 poll … 是**本批的净增成本面**」。
  F2 正是该项——**新证据足以推翻旧论断**（该「净增成本」今天的实测值 = 12.54% 线程样本 + 6.3k–17.2k
  次/s 的重复 syscall），本设计**显式登记该反转**：旧表该行自本批起失效；旧表的另两栏仍成立——
  「API 面」由 `pump_with(None)` 薄壳保住、**「唤醒时延」由「最晚一个超时拍」改为「立即」**（行为变更，
  见上「第二通道」）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/mod.rs`（`reactor_turn` + 两个新 API + 容量访问器）、
  `crates/homeway-core/src/server/engine.rs`（pollfds 组装、腿切片、pump 调用）。
- **风险**：**中**。① 快照与派发之间流集变化 ⇒ 该条跳过、下一拍重报（非错数据：fd 全非阻塞 + 读/写路径
  各自复查门与 EAGAIN）；② append 与派发**必须共用 `interests_for` 单源**（禁止两处各写一套规则）；
  ③ **新建流从「本拍即可服务」变为「下一拍」（≤1/5ms）**——本设计**显式接受**该行为变更（记入 §3/§4）；
  ④ 引擎 poll fd 数 +≤N ⇒ 单拍成本略升（含在收益模型里）；⑤ 兴趣集每拍只建一次（快路径不重建）。
- **测试计划**：① 新增 `reactor_fastpath_dispatches_snapshot_revents`——建一条带数据的 flow，走
  「append → 手动 poll（模拟引擎）→ `pump_with(Some(revents))`」，断言数据在本拍被读走；
  ② 新增 `reactor_skips_stale_entries`——快照后拆掉该流（fd 复用同号新流）⇒ 断言不误派发；
  ③ 新增 `reactor_turn_none_falls_back_to_poll0`（`None` 路径语义与今天逐位等价）；
  ④ 新增「reactor fd 不进腿消费」断言；⑤ 现有 `intercept` 全套 + `pump`/`pump_hold` 语义测试全绿；
  ⑥ lo0 臂 `sample`：`reactor_turn → poll` 叶帧 ≈0 + `两项 poll 样本合计`对比 + `pump=/5s` 对比。
- **预期收益**：见上「收益模型」——**≈2.6–4.6 线程点**（一次 syscall 入口/拍）+ 唤醒时延改善（可能转为
  吞吐/拍频收益）。**止损闸门**：F2 落地后、继续 F3 之前，先跑一次短臂 sample——若「`reactor_turn→poll`
  \+ `driver_loop→poll` 样本合计」降幅 **<20%** ⇒ **回退 F2**（独立 commit），继续其余条目。
- **测量臂**：§4 的 lo0 判别臂（micro 三件套 + s/GB 参考）+ en0 臂。
- **判据行影响**：无编号判据行；`intercept: reactor 观测 …`（**非编号 additive 行**）加 `兜底=N` 字段
  ⇒ 按 Q-F/Q-G 先例在「判据变更记录」登记一行（行文变更、非编号）。

### F3 `dnsface::service_face` 每读尝试 8KB 零初始化 → 复用 `Interceptor.rx_scratch`（**v2 方案重写**）

- **条目**：1 的同函数邻位（本轮复验新增；前段 F3 只改了 `intercept` 的两处读循环）。
- **v1 错误订正（评审点名）**：v1 写「`service_face` 内改用 `self.rx_scratch`……借用面已核」——**不成立**：
  `service_face` 是 **`DnsFaces`** 的方法（`dnsface.rs:218`，`&mut self` = `&mut DnsFaces`），而
  `rx_scratch` 是 **`Interceptor`** 的字段（`intercept/mod.rs:837/914`），`DnsFaces` 没有该字段。
- **方案（按代价排序）**：
  1. **穿参（主方案）**：`DnsFaces::service(&mut self, dns, sockets, scratch: &mut [u8])`
     → `service_face(..., scratch)`；调用点 `service_dns`（`intercept/mod.rs:1880-1888`）改成
     `faces.service(&dns, &mut self.sockets, &mut self.rx_scratch[..])`——`self.dns_faces` 与
     `self.sockets`/`self.rx_scratch` 是**不相交字段**（现有代码已在用同样的拆分：`:1885` 就是
     `faces.service(&dns, &mut self.sockets)`），零新增分配、零新字段。
  2. 若穿参不便：退「`let mut chunk = [0u8; 8192]` 提到 `for h in handles` 循环外」——只消「每尝试」，
     不消「每拍每面」（`dnsface.rs:260` 现位于每连接×每 recv 尝试的循环体内）。
- **涉及文件**：`crates/homeway-core/src/server/intercept/dnsface.rs`（+ `intercept/mod.rs` 的 `service_dns`）。
- **风险**：低（纯缓冲来源替换；`read_gated` 软背压与 `CONN_BUF` 门不动）。
- **测试计划**：现有 DNS-TCP 面单测（分帧/续写/收线）全绿；lo0 臂 `service_face → __bzero` 叶帧 = 0
  （冷路径，常态无流量 ⇒ 判据以单测为主）。
- **预期收益**：**非每拍税**（仅 DNS-TCP 面有帧流量时按「每连接×每尝试」计价）⇒ 结构性收敛，
  **不计入收益口径**（v1 把它与 F1 并列为「同拍税形态」，措辞偏松，v2 收紧）。
- **判据行影响**：无。

### F4 dnsface `TcpConn.rx/tx`：`Vec+drain` → `VecDequeLite`（同物种收敛；最低优先级）

- **条目**：3a（前段代码门 L5 登记）。
- **方案**：`TcpConn.rx: Vec<u8>`/`tx: Vec<u8>`（`dnsface.rs:101/105`）换 `super::VecDequeLite`
  （前段 F1 的摊还实现，`intercept/mod.rs:151-195`）。**站点清单（评审订正：≈13 处，不是 5 处）**——
  `:230`（两处 `Vec::new()`）、`:242`/`:348`（`send_slice(&c.tx)` → `c.tx.remaining()`）、`:245`/`:350`
  （`drain(..w)` → `consume(w)`）、`:256`/`:337`（`tx.len()` → `remaining().len()`）、`:265`（`rx.len()`）、
  `:269`/`:344-345`（`extend_from_slice` → `push`）、`:295`（`rx.drain(..2+m)` → `consume`）、
  `:285`（`decode_tcp_frame(&c.rx)` → `c.rx.remaining()`）。`VecDequeLite` 无 `len()`/`drain()` ⇒ 漏改编译期暴露。
- **内存包络（评审补充，须登记）**：`VecDequeLite` 不变式 `len < 2*remaining` ⇒ 单连接 `rx` backing
  最坏从 `CONN_BUF=128KiB` 变 ≈256KiB（×`MAX_TCP_CONNS=64` ⇒ **最坏 +≈8MiB**）；常态远小于此（帧小、
  消费及时）。此项进 §4.3 判据④的说明。
- **涉及文件**：`crates/homeway-core/src/server/intercept/dnsface.rs`。
- **风险**：低（`consume` 有 `debug_assert` 兜超量；编译期强制改全）。
- **测试计划**：现有 `deliver_tcp_*` 单测 + 新增「部分写续传后帧边界不错位」（对照 `Vec` 参照实现）。
- **预期收益**：冷路径结构性（**形态收敛 > 收益**；顺带消「backing 随累计传输量增长」的同族隐患）。
  **不计入收益口径**。
- **判据行影响**：无。

### F5 dnsproxy 每查询分配：上游列表快照 + 上游读缓冲复用

- **条目**：6（`dnsproxy.rs:256` 整表 clone / `:673` 64KB 全零；**每查询新 socket 不做**）。
- **方案**：
  1. **上游列表快照**：`UpstreamsState.list` → `Arc<Vec<String>>`；`list()` 返回 `Arc<Vec<String>>`
     （clone = 引用计数）；列表变更时**换新 Arc**（不原地改）⇒ 在途查询读到一致快照。`text()`（E4 行）
     输出逐字不变。
  2. **上游读缓冲复用**：worker 线程自持 `Vec<u8>` scratch **（懒分配：首次用到才 `vec![0u8; UDP_BUF]`）**，
     穿参 `respond(&self, query, is_tcp, scratch: &mut Vec<u8>) → forward(...) → exchange(..., buf: &mut Vec<u8>)`；
     `exchange` 内 `conn.recv(&mut buf[..])`（交付前 `to_vec()` 保 owned 语义）。`answer_sync` 走同一
     worker 管线 ⇒ 自动受益。**热路径无短读风险**：`UDP_BUF = 64KiB` ≥ UDP 最大载荷。
- **涉及文件**：`crates/homeway-core/src/server/dnsproxy.rs`（**测试内 `core.respond` 调用点 4 处**
  `:985/991/1026/1040`，3 个测试函数）。
- **风险**：低（borrow 面单线程独占；`Arc` 快照防「读一半列表被换」）。
- **测试计划**：现有 `dnsproxy` 全套（fake 上游/兜底/期限/worker 并发/上游跟随）全绿 + 新增
  「上游列表变更后新查询用新表、在途查询不受影响」（fake 上游计数断言）。
- **预期收益**：**结构性**——**每查询每腿**省 1 次 64KB alloc+零初始化（≈2–5µs）与 1 次 `Vec<String>`
  深拷贝（≈100–300ns）；@100qps ≈ **0.02–0.05% CPU（估算）**，不设墙钟判据。**RSS**：懒分配 + 实触页
  少（64 worker × 64KiB = 4MiB 虚拟上界，常态 DNS 空闲 ⇒ 0）。
- **判据行影响**：无（E4 `upstream=` 摘要行文与语义不变）。

### F6 files 收发拷贝：服务端帧缓冲复用 + 客户端前缀偏移缓冲 + 上传单拷化

- **条目**：7（审计 `files.rs:438-456`/`files_server.rs:390/499`，行号订正见 §1.2）。
- **方案（三条，按收益/风险排序；服务端缓冲**必须每连接**——沿用 `receive_upload` 栈内持有，不跨连接共享）**：
  1. **服务端 `read_frame` 缓冲复用**（`files_server.rs:833-847`）：`receive_upload`（`:686-705`）每帧现调
     `read_frame` ⇒ 每帧 `vec![0u8; n]`（≤`MAX_CHUNK`=256KiB）**整段零填充**。改为循环外持有
     `frame_buf: Vec<u8>`，每帧 `if buf.len() < n { buf.resize(n, 0) }`（**只零填增量**；等长帧第二帧起
     零成本）后 `read_exact(&mut buf[..n])`。`read_frame` 拆出
     `read_frame_into(r, buf: &mut Vec<u8>) -> io::Result<Option<usize>>`——**`None` = 终止帧、
     `Some(n)` = 载荷在 `buf[..n]`**（语义写死；既有 `read_frame` 保留为测试壳或同步改测试）。
  2. **客户端 `self.buf` 偏移式消费**（`files.rs:434-454`）：**范围包含 `read_line_opt`（`:327-342`）**
     ——两者共用同一字段（`:329` `iter().position` / `:330` `drain(..=pos)` / `:340` `extend_from_slice`），
     统一改为偏移式（`off` 推进 + 摊还压缩，与 `VecDequeLite` 同形），消 `drain` 尾部整搬；载荷仍
     `to_vec()` 一次（所有权交给 `get` 的落盘写）。
  3. **客户端上传单拷化**（`files.rs:457-462` + `:385-417`）：`write_frame` 现为 `alloc frame + 拷 payload`，
     随后 `write_all` 又 `data[off..].to_vec()`（第 2 拷）交 RPC。改：新增 `write_all_owned(Vec<u8>)`
     ——首块**所有权直递**；**部分接纳 ⇒ `drain(..n)` 原地保尾部**（不新分配），零接纳 ⇒ 沿用既有
     `pending` 回带语义；Err ⇒ 与今天同路径上报。
- **涉及文件**：`crates/homeway-core/src/files.rs`、`crates/homeway-core/src/files_server.rs`。
- **风险**：低-中（见测试计划；帧格式不触）。
- **测试计划**：① 既有 files 全套（协议/水位/沙箱/响应行）；② **字节等价回归**：256MiB 随机文件
  `put` 后 `get` 回、与源文件 sha256 相同（覆盖跨帧/部分写/终止帧）；③ 上传「帧长交替
  （256KiB↔小帧↔256KiB）」断言缓冲复用下内容不错位；④ `read_frame_into` 单测（等长帧不重复零填）。
- **预期收益**（**估算**，需 §4 files 臂取证）：服务端省 ≈256KiB memset/帧；客户端省 ≈1 拷/帧 + drain 尾搬；
  @250MB/s（帧 256KiB、≈1000 帧/s）≈ 各端 1–3% CPU。**测不出 ⇒ 按「收益 < 带内噪声」如实记录**（前段先例）。
- **测量臂**：§4 的 files 臂（唯一主侧 = **CLI 进程侧**；出口侧 CPU 只作 F6.1 的辅证）。
- **判据行影响**：无。

### F7 测量 harness 改进（`tools/qi-ab.sh` 新增）

- **条目**：4（前段代码门 L4/L6 + §3.8 备注；本轮复验缺口仍在）。
- **方案**：新增 `tools/qi-ab.sh`（薄封装，只在本地私有实例上跑；**不碰生产**）：
  ```
  tools/qi-ab.sh speedtest <binA> <binB> [--rounds 3] [--streams 4] [--en0]
  tools/qi-ab.sh files     <binA> <binB> [--rounds 2] [--size 256M]
  tools/qi-ab.sh rss       <binA> <binB> [--flows 16]
  ```
  产物目录（默认 `/tmp/qi-ab/<ts>/`）：
  - `loadavg.tsv`：**带时间戳 + 轮首/轮末标记**（评审订正：只记数值不可按拍归因，无法判断越限发生在
    down 还是 up 段）；
  - `speedtest-<arm>-r<N>.json`、`cpu-<arm>.tsv`（`ps -o time=` 前后差）、`rss-<arm>.tsv`（1Hz max）、
    `sample-<arm>-r<last>.txt`、`endpoint-<arm>-r<N>.txt`（逐轮采纳端点：`lsof -nP -p <client_pid> -i UDP`
    远端地址 + `host list --json` 快照）、**`exit-<arm>.log`（出口 stdout 全量归档——reactor 剂量行
    可复核）**；
  - **A/B 臂切换** = 复制传入二进制到 `target/release/homeway-cli`（**`trap` 恢复原物**；同时给
    `local-rust-exit.sh` 加 `HOMEWAY_BIN` 覆盖，脚本优先走它）；脚本留痕两端 sha256。
  - **RSS 合成多流**：`--flows 16` 经隧道开 16 条慢消费 TCP 流（本地慢排空 server：读 1KiB 停 50ms），
    出口 1Hz RSS 取 max；**A/B 两臂都跑**（判据④是「B ≤ A×1.1」——只跑 B 销不了前段 L6 的账）。
- **风险**：低（工具面）；**纪律**：只起本地私有实例（state/端口 4265x 段），测毕全停并 `pgrep` 核对零残留。
- **测试计划**：跑一次 `speedtest` 三臂流程 + 一次 `files` 臂 + 一次 `rss` 臂，核对产物齐全、
  loadavg 时间戳齐、端点核实文件在、出口日志归档在。
- **判据行影响**：无。

---

## 3. 判据行与观测面影响汇总

| 项 | 编号判据行行文 | 计数输入集 / 数值语义 | 说明 |
|---|---|---|---|
| F0 | 无 | 无 | 工具面（删两个 flag token）。**上报项**：`--stun`/`--stun6`（及同类 `--relay=`/`--public-endpoint=`）的**空值覆盖形态**仍不可用 = 产品 CLI 缺陷；若裁决修 CLI，则是对 Q-H 已登记条目「空值一律 fail-fast」的局部回退 ⇒ **必须**在「判据变更记录」登记一行 |
| F1 | 无 | 无 | 内部缓冲；`pending` 登记序/容量语义不变 |
| F2 | 无 | 无 | **非编号行** `intercept: reactor 观测 pump=…/5s（均周期 …ms）名下fd峰=… 单拍峰=…µs` **加 `兜底=N` 字段** ⇒ 登记「行文变更（非编号 additive 行）」。**行为变更两条须登记**：① 上游就绪成为引擎唤醒源（唤醒时延 1/5ms → 0）；② 窗口内新建流从「本拍即可服务」→「下一拍（≤1/5ms）」。**不新增判据行** |
| F3 | 无 | 无 | 内部缓冲（穿参形态） |
| F4 | 无 | 无 | 内部缓冲；**内存包络注记**：DNS-TCP 单连接 rx backing 最坏 128KiB → ≈256KiB（×64 ⇒ ≤+8MiB） |
| F5 | 无 | 无 | E4 `upstream=` 摘要行文/语义不变（`text()` 输出逐字同） |
| F6 | 无 | 无 | files 协议帧字节不变（sha256 字节等价回归兜） |
| F7 | 无 | 无 | 工具面 |
| **不做：DNS TTL 缓存** | 若做 = 需登记 | 若做（按附录 A 的「命中同走后处理」形态）：`resp`/`trunc`/`aaaa-mixed` 的**输入集把命中腿纳入**（`aaaa-mixed` 数值上升、`resp` 数值上升、`trunc` 规则不变）、`fail`/`fallback` **数值下降**（命中不再走上游）、上游查询率下降；另加 hit/miss/evict **additive 行** | 本轮**不做** ⇒ 零影响；骨架与登记清单留档（附录 A） |

**结论：本批预期零编号判据行变更**；唯一行文变更是 F2 的**非编号 additive 行加字段**（按 Q-F/Q-G 先例登记）。
若实现期出现任何偏差（例如 F2 快照直派引入 dispatch 顺序变化、F6 字节等价回归失败），必须在本批 commit 内
按 `docs/INTEROP-CRITERIA.md`「判据变更记录」节登记，不得静默。

---

## 4. PERF-AB 测量与验收计划

### 4.1 纪律（沿用 Q-I 前段，强化三处）

- **安静环境**：判决只在 **1min loadavg ≤ 4** 时作数；**轮首 / 全轮（带时间戳）/ 轮末**三档 loadavg 落盘；
  任一轮跑中出现 loadavg > 6 即整轮作废（前段 §3.7 教训：跑中未落盘 = 判据不可复现）。
- **轮序（评审订正）**：改**平衡序** `A,B,B,A,A,B`（或 `B,A,A,B,B,A`），并按**轮配对**分析（每对 A/B 比值
  取中位）——前段记录过「B 恒当第二拍 ⇒ loadavg 系统性高 0.3–0.5」，固定 A→B 顺序会把系统漂移算进结论。
- **隔离**：只用本地私有实例（4265x/4267x 段、state `/tmp/qit-*`）；两台生产出口、tier/homeway 两仓、
  `baseline/` 全程不碰；测毕全停 + `pgrep` 核对零残留。
- **CPU 口径**：累计 `ps -o time=` 测前/测后差 ÷ 墙钟（**不用** `ps -o %cpu` 短窗值）；分母 = 出口进程
  **全部线程**合计；**主判据 = 驱动线程 `sample` 的「两项 poll 样本合计」**，s/GB 为参考量（下）。
- **产物**：`/tmp/qi-ab/<ts>/`（不入库，含出口 stdout 归档）；判据数字与结论固化进 `docs/PERF-AB.md` 新节
  \+ `docs/reviews/QIt.md`。

### 4.2 臂

| 臂 | 形态 | 采什么 | 用途 |
|---|---|---|---|
| **lo0 判别臂** | `tools/qi-ab.sh speedtest <A> <B>`（local exit #9 + token `--loopback-only` + 统一客户端 + `speedtest 15/15/4`） | **两项 poll 样本合计 + `pump=/5s` 拍频**、F1/F3 叶帧、吞吐 down/up、s/GB（参考）、进程累计 CPU、RSS max | **主判据** |
| **en0 产品形态臂** | 同上去 `--loopback-only`（采纳 192.168.3.x） | 同上 + 逐轮采纳端点核实 | 旁证（产品形态） |
| **files 臂** | `tools/qi-ab.sh files <A> <B>`（`files put`/`get` 256MiB，`--rate-limit 0`） | 墙钟（MB/s）、**CLI 进程 CPUδ（唯一主侧）**、出口 CPUδ（辅） | F6 专用 |
| **RSS 合成多流** | `tools/qi-ab.sh rss <A> <B> --flows 16`（慢消费） | 出口 RSS max（1Hz，两臂） | 判据④ + 前段 L6 销账 |
| **DNS 面（结构性）** | 单测 + lo0 臂叶帧 | `DnsFaces::service → __bzero`；F5 = alloc 站点消失 | F1/F4/F5 |

**en0 端点采纳规则（评审订正）**：token 含内网 + 回环两个候选，采纳由赛跑决定 ⇒ **预登记「采纳到回环的轮
作废并补跑」（补跑上限 2 次）**；`lsof` 核实结果逐轮落盘。

**files 臂的 IO/CPU 边界定性（评审订正）**：先做一次「IO 绑定 vs CPU 绑定」判定（`/tmp` 落盘 + 预热
page cache + 观察 `iostat`/CPU 曲线）；若判定为 IO 绑定（磁盘噪声 ≫ 1–3% 效应）⇒ 如实登记「该臂不可判」，
不做数字结论。

### 4.3 逐条判绿 / 证伪

**主判据（必达，micro 三件套）**：

- ① **两项 poll 样本合计降幅 ≥30%**（lo0 臂 `sample`：`reactor_turn→poll` + `driver_loop→poll`，
  配对中位）——这是 F2 的直接证据；`reactor_turn→poll` 单项归零**不单独构成收益证据**。
  **三档口径**：≥30% = 达标；20–30% = 部分达标（保留实现 + 如实记录)；**<20% ⇒ 止损回退 F2**；
- ② **拍频（`pump=/5s`）与每次调用成本变化必报**（唤醒语义变化的自变量；允许拍频上升——那是吞吐通道）；
- ③ **吞吐不回归**：lo0 与 en0 两臂 down/up 中位相对 A ≥ −2%；
- ④ **RSS ≤ A×1.1**（F1 +64KiB 常驻、F5 懒分配、F4 ≤+8MiB 最坏、F6 缓冲复用**降** RSS）；
- ⑤ **F1 叶帧 = 0**（`DnsFaces::service → __bzero`）。

**参考量（不作"必达"，但必须报）**：s/GB 相对 A 的变化（含置信叙述）。**模型预期 s/GB −1~−4%**
（F1+F2 合计 ≈4.5–6.5 线程点；驱动线程 ≈57% 进程 CPU）⇒ **预先登记**：「若 ⑤ 与 ① 达标而 s/GB 落
0~−3%（带内噪声），按前段 §4.3 证伪条款记『结构性交付、端到端持平』，**不宣称 CPU 收益**」。

**逐条证伪**：

- F1：叶帧未归零 ⇒ 定位残留 memset；
- F2：① 未达（降幅 <20%）⇒ **止损回退 F2**（独立 commit，其余条目不受影响）；
- F2：若 poll 叶帧消失、拍频不变、吞吐与 s/GB 都不动 ⇒ 记「收益落在带内噪声」并给机制；
- F5：单测断言失败 ⇒ 回退该条；
- F6：字节等价回归失败 ⇒ 该条回退；files 臂测不出 ⇒ 记「收益 < 带内噪声」（不虚报）；
- 全体：吞吐回归 >3% 且复现 ⇒ 回退对应条目（逐条独立 commit）。

**增益判据（期望达标，不必达）**：同 CPU 下吞吐 +≥3%（若「唤醒时延改善」通道占主导）。

### 4.4 复现命令（F0 修好后可直接跑）

```bash
# 0. 构建（A 臂 = HEAD 检出后同法构建；B 臂 = 实现后）——A/B 二进制都留 sha256
cargo build --release -p homeway-cli && shasum -a 256 target/release/homeway-cli

# 1. 三臂流程（脚本内含：出口起停 / 客户端 state / loadavg 带时间戳 / CPUδ / RSS / sample / 端点核实 / 日志归档）
tools/qi-ab.sh speedtest target/release/homeway-cli.A target/release/homeway-cli.B --rounds 3
tools/qi-ab.sh speedtest target/release/homeway-cli.A target/release/homeway-cli.B --rounds 3 --en0
tools/qi-ab.sh files     target/release/homeway-cli.A target/release/homeway-cli.B --rounds 2
tools/qi-ab.sh rss       target/release/homeway-cli.A target/release/homeway-cli.B --flows 16

# 2. 手工形态（脚本未覆盖时的最小复现）
#    出口：serve --state /tmp/qit-exit9 --listen 42659 --bind-interface none \
#           --public-endpoint 127.0.0.1:42659 --verbose        # 不带任何 stun flag（F0 订正后形态）
#    客户端：--state /tmp/qit-client（serve/relay 双关）+ serve token → token <hmw1> --loopback-only → host add
#    speedtest --host qi --state /tmp/qit-client --json --down 15s --up 15s --streams 4
```

---

## 5. 设计门记录

> 第 1 棒设计门 = `reviewer` skill（dsh headless 外部评审）。
> **轮次目录** `/tmp/dsh-review/r21.NQMZfM/`（`prompt.txt` / `output.md` / `stderr.log`）；
> **exit code = 0**（dsh 前台跑，成败只认 exit code）。
> 评审独立做的事：约 25 处源码位置逐条回核（**结论：行号可信度很高，未发现漂移**）、回读
> `/tmp/qit-ab` 原始产物复算 §0.2 数字、用等价最小样例 `rustc` 实测 F1/F3 的借用形态、
> 沿 `driver_loop → handle_inbound → pump → reactor_turn` 全链核对 F2 的变更窗口、
> 独立复现 F0（含 `--relay=`/`--public-endpoint=` 同类形态）、对照 Go 基线 flag 帮助文本、
> 读 `docs/reviews/reactor-design.md` 找出 F2 与已批决策的冲突。

### 5.1 结论

**意见计数**：评审**总评自述「1 条高危 + 5 条中危」**，但其**逐条严重度标签**实为 **2 高**
（1.1 F0 定性与最小修法、1.3(a) F2 收益模型）× **11 中**（1.3(b)(c)(d)(e)、1.4 F3、4.2 附录 A、
6.1–6.5 测量计划）+ 多条低（1.5/1.6/1.7/2/5/7 与 6.6–6.8）——**评审自身计数有出入**，本记录以
**逐条标签**为准并如实标注。另 §8 明确列出「看过、没发现问题」的九方面。
**门结论（评审原文口径）："改后过"**——不必推翻整体方案；F1/F4/F5/F6 可直接进实现。
**本设计处置：全部认同并入 v2**（0 条不认同；两处为「认同并订正我方的错误结论」）。

### 5.2 评审原文摘要（逐条）

| # | 评审意见（摘要） | 严重度 |
|---|---|---|
| 1.1 | **F0 定性 + 最小修法**：是产品 CLI 缺陷（Go 文档化「空 = 关」被 Q-H 泛化纪律打断）；**影响面更大**（`--relay=`/`--public-endpoint=` 空值覆盖形态同样 rc=2）；**最小修法不是 config 预置而是直接去掉两个 flag**（`--public-endpoint` 非空 ⇒ `engine.rs:1424-1441` 短路，UPnP/STUN 根本不会被调用）；「§0.2 引用 E20 行当 config 预置生效的证据」不成立 | 高 |
| 1.2 | **F1 看过，没问题**；借用形态用最小样例 `rustc` 实测可编译（v1 的两级回退大概率用不上；「不得引入第三种形态」是无依据的自我设限） | — |
| 1.3(a) | **F2 收益模型高估约 2× 且与 §4.3 证伪条款自相矛盾**：合并后每拍只剩一次 poll，但它仍扫同一批 fd ⇒ 消失的是**一次 syscall 入口**（≈2.5–3.5µs/拍 ≈4–6 线程点），不是 12.54 点；`driver_loop→poll` 会从 5.96% 升到 ≈12–13%（**v2 按 §0.2 的 sample 口径重算为 ≈2.6–4.6 点**）；s/GB 的合理模型只有 −3~6%，正好压在 5% 门槛上 ⇒ ① 主判据改「两项 poll 成本之和 + 每次调用成本 + 拍频」三件套、s/GB 降参考、预登记「叶帧达标而 s/GB 持平」的记录口径；② 建模文字改对 | 高 |
| 1.3(b) | 逐元素 `fd`+`events` 相等**不能**一般性证明 revents 与流号对齐（fd 复用 + HashMap 序）；今天恰成立但没有不变量守着 ⇒ 采用条件应把 `poll_index`（流号）逐元素加入比较 | 中 |
| 1.3(c) | `engine.rs:1097` 的 `pollfds[1..]` 必须收窄（否则 reactor fd 被当腿处理：无害但每拍 O(L) 白扫 + 靠被调方兜底） | 中 |
| 1.3(d) | F2 是对 `reactor-design.md:22-40` **已批否决项**的反转，设计未引用/未登记 | 中 |
| 1.3(e) | F2 不只消 syscall，还**改变唤醒语义**（上游就绪成为引擎唤醒源；1/5ms → 立即）——这可能是真正的端到端收益来源，须写入收益模型并必报拍频 | 中 |
| 1.3(f) | poll 失败路径（`n<=0`）未定义 ⇒ 传 `None` 走兜底 | 低 |
| 1.3(g) | 每拍兴趣集被构建**两次**（append 一次、重建一次），N→1024 时不可忽略 | 低 |
| 1.3(h) | **更简做法（推荐）**：快照直派——append 返回流号快照，`pump_with` 后不再重建/比较/兜底，按快照派发 + 校验「流在册 ∧ fd 一致」；代价 = 窗口内新建流晚一拍（≤1/5ms），需显式接受。另有「不值得做」的反面论证与折中（F2 排后 + 先行微判据 + 止损回退） | — |
| 1.4 | **F3 方案结论不成立**：`service_face` 是 `DnsFaces` 的方法，`rx_scratch` 属 `Interceptor` ——「借用面不相交 ⇒ 可行」是错的；正解 = `service(..., scratch: &mut [u8])` 穿参（`service_dns` 处三个不相交字段），或退「每拍每面一次」 | 中 |
| 1.5 | **F4 正确**，三点订正：站点数 **13**（非 5）；**内存包络** rx backing 128KiB→≈256KiB（×64 = ≤+8MiB，进 RSS 口径） | 低 |
| 1.6 | **F5 正确**，订正：测试内 `respond` 调用点 **4 处**；口径改「每查询**每腿**」；RSS 64×64KiB 已进判据④ | 低 |
| 1.7 | **F6 三条正确**，补：`self.buf` 还被 `read_line_opt` 共用（范围要含它）；`read_frame_into` 返回语义写死；`write_all_owned` 部分接纳的保尾方式点名 `drain(..n)` | 低 |
| 2 | 「是否每拍税」复核：F1/F2 是；**F3 不是**（每连接×每尝试，措辞要收紧）；F1 的「1.1GB/s 理论」与实测 278 样本差 3.5×，统一以实测为准 | 低 |
| 4.1 | 「零编号判据行变更、零 wire」**站得住**；F2 的非编号行加字段按先例登记即可 | — |
| 4.2 | **DNS TTL 缓存「本轮不做」：认同**；补第三条更硬的理由（缓存会削弱 tier 明文的「上游跟随主机解析（秒级）」可观测性）；**附录 A 登记口径不完整且自相矛盾**（命中同走后处理 ⇒ `trunc` 规则不变、`aaaa-mixed` 不缩小、`resp` 无需重定义；真正变的是 `fail`/`fallback` 数值与 `q`/`resp` 数值）；补 TTL 递减边界（`saturating_sub` + 龄≥寿命判过期）、UDP/TCP 共键后处理分叉；**收口须同步改 ROADMAP 尾段文字** | 中 |
| 5 | Go 直译/过度设计：**唯一「为对齐而对齐」的是 F4**（冷路径形态收敛，应标最低优先级）；F2 的三件套相对快照直派是多余复杂度；F6.3 属「边际收益/新增分支」权衡（可只做 F6.1+F6.2） | 低-中 |
| 6.1–6.8 | **测量计划口子**：① A→B 顺序偏差无对抗 ⇒ 平衡序 + 配对分析；② files 臂「任一成立」= 多重比较口子 ⇒ 定唯一主侧 + 两轮同号 + IO/CPU 定性；③ RSS 臂只跑 B ⇒ 改 A/B；④ en0 端点采纳不可控 ⇒ 预登记作废+补跑；⑤ 判据①与证伪条款冲突（同 1.3a）；⑥ loadavg 无时间戳/非 1Hz ⇒ 带时间戳 + 轮首轮末标记；⑦ 二进制覆盖 `target/release` 无 `trap` ⇒ trap 恢复 + `HOMEWAY_BIN`；⑧ reactor 剂量数字无产物 ⇒ 归档出口 stdout | 中/低 |
| 7 | 行号/计数独立复核：≈25 处**未发现漂移**；订正：`__bzero` 合计 328（我方 325，符号口径差异 ⇒ v2 写 ≈331 含 stub 变体）、pump 测试调用点 12（我方写 13）、`respond` 测试点 4（我方写 2）、loadavg 非 1Hz 无时间戳 | 低 |
| 8 | 明确「看过、没发现问题」：F1 借用/清零、F4 API 映射、F5 Arc 快照与局部队冲、F6 服务端增量零填字节等价、审计行号订正、零判据行/零 wire、不做项理由、F7 隔离纪律 | — |

### 5.3 逐条处置表

| # | 处置 | 落到 v2 的位置/证据 |
|---|---|---|
| 1.1 | **认同（高危）**——F0 改「**直接删两个 flag**」为主方案（保留 config 预置为「若某些形态确需显式关」的备选并写明其坑）；影响面补 `--relay=`/`--public-endpoint=`；§0.2 删掉「E20 行 = config 预置生效证据」的错误引用；上报项扩面（§6.3-1） | §0.2、§0.3、§2 F0、§6.3 |
| 1.2 | **认同**——删掉「不得引入第三种形态」的自我设限；写明「评审已用最小样例 `rustc` 实测可编译」 | §2 F1 |
| 1.3(a) | **认同（高危）**——§2 F2 增「收益模型（v2 订正）」小节（按本轮 §0.2 的 sample 口径重算：净省一次 syscall 入口 ≈2.6–4.6 线程点）；§4.3 主判据改 micro 三件套、s/GB 降参考 + 预登记「结构性达标/端到端持平」记录口径 | §2 F2、§4.3 |
| 1.3(b) | **认同**——v2 采用**快照直派**（比「加流号比较」更彻底：快照即带流号 + 派发前校验「流在册 ∧ fd 一致」） | §2 F2 |
| 1.3(c) | **认同**——引擎腿切片改 `pollfds[1..leg_end]` + 断言/单测 | §2 F2、§2 测试计划④ |
| 1.3(d) | **认同**——新增「与已批设计的关系」小节：引用 `reactor-design.md:22-40` 原表、登记该行反转、说明另两栏（API/唤醒时延）的处置 | §2 F2 |
| 1.3(e) | **认同**——收益模型写明「第二通道 = 唤醒时延 1/5ms→0」；§4.3 ② 拍频必报；§3 登记两条行为变更 | §2 F2、§3、§4.3 |
| 1.3(f) | **认同（细则微调）**——`n<0`（EINTR/错误）⇒ 传 `None`；**`n==0`（超时、revents 全 0）⇒ 传 `Some`**（快路径空派发、零 syscall，不必再 poll 一次） | §2 F2 代码草图 |
| 1.3(g) | **认同**——快照直派后快路径**不重建**兴趣集（每拍只建一次） | §2 F2 |
| 1.3(h) | **认同并采纳**主推形态（快照直派 + 「窗口内变过的条目晚一拍」显式接受 + 止损回退闸门） | §2 F2、§4.3 |
| 1.4 | **认同（我方错误）**——F3 方案重写：`DnsFaces::service(..., scratch: &mut [u8])` 穿参（`service_dns` 三个不相交字段）；退路 = 每拍每面一次 | §2 F3 |
| 1.5 | **认同**——F4 补 13 站点清单 + 2× 内存包络（进 RSS 口径）+ 标「形态收敛 > 收益」 | §2 F4、§3 |
| 1.6 | **认同**——F5 改「每查询**每腿**」；测试调用点 4 处；scratch **懒分配**（附 RSS 说明） | §2 F5 |
| 1.7 | **认同**——F6 范围含 `read_line_opt`；`read_frame_into` 语义写死；`write_all_owned` 部分接纳用 `drain(..n)`；服务端缓冲每连接 | §2 F6 |
| 2 | **认同**——F3 归类收紧（非每拍税）；F1 收益统一按实测（278 样本 ≈19.5ms/s），理论值标上界 | §1 表、§2 F1/F3 |
| 4.1 | 记录（无改动） | — |
| 4.2 | **认同**——§1 条目 5 补第三条理由；附录 A 计数口径按「命中同走后处理」统一（`fail`/`fallback` 降、`resp`/`trunc`/`aaaa-mixed` 输入集含命中腿）；补 TTL 递减边界与 UDP/TCP 后处理分叉说明；§6.3 加「ROADMAP 尾段文字同步」义务 | §1 表 5、§3、§6.3、附录 A |
| 5 | **认同**——F4 标最低优先级且「形态收敛 > 收益、不进收益口径」；F6.3 标注「可只做 F6.1+F6.2」；F2 三件套已换快照直派 | §2 F4/F6、§5.4 |
| 6.1–6.8 | **认同（全部）**——平衡轮序 + 配对分析；files 臂唯一主侧（CLI）+ 两轮同号 + IO/CPU 定性；RSS 臂 A/B；en0 作废补跑规则；判据改 micro 三件套；loadavg 带时间戳 + 轮首轮末标记；二进制 `trap` + `HOMEWAY_BIN`；出口 stdout 归档 | §4.1/4.2/4.3、§2 F7 |
| 7 | **认同**——计数订正：`__bzero` 族写 ≈331（含 `DYLD-STUB$$_platform_bzero`；纯 `__bzero` 符号 325）、pump 调用点写「≈13（tests/pump 内部）」、`respond` 测试点 4、loadavg 口径订正 | §0.2、§2 F1/F5/F7 |
| 8 | 记录（无改动） | — |

### 5.4 不认同项

**无。**（两处是「认同并订正我方错误结论」：1.4 的 F3 借用面、4.2 的附录 A 计数口径；
一处是「认同并采纳更简形态」：1.3(h)。）

---

## 6. 不做与残余登记（防「静默漏做」）

### 6.1 本轮明确不做（有据）

| 项 | 结论 | 证据/理由 |
|---|---|---|
| **DNS TTL 缓存**（原尾段头条） | **不做（登记 + 附录 A 骨架）** | 见 §1 条目 5（规格面 raw forwarder / 上游跟随可观测性被削弱 / 本批无可执行端到端判据 / 风险最高 vs 不可证收益）；**若做须另起小批并同步 ROADMAP** |
| DNS「每查询新 socket」（`dnsproxy.rs:668-669`） | **不做** | 随机源端口 = 防投毒属性（事务 ID 已随机，源端口是第二维熵），Go 同形（Q-E 复验亦判「非缺陷」） |
| `server/bind.rs:757` 每包 `Vec`（发送环槽） | **不做（登记）** | ≈0.3% 量级（代码注释自估，本轮未实测其叶帧）；回收环 = 跨线程所有权改造，风险 > 收益 |
| `wtransport/bind.rs:287` 每包 `Vec`（客户端出站） | **不做（登记）** | 手机核侧（本地无采样判据） |
| `dnsface::service_face` 两处 `collect()` 小 Vec | **登记不改**（F3 顺手可做） | 实测 0.19%；与 F3 同函数、零风险可用复用缓冲 |
| SipHash（五元组/流号哈希） | **观察项（数值上调）** | 本轮实测 ≈3.1% 驱动线程；换 hasher 是全局决策且键含用户可达五元组 ⇒ 弱 hasher = HashDoS 面，须带随机种子/上限评估，属独立批 |
| 统一事件循环（kqueue/epoll 收编 reactor） | **观察项（F2 已吃掉主要税）** | 见 §2 F2；更大改造留后续批 |
| 状态快照 / `RelayLog` 缓冲 / `flows.keys().collect()` / 新流全表 count / `resolve_pending` 复用 / `stackb` 池化 | **不做（沿前批结论）** | 前段设计 §6 + 代码门 L5/L6 已逐条论证；本批复验未出现新证据 |

### 6.2 Q-E 登记 DNS 面残余评估结论（评估后登记）

| 项 | 评估 | 结论 |
|---|---|---|
| DNS 全死上游稳态 ≈25.6 qps（64 worker / 2.5s 预算） | 该形态下客户端本来就在 2.5s 后收 SERVFAIL；扩 worker 只把更多 SERVFAIL 送得更快，不改善用户面；fds/内存面（64×512KB 栈）已是权衡位；Go 的 goroutine-per-query（≈102qps）是运行模型差异 | **登记维持**（不改 worker 数） |
| smoltcp rx 溢出无观测面 | `PacketBuffer` 无公共 drop 计数（丢点只有 `net_trace!`）；开 `log` feature 会给**热路径**插桩（性能批反向）；容量已由 Q-E F4d 对齐在途上限 | **登记（不可观测面如实记录）** |
| UPnP 域名解析（`to_socket_addrs`）无可取消面 | 字面 IP 快路径（Q-E M3）已覆盖生产 IGD LOCATION 形态 | **登记维持** |

### 6.3 需上报项（主会话裁决）

1. **Q-H 取值纪律回归（阻塞面）**：`--stun=`/`--stun6=` 空值被 `cli_flags` 拒 ⇒ `tools/local-rust-exit.sh`、
   `perf-ab.sh`、`matrix.sh` 的 Rust 出口全起不来；**同类还有 `--relay=`/`--public-endpoint=` 的
   空值覆盖形态**（config 开了想用 CLI 关掉时无路可走）；且 `serve_cli.rs:530` 注释与 Q-H 行为自相矛盾，
   Go 基线文档化「空 = 关」。本轮按工具侧最小修法（F0）解锁测量；**是否给 CLI 加空值 carve-out
   （限语义为空有效的 flag）由主会话裁决**（若加，需在 `INTEROP-CRITERIA.md` 判据变更记录登记该局部回退）。
2. **DNS TTL 缓存的范围收缩**：原尾段头条经取证判「本轮不做」（§1 条目 5 / §6.1）；骨架与再入条件见附录 A。
   若用户要求做，建议**独立小批**（含 tier 规格面与判据登记）；**本批收口须同步修改
   `REVIEW-ROADMAP.md` §Q-I 尾段文字**（否则账实不符）。
3. **F0 归属**：工具修复在本批 commit（可独立回退）；若主会话认为应由 Q-H 补丁承担，则本批 F0 退为
   「临时脚本（`/tmp`）」并另立条目。

---

## 附录 A：DNS TTL 缓存设计骨架（本轮不做；若裁决要做，按此实施）

**目标**：重复查询在 TTL 窗口内由出口直接应答（省一次上游往返 + 上游负载）。

**硬规则（安全边界）**：

1. **键 = 查询字节去掉 ID**（含 question 段原样字节 + additional 段原样）——**不做大小写归一**
   （0x20 混大小写查询 = 未命中，已知代价）；保证「缓存应答的 question 段与当前查询逐字节相同」
   ⇒ 命中路径只需回填 ID，无拼接/重压缩风险；
2. **只缓存正向应答**：RCODE=0 ∧ ANCOUNT≥1 ∧ **TC=0** ∧ 载荷 ≤ 4KiB；**不做负缓存**；被过滤 qtype
   在缓存之前返回，不涉缓存；
3. **条目寿命 = min(全部非 OPT/TSIG RR 的 TTL)**（存储前已 `clamp_ttl(≤MAX_TTL=60s)` ⇒ 寿命 ≤60s）；
   **命中前先判「龄 ≥ 寿命 ⇒ 过期重取」**；命中时按龄递减各 RR 的 TTL（**`saturating_sub`**；OPT/TSIG
   跳过——沿用 `clamp_ttl` 的 RR walk，泛化为 `rewrite_ttl(resp, f: Fn(u32)->u32)`）；
4. **命中后仍走原后处理**（与未命中路径同源）：`count_aaaa` → `clamp_ttl` → `!is_tcp ⇒ truncate` → `resp++`
   ⇒ **UDP/TCP 共用一个键**，TCP 只跳截断那一步（后处理分叉显式说明）；
5. **并发**：单 `Mutex<Cache>`（条目 ≤512、总字节 ≤1MiB、插入序 FIFO 淘汰）；DNS qps 量级下锁不是问题；
6. **计数纪律（v2 订正）**：因命中同走后处理，**`resp`/`trunc`/`aaaa-mixed` 的输入集把命中腿纳入**
   ——行文不变、**数值上升**（`trunc` 的判定规则不变）；**`fail`/`fallback` 数值下降**（命中不再走上游）；
   `q`/`qtcp` 不变；另加 hit/miss/evict **additive 观测行**（`dns 缓存：hit=… miss=… evict=… entries=…`）。
   **E22 行文不变**；
7. **行为注记**：上游跟随/主机 DNS 架构变化的生效延迟被条目寿命（≤60s）延后——**须在
   `INTEROP-CRITERIA.md`「已知口径注记」与 tier 侧规格面一并登记**（这是本项最大的规格面代价）。

**验收（若做）**：① 单测：N 条同查询窗口内上游 fake 计数 = 1；TTL 过期后再查 = 2；不同 qtype/大小写
= 各自未命中；TC=1 / 负应答不入缓存；命中路径与未命中路径的应答字节除 ID/TTL 递减外逐字节相同；
② 隧道冒烟：`dnstest <域>` ×3（同域）⇒ additive 行 `hit=2 miss=1`；
③ 端到端：**需先补一个带时延的隧道 DNS 探针**（现无）——**这是本项再做时的前置条件**。

**再入条件**：用户/主会话裁决接受「出口侧缓存」这一规格面偏离（含上游跟随延迟的登记）；且先补带时延的
隧道 DNS 探针（否则无端到端判据）。
