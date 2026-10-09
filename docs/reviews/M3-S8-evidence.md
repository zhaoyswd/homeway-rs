# M3 S8 门槛与真机验证读数台账（读数留证）

> **为什么建这份文件**（照 `docs/reviews/M1-S5-evidence.md` / `M2-S5-evidence.md` 的先例与理由）：
> S8 的原始读数分散在 `/tmp/m3s8-res/`、`/tmp/m3s8-*`（易失），而收口报告要**逐格**回答
> 「M3 的改动（STREAM 流面 + 阶梯 + 服务入口换轨）有没有让 M1/M2 登记的每包 CPU / 线开销 /
> footprint / `.so` 退化，以及服务流吞吐/共存相对门槛是否成立」——数字只活在 `/tmp` 里，
> S9 代码门与 M5 的删码余量判定都会变成「口说无凭」。故本文件 = **读数台账**
> （逐格给「实测值 + 基线/门槛 + 偏差 + 判据」+ 来源文件路径），**不改任何代码/判据**。
>
> 范围与边界：`docs/reviews/M3-design.md` §7（预算）+ §9（真机 R1–R7）+ §11-S8 行；
> 实施期订正以该文件 §15 为准（§15-3 授权流窗口/并发/队列初值标定；§15-2 快探参数 env 消融臂）。
> 判据行真源仍是 `docs/INTEROP-CRITERIA.md`（本文件不含新判据）。

---

## 0. 口径、机器与纪律（先读，否则下面的数字都不能用）

| 项 | 值 / 证据 |
|---|---|
| 机器 | Mac mini（Apple M2 / 8 core，Darwin 25.5.0 arm64）；`rustc 1.99.0` |
| 主指标 | 本地门槛：**每包 CPU**（`getrusage` user+sys µs / 成功往返数）；墙钟只作副读（PERF-AB §9.15.1） |
| 内存口径 | `vmmap -summary` 的 **Physical footprint**（`ps -o rss=` 不作判据——M0 已证不可用） |
| 中位口径 | **下中位**（harness `lower_median`，照搬 M0/M1/M2，勿「修」） |
| 独占纪律 | 每轮开工前查 `vm.loadavg` 1min ≤ 4；逐轮读数对照同目录 `loadavg.tsv`（见 §1.0） |
| 仪器同一性 | 六枚 `quic-ab` 臂二进制与 M1/M2 **sha256 逐字节同**（§1.0）⇒ 三期数字可比 |
| 本切片代码改动 | **只有测试/harness**：新增 `crates/homeway-core/tests/quic_stream_perf.rs`、`tools/m3-s8-perf.sh`、`tools/m3-s8-coexist.sh`；`crates/**` 产品代码**零改动**（S1–S7 已落地） |
| 读数落点 | `/tmp/m3s8-res/{cpu,overhead,size,mem-*,perf,coexist}/`；真机证据 `/tmp/m3s8-*.log|json|jpeg`；收口门 `/tmp/m3s8-gates/` |
| 真机 | `FMR0224116011480`（HUAWEI ALN-AL00，API 24；专用测试机，用户已授权） |
| 真机核 | **M3 版**（`fc2bb50db4b5-rust`；`.so` **4,850,272 B**）——在场证据 = M3 独有判据行（流面参数/快探参数/准入回执/服务流已开/桥泵，见 §3.0） |
| 真机出口 | **本地私有 Rust 出口**实例 #2（`/tmp/homeway-rs-rustexit-2`）；**现役出口 pid 33667 全程未碰** |

### 0.1 loadavg（首/末与峰值；逐运行）

| 运行 | 首 | 末 | 峰值 1min | 备注 |
|---|---|---|---|---|
| `cpu`（5 轮） | `{ 1.43 1.99 2.05 }` | `-- round 5 end` | **3.23** | 全程 ≤4 ✓ |
| `overhead` | `{ 2.96 2.48 2.23 }` | `-- overhead end` | ~3.0 | ≤4 ✓ |
| `mem steady` | `{ 2.59 2.42 2.21 }` | `-- mem(steady) end` | ~2.6 | ≤4 ✓ |
| `mem load` | `{ 2.33 2.32 2.20 }` | `-- mem(load) end` | ~2.4 | ≤4 ✓ |
| `mem conns`（五点 / 八点） | `{ 2.30 2.28 2.19 }` / `{ 1.71 2.11 2.13 }` | 各 `end` | ≤2.5 | ≤4 ✓ |
| `mem conns-load`（两档） | `{ 2.42 2.22 2.17 }` / `{ 2.35 2.20 2.16 }` | 各 `end` | ≤2.5 | ≤4 ✓ |
| `size`（lab+product 交叉构建） | `{ 2.24 2.22 2.17 }` | **`{ 4.53 2.70 2.34 }`** | **4.53** | 唯一 >4 的窗口；`size` 非计时读数 ⇒ 不作废 |
| `perf`（吞吐 A/B）/`coexist` | 未采集 loadavg（非 `quic-ab` 面） | — | — | 采用**逐轮交替 + 三轮一致**控噪（§2.1） |

来源：各目录 `loadavg.tsv`；`cpu` 的峰值见 `cpu/loadavg.tsv` 轮末。

---

## 1. S8-A 本地门槛（`tools/quic-ab.sh`；逐格对 M1/M2 登记值）

### 1.0 仪器同一性（先立这条，后面所有偏差才有意义）

| 臂 | M1/M2 快照 | M3 本轮（15:36:24 快照） |
|---|---|---|
| `raw` | `f8af4979…fce0` | `f8af4979…fce0` |
| `wg` | `b5f8347e…e013` | `b5f8347e…e013` |
| `wg_size` | `e3da3494…bbba` | `e3da3494…bbba` |
| `quic` | `2ac23dcf…b010` | `2ac23dcf…b010` |
| `multiconn` | `a1192654…8795` | `a1192654…8795` |
| `wg-shim` 的 `wg` | `2cc5d582…978a` | `2cc5d582…978a` |

来源：`/tmp/m3s8-res/cpu/bins.sha256`（六枚全同 M1/M2）⇒ **同一套测量仪器**。

### 1.1 每包 CPU（四臂全量；lab 档；1280B 载荷 = MTU1400 口径；`--rounds 5`）

| 臂 | M3 本轮（下中位） | 逐轮 | M2 登记 | M0 基线 | 对 M2 偏差 | ±10% |
|---|---|---|---|---|---|---|
| `raw` | **4.769 µs** | 4.807 / 4.748 / 4.769 / 4.726 / 4.905 | 4.851 | 4.882 | −1.7% | 过 |
| `wg-shim`（现役手机形态） | **14.624 µs** | 14.560 / 14.687 / 14.615 / 14.624 / 14.638 | 14.713 | 14.623 | −0.6% | 过 |
| `wg-ring`（诊断臂） | **10.775 µs** | 10.787 / 10.775 / 10.736 / 10.602 / 10.784 | 10.640 | 10.740 | +1.3% | 过 |
| `quic`（quinn+rustls(ring)+tokio，DATAGRAM） | **12.356 µs** | 12.198 / 12.985 / 12.535 / 12.356 / 12.179 | 12.250 | 12.668 | **+0.9%** | 过 |

- **M1 门槛（设计 §8）**：`quic ≤ 现役 WG+shim × 1.0` ⇒ **12.356 ≤ 14.624 = 0.845× 成立** ✓
- **M3 的改动对每包 CPU 无退化**：`quic` 相对 M2 登记 **+0.9%**（相对 M0 基线 −2.5%）；四臂全部落在 ±10% 带内。
  （M3 的改动在服务流面，DATAGRAM 每包路径未动 ⇒ 与设计预期一致。）
- 来源：`/tmp/m3s8-res/cpu/{summary.txt,cpu-*.json,loadavg.tsv}`、`/tmp/m3s8-res/cpu-run.log`。

### 1.2 线开销

| 项 | M3 本轮实测 | M2 登记 | 偏差 | 判据 |
|---|---|---|---|---|
| WG 每包线上字节（1280B 载荷） | **1312 B（开销 32 B）** | 1312 B（32 B） | 逐字节同 | ≤40B ✓ |
| QUIC 每包线上字节（oneway，服务端 `udp_rx` 口径） | **1310.181 B（开销 30.181 B）** | 1310.136 B（30.136 B） | **+0.045 B（+0.0003%）** | ≤40B ✓ |
| `max_datagram_size`（MTU1200 / MTU1400） | **1162 / 1362** | 1162 / 1362 | 精确 | 精确 ✓ |

来源：`/tmp/m3s8-res/overhead/summary.txt`。

### 1.3 内存（`vmmap` Physical footprint）

**(a) 稳态（单连接 hold，IDLE 9s，三轮下中位）**

| 臂 | M3 本轮 | M2 登记 | M0 基线 | 偏差（对 M2） | ±10% |
|---|---|---|---|---|---|
| raw | **960K** | 960K | 960K | 0% | 过 |
| wg-shim | **1008K** | 1008K | 1008K | 0% | 过 |
| wg-ring | **1552K** | 1552K | 1552K | 0% | 过 |
| **quic** | **1248K** | 1216K | 1232K | **+2.6%**（对 M0 +1.3%） | 过 |

逐轮：`quic 1232/1248/1264`（来源 `mem-steady/mem-steady.txt`）。

**(b) 负载态（N=300k 传输中 max footprint）**：raw 960K / wg-shim 1008K / wg-ring 1552K /
quic **1248K**（M2 同格 960/1008/1552/**1296**）⇒ 最大偏差 **−3.7%**（quic）✓

**(c) 每连接边际（`multiconn` 服务端；五点口径 = 设计 §4.3）**

| 口径 | 点集与逐点值（K） | 拟合（本轮） | M2 同口径 | 判据 |
|---|---|---|---|---|
| **五点（缺省）** | 1/1248 2/1408 3/1472 4/1568 5/1568 | **base=1212.8K，边际 80.00K** | base=1200.0K，84.00K | **≤96K ✓**（−4.8%） |
| 八点（含 **32**） | 1/1264 2/1408 3/1504 4/1520 5/1616 8/1760 16/2144 **32/2720** | base=1343.8K，边际 44.86K（端点斜率 46.97K） | — | 见 (d) |

**(d) 32 设备稳态（§9.1-2 修订门槛 ≤ +3.1 MiB）**

- 直测：`N=32 → 2720K`，同轮基线（N=1）1264K ⇒ **增量 1456K = 1.42 MiB ≤ 3.1 MiB ✓**
  （M2 同格：2720K − 1280K = 1360K = 1.33 MiB ⇒ M3 基本同档 +3.5%）
- 保守斜率复核：五点 80K × 32 = 2560K = 2.50 MiB ✓；八点端点 46.97K × 32 = 1503K = 1.47 MiB ✓

**(e) 负载态门槛（§9.1-4：32 连接 + 持续流量 ⇒ 增量 ≤ 64 MiB + 自有队列上限）**

| 档 | 空转 | 负载态中位 | 增量 | M2 同格增量 | 判据 |
|---|---|---|---|---|---|
| N=32 × 200pps × 1280B | 2752K | **3424K** | **+672K** | +832K | ≤65536K ✓（改善 −19%） |
| 同上 + 客户端**不读回程**（`--no-read`） | 2704K | **3984K** | **+1280K** | +752K | ≤65536K ✓（大余量） |

- 口径注记照 M1/M2 原样（勿当「缓冲已测透」）：**quinn 的 datagram 收发缓冲按需增长**；
  64 MiB 是解析上界，在回环上不可达。「把缓冲真顶满」= M6/真机面。
- 来源：`/tmp/m3s8-res/mem-*/{summary.txt,mem-conns.txt,mem-conns-load.txt}`。

### 1.4 体积（`tools/quic-ab.sh size --profile lab,product` + `tools/build-app-core.sh`）

| 格 | M3 本轮实测 | M1/M2 登记 | 偏差 | 判据 |
|---|---|---|---|---|
| lab 档 空壳 cdylib | **323,408 B** | 323,408 B | 逐字节同 | ±10% ✓ |
| lab 档 空壳 + QUIC 全栈 | **778,032 B** | 778,032 B | 逐字节同 | ±10% ✓ |
| lab 档 boringtun+smoltcp 在场 + 真实引用全路径 | **1,735,384 B** | 1,735,384 B | 逐字节同 | ±10% ✓ |
| product 档 同三格 | 326,120 / 852,568 / **2,496,256** B | 同三格逐字节同 | 0 | 只登记 |
| **真产品面 `libclientcore.so`** | **4,850,272 B**（`[size]`，`[sym] 20/20`，`[ver] fc2bb50db4b5-rust`） | M2 收口 **4,685,472 B** | **+164,800 B（+3.52%）** | 3.8MB 阈值按设计属 **M5 判**（本值 = 3.8MB 的 **1.276×**） |

- **M3 体积代价分解（逐切片登记值）**：S1 +133,760 B → S4 +43,336 B → S6/S7 +2,224 B
  ⇒ 累计 **+179,320 B** 的中间读数与两期收口差 **+164,800 B** 的差额（−14,520 B）= S3/S5 的
  净删/改（客户端换轨净删 + 准入面净增，S3 记录：相对 S2 读数 **−6,040 B**）+ 轮间噪声。
  **以本行两期收口实测差为准**：**M3 净增 +164,800 B**。
- **交 M5**：该增量进「M5 删码余量重算」输入（`docs/reviews/M5-design.md` 须据此重估）；
  M3 自身**不判死** 3.8MB（设计 §7 明列）。
- 来源：`/tmp/m3s8-res/size/summary.txt`、`/tmp/m3s8-res/build-app-core.log`。

---

## 2. S8-B 服务流吞吐相对门槛 + bulk/L3 共存 A/B（设计 §7/§15-3；**本切片新增仪器**）

### 2.0 仪器（新增，随本台账入库）

| 件 | 说明 |
|---|---|
| `crates/homeway-core/tests/quic_stream_perf.rs` | `#[ignore]` 用例：App 核真世代 + 隧道桥 + 真 FilesServer，**同一条产品路径**上做 files 大文件下载并计时；**唯一变量 = 承载**（`transport=quic|wg`）。读数一行：`[perf] transport=… bytes=… secs=… mibps=…` |
| `tools/m3-s8-perf.sh` | 起干净本地私有出口 → 灌 token → **逐轮交替**跑两臂（轮序奇偶反转）→ 汇总 `SUMMARY.txt` |
| `tools/m3-s8-coexist.sh` | 基线臂（`m1-ab` 空载 L3 容量档）vs 共存臂（同刻后台跑一条 files STREAM 下载）|

**两臂的产品路径差异（必须写清，否则读数会被误读）**：
`quic` 臂 = 岛 `STREAM[tag=files]` → 出口 tag 分发 → intake → socketpair 泵 → FilesServer；
`wg` 臂 = WG 会话 → 隧道 IP `:7802` → intercept 豁免 → UDS → **同一个** FilesServer。
文件、字节数（67,108,864 B）、源 sha256、进程内同一套桥/泵代码**均相同**。

### 2.1 服务流吞吐相对门槛（**同刻交替，3 轮**；64 MiB 下载）

| 轮 | quic 臂（MiB/s） | wg 臂（MiB/s） | 比值（quic/wg） |
|---|---|---|---|
| 1（quic→wg） | 23.40 | 52.00 | 0.450 |
| 2（wg→quic） | 23.80 | 51.29 | 0.464 |
| 3（quic→wg） | 24.69 | 51.51 | 0.479 |
| **下中位** | **23.80** | **51.29** | **0.464×** |

- **判据（设计 §7/N15）**：同一服务操作吞吐 **≥0.95× WG/UDS 档读数** ⇒ **未过（0.464×，差 2.15×）**。
  **不许粉饰**：这是本切片最重要的负面读数。两臂三轮各自极稳（quic ±2.7% / wg ±0.7%），
  轮序平衡 ⇒ 不是噪声、不是冷启。
- **交叉印证（出口侧同一读数面）**：出口日志 `quic: 服务流结束（tag=files，↑41B ↓67113036B，
  耗时 2.69–2.93s）`（出口侧 elapsed 2.69/2.73/2.85/2.93s）与客户端计时（2.59–2.74s）一致
  ⇒ 慢在**服务流管道本身**（岛↔客户端泵/命令面），不是某一侧计时口径。
- **可能成因（未定论，交 S9/M5）**：客户端侧读交接通道 = 每次 `Cmd::StreamRead` 一次命令往返
  （设计 §5.3-A14 已登记「无界 mpsc」的同一处）；岛/出口 `current_thread` 上的每流拷贝开销；
  以及 §1.7 的待发队列 64 KiB 与 `send_window` 2 MiB 的联合效应。**本切片不改产品代码**
  （红线：测量切片）。本地口径 vs 真机口径：真机 App 测速（4×STREAM 并行）实测 **≈9 MB/s/流**
  （§3.3），与本地 24 MiB/s 同量级偏低 ⇒ 该瓶颈在真机同样存在。
- 来源：`/tmp/m3s8-res/perf/SUMMARY.txt` + `perf-{quic,wg}-r{1,2,3}.log`（逐轮原文）。

### 2.2 bulk（L3/DATAGRAM）与服务流（STREAM）共存 A/B（设计门 M-1 的直接读数）

口径：同一本地出口、同一 QUIC 承载、同一 L3 产品路径（`tools/m1-ab` 的 ClientCore 世代 +
TUN socketpair + 合成 UDP 流，`--rate 0 --window 64 --secs 8` = 容量档）；只变**服务流是否在跑**。

| 轮 | 基线臂（空载）reply_bytes | 共存臂 reply_bytes | 相对差 | 共存中服务流读数 |
|---|---|---|---|---|
| 1 | 135,014,428 | **135,427,588** | **+0.31%** | 23.89 MiB/s |
| 2 | 128,495,264 | **129,201,392** | **+0.55%** | 24.13 MiB/s |

- **结论**：L3 吞吐在「同刻一条 24 MiB/s 服务流在跑」时**未观察到拖累**（±0.6%，且方向为正）
  ⇒ 设计门 M-1 的「相互拖累」风险**本机口径未显现**（两侧同刻合计 ≈335 Mbps 量级）。
  两臂 L3 读数与 M1/M2 容量档（136/108 Mbps 级）同带；服务流速率与 §2.1 单跑时同值（23.9/24.1 vs 23.8）
  ⇒ 也**没有服务流被 L3 拖累**的迹象。
- **限制（如实）**：本机回环 + 单 DAU 形态；真机「边下文件边浏览」面见 §3.3（未单独构造）。
- 来源：`/tmp/m3s8-res/coexist/{SUMMARY.txt,base-r*.log,coex-r*.log,stream-r*.log}`。

---

## 3. S8-C 真机验证（R1–R7；设备 `FMR0224116011480`）

### 3.0 前置（出包/装机/承载/版本事实）

| 项 | 值 / 证据 |
|---|---|
| 核版本 | `fc2bb50db4b5-rust`（build-core 版本注入校验 + `[size] 4,850,272`）|
| tier 出包 | `build-core.sh` 的 pin 门（HEAD 是 pin `cbd45f0439e6` 的后代）**放行**；**log-index 门红**（既存，M1 已登记）⇒ 按手册逃生口**手工双落盘** `.so`（两处 md5 一致 `0e562e6511bd738d25fbe35f0764f4f9`；4,850,272 B） |
| 包 | `entry-default-signed.hap` 2,724,698 B / `tailcat-default-signed.hsp` **2,889,328 B**（HSP 内 `libs/arm64-v8a/libclientcore.so` = 4,850,272 B）/ `terminal-default-signed.hsp` 4,166,072 B（三个 hvigor 目标 BUILD SUCCESSFUL） |
| 装机 | `bm install -p /data/local/tmp/mod` → `install bundle successfully.`（覆盖装）|
| 连接链 | `aa start --ps host_token '<hmw1…>'`（免点屏）→ `uitest uiInput click 1086 778` 点 VPN 开关 → 核日志 `transport: 本世代 L3 承载 = quic` / `quic: 准入完成` / `岛已建连…L3 承载 = 岛` / `warmup pong: 就绪（判据=quic）` / `link: via=direct ep=192.168.3.12:42653 rtt=…` |
| **M3 在场证据（M2 无此三行）** | `quic: 流面参数（bidi=64 uni=0 recv_window=262144B send_window=2097152B 待发=65536B；有效服务流 62；env 覆盖 0 项）` / `quic: 快探参数（首探 700ms，拍间 250ms，复探 ×2，待机 60s，抖动阈值 3，B 门 连续 2/窗 10s，发送面新鲜度窗 5s，在用窗 5s；env 覆盖 0 项）` / `quic: 准入回执（…）` |
| 真机 `.so` 加载面 | 扩展进程 `me.zhaozhe.tier:vpn`：`VmRSS 127,148 kB / VmHWM 131,676 kB / Threads 30`（口径：进程级，含 OHOS 运行时 + 三座桥 + 扩展壳，**非核独占**；对照 M1 同格 134,984/137,292/37） |

### 3.1 R1 files（**部分过**：列目录 ✓ / 下载 ✓ / 上传 ✗ 自动化受阻）

| 步 | 结果 | 两侧原文证据 |
|---|---|---|
| **列目录** | **过** | 核：`quic: 服务流已开（tag=files；第 1 条）` / `已关（id=1 tag=files，↑0B ↓44B）`、`（第 2 条，↑25B ↓8766B）`（根目录 8.7 KB 应答，条目与 Mac `$HOME` 实况一致：`.0m3s8/.Huawei/.Trash/.acme.sh…`）；截图见 `/tmp/m3s8-ui5.json|ui6.json`（`.0m3s8` 行在行首＝字节序排序语义在位） |
| **下载** | **过（sha256 一致）** | 出口：`quic: 服务流已受理（tag=files dev=aca645d3 第 3 次）`（15:54:51.281）+ `服务流结束（tag=files，↑41B ↓1048715B）`；核：`桥泵[down] EOF（累计 1048715B / 783 次）`；**设备侧缓存件 sha256 `3bf2151a…b528` = 源件 sha256 `3bf2151a…b528`**（1,048,576 B；`/tmp/m3s8-dl-recv2.bin` vs `~/.0m3s8/dl.bin`） |
| **上传** | **✗ 未做（自动化受阻，如实登记）** | 系统文档选择器**可驱动到「已选 1/1」**（`uitest uiInput click` 选中文件行），但**点「完成」不产生回传**：选择器关闭、App 未收到 URI（`doc.select()` 走 `uris.length===0` 的静默分支）⇒ 无 upload 流、无出口行、Mac 侧无文件。缺的一环 = **系统 picker 的完成按钮不接受注入点击**（tier `docs/agents/uitest.md` §3.3 已登记「系统文档选择器不自动化」）。**替代证据**（不冒充 R1）：上传方向由真机 speedtest 的 uplink 与本地 `[e2e4]` 的 files 帧面覆盖 |

### 3.2 R2 term（**过**；分离用 App 的返回语义替代 `Ctrl-b d`）

| 步 | 结果 | 两侧原文证据 |
|---|---|---|
| 起会话 | **过** | App：`终端 → 新建会话` 进入终端页（`term-btn-bar`/`term-btn-kb` 在位）；出口：`term: 新建会话 90878f9d（pid=85867 49x34 shell=/bin/zsh）` + `term: 会话 90878f9d 腿接入（kind=app 49x34 id=4f273b3068220976 首腿=true）n=1/8` |
| 回显 | **过（截图逐字节）** | 键入 `echo m3s8-term-ok`：屏幕 `zhaozhe@zhaozhedeMac-mini-6 ~ % echo m3s8-term-ok` → `m3s8-term-ok` → 新提示符（`/tmp/m3s8-scr7.jpeg`）；出口：`term: 会话 90878f9d 状态 shell/idle` |
| 分离 | **过（形态替代）** | 设备键盘栏**只有 ESC/TAB/退格/回车/粘贴/Copy —— 无 Ctrl 键** ⇒ `Ctrl-b d` 无法注入；改用 App 自身的分离面（返回键）：出口 `term: 会话 90878f9d 腿断开（kind=app 原因=client_closed）｜快照=1 差分=19 降级=0 背压=0 队列溢出=0 编码失败=0 分片=21 下行=2461B FETCH 命中=0 落空=0` |
| re-attach | **过（内容回放 + 同会话号）** | 会话列表仍在（`sessions-row-90878f9d · Shell · 刚刚 · 空闲`）→ 点回：出口 `term: 会话 90878f9d 腿接入（kind=app 49x33 id=4f273b3068220976 首腿=true）n=1/8`（**同一会话号**）；屏幕恢复出分离前的命令与输出（`/tmp/m3s8-scr8.jpeg`） |

- 未做（登记）：**跨世代重建的 re-attach**（断开-重连后恢复）——本切片未构造「term 会话在跑时
  世代重建」的相位；出口侧会话持存已由上面的「腿断开→腿接入」与 §3.4 的世代重建读数间接支撑。

### 3.3 R3 speedtest（**过**；含一次**出口进程死亡**事件，见 §4-1）

| 轮 | 出口侧证据（原文节选） | 核侧证据 |
|---|---|---|
| 第 1 次（15:57:10，**异常轮**） | `speedtest: 会话 #1..#4 role=recv warmup=2s window=10s`；**无结算行**（进程随后死亡，§4-1） | 核 `桥泵[down] 写失败 after ~74.8MB：Broken pipe` ×4 ⇒ 服务流侧 300 MB 级 bulk |
| 第 2 次（16:00:24，**正常轮**） | `quic: 服务流已受理（tag=speedtest dev=aca645d3 第 2/3 次）`；结算 `speedtest: 会话 #4 role=recv bytes=91814535（含预热 16252680）用时=12007ms` + 归因 `下行逐秒MB=[8.8/9.1/9.2/9.1/9.2/9.4/8.7/7.4/8.0/8.6] 尾3片=67Mbps`（四会话各 ≈91–92 MB / 12 s ⇒ **≈9.0 MB/s/流、四流合计 ≈36 MB/s**） | `quic: 服务流已开（tag=speedtest；第 1/2/3 条）` + `桥泵[down] EOF（累计 ~1.08×10⁸ B / 8 万次）` ×4；**出口进程存活**（RSS 14.5 MB → 20.2 MB） |

- 判据「出口 E13 结算行在场 + 核 `tag=speedtest` 受理行」⇒ **过**（第 2 次为判据面；
  第 1 次因 §4-1 事件中断，其结算行缺席已如实登记）。
- 与 M2 同档对照：口径不同（M2 = App 卡片文本步进 1 MB/s；本轮 = 出口结算字节/用时），
  仅登记不判死；**注意**：本读数 ≈36 MB/s 与 M1 真机卡片读数（↑33–35 MB/s）**同带** ⇒ 无数量级回退。

### 3.4 R4 恢复 ≤3.5s（**过**；并用样登记了「待机档」的长尾）

**判据面（在用档 = App 前台 + 浏览器导航持续产流）**，`kill -9` 本地出口（`/tmp/m3s8-r4i-{A,B}/`）：

| 相位 | kill 时刻 | `快探失败`（T_detect） | 出口重新监听（E1 `serve 就绪`） | `链路重连完成` | **T_recv** | 判据 |
|---|---|---|---|---|---|---|
| A（立刻重启；停机 ≈0.07s） | 15:52:42.341 | +2.319s | 15:52:42.415 | 15:52:44.711 | **2,296 ms** | ≤3500 ✓ |
| B（停机 5s） | 15:53:16.935 | +2.393s | 15:53:22.041 | 15:53:22.888 | **847 ms** | ≤3500 ✓ |

- 相位 B 的完整链（原文）：`链路重连中（第 1 次）` → `赛跑小结：无胜者（候选 1 个，耗时 2.803s）`
  → `链路重连失败（…第 1 次）—— 交世代重建` → `交另一动作（M↔R；窗 2.807s / 门 2 次未到）`
  → `本地 socket 已换绑` → `迁移未确认（…700ms 内无对端回包）` → `链路重连中（第 2 次）`
  → `赛跑结算：胜出 直连 192.168.3.12:42653（耗时 33ms）` → `准入完成（15ms）` → `链路重连完成（耗时 5ms）`
- 与本地读数对照：本地（S4）T_recv **2455 / 1315 ms**、T_detect **2514 / 2520 ms**；
  真机 T_detect **2319 / 2393 ms**、T_recv **2296 / 847 ms** ⇒ 同带、无退化。
- **待机档反例读数（重要，如实登记）**：同一设备在**无 TUN 流量**（App 前台但无出站流量、
  in-use 窗口失效 ⇒ 快探走 60s 待机拍）时，`kill -9` 出口后**首个失败信号在 +32.06s**
  （15:49:38.3 → 15:49:57.917，原因=`探活无回显`；重连本身仍只需 6ms）。
  ⇒ **≤3.5s 的门限口径 = 在用档**（设计 §9 R4 前置列「App 前台/有流量」）；待机档的恢复时间由
  **QUIC 空闲回收 30s + keep_alive 相位**支配（与 §15-5 的根因订正同源），**不是 M3 阶梯回归**。

### 3.5 R5 瞬时黑洞（**未做——缺一环，如实登记**）

- 设计要的形态：把设备侧 QUIC 端点指向 `tools/quic-wedge-proxy.py` 再投放 1.5s 丢窗。
- **缺的一环**：**token 的 QUIC 端点改写面**——产品 CLI 的 `token` 只有 `--dead-direct`
  （WG/Direct 端点）；`--quic-ep` 只存在于 **harness** `tools/m1-ab`（其 `token_rewrite` 不外露
  成可注入 App 的子命令）。设备侧 env 不可设（§3.7）⇒ 无法把设备引到楔子上。
- **替代证据（不冒充）**：本地 `tools/quic-ladder-e2e.sh 2` 的负向①「瞬时黑洞 1.5s ⇒ 只记抖动、
  不动作」已过（S4 记录：抖动 1 条、`action=""`）；真机侧未复跑。

### 3.6 R6 准入失败归因（**过**）

形态：**篡改 token 的 secret 一字节 + 重算 CRC（SHA-256(前文)[:4]）**（token 格式见
`crates/homeway-core/src/token.rs:7`），经 `--ps host_token` 注入 ⇒ 客户端的 4 帧准入 Proof MAC 必错。

| 侧 | 原文证据 |
|---|---|
| 核 | `quic: 准入已发起（dev=aca645d3，Hello 50B；等挑战/回执）` → **`quic: 准入失败（登记失败（准入被拒（code=0x11））；预算 4.971s）—— 连接已显式关闭（不留悬挂）`** → **`quic: 准入回执（code=0x11 凭证不被接受）——本世代回落 WG 承载`** → `quic: 赛跑未成（准入被拒（code=0x11））` → `岛未就用（…）——本世代回落 WG 承载（L3 与判据行按 WG 档；下一世代重试）` |
| 负例（顺手采到） | 只改 CRC 不改 secret ⇒ 客户端侧 **`token 解析失败：homeway/token: 校验失败（串被截断或损坏）`**（App 日志），不进入准入面 ⇒ 「解析失败 ≠ 准入被拒」两分在位 |

### 3.7 R7 服务面不外溢（A/B；**本地**，`serve.quic=false` 档）

- 执行面 = `tools/quic-wg-e2e.sh 1`（`--quic=false` 出口 + 两条用例）——**归 §5 收口门读数**。

---

## 4. S8-D 新发现（真机专有 / 与本地不一致；按重要性排序）

### 4-1 **本地出口进程在一次真机 speedtest（下行 bulk）中死亡（单次，未复现）**——交 S9/M5

- **现象**：出口 pid 72134（15:53:22 起）在 15:57:17.216 后再无日志（末行 = `intercept: reactor 观测…`，
  即**运行中突然终止**）；`local-rust-exit.sh status` = 「未在跑」。当时设备侧正在跑 speedtest
  下行（4 条 STREAM，出口侧已推 ~75 MB/流）。
- **两侧原文**：设备核 15:57:20.391 `桥泵[down] 写失败 after 74867251B / 75092704B / 75044341B / 74928051B：Broken pipe`；
  此后 15:57:25.928 起 `链路重连中` ×4 全败（`赛跑小结：无胜者（耗时 ~2.8s）`）→ 15:57:39.278
  `世代重建（…连续重连失败 4）` → 新世代 `赛跑未成` ⇒ `回落 WG 承载`。
- **已排除/已查**：出口日志**无 panic 文本**、**无收工行**（`收到停止信号` 缺席）、
  `~/Library/Logs/DiagnosticReports/` **无新 .ips**、统一日志该窗口无 termination 记录。
- **未复现证据（同规格复跑）**：16:00:24 同一 App/同设备/同出口身份，speedtest 4 流
  **各 ≈91–92 MB 全部结算**，出口存活（RSS 14.5→20.2 MB，见 §3.3 第 2 次）。
- **归因未定（如实）**：候选 = ①单机资源/内存面（speedtest 内存收发）；②本棒沙箱环境对
  「非本会话启动的 homeway-cli」的处置（本会话另有 ASP 拒绝新 homeway-cli 启动的记录，
  见 §6）；③M3 服务入口换轨（intake/泵）下的某条资源面。**M3 侧无一条读数能证明或证伪**
  ⇒ 需 S9 代码门 + M5/M6 专门构造（建议：真机 speedtest 与出口 RSS 采样同跑）。
- **影响面**：真机 speedtest 期间出口死亡 ⇒ 世代重建 + 回落 WG（**用户可见的断流 ~40s**）。
  与本切片的其它判据不冲突（R3 第 2 次已过），但**必须记入 M5 风险**。

### 4-2 **服务流吞吐低于 WG/UDS 档 46%（相对门槛未过）**——见 §2.1

- 本地 24 MiB/s vs 51 MiB/s（同刻交替 3 轮，轮序平衡）；真机 speedtest 9 MB/s/流 交叉印证。
- 与 §2.2「共存不拖累」并存 ⇒ 瓶颈是**服务流管道自身**，不是与 L3 争用。

### 4-3 **待机档恢复长尾 32s**（见 §3.4）：≤3.5s 只在**在用档**成立；`QUIC 空闲回收 30s + keep_alive`

### 4-4 **R1 上传的系统 picker 完成按钮不可注入**（见 §3.1）：真机「上传」面**当前不可自动化**，
需 tier 侧补一条**非 picker 的注入缝**（或用户手工一次）。

### 4-5 C19/E-q5 节流窗**跨 tag 共享**（probe 占位）在真机复现：出口重启后 probe 先占「第 1 次」，
本轮 files 的受理行落在「第 2/3 次」（`首 3` 用尽后即静默）⇒ 判读脚本必须按「或」形态
（与 S4/S6 的登记一致，本切片拿到真机实证）。

---

## 5. S8-E 收口门（本切片实测）

| 门 | 命令 | 读数 |
|---|---|---|
| 全量测试 | `cargo test --workspace` | **18 个测试目标全 ok / 0 failed**；`homeway-core --lib` **708 passed / 4 ignored**；`homeway-quic --lib` **178 passed**（新增 `quic_stream_perf` = 0 passed / 1 ignored，符合 `#[ignore]` 形态）。**零 flake 复现**（S6/S7 的两例 load-sensitive flake 本轮未出现） |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | **0 警告（rc=0）** |
| 三目标 check | OHOS = `CC=NDK clang`；musl×2 = `CC=clang` + `-nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem tools/cc-check-shim`（照 `.github/workflows/ci.yml:81-83`） | 三目标 **0 error**；仅余**既存**警告（`go_fmt.rs:69` 的 `libc::time_t` deprecated + musl 的 cdylib 提示）。**首轮本机 harness 漏设 musl 的 `CC_*` env ⇒ ring 构建脚本失败（rc=101）——harness 环境缺陷（非代码回归），补 CI env 后复跑 0 error（如实登记）** |
| app 核构建 | `tools/build-app-core.sh` | `[sym] 20/20`；`[ver] fc2bb50db4b5+dirty-rust`（dirty = 本切片未提交文件）；`[size] **4,850,272 B**`。**提交后干净树复跑**（HEAD `66c306b`）：`[ver] **66c306b49a31-rust**` + `[sym] 20/20` + `[size] **4,850,272 B**`（同值）⇒ 本切片**零产品代码改动**，体积面不动 |
| 隔离门 | `tools/check-quic-isolation.sh` | **11/11 全绿**（⑪「QUIC 档零 `stackb::`」：岛 32 文件剥注释零命中 / raw 67 自校准；拨号缝桥闭包块 22 行 `dial=1/session_connect=1`；管线自校准 2/2） |
| 词表门 | `tools/check-vocab.sh` | **PASS**（Rust 声明 5 单元 / 26 值；ledger sha256 一致；缺席表 4 项在册） |
| 岛侧 e2e | `tools/quic-island-e2e.sh 1`（先 `wipe 1`） | **五条全绿 rc=0**；含 `[e2e5] exit.exempt_lines=0（换轨负判据）`（服务流不再经 WG 服务腿） |
| `_wg` e2e（**R7 判据面**） | `tools/quic-wg-e2e.sh 1`（先 `wipe 1`） | **两条全绿 rc=0**；C2/C4/C5/C6/C10/C15 **原串在场**、`quic_lines=0`、`intercept: tcp exempt 100.64.255.1:7724 ← 100.64.77.234:43301（dialok）`（WG 服务腿 UDS 入口在位）⇒ **R7（D1 形态）全绿** |
| 阶梯 e2e | `tools/quic-ladder-e2e.sh 2`（先 `wipe 2`） | **四条全绿 rc=0**：相位A `T_recv=**2472ms**`（`T_detect=2512ms`）/ 相位B `T_recv=**1317ms**` / 负向①只记抖动（`action=""`）/ 负向②动作落纸（`action=migrate`） |

来源：`/tmp/m3s8-gates/{workspace-test.log,clippy.log,check-*.log,isolation.log,vocab.log,build-app-core.log,e2e-*.log}` 与 `/tmp/m1s2b-res/`、`/tmp/m1s3-res/`、`/tmp/m3s4-res/`；汇总见 `/tmp/m3s8-gates-run.log`。

---

## 6. 仓内副作用残留（如实登记）

- 新增（本切片）：`crates/homeway-core/tests/quic_stream_perf.rs`、`tools/m3-s8-perf.sh`、
  `tools/m3-s8-coexist.sh`、`docs/reviews/M3-S8-evidence.md`（本文件）。
- `/tmp`（仓外）：`/tmp/m3s8-res/`（cpu/overhead/size/mem-*/perf/coexist 全部原始件）、
  `/tmp/m3s8-*.{log,json,jpeg,txt}`（真机证据）、`/tmp/m3s8-gates/`（收口门读数）、
  `/tmp/m3s8-dl-recv*.bin`、`~/m3s8-perf.bin`（64 MiB 源件）、`~/.0m3s8/`（真机 R1 用目录）、
  `~/m3s8-dl.bin`。
- 设备侧：App 覆盖装为 **M3 版**（`.so` 4,850,272 B）；VPN 世代状态与主机表条目（本棒注入
  1 台正常 token + 1 台伪造 token「主机 109」）**留档**；`Download/m3s8-up.bin`（3 MB，上传尝试用）
  与 App 缓存 `cache/tier-files/dl.bin`（1 MiB）留在设备。
- 本地私有实例：#1（e2e）/ #2（真机对端 + 阶梯 e2e）/ #5（吞吐 A/B）**收工时已全部 `stop`**；
  **现役出口 pid 33667 全程未碰**（收工复核仍在跑）。
- 设备收工状态：`aa force-stop me.zhaozhe.tier`（VPN 世代与扩展进程随之收工）；App 内主机表留下
  本棒注入的 2 条（正常 token 1 条 + 伪造 token「主机 109」1 条，**留档勿删**——它是 R6 的复现件）。
- `$HOME` 测试件已清（`~/.0m3s8/`、`~/m3s8-dl.bin`、`~/m3s8-perf.bin` 64 MiB）；`/tmp` 读数按惯例留档。
- tier：`git status --porcelain` **前后对比 = 仅 `?? openspec/changes/term-local-scrollback/` 一条
  untracked**（开工前即存在）：

  ```
  开工前 /Users/zhaozhe/Documents/projects/tier status：
  ?? openspec/changes/term-local-scrollback/
  收工后：同一条（零新增脏文件；`.so` 双落盘与 build 产物均 untracked）
  ```
