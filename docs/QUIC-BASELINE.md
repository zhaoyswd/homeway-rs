# 三基线登记（WG → QUIC 换代 · M0 入册）

> 落点与语义（M0 设计 §5/§4.5）：**本表的数字 = M0 用 `tools/quic-ab.sh` 实测的重新登记值**
> ——「复测语义 = 以本 harness 实测值重新登记基线，附录 A 的旧值只作量级对照」。旧值出处的
> 原始证据链缺陷已由设计门订正（附录 A 的 CPU 四个数只在手抄 `SUMMARY.md` 里，
> 矩阵客户端 JSON 未落盘；第二份独立测量见 `/tmp/pk-*.cli.out`）。
>
> 本轮实测环境：Mac mini（Apple M2 / 8 core，Darwin 25.5.0 arm64），
> `rustc 1.99.0`，harness 产物 `/tmp/quic-ab/m0/`（`summary.txt` / `loadavg.tsv` /
> 逐轮原始件 / `bins.sha256`），loadavg 首/末 = `{2.04 3.48 3.14}` / `{2.07 3.01 3.02}`。
> 四臂二进制指纹见 `/tmp/quic-ab/m0/bins.sha256`（`wg-ring` 与 `wg-shim` 两份 `wg`
> sha256 不同 = ring 来源不同，正是该对照臂要隔离的变量）。

## 1. 体积基线（OHOS aarch64 cdylib，release + LTO + strip）

| 档 | 值（本轮实测） | 附录 A 旧值 | 偏差 | 出处 / 复测命令 |
|---|---|---|---|---|
| 空壳 cdylib（无依赖） | **323,408 B** | 323,632 B | −0.07% | `tools/quic-ab.sh size`（`arms/size-shell` 无 feature；`/tmp/quic-ab/m0/libclientcore-shell.so`） |
| 空壳 + QUIC 全栈（死码消除后） | **778,032 B** | 778,272 B | −0.03% | 同上（`--features quic`） |
| boringtun+smoltcp 在场 + 真实引用全路径 | **1,735,640 B** | 1,825,088 B | −4.9% | 同上（`arms/size --features quic`；v2 形态探针） |
| 现役 `libclientcore.so`（product 档：默认 release + NDK strip，**主检出只读对照**） | **2,213,744 B**（sha256 `03177a91…`） | 2,213,744 B | 逐字符一致 | `tools/quic-ab.sh size` 的「现役对照」行（只读主检出产物） |
| 本 worktree 重出产物（`tools/build-app-core.sh`，product 档） | **2,339,344 B** | — | — | 见下「M0 增量」段（与主检出旧产物的差 = 分支差异，非 M0） |
| **M0 增量（本批判据）** | **+128 B（0.005%）** | — | — | 同分支同 profile 两测：带 `homeway-quic` 依赖 2,339,344 B vs 临时摘掉该依赖行 2,339,216 B |

**现役对照行**：主检出 `target/aarch64-unknown-linux-ohos/release/libclientcore.so`
= **2,213,744 B**，sha256 前缀 `03177a911be5…`（与 M0 设计 §5.1 登记逐字符一致）。
**该行只登记、不设判据**（设计 §4.5：product 档与 lab 档不可比）。

**M0 增量的口径与证据（设计 §7 的硬判据：> +64KB（+3%）须解释）**：
- 本 worktree 的产物 = **2,339,344 B**（`[size]` 行见 `/tmp/quic-ab/m0/summary.txt`），
  比主检出旧产物（2026-10-07 12:26 构建，2,213,744 B）大 **+125,600 B**；
- **该差不是 M0 引入**：临时摘掉 `homeway-core` 的 `homeway-quic` 一行依赖重建 ⇒
  **2,339,216 B**，即 M0 的真实增量 = **+128 B**。125.6KB 的来源 = 主检出那份产物早于
  当前分支的 `Q-F-B`（portfwd 真监听器）等改动（主检出 HEAD 已是 `082e120`，但其产物
  时间戳停在 10-07 12:26）；
- **死码消除的构造性证据**：产物里 `ring_core_0_17_14` / `tokio` / `quinn` / `rustls`
  动态符号计数 **均为 0**（`llvm-nm -D`），即未接线的岛与整条 QUIC 依赖面**没有**被链进
  `.so`（设计 §3.5「产物面判据」成立）；
- 三道门（符号 20/20、版本注入、体积行）全过；本 worktree 产物 = `2.2MB`（`[size]` 行）。

**lab 档两档尺寸差（防「档位静默失效」，设计 §4.4）**：同一份源码
`arms/size --features quic` 在 lab 档 = 1,735,640 B、product 档 = 8,661,384 B
（差 6,925,744 B）⇒ profile 切换真生效；构建日志无 `profiles for the non root package
will be ignored`（harness 对这两条都有 fail-closed 自检）。
**口径注**：`cargo metadata` **不暴露** profile 值（无该字段），故「档位断言」由
「manifest 原文记录（`profile-record.txt`）+ 两档尺寸差 + 非根 profile 警告检查」三条合成。

**M5 预算**：OHOS `.so` ≤ 3.8MB（按 product 档判）。现役 2.21MB + QUIC 栈（lab 实测
+1.4MB 量级，product 档会更大）——M5 删码后复测。

## 2. 每包 CPU 基线（µs/往返包，1280B 载荷 = MTU1400 口径，lab 档，Mac M2）

| 臂 | 本轮实测（三轮下中位） | 逐轮 | 附录 A 旧值 | 偏差 | 相对地板 |
|---|---|---|---|---|---|
| `raw`（纯 UDP 往返） | **4.880** | 4.880 / 4.823 / 4.886 | 4.85 | +0.6% | 1.00× |
| `wg-ring`（boringtun + **真 ring 0.16.20** asm；诊断臂） | **10.699** | 10.662 / 10.699 / 10.703 | 10.69 | +0.1% | 2.19× |
| `wg-shim`（boringtun + `tools/ring-shim`；**现役手机形态**） | **14.671** | 14.849 / 14.663 / 14.671 | 14.64 | +0.2% | 3.01× |
| `quic`（quinn 0.11 + rustls(ring 0.17) + tokio，DATAGRAM） | **12.769** | 12.769 / 11.813 / 13.005 | 12.5 | +2.2% | 2.62× |

- 复测命令：`tools/quic-ab.sh cpu --arms raw,wg-shim,wg-ring,quic --rounds 3`
  （轮序平衡；主指标 = 每包 CPU 而非墙钟——PERF-AB §9.15.1）。
- **M1 门槛参照**：QUIC ≤ 现役 WG+shim × 1.0 ⇒ 12.769 ≤ 14.671 **成立（0.87×）**。
- **±10% 判据的依据**（设计 §0.3 订正 3）：同机同档的第二套独立测量曾差 1.3–5.9%
  ⇒ 带宽本身有实测依据，不是冗余。
- **loadavg 敏感性实测（本轮副产物）**：与三目标交叉编译并发那一轮，四臂值整体上抬
  （wg-ring 11.987 = +12.1%，越出 ±10%）⇒ 用 harness 复现判据时**必须独占机器**
  （harness 把 loadavg 1Hz 落盘，判读时对照 `loadavg.tsv`）。

## 3. 线开销（MTU1400 + 1280B 载荷）

| 项 | 本轮实测 | 附录 A 旧值 | 偏差 | 出处 |
|---|---|---|---|---|
| WG 每包线上字节 | **1312 B（开销 32 B）** | 1312 B（32 B） | 逐字节同 | `wg_size` 探针（会话建立后 encapsulate 一枚 1280B 内层包） |
| QUIC 每包线上字节（oneway，服务端 `udp_rx` 口径） | **1310.129 B（开销 30.13 B）** | 1310.16 B（30.16 B） | −0.002% | `tools/quic-ab.sh overhead`（60000 包，服务端统计） |
| `max_datagram_size`（MTU1200） | **1162**（载荷被压到 1162） | 1162 | 精确 | 客户端打印行（`/tmp/quic-ab/m0/mds-1200.err`） |
| `max_datagram_size`（MTU1400） | **1362**（载荷 1280 原样） | 1362 | 精确 | 同上 |

M1 门槛（≤ 40B/包）：WG 32B / QUIC 30.13B 均在门槛内。**产品后果**（附录 A §3 的约束，
本轮复测确认）：MTU1200 装不下 1280B 内层包 ⇒ 产品必须 MTU ≥ ~1340（M1 采用 1400 + DPLPMTUD）。

## 4. 内存基线（`vmmap` **Physical footprint**，三轮下中位）

| 场景 | raw | wg-shim | wg-ring | quic | 附录 A 旧值 | 偏差 |
|---|---|---|---|---|---|---|
| 稳态（单连接 hold，IDLE 9s） | **960K** | **1008K** | **1536K** | **1232K** | 944 / 1008 / 1552 / 1216 K | +1.7% / 0% / −1.0% / +1.3% |
| 负载态（N=300k 传输中 max） | **960K** | **992K** | **1536K** | **1264K** | — / 1008 / — / 1264 K | — / −1.6% / — / 0% |
| 每连接边际（服务端，multiconn） | — | — | — | **≈108–192K/连接** | ≈37.6K/连接（base≈1399K） | **见下注** |

- 复测命令：`tools/quic-ab.sh mem --mode steady --rounds 3`、`--mode load`（N=300000，与基线
  同口径）、`--mode conns --conns 5`。
- 逐轮原始值：`raw 960/960/960`、`wg-shim 1008/1008/1008`、`wg-ring 1552/1536/1536`、
  `quic 1232/1216/1232`（**第三轮 1216 是 16K 离群，中位仍 1232**——与附录 A §5.3 同型，
  故 ±10% 对稳态格是必要的而非冗余）。
- **`ps -o rss=` 不作判据**（`--mode rss` 为诊断档；lab 已证同机两臂差 4.3MB 而二进制差 176B）。
- **每连接边际：与附录 A 差异显著，登记为本轮实测值 + 差异说明**（不是判据格，但影响 M1 阈值）：
  - harness 口径（设计 §4.3 指定）= N=1,3,5 三点拟合：`N/中位K = 1/1232, 3/1488, 5/1648`
    ⇒ **base=1104K，边际=128.0K/连接**；
  - 补测 5 点（N=1..5：1216 / 1408 / 1440 / 1568 / 1648）⇒ 端点口径 ≈108K/连接，
    首两点口径 ≈192K/连接（页粒度台阶 + 握手余波未落定 ⇒ 拟合斜率对取点敏感）；
  - **差异说明**：附录 A 的 ≈37.6K/连接出自 lab 的 5 点拟合，其原始采样文件已不留存
    （同 §5.1 的「只有手抄 SUMMARY」问题），且与本轮任一取点口径都不吻合（差距 3–5×）。
    **⇒ M1 设计门须先复核拟合口径（延时落定时间、采样密度、是否含客户端）再判阈值**：
    按本轮实测，`每设备 ≤ 64K` 这条门槛以当前 quinn 惰性缓冲行为**很可能不达标**
    （实测 108–192K/连接），单连接 ≤ +256K 仍在范围内。
- M1/M6 门槛：单连接 ≤ +256K、每设备 ≤ 64K、32 设备 ≤ +2MB（后两条见上面的差异说明）。

## 5. 复现命令（一条链）

```sh
# 前置：openssl（certs 现场生成）；size 子命令另需 OHOS target + DevEco NDK
tools/quic-ab.sh all --arms raw,wg-shim,wg-ring,quic --rounds 3   # cpu → overhead → size → mem
# 分项：
tools/quic-ab.sh cpu      --arms raw,wg-shim,wg-ring,quic --rounds 3
tools/quic-ab.sh overhead --mtu 1400
tools/quic-ab.sh size     --profile lab,product
tools/quic-ab.sh mem      --mode steady --rounds 3
tools/quic-ab.sh mem      --mode load                 # N=300k（与基线同口径）
tools/quic-ab.sh mem      --mode conns --conns 5
```

产物目录含：`summary.txt`（本次全部读数 + 环境 + loadavg 首末）、`bins.sha256`（四臂二进制
指纹）、`loadavg.tsv`（1Hz 采样 + 轮首/轮末标记）、`profile-record.txt`（两档 profile 原文）、
逐轮原始件（`cpu-<arm>-r<N>.json` / `srv-*.out` / `fp-*.med` / `peak-*.txt` / `size-*.so`）。
