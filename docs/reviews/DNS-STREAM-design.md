# DNS-STREAM 承接批 · 设计稿（`STREAM[tag=6]` dns-resolve，2026-10-12）

> **任务书**：用户 2026-10-12 指令「做了」——把 M5 遗留的待裁决项 **§12-C-8**（`:5300` 解析腿
> 承接 vs 退役）按**承接**执行：新增 `STREAM[tag=6]`，恢复 socks 域名目标的远程解析（CA5）。
> 背景与现状盘点见本稿 §1；M5 的退役执行与登记（CA5 降级）见 `M5.md` / INTEROP-CRITERIA
> 「判据变更记录」2026-10-10 条。
>
> **决议来源**：M5-design §12-C-8 原文建议即「承接」（理由：socks 域名面是产品能力；CA5/E4
> 以它为前置），当时因「M5 = 删码批、不擅动出口 wire（增项）」而待裁决 ⇒ 本批立项落地。

## 一、现状盘点（逐点回源核实，2026-10-12）

| # | 事实 | 出处（已核） |
|---|---|---|
| 1 | 出口在**隧道栈内**同时服务 `:53`（UDP+TCP，手机解析）与 `:5300`（TCP，客户端远程解析腿），两条腿共用同一 `dnsproxy` | `intercept/mod.rs:828`（`dns_ports` 只装 `:53`/`:5300`）、`dnsproxy.rs:564`（`submit_tcp` 的 `qtcp` 计数 = 「出口 5300 收到 TCP 查询」判据面） |
| 2 | WG 时代宿主会话经**拦截层回环豁免**到达 `:5300`；M5 豁免面退役、服务腿改 `STREAM[tag]`，而 tag 表穷尽为 1..5（无 6）⇒ `tag_for_port(5300) = None` ⇒ `DialErr::NoService(5300)` | `stream.rs:26-83`；`facade/quic_stream.rs:43`；`daemon/mod.rs:263` |
| 3 | **唯一生产消费者** = socks 承载面域名目标（Go 侧 `socksDNSPort` 全仓唯一使用点 = `socksmgr.go:359`；Rust 同源 `socksmgr.rs:364`）；另有 Rust-only 排障动词 `dnstest`（已退役，G6）；**App 不使用**（tier `dnsAddresses = [exitIp]` ⇒ 走 `:53`/L3） | `baseline/homeway/clientcore/facade/socksmgr.go:32/359`；`tier:.../TierVpnExtensionAbility.ets:1855`；tier 全仓 `.ets` 对 5300 零命中 |
| 4 | 客户端解析器已按 RFC1035 TCP 分帧（**2B BE 长度前缀**）读写，与出口 `:5300` TCP 面同帧 ⇒ 承载换轨**不需要改 dnsq/socks 任何协议代码** | `daemon/carriers/dnsq.rs:3/117-140`（`resolve_over_conn`） |
| 5 | 出口服务面有现成统一通道：`serve_via_intake`（socketpair + 入队 + 泵 + 受理计数/拒入码），intake 两源（UDS + QUIC 队列）已服务 files/term/speedtest | `exit/serve.rs:95/104-167`；`exit/intake.rs:116-176` |
| 6 | dnsproxy 有同步等应答面 `answer_sync`（自检/测试用；**不过在途门、不计 qtcp、`is_tcp=false`**）；`in_flight` 由 worker 应答后自减 | `dnsproxy.rs:461/611/510-534` |
| 7 | 引擎装配点持有 `Arc<DnsProxy>` 与 `service_intakes`；DNS 仅在 `dns_enabled` 时创建 | `engine.rs:378-421`、`:454`、`:658` |

**缺口定性**：出口能力完好（dnsproxy/`:5300` 均在），断的是「宿主会话把查询送到出口」的那条承载
——`STREAM[tag=6]` 正是把服务腿重新收进 QUIC 档的线值面。

## 二、方案（四件 + 收口）

### A. 线值：`StreamTag::DnsResolve`（byte 6，文本 `dns`）

- `stream.rs`：枚举成员 + `ALL`（顺序 = 线值序）+ `as_byte`/`from_byte`/`text`（穷尽匹配，
  漏改即编译红）；pin 测试 `tag_bytes_are_the_designed_single_source` 同步（非法集抽样
  `6 → 7`）。
- 语义：**客户端读/写 = DNS-over-TCP 裸分帧**（2B BE 长度 + 报文），出口逐帧代答；
  一条流可跑多帧（帧间串行），客户端既有形态 = 每查询一条新流。

### B. 出口受理：tag=6 并入 `serve_via_intake`

- `exit/intake.rs`：`ServiceIntakes` 加 `dns: Option<ServiceIntakeTx>` 槽 + `slot()` 臂 +
  `any()` 计入。
- `exit/serve.rs`：`StreamTag::DnsResolve` 与 1/2/3 同 arm（`serve_via_intake`）——**零新泵代码**；
  未装配 ⇒ 既有 `reset(0x22)` + 受理计数即可观测。

### C. 出口服务体：新模块 `crates/homeway-core/src/server/dns_stream.rs`

- `pub fn serve(intake: ServiceIntake, dns: Arc<DnsProxy>, stop: Arc<AtomicBool>, logf: Logf) -> io::Result<()>`：
  - accept 循环（`ServiceIntake::accept()`，既有 poll/停止位节拍）→ **每连接一线程**
    （名字 `hw-dns-stream`、栈 256 KiB——与 Q-F-B pf / term 先例同款）；
  - handler：读 2B 长度（读期限 `READ_BUDGET = 10s`）→ `len == 0` ⇒ 收线（镜像 Go「空 TCP
    消息」判异常）→ 读正文 → `dns.answer_stream(&body)` → `Some(resp)` ⇒ 写 `2B+resp`
    （写期限 5s）；`None`（在途门/队列丢弃）⇒ **不回帧**（与栈内 TCP 面同形：客户端按 5s
    预算超时归因，不缓存）；
  - EOF/读写错 ⇒ handler 收尾（socket 关 ⇒ 出口泵 FIN）。
- `pub const MAX_CONNS: usize = 64;`（对齐 `dnsface::MAX_TCP_CONNS`）+ 容量走
  `tuning::service_defaults::intake_capacity(MAX_CONNS)`。

### D. 代答门面：`dnsproxy::answer_stream`（新公开方法，不改既有面）

```rust
/// 流服务面（STREAM[tag=6]）：在途门 + qtcp 计数 + 同步等应答（≤ budget+1s）。
/// 与 submit 同门（dropped 记账）、与 answer_sync 同等待形态（一次性通道）。
pub fn answer_stream(&self, query: &[u8]) -> Option<Vec<u8>>
```

- 语义 = «`submit` 的闸 + `answer_sync` 的同步等»：过门 ⇒ `qtcp` +1（CA5 的「计数增长」面）
  → `try_send` → `recv_timeout(budget+1s)`；门拒/入队失败 ⇒ `dropped` +1 且返回 `None`。
- `in_flight` 由 worker 应答后自减（既有语义）⇒ 超时不双扣。
- **`answer_sync` 零改动**（自检面 `is_tcp=false` 口径保持）。

### E. 客户端映射

- `bridge_host::port` 加 `DNS_RESOLVE: u16 = 5300`（与 FILES/TERM/SPEEDTEST 同处，注明
  「clientcore 写死 5300」）；`daemon/carriers/dnsq.rs::SOCKS_DNS_PORT` 改为同源引用（单一常量）。
- `facade/quic_stream.rs::tag_for_port`：`5300 => Some(StreamTag::DnsResolve)`；
  注释与 `ServicePort`/`NoService` 文档同步（5300 不再是「无 tag」例）。
- 装配（engine.rs）：`dns_enabled` 分支内 `ServiceIntake::quic_only(intake_capacity(MAX_CONNS))`
  ⇒ 填 `service_intakes.dns` + 起服务线程（stop 位入既有 `stop_flags`）+ 新日志行
  （**非判据行**，additive）：`dns 流腿就绪：STREAM tag=6（虚拟端口 5300；代答复用 :53 同一上游）`。

### F. 文案/注释收口（随批）

- `facade/host_session.rs` 模块头 G7 段：退役描述 → 承接事实（tag=6 承载；`NoService` 仍留
  给未知端口）。
- `daemon/carriers/mod.rs::DialErr::NoService` 注释与 `dnsq.rs` 头注（「拨 5300」→「走
  STREAM[tag=6]（虚拟端口 5300）」）。
- `carriers_cli.rs` 的 socks 提示/帮助（「域名经 … 出口远程解析」）**零改动**——承接后恢复为
  真陈述。

## 三、边界与错误面（逐条定死）

| # | 面 | 行为 |
|---|---|---|
| 1 | DNS 未启用 / intake 建不起来 | 装配面记行 + `service_intakes.dns` 为空 ⇒ 客户端收 `reset(0x22)`（既有 `SERVICE_DISABLED` 语义；「保守选择」N16 同款） |
| 2 | 在途门超限 / worker 队列满 | `dropped` +1、**不回帧**；客户端 5s 预算超时（DnsErr::Io 归因），socks 回 rep=0x04，日志含「解析腿拨号（出口 5300）失败」原文不变 |
| 3 | 帧长 0 | 收线（镜像 `dnsface` 的 Go 同款判异常）；长度域 u16 天然封顶 |
| 4 | 客户端半途消失/半关闭 | 读 EOF/错 ⇒ handler 收尾；泵侧 FIN 收口（既有语义） |
| 5 | 并发额度 | intake 容量 64+K；每连接一线程、栈 256 KiB ⇒ 最坏 ≈16 MiB（与 `dnsface` ≤+8 MiB 同阶，批记录登记） |
| 6 | 停止 | `stop` 位入 `stop_flags` ⇒ 引擎停 = intake 收线 + 线程退（既有节拍） |

## 四、判据与登记

- **CA5 恢复**：`curl --socks5-hostname 127.0.0.1:<port> http://example.com/` → `HTTP 200`；
  机制行文改述为「DNS-over-TCP → **STREAM[tag=6]（dns-resolve）** → 出口代答 → 应答候选按序拨；
  出口 `dns: … qtcp=N` 计数增长」。
- **E4 行文零改**（出口侧 `resolve=…:5300（TCP）` 监听与代答照旧；栈内监听面对 L3 客户端仍服务）。
- **E5 行文零改**（`DNS 代答腿 :53/:5300` 属出口面事实）。
- **判据变更记录新条**（五字段）：CA5（降级 → 恢复）+ 新增 `STREAM[tag=6]`（wire 增项，additive；
  按「无兼容包袱」口径不设旧形态兼容面）。
- `tools/check-vocab.sh` / WG 残留门 / 词表台账：tag 不在词表面内（预检后定；若命中则随批登记）。

## 五、测试计划

1. **quic 单测**：tag pin 测试（+ byte 6/text/非法集）。
2. **core 单测（dnsproxy）**：`answer_stream` —— ①正常应答逐字节（fake 上游）+ `qtcp` +1；
   ②`max_in_flight=1` 下第二并发 ⇒ `None` + `dropped` +1；③超时路径（worker 不答 ⇒ budget 上界）。
3. **core 单测（dns_stream）**：①`quic_only` intake + 自塞 socketpair ⇒ 写帧/读回帧逐字节
   （fake 上游）；②`len=0` ⇒ 收线；③多帧串行；④`stop` ⇒ 服务退。
4. **e2e（hermetic，`crates/homeway-core/tests/`）**：本地出口（`dns_upstream` 指本机 fake UDP
   上游）⇒ 客户端会话 ⇒ `host_session.dial_port(host, 5300)`（或 CLI 缝）⇒ 帧交换 ⇒ 应答
   逐字节 = fake 上游；出口快照 `qtcp` 增长。**这钉死 CA5 的机制面**（socks 层既有注入缝单测
   覆盖 rep 映射，不重做）。
5. **门禁**：`cargo test --workspace` + `clippy --all-targets -D warnings` + 三目标交叉 check +
   `check-quic-isolation.sh` + `check-wg-removed.sh` + `check-vocab.sh`；`ci-local` 视时长跑。

## 六、非目标（本批不做）

- 不做本机解析兜底（M5 设计选项②字面形态）——语义代价（fake-ip/geo 拿错地址 + 泄漏）已论证。
- 不恢复 `dnstest` 动词（G6 退役维持）。
- 不动栈内 `:5300` listener（L3 客户端面照旧）。
- App/tier 侧零代码（App 走 `:53`，不经本腿）。

---

## 七、设计门（dsh r33，2026-10-12）处置记录

**评审结论**：方案骨架（复用 `serve_via_intake` + 新增 tag=6 + `answer_stream`）**成立且确实是最小
改动**，四件套划分正确；**有条件通过** —— 1 高 + 6 中须在落地前定死。评审独立回源核实了 §一 的
7 条事实与全部关键接口（原文 = `/tmp/dsh-review/r33.5dI5vp/output.md`）。

**逐条处置**（认同 = 已改设计/实现 + 测试覆盖）：

| # | 级别 | 意见 | 处置 |
|---|---|---|---|
| 2-1 | **高** | 在册闸没真落地：`ServiceIntake` 容量只约束队列条数（取走即释放）⇒ 设计稿的 ≈16 MiB 内存账不成立 | **认同**：`dns_stream` 自带 `MAX_CONNS=64` 原子闸（RAII 守卫，超限**直接关流**）；内存账重记 = 栈 256 KiB + socketpair 64 KiB×2 向 + 泵 8 KiB ≈ **448 KiB/连接 ⇒ 最坏 ≈28 MiB**（模块头留痕）；`serve_with_cap` 可注入面 + 用例「第 cap+1 条 ⇒ EOF」；intake 容量不变量「对本服务不适用」写进 intake.rs 注释 |
| 2-2 | 中 | 帧读期限 per-syscall ⇒ 滴流半帧无限续命 | **认同**：`read_exact_deadline` 逐 syscall 收敛到**每帧绝对预算**（`FRAME_BUDGET=10s`，同 `dnsproxy` 的 `DeadlineIo` 口径） |
| 2-3 | 中 | 应答长度域 > u16::MAX 无处置（静默截断 = 帧语义坏） | **认同**：`u16::try_from` 失败 ⇒ **收线** + 记行（镜像 `dnsface::deliver_tcp`） |
| 2-4 | 中 | 未装配（0x22）在客户端退化成「连接被拒（对端 RST）」（M5 H-2 修过的误导复现）；`DialErr::NoService` 失去唯一生产构造点 | **认同（选项 a）**：`HostErr::ServiceDisabled` 新变体（`StreamErr::NotSupported` 专收）+ `DialErr::ServiceDisabled(port)`（带端口可行动文案）；`speedtest`/`speedrun`/`host_reach` 同族分支同步；`NoService` 保留给真正无 tag 的端口；可达性变化记入变更记录 |
| 2-5 | 低 | tag=6 可用性 = `dns_enabled`（与 `dns_port` 取值无关）须写明 | **认同**：装配行改「客户端虚拟端口 5300；代答复用 :53 同一上游 dnsproxy」+ engine 注释写死口径 |
| 2-6 | 低 | drop 路径的客户端日志是「解析超时」而非「拨号失败」 | **认同**：记录/论证两分支分开写（本文 §三-2 的口径按此理解；日志原文 = `dnsq` 的 `解析超时（预算 5s）`） |
| 2-7 | 低 | 停止语义措辞（「intake 收线」不准确） | **认同**：改为「accept 循环下一拍退（≤200ms）；在途 handler 随泵/FIN 收尾、不 join」 |
| 2-8 | 低 | 畸形（len<12）不回帧须写进帧异常面 | **认同**：模块头与用例覆盖（畸形计 `qtcp`+`malformed` 且不回帧） |
| 3-1 | — | `in_flight` 四路径闭合（看过无问题）+ 建议抽单源 + worker panic 残余登记 | **认同**：抽 `admit_and_send`（`submit`/`submit_leg_impl`/`answer_stream` 三调用点单源）；worker panic 槽泄漏为**既有**残余（登记在记录） |
| 3-2 | 中 | `qtcp` 自增时机应与 `submit_tcp` 一致（**收到即计**，门之前） | **认同**：`answer_stream` 自增在门之前；计数输入集表新增 E22 `qtcp` 行（含畸形计/`len=0` 不计/丢弃计的口径） |
| 3-3 | 中 | `is_tcp=true` 须写死（否则大应答被静默截断，CA5 的小 A 查询测不出） | **认同**：`answer_stream` 写死 `is_tcp: true`；用例「100 条 A（≈1.6KB）不截断 + `trunc=0`」 |
| 3-4 | — | 与 `dnsq` 帧兼容（看过无问题） | 无动作 |
| 3-5 | 低 | 补 `tag_for_port(5300)` 正向 pin + 常量同源 pin | **认同**：`virtual_ports_map_to_their_stream_tags` 加两断言（含 `SOCKS_DNS_PORT == bport::DNS_RESOLVE`） |
| 3-6 | 低 | 写一句「应答 ≥12B 由 dnsproxy 保证」 | **认同**：模块头帧异常面已含该口径 |
| 4-1 | 中 | 登记漏项：E-q5/C19 值域、E22 `qtcp`、服务流计数、M5 批④ 作废、CA5 降级注删除、新行归属、NoService 可达性 | **认同**：全部落「判据变更记录」2026-10-12 条（含影响面逐项）+ 计数输入集表新行 + M5 批④ 状态注 + CA5 行恢复；新行**明写 additive 非判据行、不立 ID**（`check-wg-removed.sh` ⑩ 零改） |
| 4-2 | — | E4/E5 零改正确 + 「`resolve=` 腿在宿主会话面零消费者」仍为真 | **认同**：保留该真陈述（承接走 tag=6，不经栈内 `:5300`） |
| 4-3 | 中 | 工作树已有「已落地」措辞但代码未动 ⇒ 拆 commit、措辞与实现同批 | **认同**：按「每工作单元一 commit」拆（flake / 内存门槛补登 / 文档指针 / DNS-STREAM 四笔），DNS-STREAM 笔含实现 + 判据 + 措辞 |
| 4-4 | 低 | 门禁预检 | **认同**：词表「不含 StreamTag 文本」已核（`check-vocab.sh` PASS）；隔离门 11/11、WG 残留门 10/10 复跑绿 |
| 5-1 | 中 | CA5 的 e2e 形态不足；`socksmgr::resolver_for` 零覆盖 | **部分认同/已办**：(a) **必做**已做 —— `resolver_for` 全链用例（FakeDial 5300 → 假解析腿 → 帧问答 → 候选 + 缓存 + 否定不缓存 + 每查询新连接）；(b) **推荐**未做（字面 `curl --socks5-hostname` 全链 e2e 需 host-session 客户端 harness/daemon 栈）⇒ 按 (c) 显式登记证据锚 = 机制面带（本机三条进程内测试 + 出口侧 tag=6 分派用例）+ 人工 `curl` 轮（见 `DNS-STREAM.md`「验收与残余」） |
| 5-2 | 低-中 | e2e 的 `qtcp` 断言要「拍边界基线 + Δ」；`qtcp` 不在 `serve status` 快照 | **认同（并入记录口径）**：本批机制面不断言快照 `qtcp`（用 `stats_line()` 进程内读数）；真机/脚本轮按既有 e2e 的拍边界口径 |
| 5-3 | 低 | fake 上游必须回显 ID + question（且 TTL ≤60） | **认同**：本批全部 fake 上游按此构造（否则 `exchange` 校验不过） |
| 5-4 | 低 | `answer_stream` 超时路径几乎不可构造 ⇒ 别写「不可构造但断言它」 | **认同**：测试改为「门拒 ⇒ `None` + `dropped`」+「全死上游 ⇒ SERVFAIL 应答（非 None）」——超时面按常量口径钉（`recv_timeout = budget+1s`） |
| 5-5 | 低 | 补 pin 清单 | **认同**：tag pin / 常量同源 pin / 大应答不截断 / 在册闸 / `len=0` 不计 `qtcp` / 未装配 0x22（出口侧用例）逐条落地 |
| 5-6 | 低 | 文档/真机面：设备手册的 DNS 行只覆盖 `:53`；本批恢复的是 daemon socks 面 | **认同**：登记证据锚（机制面 + 人工轮），不在本批改设备手册（daemon socks 非真机档主线） |
| 1-2/1-3 | 低 | 先例措辞（term 无 256KiB 栈）/ `any()` 无消费者 | **认同**：改措辞为「照 `PF_THREAD_STACK` 先例」并注明 term 用默认栈；`any()` 注释写明「当前无消费者」 |
