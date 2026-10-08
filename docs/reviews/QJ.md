# Q-J 批记录：通用性批（keyenc 平台口径 / DNS·探针配置化 / UPnP 协议面 / `if_nametoindex` / launchd 精确化）

> 批次 = **Q-J**（`docs/REVIEW-ROADMAP.md`「Q-J 通用性批」；Q 批整改的**最后一个整改批**）。
> 实现规格 = `docs/reviews/QJ-design.md`（v2，674 行；设计门 dsh `r23.yzpO66` exit=0——评审
> 全部认同或部分认同并入 v2，0 不认同、0 误报）。
> 本文件 = 实现棒记录（F1–F7 实现清单 + 测试/判据证据 + 代码门 r24 + 逐条处置 + 不做项/上报项）。
> 实现基线 = HEAD `e3b1d8b`（Q-I 收口）。
> 执行协议 = `REVIEW-ROADMAP.md`「每批执行协议」3→5 步；判据政策 = `INTEROP-CRITERIA.md`「判据变更记录」。

---

## 0. 工作树预检（开工第一步：dsh 产物核查）

- 开工时 `git status --porcelain` = 仅 `?? docs/reviews/QJ-design.md`（设计棒产物）——
  设计棒报告所述「dsh 评审曾在仓内自建 `docs/reviews/QJ-design-review.md`、已转存 `/tmp` 并删除」
  **核实属实**：该文件不在工作树。
- 代码门（r24）跑完后复查 `git status`：**无新增仓内副作用文件**（本轮 dsh 未在仓内写文件），
  改动集合 = 本批 14 个源码/测试文件 + `docs/INTEROP-CRITERIA.md`（判据登记）+ 批文档。

---

## 1. 实现清单（F1–F7；文件/函数级）

### F1（P1｜wire 面）keyenc 平台口径字段化：客户端 HELLO 声明，缺省 = 宿主推断

| 面 | 改动 |
|---|---|
| wire | `term/frames.rs::caps` 新增互斥两位 `KEY_ALT_NO_ESC_PREFIX = 1<<2`（darwin 分支语义）/ `KEY_ALT_ESC_PREFIX = 1<<3`（非 darwin 分支语义）；**尾随块形状/`PROTO_VER` 门零变化**（`enc_hello_tail`/`dec_hello_tail` 体未动） |
| 编码器 | `term/keyenc.rs`：删 `IS_DARWIN`，新增 `enum KeyFlavor { AltEscPrefix, AltNoEscPrefix }` + `KeyFlavor::host_default()`（**唯一** `cfg!(target_os)` 残留点 = 缺省兼容推断）；`encode_key`/`legacy_encode`/`kitty_encode`/`legacy_alt_prefix` **必收** flavor（无默认参数 ⇒ 编译期强制传递）；4 处原 `IS_DARWIN` 落点（alt 前缀 / mok2 剥 alt 位 / super 抑制文本 / kitty 关联文本）全改 flavor 判定；模块头注释与 5 处行内注释同步 |
| 服务端 | `term/service.rs`：`key_flavor_from_caps()`（**语义级判定放 `serve_hello`**——`dec_hello_tail` 保持纯形状契约、不新增 `FrameError` 变体）；`LegRt.key_flavor`（唯一构造点 = 腿入表）；`handle_input` 按 `LegKey` 取本腿口径，**查不到腿（`end_leg` 竞态）= 丢弃 + 计数 + 首/每 100 次记行，任何路径不回落 `host_default()`**；歧义（两位同置，含裸 ID 形态 `caps=0x7F`）= **fail-soft：按未声明处理 + 计数 + 一次性告警，绝不拒腿** |
| 会话壳 | `term/vt.rs::SessionVt::encode_key(ev, flavor)` 必收参 |

### F2（P1｜配置化）DNS 上游 / 探针目标配置化（默认不变）

| 面 | 改动 |
|---|---|
| 配置键 | `homeway-cli/serve_cli.rs`：`[serve]` 新增 `dns_upstream` / `dns_fallback` / `ddns_resolver` / `dns_probe_target` / `stun_probe_target`（`FileServe` 字段 + `validate_file` 值域 + `serve_config_of` 边界解析成型）；**空数组 = 取默认**；`dns_fallback` **空串 = 拒启**；**端口 0 五键一律拒启**（代码门 L2）；非法即拒启 + 可行动文案（Q-H F1 形态）；**不加 CLI flag**（登记「不加」） |
| 配置模板 | `nodestate.rs::DEFAULT_CONFIG_TOML` 注释键表补五键（含「`dns_probe_target` 一键喂三路」与「`stun_probe_target` 须带端口」注记） |
| 承载 | `engine.rs::ServeConfig` 五字段（默认 = 修前硬编值：`223.5.5.5` / `DDNS_RESOLVERS` 投影 / `default_probe_targets()` / `default_stun_targets()` / 空上游） |
| 穿参 | `dnsproxy.rs`：`DnsConfig.upstreams` + `Upstreams::with_static`（**非空时静态返回、不做 mtime 跟随**；空 = 今日跟随语义逐字不变）+ `DnsProxy::spawn` 的**覆盖生效一次性告警**（代码门 M1）；`ddnscheck.rs`：`run_ddns_self_check`/`resolve_ddns` 收 `resolvers: &[SocketAddr]`（空 = 默认常量表）；`egress.rs`：`preferred_iface(route_probe)`（路由探针 = 配置目标首项，默认等价 `223.5.5.5:53`）；`bindwatch.rs`：`WatcherArgs.pick_targets`（挑卡恒吃 config）+ `health_probe_targets_from_env(cfg)`（env 缝只喂健康探针）；`engine.rs`：`select_best` 两处、udpcap `probe_once(dns_targets, stun_targets)`、`TokenCtx.ddns_resolvers` 全部接线 |

### F3（P1｜观测统一）fake-IP 卫兵统一（**不改应答**）

| 面 | 改动 |
|---|---|
| 单源 | `is_fake_ip` 从 `ddnscheck.rs` **迁入 `egress.rs`**（`pub(crate)`；与 `is_public_addr` 同域），ddnscheck / dnsproxy 两处各留「单一实现」断言 |
| 观测 | `dnsproxy.rs::ResponderCore::respond` 在 **`clamp_ttl`/`truncate` 之后**（= **交付**的应答）扫答案段 A 记录：命中 ⇒ `DnsStats.fakeip += 1`（每应答 ≤+1）+ **每进程一次性告警**（`fakeip_once`）；新增纯函数 `has_fake_a` |
| 语义边界 | **不改应答一个字节**（tier `wg-native-dns:50` MUST）；fake-IP **不触发**换上游/兜底；告警文案写明「常态增长、不区分透传/污染、配 `dns_upstream` 后 `fakeip=0` 不代表干净」 |

### F4（P2｜协议面加固·超 Go）UPnP：IGD:2 / ST 兜底 / 应答判定表 / `AddAnyPortMapping` / 钉卡降级

| 面 | 改动 |
|---|---|
| SSDP | `upnp.rs`：`msearch_message(st)` 参数化 + 每轮固定三发（`SSDP_ST_IGD1`/`SSDP_ST_IGD2`/`SSDP_ST_ROOT`，`MX: 2` 不变）；`ssdp_location_with(local_ip, deadline, logf)` |
| 判定表 | 新增 `ssdp_is_igd_type`（ST 整串 / USN `::` 后段**精确相等** ⇒ `:1` 不误命中 `:10`）；`ssdp_location_with`：IGD 型 ⇒ 立即采信；非 IGD 型 ⇒ **待定，只在 SSDP 腿期限到点（或三重试耗尽）后回退**（不在各轮窗口尽头提前返回——不抢 MX=2 迟到应答、不吃 attempt 2/3） |
| 末位兜底 | `Igd::add_any_port_mapping`（`try_add_any_port_mapping`）：同控制 URL/service_type、args 与 `add_with_lease` 同序同 case、`NewExternalPort` = 期望端口（prefer 非 0 否则 internal）、**假成功判定**（`NewReservedPort` 可解析且 ≠0）、**租期 3600→0 回退**、成功 logf 一行 / 失败 dlogf 一行后回落 `NoPortAvailable`；**仅 `select_external_port` 末位触发，不进缩租路径** |
| 钉卡降级 | `pin_multicast_with`（可注入钉卡面）：③ 失败 ⇒ 记行（「发送已由 IP_MULTICAST_IF 钉在 <if>，但接收可能被默认路由/TUN 抢走……继续尝试」）+ **不终止候选**；`discover_igd_with` / `shrink_lease_before(logf)` 透传日志 |

### F5（P2｜平台纠偏）`if_nametoindex` 完整语义（含三处 index 键面）

| 面 | 改动 |
|---|---|
| 守卫分档 | `egress.rs::pin_socket_to_iface`：`index==0` 硬拒改 **darwin 专属**（`IP_BOUND_IF=0`=解绑）；linux 按名（`SO_BINDTODEVICE`，index 仅诊断）；**linux 空名补硬拒**（代码门 L1：内核 `optlen=0` 是「解绑且成功」，与 darwin 同型） |
| 候选/注释 | `IfaceInfo.index_ok` 文档平台条件句 + `interfaces()` 告警平台化；`physical_candidates` darwin 排除 `!index_ok`（`candidate_pinnable` 纯谓词）、linux 不过滤 |
| 三处键面 | `bindwatch::state_of_from(&[IfaceInfo], &IfaceInfo)`（纯函数；**name-only 回查**，找不到 = down + 空地址集）、`bindwatch` 重挑比较 `egress::iface_same`、`select_best` 默认路由偏好 `iface_same`；`IfaceFingerprint` 保留 index 字段（换 index ⇒ 指纹变 ⇒ 重钉信号不丢） |

### F6（P2｜Rust 独有语义精确化）launchd 探测：plist 内容精确匹配

| 面 | 改动 |
|---|---|
| 探测 | `daemon_cli.rs`：`detect_launchd_agent_for_state(state)` + `detect_in_dirs(dirs, state, default_state)`（目录/默认 state 可注入）；`parse_program_arguments`（**手写 XML 子集 + 白名单纪律**：二进制/自闭合 `<string/>`/CDATA/注释/嵌套 array/未定义实体 ⇒ 未知形态）+ `decode_xml_text`（五预定义实体）+ `plist_mentions_state`/`plist_mentions_any_state` + `normalize_state`（两侧共用：canonicalize 成功用其值，失败退化字面去尾 `/`） |
| 规则 | 提及该 state ⇒ 相关；零参 + 默认 state ⇒ 相关（Q-H F14 旧判据，**不引入 argv[0] 谓词**）；未知形态 ⇒ 保守退化（默认⇒相关/非默认⇒不相关）；**默认或非默认 + 只提别的 state ⇒ 不相关**（代码门 L3 补记）；多命中取首个 + 记行；Label 仍取文件名去后缀 |
| 文案 | 非相关分支 `（未在册 launchd 代理提及 state=…——不等 launchd KeepAlive，直接拉起）`（旧文案在「默认 state 但无相关 plist」时**说假话**）；**该行仅 darwin 打**（代码门 L4：Linux 旧形态此情形无输出）；等待分支（CA11 主文案）不变 |

### F7（P2｜盘点·文档）平台假设清单

§0.5 表即交付物（含设计门补的三行），见 `QJ-design.md` §0.5；本批处置口径复述见本文件 §5.3。

---

## 2. 测试与门证据

### 2.1 门（2026-10-08，实现棒复跑；代码门 dsh 独立复跑同值）

| 门 | 命令 | 结果 |
|---|---|---|
| 测试 | `cargo test --workspace` | **lib 642 passed / 0 failed / 4 ignored** + 其余目标全绿（E2E `qh_config_failfast` 5 passed，含本批新增 2 例） |
| 静态 | `cargo clippy --workspace --all-targets -- -D warnings` | **clean**（`-D` 置于 `--` 后；仓内惯例写法） |
| 交叉 | `cargo check --target x86_64-unknown-linux-musl --all-targets -p homeway-core -p homeway-cli`；`--target aarch64-unknown-linux-musl`；`--target aarch64-unknown-linux-ohos -p homeway-core -p homeway-cli -p homeway-capi` | **全过**（含 `#[cfg(target_os="linux")]` 测试面 = 防 Q-H H1 同型「linux 下 `--all-targets` 编译失败」；唯一告警 = 既有 `libc::time_t` deprecated，非本批） |
| 词表 | `zsh tools/check-vocab.sh` | **PASS**（5 单元/26 值；ledger sha256 一致） |

### 2.2 **F1 缺省逐字节同今日的证据**

1. **代码面**：`KeyFlavor::host_default()` 是唯一 `cfg!(target_os)` 点，取值与旧 `IS_DARWIN`
   逐字节同义（单测 `host_default_matches_legacy_is_darwin` 钉死两平台期望）；未声明 / 两位全不置 /
   歧义三条路径全部落 `host_default()`。
2. **向量面（387 案 / 16 模式）**：`keyenc_parity_with_go_vectors` **去掉 `#[cfg(target_os="macos")]`
   全平台跑**、显式传 `KeyFlavor::AltNoEscPrefix`（向量宿主 = darwin 口径）⇒ 逐字节全绿；
   即「darwin 形态口径」与夹具完全一致（缺省在 darwin 上 = 该口径）。
3. **分叉面**：`non_darwin_flavor_expectations` 8 案（源码行级真源 = vendored ghostty 四处
   darwin 门 + fixterms 无平台门）双向断言——同时钉 darwin 值与非 darwin 值，
   证明「分叉面 = 这 8 案、且 darwin 值 = 向量宿主形态」；**非 darwin 口径无夹具**已显式登记。
4. **端到端**：`alt_flavor_declared_per_leg`（两腿两种声明，raw 腿观察 PTY 字节）——
   声明 `KEY_ALT_ESC_PREFIX` ⇒ `\x1bZ`；声明 `KEY_ALT_NO_ESC_PREFIX` ⇒ 无 `\x1bZ`。
5. **fail-soft 回归钉**：`hello_caps_flavor_ambiguity_is_fail_soft` 五态（未声明/两单声明/同置/
   裸 ID `[4,'h','o','s','t']` ⇒ caps=0x7F）——全部拿到 ATTACHED（不拒腿）+ 计数 1、2 + 告警恰一行。

### 2.3 F2 缺省逐值同今日 / F3 透传 / F4 失败路径 / F5 / F6 证据（单测）

| 面 | 用例 |
|---|---|
| F2 缺省 | `f2_keys_defaults_unchanged`（五值与 `default_probe_targets()`/`default_stun_targets()`/`default_resolvers_v4()`/`223.5.5.5`/空上游逐值相等）；`defaults_unchanged_by_new_keys` |
| F2 生效 | `f2_keys_take_effect`（裸 ip 补 :53 / `ip:port` 原样 / 空列表 = 默认）；`static_upstream_override_no_follow`（覆盖生效 + resolv.conf 变更**不影响** + 无兜底）；`fallback_from_config`；`resolve_ddns_uses_injected_resolvers`（代码门 M3①）；`probe_once_uses_injected_targets`（代码门 M3②：本地 UDP 桩双答 DNS/STUN ⇒ flags 含 DNS:53+通用）；E2E `config_failfast_does_not_kill_unified` 新增 `dns_fallback=""` 与 `dns_probe_target=["1.2.3.4:0"]` 两例（代码门 M3③：rc=1 + 可行动文案 + 进程存活 + 文件逐字节未变） |
| F2 env 缝 | `pick_targets_ignore_env_seam`（代码门 M3④：**真注入** `HOMEWAY_BINDWATCH_PROBE`——健康探针取 env、挑卡目标不变） |
| F3 | `fakeip_counted_on_delivered_response_only`（逐字节透传断言 `resp == want` + 计数 +1/+1 + 告警恰一次 + 非 fake 不计数）；`fakeip_counts_only_surviving_records`（截断掉的不计）；`fakeip_guard_single_source` + `fake_ip_single_source`（ddnscheck 侧） |
| F4 | `msearch_shape_three_states`；`ssdp_igd_type_matching`（IGD:1/IGD:2/USN/`:10` 不误命中/缺 ST+USN）；`add_any_port_mapping_last_resort`（成功改派 + `AddAnyPortMapping:65535` 期望端口 + 假成功 ⇒ `NoPortAvailable` + 401 不支持 ⇒ `NoPortAvailable` + 租期 `[3600, 0]` 回退 + 观测行）；`shrink_path_unaffected_by_any_mapping`；`multicast_pin_failure_degrades`（注入失败钉卡 ⇒ Ok + 告警行）；既有 mock 全链/候选序/`allow_evict`/枚举一次 5 测全绿 |
| F5 | `if_nametoindex_zero_platform_split`（darwin 拒 / linux 按名 ENODEV + 空名 InvalidInput）；`candidate_filter_platform_split`；`iface_same_three_states`；`state_of_from_name_keyed`；`repick_same_name_with_index_flap_stays`；bindwatch 既有六测全绿 |
| F6 | `plist_state_matching_four_states`（生产夹具逐字 + 提别的 state）；`plist_unknown_shape_conservative_fallback`（bplist00/坏 XML）；`plist_state_separator_and_normalize_forms`（空格/等号/尾斜杠 + 六条白名单拒绝面 + 合法实体解码） |

### 2.4 flake 甄别（按 ROADMAP 口径：隔离复跑 + 改动面交集 + 基线可复现）

| 测试 | 观察 | 判定 |
|---|---|---|
| `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect`（**在册**） | 全量轮首跑红（`daemon/tests.rs:696` 空读）；本树隔离 **5 跑 3 红 2 绿**（同一二进制红绿翻转）；**基线 `git stash` 隔离 8 跑 3 红 5 绿**（可复现）；与改动面零交集（`daemon/**` 未改） | **flake**（三项齐：隔离红绿翻转 + 无交集 + 基线复现）；后续全量轮未再命中 |
| `server::bind::tests::dual_stack_listen_and_unmap`（**新登记**，代码门 dsh 观察） | 与另一 `cargo test` 并发时红（200×5ms 轮询窗）、隔离 5/5 绿；`bind.rs` 未改 | **flake**（已补进 `REVIEW-ROADMAP.md` 已知 flake 表） |

---

## 3. 判据行登记（随代码同批 commit：`docs/INTEROP-CRITERIA.md`）

| 条目 | 变更 | 表 | 行 |
|---|---|---|---|
| **E22** | 新增 `fakeip=%d` 尾字段（行文 + 计数输入集；计数落点 = 交付应答、每应答 ≤+1；不以它判污染） | 判据变更记录（主表） | E22 清单行已改 + 登记行 |
| **term 键编码** | 平台口径「出口编译宿主」→「客户端 HELLO 声明；未声明/歧义 = 宿主推断」；新 caps 位 bit2/3 + 歧义 fail-soft；声明后 macOS 上 `alt+文本` `text`→`ESC text`；**取代 D-10**；非 darwin 无夹具 | 主表 | — |
| **CA11** | 判定输入集 = plist 内容提及该 state（四态 + 未知形态保守退化；默认 state 亦然）+ 非相关分支文案改写（仅 darwin 打） | 主表 + CA11 清单行注记 | — |
| **UPnP（F4）** | 与 Go 四条有意分歧逐列（钉卡降级 / 三发 ST / 判定表 / A-any 末位）+ E20 族数值语义 + **A-any 成功 ≡ 对 tier `exit-upnp-port-mapping:32-35` MUST 的 opt-in 等价偏离**（代码门 M2） | 主表 | — |
| **F2 五键** | additive 配置面 + 两处 spec opt-in 偏离（`dns_upstream` ↔ `wg-native-dns:40` MUST；`dns_fallback` ↔ `:77` SHALL）+ env 缝纪律 + config.toml 对 Go 单向 + 端口 0 拒启 + 覆盖告警一行 | 主表 | — |
| **E21** | 行文/渲染形态不变；linux `index=0` 不再蕴含不可钉 + darwin 候选过滤 + 三处 index 键面 name 优先 + linux 空名补门 | 计数输入集表 | — |
| **E4** | 行文不变；`upstream=` 取值来源扩展（配置列表 / 默认跟随） | 计数输入集表 | — |
| **C14 + `UDP 默认路径` 行** | 行文逐字不变；取值来源 = 配置探针目标（默认不变；一键喂三路耦合写明） | 计数输入集表 | — |

`diff` 摘要：E22/CA11 两条清单行文改动 + 主表新增 5 行 + 计数输入集表新增 3 行 + 批注段落（「其下五行 = Q-J」）。

---

## 4. 代码门（dsh 外部评审 r24）记录

### 4.1 轮次与执行事实

| 项 | 值 |
|---|---|
| 目录 | **`/tmp/dsh-review/r24.YGC12g/`**（`prompt.txt` 5198B / `output.md` 17675B / `stderr.log` 223561B） |
| 命令 | `dsh --profile headless "$(cat prompt.txt)"`，**仓根前台**执行 ⇒ **`exit=0`**（`echo exit=$?` 捕获） |
| prompt 口径 | 只喂指路信息（设计文档 + 改动文件清单 + `git diff HEAD` 范围 + 七个评审重点 + 纪律「不改仓内文件」），**不喂结论** |
| 仓内副作用 | **无**（评后 `git status` 与评前一致；dsh 未在仓内写文件） |
| 评审方独立复跑 | `cargo test --workspace` **640 passed / 0 failed**（当时值）、clippy clean、两目标交叉 check 过、`check-vocab.sh` PASS |

### 4.2 评审原文摘要（不改写，按高/中/低）

**总体结论（原文）**：「**可过代码门：无高危、无 wire 破坏，F1 缺省逐字节/F2 缺省逐值不变、
越界干净、门全绿；但需办 1 项登记-实现一致性必改（M1：补 `dns_upstream` 覆盖告警行 或 改登记措辞），
并建议随批处理 M2（A-any ↔ tier MUST 的定性升格 + 登记）与 M3（F2/F6 未落地的 4 处测试），
9 条低危可改可登记。**」

- **七项重点结论**：①wire 兼容性**成立未发现破坏面**（含「旧出口只读 bit0/bit1 ⇒ 新位静默忽略＝宿主推断」
  的 HEAD 源码核对；裸 ID 形态仍拿 ATTACHED）；②配置化**逐值同今日**、env 缝成立、`dns_upstream` 确关
  mtime 跟随——**唯一缺口 = 设计/登记承诺的覆盖告警行不存在 → M1**；③F4 与设计自洽、既有成功路径未扰动；
  ④三处 index 键面收干净、F6 四态与 normalize 成立（linux 放宽反例 = 空 name → L1）；
  ⑤Go 默认等价成立、超 Go 分歧逐条登记；⑥Go 直译痕迹基本干净（残余 3 条 L6）；⑦**越界干净**（无 TTL 缓存、
  无 portfwd 监听器、改动集合与声明集合逐项一致）。
- **M1**：`dns_upstream` 覆盖生效告警行「登记有、代码无」——登记失真 + opt-in 偏离静默。
- **M2**：A-any 触发条件与 tier `exit-upnp-port-mapping:32-35` MUST 的 WHEN 完全重合 ⇒ 建议升格为
  「opt-in 等价偏离」登记 + §8.2 由「知会」升为「待 tier 确认/修订」。
- **M3**：设计测试计划 4 处未落地（`dns_probe_target` 生效 / `ddns_resolver` 穿参 / 五键 E2E /
  env 真注入 / F6 集成分支）。
- **低危 L1–L9**：linux 空名解绑缺口 / 端口 0 放过 / F6 未登记收窄格 / Linux 下新文案空转 /
  无主腿输入计数不可观测 / Go 味残余 3 条 / 排版残留 / 登记表非追加序 / 新 flake 待登记。

### 4.3 逐条处置表

| 编号 | 处置 | 落点 |
|---|---|---|
| **M1**（登记-实现一致） | **认同（必改）**：取建议①——`DnsProxy::spawn` 在 `cfg.upstreams` 非空时打**一次性覆盖告警**（复用语措「真实 IP/域名规则失效」）+ 测试断言恰一次 | `dnsproxy.rs::spawn`；`static_upstream_override_no_follow` 断言；登记行补「含测试断言」 |
| **M2**（tier MUST 定性） | **认同**：登记升格为「**opt-in 等价偏离**」（仅当路由器支持 A-any 时生效；不支持/失败回落原失败路径与计数不变）+ 上报清单升「待 tier 确认/修订」；**实现不改**（削收益需主会话拍板，见 §6.2） | `INTEROP-CRITERIA.md` F4 行；本文件 §6.1/§6.2 |
| **M3①** | **认同**：补 `resolve_ddns_uses_injected_resolvers`（mock UDP 直测注入解析器 + `AllFailed(注入表长度)`） | `ddnscheck.rs` 测试 |
| **M3②** | **认同**：补 `probe_once_uses_injected_targets`（本地 UDP 桩双答 DNS 探针/STUN ⇒ 断言 `DNS:53`+通用+PROBED 位） | `engine.rs` 测试 |
| **M3③** | **认同**：`qh_config_failfast.rs` 的 E2E 用例表增 `dns_fallback=""` 与 `dns_probe_target=["1.2.3.4:0"]` 两例（走全链：rc=1 + 文案 + 进程存活 + 零副作用 + 状态面） | `tests/qh_config_failfast.rs` |
| **M3④** | **认同**：env 缝测试改**真注入**（`set_var`/`remove_var`；仓内唯一读写该变量点）+ 保留结构断言（挑卡不吃 env） | `bindwatch.rs` 测试 |
| **M3（F6 集成分支）** | **部分认同**：`dial_control_spawn` 集成分支需起真进程树（含 launchd 语义），成本/收益不符——**改为登记**（§5.4 残余）；匹配四态与文案已由纯函数测试覆盖 | 本文件 §5.4 |
| **L1**（linux 空名） | **认同**：linux 分支补空名硬拒（`InvalidInput`）+ 平台分档测试补空名例 | `egress.rs` |
| **L2**（端口 0） | **认同**：五键值域统一拒端口 0（文案沿用值域族）+ 坏值表增 5 行 | `serve_cli.rs` |
| **L3**（F6 未登记格） | **认同**：登记行补「仅提别的 state ⇒ 不等（默认 state 亦然）」 | `INTEROP-CRITERIA.md` CA11 行 |
| **L4**（Linux 空转文案） | **认同**：非相关分支文案**仅 darwin 打**（`cfg!(target_os="macos")`）+ 登记补注 | `daemon_cli.rs`；CA11 行 |
| **L5**（丢输入不可观测） | **认同**：改「首 1 次 + 每 100 次」记行（有界）+ 测试断言首跳出声、前三跳不刷屏 | `service.rs` |
| **L6**（Go 味残余） | **部分认同**：`dns_fallback` 保持 `String`——设计表格自定该形态且 `dial_addr`/日志文案（Go 同串 `223.5.5.5` 不带端口）依赖字面形态，改 `SocketAddr` 会动文案面；`Result<Vec<String>, ()>` 与 `#[allow(too_many_arguments)]` 为白名单/显式豁免——三条**登记不改** | 本文件 §5.4 |
| **L7**（排版残留） | **认同**：`upstream_follow_on_change` 并行还原 | `dnsproxy.rs` |
| **L8**（登记表非追加序） | **认同**：Q-J 三行移至计数输入集表**末尾**（Q-G 行之后） | `INTEROP-CRITERIA.md` |
| **L9**（新 flake） | **认同**：`server::bind::tests::dual_stack_listen_and_unmap` 补进已知 flake 表 | `REVIEW-ROADMAP.md` |

**高危：0 条。必改（M1）：已改 + 复跑门（测试/clippy/交叉）全绿。**
不认同：0 条（M3-F6 集成、L6 采「部分认同」的登记/不动处置，理由已写）。

---

## 5. 不做项 / 范围收窄 / 残余

### 5.1 不做（设计 §4 对照；逐条留痕）

- DNS TTL 缓存 / 每查询缓存 = **Q-I-DNS 小批**（REVIEW-ROADMAP）——本批未做（代码门 ⑦ 独立确认）；
- portfwd 实装监听器 = **Q-F-B 批**——本批未做；
- keyenc 布局字段（`unshifted_codepoint`/per-layout）——wire 无字段（残余登记）；
- `option_as_alt` 的 `.true/.left/.right` 三档——wire 无字段 ⇒ 恒 .false（残余登记）；
- macOS 系统 DNS 真源自动读取（`scutil`/`dns_configuration_copy`）——D3（私有 API 不稳/子进程粒度不匹配）；
- fake-IP **拒绝式**卫兵——D2（违反 tier MUST）；拦截层「目的 ∈ 198.18/15」专用计数——范围外（既有可观测量：`dialfail` + ddnscheck 卫兵告警）；
- UPnP 描述文件 `deviceType` 白名单 / `GetExternalIPAddress` 自检——收益低；
- `IfaceInfo.index` 改 `Option<NonZeroU32>`——面大收益小（登记「考虑未采纳」）；
- 给新配置键加 CLI flag——配置面足够（登记「不加」）；
- 两台生产出口滚动升级 / tier pin 前进——**用户触点**。

### 5.2 范围收窄（登记）

- `REVIEW-ROADMAP.md:180`「keyenc 平台/**布局**字段化」的「**布局**」不做（wire 无字段；章程条目收窄，上报）；
- ROADMAP「DNS 上游列表/fake-IP 卫兵统一配置化」的「卫兵」按 tier spec 收窄为**观测面**（F3）；
- 🔎 四条（`same_candidates`/`tunnel_addr`/`direct_first`/`frame` 长度域）复验**不成立**（已由 Q-C F11 处置）——勾选不重做。

### 5.3 F7 平台假设盘点（三类）

- **已处置**：键平台语义（F1）、DNS 兜底/自检/探针目标/默认路由探针（F2）、`if_nametoindex` 语义（F5）、
  launchd 形态判定（F6）、`option_as_alt ≡ .false` 假设本身（F1 残余登记，与「布局不做」同族）；
- **已勾选（前批）**：`sun_path` 上限（Q-G F4）、fd 语义（Q-G F1）、`is_fake_ip` 单源化（本批 F3）；
- **不动（合法的平台差异，非环境假设）**：PATH/shell/dscl/ps/proc/TIOCGPGRP（`term/pty.rs`/`term/agent.rs`）、
  虚拟网卡前缀 19 条（`egress.rs`，多跳一张虚拟卡只少一个候选）、`stun/stun6` 服务默认（走既有 flag/config 面）。

### 5.4 残余（登记不改）

| 残余 | 理由 |
|---|---|
| F1 非 darwin 口径无夹具（仅源码行级真源 + 期望表） | 向量由 darwin 宿主产出；伪造向量 = 假判据 |
| `leg_missing_input_drops` 无状态面字段（仅节流日志 + 测试可读） | 本批观测量足够归因；并入 term 状态面留后续批 |
| F6 集成分支（`dial_control_spawn` 命中/未命中）无自动化用例 | 需起真进程树/launchd 语义；纯函数四态 + 文案已覆盖（代码门 M3 部分认同） |
| `dns_fallback` 保持 `String`（非 `SocketAddr`） | 设计表格自定形态；`dial_addr` 与 Go 同串日志文案依赖字面形态（代码门 L6 部分认同） |
| `parse_program_arguments -> Result<_, ()>` 非类型化错误 | 白名单语义下仅「未知形态」一种 Err（代码门 L6） |
| UPnP 描述文件 `LOCATION` 主机名解析不可取消（挂死 LAN 主机名不受 deadline 约束） | R3 起既有残余；真机形态不可本地测 |

---

## 6. 需上报项（主会话 / tier）

### 6.1 tier 侧（需求真源偏离与采纳）

1. **spec opt-in 偏离 4 条**（默认面不动；需 tier 知会或修订）：
   - `dns_upstream` ↔ `wg-native-dns:40` MUST（系统解析配置跟随）——仅显式配置时偏离；
   - `dns_fallback` ↔ `wg-native-dns:77` SHALL（223.5.5.5 直查）——仅显式配置时偏离；
   - **F6 精确化 ↔ `role-management:184-188`**（「或本机任意 homeway 代理 plist」检测集合）——本批把
     「等 KeepAlive」的触发面收窄为「plist 内容提及该 state」；**若 tier 坚持广义检测需修订或确认**；
   - **E22 `fakeip` 字段 ↔ `wg-native-dns` 代答可观测性段的统计行描述**；
   - **（代码门 M2 新增）A-any 成功形态 ↔ `exit-upnp-port-mapping:32-35` MUST**（候选全被占用/拒绝 ⇒
     MUST 走映射失败路径）——触发条件 WHEN 完全重合，定性为 **opt-in 等价偏离**，由本批「知会」升格为
     **待 tier 确认/修订**。
2. **tier 侧采纳（跨仓触点，本仓只备机制）**：
   - App 在 HELLO caps 置 **`KEY_ALT_ESC_PREFIX`**（建议值；证据 = App 键源无 alt 产字符 +
     迁移前本地库 aarch64-linux-musl ⇒ 非 darwin 口径）；
   - `term-surface-protocol:172-174` 现无 Alt/口径条款 ⇒ 建议补 spec 条款（F1 给客户端新增义务）；
   - `exit-upnp-port-mapping` 的候选序 SHALL 与 A-any 兜底关系 = 见上（升格为待确认）。
3. **F1 分叉消失的两个前提**（均为用户触点）：客户端置位 + 两台生产出口升级。本批交付 = 机制 + 缺省兼容。
4. **Go 回滚面**：`config.toml` 写进新键后对 Go 侧（`Undecoded()` 检查）**单向不兼容**（Go 已退役，仅影响回滚/对照）。
5. **两台生产出口滚动升级 / tier pin 前进** = 用户触点（本批不动）。

### 6.2 主会话裁决项

- **A-any 与 tier MUST 的处置二选一**（代码门 M2 给的两条路）：① 维持实现 + 登记为 opt-in 偏离 + 请 tier
  确认/修订（**本批已按此登记**）；② 把 A-any 限定为「候选被路由器拒绝（非被他人映射占用）」子形态
  （零 spec 冲突，但削掉大部分收益）。**本批不自行改实现。**

---

## 7. 复核勾选（跨批义务）

- 🔎 四条（`same_candidates` 多重集 / `tunnel_addr` 撞车 / `direct_first` 哨兵 / `frame` 长度域）=
  **Q-C F11 已处置**，本批复验不成立、勾选不重做（证据 = `QJ-design.md` §0.2 条目 6–9 + §0.4）；
- `AUDIT-2026-10-07.md` Q-J 五条（含 🔎 行）已逐条补「Q-J 已修（F1/F2+F3/F4/F5/F6）」处置注记；
- `sun_path`（Q-G F4）已勾选（§5.3）。
