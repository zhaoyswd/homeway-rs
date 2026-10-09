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

---

## 7. S9 整改批（**服务流吞吐定位 + 整改**与**出口死亡随访**；2026-10-09）

> 本节的用途与口径同 §0：**逐格给「实测值 + 来源」**。为什么续在本文件而不是另起：
> S9 的输入就是 §2.1 的负面读数（0.464×）与 §4-1 的出口死亡事件——同一套仪器、同一台机器、
> 同一出口形态，续写才能逐值对照（**改前/改后同仪器**是本批的判据形态）。
> 纪律照旧：改前/改后都**不碰现役出口**（一切读数走 `tools/local-rust-exit.sh` 的本地私有实例）；
> `relay/**`、`relaywire.rs`、`server/intercept/**` 零 diff；判据行**格式**未动
> （`quic: 流面参数（…）` 逐字不变，只有 `recv_window=` 的**读数**随配置变化——值不是措辞）。

### 7.0 仪器（同一性先立，后面所有偏差才有意义）

| 件 | 改前 | 改后 |
|---|---|---|
| `tools/m3-s8-perf.sh`（S8 建） | 逐轮交替 3 轮、64 MiB files 下载、唯一变量=承载 | **同左（未动一行）** |
| `crates/homeway-core/tests/quic_stream_perf.rs` 的单流用例 | S8 版逐字节未动 | **同左**（新增的是**另一个** `#[ignore]` 用例，见下） |
| 新增仪器 | — | `stream_files_parallel_download`（N 流并行 bulk；`HOMEWAY_PERF_PARALLEL/ROUNDS`）+ `tools/m3-s9-bulk.sh`（逐轮读数 + 出口进程 **RSS/线程/fd** 监测 + **退出形态判定**：无「收到停止信号」收工行 ⇒ `UNCLEAN` + 末 40 行 + crash report 清单） |
| 临时测量补丁（**已 revert，不在库内**） | — | `exit/pump.rs::downstream` 的停等账（每次 `write_all` >200µs 即累计；临时 `[TEMP-DIAG]` 行） |
| 负载闸 | 每轮前查 loadavg ≤ 4 | 同左（本批全程 1.2–1.5） |

读数落点：`/tmp/m3s9-res/{baseline-run.log,cal-*,rep-*,diag-*,win*,rc64-*,sw8-*}`、`/tmp/m3s9-res/bulk/`、
`/tmp/m3s8-res/{perf,coexist}/`（改后覆写；S8 原件已备份 `/tmp/m3s9-res/perf-s8-orig`、`coexist-s8-orig`）。

### 7.1 定位：**逐段拆解 + 窗口消融**（每段都有读数，先排除再确认）

**嫌疑逐条排除（同一仪器：单流 64 MiB 下载，客户端接收窗用 `HOMEWAY_QUIC_STREAM_WINDOW` 变）**：

| # | 嫌疑（S8 点名） | 实验 | 读数 | 结论 |
|---|---|---|---|---|
| 1 | A14 读交接通道（`files.rs:213-229` 形态） | 本棒走的是**桥 UDS 形态**（App 核），不经 `files.rs` 的读线程；`READ_CHUNK` 16 KiB→64 KiB 临时改（岛侧读块 ×4） | 4 MiB 窗下 69.97/71.87 MiB/s（改前 70.97/70.89） | **排除**（读块粒度不敏感） |
| 2 | 岛内每流拷贝 / `Bytes`↔`Vec` | 同上（受 1 的读数覆盖：若每 16 KiB 的岛内搬运是瓶颈，×4 块必见效） | 同上 | **排除**（残余瓶颈不在岛内单块搬运） |
| 3 | **待发队列 + `send_window` 与 quinn 流控** | ① 出口侧 `SEND_WINDOW` 2 MiB→8 MiB 临时改；② 客户端 `stream_receive_window` 逐档消融 | ① 70.27/71.00 vs 对照 69.97/71.87 ⇒ **零影响**；② **见下表——强相关** | **确认（是"接收窗"这一半，不是 `send_window`）** |
| 4 | sink 泵单次读块（`exit/pump.rs` 8 KiB） | ① 泵停等账（下）；② `COPY_BUF` 8 KiB→32 KiB 临时改（4 流并行臂） | ① 停等 = 纯流控等待（下）；② 73.38/73.48 MiB/s（对照 73.75–75.31） | **排除**（8 KiB 读块不是吞吐上限；它是**流控等待的观测点**） |

**每流接收窗 W → 吞吐（同仪器，逐轮原值）**：

| W | 逐轮 MiB/s | 代表值 | 说明 |
|---|---|---|---|
| **256 KiB**（M3-design §1.7 缺省 = 改前） | 24.28 / 23.00；24.42 / 23.32 / 24.26 | **≈24** | S8 负面读数所在档 |
| 512 KiB | 21.88 / 18.12；18.09 / 27.93 | ≈21（**抖动大**） | 未见增益——平顶区的噪声 |
| 1 MiB | 23.45 / 23.32；24.12 / 24.23 | ≈23.5 | **平顶**（与 256 KiB 同值） |
| 2 MiB | 47.40 / 41.69 | ≈44 | 跳出平顶 |
| **4 MiB** | 72.12 / 72.43；71.40 / 72.56 | **≈72** | 本批取值 |

**"平顶 ⇒ 跳变"的机制（定量闭合）**：临时补丁量到的**出口泵停等账**（同一 64 MiB 下载）：

| W | 泵 reads | `write_all` 次数 | 停等累计（>200µs） | 单次最大停等 | 泵 elapsed | 吞吐 |
|---|---|---|---|---|---|---|
| 256 KiB | 8197 | 8197 | **2512 ms（占 elapsed 94%）** | 9 ms | 2678 ms | 25.29 MiB/s |
| 4 MiB | 8786 | 8786 | **80 ms（占 8.9%）** | 6 ms | 901 ms | 71.00 MiB/s |

- 下载 64 MiB / 256 KiB = **256 个窗周期**；2512 ms ÷ 256 ≈ **9.8 ms/周期**（= 单次最大停等 9 ms 同量级）
  ⇒ **R_eff ≈ 9.8 ms 是"窗更新往返"**（MAX_STREAM_DATA 与 ACK 同拍：本仓 `ACK_ELICITING_THRESHOLD=16`
  + `MAX_ACK_DELAY=5ms` 是**设计值**，S9 不动——见 §7.3 的"不改什么/为什么"）。
- 于是 **吞吐 = W / R_eff**：256 KiB/9.8 ms = **25.6 MiB/s ↔ 实测 25.29 MiB/s**（差 1.2%）；
  W 抬到 4 MiB 后停等只剩 4–9%，吞吐跳到 71 MiB/s（此时不再由窗支配）。
- 为什么 512 KiB/1 MiB 不涨（而 2 MiB 才涨）：这两档仍落在"每个周期只能推进 ~W"的平顶里
  （quinn 的窗更新有 **W/8 迟滞**：`recv.rs::max_stream_data` 的 `diff >= stream_receive_window/8`
  才发 MAX_STREAM_DATA ⇒ 可用窗 ≈ 7W/8），与 256 KiB 同为"每周期 ~0.2 MiB"量级；
  到 2 MiB 档 `7W/8 = 1.75 MiB`，4 MiB 档 `3.5 MiB`——**跨过"停等期单次可推进量"的台阶**才见跳变。
  （本条为读数拟合的解释，**不是**协议级证明；不改判据。）

**改前/改后同仪器对照（`tools/m3-s8-perf.sh 5 3`，同刻交替 3 轮）**：

| | quic 逐轮 | quic 下中位 | wg 逐轮 | wg 下中位 | **比值** | 门槛 ≥0.95× |
|---|---|---|---|---|---|---|
| 改前（HEAD `e0ee4a9` 行为） | 24.42 / 23.32 / 24.26 | **24.26** | 50.60 / 51.74 / 51.30 | **51.30** | **0.473×** | ✗ |
| **改后（本批）** | 70.97 / 70.89 / 70.06 | **70.89** | 51.57 / 51.90 / 52.24 | **51.90** | **1.366×** | **✓** |

- 出口侧同一读数面同向（改后 `服务流结束（tag=files，…耗时 903ms/1.10s）` vs 改前 2.64–2.76s）。
- 来源：`/tmp/m3s9-res/baseline-run.log`（改前）、`/tmp/m3s8-res/perf/SUMMARY.txt`（改后）。

### 7.2 整改内容（代码面）

| 项 | 从 → 到 | 依据 | 内存面 |
|---|---|---|---|
| `stream_defaults::RECV_WINDOW`（`crates/homeway-quic/src/tuning.rs`） | 256 KiB → **4 MiB** | §7.1 的消融（平顶 24 → 72 MiB/s；quinn 对 `stream_receive_window` 的原话：应 ≥ 连接时延 × 期望吞吐 ⇒ BDP 口径：4 MiB/100 ms = 40 MB/s/流） | 单流窗本身不预留内存（按到货增长） |
| `stream_defaults::CONN_RECV_WINDOW`（**新增**） | （quinn 缺省 = 无界 `VarInt::MAX`）→ **8 MiB** | 每流窗 ×4 后若不吃聚合闸，最坏接收面 = 64×4 MiB = **256 MiB/连接**（超设计 §7 的 16 MiB 账 16×）；显式 8 MiB ⇒ 最坏 **8 MiB/连接**（**比设计账还低一半**），且单条 bulk 流仍可吃满 4 MiB 的每流窗 | 聚合上界 = 8 MiB/连接（构造性） |
| `exit/transport.rs::transport_config_with` | 增加 `t.receive_window(...)` | 同上（两端共用同一组装函数 ⇒ 客户端与出口同时生效） | — |
| env 面（§15-2/§15-3 纪律） | `HOMEWAY_QUIC_STREAM_WINDOW` 值域上限 4 MiB → **16 MiB**；**新增** `HOMEWAY_QUIC_RECV_WINDOW`（256 KiB…64 MiB） | 缺省值抬进原上限会吃掉消融臂的可抬空间；新增旋钮必须同批登记（S7 表跟） | — |
| **不改**：`SEND_WINDOW`（2 MiB）、ACK 频率（16 + 5ms）、`PENDING_BYTES`（64 KiB）、`SOCKPAIR_BYTES`（64 KiB）、`exit/pump.rs` 的 8 KiB 读块、`client/streams.rs` 的 `READ_CHUNK`（16 KiB） | — | 各有本批**负面读数**兜底：SEND_WINDOW ↑4× 零效果；岛/出口两侧读块 ×4 均零效果；ACK 频率是 M1 §1.2/§7.2 B3 的中继 200pps 预算设计值（S9 无真机证据前不动） | — |

单测面（随同批）：`tuning.rs` 的缺省/值域/env 用例全部按新值订正（含「聚合闸 ≥ 每流窗」的构造性断言
与「旧缺省 256 KiB 仍可显式指定」的**负向对照臂**）。

### 7.3 回归读数（改后）

**(a) bulk(L3) × 服务流(STREAM) 共存 A/B**（`tools/m3-s8-coexist.sh 5 2`；判据形态同 S8 §2.2）

| 轮 | 基线臂（空载） | 共存臂 | 相对差 | 共存中服务流 |
|---|---|---|---|---|
| 1 | 135.79 Mbps | 137.54 Mbps | **+1.29%** | 68.82 MiB/s |
| 2 | 132.81 Mbps | 130.46 Mbps | **−1.77%** | 69.26 MiB/s |

⇒ 与 S8 的 ±0.6% 同带（本批噪声略大），**L3 未被更大窗拖累**；服务流本身 68.8–69.3 MiB/s（S8 同格 23.9–24.1）。
（来源：`/tmp/m3s8-res/coexist/SUMMARY.txt`；S8 原件备份 `/tmp/m3s9-res/coexist-s8-orig/`。）

**(b) 多流并行 bulk（真机 speedtest 的同规格形态：同连接 4 条 `STREAM[tag=files]`，每流 ~91 MiB）**

| 批 | 轮数 | 聚合 MiB/s | 逐流 MiB/s | 出口存活 |
|---|---|---|---|---|
| 3 轮（`tools/m3-s9-bulk.sh 5 3 4 95420416`） | 3 | 73.75–75.07 | 18.8–21.4 | **全程存活**（pid 23163；RSS 13.4→18.8 MB；线程 74→78；fd 30→46） |
| 10 轮（同参数） | 10 | 73.04–75.71 | 18.6–21.5 | **全程存活**（pid 20616；RSS 峰值 19,088 KB；10 轮 × 4 流 × 95.4 MB = **3.8 GB** 全部结算、逐流字节 = 源字节） |

- **聚合 74–76 MiB/s ≈ 单流的 71 MiB/s** ⇒ 4 流并行**只多 ~5%**：残余瓶颈是**共享段**（未定论，
  见 §7.5-3），**每流 19–21 MiB/s 是聚合分摊的结果**，不是每流窗不够（4 流共享 8 MiB 聚合闸 = 每流 2 MiB）。
- 与真机对照：S8 真机 speedtest 4 流合计 ≈36 MB/s（9 MB/s/流）——本地 4 流合计 74 MiB/s（≈78 MB/s）
  高出 2.1×，**每流 19–21 MiB/s vs 真机 9 MB/s**（真机受 RTT/链路支配，本机回环不是）。

### 7.4 出口死亡随访（S8 §4-1 的复核）

| 项 | 本批读数/证据 |
|---|---|
| 复现尝试 | 13 轮 × 4 流 × ~91–95 MiB（**3 轮 + 10 轮两批，合计 ≈5.0 GB**）；两批出口**全程存活**，逐流字节数与源件相等，`[perf-par]` 读数齐全 |
| 进程面证据 | RSS 13.4→18.8/19.1 MB（**无 4× 级增长**）、线程 74→78、fd 30→46（**无单调泄漏**：轮间回落至 30） |
| 静默死亡路径扫查（代码面） | `serve` 前台壳的 `process::exit` 只在 flag 解析/`--help`（`serve_cli.rs` 注释即此纪律）；运行期收工走 `wait_stop_pipe()`（**有**`收到停止信号`行）；panic 走默认 hook（会落 stderr→stdout.log）且 macOS 会留 `.ips` ⇒ **产品侧未找到"无痕退出"路径** |
| 新增随访仪器 | `tools/m3-s9-bulk.sh` 的**退出形态判定**：出口若不在，按「有无收工行」判 `clean`/`UNCLEAN`，`UNCLEAN` 自动落末 40 行 + call `~/Library/Logs/DiagnosticReports/*homeway*` 清单（下次真机/本地再遇时**当场可用**，不用再事后翻） |
| 结论（如实） | **未复现**（本地两批 + S8 真机一次未复现）⇒ 归因仍未定；**本批不改产品代码**（无证据的加固 = 设计面暗改）。已有信息面：S8 §4-1 的三条候选（内存面 / 沙箱处置非本会话进程 / M3 换轨资源面）本批**均未取得支持或证伪的读数**（RSS/fd 曲线平稳是对①的弱反证） |

### 7.5 未决 / 交后续

1. **残余共享段上限 ~73–76 MiB/s**（4 流并行只比单流多 5%）：已排除窗（§7.1）、岛侧读块粒度（×4 无效）、
   出口泵读块（8→32 KiB 无效）、出口 `SEND_WINDOW`（×4 无效）⇒ 候选 = 岛单线程 runtime 的读/回执面、
   出口 `current_thread` 的泵+packetizer 面、或回环 UDP 路径。**未定论**（本批不再展开：已达门槛 1.37×，
   继续动要新判据）。
   **订正（S9c 代码门 r18 ③-6，按盘上原件逐值复核）**：①下界按实读改「**73–76**」——`/tmp/m3s9-res/bulk/`
   的 10 轮 `[perf-par]` 实测 **73.04–75.71 MiB/s**（文档原写「74–76」把下界抬了 ≈1 MiB/s；该档暂无中位数）；
   ②已排除项之外**补两条候选**：**同连接多流共享 cwnd/pacer**（设计 §7 风险 5 点名；「4 流只比单流多 5%」
   与该假设高度一致）与**同机两端 CPU 竞争**（客户端测试进程与出口进程同机）。两条都需新判据才可判
   （→ 交 M5）。
2. **真机复测**（此棒未上设备）：窗口整改的真机读数（speedtest 每流 MB/s、4 流合计）需 M5/真机棒
   在同规格下复采——**"窗口 = BDP"是真机收益的主项**（真机 RTT 30–100 ms ⇒ 每流上限 256 KiB/40 ms
   ≈6.4 MB/s，与实测 9 MB/s 同量级；4 MiB 窗 ⇒ 该上限抬到 100 MB/s 级）。
3. **出口死亡**：仍未复现（见 §7.4）；建议 M5 风险表保留 + 真机侧用新增的退出形态判定随访。
4. **判据行/配置登记**：`quic: 流面参数（…）` 格式未动、**值变**（`recv_window=4194304B`）；
   新增 env 两条（`HOMEWAY_QUIC_RECV_WINDOW` 新、`HOMEWAY_QUIC_STREAM_WINDOW` 值域上限放宽）
   ⇒ **S7 登记表须跟**（本批在 `M3.md` 的 S9 节登记）。

### 7.6 仓内副作用残留（本批新增）

- 新增（入库）：`tools/m3-s9-bulk.sh`、`crates/homeway-core/tests/quic_stream_perf.rs::stream_files_parallel_download`、
  本 §7 与 `M3.md` 的 S9 节。
- `/tmp`（仓外）：`/tmp/m3s9-res/`（全部原始读数）、`/tmp/m3s9-res/bulk/`（逐轮 + 存活表 + SUMMARY）。
- `$HOME`（仓外）：`~/m3s9-bulk.bin`（95,420,416 B，多流器源件）；`~/m3s8-perf.bin` 沿用（S8 件）。
- 本地私有实例：#5 **收工时 stop**（`tools/local-rust-exit.sh stop 5`）；现役出口 pid 33667 全程未碰。
- **临时测量补丁已 revert**（`exit/pump.rs`、`client/streams.rs`、`tuning.rs` 的 TEMP 改动：
  `git diff` 已核对为零残留）。

### 7.7 干净树复跑（收工状态）

| 项 | 读数 |
|---|---|
| `tools/build-app-core.sh`（HEAD **`fcb605e`** 提交后干净树） | `[ver] **fcb605eb8d51-rust**`、`[sym] 20/20`、`[size] **4,850,416 B**`（与 dirty 复跑同值 ⇒ 本批体积代价 = **+144 B / +0.003%**，来源 = 常量值变大 + `receive_window` 组装行） |
| `tools/m3-s8-perf.sh 5 3`（干净树复跑；同仪器） | quic **69.05 / 69.93 / 70.47**（下中位 **69.93**）vs wg **51.92 / 51.90 / 52.07**（下中位 **51.92**）⇒ **1.347×**（≥0.95× ✓；与 dirty 轮 1.366× 同带——两轮合计 6 个 quic 读数 = 69.1–71.0 MiB/s，离散 ±1.4%） |
| 收工 | 本地私有实例 #1/#2/#5 与本地中继 #1 全部 `stop`；**现役出口 pid 33667 全程未碰**（收工复核仍在跑）；`~/m3s9-bulk.bin` 已删；两仓只读复核：tier = 仅既有 `?? openspec/changes/term-local-scrollback/`、homeway（Go）与主检出 `homeway-rs`（`main`）均干净 |
