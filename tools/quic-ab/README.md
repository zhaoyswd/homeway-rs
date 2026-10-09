# tools/quic-ab —— WG vs QUIC 承载对比实验台（M0 转正自 `/tmp/quic-lab`）

**入口 = `tools/quic-ab.sh`**（唯一入口；与 `qi-ab.sh` / `perf-ab.sh` 同层）。
本目录是**两个独立 workspace**（`arms/` 与 `wg-shim/`——`[patch]` 是 workspace 级，
一个 workspace 装不下「真 ring」与「垫片」两臂），根 `Cargo.toml` 的 `exclude` 已排除本目录。

## 纪律（与 `qi-ab.sh` 的差别，写清）

- 本 harness **完全不碰 `homeway-cli` / 任何生产实例**：四臂都是独立探针二进制，
  只绑 `127.0.0.1:0`（端口由内核分配后读回），无 state 目录、无出口、无 token。
  因此不需要臂切换原子替换、不需要 `wipe_session`；收工**只按记录的 PID** kill
  （绝不 `pkill -f` 泛匹配——主检出可能正跑着现役实例）。
- **不接 CI**（四臂全量重建成本高）；M0 由实现棒手动跑留证；是否 CI 化（smoke 档）列为 M1 候选。
- 产物落 `${QUIC_AB_DIR:-/tmp/quic-ab/<时间戳>}/`：逐轮原始件 + `summary.txt` + `bins.sha256`
  + `loadavg.tsv`（1Hz 带时间戳 + 轮首/轮末标记）。

## 两臂与口径（**M5 C4 收窄**：WG 两臂退役）

| 臂 | 构造 | 回答 | 二进制 |
|---|---|---|---|
| `raw` | 纯 UDP `sendto`/`recv_from`（地板） | 传输/内核成本 | `arms/target/release/raw` |
| `quic` | quinn 0.11 + rustls(ring 0.17) + tokio，QUIC DATAGRAM | QUIC 每包成本（相对地板） | `arms/target/release/quic` |

**WG 两臂（`wg-ring` = boringtun + crates.io 真 ring 0.16.20 / `wg-shim` = boringtun + 本仓
`tools/ring-shim`）随 WG 面退役删除**（M5 C4 / S1b：`boringtun` 依赖、`tools/ring-shim`、
`[patch.crates-io]` 与 `arms/wg-ring`、`wg-shim/` 全部移除）。连带后果（登记）：门槛表的
「每包 CPU ≤ 现役 WG+shim ×1.0」「线开销 ≤40B」两条**失去参照臂**——设计 §9.2 默认 (c)：
保留**绝对列**（M1/M2 历史的 12.895µs / 12.250µs 等作历史锚并标档位）+ 本文件的历史读数
**跨期不可互引**。`overhead` 子命令的 WG 线上字节条同批退役（只给 QUIC 绝对列）。

**口径微漂登记（代码门 L9/L5，如实列，勿当"逐项照搬"的完美复制）**：

- lab 矩阵的 QUIC 臂用 `NQ=40000`（`/tmp/quic-lab/run_matrix.sh`），本 harness 统一 `--n 60000`
  （设计 §4.2 的默认口径）——差异对每包 CPU 无影响（比值口径），但读数次数不同；
- `mem --mode load` 的采样窗 = 120×250ms = **30s**（lab `peak_probe.sh` 为 60×250ms = 15s）
  ——覆盖更完整，代价是单臂更久；
- `mem --mode conns` 的点集（**M1 S5-1a 起：缺省 = 五点 `1,2,3,4,5`**——依据
  `docs/reviews/M1-design.md` §9.1-1 的裁决「拟合口径改五点（N=1..5）+ 三点仅作对照」；
  三点拟合的 base 截距/斜率对 16K 页粒度台阶极敏感，M0 同一份数据的 96.0K vs 81.6K 即
  采样密度差异）：缺省五点；`--conns N` ⇒ 1,3,5,… 到 N（奇数序列 = M0 旧口径的显式形态，
  供 32 连接扩展用）；`--conns-points "…"` 显式指定（优先于 `--conns`；三点对照片
  用 `--conns-points 1,3,5`）；
  拟合 = **全点最小二乘**（旧 lab 的手抄 37.6K/连接为 5 点口径，原始采样未留存）；
- `arms/size`（四档第三格）**M5 C4 起 base 由 `boringtun+smoltcp` 改为 `smoltcp`**（WG 依赖
  退役）⇒ 该格与 M0–M4 的读数**不可直接互引**（base 变小）；①② 两格（空壳 / 空壳+QUIC）零改。

**口径订正（历史叙述保留；M5 C4 起 WG 两臂已删）**：

1. （历史）`wg-ring` 臂的 ring 是 **0.16.20**（crates.io 真版，boringtun 0.6 自身声明）——
   **不是 0.17**；0.17.14 只出现在 `quic` 臂（rustls/quinn-proto 侧）。现仓内 `ring` 只有
   0.17.14 一条（`grep -A3 'name = "ring"' Cargo.lock`）。
2. aws-lc 的唯一开关 = **本仓 `rustls` 声明关默认 features**（quinn 侧默认关不关都不影响
   这条）；`quinn` 关默认是另一件事（去 `platform-verifier`/`bloom`/`log`）。
3. CPU 基线的原始证据链：lab 的 `m-*.out` 只有 11 字节（客户端 JSON 未落盘），四个数只在
   手写 `SUMMARY.md` 里；第二份独立测量 = `/tmp/pk-*.cli.out`（N=300k），与手写值差 1.3–5.9%
   ⇒ **±10% 是实测带宽**，且 M0 复测语义 = 「以本 harness 实测值重新登记基线」，旧值只作量级对照。
4. **中位 = 下中位**（lab 脚本 `a[int((NR+1)/2)]` 在偶数采样点取下中位）——**照搬，勿当 bug 修**。

## 子命令

| 子命令 | 作用 | 关键开关 |
|---|---|---|
| `cpu` | 多臂每包 CPU 矩阵（默认 `raw,wg-shim,quic`） | `--arms a,b,c`（含 `wg-ring` 诊断臂）、`--payload 1280`、`--n 60000`、`--rounds 3`、`--mtu 1400`、`--profile lab\|product` |
| `overhead` | 线开销（WG 侧 = `wg_size` 探针的 encapsulate 线上字节；QUIC 侧 = oneway 服务端 `udp_rx` 口径）+ `max_datagram_size` 精确打印（MTU1200→1162 / MTU1400→1362） | `--n 60000`、`--mtu 1400` |
| `size` | OHOS cdylib 体积矩阵（lab 档三格 + 现役对照 + product 档增量） | `--profile lab,product`（默认两档都跑） |
| `mem` | footprint 采样 | `--mode steady\|load\|conns\|rss`、`--rounds 3`、`--conns 5`、`--conns-points "1,2,3,4,5"` |
| `all` | 顺序跑 `cpu → overhead → size → mem` | 透传上述；`--profile` 为逗号列表时 `size` 逐档跑（其余子命令取第一个档） |

## 判据（附录 A ±10%；M0 设计 §4.5）

| 指标 | 附录 A 目标值 | 判据 |
|---|---|---|
| 每包 CPU（raw / wg-shim / quic） | 4.85 / 14.64 / 12.5 µs（1280B 载荷 = MTU1400 口径，lab 档） | 各臂 ±10% |
| 线开销 | WG 32B / QUIC 30.16B（MTU1400 + 1280B 载荷） | ±10% |
| `max_datagram_size` | MTU1200→1162 / MTU1400→1362 | **精确**（打印值） |
| footprint（steady） | raw 944K / wg-shim 1008K / quic 1216K | ±10% |
| 体积（lab 档三格） | 323,632 / 778,272 / 1,825,088 B | ±10% |
| 体积（product 档） | 现役 2,213,744 B + M0 增量 | **只登记，不设判据**（档位不同，与 lab 档不可比） |

`mem --mode load` 只登记绝对值 + 抖动（max 受采样相位影响）；`mem --mode rss`
（`ps -o rss=`）**不作判据**（同机两臂差 4.3MB 而二进制差 176B——该口径不可用）。

## 前置与复现命令

前置：`openssl`（certs 现场生成）；OHOS 目标 + DevEco NDK（只有 `size` 需要；
`rustup target add aarch64-unknown-linux-ohos`）。

```sh
tools/quic-ab.sh all                       # 一键（cpu → overhead → size → mem）
tools/quic-ab.sh cpu --arms raw,wg-ring,wg-shim,quic --rounds 3
tools/quic-ab.sh overhead --mtu 1400
tools/quic-ab.sh mem --mode steady --rounds 3
tools/quic-ab.sh size --profile lab,product
```

**`size` 的真链接纪律**（fail-closed）：必须用 NDK 包装 clang
（`CC_aarch64_unknown_linux_ohos=<NDK>/llvm/bin/aarch64-unknown-linux-ohos-clang`）
**且** `CFLAGS_aarch64_unknown_linux_ohos` 不含 `-nostdlibinc`——抄 `ci.yml` 的 check 档
flags 会产出「无 libc 的 ring 对象」再链接失败（或在更坏的情形下静默成一锅杂烩）。
harness 对此有显式断言。

## 未测项（如实登记，勿当结论）

- 本机是 macOS 回环，**没有空口**：真实瓶颈（WiFi 电源态、蜂窝）不在口径内；
- quinn 在本机无 GSO/GRO（macOS 限制）——Linux 出口/手机侧会更乐观；
- **未测**小包/交互形态（每包 syscall 成本占比更高时 QUIC 的额外帧开销占比会变差）；
- **未测**多连接并发吞吐（`mem --mode conns` 只量内存边际，不量吞吐）；
- **未测** DPI / 抗封（需真机实网）；
- 手机侧单核贴顶（PERF-AB §9.7-bis 的 28MB/s B 路径）**未复现**——那是真机 × OHOS 内核形态。

## 安全边界（重要）

探针里的 `.dangerous().with_custom_certificate_verifier(SkipVerify)` 与自签证书
（`certs/gen_certs.sh` 生成，DER **不入库**）**只允许存在于本目录**，且每处带
`// SECURITY: harness-only` 标记。产品面（`crates/`）零命中由
`tools/check-quic-isolation.sh` 断言；M2 设计门把「SkipVerify → RPK 钉定」列为门的一项。
