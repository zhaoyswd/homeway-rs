# 三基线登记（WG → QUIC 换代 · M0 入册）

> 落点与语义（M0 设计 §5/§4.5）：**本表的数字 = M0 用 `tools/quic-ab.sh` 实测的重新登记值**
> ——「复测语义 = 以本 harness 实测值重新登记基线，附录 A 的旧值只作量级对照」。旧值出处的
> 原始证据链缺陷已由设计门订正（附录 A 的 CPU 四个数只在手抄 `SUMMARY.md` 里，
> 矩阵客户端 JSON 未落盘；第二份独立测量见 `/tmp/pk-*.cli.out`）。
>
> **本轮实测（单一自洽证据包）**：日期 **2026-10-08**；环境 = Mac mini（Apple M2 / 8 core，
> Darwin 25.5.0 arm64）、`rustc 1.99.0`；harness 产物目录 **`/tmp/quic-ab/m0-final/`**
> （`summary.txt` / `loadavg.tsv` / 逐轮原始件 / `bins.sha256` / `profile-record.txt`），
> 四类读数由一次连续运行产出：`all --arms raw,wg-shim,wg-ring,quic --rounds 3`
> （cpu → overhead → size → mem steady）→ `mem --mode load` → `mem --mode conns --conns 5`
> → `mem --mode conns --conns-points 1,2,3,4,5`；loadavg 全程 `{2.26 2.09 2.27}` 量级
> （`loadavg.tsv` 1Hz 采样 + 轮首/轮末标记）。
> 执行用二进制指纹 = `bins.sha256` 的**构建时快照**（`wg-ring` 与 `wg-shim` 两份 `wg`
> sha256 不同 = ring 来源不同，正是该对照臂要隔离的变量）。
> **前一轮（`/tmp/quic-ab/m0/`）的读数已弃用**：那一轮与本表的差（如 wg-ring 稳态 `f=1536`
> 这类 harness 旧版产物）见 `docs/reviews/M0.md` 的代码门处置表（M1/M2）。

## 1. 体积基线

> **口径变更（M5，2026-10-10；登记 = `docs/INTEROP-CRITERIA.md` L-1）**：product 档自 M5 起为
> **LTO + `codegen-units=1` + NDK strip**（此前为「默认 release 无 profile 档」）。**同档判、跨档不得
> 互引**——本表 M0–M4 各行均为**无 profile 档**读数。同档历史锚：WG-only（`4841b20`）+ LTO =
> **1,685,144 B**。
>
> **M5 终值**：OHOS `.so` = **2,958,896 B = 0.779× 判据（≤3,800,000 B）**（`[sym]` 20/20、`[ver]` 过）；
> 出口二进制单独量 = `homeway-cli` **8,758,816 B**（同档 LTO 未 strip；**不设判据**）。
> 双栈期（M3/M4 无 profile 档）= 4,891,776 B；**同一份双栈码改档后 = 3,556,520 B（0.936× 判据）**——
> 即 **3.8MB 判据的主杠杆是构建档位，不是删码**（删码后终值再降至 2,958,896 B）。

（OHOS aarch64 cdylib，release + LTO + strip）

| 档 | 值（本轮实测） | 附录 A 旧值 | 偏差 | 出处 / 复测命令 |
|---|---|---|---|---|
| **M6.5 终值**（product 档 = LTO + cgu=1 + NDK strip） | **2,966,608 B**（0.781× 判据） | — | — | `tools/build-app-core.sh`（M6.5 收口读数） |
| **M6.7 修复后**（同档；**现役**） | **2,968,016 B**（0.781× 判据；md5 `2a348ff13deeeaa5c59fc649531c7fbc`） | — | — | 同上（M6.7 收口读数） |
| 空壳 cdylib（无依赖） | **323,408 B** | 323,632 B | −0.07% | `tools/quic-ab.sh size --profile lab`（`arms/size-shell` 无 feature；文档级产物 `libclientcore-shell-lab.so`） |
| 空壳 + QUIC 全栈（死码消除后） | **778,032 B** | 778,272 B | −0.03% | 同上（`arms/size-shell --features quic`） |
| boringtun+smoltcp 在场 + 真实引用全路径 | **1,735,384 B** | 1,825,088 B | −4.9% | 同上（`arms/size --features quic`；v2 形态探针）。**轮间抖动 ±64B 量级**（同源复跑曾得 1,735,576/1,735,640） |
| 现役 `libclientcore.so`（product 档：默认 release + NDK strip，**主检出只读对照**） | **2,213,744 B**（sha256 `03177a91…`） | 2,213,744 B | 逐字符一致 | `tools/quic-ab.sh size` 的「现役对照」行（只读主检出产物） |
| 本 worktree 重出产物（`tools/build-app-core.sh`，product 档） | **2,339,344 B** | — | — | 见下「M0 增量」段（与主检出旧产物的差 = 分支差异，非 M0） |
| **M0 增量（本批判据）** | **≤ 128 B（0.005%；含链接/元数据噪声，量级 = 0）** | — | — | 同分支同 profile 两测：带 `homeway-quic` 依赖 2,339,344 B vs 临时摘掉该依赖行 2,339,216 B。**更强的构造性证据**：产物内 `ring_core_0_17_14`/`tokio`/`quinn`/`rustls`（`llvm-nm` 全符号含静态）+ `homeway_quic` 计数**全 0** ⇒ 「未接线的岛没有被链进 `.so`」与那 128 B 是否噪声无关 |

**现役对照行**：主检出 `target/aarch64-unknown-linux-ohos/release/libclientcore.so`
= **2,213,744 B**，sha256 前缀 `03177a911be5…`（与 M0 设计 §5.1 登记逐字符一致）。
**该行只登记、不设判据**（设计 §4.5：product 档与 lab 档不可比）。

**M0 增量的口径与证据（设计 §7 的硬判据：> +64KB（+3%）须解释）**：
- 本 worktree 的产物 = **2,339,344 B**（`[size]` 行见 `/tmp/quic-ab/m0/summary.txt`），
  比主检出旧产物（2026-10-07 12:26 构建，2,213,744 B）大 **+125,600 B**；
- **该差不是 M0 引入**：临时摘掉 `homeway-core` 的 `homeway-quic` 一行依赖重建 ⇒
  **2,339,216 B**，即 M0 的真实增量 = **≤128 B**（同源复跑抖动 ±64B 量级，属链接/元数据噪声）。125.6KB 的来源 = 主检出那份产物早于
  当前分支的 `Q-F-B`（portfwd 真监听器）等改动（主检出 HEAD 已是 `082e120`，但其产物
  时间戳停在 10-07 12:26）；
- **死码消除的构造性证据**：产物里 `ring_core_0_17_14` / `tokio` / `quinn` / `rustls`
  动态符号计数 **均为 0**（`llvm-nm -D`），即未接线的岛与整条 QUIC 依赖面**没有**被链进
  `.so`（设计 §3.5「产物面判据」成立）；
- 三道门（符号 20/20、版本注入、体积行）全过；本 worktree 产物 = 2,339,344 B（`[size]` 行）。
- **反例防护（代码门 M7 整改）**：`tools/build-app-core.sh` 现对 `CFLAGS_aarch64_unknown_linux_ohos`
  含 `-nostdlibinc` 直接 fail-closed（评审实测：污染该 env 时旧脚本会**静默成功**并产出同尺寸
  `.so`）；`tools/ci-local.sh` 的真链路步骤前显式 `unset` 该变量。

**lab 档两档尺寸差（防「档位静默失效」，设计 §4.4）**：同一份源码
`arms/size --features quic` 在 lab 档 = 1,735,384 B、product 档 = 8,660,024 B
（差 6,924,640 B）⇒ profile 切换真生效；构建日志无 `profiles for the non root package
will be ignored`（harness 对这两条都有 fail-closed 自检；`--profile lab,product` 令 `size`
逐档全跑，`cpu/overhead/mem` 取列表第一个档——代码门 L13 整改）。
**口径注**：`cargo metadata` **不暴露** profile 值（无该字段），故「档位断言」由
「manifest 原文记录（`profile-record.txt`）+ 两档尺寸差 + 非根 profile 警告检查」三条合成。

**M5 预算**：OHOS `.so` ≤ 3.8MB（按 product 档判）。现役 2.21MB + QUIC 栈（本轮 lab 实测
1,735,384 − 323,408 ≈ **+1.41MB** 边际；product 档会更大——探针实测 `arms/size --features quic`
的 product 档 = 8,660,024 B 只作量级参照，不是产品形态）——M5 删码后按 product 档复测。

## 2. 每包 CPU 基线（µs/往返包，1280B 载荷 = MTU1400 口径，lab 档，Mac M2）

| 臂 | 本轮实测（三轮下中位） | 逐轮 | 附录 A 旧值 | 偏差 | 相对地板 |
|---|---|---|---|---|---|
| `raw`（纯 UDP 往返） | **4.882** | 4.997 / 4.868 / 4.882 | 4.85 | +0.7% | 1.00× |
| `wg-ring`（boringtun + **真 ring 0.16.20** asm；诊断臂） | **10.740** | 10.702 / 10.807 / 10.740 | 10.69 | +0.5% | 2.20× |
| `wg-shim`（boringtun + `tools/ring-shim`；**历史对照臂 —— WG 已退役（M5）**，臂本体随 `wgcore`/`ring-shim` 删除而**构成性不可得**） | **14.623** | 14.623 / 14.677 / 14.604 | 14.64 | −0.1% | 3.00× |
| `quic`（quinn 0.11 + rustls(ring 0.17) + tokio，DATAGRAM） | **12.668** | 12.668 / 12.159 / 12.834 | 12.5 | +1.3% | 2.60× |

- 复测命令：`tools/quic-ab.sh cpu --arms raw,wg-shim,wg-ring,quic --rounds 3`
  （轮序平衡；主指标 = 每包 CPU 而非墙钟——PERF-AB §9.15.1）。
- **M1 门槛参照**：QUIC ≤ 现役 WG+shim × 1.0 ⇒ 12.668 ≤ 14.623 **成立（0.87×）**。
  **该相对口径自 M5 起作废**（WG 臂构成性不可得 ⇒ 无法再测相对值）；M5/M6/M6.7 起的每包 CPU
  判定一律走**绝对列**（读数与门槛见 `docs/reviews/M6.md` / `docs/PERF-AB.md` §9.20 族）。
- **±10% 判据的依据**（设计 §0.3 订正 3）：同机同档的第二套独立测量曾差 1.3–5.9%
  ⇒ 带宽本身有实测依据，不是冗余。
- **loadavg 敏感性实测（本轮副产物）**：与三目标交叉编译并发那一轮，四臂值整体上抬
  （wg-ring 11.987 = +12.1%，越出 ±10%）⇒ 用 harness 复现判据时**必须独占机器**
  （harness 把 loadavg 1Hz 落盘，判读时对照 `loadavg.tsv`）。

## 3. 线开销（MTU1400 + 1280B 载荷）

| 项 | 本轮实测 | 附录 A 旧值 | 偏差 | 出处 |
|---|---|---|---|---|
| WG 每包线上字节 | **1312 B（开销 32 B）** | 1312 B（32 B） | 逐字节同 | `wg_size` 探针（会话建立后 encapsulate 一枚 1280B 内层包） |
| QUIC 每包线上字节（oneway，服务端 `udp_rx` 口径） | **1310.337 B**（导线字节 −0.013%；**开销列 30.34 B vs 30.16 B = +0.6%**——含握手/ACK 摊销，故「逐字节同」只适用于 WG 侧） | 1310.16 B（30.16 B） | 总字节 −0.013% / 开销列 +0.6% | `tools/quic-ab.sh overhead`（60000 包，服务端统计） |
| `max_datagram_size`（MTU1200） | **1162**（载荷被压到 1162） | 1162 | 精确 | 客户端打印行（`/tmp/quic-ab/m0/mds-1200.err`） |
| `max_datagram_size`（MTU1400） | **1362**（载荷 1280 原样） | 1362 | 精确 | 同上 |

M1 门槛（≤ 40B/包）：WG 32B / QUIC 30.13B 均在门槛内。**产品后果**（附录 A §3 的约束，
本轮复测确认）：MTU1200 装不下 1280B 内层包 ⇒ 产品必须 MTU ≥ ~1340（M1 采用 1400 + DPLPMTUD）。

## 4. 内存基线（`vmmap` **Physical footprint**，三轮下中位）

| 场景 | raw | wg-shim | wg-ring | quic | 附录 A 旧值 | 偏差 |
|---|---|---|---|---|---|---|
| 稳态（单连接 hold，IDLE 9s） | **960K** | **1008K** | **1552K** | **1232K** | 944 / 1008 / 1552 / 1216 K | +1.7% / 0% / 0% / +1.3% |
| 负载态（N=300k 传输中 max） | **960K** | **1008K** | **1536K** | **1280K** | — / 1008 / — / 1264 K | — / 0% / — / +1.3% |
| 每连接边际（服务端，multiconn） | — | — | — | **77–96K/连接** | ≈37.6K/连接（base≈1399K） | **见下注** |

- 复测命令：`tools/quic-ab.sh mem --mode steady --rounds 3`、`--mode load`（N=300000，与基线
  同口径）、`--mode conns --conns 5`。
- 逐轮原始值（`/tmp/quic-ab/m0-final/mem-steady.txt`）：`raw 960/960/960`、
  `wg-shim 1008/1008/1008`、`wg-ring 1568/1552/1536`、`quic 1232/1248/1216`
  （**quic 第三轮 1216 是 16K 离群，中位仍 1232**——与附录 A §5.3 同型，故 ±10% 对稳态格
  是必要的而非冗余）。
- 复测命令（含臂列表）：`tools/quic-ab.sh mem --mode steady --arms raw,wg-shim,wg-ring,quic --rounds 3`。
- **`ps -o rss=` 不作判据**（`--mode rss` 为诊断档；lab 已证同机两臂差 4.3MB 而二进制差 176B）。
- **每连接边际：与附录 A 差异显著，登记为本轮实测值 + 差异说明**（不是判据格，但影响 M1 阈值）：
  - harness 默认口径（设计 §4.3 指定 = N=1,3,5 三点，**全点最小二乘**拟合）：
    `N/中位K = 1/1232, 3/1488, 5/1616` ⇒ **base=1157.3K，边际=96.0K/连接**
    （复测：`tools/quic-ab.sh mem --mode conns --conns 5`）；
  - 5 点口径（**可复现命令**：`--conns-points 1,2,3,4,5`）：1264 / 1392 / 1472 / 1536 / 1600 K
    ⇒ **base=1208.0K，边际=81.6K/连接**（端点斜率 84.0K）——同源复跑第二轮得 76.8K。
  - **差异说明**：附录 A 的 ≈37.6K/连接出自 lab 的 5 点拟合，其原始采样文件已不留存
    （同 §5.1 的「只有手抄 SUMMARY」问题），与本轮实测差 ≈2×（拟合口径 + 16K 页粒度台阶 +
    握手余波未完全落定都会影响斜率）。
    **⇒ M1 设计门须先复核拟合口径（落定延时、采样密度、是否含客户端）再判阈值**：
    按本轮实测（77–96K/连接），`每设备 ≤ 64K` 这条门槛以当前 quinn 惰性缓冲行为**偏紧**
    （差 1.2–1.5×），单连接 ≤ +256K 仍在范围内（实测单连接 +272K 相对地板 960K，
    与 M1 门槛要按「产品形态客户端」再量）。
- M1/M6 门槛：单连接 ≤ +256K、每设备 ≤ 64K、32 设备 ≤ +2MB（后两条见上面的差异说明）。
  **⇒ 已被门槛表取代（M6/M6.7，2 次用户批准）**：**单连接 ≤ +640K / 每设备 ≤ 96K /
  32 设备 ≤ +3.1 MiB**——现行门槛真源 = `docs/QUIC-ROADMAP.md` 的门槛表（本条只作历史锚）。
  **M6.7 客户端显式 socket 缓冲（入账项）**：`ClientSock::open` 设 **`SO_RCVBUF=2 MiB`（下行接收）
  / `SO_SNDBUF=1 MiB`（上行发送）**——**设定值 +3 MiB**（Linux 内核口径 ×2 ⇒ 记账上界 +6 MiB；
  **口径**：每岛一枚活跃 socket，`rebind` 瞬间新旧短暂并存；**本表四格测的是 `tools/quic-ab/arms/`
  独立探针（不含 `homeway-quic` 的 `ClientSock`）⇒ 本项是「设定值入账」而非本表测出值**，
  产品侧实测面 = 真机 App Pss / U2-U3 观察期）；
  观测行 = `岛 socket 缓冲（SO_RCVBUF（下行接收）|SO_SNDBUF（上行发送））：设定 {want}B，读回 {got}B…`；
  判据面登记 = `docs/INTEROP-CRITERIA.md` 登记表 2026-10-10（M7 补登）条
  〔M6.7 实读：修前 `RcvbufErrors +17…+32/轮` ⇒ 修后六轮全 0、T2 热 11.27 → 20.14 MB/s〕。

## 5. 复现命令（一条链）

```sh
# 前置：openssl（certs 现场生成）；size 子命令另需 OHOS target + DevEco NDK
tools/quic-ab.sh all --arms raw,wg-shim,wg-ring,quic --rounds 3   # cpu → overhead → size → mem
# 分项：
tools/quic-ab.sh cpu      --arms raw,wg-shim,wg-ring,quic --rounds 3
tools/quic-ab.sh overhead --mtu 1400
tools/quic-ab.sh size     --profile lab,product
tools/quic-ab.sh mem      --mode steady --arms raw,wg-shim,wg-ring,quic --rounds 3
tools/quic-ab.sh mem      --mode load   --arms raw,wg-shim,wg-ring,quic   # N=300k（与基线同口径）
tools/quic-ab.sh mem      --mode conns  --conns 5                        # 三点（设计 §4.3 口径）
tools/quic-ab.sh mem      --mode conns  --conns-points 1,2,3,4,5         # 五点（补测口径）
```

产物目录含：`summary.txt`（本次全部读数 + 环境 + loadavg 首末）、`bins.sha256`（四臂二进制
指纹）、`loadavg.tsv`（1Hz 采样 + 轮首/轮末标记）、`profile-record.txt`（两档 profile 原文）、
逐轮原始件（`cpu-<arm>-r<N>.json` / `srv-*.out` / `fp-*.med` / `peak-*.txt` / `size-*.so`）。
