# DNS-STREAM 承接批 · 批记录（2026-10-12）

> **任务书**：用户 2026-10-12 指令「做了」+「其它事情都按你推荐的来改，tier 里的内容也都改了」。
> 本批 = M5 遗留待裁决项 **§12-C-8（`:5300` 解析腿：承接 vs 退役）** 按**承接**执行：
> 新增 `STREAM[tag=6]`（dns-resolve），恢复宿主侧 socks 域名目标的远程解析（判据行 CA5）。
>
> **两门记录**：设计门 = dsh `r33.5dI5vp`（exit=0，**有条件通过**：1 高 + 6 中 → 逐条处置见
> `DNS-STREAM-design.md` §七）；代码门 = dsh `r34.41p3LA`（exit=0，**无高危**：3 中 + 10 低 →
> 处置表见本文 §四）。两门原文 = `/tmp/dsh-review/r{33.5dI5vp,34.41p3LA}/output.md`。

## 一、实现（提交 = `21e368e`，主体）

四件 + 收口（细节与逐条依据 = `DNS-STREAM-design.md` §二/§三/§七）：

| 件 | 落点 |
|---|---|
| 线值 | `StreamTag::DnsResolve`（byte 6，文本 `dns`）+ `ALL`/`as_byte`/`from_byte`/`text` + pin 测试（非法集抽样 6→7） |
| 出口受理 | `exit/serve.rs` 的 tag=6 并入 `serve_via_intake`（socketpair + 入队 + 泵，零新泵代码）；`exit/intake.rs` 加 `dns` 槽 + `slot()`/`any()`（`any` 注明当前无消费者） |
| 出口服务体 | 新增 `crates/homeway-core/src/server/dns_stream.rs`：每连接一线程（栈 256 KiB）；**真在册闸** `MAX_CONNS=64`（RAII，超限直接关流 ⇒ 客户端立刻 EOF）；**每帧绝对读期限**（逐 syscall 收敛）；帧面 = RFC1035 2B 长度前缀（`len=0` 收线且不计 `qtcp`；畸形交 dnsproxy 不回帧；应答超 `u16::MAX` 收线） |
| 代答门面 | `dnsproxy::answer_stream`（同步等应答）+ `admit_and_send` 单源（三调用点共享在途门不变量）；`qtcp` 收到即计（门之前）；`is_tcp=true` 写死（大应答不按 1232 截断） |
| 客户端映射 | `bridge_host::port::DNS_RESOLVE=5300`（单源）+ `tag_for_port(5300) → DnsResolve` + `dnsq::SOCKS_DNS_PORT` 同源引用 + 两枚 pin |
| 归因分档 | `HostErr::ServiceDisabled`（`StreamErr::NotSupported` 专收）+ `DialErr::ServiceDisabled(port)`（带端口可行动文案）；`speedtest`/`speedrun` 同族分支同步（`host_reach` 面无该分支——代码门 L9 订正）；`NoService` 保留给真正无 tag 的端口 |
| 装配 | `engine.rs` 的 `dns_intake_of`（可测助手）+ 新行 `dns 流腿就绪：STREAM tag=6（客户端虚拟端口 5300；代答复用 :53 同一上游 dnsproxy）`（**additive 非判据行、不立 ID**）；可用性 = `dns_enabled`（与 `dns_port` 取值无关） |

同批独立小笔：`2cabc5e`（CI flake 断言改有界等待 + 台账销账）、`21f85b1`（内存门槛 ≤+640K 补登）、
`3a6ae28`（文档指针收口：ROADMAP/QUIC-ROADMAP/GAP-AUDIT）。

## 二、判据与登记

- **CA5 恢复**（机制面改述为 `STREAM[tag=6]`）+「判据变更记录」2026-10-12 条（五字段，覆盖
  E-q5/C19 的 `tag=%s` 值域、E22 `qtcp` 输入集、服务流计数五键、`DialErr` 分档、M5 批④ 作废注）
  +「计数输入集」2026-10-12 行；**E4/E5 行文零改**；M5 批④ 保留的真半句（栈内 `:5300` 在宿主
  会话面仍零消费者——daemon 走 tag=6，不经它）**不删**。
- 与 tier 的交界：**App 零代码**（App 走 `:53`/L3，不经本腿）。

## 三、证据

**测试**（每条都做过「实现回退会不会变红」核对，逐条见两门记录）：

| 判据面 | 用例 |
|---|---|
| 线值/tag 表（含文本 + 非法集） | `homeway-quic/src/stream.rs::tests::tag_bytes_are_the_designed_single_source` |
| 出口分派两态（已装配逐字往返 + 受理行 `tag=dns`；未装配 `reset(0x22)` + 拒绝行） | `homeway-quic/src/exit/tests.rs::dns_stream_tag6_rides_intake_and_refuses_when_unassembled` |
| 帧往返 + 大应答不截断 + `trunc=0` + `q=0` | `server::dns_stream::tests::stream_serves_frames_closes_on_len0_and_keeps_big_answers` |
| `len=0` 收线且不计 `qtcp` / 畸形计 `qtcp`+`malformed` 不回帧 / 在册闸第 cap+1 条 EOF | 同上 + `stream_live_gate_closes_extra_connections` |
| `answer_stream`：`qtcp` 收到即计、门拒 `None`+`dropped`、全死上游 SERVFAIL、**门饱和排队让路**（不计 `drop`） | `server::dnsproxy::tests::answer_stream_counts_qtcp_before_gate_and_gates` |
| `resolver_for` 全链（拨 5300 → 帧问答 → 候选/缓存/否定不缓存/每查询新连接） | `daemon::carriers::socksmgr::tests::resolver_for_dials_resolve_leg_frames_and_caches` |
| 装配两态 + 就绪行（M2③） | `server::engine::tests::dns_intake_assembles_only_when_dns_is_enabled` |
| 归因分档（io kind 同档 + `0x22`/`0x25` typed 分档 + `dial_port` 换型带端口） | `facade::host_session` 的 `host_err_maps_to_io_kinds_in_one_place`（扩）+ `daemon::tests::dial_port_error_attribution_splits_disabled_from_refused` |

**门禁读数（本批终态）**：`cargo test --workspace` 全绿（core lib 681 / quic lib 222 + 其余目标）；
`cargo clippy --workspace --all-targets` 零告警；隔离门 **11/11**；WG 残留门 **10/10**；
词表门 **PASS**；OHOS/musl 交叉 check 走 `tools/ci-local.sh` 的垫片配方（**未在本机手跑成功**：
裸 `cargo check --target …` 缺交叉工具链——如实登记，见 §五-3）。

## 四、代码门（r34）逐条处置

| # | 级别 | 意见 | 处置 |
|---|---|---|---|
| M1 | 中 | `answer_stream` 的「超时只在残余面」不成立：门饱和时排队等待最坏 ≈7.5s > `budget+1s` ⇒ 健康查询也可能被让路（与栈内 TCP 面分叉） | **认同**：docstring 改为如实口径（负载下主动让路 + 与栈内面分叉 + 重试同族）；**补用例**钉死语义（`drop=0` 且返 `None`）。**首版用例曾以「全死上游 + 四条排队」靠墙钟凑等待 ⇒ 公开 CI（macos-latest）假红**（dead 上游的 job 耗时随平台漂移 ⇒ 探针反而拿到应答）⇒ 改为 **`#[cfg(test)] TEST_JOB_HOLD_MS` 注入口驱动**（worker 取到 job 先睡 3s，探针 `budget+1s=1.3s` 恒超时；断言确定性、不依赖上游墙钟行为）。本记录与计数输入集行同步登记 |
| M2 | 中 | ① `DNS-STREAM.md` 在 HEAD 不存在而三处引用它；②「本机 e2e」措辞夸大（实为进程内单测）；③ 装配面零自动断言（删 `service_intakes.dns = Some(tx)` 全仓不变红） | **认同全三条**：① 本文件即该记录（随本笔落库）；② 判据登记的「验收证据锚」措辞订正为「进程内机制面单测 + 出口侧分派用例 + 装配断言 + 人工 curl 轮」；③ 抽 `dns_intake_of` 助手 + 装配两态用例（`server::engine`） |
| M3 | 中 | 中间 commit `2cabc5e`/`21f85b1` 在 `--tests` 下编译红（拆笔时把 DNS 用例的 5 行 `///` 注释留在了前一笔 ⇒ 文件以 doc 注释结尾），且其提交信息称「全量绿」——验证是在含下一笔内容的工作树上做的 | **记录在案**（HEAD 绿、无功能影响；不改写已推前历史）：教训 = 拆笔后须**逐笔** `cargo check --tests`；后续拆笔用 `git add -p` 后再验，不靠工作树全量绿推断中间笔 |
| L1 | 低 | 「承接后宿主会话面 `0x22` 的唯一生产来源 = `dns_port=0`」过强（files/speedtest 未装配与 `HOMEWAY_TERM=off` 同产 `0x22`） | 认同：改「**dns 腿**的 `0x22` 来源 = `dns_port=0`；其余服务未装配同产 `0x22`，本批同档归因」 |
| L2 | 低 | 计数输入集三处不精确（漏 tag=5；「四键」实列五键；`qtcp` 输入集漏拦截层 TCP 腿） | 认同：逐项订正（①③ 属既有面补全，非本批引入） |
| L3 | 低 | `dns_stream` 内存账算术不自洽（256+64×2+8 = 392 KiB ≠ ≈448 KiB；且与仓内「按设置值记」口径冲突） | 认同：按 `exit/pump.rs` 口径重算 = 栈 256 + socketpair **4 面装填值** 256 + 泵 8 ⇒ **≈520 KiB/连接 ⇒ 最坏 ≈33 MiB**（模块头写死算式） |
| L4 | 低 | `quic_island_e2e.rs` 的「宿主会话面对该端口已退役」在承接后失实 | 认同：注释改为「本机轮不覆盖栈内 `:5300`；宿主会话面消费已改经 `STREAM[tag=6]`」 |
| L5 | 低 | 新增归因面缺断言（`ServiceDisabled → ConnectionRefused`；`0x22 → DialErr::ServiceDisabled(port)` 换型） | 认同：两条各补断言（io kind 测试扩 + 抽 `map_dial_port_err(e, port)` 助手 + 用例） |
| L6 | 低 | spawn 失败日志无节流（fd/内存耗尽期正是放大期） | 认同：加「首 3 + 每 100」节流（与同文件在册满日志同款） |
| L7 | 低 | 设计稿 3-6 处置称「模块头已含（应答 ≥12B 由 dnsproxy 保证）」但文件无此句 | 认同：模块头补该句（语义成立：过 `qtype` 的查询必得 ≥12B 应答或 SERVFAIL） |
| L8 | 低 | 4-3 的「措辞与实现同批」未完全做到：文档指针笔早于实现笔就写「已落地」并指向当时不存在的记录文件 | **记录在案**（历史不改）：教训 = 「已落地」类措辞一律压到实现笔；本笔已把该记录落库、指针不再悬空 |
| L9 | 低 | 「`host_reach` 同族分支同步」——该分支**不存在**（`daemon host reach` 走 token 端点裸 UDP 探测，零 `HostErr` 分支） | 认同：设计稿该词订正为「已核：无该分支，无需同步」 |
| L10 | 低 | 实现提交信息称「quic **221**」，实测 222（221 是加本批用例前） | 认同：本记录订正（不影响结论） |

## 五、验收与残余（如实）

1. **CA5 的验收方式**：机制面板 = 上表八条用例（含 `resolver_for` 全链 = 拨 5300 → 帧问答 →
   候选，即 CA5 的客户端机制面）+ 出口侧 tag=6 分派两态 + 装配断言；**字面全链 e2e**
   （`curl --socks5-hostname` 走真 daemon `socks` 载体）**未落**——需 host-session 客户端
   harness + daemon 栈（设计门 5-1(b) 属「推荐」，本批按 (c) 显式登记锚点）。人工轮 =
   起本地出口 + `daemon host add` + `socks on` + `curl --socks5-hostname`（本地形态，可随时补跑）。
2. **负载下的让路语义**（代码门 M1）：门饱和（在途 256 + 64 worker）时流腿查询可能等待超时
   ⇒ 不回帧 ⇒ 客户端按自身 5s 预算超时、rep=0x04、不缓存（重试语义同族）。**不改上界**的理由：
   把等待上界抬到含排队项（最坏 ≈7.5s）会让 64 条 handler 线程各挂更久，负载下更差；栈内 TCP
   面的「晚到也送达」是其异步回投结构使然，两面的这一分叉已登记。
3. **交叉 check 未在本机手跑成功**：裸 `cargo check --target aarch64-unknown-linux-ohos` /
   `x86_64-unknown-linux-musl` 缺交叉工具链（NDK clang 垫片 / musl gcc 配方）⇒ 按仓内惯例走
   `tools/ci-local.sh`（其第 3–5 步做交叉面）。CI（GitHub）会覆盖四目标 —— 本批推送后以 CI 为准。
4. **未做（非本批范围）**：真机/设备手册不加 daemon-socks 步骤（非真机主线）；`dnstest` 维持退役
   （G6）；栈内 `:5300` 监听面照旧（L3 客户端面仍可达）。

## 六、后续可选（登记不阻塞）

- 字面 CA5 e2e（daemon socks + curl）落地 ⇒ 可把「人工轮」升级为常驻 e2e；
- `engine.rs` 的 DNS 装配若再增服务，`dns_intake_of` 模式可推广（端口→助手→两态用例）。
