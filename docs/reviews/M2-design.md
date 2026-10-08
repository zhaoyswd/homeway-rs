# M2「身份、设备表与准入」设计文档

> 批次：WG → QUIC 传输层换代程序 **M2**（真源 `docs/QUIC-ROADMAP.md` 的「M2 身份、设备表与准入」节
> + 该节顶部的**范围调整注记**：出口身份半边已前移到 M1）。
> 第 1 棒（设计）产出。**本棒不写产品代码**（实现是后续棒）——本文件是实现的输入契约。
> 基线：工作树 `/Users/zhaozhe/Documents/projects/homeway-rs-quic`（分支 `quic`），
> `HEAD = 4715939`，开工 `git status --porcelain` = 空（复验见 §0.1）。**行号均为本 HEAD 实测值**，
> 实现以符号定位为准（M0/M1 先例：行号会漂）。
> **范围边界**：本批只做 ①`hr-reg4` 三连准入（Hello/Challenge/Proof）②设备表语义对照与
> 「连接 = 设备」收口 ③抗放大（Retry）与握手限流 ④token 格式候选与推荐 ⑤威胁模型。
> **不做**：M3 的服务流迁移（STREAM 分发 / stackb 退役）、M4 的 portfwd 换轨、M5 的 WG 删除、
> 任何中继代码改动（红线）、任何 intercept 改动。
> 主检出 `~/Documents/projects/homeway-rs`、`~/Documents/projects/homeway`、`~/Documents/projects/tier`、
> `baseline/` 全程只读。
> **状态**：v2（设计门**已过** @ `/tmp/dsh-review/r14.JxDBI4/`，2026-10-09——exit 0；意见 **30 条**
> 〔高 5 全部"建议阻塞" / 中 13 / 低 12〕，**认同 30 / 部分认同 0 / 不认同 0**；5 条高危已全部落到 v2，
> token 拍板项改为**带前置条件**〔§4.2.1〕。结论、原文摘要、逐条处置与残余见 **§11**）。

---

## 0. 复验（证据先行）

### 0.1 工作树与既有事实（**引用而非重测**）

| 项 | 实测 / 出处 |
|---|---|
| 工作树 | 分支 `quic`，`HEAD 4715939`；**开工时 `git status --porcelain` = 空**，收口时 = 本棒的两份未跟踪产物（`docs/DEVICE-TEST-OHOS.md` + `docs/reviews/M2-design.md`），**无其他改动**（复验见 §11.0 末条） |
| M1 收口读数 | `cargo test --workspace` 全绿（`homeway-core --lib` 688 passed / 4 ignored；`homeway-quic` 76 passed）；clippy 0；`docs/reviews/M1.md` §5 |
| M1 门槛 | 10 格通过 / 1 格未过（产品形态单连接内存 +496K/+608K vs ≤+320K，**已上报待裁决**）/ 1 格按设计属 M5 判（体积）——`docs/reviews/M1.md` §4.2 |
| 真机 | 层 0 全通 + OHOS 运行期已验（2026-10-09，`FMR0224116011480`）；设备现装**带 QUIC 核的 App**（本棒只读侦察复核：bundle 在位、沙箱日志 `tailcat-tun.log` 4,244,248 B）；读数 `/tmp/m1dev-res/SUMMARY.txt` |
| M2 起点 | **M2 不得破坏上述读数**：`_wg` 档逐字节回退（token/C 族原串/`quic:` 族零输出）与三道门槛（每包 CPU / 线开销 / 内存）在 M2 后仍须成立 |

### 0.2 源码接缝重定位（HEAD 4715939，逐条实测）

| 接缝 | 位置 | M2 用法 |
|---|---|---|
| `hr-reg3` 帧层（MAC 输入唯一真源） | `crates/homeway-quic/src/reg3.rs`（`MAGIC:28`/`LEN:31`/`MAC_LABEL:37`/`EXPORTER_LABEL:40`/`mac_of:47`/`parse:78`/`mac_matches:93`/`encode:100`） | **本批改写为 `reg4.rs`**（§1.2） |
| 出口准入落点（控制流任务） | `crates/homeway-quic/src/exit/conn.rs`（`control:92` / `reject:151` / `datagrams:168`） | Hello/Challenge/Proof 状态机 + pending（**任务内局部状态**） |
| 出口端点主循环与三闸 | `crates/homeway-quic/src/exit/mod.rs`（`DEFAULT_HANDSHAKE_CAP:77`/`DEFAULT_HANDSHAKE_DEADLINE:81`/`log_due:83`/`ExitQuicConfig:88`/`conn_cap:112`/`ExitQuicSnapshot:131`/`run_exit:479`；三闸在 `:578-614`） | Retry 决策点（`endpoint.accept()` 的 `Incoming`）+ 闸升级 + 计数扩展 |
| 引擎裁决与设备表桥 | `crates/homeway-core/src/server/engine.rs`（`on_quic_inbound:1508`/`admit_reg3:1533`/`apply_dev_ops:1573`） | 裁决面改名 `admit_reg4`（**重建 v2 报文 → `table.register` 的手法原样保留**） |
| 设备表 | `crates/homeway-core/src/server/table.rs`（`DEFAULT_MAX_DEVICES:27`/`DEFAULT_TTL:29`/`DEFAULT_GRACE:31`/`REG_WINDOW:33`/`verify_reg:51`/`RejectReason:99`/`match_reg3:229`/`device_addrs:241`/`register:295`/`gc:424`/`select_stale_victim:459`） | **语义零改动**（§2.1 逐条对照）；只换 secret 匹配入口名 |
| 客户端登记面 | `crates/homeway-quic/src/client/register.rs`（`REG_SETTLE:24`/`now_unix:27`/`exporter_of:35`/`write_frame:80`/`register_on_control_stream:113`） | 四帧流程 + 刷新帧（`REG_SETTLE` 退役，§1.7） |
| 客户端连接面 | `crates/homeway-quic/src/client/mod.rs`（`Face:57`/`open:80`/`rebind:148`/`Live:202`/`refresh_if_due:261`/`probe:289`）；`race.rs:67`（`run`；**设计门 r14 订正**：v1 写 65） | 准入状态机接线 + 预算复核 |
| 岛配置/凭据 | `crates/homeway-quic/src/config.rs`（`TokenSecret:40`/`IslandCredential:81`/`IslandConfig:123`） | 无新字段（证明协议复用 secret；§4.1） |
| token 字节层 | `crates/homeway-core/src/token.rs`（`EndpointKind:81`/`wg_endpoint_refs:118`/`TokenRef:260`/`Token:316`/`decode:344`/`parse_body:368`/`TokenSpec:483`/`encode:493`） | token 候选落点（§4） |
| 台账 / 铸造 | `crates/homeway-core/src/server/state.rs`（`issue_token:220`/`append_record:263`/`revoked_secrets:324`/`last_token:395`/`secrets:447`）；`crates/homeway-cli/src/serve_cli.rs:1031`（`serve token` 渲染） | token 变更的消费面清单（§4.4） |
| reg v1→v2 先例（帧版本纪律） | `crates/homeway-core/src/wtransport/reg.rs`（`REG_LEN:23`/`encode_reg_parts:32`） | M2 重建 v2 报文仍走它（不动） |
| 世代装配（岛凭据注入 / 预算） | `crates/homeway-core/src/facade/tun_exec.rs`（`L3Bearer:93`/`resolve_bearer:130`/`QUIC_CONNECT_BUDGET:120`/`Connect:2297`/凭据构造 `:2267-2276`） | 赛跑+准入预算复核（§1.7） |
| 隔离门（新代码的合规面） | `tools/check-quic-isolation.sh`（九条；异步面 = **显式文件清单**，新文件必须加进清单） | §0.6 合规检查清单 |

### 0.3 本棒实测（探针 `/tmp/m2-probe`，**仓外**）

> 目的：M2 的「抗放大」是路线文件点名的交付物，而 quinn 的 Retry 面在 M1 里**一行未用**——
> 本棒用最小探针把「能不能用 / 用了什么代价 / 用完之后服务端看到什么」钉死。
> 形态 = 单文件 `main.rs`（quinn 0.11.12 + rustls 0.23(ring) + tokio；自签 **leaf** 证书现场生成于
> `/tmp/m2-probe/certs/`——注意首版用 `openssl req -x509` 无 `CA:FALSE`，被 webpki 判
> `CaUsedAsEndEntity` 拒握手，**已订正**；读数 `/tmp/m2-probe` 复现命令见 §0.4）。
> 口径 = **本机 Mac mini M2 / 回环 / 单进程**，不外推为真机结论。

| # | 断言 | 实测 |
|---|---|---|
| R1 | **`Incoming::retry()` 可用**：服务端在 `accept()` 里对「地址未验证」的 incoming 发 Retry ⇒ 客户端**自动**带 token 重试并完成握手（quinn 客户端无需任何改动） | `M2_RETRY=1` 5/5 轮：`server_events=retry-sent,accept-validated`（先 Retry、再以**已验证地址**采纳） |
| R2 | **不 Retry 时地址恒未验证**：`M2_RETRY=0` 5/5 轮 `server_events=accept-unvalidated` ⇒ 服务端拿不到「源地址真实」的证据（只有 RFC 9000 §8.1 的 3× 放大限兜底） | 同上 |
| R3 | **Retry 的代价 = +1 RTT**：回环上无法分辨（`hs_ms` 两臂 1.4–5.7ms 同量级，噪声主导）⇒ **精确代价必须在真实路径（真机/M6）上量**，本棒只登记「协议上多一次往返」 | 上两行读数 |
| R4 | **`may_retry()` / `remote_address_validated()` 的语义可依赖**：带有效 token 的 incoming 不再需要 Retry（本探针按 `!validated && may_retry` 决策，5/5 轮无一次多余 Retry） | 同 R1 代码路径；API 出处 = `quinn-0.11.12/src/incoming.rs:52/85/93`（`may_retry` 在 **93**；**r14 订正**：v1 写 90） |

### 0.4 复现命令

```bash
cd /tmp/m2-probe
# certs 已在盘（leaf：CA:FALSE + SAN=localhost/127.0.0.1）；重生成：
openssl req -x509 -newkey rsa:2048 -keyout certs/key.pem -out certs/cert.pem -days 3650 -nodes \
  -subj "/CN=localhost" -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=serverAuth" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  && openssl x509 -in certs/cert.pem -outform DER -out certs/cert.der \
  && openssl pkcs8 -topk8 -nocrypt -in certs/key.pem -outform DER -out certs/key.der
cargo build --release
M2_RETRY=0 ./target/release/m2-probe 5   # 不 Retry 臂
M2_RETRY=1 ./target/release/m2-probe 5   # Retry 臂
```

### 0.5 依赖面事实（本棒源码级复核，quinn 0.11.12 / quinn-proto 0.11.19）

| 事实 | 出处 | 对 M2 的含义 |
|---|---|---|
| `Incoming::{retry, refuse, accept, ignore, remote_address_validated, may_retry}` 齐备 | `quinn-0.11.12/src/incoming.rs:27/44/52/85/93` | 抗放大可用协议原生面，**不需自研** |
| `refuse()` = 发一个 CONNECTION_REFUSED 的 Initial close；**但并非全部拒绝都到这一层**：<1200B 的 Initial 与端点饱和（`max_incoming` 缺省 1<<16）在 `Incoming` 产生**之前**就被静默丢 | `quinn-proto-0.11.19/src/endpoint.rs:731-743`（refuse）/ `:452-465`（短包与饱和静默丢）；`EndpointStats` 的 `refused_handshakes` 只计应用层 `refuse()` | 闸命中时客户端能得到失败（不挂死）、回包 ≈50B ≪ 1200B（无放大）；但「静默丢」形态存在 ⇒ §3.3 的对账口径须带此注记（**r14 F18**） |
| 服务端 anti-amplification 3× 限内建 | `quinn-proto-0.11.19/src/connection/paths.rs:159-161`（`anti_amplification_blocked`；调用点 `connection/mod.rs:590-600`） | M2 不重复实现；只做**状态与准入**面的加固 |
| Retry token：`token_key` + `retry_token_lifetime`（缺省 **15s**，可配） | `quinn-proto-0.11.19/src/config/mod.rs:213/242/283`（字段在 **213**；**r14 订正**：v1 写 212） | 与 M2 应用层 nonce（5s，§1.3）是**两层**，互不替代 |
| **⚠️ Retry token 无一次性语义**（**r14 F2 订正**）：`Token::decode` 后用无状态 AEAD 解出 `{address, orig_dst_cid, issued}`，校验只做「地址相同 + 未过 `retry_token_lifetime`」；**没有一次性记账**（`TokenLog` 只在 `TokenPayload::Validation` 分支被查 = NEW_TOKEN 面）⇒ 一次通过 Retry 的地址，**在 15s 窗口内可反复带同一 token 免 Retry 新建连接**（每次都被判 `validated=true`） | `quinn-proto-0.11.19/src/token.rs:141-170`（`Retry` 分支无 log 查询）、`:172-190`（`Validation` 分支才 `check_and_insert`）、`:70-76`（bloom 关 ⇒ `NoneTokenLog` 恒 `Err`） | ⇒ 「抗放大」的真实界 = **每源速率闸（§3.2-④）**，不是 Retry；Retry 的价值 = 提高一次性成本 + 挡住不会重放的伪造源。**M2 追加动作**：把 `retry_token_lifetime` 收到 **5s**（与 `NONCE_TTL` 同档，配置项 `retry_token_lifetime`），压缩复用窗（登记） |
| `ValidationTokenConfig::sent()` 可设，但 **bloom feature 关 ⇒ `NoneTokenLog`：任何 NEW_TOKEN 都被判无效**（`check_and_insert` 恒 `Err`），且 `sent` 缺省 0 ⇒ 服务端不发 NEW_TOKEN | `config/mod.rs:518/533` + `token.rs:70-76`；我们的 quinn 是 `default-features=false, features=["rustls-ring","runtime-tokio"]`（`Cargo.toml:59`；**r14 订正**：v1 写 66 = tokio 注释行）⇒ bloom 关 | **「先验地址、后续连接免 Retry」的捷径 M2 不用**（要开 `bloom` = 拉 `fastbloom 0.17` 新依赖 + OHOS 交叉复核）⇒ M2 取**压力触发**策略（§3.1） |
| `retry()` 的失败面：`Err(RetryError)` **内含** `Incoming`（`RetryError::into_incoming()` 是公开 API）；错误值被 drop ⇒ `Incoming` 的 `Drop` 隐式 `refuse()`（= 假拒绝） | `quinn-0.11.12/src/incoming.rs:52-60`（构造）/ `:121-128`（`into_incoming`） | 实现纪律（**r14 F19**）：Err 分支必须 `into_incoming()` 再决定 accept/refuse；或直接用「`!validated() && may_retry()` 守卫」避免进 Err |
| `EndpointStats{accepted_handshakes, outgoing_handshakes, refused_handshakes, ignored_handshakes}` 可读 | `quinn-0.11.12/src/endpoint.rs:341-350`（**字段名带 `_handshakes` 后缀**，**r14 F20 订正**） | 观测面可与自有计数对账（§3.3；口径注记见 F18） |

### 0.6 新代码必须过的门（实现期自查清单）

1. `tools/check-quic-isolation.sh` 九条：**新增的异步面文件必须加进 `ASYNC_FILES` 显式清单**（`tools/check-quic-isolation.sh:39-46`）；
   非异步面文件零 `tokio|quinn|rustls|async fn|.await`；裸 `send_datagram(` 仍只许两处包装体内；
   `relay/**` 与 `relaywire.rs` 整文件零异步名（M2 不碰中继 ⇒ 天然成立）。
2. `cargo test --workspace` 全绿 + `cargo clippy --all-targets -D warnings` 0。
3. 三目标 `cargo check`（OHOS 真链路 / musl 双架构 / darwin）。
4. `tools/check-vocab.sh` PASS（M2 不新增词表单元——**须实测确认**；若新增须同批更新 ledger）。
5. 判据行登记与代码**同批 commit**（§6）。

---

## 1. 客户端证明协议（`hr-reg4`）

### 1.1 与 M1 `hr-reg3` 的关系（**先定这一条，后面全依赖它**）

**裁决：替换，不并存。** `hr-reg3`（单帧：`mac = HMAC(secret,"hr-reg3"‖pubkey‖devTag‖ts‖exporter32)[:16]`）在 M2 被
**`hr-reg4` 四帧流程**取代；`H3` 魔数在 M2 起**被拒**（与 `H2` 在 M1 被拒同款纪律，`reg3.rs:148-182` 的版本纪律）。

逐条理由：

1. **M1 的半边已经很强，M2 加的不是密码学强度而是「状态与资源面」**（**如实登记，不夸大**）：
   `hr-reg3` 已经把「token secret 持有」+「连接绑定（TLS exporter）」两件事做进 MAC，
   **跨连接重放已经被关闭**（`reg3.rs:186-209` 的 `mac_is_bound_to_connection_and_fields` 单测）。
   Hello/Challenge/Proof 的**新增价值**在于：
   - 服端多了一个**更便宜的拒绝面**：Hello 只需 50B 解析即可判「拒发挑战 / 限流 / 不进入试秘」，
     而 `hr-reg3` 的对应拒绝发生在「已完成 QUIC 握手 + 已投引擎 + 已做逐 secret 的 HMAC 试秘」之后
     （**r14 F6 订正**：v1 写「发挑战**之前**」是错的——Hello 也发生在握手完成之后（控制流帧），
     不早于 reg3；真正变便宜的是「**MAC 试秘之前 + 不跨线程投引擎 + 不触碰设备表**」）；
   - 服端有了**自己挑的、一次性的新鲜值**（nonce）⇒ 可以在**同一条连接内**拒绝 Proof 复用，
     并给「未认证连接」一个**显式、有界、可观测的生命周期**（M2 的「未认证连接不占额度」需要一个可命名的状态）；
   - 客户端拿到**确定性回执**（`A4`），替掉 M1 的经验窗 `REG_SETTLE=400ms`（`client/register.rs:24`）。
   **安全面的准确表述（r14 复核后的收窄）**：nonce **不提供对「未持 secret 者」的新抵抗力**；
   它的唯一密码学附加值 = 「连接密钥泄露但 secret 未泄露」档的一条**纵深防线**
   （那时重放捕获的 Proof 会被一次性 nonce 挡下）。⇒ 设计与代码门都**不得**把 nonce 说成抗重放的主防线。
2. **不并存的理由**：若 `H3` 与 `H4` 都收，则「未认证面」有两套生命周期（一帧式 vs 三连式），
   抗放大/限流/观测都要各做一份 ⇒ 与用户拍板原则③（**简洁**）冲突，且把安全面翻倍。
3. **无兼容包袱的边界**：用户 2026-10-08 拍板①已明确「token 格式 / wire 协议 / 身份体系均允许破坏性变更」；
   M1 的实现仍在分支 `quic` 上（未合回 main）⇒ **不存在需要兼容的旧客户端**；真机上的 M1 核在 M2 验证时重刷。
4. **代价如实登记**：①准入多两个往返（Hello→Challenge→Proof→Accept）；②服端每连接多一份
   **有界临时状态**（pending，§1.3）；③客户端状态机从「写一帧 + 睡 400ms」变成「四帧握手 + 预算」；
   ④客户端赛跑预算（`QUIC_CONNECT_BUDGET=5s`，`tun_exec.rs:120`）必须覆盖准入两往返（§1.7）。

### 1.2 四帧逐字节（**定长切帧**；控制流 = 首条 bidi 流）

```
Hello      客户端 → 出口                                   50 B
  "H4"(2) ‖ pubkey(32) ‖ devTag(8) ‖ ts(8, BE 秒)

Challenge  出口 → 客户端                                   18 B
  "C4"(2) ‖ nonce(16)

Proof      客户端 → 出口（回显 Hello 四字段 + nonce）        82 B
  "P4"(2) ‖ pubkey(32) ‖ devTag(8) ‖ ts(8) ‖ nonce(16) ‖ mac(16)
  mac = HMAC-SHA256(secret, "hr-reg4" ‖ pubkey ‖ devTag ‖ ts ‖ nonce ‖ exporter32)[:16]

Accept     出口 → 客户端（**绑定建立之后立刻写**）             2 B
  "A4"(2)
```

- **字段宽度**：`pubkey/devTag/ts` 全程沿用既有宽度（32/8/8；`ts` = BE 秒 ⇒ 与 `wtransport/reg.rs` 的
  `reg[42..50]` 布局同源，重建 v2 报文时**逐字节可复用**）；`nonce` = 16 B（仓库既有 16 B 惯用法，
  与 MAC 截断同长；它**不是**密钥材料，是新鲜值 + 一次性令牌）。
- **帧长**：四帧全定长（50/18/82/2）⇒ 控制流按「读 2B 魔数 → 定长收全帧」切分；
  **不可解/未知魔数 ⇒ 拒绝并关连接**（M1 的纪律：`exit/conn.rs:104-108`）。
- **`ts` 的作用不变**：只喂 `table.register` 的 ±90s 窗（`table.rs:33 REG_WINDOW`，`verify_reg:51`），
  **M2 不改窗口、不改时钟偏移容忍**；`ts` 由客户端给（客户端时钟歪到窗外会被既有路径拒，归因行来自表内）。
- **`exporter32` 标签沿用 `hw-quic-reg`**（`reg3.rs:40`）：它是「连接绑定值」的域分隔标签，
  **不是帧版本号**——版本由 `H3→H4`/`hr-reg3→hr-reg4` 承载。改标签会让 M1 的 exporter 取值面多一个
  无收益的版本分支（登记理由；若实现期认为必须随版本换标签，须在设计文档补记）。
- **客户端 devTag/pubkey 的来源**：`IslandCredential`（`config.rs:81`）——`parts()` 原样，M2 不动。
- **刷新帧**（同一控制流，60s 节拍；见 §1.8）：
```
Refresh    客户端 → 出口                                   66 B
  "R4"(2) ‖ pubkey(32) ‖ devTag(8) ‖ ts(8) ‖ mac(16)
  mac = HMAC-SHA256(secret, "hr-reg4-refresh" ‖ pubkey ‖ devTag ‖ ts ‖ exporter32)[:16]
```
  **域分隔标签不同**（`hr-reg4-refresh` ≠ `hr-reg4`）⇒ 准入 Proof 与刷新帧**不可互相冒充**
  （防「用刷新帧完成准入」与「用 Proof 当刷新」两类跨域重放）。

### 1.3 nonce 生命周期、重放窗与「复用拒绝」

| 项 | 裁决 | 依据 |
|---|---|---|
| **准入总期限（r14 F3，新增，阻塞项整改）** | **连接被采纳起 `ADMIT_DEADLINE = 10s` 内未走完四帧 ⇒ `reject` + 关连接**（`tokio::time::timeout` 包住控制任务的「等首帧 + 整段状态机」） | ①`max_idle_timeout=30s` + `keep_alive_interval=10s`（`exit/transport.rs:46/50`）意味着：只要对端 ACK 服务端 PING，**连接永不过期**；②握手闸只在 `Connecting` 阶段生效（`exit/mod.rs:604-613`），pending 只在收到 Hello 后才存在，`conns.retain` 只清 `close_reason` 非空的（`:661-667`），控制任务会**永久停在 `accept_bi()`**（`exit/conn.rs:93`）⇒ 单攻击者用 64 条「握手完成但不发 Hello」的连接即可长期占满全部连接槽 |
| 生成 | 出口 `conn::control` 任务收到可解 `Hello` 后，用 `getrandom`（workspace 既有 0.2）取 **16B CSPRNG** | 零新依赖面（`getrandom` 已在 workspace deps；岛 `Cargo.toml` 加一行——**非异步栈名字**，隔离门不涉） |
| 存放 | **该控制流任务的局部变量** `pending: Option<Pending>`（`{nonce, issued_at: Instant}`）——**无共享表、无锁、无 `Arc`** | 「连接 = 设备」的未认证面天然按连接隔离；状态上界 = 连接数上界（构造性有界），无需淘汰策略 |
| 有效期 | `NONCE_TTL = 5s`（新常量，可配；同族常数：`DEFAULT_HANDSHAKE_DEADLINE=10s`、`ADMIT_DEADLINE=10s`） | 覆盖客户端「收 Challenge → 回 Proof」的往返 + 客户端侧排队；用**服务端 `Instant`** 计量 ⇒ **与客户端时钟无关**（时钟偏移容忍不需要新增面）。**r14 复核**：`NONCE_TTL` 只需覆盖 1 RTT，而客户端总预算 5s 覆盖 3 RTT ⇒ 二者不冲突（后者更紧） |
| 一次性 | Proof 到达即**先取走再校验**（`pending.take()`）；任务串行 ⇒ 无并发窗口 | 服务端侧「nonce 复用」不可能通过；即便同一连接重发 `P4` 也必然 `nonce 缺失` ⇒ 拒（并关连接） |
| 过期处置 | `reject(...)` + 记行 + **关连接**（`CONNECTION_CLOSE`），不等 `max_idle_timeout=30s` | 「未认证连接的生命周期有界」的可观测落点；三个闸的拒绝族已用同一出口（`exit/conn.rs:151`） |
| Hello 重发（**r14 F22 订正：删除**） | **一个连接只接受一个 `Hello`**；第二个 `Hello`（不论 pending 是否在）⇒ 拒 + 关连接；客户端预算到点 ⇒ 直接 `RegistrationFailed`（恢复 = 重连/重赛跑） | 控制流是 QUIC 的**可靠有序流** ⇒ Hello 不会丢，「首包丢失自愈」的理由不成立；重发也不解决「客户端预算到点」。**省掉一个状态分支 + 一个常量**（简洁原则） |
| **已绑定连接的再准入（r14 F15，新增）** | 连接一旦 `bridge.bind` 成功 ⇒ 该连接上的 `Hello`/`Proof` **一律拒**（只接受 `R4` 刷新帧）；同一连接只能对应一个 dev | 否则 `bind()`（`exit/bridge.rs:281-302`）在同一 `conn_id` 二次绑定时会覆盖 `by_conn`，而旧 dev 的 `by_dev`/`by_pub` 残留指向同一连接 ⇒ 一条连接可挂多个设备身份，「连接 = 设备」不变量破裂（判据面：未登记数据报/源校验随之漂移） |
| 跨连接重放 | 由 **exporter32 连接绑定**（M1 已落）承担；`nonce` 不承担该职责 | §1.5 职责表 |
| 重放窗（客户端 ts） | **不变**：`REG_WINDOW=90s`（表内），M2 不新增窗口常数 | 「移动时间窗」= 判据语义，改它要登记；M2 无此必要 |
| nonce 是否落盘 | **不落盘**（只在内存；§5 的威胁模型「secret 不落盘」纪律延伸到 nonce 与 pending） | 排障靠行文，不靠转储 |

### 1.4 服务端裁决顺序（**单一漏斗，定序八步**）

```
0) 连接被采纳：起 ADMIT_DEADLINE（10s）计时（§1.3）；到点未走完 ⇒ reject("准入超时") + 关连接
1) 帧可解？          否 ⇒ reject("帧格式非法") + 关连接        （不进入任何状态）
   版本判别（r14 F4）：`H2/H3` ⇒ reject("帧版本不符（H2/H3——旧核或垃圾包）")（**可辨归因**，M7 灰度期排障要用）
2) 已绑定连接？      是 ⇒ 只接受 `R4`；`H4/P4` 一律 reject("已绑定连接的再准入") + 关连接（§1.3 的 F15 行）
3) Hello 专用：      生成 nonce，写 "C4" 回程，置 pending      （不触碰设备表、不投引擎）
4) Proof 专用：
   4.1 nonce 命中本连接 pending 且在窗内？   否 ⇒ reject("nonce 缺失/过期/已消费") + 关连接
       （**一次性消费**：take() 先于任何后续判定）
   4.2 投引擎：`table.match_proof`（逐 secret 试秘）→ 重建 v2 报文 → `table.register(now)`
       （**原语义：窗口/吊销/表满/冲突/判据行/拒绝计数全走原路径**）
       回执 = [`Accepted{tunnel_ip,tun_ip}` | `Rejected{why}`]（**类型化 why**，r14 F7：MAC 试秘在引擎侧，
       出口面拿不到原因 ⇒ 必须由 verdict 携带；`Challenge` 变体**删除**——nonce/pending 全在出口面）
   4.3 `Rejected{why}` 的两类落点（**r14 F1 订正**）：
       · `why=MacMismatch` ⇒ 出口 `quic: 准入被拒（… hr-reg4 MAC 不符——含换连接重放 …）`
       · `why=EngineRejected` ⇒ 出口 `quic: 准入被拒（… 引擎裁决拒绝 …）`；
         **`ts` 超 ±90s 窗的 Proof 走这一类**（MAC 验过、窗拒）——表内同时打
         `peer: ! reject reason=no-token`（`table.rs` 的 `verify()` 把 `Expired` 归 `NoToken`，
         `:278-291`；M1 用例 `admit_reg3_still_enforces_reg_window` 已断言此形态）
5) 成功 ⇒ bridge.bind（同 devTag 后到者替换并关旧连接，`exit/bridge.rs:269`）⇒ 写 "A4"
6) Refresh 专用（§1.8）：三道前置 **①连接已绑定（帧内 pub/devTag == 绑定）②表内仍在册
   （`table.device_addrs(dev).is_some()`，r14 F25——防「刷新帧跨过淘汰把设备 resurrect」）
   ③MAC 过** ⇒ 重建 v2 → `table.register`（**成功 ⇒ 不重绑、不打 `连接采纳` 行**，r14 F11）
7) 任何拒绝都经 `reject()`（**准入拒绝的唯一出口**，`exit/conn.rs:151`）：计数 + 节流记行 + 关连接
```

- 「**未触碰设备表**」= 步骤 1/2/3/4.1 的拒绝路径（设备表 `entries`、`rej` 计数、判据行**都不动**）。
  这正是「未认证连接不占额度」的结构性落点（§2.2）。
- `table.match_proof` 的形态 = 现有 `match_reg3`（`table.rs:229`）改名 + 换 MAC 输入（**仍是「纯校验」**：
  不做时间窗、不走吊销钩子、不计数、不打行），保证 `register` 的语义与判据行逐字不变。

### 1.5 职责切分（**谁防什么**，M1 已落的防线不重造）

| 防线 | 载体（期） | 防什么 | **不防什么** |
|---|---|---|---|
| 出口 RPK 钉定 | TLS1.3 + Ed25519 RPK（M1，`exit/rpk.rs:79` `client_pin`） | 假出口 / MITM（客户端侧身份验证） | 未授权**客户端** |
| token secret 持有 | `hr-reg4` MAC（M2） | 未持有 token 的设备接入 | 合法 token 泄露后的滥用（§5-1） |
| **TLS exporter 连接绑定** | `export_keying_material(b"hw-quic-reg")` 入 MAC（**M1 已落**，M2 保留） | 被动窃听者把捕获的帧**换连接重放** | 同连接内重放（由 nonce 覆盖） |
| **服务端 nonce** | 控制流 `C4`（M2 新增） | 同连接内 Proof 复用（一次性）；**未认证状态的显式生命周期**；给「首帧之后、MAC 试秘之前」一个资源决策点（**r14 F6 订正措辞**） | **不提供新的密码学强度**（重复强调，防夸大）；唯一加分 = 「连接密钥泄露但 secret 未泄露」档的纵深 |
| 设备表语义 | `table.rs`（M1 未动，M2 仍未动） | 表满 / 地址冲突 / 吊销 / TTL / 轮换 | 洪泛（§3 的面） |
| 资源闸 | Q-O 三闸（M1）+ 每源闸 + pending TTL（M2） | 连接/握手/未认证状态的无界堆积 | 分布式（多源）洪泛（§5 残余） |

### 1.6 失败面与归因（判据行，additive 为主）

| 面 | 行 | 说明 |
|---|---|---|
| 岛侧（改写） | `quic: 准入已发起（dev=%s，Hello %dB；等挑战/回执）` | 由 M1 的 `quic: 登记已发（dev=%s，%dB；等准入窗 %s）` 改写（`REG_SETTLE` 退役，§1.7） |
| 岛侧（新增） | `quic: 准入完成（dev=%s，耗时 %s）` | 「四帧走完 + 收到 `A4`」的一次性行（**r14 F22 订正**：v1 的「挑战 %d 次」字段随 Hello 重发分支一起删除） |
| 出口侧（新增） | `quic: 准入挑战已发（%v；在途未认证 %d/%d；第 %d 次）` | 节流「首 3 + 每 100」（`exit/mod.rs:83 log_due` 同款）；`在途未认证` = **未完成四帧的连接数 / 上限**（「不占额度」的直接证据） |
| 出口侧（扩展） | `quic: 准入被拒（dev=%s ← %v；%s；第 %d 次）` | `why` 取值集扩展（登记「接受集」）：`帧格式非法` / **`帧版本不符（H2/H3——旧核或垃圾包）`**（r14 F4）/ `已绑定连接的再准入`（F15） / `nonce 缺失/过期/已消费` / `hr-reg4 MAC 不符——含换连接重放`（**r14 F7**：由引擎 verdict 携带 `why`，否则出口面拿不到）/ `引擎裁决拒绝（见引擎侧归因行）`（含 ts 超窗，见 §1.4-4.3）/ `刷新帧但连接未绑定` |
| 出口侧（改写，**r14 F4 漏登记项**） | `quic: 准入被拒（dev={dev} pub={pub}；hr-reg3 MAC 不符——含换连接重放）`（**引擎侧**，`engine.rs:1541-1550`）→ `… hr-reg4 MAC 不符——含换连接重放 …` | 这是 **M1 S6 补登的判据行**（`docs/INTEROP-CRITERIA.md:576` 点了两种形态）⇒ 协议换版本时该串**必改**，必须登记（§6.2 已补条目） |
| 出口侧（新增） | `quic: 认证超时（%v 未在 %s 内完成证明——已弃；第 %d 次）` | pending 过期或 ADMIT_DEADLINE 到点（§1.3） |
| 出口侧（频率语义变化） | `quic: 连接采纳 …`（E-q2，已登记行）**频率从「每次 Accepted（含 60s 刷新）」改为「仅首次准入」** | **r14 F11**：M1 的控制循环对每次 Accepted 都 `bridge.bind`（`exit/conn.rs:130-139`）⇒ 今天每 60s 刷新会重打一条 E-q2；M2 明确「刷新不重绑、不打采纳行」⇒ 登记「频率/输入集」 |
| 计数（快照/JSON） | `ExitQuicSnapshot` 增：`challenges_issued` / `challenges_refused` / `proof_rejected` / `pending_expired` / `admit_timeouts`（+ `retry_sent` / `flood_refused`，§3） | `quic` JSON 段 additive（M1 已建该段，tier 只校验既有键 ⇒ 零影响） |

### 1.7 客户端状态机、预算与 `REG_SETTLE` 退役

- **客户端流程**（在 `race::run` 的胜者路径内，`client/race.rs:65` → `register_on_control_stream`）：
  `open_bi` → 写 `Hello` → 等 `C4`（预算内）→ 写 `Proof` → **等 `A4`** → 交回两半边。
- **`REG_SETTLE=400ms` 退役**（`client/register.rs:24`）：它存在的原因是「写一帧后无法知道出口何时完成裁决」
  ⇒ 只能睡一个经验窗；四帧流程给出**确定性回执**（`A4` 在 `bridge.bind` 之后由出口面立刻写出，
  `exit/conn.rs` 的 Accepted 分支）⇒ 用「等 `A4`，预算 = 准入预算」替代经验窗。
  - 代价对比（算术）：LAN RTT 1ms ⇒ 两往返 ≈ 2ms ≪ 400ms；LTE RTT 50ms ⇒ ≈100ms < 400ms
    ⇒ **确定性替代不仅更准，常态更快**（真机口径归 M6 复测）。
- **预算语义（r14 F8 订正后的现状 + 裁决）**：
  - **现状（实测，不是设计主张）**：`QUIC_CONNECT_BUDGET = 5s`（`tun_exec.rs:120`）只用于
    **赛跑循环的 deadline**（`client/race.rs:104-140`）；`register_on_control_stream`（`race.rs:174` 调）
    **今天没有任何超时**，唯一外层界是宿主的 `island_cmd(…, 5s + 5s)`（`tun_exec.rs:2297-2302`）
    ——超时归因会落到笼统的 RPC 超时，且**岛内任务仍持有连接**。
  - **M2 裁决**：①准入段必须**自带期限**（= §1.3 的 `ADMIT_DEADLINE=10s` + 客户端侧等待上限），
    不再依赖宿主 RPC 超时；②「剩余预算」规则写死：准入可用预算 = `max(剩余, ADMIT_MIN=2s)`
    （**r14 建议**）——晚胜（赛跑吃光预算）时仍给准入一个下界，避免「必然失败但不说清」；
    ③**回归**：`岛内任务在失败/超时后必须显式关闭连接**（不留悬挂连接）。
  - **实测判据**（实现期）：注入 300ms RTT 的本地代理下四帧流程 ≥3 轮成功；`ADMIT_MIN` 形态
    （人为把赛跑预算压到 0.5s）⇒ 归因为 `RegistrationFailed` 且**连接被显式 close**（不是等 30s idle）。
- **挑战重发/失败回落**：与 M1 同址同序（`rebind` 优先 → 既有阶梯），M2 不改恢复阶梯（M3 才重写）。

### 1.8 刷新面（60s，无挑战）

- **形态**：`R4` 帧（§1.2），走**已绑定**的连接；服务端要求「连接已绑定 + 帧内 pub/devTag == 绑定」
  （两道本地检查，零设备表查询）后才重建 v2 报文走 `table.register`。
- **为什么刷新不带挑战**：刷新**不重新做准入决策**（连接已在设备表内、绑定已在位），
  它的作用是「延长 `last_reg`」；加挑战会给每台设备每 60s 加两个往返（真机耗电/流量面劣化），
  收益为零。**连接绑定仍在**（exporter 入 MAC）⇒ 刷新帧跨连接重放无效。
- **与 M1 的关系**：M1 的刷新就是「同一个 `hr-reg3` 帧换个 ts」（`client/register.rs:80`），
  M2 换成 `R4`（域分隔独立），节拍不变（`RefreshTimer`，`client/register.rs:56`，60s）。
- **刷新成功不重绑**（**r14 F11 订正**）：M1 的控制循环对**每次** `Accepted` 都调 `bridge.bind`
  （`exit/conn.rs:130-139`）⇒ 今天每 60s 刷新会重打一条 `quic: 连接采纳 …`（E-q2，已登记行）。
  M2 明确：**刷新成功 = 只走 `table.register`（Refreshed）+ 打 C15' 行**，不重绑、不打 E-q2。
  ⇒ 登记「E-q2 的频率/输入集变化」（可观测面更准；排障脚本若按"每 60s 一条采纳行"写断言须改）。
- **C15' 行**（`quic: 注册刷新 → %v（dev=%s，中继=%v）`）**行文不变**（M1 已登记），M2 只换载荷帧。

---

## 2. 设备表语义与「连接 = 设备」

### 2.1 `table.rs` 语义逐条对照（**M2 的裁决 = 一行不改**）

| # | 语义（行号） | 今日实现 | M2 动作 → 依据 |
|---|---|---|---|
| 1 | 键 = devTag（`entries: HashMap<[u8;8], Entry>`，`:183`） | 同 devTag 同 pubkey ⇒ `Refreshed`（只刷 `last_reg`，打 E8，`register:305-318`） | **不改**；QUIC 档的刷新源换成 `R4`（§1.8） |
| 2 | 同 devTag 换 pubkey ⇒ `Rotated`（`register:319-350`）：先算新地址（排除视图）→ 原地改表 → 产 `Remove(old)+Add(new)` | 原子替换 + 固定顺序 | **不改**；QUIC 面额外动作 = `bridge.bind` 的「后到者替换并关旧连接」（`exit/bridge.rs:269-313`，M1 已落） |
| 3 | 表满 ⇒ 先**纯选择** stale victim（`select_stale_victim:459`，严格 `>` grace）→ **排除视图**校验地址（`assign_ip:476` / `ip_taken:523`）→ 通过才落库并产 `Remove`；全活跃 ⇒ `TableFull` 拒 | 表-设备一致性（P0-2） | **不改**（注意：`max_devices` 的额度**只由这里消耗** ⇒ §2.2） |
| 4 | TTL 回收（`gc:424`）：超 `DEFAULT_TTL=7d` 未刷新 ⇒ 移除 + `Remove` + E9 `reason=ttl`；`ttl=0` 关 | 周期 GC | **不改**（QUIC 面在 `Remove` 落位时同摘绑定/关连接，`apply_dev_ops:1573`） |
| 5 | 吊销（`verify:278-291` + `revoked` 钩子 `:134`）：每次验证复查 ⇒ `Revoked` 拒 + E9 `reason=revoked`（"吊销即时对新注册生效"） | 跟随读语义 | **不改**（M2 强化可观测：§2.4 的连接拆解最晚 60s 生效，登记为残余） |
| 6 | 地址派生与冲突（`assign_ip`/`assign_tun_ip`/`ip_held_by_pub`） | 双地址并集判定；同公钥不同 devTag 不算冲突 | **不改** |
| 7 | 拒绝计数与「首次大声」摘要（`rej`，`RejectReason` 四值 `NoToken/Revoked/TableFull/IpConflict`） | 计数 + 提示 | **不改**；QUIC 档的**新**拒绝面（nonce/MAC）在出口面计数（§1.6），不进本表（**输入集移动，登记**） |
| 8 | 时间窗 `REG_WINDOW=90s`（`:33`） | `verify_reg` 内 | **不改** |

**结论**：M2 对设备表是**语义等价**（不是"重写"）——`table.rs` 的实现与判据行**零 diff**，
唯一改动是**入口函数改名**（`match_reg3` → `match_proof`）+ MAC 输入换域标签。

### 2.2 「未认证连接不占额度」怎么落（三条，逐条可测）

1. **额度面**：`max_devices` 只被 `register` 成功路径消耗（`entries.insert`，`:398`）⇒ 未通过
   §1.4 步骤 4.2/4.3 的连接**结构上不占额度**。判据（**r14 F24 订正措辞**：E6 是启动期一次性行，
   本就不会"新增"）：注入 N（> `max_devices`）个「只发 Hello 不回 Proof」/「握手完成但不发 Hello」的连接 ⇒
   `table.len()` 不变、`reject_counts()` 不变、E7 族零新增行、`serve.status` peers 计数不变。
2. **资源闸面**：未认证连接受**另一套更紧的闸**（§3.2）：并发握手 ≤64、每源速率闸、
   **`ADMIT_DEADLINE=10s`（连接级）+ `NONCE_TTL=5s`（proof 级）双期限**（r14 F3）⇒
   「握手完成但不发 Hello」的形态也被收口（M1 的 `max_idle_timeout=30s` + `keep_alive=10s`
   组合下这类连接**不会自然过期**，是 M2 必须补的一格）。判据：注入 2× `conn_cap` 条「只握手入帧、
   不发 Hello」的连接 ⇒ ①全部在 `ADMIT_DEADLINE` 内被关（`admit_timeouts` 计数 = 注入数）；
   ②槽位回收后可再接纳合法连接；③footprint 增量 ≤ 闸上限 × 每连接上限。
3. **可观测面**：`quic: 准入挑战已发（…；在途未认证 %d/%d；…）` 与快照字段同源（§1.6）。
   判据：行与 JSON 同源（M1 的「同源」纪律，`M1.md` §4.3）。

### 2.3 E6–E9 / E18 逐行「保留 / 重写 / 登记差异」

| 行 | 现状（`docs/INTEROP-CRITERIA.md`） | M2 动作 | 理由 |
|---|---|---|---|
| **E6** | `peer 表：设备表就绪（cap=%d，ttl=%v，grace=%v；按 devTag 记账/刷新/轮换）` | **保留原串** | 三常量与语义零改动；行文里的「按 devTag 记账/刷新/轮换」在 QUIC 档**仍是事实**（M2 让它更准确） |
| **E7** | `peer: + dev=%s pub=%s ip=%v n=%d/%d` | **保留原串** | `register` 的 Added 路径逐字未改；QUIC 档新增的只是「谁有资格**到达** register」 |
| **E8** | `peer: ~ dev=%s refresh (idle=%s) n=%d/%d` / `peer: ~ dev=%s rotate pub=%s→%s ip=%v→%v …` | **保留原串** + **登记「计数输入集/数值语义」** | ①refresh 输入集在 QUIC 档 = 60s `R4` 帧（M1 的 C15' 起如此）；②**QUIC 档 `idle=` 的语义** = 距上次刷新帧（≤60s 节拍，比 WG 的"距上次注册"规整）；③rotate 输入集 = 新连接带新 pubkey 的 Proof。**行文一字不改** |
| **E9** | `peer: - … reason=ttl/stale …` / `peer: ! reject reason=revoked\|no-token\|verify` | **保留原串** + **登记「拒绝输入集移动」**（**r14 F1 订正**） | ttl/stale/revoked 触发集不变。**`no-token` 在 QUIC 档的真实输入集 = {MAC 已过、`ts` 超 ±90s 窗}**（`table.rs::verify` 把 `verify_reg` 的 `Expired` 归 `NoToken`，`:278-291`；M1 用例 `admit_reg3_still_enforces_reg_window` 已断言）—— **不是"输入集为空"**（v1 写错）。**MAC 不符**在 `match_proof` 就被拦（不进表）⇒ 「MAC 类拒绝」的归因由出口侧 `准入被拒` 行承接，「窗超类」的归因由**表内 `no-token` 行 + 出口 `引擎裁决拒绝`** 双面呈现 |
| **E18** | `凭证台账：%d 行记录 / %d 枚在用凭证（其中 %d 行已吊销；吊销即时对新注册生效）` | **保留原串** | 台账语义与计数与 token **载荷布局**无关（§4 改布局不改台账行） |

> **三选一的口径说明**：本表五行**全部选「保留原串」**，差异面（三处）一律走
> **「计数输入集 / 数值语义变化」节**登记（该节政策 = 行文不变、登记留痕——
> `docs/INTEROP-CRITERIA.md:628-632`）。**没有一行需要重写**，因为 M2 的设备表面
> 是语义等价实现（§2.1）。

### 2.4 撤销 / 轮换 / 表满在 QUIC 面的动作（收口 M1 的「最小对齐」）

| 事件 | 设备表侧（不改） | QUIC 面动作（M2 明确化） |
|---|---|---|
| `Removed`（TTL/表满淘汰/轮换旧钥） | `gc`/`register` 产 `Remove` | `apply_dev_ops:1573` → `q.unbind_pub(pubkey)`（关连接 + 拆绑定，M1 已落） |
| `TableFull` 拒 | E9 摘要行 + 计数 | 引擎回 `Rejected` ⇒ 出口面 `reject()` ⇒ 关连接（客户端 ≤1 RTT 得到失败，归因为 `引擎裁决拒绝`） |
| 吊销（合法 token 但已吊销） | `Revoked` 拒 + E9 | 同上；**在线连接**在下次刷新（≤60s）时被拒 ⇒ 关连接。**残余登记**：无「吊销 → 立即拆在线连接」的主动链（今日同档；M5/M7 若要可加 control-plane 钩子） |
| 轮换（同 devTag 新 pubkey） | `Rotated`（Remove+Add） | 新连接 `bind` 替换旧连接并 `CONNECTION_CLOSE`（`替换旧连接（dev=…）`行） |

---

## 3. 抗放大与限流

### 3.1 quinn Retry / token 的取舍（**证据先行**；§0.3 R1–R4 + §0.5）

**裁决：启用 quinn `Incoming::retry()`，策略 = 压力触发（不恒开）；并把 `retry_token_lifetime` 收到 5s。**

**先纠正 v1 的一处错论（r14 F2，阻塞项）**：v1 写「bloom 关 ⇒ 每次连接尝试都要走 Retry 才能拿到地址验证」
——**错**。Retry token 是**无状态 AEAD**，校验只查「地址一致 + 未过 `retry_token_lifetime`」，
**没有一次性记账**（`token.rs:141-170`；`TokenLog` 只在 NEW_TOKEN 分支被查）⇒
**一次通过 Retry 的地址，在 15s 窗口内可反复带同一 token 免 Retry 新建连接**（每次都判 `validated=true`）。
⇒ 「抗放大」的真实界是**每源速率闸（§3.2-④）**，Retry 的作用是**提高一次性成本 + 挡住不会重放的伪造源**
（被伪造的源收不到 Retry ⇒ 收不到 token ⇒ 完不成握手）。**登记**：`retry_token_lifetime` 从缺省 15s
收到 **5s**（与 `NONCE_TTL` 同档），压缩复用窗；若实测影响正常重连再调（配置项）。

| 候选 | 形态 | 代价 | 收益 | 取舍 |
|---|---|---|---|---|
| **恒 Retry** | 每次连接尝试都先 Retry | **每次重连 +1 RTT**（真机 LTE ≈50ms；M1 判据「断线恢复 ≤3.5s」被吃掉一小段）；且 bloom 关 ⇒ **没有任何"已验地址"捷径**（§0.5） ⇒ 每次重连都付 | 最强抗放大/抗源伪造 | **不取**（对单设备产品的常态路径是纯损失） |
| **压力触发 Retry**（**推荐**） | 三条触发条件任一命中才 Retry：①**未认证在途** ≥ `handshake_cap/2`（缺省 32）；②**同源「未完成/被拒」次数** ≥ `RETRY_AFTER_FAILS`（缺省 5 / 10s）；③最近窗口内有闸拒绝（攻击迹象） | 常态 0 额外 RTT；**r14 F10 订正**：v1 的「同源尝试 ≥3 次」会落在**正常赛跑包络内**（每轮对每个候选各发一条 Initial，多候选 + 阶梯重赛 ⇒ 同源 10s 内 ≥3 次是常态）⇒ 改为只计**未完成/被拒**的尝试（正常赛跑会完成 ⇒ 不计数） | 攻击/洪泛下自动升到「地址验证」档：提高攻击成本（每 5s 需重做一次 Retry）、伪造源被挡 | **取** |
| **不用 Retry（只靠 3× 限）** | 什么都不做 | 0 | RFC 底线仍在（quinn 内建 3×） | **不取**：M2 的交付物就是「抗放大面」，且闸拒绝是在**状态已建立之后**才发生（Retry 的早期拒绝更便宜） |
| 开 `bloom` feature + NEW_TOKEN | 已验地址的连接拿 token，后续免 Retry（**且 NEW_TOKEN 有一次性记账**） | 新依赖 `fastbloom 0.17` + OHOS 交叉复核 | 常态重连也 0 额外 RTT 且地址已验 | **M2 不做**（登记为 M6/M7 候选；理由 = 新依赖面 + 收益只在「恒 Retry」档才体现，而 M2 常态不 Retry） |

**实现要点**：
- 决策点 = `endpoint.accept()` 拿到 `Incoming` 之后（`exit/mod.rs:575` 的 `inc` 分支），
  **在现有三闸之前**取 `remote_address_validated()`/`may_retry()`（§0.3 R4）；
  **实现纪律（r14 F19）**：用 `!validated() && may_retry()` 守卫避免进 `Err`；若仍进 `Err`，
  **必须** `RetryError::into_incoming()`（公开 API，`incoming.rs:121-128`）再决定 accept/refuse——
  直接丢错误值会连带 drop `Incoming` ⇒ **隐式 `refuse()`（假拒绝）**。
- **Retry 与三闸的顺序**：先闸（廉价、本地）后 Retry？还是先 Retry？**裁决：先闸后 Retry**
  （闸拒绝 = `refuse()` 发 CONNECTION_REFUSED，同样不给攻击者建立状态；顺序影响只体现在
  计数归因上，登记为「闸优先」）。
- **观测**：`quic: 地址校验挑战（%v；在途未认证 %d/%d；第 %d 次）`（节流同款）+ 快照 `retry_sent`；
  与 `EndpointStats::refused_handshakes`（§0.5）对账——**口径注记（r14 F18）**：该计数只含应用层
  `refuse()`，不含 `<1200B` 短包与端点饱和的静默丢（`endpoint.rs:452-465`）。
- **措辞订正（r14 F17）**：Retry **是发往包内声称的源地址**（可被伪造）⇒ 准确表述 =
  「挡住未验证握手（伪造源收不到 token）+ 反射量有界（Retry ≈100B vs Initial ≥1200B）」，
  **不是**「只回给能收到的对端」。

### 3.2 握手限流迁移（Q-O 三闸的升级路径，**只增不减**）

| 闸 | M1（已落） | M2 升级 |
|---|---|---|
| ① 连接总数 | 存活连接 + 在途握手 ≤ `2 × max_devices`（`exit/mod.rs:112`/`:580`） | **不变** |
| ② 并发握手 | ≤ `DEFAULT_HANDSHAKE_CAP=64`（`:77`/`:589`） | **不变** |
| ③ 握手期限 | ≤ `DEFAULT_HANDSHAKE_DEADLINE=10s`（`:81`/`:602`） | **不变** |
| ④ **每源准入速率**（新） | — | **语义（r14 F9/F10 定死）**：**滑动窗**（两种实现都可，但必须写死并测）；键 = v4 **/32**、v6 **/64** 前缀；**只计「未完成/被拒」的尝试**（正常完成的建连不计数 ⇒ 不误伤多候选赛跑）；上限 `per_src_fails = 10 / 10s`（可配）；超限 ⇒ `refuse()` + 计数 + 节流行。**结构（r14 F30 定死）**：插入序 `VecDeque` + `HashMap` 索引（O(1) 摊还），**不是**每次扫描挑最旧（那会在洪泛下变成 CPU 放大器）；表 ≤1024 条（≈40–64KB） |
| ⑤ **准入双期限**（新） | — | `ADMIT_DEADLINE=10s`（**连接建立起算**：未走完四帧 ⇒ 关；覆盖「握手完成但不发 Hello」= r14 F3）+ `NONCE_TTL=5s`（proof 起算，§1.3）。两者都主动 `CONNECTION_CLOSE`，不等 idle 30s |
| ⑥ **Proof 失败闸（按 devTag）**（新） | — | 同 devTag 在 60s 内**nonce/MAC 类**失败 ≥ `proof_fail_threshold=10` ⇒ 冷却期内不再发 Challenge（拒 Hello）。**r14 F12 订正**：计数集**必须排除 `引擎裁决拒绝`**（表满/冲突/吊销/窗超都是合法设备的可用性故障，把它们算进冷却会把一次可用性故障放大成更长的锁死）⇒ 依赖 §1.4 的**类型化 `why`**；判据加「表满压测下合法 devTag 不被冷却」 |
| ⑦ 退出/收工 | `stop_within` 有界（`:385`） | **不变**；新增面（pending/闸表）随任务 drop 释放（无独立生命周期） |

- **闸表的形态**：新模块 `crates/homeway-quic/src/exit/admit.rs` —— **定死为纯 std**（只用
  `std::time::Instant`/`VecDeque`/`HashMap`，不含 `tokio|quinn|rustls|async fn|.await`）
  ⇒ **不进 `ASYNC_FILES` 清单**（r14 F26 定死，不再留问号）；S2-4 的负例自检保留
  （把该文件挪进清单应确定性红，反之亦然）。
- **配置面（r14 F14 定死值域与纪律）**：`serve.quic_admit` 段（additive；缺省 = 本文定值）：

| 键 | 类型 / 值域 | 缺省 | 越界处置 |
|---|---|---|---|
| `retry_token_lifetime` | 时长，`1s..=60s` | `5s`（收自 quinn 缺省 15s） | **拒启**（`serve` 段纪律照 Q-H 严格表：`serve.listen`/`quic_listen` 同款；与 `HOMEWAY_*` env 的「记行 + 缺省」纪律**不同面**，两套并存且写明） |
| `per_src_fails` / `per_src_window` | 次数 `1..=1000` / 时长 `1s..=1h` | `10` / `10s` | 同上（拒启） |
| `nonce_ttl` | 时长 `1s..=30s` | `5s` | 同上 |
| `admit_deadline` | 时长 `1s..=60s` | `10s` | 同上 |
| `proof_fail_threshold` | 次数 `0..=1000`（**0 = 关闭该闸**） | `10` | 同上 |
| `retry_policy` | 枚举 `pressure`（缺省）\| `always` \| `never` | `pressure` | 非法值 ⇒ 拒启；**`always` 的代价须记行**（常态 +1 RTT，§5-11） |

- env 面（排障）：`HOMEWAY_QUIC_ADMIT_RETRY=pressure|always|never`（非法值 ⇒ 记行 + 缺省，**不 fail-fast**——
  与 M1 的 `HOMEWAY_TRANSPORT` 同纪律，理由：排障开关不该把出口打进死路）。

### 3.3 「重连洪泛有界」的判据与可观测行

**判据（可 falsify，实现期逐条测）**：

1. **单源有界**（**r14 F9 订正算式**）：闸 = 滑动窗 `W`（缺省 10s）、上限 `F`（缺省 10 次）。
   单源以「**不完成准入**」的方式连续尝试 `K` 次（K ≫ F）：被拒次数 = `K − m`，其中 `m` 为窗口内
   被放行的次数（定窗口径 m = ⌈T/W⌉×F；滑窗口径 m ≈ F × T/W 的滑动量）⇒ **判据写成「在单个 W 内**：
   第 `F+1` .. `K` 次全部被拒（且 `flood_refused` 计数 = K−F）**」**，跨窗口只断言「增长速率 ≤ F/W」。
   **窗口语义（定窗/滑窗）与「被拒是否计数」必须在实现期写死并单测**（本设计不指定算法，只钉语义与可测式）。
2. **全局有界**：多源（本地起多 socket 源地址）合计尝试远超全局闸 ⇒ `handshakes_in_flight ≤ 64`
   且 `conns.len() + inflight ≤ 2 × max_devices`（快照可读），**且「握手完成但不发 Hello」的形态
   在 `ADMIT_DEADLINE` 内全部被关**（r14 F3 的判据）。
3. **不伤既有连接**：洪泛期间**已采纳连接**的隧道吞吐下降 ≤ 10%（同刻 A/B，本地合成流）；
   隧道可用性不劣化（`warmup pong`/`link:` 判据行仍按巡检节拍出现）。
4. **内存有界**：洪泛前后出口 footprint 增量 ≤ `闸上限 × 每连接上限`（M1 已登记的负载态口径：
   ≤64MiB + 自有队列）；闸表 ≤1024 条（≈40–64KB）。
5. **可观测**：三条行族（挑战/拒绝/洪泛）**都出现且计数与快照同源**（grep + JSON 双向可读）。
6. **常态不被误伤**（**r14 F10 新增**）：≥3 候选的**正常**赛跑连续 N 轮（≥5）⇒ `retry_sent = 0`
   且每源闸 `flood_refused = 0`（否则说明触发条件落进了常态包络——§3.1 的②按「未完成/被拒」计数
   正是为此）；`retry_policy=always` 档另测（= 每轮 +1 RTT 的代价登记）。

**可观测行（additive）**：
```
quic: 地址校验挑战（%v；在途未认证 %d/%d；第 %d 次）        首 3 + 每 100
quic: 握手洪泛拒绝（%v 在 %s 内第 %d 次尝试——已拒；第 %d 次） 首 3 + 每 100
quic: 认证超时（%v 未在 %s 内完成证明——已弃；第 %d 次）        首 3 + 每 100
quic: 证明失败闸（dev=%s 在 %s 内失败 %d 次——冷却 %s）         首 3 + 每 100
```

### 3.4 与中继预算的关系（**不新增压力**）

- M2 的新增控制包只出现在**建连/重连**时：Retry ≈100B（含 token + 16B 完整性标签；**r14 F27 订正**，
  v1 写"各 ≤50B"偏乐观）+ Challenge 18B + Accept 2B（**应用层帧会在 QUIC 包内共载**，不是各占一包）。
  算术：一次建连额外 ≤150B；按 60s 重连一次的极端假设 = 0.25 B/s ≈ 0.002 pps，对中继
  `200pps/源`（`relay/mod.rs:665`）与 `16MiB/s` 下行桶（`assoc_down_bytes`）**可忽略**。
- **中继代码零改动**（红线）：Retry/Challenge 都是 QUIC 加密载荷内的东西，中继不解析
  （`relay/mod.rs:907 forward_up` 保 kind 原样）⇒ M2 不动 `relay/**` 与 `relaywire.rs`。

---

## 4. token 格式（**待用户拍板**）

### 4.1 现状盘点（M1 已落的形态，代码事实）

```
"hmw1" ‖ base64url( peerId(32) ‖ secret(32) ‖ epCount(1) ‖ [type(1)+len(1)+addr]* ‖ [rpk(32)]? ‖ crc32(4) )
```
- `type`：0=direct / 1=relay / **2=QUIC**（`token.rs:81` `EndpointKind`，M1 新增）；
- `rpk`：**可选 32B 尾字段**（M1 新增；`parse_body:368` 只认尾长 `0|32`，其余判 `Malformed`）；
- 台账记录（`state.rs:263 append_record`）在既有四键后追加 `"rpk"`（additive）；
- 消费面：`token::decode/encode`（`token.rs:344/493`）、`serve` 铸造与 `serve token` 渲染
  （`state.rs:220/395`、`serve_cli.rs:1031`）、岛侧 `IslandCredential`（`facade/tun_exec.rs:2267-2276`）、
  候选过滤（`wg_endpoint_refs:118` + 岛的 `quic_endpoints`）、`fixtures/vectors/token.json`。

### 4.2 三个候选（代价 / 收益）

| 候选 | 形态 | 代价（本仓 + 跨仓） | 收益 |
|---|---|---|---|
| **A 继续 additive** | 在 `rpk` 之后再挂可选尾字段（例：QUIC 专用段 `[quic_flag(1)+port(2)]?`） | 解析器再加一条尾长分支；**n 个可选尾字段 ⇒ n+1 个合法长度**，且顺序耦合（不长在字段上） | 改动最小；既有 Go 冻结向量逐字节不变（但**现在没有兼容包袱 ⇒ 这个收益是零**） |
| **B 版本化容器**（**推荐，但有前置条件——见 §4.3-1**） | `"hmw2" ‖ base64url( peerId(32) ‖ secret(32) ‖ segCount(1) ‖ [segType(1)+len(2)+body]* ‖ crc32(4) )`；段分**两类**（**r14 F16 订正**）：`info`（可跳过的附加信息）vs `critical`（必须理解否则**拒**——用段类型高位或独立 `critical` 位标记，防止"未来加限制性凭证被旧解析器静默忽略"的 fail-open） | 一次性的编码/解析重写（`token.rs`）+ **跨面连锁（r14 F5 实测，五类，见下）** | 加字段**不再改布局**（每段自描述 ⇒「猜长度」口子结构性消失）；`EP_LIST` 成为类型化段族；M3+ 的字段需求不再触发第三次格式变更 |
| **C 派生凭证**（把 secret 换成派生凭证） | token 里带 `k_reg = HKDF(secret,"homeway/quic-reg")` 等**按用途派生**的凭证，或直接带 grant id（出口台账映射） | 设备表要按凭证 id/多密钥索引（`table.secrets: Vec<[u8;32]>` 现在是线性试）；台账结构变更；**若走 grant id 则要求出口台账在线**（破坏今日"token 自包含、出口无状态校验"的语义） | 密钥分离（reg/relay/psk 各自一把，一处泄露不牵连别处）+ 可精细撤销（按用途/按次） |

### 4.2.1 候选 B 的连锁影响面（**r14 F5 实测；这是拍板前必须落纸的东西**）

| # | 事实（回源复核） | 后果 |
|---|---|---|
| 1 | `token.rs:40 PREFIX="hmw1"` 是**全体 token 共用**的单一常量；`serve.quic=false` 的 WG-only token 也走它。而「`serve.quic=false` ⇒ token 串与 M1 前逐字节相同」是**已登记行**（`docs/INTEROP-CRITERIA.md:565`）+ 代码注释背书（`engine.rs:439-441/2143-2145`） | B 必然改掉**包括 `_wg` 在内每一个** token 的字节 ⇒ 与 §0.1/§6.1/S4-3 的「`_wg` 档 token 逐字节」硬判据**互斥**（二者必须只留一个，且要登记） |
| 2 | `relay/rltoken.rs:4/38-52`：`rl1` **复用 `token::encode/decode` 的 body**（只换前缀） | B 会**静默改掉 `rl1` 的 wire**：`config.toml` 里存量 `serve.relay=rl1…` 失效、Go↔Rust 中继互操作断——而 `relay/**` 一行没动 ⇒ **「中继零改动」在行为面被破**（形式面仍绿） |
| 3 | `fixtures/vectors/token.json` 把 `hmw2` 明文钉成 `unsupported-version-hmw2`（另有哨兵「缺少 hmw1 前缀」）；`tests/token_vectors.rs:88` 是**唯一** pre-M1 字节锚；`quic_wg_e2e.rs:304` 只是「同载荷再编码自洽」（B 下会**继续绿**但名字变假话 ⇒ 该不变量再无见证者） | 冻结向量需重生成 + 那个字节锚要么重写要么退役（**登记**） |
| 4 | 客户端 `daemon/hosts.rs` 的 `HostRecord.token: String` **存原始 token 串** | 既有主机条目全部要重 `host add`（真迁移，不只是"重贴"） |
| 5 | `tools/` 里按 `hmw1` 正则抽串的脚本 ≥10 处（`quic-wg-e2e.sh`（**S4-3 的复验脚本本身**）/`local-rust-exit.sh`/`local-exit.sh`/`matrix.sh`/`perf-ab.sh`/`qi-ab.sh`/`m1-ab-e2e.sh`/`rekey-check.sh`/`quic-island-e2e.sh`/`quic-probe`/`m1-ab`）+ 向量生成管线（`tools/vector-gen/vecgen_vectors_test.go` + ci-local 第 4 步 `git diff --quiet -- fixtures/vectors/`） | 脚本面要同批改；向量管线会把 `token.json` **覆写回 `hmw1`** ⇒ 不同批改必红 |

- **另**：§4.4 原清单**过度列出** `state.rs`（台账是**字段式**存储、渲染走 `encode`）与 `serve_cli.rs`
  （只是 `encode` 的调用方）——B 下这两处**无需改**（r14 F5-6 认同）。
- **改正后的 B 前置条件（拍板前必须落纸，见 §12-①）**：
  (a) 不变量重述为「**`_wg` 档 token 不含 QUIC/RPK 内容**」（而非"逐字节同"）并登记；或 (b) **只给
  QUIC 档换 `hmw2`、WG-only 保留 `hmw1`**（= 真正的 additive 折中，代价是双形态并存——与"简洁"冲突）；
  (c) 明确 `rl1` 处置（同批升版 + 中继互操作登记，或 `rl1` body 冻结在 `hmw1` 布局不随动——
  **推荐后者**：`rl1` 与 `hmw1` 解耦，各走各的版本常量）。

### 4.3 推荐：**B（版本化容器）** —— 给非本语境的用户

**一句话**：M2 是**已知唯一会动 token 的期**（无兼容包袱只在现在成立），现在把布局从
「按长度猜尾字段」换成「自描述段容器」，是一次性把后续所有字段需求变成"加一段"而不是"再改一次布局"。
**M2 本身并不需要新字段**（证明协议只用既有的 secret/rpk/端点表）——所以这不是"必须改"，
而是"要不要趁现在唯一一次免代价窗口把地基换掉"。

支持理由（三条，逐条可核）：
1. **脆弱点是真的**：当前解析靠尾长分支（`0|32`）识别 `rpk`；再加任何一个可选字段就变成三支，
   且字段顺序不可自描述 ⇒ 「猜长度」从"仅 rpk"变成"常态"。这正是候选 A 的长期代价。
2. **代价集中在可清点的五类**（§4.2.1 已逐条回源）：token.rs / fixtures 向量 / tools 脚本 / `rl1` /
   `hosts.json`；tier 侧**零代码**（App 透传）。若拖到 M5/M7（有部署/交付面）再做，代价更大。
3. **B 不排斥 C**：段类型化之后，C（派生凭证/grant）只是**新增一种段**，不是第三次布局重写；
   而 A 之后还想做 C，等于在"猜长度"的地基上再加固 —— 顺序上 B 优先。

**风险与缓解**（如实登记）：
- **阻塞前置（r14 F5）**：见 §4.2.1 末尾三条 (a)(b)(c)——**不落纸则不进入 B**（§12-① 已改写为带前置的拍板项）。
- 向量面：`fixtures/vectors/token.json` 是 **Go 冻结向量**（`docs/BASELINE.md` 冻结锚 `d4148f6`）
  ⇒ 转「历史参照」，新向量由本仓生成并纳入 `fixtures/SHA256SUMS` 与 `MANIFEST.md` 口径（**登记**）；
  `token_vectors.rs:88` 的 pre-M1 字节锚同步重写（否则成为假绿）。
- 兼容面：`hmw2` 前缀是新版本号（既有 `hmw1` 解析器报 `UnsupportedVersion`，`token.rs:344`
  ——与 Go 的 G1 panic 形态不同，本仓安全）；**旧 token 一律失效** ⇒ 用户重贴 + `hosts.json` 条目重建。
- **段的两类语义（r14 F16）**：`info` 段可跳过、`critical` 段必须理解否则整串拒——
  否则"未来加限制性凭证被旧解析器静默忽略"= fail-open。
- 若用户选 A：本设计**照常可实施**（证明协议不依赖新字段），只需把 §4.4 的登记条目换成
  「token 端点类/RPK 字段（M1 已登记）+ 无新增」；**若未来仍要改，须再来一次拍板**。

### 4.4 判据行登记条目草案（token，**两候选都先给出**）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-XX（M2 落地） | **token 载荷布局**（非编号判据行；**取值来源/格式化面**） | （选 B）`hmw1` + 尾长分支识别 `rpk` → **`hmw2` + 段容器**（`segCount` + `[segType+len+body]*`，段分 `info`/`critical` 两类）；E3 行文不变（token 仍是字符串载体） | 见 §4.3：趁唯一的无兼容包袱窗口，把"按长度猜字段"换成自描述容器（M3+ 加字段不再改布局）；**前置 = §4.2.1 的 (a)(b)(c) 三条落纸** | ①`crates/homeway-core/src/token.rs`（encode/parse_body + `PREFIX`）；②`fixtures/vectors/token.json`（**Go 冻结向量退役为历史参照** + 新向量纳入 `SHA256SUMS`/`MANIFEST.md`）+ `tests/token_vectors.rs:88` 的 pre-M1 字节锚重写（否则假绿）；③`tools/**` 的 `hmw1` 抽取面 ≥10 处（含 `tools/quic-wg-e2e.sh` = S4-3 的复验脚本本身）+ `tools/vector-gen`；④`relay/rltoken.rs`（`rl1` 的处置二选一：同批升版 / **推荐 = body 冻结在 hmw1 布局不随动**）；⑤客户端 `daemon/hosts.rs` 的 `HostRecord.token`（既有主机条目须重 `host add`）；**tier App 零代码**（只透传） |
| 2026-10-XX（M2 落地） | **`_wg` 档 token 不变量重述**（若选 B） | 「`serve.quic=false` ⇒ token 与 M1 前**逐字节相同**」（`docs/INTEROP-CRITERIA.md:565` 已登记）→ **「`_wg` 档 token 不含 QUIC/RPK 内容」**（字节面按 §4.2.1-(a) 的选择：全量升版 / WG-only 保留 `hmw1`） | 单一 `PREFIX` 常量（`token.rs:40`）与"逐字节同"在 B 下**不可能同时成立**（r14 F5-1）⇒ 必须显式改口径，不许静默 | 同上 + 判据验收方（`_wg` 档断言脚本） |
| 2026-10-XX（备选 = 选 A） | 同上 | 保持 `hmw1`，新增字段**零**（M2 证明协议不需要新字段） | 若用户判「地基换不换不急」⇒ 本批不动 token，只登记「M2 未改 token」 | 同左（`token.rs` 零改动；向量零改动；`tools/**` 零改动） |

---

## 5. 威胁模型（设计门要求：随设计定稿）

| # | 攻击面 | 现有防线（引 M1 已落的） | **M2 新增** | 残余风险（如实登记） |
|---|---|---|---|---|
| 1 | **token 泄露**（截图/转发/备份） | MAC 准入（`hr-reg3`）+ 出口 RPK 钉定 | 每源速率闸 + Proof 失败闸 + 未认证状态有界 + 拒绝可观测 | **不阻止持有者接入**（这是「token = 凭证」的设计语义）；吊销后**在线连接最晚 60s（下次刷新）被拒**（无「吊销 → 立即拆」主动链，今日同档） |
| 2 | **帧重放（跨连接）** | **TLS exporter 连接绑定**（M1 `hr-reg3`，M2 保留进 `hr-reg4` 的 MAC 输入） | —（职责已由 M1 关闭） | 无（需同时具备：捕获帧 + 完成 TLS 握手成为对端；后者被 RPK 钉定挡住）。**证据锚（r14 F23）**：`exit/rpk.rs::server_config` 不开 session storage/early data ⇒ exporter **恒出自完整握手**（全仓零 `early_data`/`into_0rtt`/`session_storage`） |
| 3 | **同连接 Proof 复用** | — | nonce **一次性消费** + `NONCE_TTL=5s` | 无（服务端串行任务内 take-first）。**准确口径（r14 F23/复核对 5）**：对「未持 secret 者」nonce **不提供**新抵抗力；其唯一密码学附加值 = 「连接密钥泄露但 secret 未泄露」档的纵深防线 |
| 4 | **MITM / 假出口** | RPK 钉定（错 pin ⇒ 握手中止，`exit/rpk.rs:79`） | — | **token 分发通道**（用户粘贴）是信任根；若 token 由不可信通道传递，pin 也被污染（登记：属产品流程面，不在本程序范围） |
| 5 | **重连/握手洪泛** | Q-O 三闸（连接总数 / 并发握手 / 握手期限） | **压力触发 Retry**（提高一次性成本；**r14 F2**：retry token 15s→**5s** 才压得住复用）+ 每源速率闸 + **双期限（ADMIT_DEADLINE 10s / NONCE_TTL 5s）** + `refuse()` 早拒 | **分布式（多 IP）洪泛**：只能靠全局闸拒绝，会连带拒合法新连接（登记为 DoS 残余；缓解 = 已采纳连接不受影响）。**另（r14 F2）**：单地址在 `retry_token_lifetime` 窗内可复用一次 Retry 的成果 ⇒ 真实界是每源闸，不是 Retry |
| 6 | **资源耗尽（内存/FD/CPU）** | 每连接 1MiB×2（`exit/transport.rs:35`）+ 连接上限 2×max_devices（=64）+ 握手在途 ≤64 | **每源闸表有界（≤1024 条）/ pending 不建表 / `ADMIT_DEADLINE` 收口「握手完成但不发 Hello」**（**r14 F3**：M1 的 `max_idle_timeout=30s` + `keep_alive=10s` 组合下这类连接**永不自然过期**） | 未认证连接的**握手 CPU**（Initial 解密）在 64 并发上限内可被吃满（缓解 = Retry 提升攻击者成本；登记 CPU 面存残余） |
| 7 | **反射放大（源地址伪造）** | QUIC 内建 3× 限（quinn-proto `connection/mod.rs:590-600`） | **Retry（地址验证）**；Challenge **只在握手完成后**（地址已由握手验证 ⇒ 结构上无放大面） | Initial/Handshake 流本身（RFC 允许 3×）——协议底线，不消除 |
| 8 | **时钟攻击**（歪 ts 重放/未来 ts） | `REG_WINDOW=±90s`（`table.rs:33`，表内原路径） | nonce 窗用**服务端 `Instant`** ⇒ 与客户端时钟解耦 | 客户端时钟歪 ⇒ 被表内既有窗拒（归因行来自表内，行为与今日同） |
| 9 | **日志洪泛 / 信息泄露** | 节流「首 3 + 每 100」（`exit/mod.rs:83`）+ secret Debug 脱敏/Drop 擦除 + 短指纹纪律 | 新行族一律带节流；nonce/pending **不落盘** | devTag/pubkey 短指纹（4B）可跨实例关联（今日同档；登记） |
| 10 | **0-RTT 重放**（附录 D 风险 6） | 未启用（M1 未开 early data） | **M2 明确不启用**（保守；理由：0-RTT 数据可在准入前到达，与「准入先于数据」语义冲突，且 RFC 9000 §9.2 明确 0-RTT 可重放） | 重连多 1 RTT（已被 M1 现状覆盖；登记为决定，非欠账） |
| 11 | **配置面误用**（`serve.quic=false` × `transport=quic`） | M1 已裁决：候选空 ⇒ 岛回落 WG + 记行（`S3-1` 行族） | 新增 admit 段**值域非法 ⇒ 拒启**（Q-H 严格表纪律，§3.2）；`retry_policy=always` ⇒ 常态 +1 RTT（记行告警） | 无新增（r14 F14：v1 只覆盖了 `serve.quic=false` 一格 ⇒ 已补 admit 段误用行） |
| 12 | **强制回落 WG**（**r14 F23 新增**） | WG 准入面（reg2 MAC + WG 握手私钥） | —（本批不加固 WG 面） | 攻击者丢弃 UDP 即可把客户端逼到 WG 档——**但 WG 面仍要求设备私钥**（重放/伪造同样过不去）⇒ 不是降级攻击；登记为**本批未加固面**（M5 删 WG 后自然消解） |
| 13 | **闸表被挤兑**（**r14 F23 新增**） | 闸表 ≤1024 条（F30 的 O(1) 结构） | 多源（>1024）可把合法源挤出表 ⇒ 合法源被重新计数（**只影响计数意义，不影响准入正确性**） | 登记：极端多源洪泛下「每源闸」退化为「近似全局闸」（仍受全局三闸兜底） |
| 14 | **单连接多设备**（**r14 F15/F23 新增**） | — | 已绑定连接拒 `H4/P4`（§1.3 的 F15 行）+ 「`by_conn`/`by_dev`/`by_pub` 三索引一致、单连接只对应一个 dev」断言 | 无（M1 的 `bind()` 二次绑定会覆盖 `by_conn` 而残留旧 dev 索引 ⇒ 本批以门禁 + 断言关掉） |

---

## 6. 判据行影响（M2 批）与登记条目草案

### 6.1 分类总表

| 类 | 条目 | 去向 |
|---|---|---|
| **行文改写（3 处；r14 F4/F21 订正——v1 写「仅 1 处」不成立）** | ①岛侧 `quic: 登记已发（dev=%s，%dB；等准入窗 %s）` → `quic: 准入已发起（dev=%s，Hello %dB；等挑战/回执）`；②**引擎侧** `quic: 准入被拒（… hr-reg3 MAC 不符——含换连接重放）` → `hr-reg4`（**M1 S6 补登的判据行**，`docs/INTEROP-CRITERIA.md:576`）；③出口侧 `quic: 丢弃 … 源校验拒=%d` 的**明细文本**加 `src=%v`（明细为自由文本，但 M1 有「文案订正」补登先例 ⇒ 一并登记，免歧义） | §1.6 / 登记条 |
| **取值/归因集扩展（行文不变）** | `quic: 准入被拒（…；%s；…）` 的 `why` 集（含新增 `帧版本不符（H2/H3）` / `已绑定连接的再准入`，且 `hr-reg4 MAC 不符` 由引擎 verdict 携带）；`ExitQuicSnapshot`/`quic` JSON 段新增字段 | 登记条（两处） |
| **频率/输入集变化（行文不变）** | `quic: 连接采纳 …`（E-q2）**频率从「每次 Accepted（含 60s 刷新）」→「仅首次准入」**（r14 F11）；E8（refresh `idle=` 语义）、E9（`no-token` 输入集 = {MAC 过、窗超}） | 「计数输入集」节 + 登记条 |
| **新增（additive）** | `quic: 准入完成（…）` / `quic: 准入挑战已发（…）` / `quic: 认证超时（…）` / `quic: 地址校验挑战（…）` / `quic: 握手洪泛拒绝（…）` / `quic: 证明失败闸（…）` | 登记条（合并 1 条 + 逐行明细） |
| **配置新增（additive）** | `serve.quic_admit` 六键（值域/缺省/越界处置见 §3.2）+ env `HOMEWAY_QUIC_ADMIT_RETRY` | 登记条 |
| **保留原样** | E6/E7/E8/E9/E18（§2.3）；M1 已登的 `quic:` 族行**除上表点名的 3 处外一字不改**；`_wg` 档全部（**× 若选 token 候选 B 则该条须按 §4.2.1 重述**） | §2.3 说明 |
| **留给 M3/M5** | C 族 WG 档形态退役（M3/M5）；`E23` 等措辞去 WG（M5） | 沿用 M1 §3.5 的交接清单 |

### 6.2 登记条目草案（照 `INTEROP-CRITERIA.md` 登记表字段；**收口时逐条粘贴**）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-XX（M2 落地） | **准入协议版本（`hr-reg3` → `hr-reg4`）**（非编号判据行；**安全面 + 线协议面登记**） | `H3` 单帧（`mac` 混 exporter）→ **`H4/C4/P4/A4` 四帧**（Hello 50B / Challenge 18B / Proof 82B / Accept 2B；`mac = HMAC(secret,"hr-reg4"‖pubkey‖devTag‖ts‖nonce‖exporter32)[:16]`）；**`H3` 起被拒** | ①M2 交付物 = 客户端证明（Hello/Challenge/Proof）；②nonce 给出口「首帧之后、MAC 试秘之前」的资源决策点与未认证状态的有界生命周期（**r14 F2/F6 订正后不再声称"发挑战前"与"抗重放主防线"**）；③确定性 `A4` 替代经验窗 `REG_SETTLE=400ms`。**`H3` 不并存**（无兼容包袱 + 不翻倍未认证面） | `crates/homeway-quic/src/reg3.rs`（→ `reg4.rs`）、`exit/conn.rs`、`client/register.rs`、`client/race.rs`、`server/table.rs`（`match_reg3`→`match_proof`）、`server/engine.rs`（`admit_reg3`→`admit_reg4` + 归因行版本串）；**tier 零改动**；旧核/新出口或反之**不互操作**（无兼容包袱允许） |
| 2026-10-XX（M2 落地） | **岛侧准入行改写 + 新增** | `quic: 登记已发（dev=%s，%dB；等准入窗 %s）` → `quic: 准入已发起（dev=%s，Hello %dB；等挑战/回执）`；**新增** `quic: 准入完成（dev=%s，耗时 %s）`（**r14 F22：无「挑战 %d 次」字段**） | 四帧流程 + `REG_SETTLE` 退役（§1.7）；「准入完成」= 收 `A4` 后一次性行 | `crates/homeway-quic/src/client/register.rs`、`client/mod.rs`；排障脚本若 grep 旧串需改（M1 登记批的读者面） |
| 2026-10-XX（M2 落地） | **出口侧准入/抗放大行族（additive）** | 无 → 有：`quic: 准入挑战已发（%v；在途未认证 %d/%d；第 %d 次）` / `quic: 认证超时（%v 未在 %s 内完成证明——已弃；第 %d 次）` / `quic: 地址校验挑战（%v；在途未认证 %d/%d；第 %d 次）` / `quic: 握手洪泛拒绝（%v 在 %s 内第 %d 次尝试——已拒；第 %d 次）` / `quic: 证明失败闸（dev=%s 在 %s 内失败 %d 次——冷却 %s）`（**全部节流：首 3 + 每 100**） | 「未认证连接不占额度」「重连洪泛有界」「抗放大」三条判据的可观测落点（§1.6/§3.3） | `crates/homeway-quic/src/exit/{mod,conn,admit}.rs`；出口排障读者；**均 additive** |
| 2026-10-XX（M2 落地） | **`quic: 准入被拒（…）` 的归因集扩展** | `why ∈ {帧格式非法（魔数/长度）, TLS exporter 不可得, 引擎裁决拒绝（见引擎侧归因行）}` → **+** `nonce 缺失/过期/已消费`、`hr-reg4 MAC 不符——含换连接重放`、`刷新帧但连接未绑定` | 四帧流程的失败面（§1.4）必须可归因（安全面排障） | 同上；计数 `regs_rejected` 语义不变，只扩 `why` 取值集 |
| 2026-10-XX（M2 落地） | **`ExitQuicSnapshot` / `quic` JSON 段新增字段（additive）** | 无 → 有：`challenges_issued` / `challenges_refused` / `proof_rejected` / `pending_expired` / `retry_sent` / `flood_refused`（`quic` 段平级新增键；既有键序/形态不变） | §1.6/§3.3 的可观测面（程序读，与行同源） | `crates/homeway-quic/src/exit/mod.rs`、`crates/homeway-core/src/facade/tun_status.rs`；tier 只校验既有键 ⇒ 零影响 |
| 2026-10-XX（M2 落地） | **引擎侧准入归因行的协议版本串（r14 F4 补登）** | `quic: 准入被拒（dev={dev} pub={pub}；hr-reg3 MAC 不符——含换连接重放）` → `… hr-reg4 MAC 不符——含换连接重放 …`（`crates/homeway-core/src/server/engine.rs:1541-1550`） | 该串是 **M1 S6 补登的判据行**（`docs/INTEROP-CRITERIA.md:576`）⇒ 协议换版本时**必改**，须登记（v1 §6.2 漏） | `server/engine.rs`；出口排障读者；**WG 档不受影响**（WG 档的 reg2/`hr-reg2` 行文中无此串） |
| 2026-10-XX（M2 落地） | **E-q2（`quic: 连接采纳`）的频率/输入集** | 「每次 `Accepted`（含 60s 刷新刷新帧）都打」→ **「仅首次准入打；刷新成功不重绑、不打」**（行文不变） | r14 F11：M1 的控制循环对每次 Accepted 都 `bridge.bind`（`exit/conn.rs:130-139`）⇒ 今天每 60s 一条采纳行；M2 明确刷新语义后频率变化须登记 | `crates/homeway-quic/src/exit/{conn,bridge}.rs`；按「每 60s 一条采纳行」写断言的排障脚本须改 |
| 2026-10-XX（M2 落地） | **E-q3 明细文本增 `src=%v`**（行文主字段不变） | `quic: 丢弃 超限=%d 发送缓冲满=%d 未登记=%d 源校验拒=%d（本次：src ∉ {tunnel_ip,tun_ip}（dev=…）；…）` → 明细里补实际 `src=%v` | M1 真机发现①（`quic: 源校验拒` ≥3 次、核侧 drops 全 0）**当前无法定性**（明细不含实际 src）⇒ 补一个字段让下次真机一眼定性（r14 F21 认同该增强，但要求口径统一：明细是自由文本 ⇒ 本条为**可选登记**，登记面 = 四字段计数行不变） | `crates/homeway-quic/src/exit/conn.rs`（源校验分支）；排障读者 |
| 2026-10-XX（M2 落地） | **配置新增段 `serve.quic_admit`（additive，六键 + 一 env）** | 无 → 有：`retry_token_lifetime`（5s）/`per_src_fails`（10）/`per_src_window`（10s）/`nonce_ttl`（5s）/`admit_deadline`（10s）/`proof_fail_threshold`（10；0=关）+ env `HOMEWAY_QUIC_ADMIT_RETRY` | §3.2：抗放大与限流的可配面（缺省即可用；值域非法 ⇒ 拒启 = Q-H 严格表纪律；env 非法 ⇒ 记行 + 缺省 = M1 开关纪律） | `crates/homeway-cli/src/serve_cli.rs`、`server/engine.rs`（`ServeConfig`）、`nodestate.rs` 模板键表；config.toml 对 Go 侧单向不兼容（Go 已退役） |
| 2026-10-XX（M2 落地） | **token 载荷布局（拍板后二选一；见 §4.4 的同名条目）** | 见 §4.4 | 见 §4.3 | 见 §4.4 |

**计数输入集节（行文不变，4 行）**：①E8（QUIC 档 refresh 输入集 = `R4` 帧；`idle=` = 距上次刷新；
**且刷新不再产生 E-q2 行**）；②E9（**QUIC 档 `no-token` 输入集 = {MAC 已过、`ts` 超 ±90s 窗}**；
MAC/nonce 类拒绝归因在出口面，不进表内计数——r14 F1）；③`quic: 丢弃 未登记`
（输入集 + 「未完成 Proof 的连接发来的数据报」——M2 后窗口更窄，数值语义更准）；
④`quic: 准入被拒`（`regs_rejected`）的输入集 = {帧非法/版本不符/再准入/nonce 类/MAC 类/引擎拒绝/未绑定刷新}（r14 F7/F1）。

---

## 7. 真机验证计划（**设备已装 QUIC 核**；操作用 `docs/DEVICE-TEST-OHOS.md`）

| M2 判据 | 真机可验？ | 做法 / 判读 |
|---|---|---|
| 四帧准入走通 | **能** | 装机 → 免点屏注入 token → 开关 VPN → 核日志应见 `quic: 准入已发起` + `quic: 准入完成`；出口日志应见 `quic: 准入挑战已发` + `quic: 连接采纳 dev=… tun=… ← 192.168.3.x:xxxxx` |
| 错 token / 错 nonce 拒绝 | **能（错 token）** | `--ps host_token '<坏 token>'` ⇒ 出口 `quic: 准入被拒（… hr-reg4 MAC 不符 …）` + 岛侧 `RegistrationFailed` 归因；**错 nonce 只能本地注入**（真机上无法构造） |
| 重连不新增设备表条目 | **能** | VPN 关→开 ×3 ⇒ 出口应见 `peer: ~ dev=… refresh`（而非 `peer: +`）；`serve.status` peers 计数不变 |
| 吊销即时生效（对新注册） | **能** | `serve token revoke` → 设备重连 ⇒ `peer: ! reject reason=revoked`（台账行 E18 同步） |
| 表满 / 淘汰 | **能（需注入）** | `--max-peers 1` 起本地出口 + 第二台设备/第二个身份 ⇒ E9 `reason=table-full` / `stale` |
| 抗放大 / 重连洪泛 | **不能**（真机不做洪泛注入） | 全部本地（`tools/` + 集成测试）；真机只复核「常态建连未因 Retry 变慢」（App 侧开关 VPN 到 `warmup pong` 的就绪耗时对比 M1 读数） |
| `_wg` 档逐字节回退 | **能** | 出口 `--quic=false` ⇒ 核日志 `quic:` 族零输出 + C 族原串（M1 的 `tools/quic-wg-e2e.sh` 已有断言，M2 后复跑） |
| 蜂窝/WiFi→蜂窝（挑战窗与 RTT） | **不能**（系统开关不吃模拟点击，M1 已实测） | 归 M6；M2 只在本地用注入 RTT 的代理验「预算够」 |
| 真机性能/耗电 | **不在 M2 范围** | M6 |

---

## 8. 预算（体积 / CPU / 内存，可测性）

| 维度 | 预期 | 测法 |
|---|---|---|
| 代码量 | 净增 ≈700–1000 行（`reg3.rs`→`reg4.rs` 改写 + `exit/admit.rs`（闸表，纯 std）+ `conn.rs`/`register.rs` 状态机 + 行/测试） | `git diff --stat` |
| 体积 | OHOS `.so` 增量 **≤ +32KB**（估：无新依赖大件；`getrandom` 已在树内） | `tools/build-app-core.sh` 的 `[size]` 行；对 3.8MB 阈值仍属 M5 判 |
| CPU | 每次准入 +2 次 HMAC-SHA256（66B/82B 输入）+ 1 次 16B RNG + 闸查表（**O(1) 摊还**，§3.2-④） | `tools/quic-ab.sh cpu`（不受影响）+ 准入路径微基准（实现期；**判据 = 不越出 M1 读数 ±10%**） |
| 内存 | pending = 每连接 ≤48B（栈上，随任务）；闸表 ≤1024×≈64B ≈ 40–64KB | `vmmap` 口径（M1 已用的 E 档）；判据 = 单连接边际仍 ≤+320K（M1 未过格不因 M2 恶化） |
| 线开销 | 准入新增 **≈120–150B/次建连（含 Retry 时）** 一次性（**r14 F27 订正**：v1「各 ≤50B」偏乐观——Retry 含 token + 16B tag ≈100B）；稳态每包不变 | `tools/quic-ab.sh overhead`（应逐字节不变） |
| 冷启动/恢复 | 准入两往返 + `ADMIT_DEADLINE`/`NONCE_TTL` 双期限（§3.2-⑤） | 注入 300ms RTT 的本地代理下 ≥3 轮成功；`ADMIT_MIN` 形态归因正确（§1.7） |

---

## 9. 风险与未决

### 9.1 M1 交下的两条真机发现在 M2 的处置（**点名，不许漏**）

| # | 发现（`docs/reviews/M1.md` §3.6 / `SUMMARY.txt` §发现-2） | M2 处置 |
|---|---|---|
| 1 | 出口 `quic: 源校验拒` 首次 attach 后 ~1s 出现 **≥3 次**（核侧 `drops` 全 0 ⇒ 出口侧单向计数；猜因 = 上一世代遗留的排队 TUN 包） | **M2 增强归因并复验**：E-q3 的源校验拒**明细行**补 `src=%v`（今日明细已有 `src ∉ {tunnel_ip,tun_ip}（dev=…）`，但**不含实际 src** ⇒ 无法一眼定性）。**r14 F21 订正**：v1 写「M2 的 `A4` 门禁可能让该现象自然消失」**缺机制支撑**（详情行绑定存在 ⇒ 说明内层 src 只是不匹配，与时序无关）⇒ **去掉该预测**，只保留「补 src 字段 + 真机复看」；明细是自由文本（登记面 = 四字段计数行，见 §6.2 的可选登记条） |
| 2 | tier `docs/agents/log-index.md` 陈旧拦 `build-core.sh`（对 main 检出亦陈旧 = 既存问题） | **转交（tier 侧触点）**，本仓不做。**M2 的义务 = 把新增/改写的判据行清单交给主会话转 tier**（否则 M2 后门更红）；操作绕过与留痕流程已写入 `docs/DEVICE-TEST-OHOS.md` §1。承接项列入 §10 的 S6-3 |

### 9.1.1 M1 明确交下的「M2 面」条目（**r14 F13：v1 全篇缺席，现补齐**）

| # | M1 交下项（`docs/reviews/M1.md` §6） | M2 处置 |
|---|---|---|
| N2 | 出口「腿表条数」行缺失（Q-N 的腿表峰值只能用中继 `分配腿` 代理） | **转交 M7**（出口腿表面随 M5 的腿表退役一并收；M2 不碰腿表）。理由 = M2 无腿表改动面 |
| N4 | N-a 与 E-q1 同前缀（`quic: 端点就绪（`）「若 M2 要统一可同批改」 | **不改**（M2 不新增同类同前缀行；改串要再动两条已登记串 + 2 脚本 + 1 测试，收益为零）⇒ 维持 M1 的豁免登记 |
| N5 | 窄路径 `tools/` 级 E2E 注入未做（单元证据已在） | **M2 承接（低成本）**：在 `tools/m1-ab-e2e.sh` 加一条 `HOMEWAY_QUIC_MTU` 注入断言（S5-5）。注意 M1 已登记「区间 [1320,1400] 产不出 `mds<1280`」⇒ 断言面 = 「窄路径不可用行 + `超限` 计数可见」而非真窄路径 |
| N8 | ①A4（**M1 的"A4"= 黑洞期已入缓冲的 1 MiB 无计数**；**注意与 M2 的 `A4` Accept 帧同名，勿混读**）承接 M2「快照暴露 `send_buffer_used`」；②L3（`host reach` 的 QUIC 过滤无接线测试）；③L6（`probe()` 证据面）→ M3 | ①**M2 承接（部分关闭）**：`quic` 段/快照暴露 `send_buffer_used`（`Connection::datagram_send_buffer_space()` 已有 API）⇒ 黑洞期的在途缓冲**可观测**；**残余**（连接终结时被 quinn 静默丢的那批仍无逐包计数）登记在案；②**转交 M3**（与 M2 的准入面无交集，且 L3 需真 token + UDP 应答器 + 3.5s 预算）；③**已是 M3**（巡检定型） |

### 9.2 M2 自身的风险与未决

| # | 风险 / 未决 | 处置 / 承接 |
|---|---|---|
| Q-M2-1 | **+2 RTT 的准入**在差网（高 RTT/丢包）下可能吃满 `QUIC_CONNECT_BUDGET=5s` | §1.7 的预算复核 + 本地注入 300ms RTT 实测；不够 ⇒ 上调预算（登记）或把 `Hello` 搭在握手后的首个 1-RTT 包里（实现期若需要，改设计先登记） |
| Q-M2-2 | **压力触发 Retry 的阈值**（32 在途 / 3 次每 10s）是**初值**，无实测依据 | 实现期用本地洪泛注入标定（§3.3 判据 1/2）；阈值入配置段 ⇒ 调整不改代码 |
| Q-M2-3 | 每源闸的**键粒度**：v4 /32、v6 /64（防前缀内轮换）——运营商 CGNAT 下同一 /32 可能承载多用户（我们产品是个人设备 ⇒ 影响小） | 登记为已知形态；若真机出现「同一 NAT 下第二台设备被误拒」⇒ 放宽到 /24 或按 devTag 二次判定（登记后改） |
| Q-M2-4 | **网关/蜂窝下的 Retry 互操作**（部分中间盒丢弃未知 UDP 载荷？） | Retry 是 QUIC 标准包（长头、标准形态），中间盒按 UDP 转发 ⇒ 风险低；真机（M6）复验 |
| Q-M2-5 | **吊销 → 在线连接**无主动链（最晚 60s） | 登记（§2.4）；M5/M7 若要"秒级拆"再加 control-plane 钩子 |
| Q-M2-6 | `nonce` 不落盘 ⇒ 出口重启后**在途未认证连接全废**（客户端重连即可） | 与今日同档（出口重启本就断所有连接）；登记 |
| Q-M2-7 | 0-RTT（拍板点之一）**本设计取不启用** | 见 §5-10；若用户要求启用 ⇒ 需重新设计准入与 0-RTT 数据的关系（**不建议**） |
| Q-M2-8 | token 格式（拍板点） | §4；A/B 两候选本设计都可实施 |

---

## 10. 实施清单（切片 + 完成判据；主会话据此切棒）

**依赖顺序**：S1 → S2 → {S3, S4} → S5 → {S6, S7}（S3/S4 依赖 S1 的帧层与 S2 的客户端状态机；
S5 须独占机器；S6 代码门在全部代码切片合入后）。

### S1 出口证明面（帧层 + pending + 裁决接口）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S1-1 | `reg3.rs` → `reg4.rs`：四帧常量/编解码（`H4/C4/P4/A4` + 刷新 `R4`）、MAC 输入（`hr-reg4` / `hr-reg4-refresh`）、exporter 标签沿用；`H3` 被拒 | 单测：布局锚点（逐字节长度与偏移）/ 版本互斥（`H3` 不解析）/ 域分隔（Proof 与 Refresh 不可互冒）/ 换连接重放必失败（exporter 变）/ 篡改任一字段必失败 |
| S1-2 | `exit/conn.rs::control` 状态机（Hello→Challenge→Proof→Accept）+ pending（任务内局部）+ 一次性消费 + TTL + Hello ≤3 | 单测：正常四帧序（真连接，回环）；nonce 过期/复用/缺 Hello 的 Proof ⇒ 拒；**拒绝路径零设备表副作用**（表 len/拒绝计数/判据行断言） |
| S1-3 | 引擎接口：`Reg3Request/Reg3Verdict` → `Reg4Request/Reg4Verdict{Challenge, Accepted, Rejected}`（`exit/bridge.rs`）；`admit_reg3` → `admit_reg4`（重建 v2 报文 + `table.register` 手法**原样**） | 单测：合法 ⇒ `Accepted` + `peer: +`/`peer: ~` 行；MAC 错 ⇒ `Rejected`；**时间窗仍由 `table.register` 承载**（超窗 ⇒ MAC 过但登记拒，证明没绕开原语义） |
| S1-4 | 归因行与计数（§1.6 全族；`ExitQuicSnapshot` +6 字段） | 行族逐条有实现 + 单测断言；行与快照同源（注入一次 ⇒ 行与计数同时 +1） |
| S1-5 | `table.rs`：`match_reg3` → `match_proof`（换 MAC 输入；**其余零改动**） | `git diff` **只含该函数、其 doc/import（新帧类型）与其测试**（**r14 F29 订正**：v1 写「只含该函数与其测试」过严）；`register`/`gc`/`select_stale_victim` **零 diff**（可按函数体抽取断言） |
| S1-6 | **准入双期限**（r14 F3）：`ADMIT_DEADLINE`（连接建立起算，包住「等首帧 + 整段状态机」）+ `NONCE_TTL`；到点 `reject` + `CONNECTION_CLOSE` | 单测/集成：**只握手不发 Hello × N ⇒ 到点全关、槽位回收**（`admit_timeouts` = N，且随后合法连接可被接纳）；`pending` 过期同链 |
| S1-7 | **类型化裁决**（r14 F7）：`Reg4Verdict{Accepted{tunnel_ip,tun_ip}, Rejected{why}}`（`why ∈ {MacMismatch, EngineRejected}`）；**删除 `Challenge` 变体**（nonce/pending 全在出口面） | 单测：MAC 错 ⇒ `why=MacMismatch`（出口行含 `hr-reg4 MAC 不符`）；ts 超窗 ⇒ `why=EngineRejected` **+ 表内 `peer: ! reject reason=no-token`**（r14 F1 的双面断言） |
| S1-8 | **已绑定连接的再准入门禁**（r14 F15）+ `by_conn`/`by_dev`/`by_pub` 三索引一致性断言 | 单测：绑定后再发 `H4/P4` ⇒ 拒 + 关连接；三索引在 bind/unbind/替换后逐次一致；**一条连接只对应一个 dev** |

### S2 客户端岛接线（四帧 + 刷新帧 + 预算）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S2-1 | `client/register.rs`：写 Hello → 等 Challenge（预算内）→ 写 Proof → 等 Accept；`REG_SETTLE` 退役；行文改写 + `准入完成` 行 | 单测：四帧序（真回环连接）；**失败面**（出口拒/超时）⇒ `IslandErr::RegistrationFailed`；`start_paused` 虚拟时钟用例（M0 flake 口径②） |
| S2-2 | 刷新帧 `R4`（60s）替换 M1 的 `hr-reg3` 刷新；C15' 行**不变**；**刷新成功不重绑、不打 E-q2**（r14 F11）；刷新前置三道（已绑定 / 表内仍在册 / MAC） | 单测：刷新帧域标签正确；服务端拒「未绑定连接的刷新帧」；**已淘汰设备（表内无）的刷新帧被拒且不 resurrect**（r14 F25）；刷新路径**零 E-q2 行** |
| S2-3 | 预算：准入段**自带期限** + `max(剩余, ADMIT_MIN=2s)` 规则（§1.7 / r14 F8）；`Connect` 语义不变 | 集成：注入 300ms RTT 代理 ⇒ ≥3 轮成功；`ADMIT_MIN` 形态 ⇒ 归因正确**且连接被显式 close**（不留悬挂） |
| S2-4 | `check-quic-isolation.sh` 的 `ASYNC_FILES` 更新（新文件显式入清单；**`exit/admit.rs` 定死为纯 std、不入清单**，r14 F26） | 隔离门九条全绿（含**双向负例自检**：把 `exit/admit.rs` 挪进清单应确定性红，反之亦然） |
| S2-5 | 岛/出口快照暴露 `send_buffer_used`（M1 交下项 N8①，§9.1.1） | `quic` 段新增一键（additive）；单测：发送缓冲被占后读数增长 |

### S3 抗放大与限流（Retry + 闸）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S3-1 | `exit/admit.rs`（**纯 std**）：每源**滑动窗**闸（O(1) 摊还结构；v4 /32、v6 /64）+ 触发判定（三条，按「未完成/被拒」计数） | 单测：闸边界（第 F+1 次起拒）、窗口推进、索引淘汰、v6 前缀聚合、**正常完成的尝试不计数**（r14 F10） |
| S3-2 | 主循环决策点：先闸后 Retry；`!validated() && may_retry()` 守卫；`RetryError::into_incoming()`（r14 F19）；`refuse()` 语义 | 集成（真回环）：**压力臂**（注入多源未完成尝试）⇒ `retry_sent > 0` 且地址被验证（§0.3 R1 形态）；**正常臂** ⇒ 0 次 Retry（r14 F10）；`retry_token_lifetime=5s` 生效（探针：5s 后同一 token 失效 ⇒ 需重做 Retry） |
| S3-3 | 洪泛判据（§3.3 的 1–6） | 洪泛注入脚本（`tools/` 侧，独立 workspace 形态照 `tools/m1-ab`）：逐条留证（含「已采纳连接吞吐下降 ≤10%」「常态不被误伤」） |
| S3-4 | 配置段 `serve.quic_admit`（六键 + env；值域/越界处置见 §3.2）+ 模板键表 | 单测：缺省不改行为；显式配置生效；**值域非法 ⇒ 拒启**；env 非法 ⇒ 记行 + 缺省；`retry_policy=always` 记行告警 |

### S4 判据行登记（与代码同批）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S4-1 | §6.2 的 6 条 + 计数输入集 3 行粘进 `docs/INTEROP-CRITERIA.md`（token 条按拍板结果二选一） | 字段四列齐全；`git diff` 与代码同批 commit |
| S4-2 | 逐行「从→到」在代码里逐条 grep 命中（M1 的 D4 正向核对法）；反向（做了没登记）自查 | 命中率 100%；无未登记行 |
| S4-3 | `tools/check-vocab.sh` 复核 + `_wg` 档逐字节回退复跑（`tools/quic-wg-e2e.sh`） | PASS；`_wg` 档 token 逐字节 + C 族原串 + `quic:` 族零输出 |

### S5 门槛与洪泛实测（独占机器）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S5-1 | `tools/quic-ab.sh` 四子命令复跑 | CPU/overhead/size/mem 不越出 M1 基线 ±10%（体积按 §8 预期 ≤+32KB） |
| S5-2 | 准入路径微基准 + 冷启动预算（300ms RTT 代理） | 登记读数；预算判据过 |
| S5-3 | 洪泛与限流实测（§3.3 五条） | 逐条留证（含「不伤既有连接」） |
| S5-4 | 真机复验（用 `docs/DEVICE-TEST-OHOS.md`；**先问用户**） | §7 的「能验」项逐条留证（含 §9.1-1 的源校验拒复看） |
| S5-5 | 窄路径 `tools/` 级注入（M1 交下项 N5，§9.1.1） | `tools/m1-ab-e2e.sh` 加一条 `HOMEWAY_QUIC_MTU` 注入断言：`窄路径不可用` 行 + `超限` 计数可见（**不承诺真 `mds<1280`**，M1 已登记区间内产不出） |

### S6 代码门（第二道门）

| 项 | 动作 | 完成判据 |
|---|---|---|
| S6-1 | dsh 代码门评审（commit 范围 = S1–S5）+ 逐条处置 | 记录入 `docs/reviews/M2.md`（原文摘要 + 处置表 + 高危必改/豁免） |
| S6-2 | 威胁模型**逐条验证**（§5 的 11 条：每条给出代码位置 + 测试/实测证据） | 逐条表（`M2.md`）；无可验证证据的条目降级为「登记」并写明 |
| S6-3 | **新增/改写判据行清单**交主会话转 tier（log-index 重生成） | 清单落 `docs/reviews/M2.md`（§9.1-2 的转交义务） |
| S6-4 | 专项 grep（**r14 F28 订正**）：①`reg4` 解析器**拒 `H3`（负例用例）**（**不是**"全仓 `H3` 零命中"——版本互斥负例必须在测试里构造 `H3` 字节，照 `reg3.rs:174-176` 对 `H2` 的写法）；②**产品代码内** `H3` 只出现在拒绝名单常量（剥测试后断言）；③`REG_SETTLE` 零命中；④新行族与快照同源；⑤`relay/**`+`relaywire.rs` 零 diff | 脚本/门断言（每条的判定式写死） |

---

## 11. 设计门记录（dsh）

### 11.0 轮次目录与指路

- **轮次目录**：`/tmp/dsh-review/r14.JxDBI4/`（`prompt.txt` / `output.md` **203 行** / `stderr.log`）；本轮**无子轮**。
- prompt 指路（不喂结论）：本文件 + `docs/QUIC-ROADMAP.md`（M2 节 + 用户触点 + 门槛表 + 附录 D）
  + `docs/reviews/M1-design.md`（§1.3/§12/§12.6/§12.7）+ `docs/reviews/M1.md`（§3.6/§6）
  + `docs/INTEROP-CRITERIA.md`（E6–E9/E18 + 变更记录 + 计数输入集）+ `docs/QUIC-BASELINE.md`
  + `docs/reviews/M1-S5-evidence.md` + `AGENTS.md` + 代码入口（`crates/homeway-quic/src/{reg3,cmd,driver,config}.rs`、
  `client/**`、`exit/**`、`crates/homeway-core/src/{server/{table,engine,device,state}.rs,token.rs,wtransport/reg.rs,facade/{tun_exec,tun_status}.rs}`、
  `crates/homeway-cli/src/serve_cli.rs`、`tools/check-quic-isolation.sh`）+ 上游 quinn/quinn-proto 源码 + 本棒探针 `/tmp/m2-probe`。
- 评审重点（**安全专项**）：RPK / 客户端证明 / nonce 与重放 / 抗放大 / token 格式 / 威胁模型 +
  路线文件评审协议 checklist + 「看过没问题的方面也明确说明」。
- **仓内副作用文件检查**：评审前后 `git status --porcelain` 均只为两份**未跟踪**的本棒产物
  （`docs/DEVICE-TEST-OHOS.md`、`docs/reviews/M2-design.md`）+ 探针在 `/tmp` ⇒ **无 dsh 副作用文件**（无需转存/删除）。

### 11.1 结论与统计

- **dsh exit code = 0**（成败只认 exit code）。
- 意见 **30 条**：**高 5**（F1–F5，**全部被评审标"建议阻塞"**）/ **中 13**（F6–F16、F28、F29）/ **低 12**（F17–F27、F30）。
  分类口径（评审自定）= 证伪 5 / 遗漏 11 / 措辞与一致性 14（其中 F28/F29 属「判据不可测」）。
- **逐条处置：认同 30 / 部分认同 0 / 不认同 0**（**无一条驳回**；每条都回源码/上游复核后成立）。
  处置形态分布：**改设计 26 条**、**补登记 4 条**（F4/F11/F14/F21）、**新增切片/判据 6 条**
  （F3→S1-6、F7→S1-7、F15→S1-8、F10→§3.3-6、F13→§9.1.1/S5-5/S2-5、F25→S2-2）。
- **评审独立完成的复核**（本门最硬的部分）：`table.rs` 八条语义逐条命中；`match_reg3` 唯一调用面；
  `race.rs` 的预算只用于赛跑循环（v1 §1.7 现状写错）；`engine.rs:2724-2741` 的 M1 用例直接证伪
  v1 的 E9 论断；`quinn-proto/src/token.rs:141-170` 证伪 Retry token 一次性语义；`keep_alive`+`idle`
  组合的可达性分析；`relay/rltoken.rs` 复用 `token::encode` 与 `tools/**` 的 `hmw1` 抽取面 ≥10 处；
  `fixtures/vectors/token.json` 的 `hmw2` 哨兵；`hosts.json` 存原始 token 串；4 处行号漂移。
- **门结论：通过（v1 → v2，带残余）**。评审给的是「**未过**：4 项高危（建议阻塞）+ 2 项高危待裁决（token 拍板项）」；
  5 条高危已**全部落到 v2**（F1/F2/F3/F4 改设计，F5 改为**带前置条件的拍板项**），
  13 条中危与 12 条低危亦全部落纸 ⇒ 升 v2 判**通过**；§11.5 登记残余与不做项。

### 11.2 评审意见原文摘要（逐条）

| # | 评审意见（摘要） | 严重度 |
|---|---|---|
| **F1** | `no-token` 在 QUIC 档「输入集为空」**错**：`verify()` 把 `Expired` 归 `NoToken` ⇒ MAC 过但 ts 超窗的 Proof 必打 E9 `no-token`（M1 用例已断言） | **高（阻塞）** |
| **F2** | **Retry token 无一次性语义**（无状态 AEAD，只查地址 + 15s 窗）⇒ 「每次连接都要 Retry 才拿到地址验证」不成立；真实界是每源闸 | **高（阻塞）** |
| **F3** | **「握手完成但从不发 Hello」的连接永不回收**（`keep_alive=10s` + `idle=30s`；握手闸只管 `Connecting`）⇒ 64 条即可占满连接槽 | **高（阻塞）** |
| **F4** | 引擎侧 `hr-reg3 MAC 不符` 是**已登记判据行**，换版本必改但 §6.2 漏登记，§6.1 还称「一行不改」 | **高（阻塞）** |
| **F5** | 候选 B 与「`_wg` 档 token 逐字节同」硬判据**互斥**，且**静默改掉 `rl1` 中继 token**（`rltoken` 复用 `token::encode`）；另列 4 类连锁（向量哨兵/`hosts.json`/`tools/**` 抽取/过度列出消费面） | **高（阻塞，拍板前置）** |
| F6 | 「发挑战前的廉价决策点」与机制不符（Hello 也在握手之后） | 中 |
| F7 | 出口 `why=hr-reg4 MAC 不符` 按 S1-3 接口不可达（试秘在引擎，verdict 无原因）；`Challenge` 变体没必要 | 中 |
| F8 | §1.7 现状前提不成立：今天 `QUIC_CONNECT_BUDGET` **不覆盖登记**（`register_on_control_stream` 无超时） | 中 |
| F9 | §3.3-1 的算式与 §3.2-④ 的窗不自洽（窗语义未定 ⇒ 判据按字面不可满足） | 中 |
| F10 | 压力触发 Retry 的②落在**正常赛跑包络**内（候选并发 ⇒ 同源 ≥3 次是常态） | 中 |
| F11 | 刷新帧落点未定：M1 每次 `Accepted` 都 `bridge.bind` ⇒ 每 60s 重打 E-q2 | 中 |
| F12 | Proof 失败闸把「引擎裁决拒绝」也算失败 ⇒ 表满/吊销时合法设备被自伤冷却 | 中 |
| F13 | M1 明文交下的 M2 面条目（N2/N4/N5/N8）**全篇缺席** | 中 |
| F14 | `serve.quic_admit` 值域/纪律未定，且与 Q-H 严格表（拒启）是两套纪律 | 中 |
| F15 | 「连接 = 设备」不变量未闭环：已绑定连接可再发 Hello→Proof（`bind` 覆盖 `by_conn`，旧索引残留） | 中 |
| F16 | 候选 B 的「未知段可跳过」对**凭证**是 fail-open（未来限制性段被静默忽略） | 中 |
| F17 | §3.1「Retry 包只回给能收到的对端」措辞不实（Retry 发往声称源地址） | 低 |
| F18 | §0.5「`refuse()` 不是静默丢」不完整（短包/端点饱和在更早静默丢） | 低 |
| F19 | `retry()` 的 `Err` 语义注解错，且丢错误值会连 drop `Incoming` ⇒ 隐式 `refuse()` | 低 |
| F20 | §0.2 行号 4 处漂移（`race.rs`/`incoming.rs`/`config`/`Cargo.toml`）+ `EndpointStats` 字段名 | 低 |
| F21 | §9.1-1 的「自然消失」预测不成立；且与「仅 1 处行文改写」自相矛盾 | 低 |
| F22 | §1.3「Hello 重发 ≤3（首包丢失自愈）」理由不成立（可靠有序流） | 低 |
| F23 | 威胁模型缺三行（强制回落 WG / 闸表挤兑 / 单连接多设备）+ 两条证据锚 | 低 |
| F24 | §2.2-1 的「E6/E7 族零新增行」措辞（E6 是启动期一次性行） | 低 |
| F25 | 刷新帧跨过淘汰会把已淘汰设备重新 Add | 低 |
| F26 | `exit/admit.rs` 的隔离门归属留问号 | 低 |
| F27 | §8 线开销数字偏乐观（Retry ≈100B） | 低 |
| F28 | S6-4 专项 grep「`H3` 零命中」按字面**不可满足**（负例必须构造 `H3`） | 中 |
| F29 | S1-5 的 `git diff` 判据过严（doc/import 必然变） | 中 |
| F30 | §3.2-④ 的 LRU 表未定结构（扫描式淘汰 = CPU 放大器） | 低 |

### 11.3 逐条处置表

| # | 处置 | 落到 v2 的位置 / 证据 |
|---|---|---|
| F1 | **认同 → 改设计**：E9 登记改写为「QUIC 档 `no-token` 输入集 = {MAC 已过、ts 超 ±90s 窗}」；§1.4-4.3 写明「窗超走 `EngineRejected` + 表内 `no-token` 双面」；S1-7 加该双面断言 | §2.3 E9 行、§1.4-4.3、§6 计数输入集②、S1-7 |
| F2 | **认同 → 改设计**：§0.5 该行订正（无一次性语义）；§3.1 增「真实界 = 每源闸」；**动作 = `retry_token_lifetime` 收 5s**（新配置项）；S3-2 加「5s 后同一 token 需重做」用例 | §0.5、§3.1、§3.2 配置表、S3-2 |
| F3 | **认同 → 改设计 + 新切片**：新增 `ADMIT_DEADLINE=10s`（连接建立起算，包住等首帧 + 状态机）+ S1-6 判据 | §1.3 表首行、§3.2-⑤、§5-6、S1-6 |
| F4 | **认同 → 补登记 + 改分类**：§6.2 新增「引擎侧归因行协议版本串」条；§6.1 改为「3 处行文改写」；`why` 增 `帧版本不符（H2/H3）` | §6.1、§6.2、§1.6 |
| F5 | **认同 → 拍板项加前置**：新增 §4.2.1（五类连锁逐条回源）；§4.4 增「`_wg` 不变量重述」条；§12-① 改为**带前置条件**的拍板项；§4.4 去掉过度的消费面 | §4.2.1、§4.3、§4.4、§12-① |
| F6 | **认同 → 改措辞**：新增价值改为「MAC 试秘之前 + 不跨线程投引擎 + 不触碰设备表」 | §1.1 理由 1 |
| F7 | **认同 → 改接口**：`Reg4Verdict{Accepted, Rejected{why}}`（类型化），删 `Challenge` 变体；S1-7 | §1.4、S1-7、§1.6 |
| F8 | **认同 → 改现状 + 定规则**：现状改为「登记段今天无预算」；新增 `max(剩余, ADMIT_MIN=2s)` + 「失败/超时必须显式关连接」 | §1.7、S2-3 |
| F9 | **认同 → 改判据**：单窗内断言（第 F+1 次起拒）+ 窗语义/是否计被拒必须在实现期写死并单测 | §3.3-1、S3-1 |
| F10 | **认同 → 改触发 + 新增判据**：②改为「同源**未完成/被拒**次数」，常态赛跑不计数；§3.3-6 新增「≥3 候选正常赛跑 ⇒ `retry_sent=0`」 | §3.1、§3.2-④、§3.3-6、S3-2 |
| F11 | **认同 → 改设计 + 补登记**：刷新成功不重绑、不打 E-q2；§6.2 新增 E-q2 频率条；S2-2 加「刷新路径零 E-q2 行」 | §1.8、§6.1、§6.2、S2-2 |
| F12 | **认同 → 改闸定义**：⑥只计 nonce/MAC 类失败（排除引擎拒绝）；判据加「表满压测下合法 devTag 不被冷却」 | §3.2-⑥、S3-1 邻域 |
| F13 | **认同 → 补章节**：新增 §9.1.1（N2/N4/N5/N8 逐条处置）+ S2-5（`send_buffer_used`）+ S5-5（窄路径 tools 级注入） | §9.1.1、S2-5、S5-5 |
| F14 | **认同 → 定死值域**：`serve.quic_admit` 六键表（值域/缺省/越界 = **拒启**）+ env 纪律（记行 + 缺省）+ `retry_policy=always` 代价告警；§5-11 补 admit 误用 | §3.2 配置表、§5-11、§6.2、S3-4 |
| F15 | **认同 → 加门禁 + 新切片**：已绑定连接拒 `H4/P4`；S1-8 加三索引一致性断言 | §1.3 表、§1.4 步骤 2、§2.4、S1-8 |
| F16 | **认同 → 段分类**：`info`（可跳过）vs `critical`（必须理解否则拒）写进候选 B | §4.2、§4.3 |
| F17 | **认同 → 改措辞** | §3.1 末条 |
| F18 | **认同 → 补注记**：短包/饱和静默丢 + `refused_handshakes` 只计应用层 | §0.5、§3.1 观测条 |
| F19 | **认同 → 写实现纪律**：`!validated() && may_retry()` 守卫 + `into_incoming()` | §0.5、§3.1 实现要点、S3-2 |
| F20 | **认同 → 订正 4 处行号 + 字段名** | §0.2、§0.3 R4、§0.5 |
| F21 | **认同 → 删预测 + 统一口径**：去掉「自然消失」预测；E-q3 明细增强标为**可选登记条**（登记面 = 四字段计数行不变） | §9.1-1、§6.1、§6.2 |
| F22 | **认同 → 删分支**：一连接一 Hello；第二个 Hello ⇒ 拒；删「挑战 %d 次」字段 | §1.3 表、§1.6 |
| F23 | **认同 → 补三行**：强制回落 WG（#12）/闸表挤兑（#13）/单连接多设备（#14）+ 两条证据锚（#2/#3） | §5-2/3/12/13/14 |
| F24 | **认同 → 改措辞**（断言面改 `table.len()`/`reject_counts`/E7 计数/peers） | §2.2-1 |
| F25 | **认同 → 加前置**：刷新分支必须 `device_addrs(dev).is_some()`；S2-2 加「不 resurrect」用例 | §1.4 步骤 6、S2-2 |
| F26 | **认同 → 定死**：`exit/admit.rs` 纯 std、不入 `ASYNC_FILES`；保留双向负例自检 | §3.2 末、S2-4 |
| F27 | **认同 → 订正数字**：≈120–150B/次建连（含 Retry ≈100B） | §3.4、§8 |
| F28 | **认同 → 改判据**：`reg4` 拒 `H3` 的负例用例 + 「产品代码内 `H3` 只出现在拒绝名单常量」 | S6-4 |
| F29 | **认同 → 放宽**：`git diff` 判据含 doc/import 与其测试 | S1-5 |
| F30 | **认同 → 定结构**：插入序 `VecDeque` + `HashMap` 索引（O(1) 摊还） | §3.2-④、S3-1 |

### 11.4 不认同项（含证据）

**无。** 30 条逐条回源码/上游复核后**全部成立**；其中 5 条高危的复核证据见 §11.1 的「评审独立完成的复核」。
**两处「认同问题但处置与评审建议不同」**（非不认同，理由登记）：
① F5 的第 6 点（评审指出 §4.4 过度列出 `state.rs`/`serve_cli.rs`）——**采纳**，但**保留**它们在
「tier/台账不影响」说明里的提及（台账是字段式、渲染走 `encode`，B 下无需改）；
② F21 的「明细属自由文本、无需登记条」——**部分保留**：本设计仍给一条**可选登记**（理由 =
M1 对 N-c 明细口径做过「文案订正」补登，口径一致性优先）。

### 11.5 门后残余与不做项（防静默漏做）

| 项 | 结论 | 理由 / 承接 |
|---|---|---|
| token 候选 B 的**前置条件**（§4.2.1 a/b/c） | **未落纸前不进入 B** | 拍板项已改写为带前置（§12-①）；若前置不落纸 ⇒ 退回候选 A |
| `retry_token_lifetime=5s` 的真机影响（重连是否变慢） | **待实测** | §3.1 登记；配置可调 ⇒ 实测后定 |
| 单地址 Retry 成果的复用窗（5s 内） | **残余**（设计上的界 = 每源闸） | §3.1/§5-5；若要彻底关需 bloom 依赖（M6/M7 候选） |
| 分布式（多 IP）洪泛 | **残余**（只能靠全局闸） | §5-5；缓解 = 已采纳连接不受影响 |
| 握手 CPU 在 64 并发内可被吃满 | **残余** | §5-6 |
| 吊销 → 在线连接的主动拆（最晚 60s） | **残余（今日同档）** | §2.4；M5/M7 若要"秒级拆"再加 control-plane 钩子 |
| M1 的 A4（黑洞期已入缓冲无逐包计数） | **部分关闭**（暴露 `send_buffer_used`） | §9.1.1 N8①；逐包计数仍缺 ⇒ 登记 |
| M1 交下的 N2（出口腿表行）/ N4（同前缀） | **转交 M7 / 不改** | §9.1.1 |
| 真机面（Retry 通过率、+1 RTT 真值、`源校验拒` 复看） | **未验（不得宣称已验证）** | §7 / §9.1-1 / M6 |
| dsh 副作用文件 | **无**（评审前后 `git status` 只有两份未跟踪文档） | §11.0 末条 |

### 11.6 覆盖度声明（评审自报）

- **本轮覆盖**（评审独立复核面）：§0.2 接缝表（抽查 `reg3.rs`/`exit/{conn,mod}.rs`/`table.rs`/`token.rs`/
  `state.rs`/`client/register.rs`/`serve_cli.rs:1031` **全部命中**；4 处漂移另计）、§0.5 上游 API 事实
  （逐条回 `quinn-0.11.12`/`quinn-proto-0.11.19` 源码）、§1 全节（四帧算术/域分隔/exporter 标签/nonce/
  pending/职责切分/失败面）、§2.1 八条语义、§2.3 E6/E7/E18、§3 全节（含 Retry 的取反）、§4 全节
  （含 `rl1`/向量/`hosts.json`/`tools/**` 五类连锁）、§5 逐条、§6/§8/§10 的可测性抽查。
- **评审明确点名「看过、没问题」的 21 项**（摘要）：四帧长度与字段宽度、域分隔无碰撞
  （`hr-reg4` 是 `hr-reg4-refresh` 的前缀但两输入总长不同 ⇒ 无 HMAC 碰撞）、exporter 标签沿用、
  nonce 长度/来源与常量时间比较、`hr-reg3`→`hr-reg4` 替换的取舍、`ts` 与 ±90s 窗的承载关系、
  nonce TTL 与预算的算术、pending 的任务内局部形态、§1.4「不触碰设备表」的结构性成立、
  `table.rs` 零改动、E6/E7/E18 保留原串、E8 的输入集登记、反放大的结构面（Challenge/Accept 只在握手后）、
  Retry/refuse 走自定义 socket、0-RTT 不启用（全仓零 `early_data`/`into_0rtt`/`session_storage`
  ⇒ 顺带**无 TLS 会话恢复** = exporter 恒出自完整握手）、隔离门合规、中继零字节改动、收工/生命周期、
  `_wg` 档回归（token 面除外）、预算数量级。
- **评审未能覆盖**：真机/中间盒（Retry 通过率、+1 RTT 真值）、真机验证计划本身与
  `docs/DEVICE-TEST-OHOS.md` 的内容、tier 侧（JSON 键消费 / log-index 流程）、上游补丁版漂移、
  §8 的实测数字、`fixtures/` 生成管线的完整依赖图。

---

## 12. 待用户拍板项

| # | 拍板点 | 候选 | 推荐 | 依据 |
|---|---|---|---|---|
| ① | **token 格式**（路线文件明列；**r14 F5 后加前置条件**） | **A** 继续 additive（本批不动 token）/ **B** 重排为版本化容器（`hmw2` + 段容器）/ **C** 派生凭证（grant/按用途派生） | **B（但先落三条前置，否则退回 A）** | §4.3：现在是无兼容包袱的唯一窗口；M2 本身不需要新字段（不是"必须改"）；B 不排斥 C。**前置（§4.2.1）**：(a) 把「`_wg` 档 token 逐字节同」重述为「`_wg` 档 token 不含 QUIC/RPK 内容」并登记；或 (b) WG-only 保留 `hmw1`（只 QUIC 档升 `hmw2`）；(c) 明确 `rl1` 处置（**推荐 = body 冻结在 `hmw1` 布局不随动**）。**这三条不落纸 ⇒ 选 A**（本设计两候选都能实施） |
| ② | **0-RTT 是否启用**（路线文件明列，M2/M3） | 启用 / **不启用** | **不启用** | §5-10：0-RTT 数据可在准入前到达，与「准入先于数据」冲突；RFC 9000 §9.2 明确可重放；且**全仓当前零 `early_data`/`into_0rtt`/`session_storage`**（r14 复核 = 事实而非承诺） |
| ③ | **压力触发 Retry 的阈值与 `retry_token_lifetime`** | 本文初值（未认证在途 ≥32 / 同源「未完成」≥5 per 10s / `retry_token_lifetime=5s`）/ 恒 Retry / 更宽松（只在闸拒绝后） | **采用初值**（全部可配） | §3.1/§Q-M2-2：初值无实测依据 ⇒ 实现期标定 + 配置可调；恒 Retry 会给常态重连加 1 RTT（真机可感）；**r14 F2 已证**「一次 Retry 的成果在窗内可复用」⇒ 窗必须收（15s → 5s） |

---

## 13. 实施期订正（主会话登记，2026-10-09；**后到的切片以本节为准**）

> 纪律依据：`docs/QUIC-ROADMAP.md`「每期执行协议」第 6 条（实测/源码与设计矛盾 ⇒ 先在设计文档登记再改）。

1. **§12 三项拍板的裁定（用户未在会话中回应 ⇒ 按设计的安全默认实施，不改任何"用户触点"面）**：
   - **① token 格式 = 候选 A（本批不动 token）**。理由：B 是**换地基**（含 `rl1` 中继 token、向量哨兵、`hosts.json` 等连锁面，见 §4.2.1），而它在路线文件里是**用户拍板点**；未获拍板即选"不改"这一侧（设计原文的兜底规则亦如此）。**B 的窗口未被关闭**——若用户后续拍板 B，按 §4.2.1 的三条前置执行即可。
   - **② 0-RTT = 不启用**（与设计推荐一致；全仓零 `early_data`/`into_0rtt`/`session_storage` 的事实不变）。
   - **③ 抗放大初值 = 采用本文初值**（未认证在途 ≥32 / 同源「未完成」≥5 per 10s / `retry_token_lifetime=5s`；全部可配，实现期标定）。
   - **待用户确认项**：以上三条（尤其①）如需改判，改动面已由 §4.2.1 界定。
2. **§10 S1-4「+6 字段」的口径订正**：以 **§1.6 的列表（5 字段）** 为准；`retry_sent`/`flood_refused` 的**生产者属 S3**，S1 不落无生产者的空字段（S1 实现按此落地，已登记）。
3. **S1/S2 边界的实施期调整（已登记）**：**客户端岛侧的四帧协议实现提前到 S1**——帧协议替换后客户端不同协议即编译不过、`cargo test --workspace` 不绿、四帧 e2e 走不通，属「影响面小就地解决」。**S2 余下** = 准入预算（`max(剩余, ADMIT_MIN)`）、失败/超时显式 close、`start_paused` 用例、刷新前置「表内仍在册」（不 resurrect）、`send_buffer_used` 暴露。
4. **`why` 归因取值集扩展 4 串**（实现在 §1.6 的集合上新增：`帧格式非法（首帧必须是 Hello）`/`重复 Hello`/`刷新帧与绑定身份不符`/`连接未绑定`）——S4 登记须覆盖（fail-visible 拆细，非新增语义）。
5. **「在途未认证 n/cap」的分母 = `conn_cap`（`2×max_devices`）**——设计只写 `%d/%d`，此为实施期选择（登记备查）。
6. **flake 补登**：`wtransport::bind::tests::unknown_source_hint_filtered`（全量并行跑红 1 次、隔离复跑 4/4 绿、该文件本批零 diff）⇒ 归入已登记的 `wtransport::bind::tests` 实 socket 时序族；主会话同批写入 `docs/QUIC-ROADMAP.md` 的「已知 flake 登记」。
