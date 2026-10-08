# Q-J 通用性批设计文档（keyenc 平台口径字段化 / DNS·探针配置化 / UPnP 协议面 / `if_nametoindex` / launchd 精确化）

> **批次**：Q 批整改最后一个整改批（Q-A…Q-I 均已完成）。
> **真源**：跨批进度 = `docs/REVIEW-ROADMAP.md`（Q-J 范围节 + 每批执行协议 + 已知 flake 表）；
> 发现清单 = `docs/reviews/AUDIT-2026-10-07.md` 的「Q-J 通用性批」节；仓规 = `AGENTS.md`。
> **本棒** = 第 1 棒（设计）：复验 + 设计 + 设计门（dsh 外部评审）+ 逐条处置。**不写产品代码**。
> **基线**：HEAD `e3b1d8b`（Q-I 收口）；工作树 clean。
> **版本**：v2（= v1 + 设计门 r23 全部处置；处置表见 §7）。
> **政策约束**：判据行变更须登记（`docs/INTEROP-CRITERIA.md`「判据变更记录」；计数输入集/数值语义
> 变化（行文不变）也要登记）；**缺省行为兼容既有部署**；不碰两台生产出口 / tier / homeway 两个
> 只读仓 / `baseline/`；不发 tag / 不发 Release / 不建 PR。
>
> **范围边界（明文）**：DNS TTL 缓存 = **Q-I-DNS 小批**（不在本批）；portfwd 实装监听器 = **Q-F-B**
> （不在本批）。本批只做「跨平台/跨环境通用性」：平台假设不硬编进代码、缺省行为不变。

---

## 0. 复验（证据先行）

### 0.1 方法与范围

- 回源码逐条重定位（审计行号会漂）：keyenc / dnsproxy / ddnscheck / egress / upnp / daemon_cli /
  frames / service / session / bindwatch / engine。
- 外部证据（**只读**）：Go 基线 oracle `baseline/homeway`（含 vendored libghostty-vt 源码）、
  tier `openspec/specs/{wg-native-dns,term-surface-protocol,host-daemon,role-management,exit-upnp-port-mapping}`、
  tier App 源码（HarmonyOS 客户端，`terminal/src/main/cpp/**`）与本机 launchd plist（只读）。
- 复验不跑任何生产出口；不动两台现役出口。

### 0.2 逐条复验结果表

| # | 审计条目（P 级） | 真伪 | 现行源码位置（复验后） | 结论 |
|---|---|---|---|---|
| 1 | **P1** 终端键编码平台假设绑出口宿主（`IS_DARWIN = cfg!(target_os="macos")`，消费点 4 处） | **成立** | `crates/homeway-core/src/term/keyenc.rs:134`（定义）、消费点 `:701`（`legacy_alt_prefix` = darwin 恒无 ESC 前缀）、`:779`（mok2 剥 alt 位）、`:839`（darwin 且 super ⇒ 无文本）、`:1072`（kitty 关联文本的 alt 阻文本口径）；测试内两处 `:1427/:1434`，注释 `:14-17/:695-696/:777/:838/:1071` | **F1**。出口按**编译宿主**选 ghostty 的平台分支 ⇒ 客户端语义被出口 OS 决定。**订正 1 处**（见 §0.3-1） |
| 2 | **P1** DNS 上游/探针目标硬编码 CN 段 + fake-IP 无卫兵 | **成立**（「统一」落点按 tier spec 收窄，见下） | `dnsproxy.rs:25`（`DEFAULT_FALLBACK = "223.5.5.5"`）、`:362-372`（`DnsConfig::default` 的 `resolv_path = "/etc/resolv.conf"`，`:365`）、`ddnscheck.rs:66-70`（`DDNS_RESOLVERS = 223.5.5.5 / 119.29.29.29`）、`egress.rs:47-63`（`default_probe_targets` 223.5.5.5+1.1.1.1 / `default_stun_targets` CF+Google）、`egress.rs:283-291`（`preferred_iface` 硬编 223.5.5.5:53 做路由探针） | **F2 + F3**。**关键取证**：tier `wg-native-dns/spec.md:40` 明文「代答上游 **MUST** 使用主机系统解析配置（/etc/resolv.conf 的 nameserver 列表，按序尝试）」；`:47-50`（fake-ip Scenario，MUST 句在 **`:50`**）「主机运行 fake-ip 型代理 **THEN** 手机经代答得到的答案与该主机主 nameserver 给出的答案一致（**含 fake-ip 地址**）」；`:77`「全部 nameserver 均**连接层失败**时，**SHALL** 退到直查公共 DNS（**223.5.5.5**）」⇒ dnsproxy 侧 fake-IP「卫兵」**不得改应答**（拒绝式直接违反 MUST）；可配置化落在「兜底/自检解析器/探针目标」与 opt-in 的上游覆盖（两处 spec 偏离，见 §6） |
| 3 | **P2** UPnP 协议面（只认 IGD:1 / 首应答即信 / 候选端口硬试 +9 无 `AddAnyPortMapping` / 钉卡硬失败无降级） | **成立** | `upnp.rs:23-24`（`SSDP_ST` 仅 `InternetGatewayDevice:1`）、`:91-93`（`msearch_message` 单 ST，`MX: 2`）、`:281-303`（内层 recv 窗口 ≤1200ms/轮，SSDP 腿期限 `min(deadline, now+5s)`）、`:292-296`（首个 `ssdp_response_ok` 即返回，**不看 ST/USN**）、`:942-956`（候选 = prefer → internal → +1…+9，无 `AddAnyPortMapping`）、`:209-212`（`pin_multicast` ③ 钉卡失败 `map_err(UpnpError::from)?` 直接终止该候选） | **F4**。Go 基线（`internal/server/upnp.go:29/138-139/161-163/122-131`）同形 ⇒ 本项 = **超 Go 加固**（登记，非对齐缺陷） |
| 4 | **P2** `if_nametoindex` 失败静默 index=0（macOS 0=解绑且成功，「已钉卡」判据反向） | **成立**（Q-C 只做最小守卫 ⇒ 完整语义缺口确在） | `egress.rs:167-187`（F12 最小守卫在位）、`:208-217`（**全平台**硬拒 `index==0`）、`:191-197`（候选不过滤）、`:441-450`（探针钉失败即候选失败）、**平台事实** `:247-263`（linux 按名，index 不参与）；`upnp.rs:209-212` 硬失败；`bindwatch.rs:198-206`（按 index 回查 live 指纹）、`:144-152`（重挑比较 `cur.index == next.index`）；`egress.rs:568-576`（`select_best` 按 index 认「默认路由卡」） | **F5**（完整语义 = 平台可钉判定 + 调用方降级 + **三处 index 键面**纠偏） |
| 5 | **P2 🔎** launchd 代理探测按「文件名含 homeway」匹配（多实例误命中） | **成立**（且与 Q-H F14 的「仅默认 state」粗判叠加） | `daemon_cli.rs:127-147`（`detect_launchd_agent`：文件名 `contains("homeway") && ends_with(".plist")`；命中返回**文件名去后缀**）、`:149-156`（`launchd_relaunch_relevant` = 仅默认 state）、`:272-287`（等待 KeepAlive 4s → 自拉起；**非相关分支现文案**「（state=… 非默认 state——不等 launchd KeepAlive，直接拉起）」） | **F6**。Q-H F14 偏差（生产 plist 用自定义 `--state`）由 plist **内容**精确匹配收口；CA11 触发集与文案变化须登记；与 `role-management:184-188` 的「任意 homeway 代理 plist」检测集合有 spec 张力（见 §6/§8） |
| 6 | **P2 🔎** `domain_eps::same_candidates` 多重集 bug | **不成立（已由 Q-C F11 处置）** | `wtransport/bind.rs:907-917`（`(addr, relay)` 排序比较 + 长度相等）；回归测试 `domain_eps.rs:745-752` | **登记「已由 Q-C 处置」**，不重做 |
| 7 | **P2 🔎** `tunnel_addr` 与 `SERVER_TUNNEL_IP` 撞车守卫缺失 | **不成立（已由 Q-C F11 处置）** | `tunnel_addr.rs:24-28`、`:60-71`（`derive_tunnel_ip` 守卫）、`:78-95`（`derive_tun_ip` 守卫共用） | 同上，不重做 |
| 8 | **P2 🔎** `direct_first` 哨兵（`Some(0)` 语义） | **不成立（已由 Q-C F11 处置）** | `wtransport/bind.rs:179-183/432`；测试 `:1404-1435` | 同上，不重做 |
| 9 | **P2 🔎** `frame` 长度域静默截断 | **不成立（已由 Q-C F11 处置）** | `wtransport/frame.rs:104-122`（`encode_batch` `debug_assert`）、`:162-169`（`hint_bytes`）、解码越界即拒 `:126-146` | 同上，不重做 |

### 0.3 复验订正 / 误报剔除记录

1. **订正（条目 1 的附加断言）**：审计原文「且 HarmonyOS 走 linux 分支是错误假设」。复验结论：
   **「因」与「果」要拆开**——
   - 「不能用目标平台/命名推断客户端语义」= **成立**（F1 要修的机制正是这个）；
   - 「HarmonyOS 客户端**应当是 macOS 口径**」= **证据不支持**：App 键源（`key_encoder.cpp:165-197`
     `LayoutText/LayoutTextFor` 无 alt 参数、`:199-213 TranslateOhosKey` 只置 ctrl/shift/alt/caps、
     **无 super**）+ surface 迁移前客户端本地编码器（`a2f8c5e~1` 的 `BuildKeySequence` +
     **aarch64-linux-musl** 预编译 libghostty-vt ⇒ `builtin.os.tag != .macos` ⇒ 非 darwin 分支）
     都指向**非 darwin（Alt 当 Meta/ESC 前缀）**。裁定见 §3-D1（本批只落机制 + 缺省兼容）。
2. **收窄（条目 2）**：审计「统一」若按「dnsproxy 也做拒绝式 fake-IP 卫兵」实现，**直接违反**
   tier `wg-native-dns` 的 MUST（§0.2-2 引文）⇒ 裁定：dnsproxy 侧只做**共享检测器 + 计数 + 节流告警
   （不改应答一个字节）**；拒绝式登记为备选（§3-D2）。
3. **无其他误报**：5 条主条目 + 4 条 🔎 条目全部回源码重定位命中；未发现需上报的「与代码现状不符」。
4. **设计门（r23）另行指出的行号/表述订正**（已并入本 v2）：spec fake-ip 句 = `:50`（`:49` 是空行）；
   `is_ula` 是 `egress.rs:93` 私有 fn（ddnscheck 只复用 `is_public_addr`）；surface 迁移前本地库为
   **aarch64-linux-musl**（非「OHOS 目标」）；`Upstreams` 跟随实现位于 `dnsproxy.rs:244-288`
   （`CHECK_INTERVAL` `:33`）；`serve_hello` 全段 `:970-1035`。

### 0.4 已由前批处置项（跨批勾选复核，写进批记录）

| 条目 | 处置批 | 复验位置 | 结论 |
|---|---|---|---|
| `same_candidates` 多重集 | Q-C F11 | `wtransport/bind.rs:907-917` + 回归测试 | **勾选** |
| `tunnel_addr` × `SERVER_TUNNEL_IP` 撞车 | Q-C F11 | `tunnel_addr.rs:24-28/60-71/78-95` | **勾选** |
| `direct_first` 哨兵 | Q-C F11 | `wtransport/bind.rs:179-183/432` + 测试 | **勾选** |
| `frame` 长度域断言 | Q-C F11 | `wtransport/frame.rs:104-122/162-169` | **勾选** |
| `sun_path` 单源与平台值（103/107） | Q-G F4 | `sysfd.rs:19-28` + 平台值单测 `:135-144` | **勾选**（本批盘点引用，不再动） |

### 0.5 平台假设盘点（F7 的交付面；行号 = 复验后位置）

| 假设 | 位置 | 现状 | 本批处置 |
|---|---|---|---|
| 客户端键平台语义 = 出口编译宿主 | `term/keyenc.rs:134` | `cfg!(target_os)` 编译期分支 | **F1**（客户端 HELLO 声明；宿主推断降为缺省） |
| **`option_as_alt ≡ .false` 假设本身** | `keyenc.rs:697-703/1071-1072` | ghostty 该选项另有 `.true/.left/.right` 三档，wire 无字段 ⇒ 恒 .false | **F1 残余登记**（与「布局字段不做」同族：要完整表达需 wire 扩展） |
| DNS 兜底 = `223.5.5.5`（CN 段） | `server/dnsproxy.rs:25` | 常量 | **F2**（可配置；默认不变） |
| DNS 上游真源 = `/etc/resolv.conf` | `server/dnsproxy.rs:365` | 常量路径（macOS 上非真源——真源在 SystemConfiguration） | **F2**（opt-in 上游覆盖；默认不变 + 文档注记；不做 `scutil`/私有 API，见 §3-D3） |
| DDNS 自检解析器 = `223.5.5.5/119.29.29.29`（CN 段） | `server/ddnscheck.rs:66-70` | 常量 | **F2**（可配置；默认不变） |
| DNS 探针目标 = `223.5.5.5/1.1.1.1` | `server/egress.rs:47-55` | 常量 | **F2**（可配置；默认不变） |
| STUN 探针目标 = CF/Google | `server/egress.rs:56-63` | 常量 | **F2**（可配置；默认不变） |
| 默认路由探针目标 = `223.5.5.5:53` | `server/egress.rs:283-291` | 硬编（只做路由查询、不发包） | **F2**（改用配置首项；默认等价） |
| STUN 服务目标 = `stun.cloudflare.com:3478` | `engine.rs:105-106`（`ServeConfig::default` 的 `stun/stun6`） | 硬编默认（已有 `--stun/--stun6` 覆盖面） | **登记**（非本批改动：走既有 flag/config 面） |
| 虚拟网卡前缀 19 条（含 macOS 专有 awdl/llw/anpi/ap） | `server/egress.rs:99-106` | 编译期常量表（名字启发式） | **登记**（不动；多跳一张虚拟卡只少一个候选） |
| launchd 形态判定 = 目录 + 文件名 | `homeway-cli/src/daemon_cli.rs:127-147` | 文件名泛匹配 + 仅默认 state | **F6**（plist 内容精确匹配） |
| `if_nametoindex` 语义 | `server/egress.rs:167-187/208-217/441-450`、`upnp.rs:209-212`、`bindwatch.rs:144-152/198-206`、`egress.rs:568-576` | 平台无关硬拒 + index 键 ×3 | **F5** |
| `sun_path` 上限 | `sysfd.rs:19-28` | 编译期单源（darwin 103 / linux 107） | **已由 Q-G F4 处置**（勾选，不动） |
| PATH/shell/dscl/ps/proc/TIOCGPGRP 等 | `term/pty.rs:40-84`、`term/agent.rs:428-445` | 编译期平台分支（正确的平台差异，非环境假设） | **不动**（盘点登记） |
| fd 语义（SOCK_CLOEXEC/pipe2） | `sysfd.rs:38-90`、`udpbatch.rs` | 编译期平台分支 | **不动**（Q-G F1 已收口） |

---

## 1. 修复清单

> 每条 = 方案 / 涉及文件 / 风险 / 测试计划 / 判据行影响。判据行汇总见 §6。

### F1（P1｜修缺陷 + 机制新增）keyenc 平台口径字段化：客户端 HELLO 声明，缺省 = 宿主推断

**问题**：出口编码器的 4 处平台分支（`keyenc.rs:701/779/839/1072`）按**出口编译宿主**选
（`IS_DARWIN`，`:134`）⇒ 同一手机连 Mac / 连 Linux（阿里云）出口时 `alt+文本键` 字节分叉
（darwin：直发文本；非 darwin：`ESC`+文本）。

**方案**：

1. **wire 面（additive）**：`term/frames.rs` 的 `caps` 新增**互斥两位**（bits 2/3 空位）：
   - `KEY_ALT_NO_ESC_PREFIX = 1<<2`：客户端声明「本端 alt **不**产 ESC 前缀」= libghostty
     **darwin 编译分支**语义（等价 `option_as_alt ≡ .false`，附随 behaviors：mok2 剥 alt 位、
     kitty 关联文本不因 alt 抑制、super 抑制文本）；
   - `KEY_ALT_ESC_PREFIX = 1<<3`：客户端声明「本端 alt 产 ESC 前缀」= libghostty **非 darwin**
     编译分支语义（附随：mok2 保留 alt、kitty alt 阻文本）；
   - **两位全不置 = 未声明** ⇒ 宿主推断（= 今日行为，逐字节不变）；
   - **两位同置 = 歧义 ⇒ 按「未声明」处理 + 计数 + 一次性告警，绝不拒腿**。理由（硬证据）：
     「裸 ID 尾随块（无 caps 块头）按 caps 解析、同形不可判别」是**格式固有属性**
     （`frames.rs:858-861` 的既有测试 `dec_hello_tail(&[4,'h','o','s','t'])` ⇒ `caps = 0x7F`
     **恰含 bit2|bit3**）⇒ 任何拒绝面都会把今天**可服务**的腿变成 `bad_capability`；fail-soft
     同时保住「fail-fast 只在无歧义时」的纪律。
   - 尾随块形状零变化（`[capLen:1][flags][ver][id]`；`dec_hello_tail` 只 OR 位、不校验未知位 ⇒
     旧出口静默忽略新位）；`PROTO_VER` 版本门不动。
2. **出口消费**：`term/service.rs::serve_hello`（`:970-1035`）解析 flavor 写入**本腿**运行时
   （`LegRt.key_flavor`，`:566-577`，唯一构造点 `:1087`）。**语义级判定放 `serve_hello`**
   （与 `:1001-1002` 的 surface/raw 判定同域），`dec_hello_tail` 保持纯形状契约（**不新增
   `FrameError` 变体**）。
3. **每腿属性**：`handle_input`（`:1369-1403`，入径门 `:1301` → `leg_is_surface` `:2947`）按
   `LegKey` 在 `rt.legs` 找本腿取 flavor（与 `leg_is_surface` 同形态）。**查不到腿（`end_leg`
   竞态）= 丢弃该输入 + 一次性计数；任何路径不得回落 `host_default()`**（防「默认宿主隐式回潮」）。
4. **keyenc 形态**：删除 `IS_DARWIN` 常量，改
   `enum KeyFlavor { AltEscPrefix, AltNoEscPrefix }` + `KeyFlavor::host_default()`
   （`cfg!(target_os="macos")` 的**唯一**残留点，文档标注「缺省兼容推断」）；
   `encode_key`/`legacy_encode`/`kitty_encode` **必收** flavor（无默认值）。
   `IS_DARWIN` 全部落点同步改：**生产 4 + 测试 2（`:1427/:1434`）+ 注释 5（`:14-17/:695-696/:777/:838/:1071`）**。
5. **不做**：wire 无 `unshifted_codepoint`/布局字段 ⇒「布局」不做；`option_as_alt` 的
   `.true/.left/.right` 三档不做（wire 无字段 ⇒ 恒 .false）——两项登记为残余。

**涉及文件**：`term/frames.rs`、`term/keyenc.rs`、`term/vt.rs:473-489`、`term/service.rs`
（serve_hello / LegRt / handle_input / leg 查找）。

**兼容性论证**：§2（含「新客户端 × 旧出口」「旧客户端 × 新出口」两格）。

**风险**：
- R1-① 歧义形态计数/告警面：只在「同置两位」时触发（含裸 ID 形态）——additive，行为不变。
- R1-② 腿查不到 ⇒ 丢弃输入（有界、可观测）；不得回落（纪律写进代码注释 + 单测）。
- R1-③ `encode_key` 无默认参数 ⇒ 编译期强制 flavor 传递。

**测试计划**：
- ① 向量 parity（387 案 / 16 模式）**去掉 `#[cfg(target_os="macos")]`**、显式 `KeyFlavor::AltNoEscPrefix`
  ⇒ Linux/musl CI 也能跑 darwin 形态向量（原缺口变可测）。
- ② **非 darwin 口径期望表**（8 案分叉面：`alt+a` ⇒ `ESC a`、mok2 保留 alt 位、kitty 关联文本被
  alt 抑制、super 不抑制文本……），真源 = vendored ghostty `input/key_encode.zig:300-308/402-410/545-547/565-575`
  + fixterms `:489`（**无**平台门，与 Rust `:812-829` 同形）；**显式登记「非 darwin 无夹具、仅源码行级真源」**
  （不伪造向量）。
- ③ HELLO 声明五态单测：未声明 / `KEY_ALT_NO_ESC_PREFIX` / `KEY_ALT_ESC_PREFIX` / 两位同置（⇒ 未声明 +
  计数 + 告警）/ 裸 ID 形态（`[4,'h','o','s','t']` ⇒ 不得拒腿——**B1 回归钉**）。
- ④ 服务端集成：两腿（两种声明）同发 `alt+a` ⇒ 两条 PTY 写字节分别为 `1b61` / `61`。
- ⑤ 腿查不到 ⇒ 丢弃 + 计数（注入缝：先 `end_leg` 再 `handle_input`）。
- ⑥ CLI 面：raw 腿不受影响（`term_cli.rs:975-986` 只发 `RAW_TERMINAL`，走 `Op::DATA`）。

**判据行影响**：默认逐字节不变；登记两条（新 caps 位 + 声明后的行为差异，见 §6）。

### F2（P1｜修缺陷·超 Go 配置化）DNS 上游 / 探针目标配置化（默认不变）

**方案**：

1. **新增 config 键（`[serve]`，`FileServe` + `validate_file` 值域 + `ServeConfig` 透传）**：
   | 键 | 形态（**边界即解析成型**） | 默认（= 今日行为） | 语义 |
   |---|---|---|---|
   | `dns_upstream` | `Vec<IpAddr | SocketAddr>`（newtype 承载） | 空 = **跟随 `/etc/resolv.conf`** | 显式上游覆盖（opt-in；偏离登记见 §6） |
   | `dns_fallback` | `String`（ip 或 ip:port） | `"223.5.5.5"` | 兜底上游（仅连接层失败触发）；**空串 = 拒启**（非「关兜底」） |
   | `ddns_resolver` | `Vec<SocketAddrV4>` | `["223.5.5.5:53","119.29.29.29:53"]` | DDNS 自检直查解析器 |
   | `dns_probe_target` | `Vec<SocketAddrV4>` | `["223.5.5.5:53","1.1.1.1:53"]` | 挑卡 / 健康探针 / **udpcap `DNS:53` 能力位** |
   | `stun_probe_target` | `Vec<SocketAddrV4>` | `["162.159.207.1:3478","74.125.250.129:19302"]` | 通用 UDP（非 53）能力位 |
   - **命名避撞**：既有 `[serve] stun/stun6`（公网端点发现的 STUN 服务）语义不同 ⇒ 探针键带
     `_probe_target` 后缀（评审 B6）。
   - **两键都是列表**（各自独立语义）；`dns_probe_target` **一键喂三路**是**有意设计**
     （三者的谓词同为「这条路能不能到 DNS:53 公共解析器」），**耦合显式登记**（见 §6 C14 行）：
     把该键指到诊断死地址会同时影响挑卡/健康/C14 取值。
   - 不加 CLI flag（配置面足够；登记「不加」）。值域非法 ⇒ 启动期拒启 + 可行动文案（Q-H F1 形态）。
   - 空数组 = 取默认（等价不配置）。
2. **穿参落点**：
   - `DnsConfig`（`dnsproxy.rs:351-372`）加 `upstreams`；`Upstreams`（`:244-288`）在
     `upstreams` 非空时返回**静态配置列表**（不做 mtime 跟随），为空时保持今日 resolv.conf 跟随
     （1s 节流 + last-good 不变）。
   - `filled()`（`:378-382`）的兜底空串替换**保留为内部防御**，但 CLI 路径已被 `validate_file`
     挡住 ⇒ 不可达（设计写明；单测钉「空串 ⇒ 拒启」）。
   - `ddnscheck::resolve_ddns/run_ddns_self_check`（`:105-250`）加 `resolvers: &[SocketAddr]` 穿参。
   - `egress::probe_default/probe_iface/probe_stun/select_best` 收 `targets: &[SocketAddrV4]`（已有形参）
     ⇒ 装配点（`engine.rs:263-300`、`:1850/1864`、`bindwatch.rs:185-195`）从 `ServeConfig` 取；
     `preferred_iface` 路由探针改用配置首项（默认等价 223.5.5.5:53）。
   - **env 缝纪律（保护既有不变量）**：`HOMEWAY_BINDWATCH_PROBE` **只影响 `WatchDeps::probe`（健康
     探针）**；**挑卡（`RealDeps::resolve`/`select_best`）只吃 config 值、不吃 env**——与
     `bindwatch.rs:185-190` 的「挑卡恒用真目标（probe 注入只模拟健康探针退化）」逐字一致，加断言测试。
3. **macOS 注记**：默认仍 `/etc/resolv.conf`（spec MUST 的跟随面不动）；macOS 部署用 `dns_upstream`
   覆盖（opt-in）。不做 `scutil`/私有 API（§3-D3）。

**涉及文件**：`homeway-cli/src/serve_cli.rs`、`homeway-core/src/server/{engine,dnsproxy,ddnscheck,egress,bindwatch}.rs`。

**风险**：① 配置面 +5 键（值域校验 + E2E 拒启）；② **两处 spec 偏离登记 + 上报**（`dns_upstream` ↔ `:40`
MUST；`dns_fallback` ↔ `:77` SHALL——评审 2-4）；③ 覆盖上游的**已知代价**（`dns_upstream` 在 fake-ip
主机上 ⇒ 手机拿真实 IP、与主 nameserver 不再一致 ⇒ 出口代理的域名规则对该流量失效）：**覆盖生效时打
一行告警**并写进文档与登记；④ 探针键一键三路的耦合（显式登记 C14/E21）。

**测试计划**：① 值域逐键（合法/非法 + 拒启文案 + 进程存活，E2E）；② `dns_upstream` 覆盖生效 + mtime
跟随关闭（本地假上游）；③ `dns_fallback` 生效（nameserver 全死，E22 `fallback` +1）；空串拒启；
④ `ddns_resolver` 穿参（mock UDP）；⑤ `dns_probe_target` 生效（本地 UDP 桩；C14 flags 随之变）；
⑥ **挑卡不吃 env**（断言：env 指向死地址时 `select_best` 仍用 config/默认目标）；
⑦ 缺省回归（不配任何新键 ⇒ `DnsConfig` 五字段与默认目标常量逐值相等）。

**判据行影响**：见 §6（E4 来源扩展；C14 + `UDP 默认路径` 行取值来源变化；两处 spec 偏离登记）。

### F3（P1｜修缺陷·观测统一）fake-IP 卫兵统一（单一实现；dnsproxy 只计数+告警，**不改应答**）

**方案**：
1. **检测器单一实现**：`is_fake_ip` 从 `ddnscheck.rs:79-88` 迁到 `egress.rs`（与 `is_public_addr`
   同域；`is_ula` 保持私有，仅内部使用），`pub(crate)` 最小可见性；ddnscheck / dnsproxy 各留一条
   「单一实现」断言。
2. **dnsproxy 观测**：`ResponderCore::respond`（`:588-617`）在 **`clamp_ttl`/`truncate` 之后**（=
   **实际交付**的应答）扫答案段 A 记录，命中 ⇒ 计数 `fakeip` + **每进程一次性**告警。
3. **不改应答字节**（spec `:50` MUST）；fake-IP **不触发**换上游/兜底（那是被 spec 要求的透传语义）。
4. **语义边界（如实写进告警与登记）**：
   - fake-ip 主机上该计数**常态增长**（每条 A 查询都可能命中）⇒ 它**不区分**「正常透传」与
     「上游被污染」，只作环境指示；
   - 真正的故障面在**拦截层到 198.18/15 的拨号**（出口绑物理卡、fake-IP 代理在 TUN 虚拟卡）——
     该面已有既有可观测量（`dialfail` 计数 + `ddnscheck` 的卫兵告警 `DdnsErr::Poisoned`
     「检查**绑卡**/代理」）；**拦截层专用计数不在本批**（登记残余）；
   - 配了 `dns_upstream`（覆盖成公共解析器）后手机拿真实 IP、与主解析器不再一致（§F2 风险③），
     此时 `fakeip=0` **不代表环境干净**——告警文案与 §6 登记写明。
5. ddnscheck 侧行为不变（仍在 `:232-250` 拒绝并分档）。

**涉及文件**：`server/{dnsproxy,ddnscheck,egress}.rs`。

**风险**：① RR 游走逻辑新增 ⇒ 复用 `skip_name` 纯函数 + fuzz 面（不 panic）；② 告警刷屏 ⇒
一次性位（与 `fb_once` 同形）+ 计数。

**测试计划**：① 纯函数（198.18/19 真、198.20/199.x 假、v6 恒假、CGNAT 不误判）；② mock 上游返回
fake-IP ⇒ 应答**逐字节透传** + 计数 +1 + 告警恰一次；③ 非 fake-IP 计数不变；④ 截断形态（大应答被
truncate）⇒ 计数按**交付后**报文（评审 2-5 的口径）；⑤ 「单一实现」断言。

**判据行影响**：E22 新增 `fakeip=%d`（行文 + 计数输入集变化）⇒ 登记（§6）；该行形态在 tier spec
「代答可观测性」段有描述 ⇒ 与 §8 上报合并知会。

### F4（P2｜加固·超 Go）UPnP 协议面：IGD:2 / ST 兜底 / 应答偏好 / `AddAnyPortMapping` / 钉卡降级

**方案**（既有成功路径**零变化**；新增面全部有界、可观测）：

1. **ST 家族（每轮固定三发，同一 socket）**：`ST=InternetGatewayDevice:1` →
   `ST=InternetGatewayDevice:2` → `ST=upnp:rootdevice`（`msearch_message(st)` 参数化）。
   `MX: 2` 不变（设备允许 2s 内随机延迟应答 ⇒ **不能**在单个 1200ms 内层窗口结束就下结论）。
2. **应答判定表（替代「首应答即信」，与 ST 兜底交叉显式定义）**：

   | 收到的应答 | 判定 | 动作 |
   |---|---|---|
   | 私网/环回来源 + `HTTP/1.x 200` + 非空 LOCATION（既有三条件）**且** `ST` 或 `USN` **精确段匹配** IGD 设备类型（`:1`/`:2`；USN 取 `::` 后段，含 `:`/`::` 边界——防 `:1` 误命中 `:10`） | **IGD 型** | **立即采信返回**（快路径） |
   | 三条件满足但非 IGD 型 | **待定** | 记为「待定首个」，**继续读**（不提前返回） |
   | 三条件不满足 | 拒 | 继续读（既有 F8 语义） |

   **「本轮/窗口」定义**：一次 M-SEARCH 的内层 recv 窗口（≤1200ms，`:287`）；**「SSDP 腿期限」
   = `min(deadline, now+5s)`**（`:267`）。**待定者只在「SSDP 腿期限」到点后**才回退采信（不在
   各轮窗口尽头提前返回——否则会抢在 MX=2 的真 IGD 应答之前并吃掉 attempt 2/3 的重发；
   评审 3-1/B9）。IGD 型缺失 + 待定缺失 + 期限到点 ⇒ `NoIgdResponse`（既有错误）。
3. **`AddAnyPortMapping`（仅末位兜底，不进缩租路径）**：
   - 触发：`select_external_port` 的 `prefer → internal_port → +1…+9` **全部失败之后**。
   - 形态：(a) 发往**同一个**已发现的 WAN 控制 URL，`SOAPAction = {发现到的 service_type}#AddAnyPortMapping`
     （`soap()` `:569` 同形；若发现的是 `WANIPConnection:1` ⇒ 401/500 即「不支持」）；
     (b) args 与 `add_with_lease`（`:635-647`）同序同 case，`NewExternalPort` = **期望端口**
     （prefer 非 0 否则 internal_port——**不依赖「0 = 通配」这一未证实解释**），由路由器改派并回报；
     (c) **假成功判定**：必须 200 + `NewReservedPort` 可解析且 ≠ 0，否则判失败；
     (d) **租期回退**：镜像 `add_with_lease` 的 3600 → 0（`:617-632`，不吃非 0 租期的机型上兜底才
       不形同虚设）；(e) **观测行**：成功 ⇒ logf 一行「UPnP：显式候选全失败，改用 AddAnyPortMapping
       由路由器选端口 {ext} → 内网 {internal_port}」；不支持/失败 ⇒ dlogf 一行后回落
       `NoPortAvailable`（既有错误语义与计数不变）。
   - **不进缩租路径**：缩租走 `re_add_short_lease`（不经 `select_external_port`）——与设计一致。
4. **钉卡降级**：`pin_multicast`（`:175-214`）③ 改为尽力而为：失败 ⇒ 记一行
   **「UPnP：SSDP socket 钉卡 <if> 失败（<e>）—— 发送已由 IP_MULTICAST_IF 钉在 <if>，但接收可能被
   默认路由/TUN 抢走（组播应答收不到），继续尝试」**（评审 3-5 的措辞订正），不终止候选；与
   `bind.rs:228-238` 的既有「钉不上卡不致命」先例同义（Linux 无 `CAP_NET_RAW` 时 EPERM）。
5. **不做**：描述文件 `deviceType` 白名单；`GetExternalIPAddress` 自检；不引 portmapper 依赖。

**涉及文件**：`server/upnp.rs`（SSDP + 端口选择 + pin 降级 + 单测）。

**风险**：① 组播 +2 包/轮（≤6 包/腿，可忽略）；② rootdevice 兜底命中非 IGD ⇒ 多一次描述抓取
（1 MiB 闸内）**消耗共享 40s 预算**——多候选机器上可能挤掉后续候选（如实登记，评审 3-3）；
③ 待定回退延后到腿期限 ⇒ 混答 LAN 环境下 SSDP 腿等满（≤5s，有界；真 IGD 形态走快路径零等待）；
④ 钉卡降级 = 语义放宽（接收可能收不到），有告警可归因。

**测试计划**：① `msearch_message(st)` 三态形状；② 应答判定纯函数四态（IGD:1 / IGD:2 / `:10` 不误命中 /
缺 ST+USN 待定）+ 待定回退时机（腿期限 vs 内层窗口，注入时钟）；③ `AddAnyPortMapping` mock IGD：
成功取 `NewReservedPort` / 缺 `NewReservedPort` ⇒ 判失败 / 401 不支持 ⇒ `NoPortAvailable` /
租期 3600 失败回退 0；④ 钉卡降级（不可钉候选 ⇒ 不终止 + 告警行 + 流程继续）；⑤ 回归：既有 mock IGD
全链、候选序断言、`clean_mappings`/`allow_evict` 语义不变；⑥ 缩租路径不受 A-any 影响（断言）。

**判据行影响**：§6（E20 族数值语义 + 两条与 Go 的有意分歧 + 新增告警行）。

### F5（P2｜修缺陷·平台纠偏）`if_nametoindex` 完整语义（含三处 index 键面）

**方案**：
1. **平台正确的可钉判定**：`pin_socket_to_iface`（`egress.rs:208-217`）的 `index == 0` 硬拒改
   **darwin 专属**（`IP_BOUND_IF=0` = 解绑 ⇒ 必须拒）；**linux 按名**（`SO_BINDTODEVICE`，index
   不参与 ⇒ 不再拒 `index==0`）。函数文档写明「darwin 0=解绑 / linux 按名，index 仅诊断值」。
2. **注释与告警平台化**：`IfaceInfo.index_ok` 文档（`:113-116`）与 `:173-175` 的告警改为平台条件句
   （darwin：不可钉；linux：按名可钉）——否则 F5 后旧注释在 linux 上变假话（评审 4-2）。
3. **候选面**：`physical_candidates()` 在 **darwin** 排除 `!index_ok`（不可钉不参与挑卡）；
   linux 不过滤。E21 渲染形态不变（`index=%d`，Q-C 已登记）。
4. **调用方语义**：
   - 探针（`probe_with` `:441-450`）：钉失败 ⇒ **该候选失败**（保持「探针必须从该卡发出」的判据，
     不降级为未钉探针）；失败原因保留 index 文案可归因。
   - UPnP：F4-4 降级。
   - `bind.rs`：打开期（`:228-238`）/ Repin（`:668-671`）已有降级，不改。
5. **三处 index 键面纠偏**（评审 B2；`IfaceFingerprint` 仍**保留** index 字段 ⇒ 「换 index ⇒ 指纹变
   ⇒ 重钉」信号不丢，评审已核此前提）：
   - `bindwatch::RealDeps::state_of`（`:198-206`）：回查 live 由 index 键改 **name-only**
     （找不到 = down + 空地址集 ⇒ 现有 None 分支语义）；抽**纯函数** `state_of_from(&[IfaceInfo], &IfaceInfo)`
     供单测（消除「直连内核无注入缝」）。
   - `bindwatch` 重挑比较（`:144-152`）：`cur.index == next.index` 改 `iface_same(cur, next)`
     （name 相等优先；index 仅双方有效时作补充）。
   - `egress::select_best`（`:568-576`）的「默认路由卡」偏好：同改 `iface_same`（index=0 多卡不互撞）。
   - 统一小 helper `iface_same(a, b)`（`pub(crate)`，name 优先）。
6. **不做**：不把 `index: u32 + index_ok: bool` 改成 `Option<NonZeroU32>`（面大、收益仅罕见形态；
   评审建议的备选路径，登记为「考虑未采纳」）；不改 Q-C 的告警形态。

**涉及文件**：`server/egress.rs`、`server/bindwatch.rs`、`server/upnp.rs`（F4-4）。

**风险**：① linux 语义放宽（`index==0` 可钉）——按名绑定与 index 无关，属纠偏；② darwin 过滤候选后
可能报「没有候选物理网卡」（既有文案）；③ 指纹/偏好比较改动影响看护触发面 ⇒ 测试钉死。

**测试计划**：① 钉卡平台分档（darwin 拒 / linux 不按 index 拒）；② 候选过滤平台分档；
③ `state_of_from` 纯函数（两张 index=0 卡不串键、名字消失 ⇒ down、换 index ⇒ 指纹变）；
④ `iface_same` 三态；⑤ bindwatch 既有六测试全绿；⑥ UPnP 降级（F4-4 同测）。

**判据行影响**：E21 行文不变；登记「linux 上 `index=0` 不再蕴含不可钉」（§6）。

### F6（P2｜Rust 独有语义精确化；Go **无 state 维度**）launchd 探测精确化

> 定性订正（评审 ⑤）：Go `spawn.go:259-277` **完全不看 state**、无 plist 解析 ⇒ 本项既非「对齐 Go」
> 亦非「修 Go 偏差」，而是**在 Rust 自己的 Q-H 收窄上再精确化**。

**方案**：
1. **plist 解析（手写 XML 子集，不引依赖）**：从 `<key>ProgramArguments</key>` 取 `<array>` 的
   `<string>` 序列（容忍制表/换行/引言 DOCTYPE）。**白名单纪律**：顶层 `<string/>` 自闭合、CDATA、
   注释、嵌套 `<array>`、未定义实体 ⇒ **一律判「未知形态」**走保守退化（评审 4-7/6-3）。
2. **匹配规则**（目录清单与 state 均可注入的纯函数）：
   - **提及该 state**：`ProgramArguments` 里 `--state <DIR>` / `--state=<DIR>`，且 `DIR` 与请求 state
     经**同一 normalize**（两侧共用：`canonicalize` 成功用其值，失败退化字面 + 去尾 `/`；评审 4-6/
     4-1）相等 ⇒ **相关**；多命中取首个（记行）；
   - **未提及任何 state（零参形态）**：`请求 state == default_state_dir()` **且该 plist 文件名含
     `homeway`** ⇒ **相关**（= Q-H F14 旧判据，**不引入 argv[0] 谓词**——评审 4-4：现行对**任何**
     `*homeway*.plist` 都等 4s，加谓词属未登记收窄）；
   - **解析失败 / 二进制 plist**：保守退化——请求 state 为默认 ⇒ 相关（旧行为）；非默认 ⇒ 不相关
     （旧行为）；不放大行为变化；
   - 非默认 state + plist 提及的是**别的** state ⇒ 不相关（F14 偏差的修复面）。
3. **接线与文案**：`detect_launchd_agent_for_state(state) -> Option<String>`（Label **仍取文件名去
   后缀**，对齐 Go/现状，评审 4-5）；等待分支文案不变（CA11「（launchd 代理 {label} 在册）」）；
   **非相关分支文案改写**（评审 4-3）：现文案「（state=… 非默认 state——不等 launchd KeepAlive，
   直接拉起）」在「默认 state 但无相关 plist」时会说假话 ⇒ 改
   **「（未在册 launchd 代理提及 state={}——不等 launchd KeepAlive，直接拉起）」**（登记文案 + 触发集变化）。
4. **不做**：不改 4s/10s 期限；不调 `launchctl`（纯文件读）。

**涉及文件**：`homeway-cli/src/daemon_cli.rs` + 单测夹具。

**风险**：① plist 形态多样 ⇒ 白名单 + 保守退化（最坏 = Q-H 行为）；② 目录不可读 ⇒ 逐目录 `continue`；
③ **与 `role-management:184-188` 的「或本机任意 homeway 代理 plist」检测集合有 spec 张力** ⇒
登记 + **上报**（§8）：F6 收窄「等 KeepAlive」的判定面（不是收窄 CA11 的行本身），tier spec 若坚持
广义检测需修订或确认。

**测试计划**：① 纯函数四态（自定义 state 命中 / 提别的 state / 零参+默认 / 零参+非默认）；
② 恶化两态（二进制 `bplist00` / 坏 XML）；③ **生产实机形态逐字夹具**（评审 4-7 实采；
经 DOCTYPE + 制表符缩进）：
   ```
   ProgramArguments = ["/Users/zhaozhe/bin/homeway-rs", "--state", "/Users/zhaozhe/.config/homeway-rs"]
   Label = me.zhaozhe.homeway-exit（文件名去后缀）
   ```
   断言：请求 `~/.config/homeway-rs` ⇒ 相关（label 命中）；请求其它 state ⇒ 不相关；
④ `--state` 空格分隔 / 等号分隔 / 相对路径 normalize；⑤ 集成分支（`dial_control_spawn` 命中/未命中，
   含新文案）。

**判据行影响**：CA11 触发集 + 非相关分支文案变化 ⇒ 登记（§6）。

### F7（P2｜盘点·文档）平台假设清单与残余登记

**方案**：§0.5 表即交付物（含设计门补的三行）；实现批在 `docs/INTEROP-CRITERIA.md` 引用本批相关登记，
批记录 `QJ.md` 复述「已处置/已勾选/不动」三类。
**不做**：合法平台分支（PATH/shell/dscl/ps/proc/fd 语义/sun_path）不动、不抽象。

---

## 2. keyenc 兼容性论证（F1 专章）

### 2.1 缺省形态（旧对端逐字节不变）

- 未声明两条新 caps 位（所有现役客户端、`term_cli` raw 腿、`fixtures` 向量宿主）⇒ `host_default()`，
  与今日 `IS_DARWIN` **逐字节同值**。
- 向量 `fixtures/vectors/term_keyenc.json`（387 案，darwin 宿主产出）在 darwin 出口继续对拍；
  Linux/OHOS 构建的缺省仍是非 darwin 口径（与 Go 基线 `pkg/term/vt` 的编译期分支同形），不引入新差异。
- 尾随块形状不变（仅 bit 2/3 的组合语义新增）⇒ 旧出口（不认识新位，OR 后按 mask 用）与新出口
  （旧客户端不发新位）双向兼容；`PROTO_VER` 门不动。

### 2.2 兼容矩阵（**含「旧出口」格**——设计门 E-2/1-1 订正）

| 客户端 | **出口 ≥ 本批** | **旧出口（< 本批）** |
|---|---|---|
| 旧客户端（不发新位） | 宿主推断 = 今日行为（逐字节同） | 宿主推断 = 今日行为（逐字节同） |
| 新客户端 · `KEY_ALT_NO_ESC_PREFIX` | 恒 darwin 口径 | 新位被忽略 ⇒ 宿主推断（在 Linux 出口 = 非 darwin 口径） |
| 新客户端 · `KEY_ALT_ESC_PREFIX` | 恒非 darwin 口径 | 同上（声明不生效） |
| 新客户端 · 两位同置（含裸 ID 形态） | 未声明（宿主推断）+ 计数 + 一次性告警 | 宿主推断 |

⇒ **声明的生效前提 = 「客户端置位」且「出口 ≥ 本批」**；两台生产出口滚动升级是用户触点。

### 2.3 HarmonyOS 语义取证

| 证据 | 内容 | 指向 |
|---|---|---|
| E1 客户端键源（tier，只读） | `input/key_encoder.cpp:165-197`：文本只按 `shift/capsLock` 生成 US 布局字符，**Alt 完全不参与**；`:199-213` `TranslateOhosKey` 只翻译 ctrl/shift/alt/caps（**不产 super**） | Alt 在客户端**不承担产字符职责**（darwin 口径的立论前提 = option 参与产字符）⇒ 非 darwin |
| E2 客户端历史语义（tier git，只读） | surface 迁移前（`a2f8c5e~1`）客户端**本地**调用 libghostty 编码（`BuildKeySequence`）；该库 = **aarch64-linux-musl** 预编译（`git cat-file` 实测 `…/libc/include/aarch64-linux-musl/bits`）⇒ `builtin.os.tag != .macos` ⇒ **非 darwin 分支** | 非 darwin；surface 迁移后 macOS 出口上的「alt 直发文本」= **新引入的回归** |
| E3 生产形态差异 | 阿里云（Linux）出口今天就是非 darwin 口径；Mac 出口是 darwin 口径 ⇒ 同一台手机两端体验不一致（审计 P1 核心） | 统一到非 darwin = 「只改 Mac 出口这一端」，与生产 Linux 出口一致 |
| E4 ghostty 真源 | vendored `input/key_encode.zig:300-308/402-410/545-547/565-575` 四处 darwin 门；fixterms `:489` **无**平台门 | 口径应与**客户端宿主**一致，而非出口；Rust 侧与 `keyenc.rs:812-829` 同形 |
| E5 审计表述 | 「HarmonyOS 走 linux 分支是错误假设」 | **复验订正**（§0.3-1）：机制（不可推断）成立；值（应为 darwin）证据不支持 |

**结论**：机制 = 客户端声明（F1）；**建议 App 声明 `caps::KEY_ALT_ESC_PREFIX`**。该声明由 **tier 侧**
跟做（本仓无 surface 客户端；`term_cli` 是 raw 腿、发 `Op::DATA`，只声明 `RAW_TERMINAL`，`:975-986`）。
**F1 给 tier 客户端新增义务**（caps 置位），而 `term-surface-protocol` spec `:172-174` 现无 Alt/口径
条款 ⇒ **需 tier 补 spec 条款**（§8 上报项）。

### 2.4 分叉面清点（默认形态下仍存在的差异，如实登记，不宣称已消除）

- `alt+文本键`：darwin `text` / 非 darwin `ESC text`；
- mok2（mode 2）修饰码：darwin 剥 alt 位；
- kitty REPORT_ASSOCIATED 文本：darwin 下 alt 不阻文本；
- `super+键`：darwin 无文本（对 App 不可达——E1 证明客户端不发 super）。

⇒ 分叉消失需**两个前提**：客户端置位 + 两台生产出口升级（用户触点）。

---

## 3. 「二选一」类决策的取证与裁定（本棒自裁 + 理由）

### D1 keyenc：缺省口径 = 宿主推断（兼容） vs 改默认为「surface 腿 ⇒ 非 darwin」

**裁定：宿主推断（兼容）**。理由：① 本批范围明文「缺省行为兼容既有部署」；② Go 基线同形
（编译期分支），改默认 = 主动偏离对齐；③ 默认变更影响两台生产出口现役行为（判据政策下需登记且属
行为差异）；④ 声明机制已就位，tier 触点一只 bit 的成本。**备选与触发条件**（留痕）：若主会话要求
「不等 tier 立即消除分叉」，备选 = surface 腿在未声明时取非 darwin 口径（只改 Mac 出口，与 E2/E3
一致）——需登记行为差异 + 与 Go 基线的有意分歧；本棒不自行采用。

### D2 fake-IP 卫兵动作：**透传 + 计数/告警** vs 拒绝 + 换上游 + SERVFAIL

**裁定：透传 + 计数/告警**。硬证据：tier `wg-native-dns:50` 明文要求「与主 nameserver 一致
（**含 fake-ip 地址**）」⇒ 拒绝式违反 MUST；且 fake-IP 透传在本架构下是可用语义（手机发往 198.18/15
的连接经隧道到出口，出口默认路由交给本地 fake-IP 代理还原域名）。拒绝式登记为备选（需 tier spec 修订）。

### D3 DNS 上游覆盖键（`dns_upstream`）：加（opt-in，登记偏离） vs 不加

**裁定：加**。理由：① 审计明文「上游列表…配置化」；② macOS 出口的 resolv.conf 非真源，`dns_upstream`
是**等价能力**的收口；③ 默认（不配置）完全保持 spec MUST 的跟随语义 ⇒ 偏离仅发生在显式 opt-in。
**「不做」的理由收窄**（评审 2-8）：(a) 私有 API `dns_configuration_copy()` 跨版本结构布局不稳；
(b) 公开路径 `scutil --dns` 需子进程且粒度与 spec 秒级跟随不匹配，收益仅覆盖「主解析器列表」这一
spec 已限定的小面 ⇒ opt-in 覆盖是等价能力。**登记 + 上报**（§8）。

### D4 UPnP `AddAnyPortMapping` 的位置：末位兜底 vs 提前到 `+1…+9` 之前

**裁定：末位兜底**。理由：既有候选序承载「路由器表认领（端口记忆）→ 同号」语义（跨重启稳定、便于
人工核账）；路由器自选端口把稳定性让给设备随机性。末位兜底 ⇒ 修前后成功路径**零变化**。

### D5 launchd 未知形态（二进制/解析失败）的兜底

**裁定：保守退化为 Q-H F14 行为**（非默认 state 不等；默认 state 等）。本批目标是修「实机自定义 state
误判」，不重定义未知形态。

### D6 `if_nametoindex` 平台语义

**裁定**：darwin 需要有效 index（`IP_BOUND_IF`；0=解绑）；linux 按名（`SO_BINDTODEVICE`，index 不参与）
⇒ 守卫平台化，而非全平台硬拒 0。理由 = 内核语义事实（`egress.rs:247-263` + 负例 `:712-733`）。

### D7 `E22` 新增 `fakeip=` 字段（行文变化） vs 复用既有字段/仅日志

**裁定：新增字段 + 登记**。理由：与 Q-B/Q-C「观测 additive」先例一致；fake-IP 命中率是环境诊断输入；
行文变化按政策登记（非静默破坏）。计数输入集 = **交付给客户端的应答**（truncate 之后）答案段 A 记录
命中数（每应答至多 +1）。

### D8 探针键：一键喂三路（挑卡/健康/C14） vs 拆键

**裁定：一键 + 显式登记耦合**（评审 2-1 的次选）。理由：三者的谓词相同（「这条路能不能到 DNS:53
公共解析器」）；拆键多一个配置面，与本批「尽量少加面」相悖；耦合后果（把键指到死地址 ⇒ C14 记
「不可用」+ 挑卡失败）本身就是**诊断语义**，登记后不会成为暗坑。**env 缝纪律**（评审 2-2）：env 只
影响健康探针，挑卡恒吃 config/默认（保护 `bindwatch.rs:185-190` 既有不变量）。

### D9 caps 位语义命名与歧义处理

**裁定**：按**语义**命名（`KEY_ALT_NO_ESC_PREFIX`/`KEY_ALT_ESC_PREFIX`，enum `KeyFlavor::{AltEscPrefix,
AltNoEscPrefix}`，注释写明等价 ghostty 的编译期分支）；**歧义（同置）按「未声明」处理（fail-soft）而
非拒腿**——「裸 ID 尾随块」与 caps 块同形不可判别（`frames.rs:858-861` 既有测试即 `caps=0x7F`），
拒绝面会把今天可服务的腿打死。**不采纳**「bit2=声明存在/bit3=取值」的备选：该形态下裸 ID 块会被读成
「显式 darwin」，在 Linux 出口上**静默改变语义**（比歧义更糟）。

---

## 4. 范围边界确认（不做项，逐条留痕）

| 不做 | 理由 |
|---|---|
| DNS TTL 缓存 / 每查询缓存 | **Q-I-DNS 小批**（REVIEW-ROADMAP 状态表） |
| portfwd 实装监听器 | **Q-F-B 批** |
| keyenc 布局字段（`unshifted_codepoint`/per-layout 声明） | wire 无该字段；需协议扩展 + tier 采纳（残余登记） |
| `option_as_alt` 的 `.true/.left/.right` 三档 | wire 无字段 ⇒ 恒 .false（残余登记） |
| macOS 系统 DNS 真源自动读取（`scutil`/`dns_configuration_copy`） | D3（私有 API 不稳 / 子进程粒度不匹配） |
| fake-IP 拒绝式卫兵 | D2（违反 tier MUST） |
| 拦截层「目的 ∈ 198.18/15」专用计数 | 本批范围外（既有可观测量：`dialfail` + ddnscheck 卫兵告警）；登记残余 |
| UPnP 描述文件 `deviceType` 白名单 / `GetExternalIPAddress` 自检 | 收益低、依赖设备树形态（模块头已论证） |
| `IfaceInfo.index` 改 `Option<NonZeroU32>` | 面大、收益仅罕见形态（评审建议备选，登记「考虑未采纳」） |
| 两台生产出口滚动升级 / tier pin 前进 | AGENTS：用户触点 |
| 给新配置键加 CLI flag | 配置面足够（登记「不加」） |

---

## 5. 测试与验收计划（汇总）

| 面 | 用例 | 判绿口径 |
|---|---|---|
| F1 向量 | 387 案 parity **全平台**跑（显式 `KeyFlavor::AltNoEscPrefix`；`#[cfg(macos)]` 门删除） | 逐字节全绿 |
| F1 分支 | 非 darwin 期望表（8 案；源码行级真源，显式登记「无夹具」） | 逐字节钉死 |
| F1 集成 | HELLO 五态 + 裸 ID 形态（不拒腿）+ 两腿同键事件分叉 + 腿缺失丢弃计数 | 语义与计数正确 |
| F2 配置 | 5 键值域（合法/非法、`dns_fallback` 空串拒启）+ E2E 拒启 + 进程存活 | 非法拒启、合法生效 |
| F2 行为 | 覆盖/兜底/自检/探针生效 + **挑卡不吃 env** + 缺省逐值回归 | 缺省与常量逐值相等 |
| F3 | 纯函数 + 透传不改写 + 交付后计数 + 告警恰一次 + 单一实现断言 | E22 新字段语义正确 |
| F4 | ST 三态 / 判定表四态 / 待定回退时机 / A-any 四态 / 钉卡降级 + 既有全链回归 | 既有零变化 + 新面全覆 |
| F5 | 平台分档钉卡 + 候选过滤 + `state_of_from` 纯函数 + `iface_same` + bindwatch 六测 | 分档断言 + 无串键 |
| F6 | plist 四态 + 退化两态 + 生产夹具逐字 + normalize 两侧共用 + 集成分支（新文案） | CA11 触发集正确 |
| 门 | `cargo test --workspace` 全绿（flake 甄别按 ROADMAP 口径）；`cargo clippy --workspace --all-targets -D warnings` clean；`tools/check-vocab.sh` PASS（**新 config 键不在词表五族内：speedtest-reason/portfwd/event-payload/files-proto** ⇒ 不受影响，需实测确认）；linux/musl 交叉 check（触 `cfg(target_os)` 与 OHOS 面 ⇒ 必跑） | 批协议第 3 条 |

---

## 6. 判据行影响清单（汇总；实现批同步 `INTEROP-CRITERIA.md`）

| 面 | 判据/行 | 变更 | 类型 | 登记要点 |
|---|---|---|---|---|
| F1 | term 键编码（真源 = `fixtures/vectors/term_keyenc.json` 387 案 + D-10 记录） | 平台口径由「出口编译宿主」→「客户端 HELLO 声明；未声明/歧义 = 宿主推断」 | 机制（本仓）+ 行为差异（声明生效时） | 默认逐字节不变；新 caps 位（bit2/3 语义命名、**歧义 fail-soft**）；声明 `KEY_ALT_ESC_PREFIX` 在 macOS 出口上 `alt+文本`：`text` → `ESC text`；旧对端策略 = §2.2 矩阵；**D-10「按宿主条件编译」由本条取代**（宿主推断降为缺省）；**非 darwin 口径无夹具**（源码行级真源） |
| F2 | **E4** `dns 代答就绪：… upstream=%s` | `dns_upstream` 生效时上游来源 = 配置列表（默认 = resolv.conf 列表） | 来源扩展（默认不变） | 行文不变；默认样例不受影响 |
| F2 | **C14** `出口能力：… DNS:53 %s / 通用（非 53）%s …` + 未编号行 `UDP 默认路径：…` | **取值来源变**：`dns_probe_target`/`stun_probe_target` 生效时探针目标 = 配置值（修前硬编 `&[]` = 默认常量） | **计数/取值输入集变化（行文不变）** | 行文逐字不变；默认逐值不变；耦合写明（一键喂挑卡/健康/udpcap 三路） |
| F2 | 新增 config 键 5 个 | 无 → 有（默认不变） | additive（配置面） | `dns_upstream` = 对 `wg-native-dns:40` MUST 的 **opt-in 偏离**；`dns_fallback` = 对 `:77` SHALL（223.5.5.5）的 **opt-in 偏离**；`ddns_resolver`/`dns_probe_target`/`stun_probe_target` = **无 spec 面**（37 份 spec 中 223.5.5.5 仅出现在 wg-native-dns）；另：新键使 config.toml 对 Go 侧（`Undecoded()` 检查）单向不兼容（Go 已退役，仅影响回滚/对照）——一并登记 |
| F3 | **E22** `dns: q=… aaaa-mixed=%d` | **新增 `fakeip=%d`** | **行文变化 + 计数输入集变化** | 输入集 = **交付**应答（truncate 后）答案段 A 记录命中 198.18.0.0/15 的应答数（每应答 ≤+1）；数值语义 = 单调累计；**不以它判「污染与否」**（常态增长） |
| F4 | E20 族（UPnP 推断路径数值） | `AddAnyPortMapping` 兜底成功 ⇒ 外部端口由路由器改派（修前该形态 `NoPortAvailable`） | 行为注记（数值语义） | 无行文变化；新增两行 additive 告警（钉卡降级 / A-any 成功） |
| F4 | 与 Go 的**有意分歧**（逐条） | ① SSDP 钉卡失败：硬失败 → **降级继续**；② M-SEARCH ST：单发 IGD:1 → **IGD:1/2 + rootdevice 兜底**；③ 应答采纳：「首应答即信」→ **IGD 型优先/待定延后**；④ 新增 A-any 末位兜底（Go 无） | 超 Go 加固（登记） | 逐条列出，附失效形态影响（多候选/混答 LAN） |
| F5 | **E21** 绑卡族 | linux：`index=0` 不再阻止钉卡（按名）；darwin：候选面过滤不可钉卡；三处 index 键面改 name 优先 | 平台语义订正 | 行文与渲染形态不变；「linux `index=0` 不再蕴含不可钉」写明 |
| F6 | **CA11** `守护进程未运行（launchd 代理 %s 在册）——等 KeepAlive 重拉…` + 非相关分支文案 | 判定输入集：「仅默认 state」→「plist **内容**提及该 state（零参默认形态额外要求文件名含 homeway；未知形态保守退化）」；非相关分支文案改「未在册 launchd 代理提及 state=…」 | 输入集变化 + 文案变化 | 与 Q-H F14 登记同族修订；生产实机形态（自定义 `--state`）现在正确命中；**与 `role-management:184-188` 检测集合的 spec 张力** ⇒ 上报 |
| — | CA13 / E20a / E22 既有字段语义 | 不触 | — | 明示「不受影响」 |

---

## 7. 设计门记录（dsh 外部评审）

### 7.1 轮次与执行事实

| 项 | 值 |
|---|---|
| 本棒调用（外层） | **`/tmp/dsh-review/r23.yzpO66/`**（`prompt.txt` / `output.md` / `stderr.log`）；命令 `dsh --profile headless`（仓根执行）⇒ **`exit=0`**（前台捕获，`echo exit=$?` 落 stdout 日志） |
| 评审方的**内层**轮次 | **`/tmp/dsh-review/r23.u6neEC/`**（dsh 评审 agent 自行按 `reviewer` skill 姿势再起一轮评审并落盘：`prompt.txt` 6482B / `output.md` 18974B / `stderr.log` 182921B）——两轮**同属一次设计门**（外层输出 = 内层评审的转述 + 外层 agent 的独立复核判断） |
| 仓内副作用 | dsh 评审 agent 在仓内**新建** `docs/reviews/QJ-design-review.md`（36406B，untracked，非本棒产物）⇒ **已删除**；删除前**原样转存** `/tmp/dsh-review/r23.yzpO66/dsh-side-effect-review-file.md`（按批协议，设计门结果只落 `/tmp` 与本设计文档 §7） |
| 原始材料 | 外层 `output.md` 30618B（187 行，已 Read 全量）；内层 `output.md` 已 Read 全量；内层完整记录（36KB 版）已 Read 并转存 `/tmp`（见上格）后从仓内删除 |

### 7.2 评审原文摘要（不改写）

**总体结论（原文）**：「**改后过。** 机制与取证方向都站得住、无越界、无 wire 破坏；但有 **1 条判据
登记面的硬错误** + 一批中危设计缺口，必须先补语义/补登记/补注入缝再进实现。」

- **事实性错误 5 条**：E-1 C14 取值由本批新键驱动、§6「不触 C14」是硬错误（高）；E-2 §2.2 兼容矩阵
  与 §2.1「旧出口忽略新位」自相矛盾（中）；E-3 `is_ula` 私有、ddnscheck 未复用（低）；E-4 迁移前库为
  aarch64-linux-musl 而非「OHOS 编译」（低）；E-5 spec fake-ip 句行号（低）。
- **① wire（F1）**：机制成立、双向容忍成立；**「两位同置即拒腿」未定落点**（`FrameError` 无该变体），
  且**外层 agent 另查出**：既有测试 `frames.rs:858-861` 的「裸 ID 形态」解析出 `caps=0x7F` **恰含
  bit2|bit3** ⇒ 拒腿会把今天可服务的腿打死（建议改 fail-soft）；腿查不到时的回落语义未定（禁回落）；
  命名把 OS 名写进语义位；`IS_DARWIN` 测试/注释落点漏列；非 darwin 口径缺夹具登记。HarmonyOS 取证
  **被独立复核确认**（客户端文本无 alt、迁移前本地库非 darwin）。
- **② 配置化（F2/F3）**：不配置时**逐字/逐值一致成立**；但 `probe_target` 一键喂三路耦合未登记
  （高，与 E-1 同源）、与 `bindwatch.rs:185-190` 不变量冲突（挑卡不得吃 env）、`dns_fallback` 空串被
  `filled()` 静默换回 223.5.5.5、**漏登记 spec `:77` SHALL 偏离**、F3 计数落点未定、fake-IP 对抗面
  （拦截层拨号面 / 常态计数信噪比 / 覆盖后 `fakeip=0` 假干净）未写、配置项应在边界解析成型。
- **③ UPnP（F4）**：各条失败路径基本自洽、与 `allow_evict/verify` 及缩租路径不冲突；但待定/rootdevice/
  窗口语义交叉未定义 + **MX=2 与 1200ms 内层窗的抢跑机制**、A-any 的 SOAP 形态只写一半（同控制 URL /
  service_type 401 / 假成功 / 租期回退 / 观测行）、与 Go 的两条有意分歧未登记、钉卡降级文案。
- **④ 平台（F5/F6）**：D6 纠偏与内核语义一致；name-first 不丢「换 index 重钉」信号（前提已核）；
  但 index 键面**三处**没收干净（`bindwatch:202`、`bindwatch:148`、`egress:573`）、`index_ok` 注释在
  linux 上失真、F6 的 CA11 文案会在默认 state 说假话、F6 零参规则对 Q-H 未登记收窄、Label 来源、
  请求侧 normalize、夹具缺；**F6 与 `role-management:184-188` 检测集合有 spec 张力**。
- **⑤ Go 对齐**：六行性质表（F1 同形保持 / F2 超 Go 配置化 / F3 与 Go 同形 / F4 整条超 Go /
  F5 修 Rust 多余硬拒 / **F6 既非对齐亦非修 Go 偏差**——Go 完全不看 state）。
- **⑥/⑦**：enum + 单点收口方向正确；唯一「Go 味」= 裸 `Vec<String>` 配置 + 手写 XML（可接受配白名单）；
  **无越界**（未做缓存/portfwd，未触 tier/homeway/baseline/生产出口）；唯一需上报 = ROADMAP:180
  「keyenc 平台/**布局**字段化」的「布局」被收窄（章程条目收窄）。
- **看过没问题 8 条**：复验行号绝大多数命中；Q-C 四条勾选手续正确；判据政策分类正确（**C14 归属例外
  见 E-1**）；F2 缺省等价逐条一致；F3 的 spec 理解正确；F5 指纹前提写对；F1 的 wire 附加性成立；
  §4 范围边界干净。

### 7.3 逐条处置表（评审意见 → 处置；全部并入本 v2）

| 编号 | 意见（摘要） | 处置 | 落点 |
|---|---|---|---|
| **E-1** | C14 取值由本批新键驱动；§6「不触 C14」是硬错误 | **认同（高）** | §6 新增 C14 + `UDP 默认路径` 行「取值来源变化」登记；F2 风险④ |
| **E-2** | §2.2「任何出口」与 §2.1 矛盾 | **认同** | §2.2 改「客户端 × 出口（新/旧）」矩阵 + 单列旧出口行；§2.4 补两个前提 |
| **E-3** | `is_ula` 私有、未被 ddnscheck 复用 | **认同** | §0.3-4、F3-1 措辞订正 |
| **E-4** | 迁移前库为 aarch64-linux-musl（非 OHOS 目标） | **认同** | §2.3 E2 措辞（`git cat-file` 实测复现） |
| **E-5** | spec fake-ip 句行号 `:50` | **认同** | §0.2-2、§0.3-4 行号订正 |
| 1-1 | 兼容矩阵表头 | **认同** | §2.2 |
| 1-2 | 两位同置的落点/承载未定 | **认同并改方案**：歧义 ⇒ **fail-soft（未声明 + 计数 + 一次性告警）**，落点 = `serve_hello`（`dec_hello_tail` 保持纯形状；不新增 `FrameError` 变体） | F1-1、D9 |
| 1-3 | 腿查不到时的语义未定 | **认同**：丢弃输入 + 一次性计数；**任何路径禁回落宿主推断** | F1-3、F1 测试⑤ |
| 1-4 | 命名/位扩展性 | **认同（采用语义命名）**；「存在+取值」备选**不采纳**（裸 ID 形态会静默变 darwin——比歧义更糟） | F1-1、D9 |
| 1-5 | `IS_DARWIN` 全部落点漏列（测试 2 + 注释 5） | **认同** | F1-4 写明「生产 4 + 测试 2 + 注释 5」 |
| 1-6 | 非 darwin 口径无夹具 | **认同**：补**非 darwin 期望表**（8 案）+ 显式登记「无夹具，源码行级真源」 | F1 测试②、§6 F1 行 |
| 1-7 | §2.4 需补「出口已升级」前提 | **认同** | §2.4 |
| 2-1 | `probe_target` 一键喂三路（高） | **部分认同**：不拆键（三路谓词相同、少加配置面）⇒ **显式登记耦合**（C14/E21）+ 告警/文档；env 缝纪律另立 | D8、F2-1、§6 C14 行 |
| 2-2 | 挑卡不得吃 env（与 `bindwatch.rs:185-190` 冲突） | **认同**：env 只影响健康探针；挑卡恒吃 config/默认 + 断言测试 | F2-2、F2 测试⑥ |
| 2-3 | `dns_fallback` 空串 + `filled()` 静默替换 | **认同**：**空串 = 拒启**（值域），`filled()` 降为不可达的内部防御 | F2-1、F2 测试③ |
| 2-4 | 漏登记 spec `:77` SHALL 偏离 | **认同**：与 `dns_upstream` 同批登记；另三键注明「无 spec 面」 | §6 F2 行 |
| 2-5 | F3 计数落点 | **认同**：= 交付应答（truncate 之后） | F3-2、D7、§6 |
| 2-6 | fake-IP 对抗面 (a)(b)(c) | **认同（部分实现）**：告警/登记写入 (a)(b)(c)；拦截层专用计数**不做**（登记残余） | F3-4、§4、§6 E22 行 |
| 2-7 | 配置项应边界解析成型 | **认同**：`Vec<SocketAddrV4>`/newtype，非法即拒启；不得 `filter_map(parse().ok())` 静默丢项 | F2-1、§5 |
| 2-8 | D3 理由收窄 | **认同** | D3 |
| 3-1 + B9 | 待定/根设备/窗口交叉 + MX=2 抢跑 | **认同**：给出判定表；「本轮」= 内层 recv 窗口；**待定者只在 SSDP 腿期限到点才回退**（不抢 attempt 2/3） | F4-2、F4 测试② |
| 3-2 | A-any SOAP 形态 (a)–(e) | **认同**：全部补齐（同控制 URL/service_type、args 同序、假成功判定、租期回退、观测行）；**不依赖「0=通配」**——用期望端口 + 接受改派 | F4-3 |
| 3-3 | rootdevice 预算影响 | **认同** | F4 风险② |
| 3-4 | 与 Go 有意分歧未逐条登记 | **认同**：四条逐列 | §6 F4 行 |
| 3-5 | 钉卡降级文案 | **认同**（发送已钉 / 接收可能被抢） | F4-4 |
| 3-6 | USN/ST 精确段匹配 | **认同**：精确段匹配（`:1`/`:2`/`:10`/缺 四态测试） | F4-2、F4 测试② |
| 4-1 | name-only + 纯函数注入缝 | **认同**：`state_of_from` 纯函数；删「index 次之」 | F5-5、F5 测试③ |
| 4-2 | `index_ok` 注释在 linux 失真 | **认同**：平台条件句 + 登记 | F5-2、§6 E21 行 |
| 4-3 | CA11 非相关分支会说假话 | **认同**：文案改写 + 触发集登记 | F6-3、§6 CA11 行 |
| 4-4 | 零参规则对 Q-H 的未登记收窄 | **认同**：默认 state 下**不引入 argv[0] 谓词** | F6-2 |
| 4-5 | Label 来源 | **认同**：仍取文件名去后缀 | F6-3 |
| 4-6 | 请求侧 normalize | **认同**：两侧共用同一 normalize | F6-2 |
| 4-7 | 夹具缺 + 白名单纪律 | **认同**：逐字夹具 + 五条白名单纪律 | F6-1/测试③ |
| 4-8 | 盘点漏三行 | **认同**：三行补进 §0.5（含 `option_as_alt ≡ .false` 假设本身） | §0.5 |
| 6-1 | 配置类型化 | **认同**（= 2-7） | F2-1 |
| 6-2 | KeyFlavor 命名 | **认同**（= 1-4） | D9 |
| 6-3 | 手写 plist 解析纪律 | **认同** | F6-1 |
| 6-4 | `is_fake_ip` 迁移可见性 | **认同**：`pub(crate)` + 单一实现断言 | F3-1、F3 测试⑤ |
| 6-5 | cfg 平台守卫是正确用法 | **认同**：函数文档写明平台语义 | F5-1 |
| B1（外层另查） | 互斥拒腿打到「裸 ID」既有容忍形态（`caps=0x7F`） | **认同（机制性缺陷）**：改 fail-soft | F1-1、D9、F1 测试③ |
| B2（外层另查） | index 键面三处（`bindwatch:202/148`、`egress:573`） | **认同**：`iface_same` 统一 + 三处改 | F5-5 |
| B3（外层另查） | F6 与 `role-management:184-188` 检测集合是 **spec 冲突**（非仅登记） | **认同**：进 §8 上报清单 | F6 风险③、§8 |
| B4（外层另查） | E22 行形态在 tier spec 有描述 ⇒ 属 spec 描述面变化 | **认同**：与 §8 上报合并 | F3 判据行影响、§8 |
| B5（外层另查） | `dns_upstream` 覆盖代价（代理域名规则失效）未写 | **认同**：告警 + 文档 + 登记 | F2 风险③、§6 |
| B6（外层另查） | `stun_target` 与既有 `stun/stun6` 键名撞车 | **认同**：改名 `dns_probe_target` / `stun_probe_target` | F2-1 |
| B7（外层另查） | 新键使 config.toml 对 Go 单向不兼容 | **认同**：登记一行 | §6 F2 行 |
| B8（外层另查） | 「argv[0] 谓词未登记」表述过重 | **不适用**（4-4 已按「不引入谓词」处置，无需登记收窄） | F6-2 |
| ⑦ 章程收窄 | ROADMAP:180「平台/**布局**字段化」的「布局」不做 | **认同**：进上报清单 | §8 |
| 行号订正 | `Upstreams` 跟随实现在 `dnsproxy.rs:244-288`；`serve_hello:970-1035`；`daemon_cli:127-147/149-156/272-287` | **认同**：全部订正 | §0.2/§0.3 |

**不认同 / 未采纳**：0 条（仅 2-1、2-6 采「部分认同」的次选方案，理由已写 D8/F3-4）。
**误报**：0 条（评审的事实性错误 5 条全部经本棒独立复现确认）。

### 7.4 过门结论

- **过门**：按评审「**改后过**」口径，v2 已把 **1 高（E-1）**、**全部中危**（E-2、1-2、1-3、1-6、
  2-1 的登记面、2-2、2-3、2-4、2-6、2-7、3-1+B9、3-2、3-4、4-1、4-2、4-3、4-4、B1、B2、B3、B5、B6）
  逐条补进方案/登记/注入缝，无遗留「设计门未决」项。
- **本批留给实现棒的硬约定**（评审三条约定的同款口径）：① 判据变更**同批 commit** 登记（§6 全表）；
  ② 未登记不得静默降级（歧义 caps = fail-soft 但必须计数/告警）；③ 发现设计-代码矛盾先登记再改。
- 设计门**通过（附 v2 整改）**；可进实现棒。

---

## 8. 需上报主会话 / tier 的事项（清单）

1. **tier spec 偏离 4 条**（opt-in、默认不变，需 tier 知会或修订）：
   `dns_upstream` ↔ `wg-native-dns:40` MUST；`dns_fallback` ↔ `:77` SHALL（223.5.5.5）；
   F6 精确化 ↔ `role-management:184-188` 检测集合（「或本机任意 homeway 代理 plist」）；
   E22 `fakeip` 新字段 ↔ `wg-native-dns` 可观测性段的统计行描述。
2. **tier 侧采纳**（跨仓触点，本仓只备机制）：App 在 HELLO caps 置 `KEY_ALT_ESC_PREFIX`（建议值，
   证据见 §2.3）；`term-surface-protocol:172-174` 现无 Alt/口径条款 ⇒ 建议补 spec 条款（F1 给客户端
   新增义务）；`exit-upnp-port-mapping` 的候选序 SHALL 与 F4 的 A-any 兜底关系（**知会**，判不构成
   硬冲突：A-any 在「候选用尽后」而非扩候选序）。
3. **章程条目收窄**：`REVIEW-ROADMAP.md:180`「keyenc 平台/**布局**字段化」的「布局」不做
   （wire 无字段；理由 §4）。
4. **F1 分叉消失的前提**：客户端置位 + 两台生产出口升级（均为用户触点）；本批交付 = 机制 + 缺省兼容。
5. **`config.toml` 单向**：写进新键的 config 无法回退给 Go 出口读（Go 已退役，仅影响回滚/对照）。
6. **Go 回滚面**：本批 5 条修法中 4 条为「超 Go 加固」（F2/F3/F4/F5 的放宽面），与 Go 并列运行期
   行为不逐字一致（有意分歧，§6 已逐条登记）。
