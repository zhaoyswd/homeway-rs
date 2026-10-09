# M2 S5 门槛与洪泛实测证据包（读数留证）

> **为什么建这份文件**（照 `docs/reviews/M1-S5-evidence.md` 的先例与理由）：S5 的读数原始件
> 全在 `/tmp/m2s5-res/`（易失），而收口报告要**逐格**回答「M2 的改动有没有让 M1 的登记值
> 退化」——数字若只活在 `/tmp` 里，S6 代码门与后续期的门槛判定都会变成"口说无凭"。故本文件
> = **读数台账**（逐格给「实测值 + 基线值 + 偏差 + 判据」+ 来源文件路径），**不改任何代码/判据**。
>
> 范围与边界：只覆盖 `docs/reviews/M2-design.md` §10 的 **S5-1…S5-5**；**S5-4 真机项不做**
> （主会话指令：用户设备上跑着生产隧道，任何安装/注入都会拆掉它 ⇒ 登记为「待用户点头」）。
> 基线值来源 = `docs/reviews/M1-S5-evidence.md`（M1 S5 登记）+ `docs/QUIC-BASELINE.md`（M0）。
> 实施期订正以 `M2-design.md` §13/§14 为准（§14-1②：`per_src_fails` 缺省 10→16）。

---

## 0. 口径、机器与纪律（先读，否则下面的数字都不能用）

| 项 | 值 / 证据 |
|---|---|
| 机器 | Mac mini（Apple M2 / 8 core，Darwin 25.5.0 arm64）；`rustc 1.99.0` |
| 主指标 | **每包 CPU**（`getrusage` user+sys µs / 成功往返数），墙钟只作副读（PERF-AB §9.15.1） |
| 内存口径 | `vmmap -summary` 的 **Physical footprint**（`ps -o rss=` 不作判据——M0 已证不可用） |
| 中位口径 | **下中位**（harness `lower_median`，照搬 M0/M1，勿"修"） |
| 独占纪律 | 每轮开工前查 `vm.loadavg` 1min ≤ 4；逐轮读数对照同目录 `loadavg.tsv` |
| 仪器同一性 | `quic-ab` 六枚臂二进制与 M1 S5 的 **sha256 逐字节同**（见 §1.0）⇒ 两期数字可比 |
| 本切片代码改动 | **只有 harness**：`tools/m1-ab`（`--mtu-cap`/`--quic-ep`）、`tools/quic-probe`（`flood` 三档 + 黑洞 socket）、**新增** `tools/m2-s5-e2e.sh`、`tools/udp-delay-proxy.py`（commit `8f82346`）；`crates/**` **零改动** |
| 读数落点 | `/tmp/m2s5-res/{quic-ab-cpu,quic-ab-overhead,mem-*,size,e2e}/`（逐轮 `.json` / `.out` / `summary.txt` / `loadavg.tsv`） |

**loadavg（首/末，逐运行；见各目录 `loadavg.tsv`）**：

| 运行 | 首 | 末 |
|---|---|---|
| `cpu`（5 rounds） | `{2.86 2.02 2.05}` | `{2.07 1.95 2.03}`（`-- round 5 end`） |
| `overhead` | `{1.90 1.92 2.01}` | `-- overhead end` |
| `mem steady` | `{2.21 1.99 2.04}` | `{1.60 1.83 1.96}` |
| `mem load` | `{1.48 1.80 1.95}` | `{2.15 1.88 1.95}` |
| `mem conns`（五点/三点/扩展） | `{2.06 1.87 1.94}` / `{1.98 1.84 1.93}` / `{1.73 1.79 1.91}` | 各 `-- mem(conns) end` |
| `mem conns-load`（两档） | `{1.45 1.71 1.87}` / `{1.72 1.73 1.86}` | 各档 `end` 行 |
| `size`（lab+product） | `{1.76 1.76 1.87}` | `{3.75 2.19 2.01}`（OHOS 交叉构建尾段） |

**全期间无 1min > 4 的窗口**（首末与逐轮对照如上；唯一 >3 的采样在 `size` 尾段的交叉构建）。

---

## 1. S5-1 门槛读数（`tools/quic-ab.sh`；逐格对 M1 登记值）

### 1.0 仪器同一性（先立这条，后面所有偏差才有意义）

| 臂 | M1 运行快照（03:26:41） | M2 本轮（09:46:20） |
|---|---|---|
| `raw` | `f8af4979…fce0` | `f8af4979…fce0` |
| `wg` | `b5f8347e…e013` | `b5f8347e…e013` |
| `wg_size` | `e3da3494…bbba` | `e3da3494…bbba` |
| `quic` | `2ac23dcf…b010` | `2ac23dcf…b010` |
| `multiconn` | `a1192654…8795` | `a1192654…8795` |
| `wg-shim` 的 `wg` | `2cc5d582…978a` | `2cc5d582…978a` |

来源：`/tmp/m2s5-res/quic-ab-cpu/bins.sha256` 与 `/tmp/m1s5-res/quic-ab-cpu2/bins.sha256`
（六枚全同）⇒ **同一套测量仪器**，本期偏差反映被测面/机器，不反映仪器。

### 1.1 每包 CPU（四臂全量；lab 档；1280B 载荷 = MTU1400 口径；`--rounds 5`）

| 臂 | 本轮实测（下中位） | 逐轮 | M1 登记 | M0 基线 | 对 M1 偏差 | ±10% |
|---|---|---|---|---|---|---|
| `raw` | **4.851 µs** | 4.825 / 4.900 / 4.882 / 4.851 / 4.850 | 4.624 | 4.882 | **+4.9%** | 过 |
| `wg-shim`（现役手机形态） | **14.713 µs** | 14.701 / 14.713 / 14.713 / 14.725 / 14.710 | 14.410 | 14.623 | **+2.1%** | 过 |
| `wg-ring`（诊断臂） | **10.640 µs** | 10.604 / 10.678 / 10.600 / 10.646 / 10.640 | 11.063 | 10.740 | **−3.8%** | 过 |
| `quic`（quinn+rustls(ring)+tokio，DATAGRAM） | **12.250 µs** | 12.250 / 12.186 / 12.243 / 12.711 / 12.813 | 12.895 | 12.668 | **−5.0%** | 过 |

- **M1 门槛（设计 §8）**：`quic ≤ 现役 WG+shim × 1.0` ⇒ **12.250 ≤ 14.713 = 0.833× 成立** ✓
  （M1 同格 12.895 / 14.410 = 0.895×，M2 更宽）。
- **M2 的改动对每包 CPU 无退化**：`quic` 相对 M1 登记 **−5.0%**（相对 M0 基线 −3.3%）；四臂全部
  落在 ±10% 带内（最大 |偏差| = `raw` +4.9%，为四臂共用的机器抬升，`quic/wg-shim` 比值不受影响）。
- 逐轮五轮**无越界**（M1 首轮曾出现的 `wg-ring` 44.673 µs 型轮首污染本轮未复现）。
- harness 措辞小瑕（M1 已登记的 L2 同类）：5 轮时 summary 仍打印「三轮下中位」，实际取的是
  轮数无关的 `lower_median`（5 轮 ⇒ `a[3]` = 真中位）。
- 来源：`/tmp/m2s5-res/quic-ab-cpu/{summary.txt,cpu-*.json,loadavg.tsv}`、`/tmp/m2s5-res/cpu-run.log`。

### 1.2 线开销

| 项 | 本轮实测 | M1 登记 | 偏差 | 判据 |
|---|---|---|---|---|
| WG 每包线上字节（1280B 载荷） | **1312 B（开销 32 B）** | 1312 B（32 B） | 逐字节同 | ≤40B ✓ |
| QUIC 每包线上字节（oneway，服务端 `udp_rx` 口径） | **1310.136 B（开销 30.136 B）** | 1310.152 B（30.152 B） | **−0.016 B（−0.001%）** | ≤40B ✓ |
| `max_datagram_size`（MTU1200 / MTU1400） | **1162 / 1362** | 1162 / 1362 | 精确 | 精确 ✓ |

- 设计 §8 的预期是「稳态每包**逐字节不变**」⇒ 实测差 0.016B（6 万包累计 960B，属 `ios` 轮次
  切分的尾数），**成立**（远小于任何判据粒度）。
- 来源：`/tmp/m2s5-res/quic-ab-overhead/summary.txt`。

### 1.3 内存（`vmmap` Physical footprint）

**(a) 稳态（单连接 hold，IDLE 9s，三轮下中位）**

| 臂 | 本轮 | M1 登记 | M0 基线 | 偏差（对 M1） | ±10% |
|---|---|---|---|---|---|
| raw | **960K** | 960K | 960K | 0% | 过 |
| wg-shim | **1008K** | 1008K | 1008K | 0% | 过 |
| wg-ring | **1552K** | 1552K | 1552K | 0% | 过 |
| **quic** | **1216K** | 1248K | 1232K | **−2.6%**（对 M0 −1.3%） | 过 |

**(b) 负载态（N=300k 传输中 max footprint）**：raw 960K / wg-shim 1008K / wg-ring 1552K /
quic **1296K**（M1 登记同格 = M0 值 960 / 1008 / 1536 / 1280）⇒ 最大偏差 +1.3%（quic）✓

**(c) 每连接边际（`multiconn` 服务端；§9.1-1 的拟合口径）**

| 口径 | 点集与逐点值（K） | 拟合（本轮） | M1 同口径 | 判据 |
|---|---|---|---|---|
| **五点（缺省）** | 1/1264 2/1376 3/1472 4/1536 5/1600 | **base=1200.0K，边际 84.00K**（全点最小二乘 83.20K） | base=1209.6K，80.00K | **≤96K ✓**（+5.0%） |
| 三点（对照） | 1/1280 3/1488 5/1616 | base=1218.7K，边际 **72.00K** | base=1209.3K，84.00K | ≤96K ✓（−14.3%） |
| 扩展点（含 **32**） | 1/1280 2/1392 3/1488 4/1552 5/1616 8/1792 16/2096 **32/2640** | base=1360.4K，边际 43.87K | — | 见 (d) |

- 边际读数在 72–84K 带内（M1 = 80–84K），**判据（≤96K）两种口径都过**，且不依赖拟合口径选择。

**(d) 32 设备稳态（§9.1-2 修订门槛 ≤ +3.1 MiB）**

- 直测：`N=32 → 2640K`，同轮基线（N=1）1280K ⇒ **增量 1360K = 1.33 MiB ≤ 3.1 MiB ✓**
  （M1 同格：2704K − 1232K = 1472K = 1.44 MiB ⇒ M2 **改善 −7.6%**）
- 保守斜率复核：三点 72K × 32 = 2304K = 2.25 MiB ✓；五点 84K × 32 = 2688K = 2.63 MiB ✓

**(e) 负载态门槛（§9.1-4：32 连接 + 持续流量 ⇒ 增量 ≤ 64 MiB + 自有队列上限）**

| 档 | 空转 | 负载态中位 | 增量 | M1 同格增量 | 判据 |
|---|---|---|---|---|---|
| N=32 × 200pps × 1280B | 2784K | **3616K** | **+832K** | +704K | ≤65536K ✓（大余量） |
| 同上 + 客户端**不读回程**（`--no-read`） | 2752K | **3504K**（峰 3520K） | **+752K** | +720K | ≤65536K ✓（大余量） |

- 口径注记照 M1 原样（勿当"缓冲已测透"）：**quinn 的 datagram 收发缓冲按需增长**，应用侧预检
  使缓冲最多只比在飞数据多一点；64 MiB 是解析上界，在回环上不可达。「把缓冲真顶满」= M6/真机面。

### 1.4 体积（`tools/quic-ab.sh size --profile lab,product` + `tools/build-app-core.sh`）

| 格 | 本轮实测 | M1 登记 | 偏差 | 判据 |
|---|---|---|---|---|
| lab 档 空壳 cdylib | **323,408 B** | 323,408 B | 逐字节同 | ±10% ✓ |
| lab 档 空壳 + QUIC 全栈 | **778,032 B** | 778,032 B | 逐字节同 | ±10% ✓ |
| lab 档 boringtun+smoltcp 在场 + 真实引用全路径 | **1,735,384 B** | 1,735,384 B | 逐字节同 | ±10% ✓ |
| product 档 同三格 | 326,120 / 852,568 / **2,496,256** B | 同三格逐字节同 | 0 | 只登记 |
| 现役对照（主检出只读） | **2,213,744 B**（sha256 `03177a91…`） | 2,213,744 B | 逐字节同 | 只登记 |
| **真产品面 `libclientcore.so`** | **4,685,216 B** | 4,659,872 B（M2 S3/S4 亦 4,685,216） | **+25,344 B（+0.54%）** | **设计 §8：M2 增量 ≤ +32KB ⇒ 过** |

- 三道门（`tools/build-app-core.sh`）：`[sym] 20/20` ✓、`[ver] 版本注入校验通过：8f8234608f67-rust` ✓、
  `[size] 4,685,216 bytes` ✓（三轮全过，rc=0）。
- **M2 的体积代价落定**：准入面（`reg4` 四帧 + `admit.rs` 闸表 + conn 状态机）的净增 = **+25,344 B
  = +0.54%**，在设计给的 ≤+32KB 预算内（余 7,424 B）。对 3.8MB 阈值：4,685,216 B = **1.233×**
  （双栈期，按设计属 **M5 判**；M5 删码余量 ≈0.86MB 仍**未实测**）。
- 来源：`/tmp/m2s5-res/size/`、`/tmp/m2s5-res/build-app-core.log`。

---

## 2. S5-2 端到端 A/B（本地代理；产品路径）

拓扑与参数照 `M1-S5-evidence.md` §2：`m1-ab`（产品路径 = `ClientCore` 世代 + TUN socketpair
+ 合成 UDP 流）→ 直连（LAN）→ 本地 Rust 出口 → `intercept` transit → 本机回显
（放大 1:1、回包满尺寸 1252B 载荷 ⇒ 内层 1280B）；两臂**同刻交替**（轮序奇偶反转）。
驱动 = `tools/m2-s5-e2e.sh 2 3`（A/A2 段）；读数 = `/tmp/m2s5-res/e2e/ab-{direct,cap}-*.log`。

### 2.1 同报头档（rate=3000pps / window=32）

| 轮 | quic 下行 | quic 丢包 | wg 下行 | wg 丢包 | 两臂 `link.via` |
|---|---|---|---|---|---|
| r1 | **30.72 Mbps** | 0.00% | **30.72 Mbps** | 0.00% | quic `direct@192.168.3.12:42653` / wg `direct@192.168.3.12:42652` |
| r2 | **30.72 Mbps** | 0.00% | **30.72 Mbps** | 0.00% | 同上 |
| r3 | **30.72 Mbps** | 0.00% | **30.72 Mbps** | 0.00% | 同上 |

- 判据「**无数量级回退**」⇒ **成立**（六格**逐字节相同**：24000 包 / 30720000 B / loss 0.00%）。
  与 M1 同格（quic 3/3 满额、wg 2/3 满额 + 1 格噪声）相比 **M2 无退化**。
- **准入代价（设计 §8「准入路径微基准」）**：`ready_ms`（Connect → 可用）= **59ms**（quic）/ 55ms（wg）
  ⇒ 四帧准入在 1ms RTT 下增加 **≈4ms**（判据：不越出 M1 读数 ±10% ⇒ 过；注意 `ready_ms` 含
  世代装配 + 面/端点起 + 赛跑 + 四帧，不是纯准入段）。

### 2.2 容量档（rate=0 / window=64 = 只受在飞窗约束）

| 轮 | quic 下行 | wg 下行 | quic/wg |
|---|---|---|---|
| r1 | **136.30 Mbps** | 107.79 Mbps | **1.26×** |
| r2 | **134.69 Mbps** | 108.99 Mbps | **1.24×** |
| r3 | **132.84 Mbps** | 105.47 Mbps | **1.26×** |

- 与 M1 同格（1.24× / 1.13× / 1.26×）同向且同量级；本机回环口径**不是 PERF-AB 真机数**（照搬 M1 标注）。

---

## 3. S5-3 洪泛 / 限流 / 窄路径 / 预算（§3.3 判据 1–6）

驱动 = `tools/m2-s5-e2e.sh 2 3`（B…G 段）+ 探针 `tools/quic-probe flood`（本批新增：三档注入面）。
实例：主出口 #2（缺省配置，挂中继）、独立出口 #4（`HOMEWAY_QUIC_ADMIT_RETRY=never`）、
独立出口 #6（`serve.quic_admit{per_src_fails=1000, retry_policy="never"}`，只用于 C2 上界触达）。
**全部为本地私有实例；现役出口未碰**（`ps` 复核见 §6）。

### 3.1 判据 1「单源有界」（干净臂 + 缺省档对照）

| 臂 | 形态 | K | 结果（客户端逐次序列） | 出口侧行 |
|---|---|---|---|---|
| **B1**（出口 #4，`retry=never`；`F=16`/`W=10s`） | 同源（127.0.0.1）顺序错钉定尝试 | 24 | 第 1–16 次 = 握手级失败（transport/crypto）；**第 17–24 次全部被拒**（`refused`）；`refused=8=K−F` ✓ | `quic: 握手洪泛拒绝（"::/64" 在 10s 内第 **17** 次尝试——已拒；第 1 次）`（+第 18/19 次行；节流首 3） |
| **B2**（出口 #2，**缺省 pressure**；§14-1② 的 `16`） | 同上 | 24 | 第 1–10 次 = 握手级失败；**第 11–24 次被拒**；`refused=14`（≠K−F） | 首拒行同为「第 **17** 次尝试」（Retry 重放各计一次 ⇒ 客户端第 11 次对应闸内第 17 次） |

- **判据 1 成立**（B1 是判据面：定式「第 F+1..K 全部被拒 + `flood_refused=K−F`」逐位命中）。
- **B2 是 S4 交下的「pressure 档最悲观形态」正式读数**（M2.md §1.1 残余点名）：缺省档下
  **客户端 10 次未完成即撞闸**（不是 16），因为 Retry 触发条件②（同源未完成 ≥5）会让
  第 6 次起的每条尝试**再叠一条重放尝试**（各计一次闸）。⇒ **登记**（真机标定归 S5-4，按住）；
  若要 16 与 5 同步需改 `RETRY_AFTER_FAILS`（一行 + 登记，本切片不改产品代码）。
- 跨窗口断言未单独测（本文只给单窗定式；跨窗速率 = F/W 由滑动窗结构保证，S3 单测覆盖）。

### 3.2 判据 2「全局有界」

| 臂 | 形态 | 读数 | 结论 |
|---|---|---|---|
| **C**（出口 #2，干净窗） | `stall`（黑洞：首发后只发不收）20 并发 | 客户端 `stalled=20`；出口行证 = `地址校验挑战` ×3 + `握手泛洪拒绝`「第 17 次尝试」×3；**未见「握手期限」行** | **本臂只作形态参考**：黑洞客户端收不到任何回包 ⇒ 客户端**无法区分**「被 Retry / 被闸拒 / 真悬在途」（`stalled` 只是"未定音"）；出口侧行证显示这批尝试被 Retry + 闸接管 ⇒ **in-flight 未被推到 64**（见下） |
| **C2**（出口 #6，闸抬到 1000 **且** `retry=never`） | `stall` 80 并发 | 出口行 **`拒新连接（连接总数 64/64 超限…）` ×3** + **`握手期限（… 未在 10s 内完成，已弃）` ×3** | `conns.len() + inflight ≤ 2 × max_devices = 64` **上界触达并被强制** ✓；到点回收 ✓ |
| **D**（出口 #2，干净窗） | `no-hello` 70 条（正确钉定 + 握手完成 + 永不发 Hello） | **47 条建立 → 全部在 `ADMIT_DEADLINE` 内被关**（客户端 47/47 被关；出口 `认证超时` ×3 行）；其余 23 条被**每源闸**拒（行证「第 17 次尝试」） | 「握手完成但不发 Hello ⇒ 期限内全关」✓（r14 F3）；**未触达 64 连接**（每源闸先把单源束在 16/窗） |

- **「64 不可达」的机制（本切片实测，值得记）**：缺省配置下 `handshake_cap=64` 与
  `conn_cap=64` 对**单源**永远不可达——两道防线都先把单源束住：①每源闸 `16/10s`；
  ②`pressure` 档 Retry（条件② 同源未完成 ≥5）让后续每条尝试只发 Retry、**不建握手任务**。
  C2 用「抬闸 + 关 Retry」把这条线**单独**测出来（形态非缺省，已登记）。
- 多源（判据 2 的「本地起多 socket 源地址」面）⇒ 见 §3.3（**该面实测证伪**）。

### 3.3 判据 2 的「多源」面：**桶不分离**（S5 新发现 D1，需主会话/S6 裁决）

**B3 证伪臂（干净窗，出口 #2）**：

| 源 | 地址（不同 /32） | k | 结果 |
|---|---|---|---|
| A | `192.168.3.12` | 10 | `refused=0 transport=10`（10 次全放行，用掉 10/16 预算） |
| B | `127.0.0.1` | 8 | **`refused=8 transport=0`**（第 1 次起被拒 —— 两个 /32 **共用一个桶**） |

**根因（代码 + 行文双向证据）**：

1. 出口 QUIC socket 是**双栈**（E-q1 行：`quic: 端点就绪（[::]:42653…）`；`server/bind.rs`
   `bind_dual_stack` ⇒ 默认路径）⇒ IPv4 对端在 socket 面以 **v4-mapped IPv6** 出现
   （行证：`地址校验挑战（[::ffff:192.168.3.12]:61979…）`）；
2. `exit/admit.rs::SrcKey::of` 只区分 `IpAddr::V4`/`V6` ⇒ v4-mapped 落 `V6([0;8])` ⇒
   键 = **`"::/64"`**（行证：`握手洪泛拒绝（"::/64" …）`——**两个不同 IPv4 源的行文键完全相同**）。

**影响（如实）**：①设计 §3.2-④ 的「v4 /32 聚合」在双栈出口上**不生效**——所有 IPv4 客户端
共用一个 16 次/10s 的预算；②一个 IPv4 源打满窗 ⇒ **其余所有 IPv4 客户端的重连被拒**（实测：
E2 ——洪泛后**立即重连被拒**「the server refused to accept a new connection」，**等 11s 窗清后
重连成功**：`握手完成 0.9ms` + `A4 准入`）；③有界性本身**不变**（甚至更严：全局 16/10s）。
**本切片不改产品代码**（S5 = 实测切片；红线之外的产品面改动须主会话裁决）。

### 3.4 判据 3「不伤既有连接」

3 轮 × 30s 产品路径流（rate=3000pps），每轮 **t=10s 起 10s 持续洪泛**（异源 `192.168.3.12`；
`flood --mode pin-fail --k 600 --interval-ms 15` ⇒ 实测 590 拒 / 10 握手级失败 / 用时 ≈10.1s）：

| 轮 | 净窗（sec 1–9） | 洪泛窗（sec 11–22） | 判定 |
|---|---|---|---|
| r1 | 3000 pps / loss 0 | 3000 pps / loss 0 | 下降 **0% ≤10%** ✓ |
| r2 | 3000 pps / loss 0 | 3000 pps / loss 0 | 同上 ✓ |
| r3 | 3000 pps / loss 0 | 3000 pps / loss 0 | 同上 ✓ |

- 逐秒表（`sent`/`acked`）**每一秒都满额 3000 且 loss=0**（含洪泛窗）；洪泛窗与净窗的比值
  = **1.000**（判据 ≤10% ⇒ 大幅过）。隧道可用性：三轮回程 `down.pkt=90000`（= 30s × 3000）
  且末拍状态面 `link={…"via":"direct"}` 在位 ⇒ 不劣化。
- **形态注记（勿读成"攻击无效"）**：本臂洪泛的 600 次尝试里 **590 次被闸秒拒**（只 10 次
  真走 TLS）——这正是「先闸后 Retry」的设计意图（攻击的边际成本被压到本地查表）。

### 3.5 判据 4「内存有界」

| 读数 | 值 | 判据 |
|---|---|---|
| 出口 footprint（`vmmap`，pin-fail 洪泛 600 次 **前** → **中** → **后**） | 10650K → 10650K（逐次 8 点全平）→ 10342K | 增量 **0K ≤ 64MiB** ✓ |
| 出口 footprint（no-hello 47 条在册连接 **中**，8 点采样） | 基线 10547K → 峰值 **10752K**（+205K）→ settle 10650K | 增量 **+205K ≪ 64MiB** ✓ |
| 闸表上限 | `SRC_TABLE_CAP = 1024` 条（≈40–64KB）+ `PROOF_TABLE_CAP` 同量级（**代码常量**；本机不可外部读） | 设计 §3.3-4 同值（登记） |

- 采样单位归一说明见 §4 L3（`vmmap` 在 ≥10MB 时打 `10.3M`，本切片已修采样器）。

### 3.6 判据 5「可观测」（三/五条行族在场 + 与客户端逐次序列同源）

| 行族 | 本切片实测条数（出口日志 grep） | 同源校验 |
|---|---|---|
| `quic: 地址校验挑战（…；在途未认证 n/64；第 N 次）` | 6（B2/C2） | 行内「第 N 次」与客户端被 Retry 的次数同源（行文含 peer 与在途分母） |
| `quic: 握手洪泛拒绝（%s 在 %s 内第 k 次尝试——已拒；第 n 次）` | 21 | **k = 闸内计数**：B1 首拒行 k=17 = F+1 与客户端「第 17 次起全拒」互证 |
| `quic: 认证超时（… 未在 10s 内完成证明——已弃；第 n 次）` | 3（D 臂 + 冒烟） | D 臂客户端 47/47 被关 vs 行到点 ⇒ 同源 |
| `quic: 拒新连接（连接总数 held/64 超限…）` | 3（C2） | 行内 `held/conn_cap` 直接给出快照同源的分子分母 |
| `quic: 握手期限（… 未在 10s 内完成，已弃；第 n 次）` | 3（C2） | 同上 |

- **限制（如实）**：`ExitQuicSnapshot` 的**数值**（`flood_refused`/`retry_sent`/`handshakes_in_flight`）
  本机**不可外部读**（无 status 面暴露）⇒ 判据 2 的「快照可读」在本切片只能以**出口行内自带的
  快照同源数**（`held/conn_cap`、`k`、`n/64`）落实；JSON 面双向可读 = S6/后续期承接项。
  行族节流 = **首 3 + 每 100**（`log_due`）⇒ 计数须按行内「第 n 次」读，不能按行数读。

### 3.7 判据 6「常态不被误伤」

**A3 臂**：**不带** `--force-direct` ⇒ 岛按 token 全候选跑**正常赛跑**，连续 **6 轮**
（rate=500pps、每轮 6s；r2 实测经中继胜出、其余经直连 ⇒ 赛道确实在跑）：

| 计数（该 6 轮出口新增行） | 值 | 判据（§14-1③ 措辞：正常赛跑在阈值内 ⇒ 不触发闸） |
|---|---|---|
| `地址校验挑战`（retry_sent） | **0** | ✓（比放宽后的措辞更严：连 `retry_sent` 都是 0） |
| `握手洪泛拒绝`（flood_refused） | **0** | ✓ |
| `认证超时` | **0** | ✓ |

### 3.8 300ms RTT 冷启动预算（正式读数；S2 只有 probe 侧旁证）

注入 = `tools/udp-delay-proxy.py 127.0.0.1:42702 127.0.0.1:42653 150`（单向 150ms ⇒ RTT 300ms；
两臂 `rtt_ms` 实测 **303–305**）：

| 臂 | 轮 1 | 轮 2 | 轮 3 | 判据 |
|---|---|---|---|---|
| 探针（`quic-probe conn`，四帧全走） | 握手 **303.5ms** + A4 ✓ | 304.4ms + A4 ✓ | 304.9ms + A4 ✓ | 3/3 成功 |
| **岛（产品路径，`--quic-ep` 指到代理）** | `ready_ms=2233` | `ready_ms=2255` | `ready_ms=2252` | **≪ `QUIC_CONNECT_BUDGET=5s`** ✓（用 45%） |

- 对照：同臂直连（RTT≈1ms）`ready_ms=59ms` ⇒ 300ms RTT 下增加 ≈2.19s ≈ 7.2×RTT
  （含 QUIC 握手 1 RTT + 四帧准入 2 RTT + 拥塞/ACK 起转与面装配；**探针面单独手 = 1 RTT**）。
- 设计 §1.7 的预算复核（Q-M2-1 风险）⇒ **不成立**：准入在 300ms RTT 下舒适落在 5s 内。

### 3.9 S5-5 窄路径（M1 交下项 N5；生产路径 + 岛缝两面）

| 臂 | 注入 | 实测 | 判据 |
|---|---|---|---|
| (a) **生产路径**（facade，`m1-ab run`） | `HOMEWAY_QUIC_MTU=1320`（合法区间下限） | 状态面 `mtu=1282 current_mtu=1320`；**无**「窄路径不可用」行、`超限=0` | 旋钮在真实路径生效；mds 1282 > 内层 1280 ⇒ 设计预期「区间内产不出窄路径」**确证** ✓ |
| (b) **生产路径**（越界） | `HOMEWAY_QUIC_MTU=1200` | 状态面 `mtu=1362 current_mtu=1400`（**回落缺省、不夹取**，与 `resolve_mtu_cap` 登记语义一致） | 行为符合登记；**「非法或越界…按缺省 1400 走」记行**未被本 harness 捕获（`m1-ab run` 不落 facade 日志面，见 §4 L4） |
| (c) **岛缝**（`m1-ab migrate --mtu-cap 1200 --req-size 1200`） | `IslandConfig::mtu_cap=1200`（区间外，测试缝） | `quic: 窄路径不可用 —— max_datagram_size=1162B < 内层 MTU=1280B …` + `quic: 丢弃 超限=1/2/3（本次：超限 ×1 包 1200B > max_datagram_size 1162B）` | **设计 S5-5 的断言面命中**（行 + 计数都可见，非静默） ✓ |

- 结论：**N5 关闭**（`tools/` 级注入断言落地）；同时把 M1 已登记的「区间 [1320,1400] 产不出
  `mds<1280`」由 (a) 正面确证 ⇒ 生产配置面**无法**触发窄路径，唯一注入面 = 岛缝（harness）。

### 3.10 产品形态单连接内存（M2 §8 点名：M1 未过格**是否恶化**）

方法照 M1 `m1-ab-e2e.sh` E 段：同一枚 `m1-ab`（= `ClientCore` 世代）跑 `wg|quic`，运行期
`vmmap` 8 次取下中位；wg 档 = 地板（不构造岛），两档之差 = 岛边际（判据 §9.1-3 修订：≤+320K）。

| 轮 | wg 地板 | quic（岛在位） | **岛边际** | M1 同格 |
|---|---|---|---|---|
| r1 | 2032K | 2624K | **592K** | 直连 608K / 512K；**轻载（本臂同形态）496K** |
| r2 | 2080K | 2576K | **496K** | 同上 |
| r3 | 2064K | 2576K | **512K** | 同上 |

- **◆ 该格 M2 后 = 持平（略差）**：三样本 496/512/592K，下中位 **512K** vs M1 同形态 **496K**
  ⇒ **+16K（+3.2%）**，在 16K 页粒度下属**同量级**（M1 直连臂自身也横跨 512–608K）。
  **结论：M2 的准入面（reg4 + admit 闸表 + conn 状态机）没有让这一格恶化**（仍**不达标**：
  512K = 门槛 320K 的 **1.60×**）——**门槛裁决仍待用户/主会话**（M1 已上报，本切片不改门槛）。


---

## 4. 未过 / 存疑项与工具局限（如实，不粉饰）

| # | 项 | 结论 | 处置建议 |
|---|---|---|---|
| **D1** | **按源闸在双栈出口上退化为全局桶**（`SrcKey::of` 未归一 v4-mapped IPv6 ⇒ 键恒为 `::/64`） | **证伪**（§3.3）：异源 /32 不独立；一个 IPv4 源打满窗 ⇒ 其余 IPv4 客户端**重连被拒至多 10s**（E2 实测） | **交 S6 / 主会话裁决**：改法 = `SrcKey::of` 里把 `V6` 的 v4-mapped（`::ffff:a.b.c.d`）归一成 `V4([a,b,c,d])`（一行 + 单测 + 判据行不变）；本切片**不改产品代码**（S5 = 实测切片） |
| **N1（M1 遗留）** | **产品形态单连接内存**（§9.1-3 门槛 ≤+320K） | **仍不达标**，且 M2 后**持平略差**：下中位 **512K**（496/512/592）vs M1 同形态 496K = **+3.2%**（§3.10） | 门槛数值修订 / 降级登记 / S6 profiling 三条老建议不变（门槛表 = 主会话触点） |
| **N2（M1 遗留）** | 体积对 3.8MB 阈值 | 双栈期 **1.233×**（4,685,216 B）；M2 增量 **+25,344 B ≤ +32KB** ✓ | M5 判；M5 设计门先实测删码余量 |
| **N3** | **`handshake_cap=64` 在缺省配置下不可达**（双闸先束住单源） | 本切片用「抬闸 + 关 Retry」单独触达（C2）⇒ 上界**成立**但**不是缺省形态的实测** | 若要「缺省形态也能触达」，需多真源（不同 v6 /64 前缀）或真机；真机面承接 |
| **N4** | `per_src_fails=16` 在 **pressure 档 + 最悲观形态**下客户端第 **11** 次就被拒（§3.1 B2） | **登记**（触发②的 `RETRY_AFTER_FAILS=5` 与 `16` 不同步） | 真机标定（S5-4，按住）；若要同步 = 改 `RETRY_AFTER_FAILS`（一行 + 登记） |
| **N5** | 350ms RTT 预算下 `ready_ms≈2.25s` 的**构成**未分解（7.2×RTT；探针面单独手 = 1 RTT） | 预算**过**（45% of 5s），但「为什么是 7 RTT」未归因 | 后续期/真机 profiling（不影响预算判据） |
| **L1** | `tools/quic-ab.sh` 的 `mem` 采样器只 strip 非数字 ⇒ `vmmap` 的 `10.3M` 会被读成 `103`（K 档无此问题，探针恒 <10MB 故一直未暴露） | **本切片踩到**（产品出口 10MB+） | 已在 `tools/m2-s5-e2e.sh` 归一（M 档 ×1024）；**`tools/quic-ab.sh` 与 `tools/m1-ab-e2e.sh` 的同款未改**（登记，防后续采样 ≥10MB 目标时误读） |
| **L2** | `tools/m1-ab` 的 `--mtu-cap` **只对 `migrate` 档有效**：`run` 档走 facade，MTU 旋钮是 `HOMEWAY_QUIC_MTU`（首跑踩过：`run --mtu-cap 1200` 被静默忽略） | 已修（用法行写明 + 注释）；`run` 档的生产旋钮面已在 §3.9(a)(b) 覆盖 | 无需再改；后续棒用 `run` 档测 MTU 请用 env |
| **L3** | 「非法 MTU 值 ⇒ 记行 + 按缺省」的**记行**在 `m1-ab run` 里落不出来（facade 日志面不接该 harness） | 只读到行为面（回落 1400）；记行由 `facade::tun_exec` 单测（`mtu_cap_resolution_clamps_by_default_policy`）与真机日志面覆盖 | 若要 E2E 看该行：走 `homeway-cli connect`/island e2e（后续期） |
| **L4** | `ExitQuicSnapshot` **无外部只读面** ⇒ 判据 2 的「快照可读」只能以行内自带数落实（§3.6 限制） | 登记 | S6/后续期：`serve status --json` 暴露 quic 段（additive） |
| **L5** | 洪泛臂的**客户端无法区分**「被 Retry / 被闸拒 / 真在途」（黑洞档收不到回包） | C 臂只作参考；C2/D 用**能定音的形态**（关 Retry / 正常握手） | 已在 §3.2 标注；后续棒若要精确读数，需在探针侧按 Retry 包特征分流 |
| **残留** | 预存在的 `tools/quic-ab/arms/target/release/{raw,wg,quic} server` 进程 **11 枚**（Oct 8 20:44 起，`0.0% CPU`，非本切片） | 不影响读数（loadavg 全期 ≤3.75） | 属前一棒的清理面；本切片不擅自 kill 非自己起的实例 |

---

## 5. 未做项（真机 / OHOS 触点；**本轮不宣称已验证**）

| 项 | 为什么本机做不了 | 承接 |
|---|---|---|
| **S5-4 真机复验**（§7 的「能验」项 + §9.1-1 的源校验拒复看） | **主会话指令：用户设备上跑着生产环境的活隧道，任何安装/注入都会拆掉它** ⇒ 本轮不做 | **待用户点头** |
| `per_src_fails=16` 的**真机标定** | 同上（本机只能给「pressure 档最悲观形态」参考读数，§3.1 B2） | 真机（用户点头后） |
| 路径 MTU 变化 / NAT 重绑 / 蜂窝切换 | 回环无这些形态 | 真机（M6） |
| 真源多前缀（多 /64）下的计 2 前半触达 | 本机可用源前缀全落 `::/64`（v4-mapped）+ 无全局 v6 | 真机 / 加 v6 别名的机架 |

---

## 6. 收口门（本切片的测试与纪律门）

| 门 | 结果 | 证据 |
|---|---|---|
| `cargo test --workspace` | **一次全绿**（16 个测试目标 0 failed；`homeway-core --lib` 692 passed/4 ignored；`homeway-quic --lib` 116 passed）——本批无 flake 复现（S4 批曾 3/4 红） | `/tmp/m2s5-res/final-workspace-test.log` |
| `cargo clippy --workspace --all-targets -- -D warnings` | **0 告警**（rc=0） | `/tmp/m2s5-res/final-clippy.log` |
| 三目标 `cargo check` | `aarch64-unknown-linux-ohos` rc=0（真 NDK clang；1 条存量 `libc::time_t` deprecated warning）；`x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl` **按 `ci.yml` 配方**（`CC_*=clang` + `CFLAGS_x86_64…=-nostdlibinc …` + `-p core/cli/capi/quic --locked`）**各 rc=0**；`-nostdlibinc` 落点断言 = 只落 ring ✓；ring `libring_core_0_17_14_.a` 两目标都在 ✓ | `/tmp/m2s5-res/check-*.log`、`check-ci-*.log` |
| `tools/build-app-core.sh` | **三道门全过**（`[sym] 20/20` / `[ver] 8f8234608f67-rust` / `[size] 4,685,216 B`） | `/tmp/m2s5-res/build-app-core.log` |
| `tools/check-quic-isolation.sh` | **九条全绿** | `/tmp/m2s5-res/final-isolation.log` |
| `tools/check-vocab.sh` | **PASS**（Rust 声明 5 单元 / 26 值；ledger sha256 一致） | `/tmp/m2s5-res/final-vocab.log` |
| 红线自查 | 本切片文件面 = `tools/**` + `docs/reviews/M2-S5-evidence.md`；**`crates/**` 零 diff** ⇒ `relay/**`、`relaywire.rs`、`server/intercept/**` 全未触碰 | `git diff --name-only 815dfd6..HEAD` |
| 副作用残留 | 本切片起的本地出口/中继实例（#2/#4/#6/#8）**全部 stop**；`homeway-cli serve --state /tmp/homeway-rs-rust*` 进程 = **0**；现役出口（`/Users/zhaozhe/bin/homeway-rs --state ~/.config/homeway-rs`，pid 33667）**未碰**（始终在跑） | 收口前 `ps` 实测 |

**建议 S6 承接（本切片不做）**：①D1 的 `SrcKey::of` v4-mapped 归一（**高危面**，一行 + 用例）；
②`serve status --json` 暴露 quic 段（含 `flood_refused`/`retry_sent`/`handshakes_in_flight`）；
③`RETRY_AFTER_FAILS`（5）与 `per_src_fails`（16）的同步裁决；④L1 采样器单位归一在
`tools/quic-ab.sh` / `tools/m1-ab-e2e.sh` 的同款修复；⑤真机（S5-4）待用户点头。

## 7. 复现命令（一条链）

```sh
# S5-1：门槛（独占机器；一运行一目录，防 loadavg.tsv 互相截断）
QUIC_AB_DIR=/tmp/m2s5-res/quic-ab-cpu tools/quic-ab.sh cpu --arms raw,wg-shim,wg-ring,quic --rounds 5
QUIC_AB_DIR=/tmp/m2s5-res/quic-ab-overhead tools/quic-ab.sh overhead --mtu 1400
QUIC_AB_DIR=/tmp/m2s5-res/mem-steady  tools/quic-ab.sh mem --mode steady --arms raw,wg-shim,wg-ring,quic --rounds 3
QUIC_AB_DIR=/tmp/m2s5-res/mem-load    tools/quic-ab.sh mem --mode load --arms raw,wg-shim,wg-ring,quic
QUIC_AB_DIR=/tmp/m2s5-res/mem-conns5  tools/quic-ab.sh mem --mode conns
QUIC_AB_DIR=/tmp/m2s5-res/mem-conns3  tools/quic-ab.sh mem --mode conns --conns-points 1,3,5
QUIC_AB_DIR=/tmp/m2s5-res/mem-conns32 tools/quic-ab.sh mem --mode conns --conns-points 1,2,3,4,5,8,16,32
QUIC_AB_DIR=/tmp/m2s5-res/mem-connsload        tools/quic-ab.sh mem --mode conns-load
CONNS_LOAD_NO_READ=1 QUIC_AB_DIR=/tmp/m2s5-res/mem-connsload-noread tools/quic-ab.sh mem --mode conns-load
QUIC_AB_DIR=/tmp/m2s5-res/size tools/quic-ab.sh size --profile lab,product && tools/build-app-core.sh

# S5-2/S5-3/S5-5：产品路径（本地私有出口 #2/#4/#6 + 中继 #2；跑完自己停）
cargo build --release -p homeway-cli                      # 工具坑②：不重建会跑到旧二进制
( cd tools/m1-ab && cargo build --release ) && ( cd tools/quic-probe && cargo build --release )
tools/m2-s5-e2e.sh 2 3        # A/A2 直连 A/B + A3 常态赛跑 + B1/B2/B3 判据1 + C/C2/D 判据2
                              # + E 判据3 + E2 附带损伤 + F 300ms 预算 + G 窄路径 + I 产品形态内存
```

产物目录：`/tmp/m2s5-res/{quic-ab-cpu,quic-ab-overhead,mem-*,size,e2e}/`（`SUMMARY.txt` /
`SUMMARY-flood.txt` / `loadavg.tsv` / 逐轮 `.json` / `*.log`）。**测试期间起的本地实例已全部停**。


