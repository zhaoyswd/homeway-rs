# Q-C 批 — 入口安全与资源上限（第 2 棒：实现 + 代码门）

> 批次：Q 批整改第三批（`docs/REVIEW-ROADMAP.md` §Q-C）。本文档 = 实现记录 + **代码门**（dsh
> 外部评审）记录 + 逐条处置 + 测试/判据证据。设计规格 = `docs/reviews/QC-design.md`（第 1 棒，
> v2 含设计门处置）。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`（Q-C 节）+ `docs/INTEROP-CRITERIA.md`（判据变更记录）。
> 实现基线 `git HEAD = 938007d`（Q-B 收口）。代码门轮次目录 `/tmp/dsh-review/r4.tywkTq/`。

---

## 0. 用户裁决记录（2026-10-07，本批执行依据）

| 项 | 裁决 | 落地 |
|---|---|---|
| **F5 中继腿表** | **先采「无状态 cookie 挑战」；实现期复验发现其不可实现**——PROOF wire 固定 50B **不含 pubkey**（`relaywire.rs:121-150`：sub(1)+nonce(16)+mac_dh(16)+mac_psk(16)+ver(1)），token 模式校验需 `HMAC(secret,"relay-psk"‖nonce‖pubkey)`、开放模式 DH 需 pubkey，而 `label=sha256(pubkey)[:8]` 不可逆 ⇒ wire 不变前提下无解。**用户据此改采设计文档「备选」= `pending` 小表**（≤64、满按最旧淘汰**不拒绝**、PROOF 通过才进 `legs`，含 Q17 三条语义） | `relay/mod.rs`（`PendingLeg`/`handle_control`/`reap_round`）；判据登记（登记表 + R1 行） |
| **F7.4 中继观测通道** | **只做日志 + 单测观测**（不新建遥测通道、不改 R10 行文） | `Stats` 新增计数仅日志/单测可见；登记「观测面 additive」行 |
| **F11 `tunnel_addr` 撞车守卫** | **本批做** | `tunnel_addr.rs` 两函数共用守卫 + `SERVER_TUNNEL_IP` 迁址；登记表 |
| **F12 `if_nametoindex`** | **本批最小守卫**，完整语义挂 Q-J | `egress.rs`（`index_ok` + 返 0 告警 + `pin_socket_to_iface` 硬拒）；E21 形态不变 |

> **注记（与派单摘要的差异）**：派单摘要写「F5 → 无状态 cookie 挑战」，设计文档 §6 记录的是
> **后续复验 + 用户改裁**（cookie 不可实现 ⇒ pending 小表）。两者冲突时以设计文档 §6（附代码证据）
> 为准，本批按 pending 表实施；cookie 版所需的 pubkey 传递在 wire 不变前提下确无实现路径。
>
> **2026-10-08 裁决确认**：主会话向用户呈报上述冲突后，用户授权「按建议定序执行」——**F5 方案
> 就此定为 `pending` 分表**（技术受限：无状态挑战需校验方掌握 HELLO 的 pubkey，而 PROOF wire
> 固定 50B 不含之、`label=sha256(pubkey)[:8]` 不可逆）。**方案已在代码中生效，不再变更**；本行的
> 「用户改裁」措辞保留为实施当时的记录，最终裁决以本行 2026-10-08 确认作准。

---

## 1. 实现清单（F1–F12）

| 条目 | 改了什么 | 文件/函数 |
|---|---|---|
| **F1**（P0-2） | 淘汰拆「纯选择 + 排除视图」：`select_stale_victim`（`&self`，不落库/不打行/不产 op）→ `assign_ip`/`assign_tun_ip` 加 `exclude`（`ip_taken`/`ip_held_by_pub` 对「表内 − victim」判定）→ **校验通过才** `remove` + E9 行 + `DevOp::Remove`；落位序列 `[Remove(victim), Add(new)]`（与 Go `removeLocked→AddPeer` 同序）；撞车错误路径不动表、不打 E9 | `server/table.rs`：`register`/`select_stale_victim`/`assign_ip`/`assign_tun_ip`/`ip_taken`/`ip_held_by_pub`；`server/engine.rs`：真实 `Device` 一致性单测 |
| **F2**（P1） | 采纳收紧为「**解出 Data 帧才 adopt**」（`frame::frame_kind` 先取 kind 结束借用再 `adopt`，避开 E0502）；Control(hint)/未知 kind/垃圾不 adopt；hint 仍学习但不接管路径；未知来源 hint 过 `probe_addr_acceptable`（封「任意地址打洞」注入面），已知来源（候选/中继腿）豁免 | `wtransport/bind.rs`：`recv_from`；`wtransport/frame.rs`：`frame_kind` |
| **F3**（P1） | 未采纳期 reg **2s 定时补投**（`last_reg_sent` + `reg_resend_due`），真写出才消费/更新；上界 = **每次未采纳期** ≤15 次 / ≤60s（rearm 归零，恢复阶梯重开预算）；节拍参数可注入（测试用，免墙钟 sleep） | `wtransport/bind.rs`：`send_wg`/`peek_reg`/`reg_resend_due`/`rearm`/`rearm_soft` |
| **F4**（P1） | 端点缓存 cap 64（落 `observe`/`mark_verified`/`load`/`merge_disk`/`entries()` 出口）；淘汰键 = `entries()` 排序**尾部**（保护 verified；**新条目永不被拒**）；投喂配额 60s 内新增未验证地址 ≤24（hint/probe 共用、只计新增、rearm 归零）；`note_rearm` 接三处 rearm 调用点；`mark_failed` 按设计门 Q13 剔除（无真实调用点） | `wtransport/endpoint_cache.rs`；`facade/tun_exec.rs`（2 处）、`session/mod.rs`（3 处）rearm 接线 |
| **F5**（P1） | 腿表分表：HELLO 只写 `pending`（≤64、满按最旧淘汰**不拒绝**，wire 不变），PROOF 校验（版本→TTL/nonce→MAC/DH）通过才提升进 `legs`（受 `max_legs` 闸；**已存在腿就地更新不迁移**——Q17①）；`reap_round` 清过期 pending；控制面建腿路径仍按 `max_legs` 闸 | `relay/mod.rs`：`PendingLeg`/`Leg`/`handle_control`/`reap_round`/`handshake_done` |
| **F6**（P1） | 拒绝类日志每原因节流（首 3 条 + 每 100 条一条），点位补全 5+2 处（max-legs、per-peer、PROOF 版本、token、并发握手、全局 assoc、下行超限/发送失败） | `relay/mod.rs`：`reject_log_due`（`RejectLog` enum 桶） |
| **F7**（P1） | ① v1 fallback 只认 `a.backend`（replay 回滚保留 backend 的场景正确），其它来源**显式丢弃 + `leg_rejected`、不续命**；② 下行字节桶 16MiB/s/会话（≥100Mbit/s 口径，**不**复用 `leg_rate_ok` 的 22Mbit/s 硬顶）；③ 未认证不 `bump_sock_bufs`（LEGUP 认证 / v1 合法源首包才抬）+ 全局 assoc 上限 1024；④ 观测只落日志/单测（用户裁决） | `relay/mod.rs`：`assoc_read`/`forward_up`/建 assoc/`Stats` |
| **F8**（P2） | `forward_up`/`forward_down` 只计成功（`Ok` 才 `forwarded_*`，`Err` 进 `send_fail_up/down`）；等腿窗 pend 回放失败只进 `send_fail_up`（不动下行面）；assoc socket 改**双栈**（`udpbatch::open_client_socket` + `xmit_addr`，对齐 Go `ListenUDP("udp", nil)`） | `relay/mod.rs`：`forward_up`/`assoc_read`/建 assoc |
| **F9**（P2） | `respond_ex` build 改**字节截断**（与 Go `probe.go:173-174` 逐字同形、不 panic；弃 `floor_char_boundary`）；中继 build 注入共享常量 `BUILD_STR`（serve 侧不动） | `probe.rs`；`lib.rs`（`BUILD_STR`）；`relay_cli.rs` 装配 |
| **F10**（P2） | 中继记录 `ctl_ok`（TCP 同号口 bind 结果）并暴露到**中继探针 flags bit5** `FLAG_RELAY_CTL_DEGRADED`（serve 侧 flags=caps 不动） | `relay/mod.rs`（`ctl_ok`/`relay_flags`）；`probe.rs` |
| **F11**（P2 🔎） | `same_candidates` 抽共享实现（`bind.rs` `pub(crate)`，删 `domain_eps.rs` 非多重集第二份）；`derive_tunnel_ip`/`derive_tun_ip` 共用 `SERVER_TUNNEL_IP` 撞车守卫（`hw-tun.N`/`hw-app.N`，常量迁 `tunnel_addr.rs` + `wgcore` re-export）；`direct_first` 修 `None` = 显式关（+ RARM 文案）；`frame` 两处 `debug_assert!` 长度域 | `wtransport/bind.rs`/`domain_eps.rs`/`frame.rs`；`tunnel_addr.rs`；`wgcore/mod.rs` |
| **F12**（P2） | `if_nametoindex` 返 0 不静默：标 `index_ok=false` + 告警行（带 errno）；`pin_socket_to_iface(index==0)` 硬拒（darwin 上 `IP_BOUND_IF=0` 是「解绑」且 setsockopt 成功——不守卫会把「解绑」误报成「已钉卡」）；**E21 `index=` 取值路径形态不变** | `server/egress.rs`；`server/bindwatch.rs`（测试夹具） |

**P2 性能面**（`relay/mod.rs` 每包堆分配/64KB 分配/全表扫描、`RelayLog::logf` 无缓冲）按批次
约定**移交 Q-I**，本批未动。

---

## 2. 测试证据

### `cargo test --workspace`（2026-10-07，本机 Mac14,3 / 8 核，loadavg 6–19）

- 主态：**473 passed / 1 failed / 4 ignored**（homeway-core lib；其余套件全绿——
  `--no-fail-fast` 全量跑：capi 14、integration 系列 1+3+3+4+2+3+1、doc 0 全过）。
- 两枚失败均为**预先已知 flake**（派单已列，甄别后非本批回归）：
  1. `wgcore::stackb::tests::stack_to_stack_tcp_transfer_fills_window`（`stackb.rs:409` 墙钟
     吞吐断言 `mbps > 100`；本机实测 17–49Mbps）。**基线复现证据**：`git stash`（本批全部
     改动移除）后在 `HEAD 938007d` 上单跑同一测试 → **同样红（42Mbps）**。
  2. `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect`（`daemon/tests.rs:694`
     读空日志行 → index out of bounds；`--no-fail-fast` 全量跑时出现）。**甄别**：同一二进制
     重复单跑 **4 次翻 3 红 2 绿**（同代码同构建，红绿翻转 = 时序敏感，非确定性缺陷面）；
     该文件**不在本批 diff 内**（`git diff 938007d..HEAD --name-only` 无 daemon 文件）。
  ⇒ 本批 DoD 的「全绿」以「除上述两枚既有 flake 外全绿 + 基线/同二进制翻转双重甄别」呈报。
- 新增/改动单测（全部绿，实际执行）：
  - **F1**：`table_full_and_stale_eviction`（ops 恰为 `[Remove(victim), Add]`）、
    `eviction_view_allows_collision_with_victim_only`（A 版视图核心回归）、
    `eviction_view_rejects_collision_with_live_device`（错误路径不留孤儿/不打 E9）、
    `engine::tests::eviction_removes_device_peer`（真实 `Device`：淘汰后 peer 真被摘、
    `peer_count` 不单调增长）、`engine::tests::clone_same_pubkey_keeps_single_peer`（弱不变量）
  - **F2**：`only_data_frames_adopt`（垃圾/未知 kind/hint 不 adopt、C5/C6 不打；Data 仍采纳）、
    `unknown_source_hint_filtered`（私网 hint 拒、可接受地址放行）
  - **F3**：`reg_resend_while_unadopted`（间隔内不补投 / 到点补投 / 次数上界 / **时长上界** /
    rearm 归零；参数注入免墙钟）
  - **F4**：`cap_evicts_tail_and_protects_verified`（含全 verified + 满 ⇒ 新条目仍接纳）、
    `load_and_merge_respect_cap`（**真断言内部表**，load/merge 自身 trim）、
    `feed_quota_shared_and_reset`（hint/probe 共用、刷新不计数、新窗重置、rearm 归零）
  - **F5**：`hello_flood_does_not_occupy_legs`、`pending_full_evicts_oldest_not_reject`、
    `legs_cap_applied_at_promotion`
  - **F7/F8**：`v1_fallback_binds_to_backend_source`、`assoc_lazy_bump_and_down_fail_counted`
  - **F9**：`respond_build_byte_truncation_no_panic`（31 ASCII + 汉字恰跨界 → 不 panic、字节同形）
  - **F10**：`probe_flags_report_ctl_degraded`
  - **F11**：`same_candidates_multiset_counterexample`、`direct_first_none_means_off`、
    `guard_against_server_tunnel_ip`（**双向研磨**：hw-tun 命中 + hw-app 命中样本）
  - **F12**：`if_nametoindex_zero_is_not_pinned`

### `cargo clippy --all-targets -- -D warnings`

- **clean（exit 0，0 warning/error）**（代码门整改后复跑）。

---

## 3. 判据行登记证据（`docs/INTEROP-CRITERIA.md`，与代码同批 commit）

本批**不新增判据行**；全部为「登记表」2 条 + 「计数输入集 / 数值语义变化」9 条（共 11 条，与
设计 §3 一致），代码门后按处置补正 3 处措辞/边缘：

- **登记表（行文/行为差异）**：
  1. 中继「注册腿总数已达上限…拒绝新的」拒绝行——**拒绝点从 HELLO 移到 PROOF 提升**（F5；
     含「极端洪水下合法 PROOF 未命中 pending 时静默计 `伪造`」边缘注记，代码门 M1）。
  2. `tunnel_addr` 撞车分支——可研磨设备（raw == `100.64.255.1`）改按 `hw-tun.N`/`hw-app.N`
     再散列（F11；**wire 差异双侧对称**：客户端与服务端共用同一函数）。
- **计数输入集表（行文不变）**：E9 触发集（F1；撞车拒绝不打 stale 行）、C5/C6 输入集
  （仅 Data 帧）、E8 频次 + `idle=` 语义（**每次未采纳期** ≤15 次/≤60s、**rearm 归零**，
  代码门 M3①）、C13 条数（cap 64 + 尾部淘汰 + 配额 24/60s）、R1「256」数值语义（只数已接纳腿）、
  C14「构建」字段（中继 build 注入）、R10（只计成功 + `dropped` 输入集扩大〔全局 assoc 上限〕+
  **等腿窗 pend 入队即计**〔既有语义，代码门 M4〕+ `send_fail_*`/`down_limited` additive）、
  中继 assoc 下行速率（0 → 16MiB/s）、观测面 additive（中继 flags bit5 / 日志节流）。
- **明确无判据行影响**：F6（拒绝行非判据行）、F9 截断（`probe` 应答非判据行）、F12（E21 形态不变）。

---

## 4. 代码门（dsh 外部评审）

- **轮次目录**：`/tmp/dsh-review/r4.tywkTq/`（`prompt.txt` / `output.md` / `stderr.log`）
- **命令**：`cd <仓根> && dsh --profile headless "$(cat prompt.txt)" > output.md 2> stderr.log`
- **exit code**：**0**（成功；`output.md` 已完整读毕）
- **评审方法（其自述）**：`git diff` 全量逐行读 + F1–F12 逐条回源码 + Go 只读基线
  （`peers.go`/`assoc.go`/`leg.go`/`probe.go`/`tunneladdr.go`/`endpointcache.go`/`bind.go`）对照 +
  判据登记逐格核 + 实跑 `cargo test --workspace --no-fail-fast` 与 `clippy -D warnings`。
- **结论**：**无高危阻塞项**；4 条中等（M1–M4）、8 条低危（L1–L8）+ 2 条补充观察；倾向「可提交，
  先做 M4（登记文案）与 M2（补测）」。

### 4.1 意见原文摘要（逐条）

**中等（M）**
- **M1**：`pending` 满淘汰可被单源洪水挤掉"PROOF 在途"的合法后端，且落 `forged++`（R10 `伪造`
  输入集未登记）。建议：① pending 加每源 IP 配额；② PROOF 未命中回新 CHALLENGE；③ 单列计数 +
  登记；④ 补 `legit→flood→PROOF` 单测。
- **M2**：F4 `load`/`merge_disk` 的 cap 测试**假绿**——`max_entries` 在 `open()` 之后才赋值，
  两处断言被 `entries()` 出口 `truncate` 兜住，测不出 load/merge 自身是否 trim；而 C13 登记
  正拿它当证据。建议真断言内部表。
- **M3**：F3 次数/时长上界被 `rearm()` 清零 ⇒ 每个恢复周期拿新 15 次预算，N1 的「条目永不 stale」
  仍可达（刷新密度变低）；登记文本「≤15 次 / ≤60s」易读成全局上界；`REG_RESEND_WINDOW` 无测试。
  建议：① 登记改写「每次未采纳期（rearm 归零）」；② 跨 rearm 累计或出口侧识别高频 refresh；
  ③ 补 window 分支单测。
- **M4**：R10 登记写「转发只计成功」，但拨腿等腿窗 `pend` **入队即计** `forwarded_up`（无发送
  动作）⇒ 登记与实现不符。建议登记补一句（或移动计数点，需另行登记）。

**低危（L）**
- **L1**：F2 采纳判据只有 2 字节（`Data=0` ⇒ `[0xBB,0x00]` 即采纳）；设计「垃圾包路径闭合；
  合法帧伪造不可行（需 WG 密钥）」把「帧头可伪造」与「帧内容需密钥」混为一谈。
- **L2**：`race_seen` 上界缺证据；设计给的「由候选 cap + F2 传递钳制」不成立（任意 Data 来源都
  insert）；实际只受 rearm 周期清空限制，所承诺的规模上界单测缺失。
- **L3**：F12 `index_ok` 生产面无人消费（半死字段）；告警裸 `eprintln!`；probe 路径把「不可钉」
  变硬错（与设计「走既有降级路径」表述有落差）。
- **L4**：`guard_against_server_tunnel_ip` 未覆盖 `derive_tun_ip` 新增的 `ip == SERVER_TUNNEL_IP`
  条件（只研磨 hw-tun 样本时该分支不执行）。
- **L5**：`reg_resend_while_unadopted` 依赖 `sleep(2100ms)`，负载机有假红风险。
- **L6**：`cargo test --workspace` 本机不绿（`stackb.rs:409` 墙钟断言；该文件不在本批 diff 内）。
- **L7**：`RL_*` u8 原因码 + `HashMap<u8,u64>` 是 Go 习惯（AGENTS 原则 1 偏好 enum）。
- **L8**：hint 过滤的 `known` 不含 `adopted`（语义注记；现状无害）。
- **补充 1**：`MAX_ASSOCS_TOTAL=1024` × 4MB+4MB 缓冲最坏约 8GB 内核缓冲（需 1024 条**已认证**
  会话）；建议给出取值理由或改字节预算。
- **补充 2**：`mark_failed` 降权未交付——设计门 Q13 已记录「无调用点即剔除」，收口勾 AUDIT 时
  应显式注明，避免账实不符。

### 4.2 逐条处置表（**认同改 / 部分认同 / 不认同**）

| 意见 | 处置 | 依据 / 落地 |
|---|---|---|
| **M1** | **部分认同** | **③ 认同并已改**：登记表 F5 行补「极端洪水下合法 PROOF 未命中 pending ⇒ 静默计 `伪造`（R10 `伪造` 输入集边缘变化；5s 重发 HELLO 自愈）」。**① 不采纳**：加「每源 IP 配额」会改用户裁决的「满按最旧淘汰」淘汰语义（策略变更，超本批授权）；残余窗口 = `HELLO→PROOF` 一个 RTT，其后端 5s 重试自愈。**② 不认同（经复核不可实现）**：PROOF 不含 pubkey，未命中 `pending` 时**无法重建可验证的挑战态**——回一帧新 CHALLENGE 只会让后端再发同样命中不了的 PROOF；且改动后端状态机交互未验证。**④ 不补测**：现有两测已覆盖「洪水→合法注册」；「合法→洪水→PROOF」是把已登记残余写成断言，价值为负。 |
| **M2** | **认同，已改** | `load_and_merge_respect_cap` 重写为**真断言**：`c.max_entries=2; c.load(); assert_eq!(c.entries.len(), 2)` 与 `c2.max_entries=1; c2.merge_disk(); assert_eq!(c2.entries.len(), 1)`（同模块测试直读私有表），出口截断降为兜底断言。 |
| **M3** | **部分认同** | **① 认同并已改**（E8 登记行改为「**每次未采纳期** ≤15 次 / ≤60s，**rearm 归零**——恢复阶梯重开一轮拿新预算」）。**③ 认同并已改**：`reg_resend_interval`/`reg_resend_window` 改可注入字段，测试补**时长上界分支** + rearm 归零断言，去墙钟依赖。**② 不采纳**：跨 rearm 累计上界会让真实换网/重启后的自愈受苦（每次网络变化都该有新预算）；根治在**出口侧**识别同 devTag 高频 refresh（服务端策略，超本批范围）——残余（隧道坏但出站可达 ⇒ 刷新密度受恢复阶梯节流）记入本表（§5 残余项）。 |
| **M4** | **认同，已改** | R10 登记行补 ③：「拨腿等腿窗 `pend` **入队即计** `forwarded_up`（既有语义，本批未改；回放失败只进 `send_fail_up`）」。 |
| **L1** | **认同（措辞）** | 代码不改（与设计 Q7 判据「`src ∈ 已知` 或（解码成功 且 kind==Data）」一致；比 Go「任意包即采纳」严格）。表述收紧记入本表 §5：「可伪造的 Data **帧头**仍可触发采纳（内容需密钥；与 Go 同为已知限制）」；设计文档已加 §7 留痕指针。 |
| **L2** | **认同（论证订正）** | 设计 Q14 的「由候选 cap + F2 传递钳制」**不成立**（已核：`adopt` 对任意 Data 来源 `race_seen.insert`）。现状：只受 rearm（含巡检 rearm_soft）周期清空约束，**无独立 cap**；「规模上界单测」不补（把无界行为写成断言价值为负）；挂账见 §5。 |
| **L3** | **部分认同** | **`index_ok` 保留**：它是「该卡不可钉」的显式状态（设计 F12 要求「标为无 index/不可钉」），删掉或改 `physical_candidates()` 过滤会变更**选卡面**（bindwatch/rltoken/upnp 共用），超出「本批最小守卫」裁决 ⇒ 完整语义按裁决挂 Q-J。**`eprintln!` 保留**：`interfaces()` 无 `Logf` 入参（调用面含无日志上下文的 CLI/token 生成），接 Logf 属 Q-J 完整语义。probe 路径「不可钉 ⇒ 该卡探针失败」= 保守方向（index=0 的卡本就不可用），记入 §5。 |
| **L4** | **认同，已改** | `guard_against_server_tunnel_ip` 增第二个研磨样本（`hw-app` raw == `SERVER_TUNNEL_IP`）并先断言其命中，再断言 `derive_tun_ip` 脱开——新增分支真实执行。 |
| **L5** | **认同，已改** | 同 M3③：节拍参数注入（生产默认 2s/60s），测试用 40–80ms，墙钟依赖与假红风险消除。 |
| **L6** | **不认同为本批问题** | 已独立复核：`git stash` 后 **基线 HEAD 同样红**（42Mbps），文件不在 diff 内 ⇒ 既有环境敏感项，非本批回归；判据改为机制断言属性能/CI 批（Q-I 面），本批不越界改 `wgcore/stackb.rs`。批次 DoD 证据链以「基线复现 + 单测复跑」呈报（见 §2）。 |
| **L7** | **认同，已改** | `RL_*` u8 常量 → `enum RejectLog`（8 变体）+ `HashMap<RejectLog,u64>`；调用点全量替换。 |
| **L8** | **不认同（给证据）** | 不把 `adopted` 纳入 `known`：`adopted` 的证据强度受 L1 已知限制约束（2 字节 Data 头可驱动采纳），纳入后「伪造帧头」即可升级为「任意 hint 注入」跳板；真实 hint 来源（中继腿）已在 `relay_eps`，且 roam 后靠 Data 帧采纳、不依赖 hint。保持现状并留痕。 |
| **补充 1** | **认同（记录）** | `1024` 取值理由：= 4 × `max_legs`（256），且 `bump_sock_bufs` **只在认证后**执行（未认证会话不占大缓冲）⇒ 最坏面从 8192 枚收敛到 1024 枚已认证会话；字节预算式替换属 Q-I/Q-J 面，记入 §5。 |
| **补充 2** | **认同（记录）** | `mark_failed` 按设计门 Q13「无真实调用点即剔除」不交付——本表明示；主会话收口勾 AUDIT 条目时应按此口径勾选（降权项剔除，非漏做）。 |

**处置汇总**：**认同并改 6 条**（M2/M4/M3①/③/L4/L5/L7——含两处为同一修法）、
**部分认同 3 条**（M1 的 ③ 改、① 不采；M3 的 ② 不采；L3 的全部建议不采但保留现状解释）、
**不认同 3 条**（L6 非本批 / L8 有证据 / M1② 复核不可实现）、**记录留痕 2 条**（补充 1/2）。
**无高危项**（评审自判亦为「无高危阻塞项」）。

---

## 5. 本批残余与挂账（不含已裁决的 Q-J/Q-I 项）

- **F1 进程级 E2E（设计 §2 测试计划 4）未执行**：`tools/local-rust-exit.sh` + 表满注入的复现需要
  「设备超 grace 未刷新」——生产装配 `grace` 硬编码 `DEFAULT_GRACE=600s`（`engine.rs:368`
  传 `Default::default()`，无 CLI 开关）⇒ 进程级复现须先让设备空闲 ≥10 分钟。表-设备一致
  **同一不变量**已由 engine 级单测（真实 `Device` + `apply_dev_ops`）覆盖；进程级复现留作
  需要时的交接口径（若要可测，需给 grace 加注入面，属 CLI/config 面）。
- **F5 pending 淘汰竞态（代码门 M1）**：单源 200pps 洪水可在 ~0.32s 刷空 64 槽，极端时序下
  合法后端 `HELLO→PROOF` 的挑战条目被淘汰 ⇒ PROOF 静默计 `forged`，后端 5s 重发 HELLO 自愈。
  按用户裁决的「满按最旧淘汰不拒绝」语义保留；已登记（登记表 F5 行边缘注记）。
- **race_seen 无独立 cap（代码门 L2）**：只受 rearm 周期（巡检 5min 级 / 会话重建）清空约束；
  设计 Q14 原论证已订正。属已知残余（触发需知晓客户端实际 UDP 端点）。
- **F12 完整语义**：`index_ok` 消费方 / 告警进 Logf / 探针口径 —— 按用户裁决挂 **Q-J**。
- **1024 assoc 全局上限的最坏内核缓冲**（~8GB，需 1024 条已认证会话）：字节预算化留 Q-I/Q-J。
- **中继无 JSON 遥测通道**（F7.4 用户裁决只做日志/单测）：新建通道属后续批次。
- **客户端方向 v6**（`relay_cli.rs` 主监听口 v4-only）：仅修 assoc（后端）方向，客户端方向挂账。
- **`stackb` 墙钟吞吐断言的环境敏感**（代码门 L6）：改机制断言属性能批；本批以基线复现呈报。

---

## 6. 测试/判据/评审证据索引

- 测试：`/tmp/qc-test-full.log`（首次）、`/tmp/qc-test-2.log`（第二次）、`/tmp/qc-test-3.log`（代码门整改后）、`/tmp/qc-test-final.log`（`--no-fail-fast` 全量）
- clippy：`/tmp/qc-clippy.log`、`/tmp/qc-clippy2.log`（整改后，exit 0）
- 代码门：`/tmp/dsh-review/r4.tywkTq/`（prompt.txt / output.md / stderr.log）；设计门 = `QC-design.md` §4（`/tmp/dsh-review/r3.e5GAso/`）
- 基线上界复核：`git stash`（临时）在 HEAD 938007d 单跑 `stack_to_stack_tcp_transfer_fills_window` → 42Mbps 红（同款失败），stash 已 pop 复原（工作树与记录一致）
