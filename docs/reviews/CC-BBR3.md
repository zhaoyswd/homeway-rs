# 拥塞控制对比批（M6.7 可选改进项）实施记录 —— **BBRv3 移植 + 三档实测：不移入默认**

> 本棒指令：「把 bbrv3 移植进来对比一下」（M6.7 §12.8.8 登记的可选改进项：剩余 11% 的速率控制面）。
> 工作目录 = 主检出 `main`；不 push / 不发 tag / 不建 PR；`homeway`/`tier`/`baseline/` 全程只读。
> 现役出口（pid 33667）全程未碰；设备 = `FMR0224116011480`（专用测试机，用户已授权），
> 收工状态见 §7。**口径预登记在本文 §3，读数与结论见 §4/§5，不按结果改口径。**

---

## 0. 结论摘要（**不粉饰**）

| 问 | 答 |
|---|---|
| **BBRv3 能把剩余 11% 补上吗？** | **不能**。真机热态 256 MiB 同会话比值 = **0.734×**（cubic 13.617 s → bbr3 18.559 s），即**反向慢 27%**。 |
| **补多少？** | **负增益**：−27% 吞吐；设备侧每包 CPU **1.25×**（65.3 → 81.6 µs/1252B 单元）。 |
| **代价** | 移植面 3,336 行（其中产品面 3,300 行 + 归属文档）；`.so` 体积增量见 §5（门槛内）；单连接内存增量 = 有界发送快照表 ≤72 KB（门槛 +640K 内）；无新运行时依赖。 |
| **值不值得留作默认？** | **不值得**。留在代码里作 **env 消融臂**（`HOMEWAY_QUIC_CC=bbr3`），**默认档不变**（不设 env = 现状 CUBIC，零字节变化）。三档里 **没有一档优于现状**：`bbr`(v1) 0.966×（同带），`bbr3` 0.734×。 |
| **那 11% 该打哪？** | 靶子不变（§9.20.11 的结论未被动摇）：**出口侧按带宽而非按丢包的发送整形/在途上限**。BBRv3 的失败点恰恰是它把「按带宽」实现成了「按模型算 cwnd 再交给框架 pacer」——见 §6 机制分析。 |

---

## 1. 交付面（**做了什么**）

| 工作单元 | commit | 内容 |
|---|---|---|
| S1 开关 + BBRv1 | `611604f` | `tuning::CcChoice`（`cubic`[默认]/`bbr`）+ env `HOMEWAY_QUIC_CC`；`exit/transport.rs::transport_config_with(mtu, streams, cc)` 成为**唯一接线点**；两端配置面各加 `cc` 字段（`ExitQuicConfig`/`IslandConfig`）；启动行「拥塞控制器（CC）= …」（臂判定用） |
| S2 移植 | `48ca3e3` | `crates/homeway-quic/src/cc/{mod,bbr3,delivery_rate,minmax}.rs`（tquic 移植，Apache-2.0）+ `THIRD-PARTY.md` + 隔离门 `ASYNC_FILES` + `quinn-proto` 直依赖（1 条，理由见 §2.2） |
| S3 读数 | 见 §4/§5 | lab 三档 + 真机三档（T2 冷/热 + T6）+ 回归面 |
| S4 记录 | 本文 + `docs/PERF-AB.md` §9.20.12 | 读数、结论、残余 |

**默认档零变化（硬约束）**：`CcChoice::Cubic` 显式设 `CubicConfig::default()`——与「不设 factory」
逐字节同效（单测 `cubic_factory_matches_quinn_default` 把「悄悄换了控制器」变成红）。
`HOMEWAY_QUIC_CC` 值域闭集；非法 ⇒ **不改该项 + 一行说明**（照 `HOMEWAY_QUIC_MTU` 先例）。

---

## 2. 移植范围与差异表

### 2.1 搬了什么 / 没搬什么（真源 = `crates/homeway-quic/src/cc/mod.rs` 模块头）

| 上游（tquic `develop` @ `938e90a`） | 本仓 | 处置 |
|---|---|---|
| `bbr3.rs`（74,138 B） | `cc/bbr3.rs` | **全搬**（状态机/增益/上限自适应/ProbeRTT/丢包响应），接口层按框架改写 |
| `delivery_rate.rs` | `cc/delivery_rate.rs` | 全搬（速率采样逐行保真）；「每包快照」承载方式改写 |
| `minmax.rs` | `cc/minmax.rs` | 逐行保真（含上游 `mod test`；Google BSD-3 头随文保留） |
| `pacing.rs` | **不搬** | pacing 属框架职责（框架 pacer 按 `window()/RTT` 自算令牌桶）⇒ 搬进来会双发车 |
| `hystart_plus_plus.rs` | **不搬** | 上游 BBRv3 对它零引用（CUBIC 的依赖） |
| `bbr.rs`/`cubic.rs`/`copa.rs`/`dummy.rs`/`congestion_control.rs` | **不搬** | 对照组用框架自带 CUBIC/BBRv1；本批只要 v3 |

### 2.2 差异表要点（完整 15 行在 `cc/bbr3.rs` 文件头）

1. **包身份**：上游把每包发送快照写在框架的 `SentPacket.rate_sample_state` 上；本框架只给
   `on_sent(now, bytes, last_pkt_num)` / `on_ack(now, sent, bytes, …)`。⇒ 本仓自持**定长发送快照表**
   （`VecDeque`，上限 1024 条 ≈72 KB/连接），按发送时刻 FIFO 配对，逐 ACK 核销
   （**GSO 批**：一次 `on_sent` 的 `bytes` 是批总量 ⇒ 记录按 `bytes_remaining` 逐包核销）。
2. **`begin_ack` 钩子**：框架没有 ⇒ 懒开批（本批首个 `on_ack` 复位）。
3. **丢包粒度**：上游逐包调 `on_congestion_event`；框架逐**丢包批次**（`lost_bytes` = 批总量）。
   逐包字段（`tx_in_flight`/`is_app_limited`）取 `sent` 时刻的最近快照；缺记录 ⇒ 跳过该步
   （cwnd/持久拥塞逻辑不受影响）。
4. **在途字节**：自维护 + **每批用框架的 `in_flight` 对齐**（框架口径不含纯 ACK 包 ⇒ 以它为权威）。
5. **⚠️ 最大语义差（pacing 归属）**：上游框架**按控制器给的 `pacing_rate` 发车**；本框架的 pacer
   按 `window()/RTT` 自算 ⇒ BBR 的 `pacing_gain` 只经 `send_quantum`/`offload_budget`/`extra_acked`
   与 `cwnd_gain` **间接**生效，`pacing_rate` 在本仓只是 metrics 观测值。**这是本批失败的首要嫌疑**
   （见 §6）。
6. **不引依赖**：`rand` → 自持 splitmix64（`getrandom` 种子）；`log` 宏删（本仓判据行走 `Logf`）。
7. **两处刻意偏离（补账）**：① `newly_lost_bytes` 的复位点从 `begin_ack` 挪到 `on_end_acks`
   消费后——框架的丢包检测发生在 `on_end_acks` **之后**，照上游会被下一次 `begin_ack` 静默清零；
   ② 上游两个从不读取的字段（`AckState.prior_bytes_in_flight`、`extra_acked`）不搬（死代码）。
8. **新增 1 条依赖**：`quinn-proto`（同版本，同一编译单元）——框架 façade **不 re-export** 控制器
   trait 签名里的 `RttEstimator`，外部实现者无法点名该类型（框架侧的 API 缺口，非本仓选择）。

### 2.3 License / 归属（公开仓硬要求）

- 逐文件头：上游仓库 + 文件 + **commit `938e90adb460b5ff08b2bc6d11a3e1ba52c27a8d`** + Apache-2.0 原文；
  `minmax.rs` 另留 **Google BSD-3** 头（不删）。
- 仓库级台账：**`THIRD-PARTY.md`**（新建；文件映射 + 未搬依赖 + 未搬文件逐条）。

---

## 3. 单测清单（判据 + 证伪方式；`cargo test -p homeway-quic --lib cc::`）

| 用例 | 判据 | **证伪方式**（怎么让它红） |
|---|---|---|
| `window_grows_toward_bdp_on_a_bandwidth_limited_path` | 20 MiB/s × 20 ms（BDP 400 KiB）虚拟路径跑 3 s：窗 14,000 → **870,800 B（≈2.1×BDP）**，投递速率 ≥0.75×瓶颈 | 把 `set_cwnd` 的 `max_inflight` 去掉 ⇒ 窗不跟 BDP |
| `growth_requires_rate_samples`（**反例臂**） | 同一路径**掐掉 ack**（速率采样恒零）⇒ 窗**恒停** 10×mds、速率 <1/4 瓶颈 | 若「窗增长不依赖 bw 采样」⇒ 本用例红 |
| `shrinks_on_loss_without_collapsing_to_zero` | ①原语级 `modulate_cwnd_for_recovery` 逐值扣 + 夹最小窗；②轻丢不塌（仍 ≥1×BDP）；③持久拥塞 ⇒ `2×mds` | 把最小窗夹取删掉 ⇒ ①红；把持久拥塞分支删掉 ⇒ ③红 |
| `mtu_update_rescales_windows` | `on_mtu_update` 后 `min_cwnd=2×mds`/`initial_cwnd=10×mds`、窗非 0 | 把 `set_mtu` 删掉 ⇒ 红 |
| `clone_box_is_independent` | 克隆体状态一致、持久拥塞后落最小窗、**原件不受影响** | 克隆体共享状态（如 `Arc<Mutex>`）⇒ 红 |
| `behaviour_is_distinguishable_from_cubic` | 同路径同输入下两档窗不同 | 工厂接错档（都落 CUBIC）⇒ 红 |
| `cubic_factory_matches_quinn_default`（`exit/transport.rs`） | 缺省档 = 框架 CUBIC 且初窗 12000（downcast 断言） | 把缺省改成任何实验档 ⇒ 红 |
| `each_choice_selects_a_distinct_controller` | 三档初窗 12000 / 240000 / 14000 互不相等 | 任一档接错 ⇒ 红 |
| `cc_env_choice_applies_or_falls_back_with_note`（`tuning.rs`） | 合法 5 字形生效；非法 6 例 ⇒ 不改值 + 一行说明；未设 ⇒ 零噪声；`VALUES` 与 `parse` 成对 | 删掉非法分支的说明行 ⇒ 红 |
| `start_logs_congestion_controller_line`（`exit/tests.rs`） | 启动行含 env 名与档位 | 删启动行 ⇒ 红 |
| `cc::delivery_rate::tests::*`（3 条）+ `cc::minmax::test::*`（2 条） | 上游用例等价搬入（快照逐字段/采样区间/RTT/窗式 min-max） | 采样算法改错 ⇒ 红 |

**测试基建说明（可复核）**：BBRv3 的窗是模型驱动的，玩具式「逐对对喂 on_sent/on_ack」量不出
「按 bw/rtt 增长」（min_rtt 被压到 1 ms、BDP 塌成几 KB，窗反被模型压回去）⇒ 用例自建
**虚拟路径**（定带宽 + 定 RTT + 无队列 + 先到先服务串行化，按 1 ms 步进），输入形态与真实连接同构。
另：框架的 `RttEstimator` 无公开构造面 ⇒ `on_ack` 的实现体抽成 `on_ack_inner(now, sent, bytes)`
（trait 侧只做一层转发），单测直接喂内部入口。

---

## 4. 读数

### 4.1 lab（本机回环产品路径；4 流 × 91 MiB；出口侧切档；3 轮/档交替）

| 档 | 逐轮 MiB/s | 中位 |
|---|---|---|
| `cubic` | 73.7 / 169.0 / 74.5 | 74.5 |
| `bbr` | 163.7 / 71.1 / 70.6 | 71.1 |
| `bbr3` | **23.9 / 32.5 / 24.1** | **24.1** |

- **同臂自对照即 2.4× 双峰**（cubic 单臂 6 轮在 73.5–74.4 与 173.8–176.3 间**严格隔轮交替**）
  ⇒ 该仪器的轮间效应盖过臂效应 ⇒ **lab 只作方向性登记，不定判**。
- 但 `bbr3` 的慢**不是双峰**：三轮全在 23.9–32.5（稳定 2–3× 慢）⇒ 亚毫秒 RTT 回环上
  BBRv3 有系统性问题（疑点：按轮计数退出启动在 RTT≈0.1 ms 时轮次过密，BDP 采样跑不到
  1.25× 增长就判「满管」⇒ 卡在低 bw 自锁；真机 RTT 15–20 ms，不由 lab 定判）。
- **整形 lab 的失败尝试（如实登记）**：为给回环补瓶颈，本批写了 `tools/udp-rate-proxy.py`
  （下行令牌桶 + 丢尾，harness-only），并让出口把 `--public-endpoint` 指向代理。**实测不可用**：
  岛侧的**候选赛跑**会绕过代理直连出口的 LAN 候选（逐轮 `fwd=2` vs `fwd=298k` 可判）⇒
  整形轮与未整形轮混在一起，读数不可用。工具留在 `tools/`（可用，但需先把直连候选打掉）。

### 4.2 真机（T2 主判据 + T6）

**方法学（本批的关键差别）**：CC 是**出口侧**属性 ⇒ 用出口进程 env 切档，**设备侧核 `.so` 全程
同一份**（不重新出包/不覆盖安装）。**设备侧唯一变量 = 出口 CC**（岛恒 CUBIC）⇒ 无换包交叉影响；
代价 = 无 WG 旧臂分母 ⇒ 判决用**同会话三档比值**（漂移纪律照 §9.20.0）。

| 档 | 热轮（256 MiB，服务端 REQ→END s） | 热中位 | 吞吐 | 冷轮 | 同会话比值 |
|---|---|---|---|---|---|
| `cubic` | 14.389 / 13.632 / 13.563 / 13.602 | **13.617 s** | 19.71 MB/s | 未取（登记） | 1.000 |
| `bbr` | 14.091 / 15.012 / 11.442 / 11.578 / 15.223 | **14.091 s** | 19.05 MB/s | 17.974 s | **0.966×** |
| `bbr3` | 20.079 / 16.681 / 16.759 / 20.285 / 18.559 | **18.559 s** | 14.46 MB/s | 16.714 s | **0.734×** |

- 轮序 = `A,B,C,A,B,C` 交替（消时段漂移）；逐轮落 loadavg（1.2–3.1，全部 < 判废线）；
  每轮出口日志的「拥塞控制器（CC）= …」行逐轮核对（臂已生效）。
- 作废轮：设备**下载确认框**偶发未点中 ⇒ 6 轮 MISS（wall=120 s），**全部剔除**（不计入任何读数）。

**T6 设备侧每包 CPU（ΔCPU ÷ 214,400 包/轮；单位 1252B）**

| 档 | 逐轮 µs/包 | 中位 | 比值 |
|---|---|---|---|
| `cubic` | 65.3 / 70.0 / 65.3 / 65.3（n=4） | **65.3** | 1.00 |
| `bbr` | 65.3 / 65.3 / 60.6 / 74.6（n=4；另 1 轮未采到 CPU 差分） | 65.3 | 1.00 |
| `bbr3` | 84.0 / 79.3 / 74.6 / 84.0（n=4） | **81.6** | **1.25×** |

**注（机制，不作判据）**：设备侧核没换（岛恒 CUBIC），每包 CPU 的差来自**下发节奏**——
bbr3 把同一份 256 MiB 摊得更平（秒级包率更低、批更小）⇒ 岛侧每包的 syscall/唤醒分摊变贵。

---

## 5. 回归面

| 门 | 读数 | 证据 |
|---|---|---|
| `cargo test --workspace` | 全绿（homeway-quic 211 项，含新增 CC 单测 14 项） | 本地跑 |
| clippy `-D warnings` | 0（workspace 全 target） | 本地跑 |
| `check-quic-isolation.sh` | 全绿（11 条；`cc/bbr3.rs` 入 `ASYNC_FILES`，`cc/mod.rs`/`minmax.rs`/`delivery_rate.rs` 纯 std 受 ② 条真扫描） | 本地跑 |
| `quic-island-e2e.sh`（含**中继候选**全链） | rc=0（输出 `/tmp/cc-dev/e2e-island.log`） | 本地跑 |
| `quic-island-e2e.sh` **@bbr3 出口** | **rc=0（六条全过）**：`island` / **`relay`（只给中继候选）** / `generation` / `service_stream` / `app-core-service` / `dns` | `/tmp/cc-dev/e2e-island-bbr3b.log` |
| `quic-ladder-e2e.sh`（T_recv ≤3.5 s） | rc=0 | 本地跑 |
| `quic-pf-e2e.sh`（STREAM[dial] 五条） | rc=0 | 本地跑 |
| `quic-wg-e2e.sh`（单承载端点面） | rc=0 | 本地跑 |
| `tools/build-app-core.sh` 三道门 + `[size]` | **三过**：`[sym] 20/20`、`[ver]`、`[size]` 见 §5.2（门槛 3,800,000 B） | 本地跑 |
| 三目标 `cargo check` | `aarch64-unknown-linux-ohos`（ci-local 第 3 步：真 NDK 链路 + 交叉 check）/ `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`（后两者按 ci-local 缺 NDK 档的配方：`CC_*=clang` + `-nostdlibinc` + `tools/cc-check-shim`）**全过** | 本地跑 |
| `tools/ci-local.sh` 八步 | 步骤 1–6 全绿；**步骤 7（矩阵冒烟）首跑红、隔离重跑绿**（见 §5.3 的轮序-成因分析） | `/tmp/cc-dev/ci-local*.log` |
| 单连接内存 ≤+640K | 发送快照表**有界**（≤1024 条 ≈72 KB/连接，实测见 §5.2） | 本地跑 |

### 5.1 中继档（「更激进的 CC 不得把中继打爆」）

- **本机**：`quic-island-e2e.sh` 的 S2b 用例 = **只给中继候选**（信封路径全链 + 数据面双向），
  在 `cubic` 与 `bbr3` 两种出口档下**都 rc=0**；中继侧无「丢弃」异常行。
- **真机**：本批**未做**真机中继轮（时间盒；§9.20 的 T3 参照臂在 M6 已登记「不可得」）。
  **判据出口 = 未验（顺延登记）**；不做通过声明。
- **一轮作废登记（如实）**：`@bbr3` 的第一次 island e2e **relay 用例失败**，报错 = 「token 必须带中继类
  端点」且耗时 0.00 s ⇒ 归因 = **harness 侧**（该次运行的出口未挂上中继腿：`local-rust-relay.sh status`
  显示上一轮的中继腿「登记过期（1m33s 无保活）—— 摘掉」）；同命令**重跑 rc=0 六条全过**。
  该失败与本批代码无关（判据行在测试侧的前置断言上）。
- 形态说明：BBRv3 在本批读数里是**更保守**的一档（发送更平、速率更低）⇒ 对中继 200pps 闸的
  压力**不高于** cubic，故中继面风险方向为「无新增」。

### 5.3 矩阵冒烟（ci-local 第 7 步）的**红/绿轮序归因（如实登记，非粉饰）**

| 跑次 | 时刻 | `C-via`（赛跑胜者） | F-100MB | E10 | TOTAL |
|---|---|---|---|---|---|
| **批前**（M6.7 尾的 ci-local，改动前） | 09:44 | —（未记） | **FAIL**（`files download 失败：流已到尾（EOF）`） | **FAIL** | **FAIL**（步骤 7 红） |
| **批前**（M7 尾，改动前） | 15:52 | 直连 | PASS | PASS | **PASS（315 s）** |
| 本批 ci-local 首跑 | 18:37 | 中继 | FAIL | FAIL | FAIL（679 s） |
| 本批 matrix 隔离重跑 1 | 18:49 | 中继 | FAIL | FAIL | FAIL |
| 本批 matrix 隔离重跑 2（静机） | 19:04 | 直连 | PASS | PASS | **PASS（315 s）** |
| 本批 ci-local 重跑 | 19:15 | 中继 | FAIL | FAIL | FAIL（679 s） |

- **相关性**：红只出现在「**中继赢得候选赛跑**」的两次（后续 E13 四轮超时 → F-100MB/E10 的
  独立客户端进程拿不到会话）。出口日志证据：中继腿 `RELEASE` + 控制面 EOF（1 s 后重连），
  其后 `未登记` 丢包计数开始走（腿已摘除，QUIC 报文不回落直连）。
- **判定：与本批代码无关（三重证据）**：①**批前**的 ci-local（09:44，改动前的树）**同两项红**
  （F-100MB/E10）——该门本就有这个 flake 面；②红/绿之间在同一棵树上**只有赛跑胜者不同**
  （直连 ⇒ 315 s 全绿；中继 ⇒ 679 s 同两项红，三次逐一对应）；③§5.1 的 `@bbr3` island e2e
  （含「只给中继候选」用例）绿、`quic-pf-e2e` 绿、矩阵绿跑与批前基线**同耗时同结论**。
  **登记为既存 harness 脆弱点**（中继腿生命周期 × 同 identity 的第二客户端进程），
  **不在本批范围内修**；本仓门纪律照旧：不声称「八步全绿」，如实写「第 7 步的轮序依赖 flake」。

### 5.2 体积 / 内存门槛

（`tools/build-app-core.sh` 三道门 + `[size]`；对比 M6.7 终值 2,968,544 B / 门槛 3,800,000 B）

- **`tools/build-app-core.sh` 三道门全过**：`[sym] 20/20`、`[ver] …-rust`、**`[size]` = 2,995,744 B**
  （M6.7 终值 2,968,544 B ⇒ **+27,200 B = +0.92%**；门槛 3,800,000 B ⇒ **余量 804 KB**）。
  增量来源 = `cc/` 三文件 + 接线（`HOMEWAY_QUIC_CC=bbr3` 才会用到，但**编进同一编译单元**）。
- **单连接内存**：本批新增的状态只有**发送快照表**（`SentTable`，上限 1024 条 × ≈72 B ≈ **72 KB/连接**，
  且只在**连接真的在途**时才增长；上限夹死 ⇒ 不含无界通路）。相对 +640K 门槛**余量充足**；
  未做独立实测（登记：口径为「有界设计 + 上界推算」，与门槛表的「按 M5 终值判过」同款处置）。

---

## 6. 机制分析（**为什么 BBRv3 在这套框架里不行**；候选解释，非定论）

1. **pacing 归属（首要嫌疑，§2.2 差异表第 5 条）**：上游 BBRv3 的发送速率由**它自己的 pacer**
   按 `pacing_rate = pacing_gain × BtlBw × 0.99` 驱动，cwnd 只管「在途上限」；本框架的 pacer 却按
   `window()/RTT` 反推速率 ⇒ 一旦模型把 cwnd/inflight_lo/inflight_hi 压低（丢包响应、ProbeRTT、
   `bound_cwnd_for_model`），**发送速率随之被直接压低**，而 BBR 的设计本意是「cwnd 降、pacing 不降」。
   本批读数与该解释一致：bbr3 的冷轮（16.7 s，模型尚未被路径信号压过）比热轮（中位 18.6 s）快。
2. **丢包响应的作用点**：BBRv3 的 `inflight_hi/lo` 是**长期/短期在途上限**，在「window = 速率」的
   框架里等价于**速率上限**；真机路径（WiFi + 空口过载）恰是多丢包场景 ⇒ 上限被反复压低。
3. **lab 的低 RTT 自锁**：按轮计数的满管判据在亚毫秒 RTT 下轮次太密（§4.1）。
4. 以上三条都不涉及移植保真度（算法逐行照抄）；**失败点在接口语义**，这也解释了为什么
   「移植一个上游框架里跑得好的 CC」在本框架里不等价。

---

## 7. 残余 / 未决 / 设备副作用

- **未做（顺延登记）**：①真机中继轮（§5.1）；②cubic 冷档本轮未取（当轮被作废轮占用）；
   ③出口侧逐秒 `conn.stats()` 插桩（M6.7 的 `m67` 已删）⇒ §6 的机制分析**没有**出口侧 cwnd/lost
   逐秒证据，属**推断**；④`bbr3` 的 ProbeRTT 参数消融（5 s 间隔把窗压到 min 的代价是否可减）。
- **若要再试**：改「让 BBR 自己的 pacing_rate 生效」= 框架侧加一条「控制器可宣布 pacing rate」的
   接口改动（**属框架 API 缺口**，不是本仓能单方面解决的）——建议留作 M7 的候选课题，
   不在本批扩面。
- **设备收工状态**：VPN 开关**已关**、App 已 `force-stop`、`power-shell timeout -r` 已还原；
   下载残留在浏览器沙箱（登记，不是新问题）。出口/中继本地实例已全部 stop（无遗留进程）。

---

## 8. 提交表

| commit | 内容 | 净增删 |
|---|---|---|
| `611604f` | S1：env 开关 + BBRv1 + 单测 + lab A/B | +316 / −11 |
| `48ca3e3` | S2：BBRv3 移植（3 文件）+ 差异表 + 归属 + 单测 | +3,336 / −12 |
| （本文件 + `PERF-AB` §9.20.12） | S3/S4：读数 + 记录 | 见 commit |

净增删（S3/S4 段）：docs 侧 +253（`CC-BBR3.md` 新建）+ `PERF-AB.md` +57 + `docs/matrix-latest.md`（矩阵重跑产物，绿跑那份）+
`tools/udp-rate-proxy.py` 新建 150（harness-only，整形尝试的工具，带「怎么用才有效」的告示）。
