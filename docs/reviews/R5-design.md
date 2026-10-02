# R5 技术设计（开工前评审输入 v2，2026-10-03；v1 评审 7高/12中/7低全处置）

> 范围 = ROADMAP R5：互操作矩阵全量 + fuzz + 性能 A/B + 豁免批 + 台账三方门 + 本地 CI。
> 必读材料已读：AGENTS/ROADMAP（R5 节+隔离条款+评审协议）/BASELINE/INTEROP-CRITERIA/
> R2.md（低-4 等）/R3.md（6 豁免）/R4.md（遗留）。v1→v2 逐条整改表 = `docs/reviews/R5.md`
> 第一道门整改记录节。

## 0. 链路定义与组合空间

{Go,Rust}×{exit,client,relay} 全组合 = 2³ = 8；**减平凡 2**（GGG 纯 Go、RRR 纯 Rust——
不含任何跨实现互操作面，各期已反复实证）= **6 条混合链路**：

| # | 链路 | exit | client | relay | 单变量叙事 |
|---|---|---|---|---|---|
| L1 | GGR | Go | Go | Rust | 只换 relay（R4 链路 1 复现） |
| L2 | GRG | Go | Rust | Go | 只换 client（R1 判据复现） |
| L3 | GRR | Go | Rust | Rust | Go 出口 × Rust 栈 |
| L4 | RGG | Rust | Go | Go | 只换 exit（R3 判据复现）；**首次方向：Rust exit → Go relay 控制面** |
| L5 | RGR | Rust | Go | Rust | Rust 出口 × Go 客户端经 Rust 中继；首次方向同 L4 |
| L6 | RRG | Rust | Rust | Go | Rust 栈 × Go 中继（R4 链路 3 近似） |

GGG/RRR 不进互操作判据集，但**进矩阵脚本**（`--perf` 档）：PERF-AB 的吞吐 A/B 需要
Go↔Go vs Rust↔Rust 同机同刻对照（§3/§4）。

## 1. 必答①：矩阵编排的端口/状态隔离与防串扰

### 1.1 端口规划表（全部落在 4264x–4290x 段，与既有脚本默认实例互不重叠）

| 段 | 用途 | 归属 |
|---|---|---|
| 42640–42649 | Go 出口（42640+n） | local-exit.sh（既有） |
| 42650–42659 | Rust 出口（42650+n） | local-rust-exit.sh（既有） |
| **42660–42667** | **矩阵出口**（42660+i，i=链路号 1..6；42660/42667 留 GGG/RRR perf 档） | matrix.sh（本设计） |
| 42690–42739 | 历史注入临时口（R2/R3/R4 用过 42697/42699 等） | 保留 |
| 42740–42749 | Go 中继（42740+n） | local-relay.sh（既有） |
| **42750–42757** | **矩阵中继**（42750+i；42750/42757 留 GGG/RRR perf 档） | matrix.sh（本设计） |
| 42780–42789 | Rust 中继（42780+n） | local-rust-relay.sh（既有） |
| **42800–42809** | **矩阵 transit/echo 监听口**（42800+i：链路本地回环监听，E10 transit 产出 + §3.8 echo RTT 测量目标） | matrix.sh（本设计） |
| **42900–42909** | **客户端本地 forward 监听口**（42900+i：Go forward / Rust portfwd 的本地口，echo RTT 测量入口） | matrix.sh（本设计） |

口径注记：不重叠的成立条件 = 既有脚本用**默认实例号**（1/2）——矩阵运行期间不与
既有脚本并发（`--lock` 互斥也挡不住跨工具，纪律挡）；matrix.sh `#!/bin/zsh`
（复用 `${=VAR}`/`print -r` 形态）。

### 1.2 状态隔离（链路级 + **段级**）

- 全部 state 在 `/tmp/homeway-rs-matrix/<链路名>/`（exit/relay/client-main/client-sub
  四子目录 + files/ 文件根）；**每链路开始前 wipe、结束后 stop+wipe**。
- `/tmp/homeway-rs-matrix/.lock` mkdir 互斥（占用即 fail-fast 并提示持锁者）。
- 客户端 config.toml 双角色断言（serve/relay 双关，写后断言）——local-exit.sh H1 纪律。
- 出口一律 `--bind-interface none --upnp=false --stun= --stun6=（Go 两侧都关）
  --public-endpoint 127.0.0.1:P --files-root /tmp/homeway-rs-matrix/<链路>/files`
  （**files 根显式隔离**——不落 $HOME，跨轮残留可删）；中继一律
  `--listen/--advertise 127.0.0.1:P`。绝不触发现役出口/中继（隔离条款 2）。
- **段级隔离（①-2 整改）**：每链路一个 identity 目录**跨段复用**（保 devTag——
  base 段 n=1 → relay 段 refresh 不新增 → 多 peer 段恰 n=2）；**每段独立 endpoint
  cache**：Rust 客户端 `--endpoint-cache-dir` 按段分目录；Go 客户端段间
  stop → `rm -rf <state>/cache/endpoints` → 重启 daemon → host add 变体 token
  （同 state 保 identity；变体 token 是 dead-direct 形态）。
- token 不落任何仓库路径；只存于 /tmp state 与进程环境。

### 1.3 防串扰（七道防线）

1. **串行执行**：六链路逐条跑；链路内三段也串行（基础 → 多 peer → 中继）。
2. **前后清场断言**：每链路开始前检查 lock + `lsof` 本链路端口未占；结束后 stop
   全部进程（**三类进程 + echo 监听**，各记 pid 进 stop 阶梯）+ 残留检查
   `ps -o pid=,args= | grep homeway-rs-matrix/<链路>` 为零才进下一条。清理失败 =
   该链路 FAIL（不清场 = 串扰源）。
3. **端口退让硬失败**：起出口后核对 `listen_port.txt == 配置端口`，退让即 FAIL 并停。
4. **日志只看本轮新增行**：`wait_line_from` 记起始行号（M12 纪律）。
5. **链路间身份目录隔离 + 段间 cache 隔离**（§1.2）。
6. **files 落点轮次随机后缀**：上传目标 `mat-<link>-<rand>.bin`，对账后删远端——
   同名残留不可能掩盖失败。
7. **矩阵互斥锁**：`.lock` mkdir，跨轮/跨会话防并发。

### 1.4 每链路判据集

段序（①-6 时序）：基础段（主客户端直连全判据）→ 多 peer 段（副客户端加入）→
副收工 → 中继段（主客户端换变体 token 重连）→ 全收工。主客户端全程存活
（Rust `--hold` 覆盖全链路预算；Go daemon 常驻）。

**基础段**（六链路全跑）：
- R-ready：中继就绪（R1）+ exit `serve 就绪`（E1）+ 注册腿（X1 两行：`注册腿开跑`/
  `注册成功`；中继侧 R3 `后端 … 注册成功`）。**L4/L5（Rust exit→Go relay 首次方向）
  X1 四行全列**：注册腿开跑/注册成功/`中继控制面：中继身份已认证（OK-MAC 通过）`/
  `中继控制面已连`。
- C-ready：客户端就绪（Go：C16；Rust：C8 `warmup pong: 就绪（判据=wg）`）+ C3。
- C-via-direct：`link: via=direct`。
- E7：`peer: + dev=… n=1/32`。
- E13 + 吞吐：speedtest 双向（Rust 客户端走新 `speedtest` 动词 --rounds 1 或
  connect --speedtest；Go 走 daemon speedtest），exit 结算行 + 摘要数字入 PERF-AB。
- F-100MB：files 100MB 上传+下载（随机后缀名），sha256 双侧一致。
- E10/E11：intercept transit `dialok` + 关闭行（Rust：`--dial 127.0.0.1:4280i`；
  Go：`forward add --listen 4290i --target 127.0.0.1:4280i` + 本地一段载荷；矩阵在
  4280i 起 python echo 监听）。
- FB（files busy，G-3 实装后）：第 17 条并发流回 `server_busy`（脚本并发 17 条
  list 流断言最后一条报 busy）。
- DNS（Rust 客户端链路 L2/L3/L6）：`dnstest --mode leg` 一查 rcode=0（E12 顺带：
  exit `udp intercept: 会话 #N dns 建立`行）；M3 落地后补 TCP :53 对照（dnstest
  新 mode 或等价）。

**多 peer 段**（六链路全跑）：
- 副客户端（与主客户端异实现）建连 → exit `peer: + … n=2/32`（等行；先主在位
  后副连接——①-6 时序）。

**中继段**（六链路全跑——组合里都含 relay）：
- 注入：主客户端 stop → **清 endpoint cache**（§1.2 段级隔离）→ 变体 token
  （Direct 端点 → 127.0.0.1:1）新会话。Rust 客户端 `--dead-direct`；Go 客户端用
  `homeway-cli token <tok> --dead-direct` 输出变体（**新测试缝**：token 动词加只读
  改写输出，吃 R0 的 parse_body/encode，crc4 无密钥重算即过——评审确认可行）。
  Rust relay 链路加 `--no-hints`；Rust 客户端链路可再叠 relay-lock（test-seams）。
- 判据（**按 relay 实现分口径**——①-1 整改）：
  - **Rust relay 链路（L1/L3/L5）**：硬判据 `link: via=relay` + RREG `中继=true` +
    经中继 speedtest（10s 窗）+ 经中继 files 5MB sha256 对账 + R7
    `中继：客户端 … 起会话 #N`行。段预算 < 300s（升级窗 5×60s 前）。
  - **Go relay 链路（L2/L4/L6）**：判据 = **首窗** via=relay（DirectFirst 解锁后
    首个 link 行）+ 首窗数据经中继（speedtest 10s 窗跑通 + R7 行）+ RREG 中继=true；
    **翻直连记为预期自愈观测**（结果表备注列，非 FAIL——Go 设计内建行为，R4 实测
    同款）；不设驻留判据（Go relay 无 --no-hints，无确定性钉法）。
- Go 客户端 host add 变体的预期档 = 「中继可达」（host_cli 验证三档之一）；若验证
  拒绝入表 ⇒ 退出口：实测定（--force 兜底或降级登记）。

**perf 档**（`--perf`，GGG/RRR 两条）：基础段 + speedtest 3 轮（交替）+ echo RTT
分布 + RSS 1Hz 轮询（§3）。

### 1.5 结果表与可重跑

- 输出：stdout 进度 + `docs/matrix-latest.md`（链路 × 判据 → PASS/FAIL + 摘录
  ≤100 字符 + 备注列（预期自愈等）+ 耗时）；失败行全文进
  `/tmp/homeway-rs-matrix/<链路>/failures.log`。
- 退出码：全绿 0；任一 FAIL 非零。`--link L1` 单链路；`--smoke` = RRR 基础段前
  4 判据（CI 冒烟档）；`--fail-fast` 可选。
- 可重跑验收 = 全量连跑两轮均全绿（每轮自 wipe 自清场）。
- `#!/bin/zsh`；`set -uo pipefail`；每链路独立函数域。

### 1.6 未纳入判据表（①-7 整改）

| 面 | 理由 | 归属 |
|---|---|---|
| E12 全 UDP 应用面（非 DNS） | Rust CLI 仅 dnstest 三面（已采 leg 形态）；Go 客户端无 UDP 拨号动词 | R7 真机（App UDP 面） |
| E15/E16（term） | term 服务面 R6 才有 | R6 |
| E17（speedtest 就绪行） | serve 装配行，非链路行为判据；R3 已实采 | —（E13 已覆盖行为） |
| E9 族（TTL/stale/revoked） | 注入型判据，R2/R3 已单测+实采钉死；矩阵跑会拖长每链路 10min+ | 保留在单测/历史实采 |
| E21/E22/E23 出口装配族 | 非互操作面（本机形态）；R3 已实采 | — |
| portfwd X1 旁支 | forward 本身作为 E10 手段已覆盖 transit 面；映射 CRUD 属 CLI 面 | — |
| C11 恢复阶梯全族 | 注入型时间窗判据（R2 四档实测入册）；矩阵全链路跑每条 +40s×N | 单测 + R2 实测档 |

## 2. 必答②：fuzz 目标选择与 corpus 策略

### 2.1 目标面（按网络可达面四类重排；全部先经 ②-1 纯解析抽取）

**前置步（硬前置，不接受 cfg(fuzzing) 缝）**：把下列私有解析抽成 pub 无 IO 纯函数
（IO 留薄封装——AGENTS「借用优先 + 测试可达」口径）：
`speedtest::decode_frame(buf)`（帧头）、`files::decode_prefix`、`probe::decode_response`、
dnsface 2B 分帧 `decode_tcp_frame`、dnsproxy 应答纯函数面（已有 message 纯函数族，
补 pub 入口）。

| 目标 | 被测面（file） | 攻击面 |
|---|---|---|
| fuzz_upnp（**最高优先**） | upnp.rs：header_value/http_call 状态/parse_http_url/xml_tag/igd 描述解析 | LAN 组播 + 攻击者可控 LOCATION 的 HTTP（**R3-H4 远程打崩前科面**） |
| fuzz_leg_frame | frame.rs：decode_frame/decode_tagged/decode_batch/decode_hint_payload + device.rs WG 报文类型分派 + table.rs verify_reg（66B H2 帧） | UDP-any（腿帧/容器/信封/reg） |
| fuzz_relay_ctl | relaywire.rs 全 decode 族 + CtlDecoder::feed 跨 chunk | 中继 TCP 控制面 + UDP 握手包 |
| fuzz_token | token.rs：decode/parse_body | 用户粘贴面 |
| fuzz_speedtest | speedtest.rs decode_frame + parse_report；**speedtest_server.rs** 三个 reader + parse_request | 出口↔客户端双向（隧道可达） |
| fuzz_files | files.rs decode_prefix；files_server.rs 4B reader + 请求 JSON 解析 | 隧道内任意已注册 peer |
| fuzz_dns | dnsproxy 消息纯函数 + dnsface 2B TCP 分帧（M3 落地后含新腿） | UDP :53 任意报文 + TCP DNS |
| fuzz_inner_pkt | intercept/nat.rs Ipv4View::parse（+ TCP/UDP 头视图） | 解封后任意字节 |
| fuzz_probe | probe.rs decode_response（flags 字节） | unconnected recv_from 任意源 |

不 fuzz：smoltcp/boringtun 内部（上游责任，黑盒经上述入口覆盖）。

### 2.2 harness 形态（双轨，都必须跑）

1. **cargo-fuzz（深挖轨）**：`fuzz/` 独立 crate + `fuzz/rust-toolchain.toml`
   （nightly，防根 1.99.0 钉死）+ 根 `Cargo.toml` `exclude=["fuzz"]` +
   `fuzz/Cargo.toml` 重复 `[patch.crates-io] ring = …`（依赖组合不漂）。ASAN 默认。
   每目标 ≥100k execs（`-runs` 读数 + `-max_total_time` 兜底）。
   **降级判定**（可判定步骤）：`rustup toolchain install nightly` 失败或
   `cargo install cargo-fuzz` 失败 ⇒ 轨 2 为主轨 + ROADMAP 登记（验收等价：轨 2 的
   ≥100k 已满足判据）。
2. **结构化随机重放（回归轨，进 cargo test）**：
   `crates/homeway-core/tests/fuzz_replay.rs`，确定性 xorshift + 固定 seed（`HER-SEED`
   可复现）+ `--full` 随机 seed 档（seed 写输出）。每目标 ≥100k 次迭代。
   CI quick 档 `#[ignore]` 跳过、全量档 `--ignored` 显式开。

**oracle（②-3 整改，panic 只是底线）**：每目标四类断言——
(a) 不 panic（含 ASAN 面由 cargo-fuzz 承担）；(b) **分块等价**：同一字节流按
{1B/随机/半帧}三种切块喂入，解析结果逐字段一致（直接钉 drain 边界类 bug——R2
critical bug 形态）；(c) **往返一致**：合法夹具 encode→decode→encode 字节稳定；
(d) **夹具期望**：`fixtures/vectors/*.json` 的 decoded 字段逐字段比对；加输出长度
上界断言（防分配放大）。

### 2.3 corpus 策略

- 种子单源 = fixtures/vectors（token 18 例/files 帧 3 例含 70KB 跨 u16/relay 控制帧
  /reg 报文）+ golden 夹具；`tools/gen-fuzz-seeds.sh` 运行期展开成每目标种子目录
  （CI 校验展开 hash），**不重复入库字节**；`fuzz/corpus.seeds/` 只存手工合成样本
  （error 含 `,`/`"`/`\` 的 report、截断 hint、713 body 等）。
- 生成配比 70% 骨架变异（长度/类型字节/截断点/嵌套容器）+ 30% 全随机。
- cargo-fuzz 的 corpus/ 产物 gitignore；crash 不入库（发现即修，回归用例进 tests/）。

### 2.4 已落地的先修项

低-4 引号感知：**工作区已实现**（`split_top_level` 引号态扫描 + `\"` 转义 +
`unescape_minimal` + 两测试；服务端 `parse_request` 同款已修共用 pub(crate) 切分）。
本批动作 = 与 Go `encoding/json` 输出字节对拍（vecgen M20 批顺带）+ 提交。

## 3. 必答③：A/B 测量的公平性口径

1. **双栈常驻、按轮交替**（③-4 整改）：GGG 与 RRR 的 exit/relay **同时常驻**
  （42660/42667、42750/42757 留口），轮间交替发起新会话测量；声明空闲对端仍在跑
  巡检（60s 一拍，影响记入噪声带）；每轮记录 `uptime`/loadavg；端口「结构同构、
  端口号不同」。
2. **预热**：每轮 speedtest 自带 2s warmup（两侧同参数：10s/10s 窗、4 流——已核对
  Go DefaultParams = Rust Params::default 同值）；交替序列 G,R,G,R… 消时段漂移。
3. **轮次**：**轮 = 同一会话内一次 speedtest::run**（③-1 整改：Rust 侧新独立
  `speedtest` CLI 动词 `--token/--identity-dir/--endpoint-cache-dir/--rounds N`
  复用 Session；Go 侧 `homeway speedtest --state -host` daemon 常驻同会话——两侧
  口径一致）。吞吐每侧 ≥3 轮取中位；echo RTT ≥200 样本。
4. **构建口径**：Rust release；Go = baseline 克隆构建（GOTOOLCHAIN=go1.24.5 基线门）。
5. **拓扑同构**：同回环形态、同 speedtest 参数、同 echo 目标；中继链路吞吐单列
  （200pps 防放大 = Go 同值锚，报告单节说明 + R2 经中继上行 2.7Mbps 实测锚引注）。
6. **RSS**：1Hz 轮询 `ps -o rss=` 取 **max**（测量期全程）+ 就绪稳态值两口径；三角色
  各自 pid；GOGC/GOMAXPROCS 未设置记录默认；注明「量级对比，非逐字节口径」。
7. **体积两口径**：原始构建产物 + strip 后（`cp … /tmp/x && strip /tmp/x`；Go 侧
  `go build` 原始 22MB 现状基线 + `-ldflags "-s -w"` 对照），构建命令行与 sha256
  入报告。
8. **尾延迟 = echo RTT 分布**（③-2 整改）：经隧道单连接 **200 次**回显往返
  （python 测量器走客户端本地口——Rust 链路 portfwd 4290i、Go 链路 forward 4290i，
  目标 = 4280i echo 服务；**测量侧同一脚本，两链路公平**），报 p50/p95/p99（ms）。
  E13 `用时` 改名「窗口结算抖动」如实报告（注明 down 由出口计时、up 由客户端报，
  计时源不同侧）。
9. **预登记阈值**（③-3 整改，报告可 falsify）：吞吐中位比值 Rust/Go ∈ [0.5, 2.0]
  判不劣化；echo RTT 中位差 ≤ 2ms 或 ≤50%；RSS 比值 ≤ 4×（同量级）；体积报比值。
   超阈处置 = 复测一轮 → 仍超则 PERF-AB 归因节登记挂账（R5 量化不优化）。

## 4. PERF-AB 报告结构（docs/PERF-AB.md）

1. 吞吐：GGG vs RRR down/up 中位 + 轮次表；六混合链路直连段 speedtest 一览；
   中继链路吞吐单节（200pps 锚 + 量化数据——R4 遗留收口）。
2. 延迟：echo RTT p50/p95/p99（GGG vs RRR）+ 窗口结算抖动（E13 用时分布，注明口径）。
3. 常驻 RSS：三角色 Go vs Rust（1Hz max + 稳态两列）。
4. 体积：两口径表 + PoC 1.0MB 锚关系一行。
5. 方法论节（§3 全文）+ 预登记阈值 + 结论行（每维一句可执行结论）+ 归因挂账节。

## 5. 豁免批处置方案（5d）

| 项 | 来源 | 处置 |
|---|---|---|
| 低-4 引号感知（双侧） | R2/R3 | **已落地**（§2.4）；提交 + M20 对拍 |
| M3 非隧道 :53 TCP 代答腿 | R3 | **做**：per-flow 同步逐消息 DNS-over-TCP（2B 分帧复用 dnsface 抽取面）；**五条口径**（① 同步逐消息无 per-conn txid 表——上游 exchange 统一随机化 ID 并回填原 ID，Rust 复用 dnsproxy 既有上游 ID 改写面 ② TCP 面独立 64 连接闸 ③ 30s idle ④ 256 在飞 ⑤ TCP 腿不回投 1232 截断——截断仅 UDP 面）。验收 = 本地 TCP :53 一查 rcode=0 + dnstest 新 mode 对照 Go exit 同答 |
| M8 残余 SSDP 三件套 | R3 | **做**：IP_MULTICAST_IF（钉卡 socket option）+ TTL=2 + 组播口钉卡；本地不可实测（macOS 本地网络隐私，与现役出口同款）——单测只证 setsockopt 返回；判据登记进 INTEROP-CRITERIA（真机判据 + 边界说明），R7 消费 |
| M11 UPnP ctx 统一 | R3 | **做**（G-1 整改口径）：**40s 总 deadline 贯穿 SOAP/描述文件**（对齐 Go http.NewRequestWithContext——Go 无 per-SOAP 5s），M-SEARCH 保留 5s 与总 deadline 取小，**缩租路径独立 8s deadline**；取消链路 = 收工路径 deadline 收紧 + join 有界 |
| M20 STUN/SPED golden | R3 | **做**：vecgen 模板目录扩 `tools/vector-gen/stun_sped/`（跨包第二落点：baseline 克隆内 `pkg/egress` + `pkg/speedtest` 真源直调——NewTxID/StunRequest/ParseStunResponse/PumpData/WriteControl 导出面已核可行）；产出 `fixtures/vectors/stun_sped.json` + SHA256SUMS 更新步骤入脚本头注 |
| M23 egress/upnp 错误类型化 | R3 | **已落地**（EgressError/UpnpError thiserror，Display 同串；138 测试绿）；提交 |
| M24 speedtest 超时归因 | R3 | **已落地**（timed_out 旗标：看门狗触发 ⇒ 连接级错误统一归因 timeout；busy 归因 R2 已有）；归因值只落在现有 7 个 REASON_*（无新常量——词表门耦合面）；提交 |
| enum Auth 类型化 | R4-§7.8 | **已落地**（Auth{Open,TokenVerified} + 结构性守卫）；提交 |
| files server 并发闸 | R3-G3 | **做**（5-d2）：MAX_CONNS=16 在册计数 + 满则吞请求行回 server_busy + `files: 并发流已满（%d）`诊断行 + 判据 FB |
| 出口侧测试扩展（Go 对照面） | R4 中-4 后续 | **做**：腿表/条纹已有单测上补 relaywire 字节对照消费点用例（golden 族向量）+ M3/闸门新面 |
| 中继吞吐量化 | R4 | **做**（矩阵中继段 + PERF-AB §4.1 单节） |
| v6/双栈族 | R3-M10 | **登记**（R7 真机前——R3-design §4.2 已登记，本批不动代码） |

## 6. 台账三方门（5e，tools/check-vocab.sh）

真源链：homeway `contracts/ledger.jsonl`（422 单元）→ tier
`tools/gen/vocab-manifest.json`（16 App 消费单元，只读）→ tier `model/gen/Vocab.ets`。
**Rust 侧对账**（G-7/G-8/G-9 整改口径）：

- **提取面 = 编译期真源**：`crates/homeway-core/tests/vocab_dump.rs`（`#[test]`）
  把五组值集打印成稳定格式（`unit<TAB>value` 行），脚本只消费其输出——不做源码
  正则解析（三种声明形态/诱饵字面量都天然消除）。五组映射：
  `speedtest.rs REASON_* 七常量 ⇄ speedtest-reason/reason`；`PortfwdErr`（**新类型化**：
  enum + as_str，main.rs 字面量改引用）⇄ `portfwd/err`；`Via::as_str ⇄ event-payload/via`；
  `SessState::as_str ⇄ event-payload/state`；`files.rs CODE_* ⇄ files-proto/code`。
- **门禁逻辑**：① ledger（baseline 只读，sha256 先对 BASELINE.md 锚定值断言）按
  **三重过滤**（family+unit 匹配 / status ∈ {active, compat-passthrough} / faces 含
  声明面）得值集；② **过滤后 active 集 ⊇ Rust 声明集**（超集不是恰等——Rust 核
  只承担 cli/cp/direct 面）；③ Rust 声明集 ∩ legacy-unreachable = ∅（如 via/tunnel
  不得出现）；④ **带理由允许缺席表**（fail-closed 只对未登记缺席生效）：
  speedtest-reason 的 `bridge_down`/`bridge_auth`（App bridge 面，Rust 无 bridge 前
  无生产者）、portfwd 的 `dial_failed`/`invalid_target`（登记保留值）；⑤
  `files-proto/code` 采用 tier 排除表语义（tier check-vocab-sync.sh 本就把它放
  排除表——族④真源码，App 消费经 bridge-files 透传）：**不做 manifest 存在性检查**，
  只对 ledger 值集做 ②③。
- manifest（tier 只读）的用途 = 校验「tier 侧 16 单元定义与 Rust 侧承担面无交叉
  遗漏」（交叉面报 INFO 不红——App 面值集归 R7 napi 批）。
- 不写生成物进 Rust 仓（词表值仍手写对齐 + 编译期 dump 对账；生成化收益 R7 评估）。

## 7. 本地 CI（5f，tools/ci-local.sh）

编排（顺序，前者失败即停）：
1. `tools/check-baseline.sh`（基线门）
2. `cargo test --workspace`（quick 档不含 fuzz_replay——`#[ignore]`）
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. 向量确定性门：`tools/gen-vectors.sh` → `(cd fixtures && shasum -c SHA256SUMS)`
   → `git diff --exit-code fixtures/vectors`（**SUMS 校验补齐**——G-11；47-50 行
   前缀先修）
5. `cargo test --workspace --ignored`（fuzz_replay 100k 全量档；quick 档跳过）
5.5 `cargo build --release -p homeway-cli`（smoke 档前置——G-10）
6. `tools/check-vocab.sh`（三方门）
7. `tools/matrix.sh --smoke`（RRR 基础段冒烟）

档位：`--quick`（1–4+6，热 target ≈2–3 分钟）/ 全量（+5/5.5/7，热 target ≈12–15
分钟；冷构建 boringtun/smoltcp/ring-shim 首轮显著更长，预算注记前提）。每步
PASS/FAIL + 总退出码。

## 8. 拆步与提交计划

| 步 | 内容 | commit | 依赖 |
|---|---|---|---|
| 5-0 | 本设计 v2 + 评审记录 + 整改 | 1 | — |
| 5-d1 | 复核提交低-4（双侧）+ M24 + enum Auth + M23（已落地，打包提交） | 1 | 5-0 |
| 5-d2 | M3 TCP DNS 腿 + M8 SSDP + M11 UPnP deadline + M20 golden + files busy 闸 + 出口侧测试扩展 | 2 | 5-d1 |
| 5-b | 纯解析抽取（②-1）→ fuzz 双轨 + 种子 + ≥100k | 2 | 5-d2（M20 向量做种子） |
| 5-a | matrix.sh + 六链路两轮 + 结果表入库（含 Rust speedtest 动词 + token --dead-direct 缝） | 2 | 5-d2（busy/DNS 判据面） |
| 5-c | PERF-AB.md（perf 档数据 + echo RTT 脚本） | 1 | 5-a |
| 5-e | vocab_dump + PortfwdErr + check-vocab.sh | 1 | 5-d1 |
| 5-f | ci-local.sh + SUMS 修复 + BASELINE.md 流程补 + 第二道门 + ROADMAP 收口 | 2 | 全部 |

## 9. 风险与退出口

- **矩阵超时**：六链路 × 三段 ≈ 40–50 分钟；每判据等待有秒上限，链路硬预算 12
  分钟（超时 FAIL 并清场）。
- **Go relay 驻留无钉法**（①-1）：分口径处置（§1.4）；Go relay 链路翻直连 = 预期
  自愈观测（备注列）；若首窗 via=relay 都拿不到 ⇒ 真 bug（DirectFirst 解锁或中继
  候选缺失面），FAIL 上报。
- **段间 cache 复活**（①-2）：段级隔离已设计；若 Go 客户端清 cache 后仍带直连
  候选（学习源 = token 本体），检查变体 token 形态（Direct 端点应全为死端口）。
- **R10 节拍**（①-3）：已降级为附带观察（≥65s 等待计入链路预算才看）。
- **L4/L5 首次方向**（①-8）：Rust exit→Go relay 控制面失败时，先 relaywire 向量
  对 Go 真源逐字节对照（vecgen 已有），再定中继侧缺口归属；预算 +10 分钟。
- **fuzz 抽取硬前置**（②-1/G-14）：纯解析抽取不做完不开 fuzz（不接受 cfg(fuzzing)
  或跳过）；nightly/cargo-fuzz 不可得 ⇒ 轨 2 主轨 + 登记（验收等价）。
- **files busy 闸**（G-3）：实装若与 Go 判据行有措辞差 ⇒ 以 Go 串为准修 Display。
- **CI 冷构建**（G-10）：预算注记热 target 前提；冷环境首轮 10 分钟级属预期。
- **同机吞吐抖动**：阈值带内皆过；超阈处置 = §3.9（复测 → 归因挂账）。
- **Go 客户端 host add 变体被验证拒**：实测定（--force 兜底或降级登记，①-9）。
