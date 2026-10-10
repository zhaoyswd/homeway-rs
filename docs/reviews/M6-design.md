# M6「真机与性能终验（2×2 + 归因 + PERF 报告）」设计（v2，r28 整改后）

> 棒别：**M6 设计棒**（只写设计 + `/tmp` 只读侦察，**不写产品代码**）；工作目录 = 主检出
> `/Users/zhaozhe/Documents/projects/homeway-rs`，分支 `main`（开工 HEAD `63207a5`）。
> 隔离：**现役出口（pid 33667，`/Users/zhaozhe/bin/homeway-rs --state ~/.config/homeway-rs`）不碰**；
> `homeway` / `tier` / `baseline/` 只读；不动 worktree `~/Documents/projects/homeway-rs-quic`；
> **不改** `docs/QUIC-ROADMAP.md` 与 `docs/INTEROP-CRITERIA.md`（后者 = 实现棒的登记面）；不 push / 不发 tag / 不建 PR。
> 范围来源 = `docs/QUIC-ROADMAP.md` 的 **M6 节（:672–690）+ 门槛表（:726–744）+ 下一步·M5→M6 交接六条（:129–135）**；
> 方法真源 = `docs/PERF-AB.md`（§1/§3/§9 全族），实操真源 = `docs/DEVICE-TEST-OHOS.md`。
> **本设计照抄路线文件的 M6 范围/判据，不加不减**；一切口径**预登记**在 §2，可 falsify，**预登记后不得按结果改口径**
> （若必须改：报告里显式登记「口径变更 + 理由 + 原口径读数」）。
> **版本说明**：**v2** = 按 dsh 设计门 **r28** 的 30 条意见整改（14 条必闭合清单）；**v3** = 按 **r29 窄审**的 5 条放行条件 + 12 条低项残余整改；两轮的轮次事实与逐条处置见 **§11**。**门结论 = 通过（r29 有条件通过 → 5 条条件已闭合）**。

---

## 0. 复验与前置事实

### 0.1 工作树与隔离面（开工必查）

| 项 | 事实（本棒实测/复核） |
|---|---|
| 工作树 | `git status --short` = 仅本设计稿；`main` HEAD = `63207a5`（开工时） |
| M5 终值 | `.so` = **2,958,896 B（0.779× 判据）**、九项门全过、内存六格全过（`docs/QUIC-ROADMAP.md:649–670`） |
| 现役出口 | pid **33667**，exe `/Users/zhaozhe/bin/homeway-rs`（7,119,496 B，2026-10-07 16:28 构建）；`strings` 含 **`hmw1`**、零 `quinn`/`hmw2` ⇒ **WG 档构建**（只读侦察，未碰进程） |
| 两仓只读 | `tier` 检出 HEAD `699e726`，pin = `cbd45f0`；`baseline/` 冻结锚 `d4148f6` |

### 0.2 旧栈锚核实（**回源码/`git log` 复核，非推测**）

| 事实 | 证据 |
|---|---|
| `4841b20` = 2026-10-08 23:11「Q-L 批收口」= **合回前的 main 末位** | `git show -s 4841b20` |
| 该树 **无 `homeway-quic`**（只有 `homeway-capi`/`homeway-cli`/`homeway-core`） | `git ls-tree 4841b20 crates/`——**「Q 批收口时 main 是否已含 M0–M4」= 否**（M0–M4 全程在兄弟 worktree 的 `quic` 分支，2026-10-09 才由 `836597d` 合回；`836597d` 的双亲 = `4841b20` + `77cf86f`） |
| 该树的 WG 面完整、可作 A/B 对照 | `tools/{local-rust-exit.sh,local-rust-relay.sh,build-app-core.sh}` 在树上（20 符号列表齐）；`tools/build-app-core.sh` 与 HEAD 的差 = **仅新增 16 行 CC 前置**（`git diff 4841b20 HEAD -- tools/build-app-core.sh`） |
| 体积锚同源 | `docs/QUIC-ROADMAP.md:637–639` 的 §0 四格矩阵把 `4841b20` 明确标为 **WG-only 树**（无 profile 档 **2,213,744 B**，与 M0 登记的现役 `.so` 基线同值） |
| 承载语义 | 该树 `intercept` 仍是「豁免 + 过境 + DNS」三径（`git show 4841b20:crates/homeway-core/src/server/intercept/mod.rs` 的 `route_upstream`：`dst == cfg.tunnel_ip ⇒ Kind::Exempt → 回环同端口`）⇒ §1.4(h) 的 TUN 测试目标在本锚上走**豁免**腿；新树走**过境**腿（HEAD 的 `route_upstream` 只剩 DNS/transit 两径） |
| 关闸差异 | 该树 WG 出口的公共 UDP 口 = `--listen` 值（旧 `tools/local-rust-exit.sh` 头注释：「WG UDP 端口 = 42650+n」）；HEAD 的公共口 = QUIC 口 = `listen+1`（HEAD 脚本 `start` 有 `actual != listen+1` 的拒绝断言） |
| 行文差异 | 旧树承载体行 = `传输：新栈（wg-native-stack）`（`git show 4841b20:crates/homeway-core/src/facade/tun_exec.rs:890`）；**旧树全仓无「L3 承载」字面**（`git grep "L3 承载" 4841b20 -- crates/` 为空）；**旧树无「快探」行族**（`git grep 快探 4841b20` 仅 `egress.rs:597` 一条注释）⇒ 旧臂恢复行集必须用**巡检失败族 + C11 `RECOVER` 行**（见 §5.1 的三分注） |
| 中继面**确有改动** | `git diff --stat 4841b20 HEAD -- crates/homeway-core/src/relay/ crates/homeway-core/src/relaywire.rs` = `mod.rs +344/−90`、`rltoken.rs +60/−…`、`ctlface.rs`、`relaywire.rs`（M5 S7a 的 Q2 v6 双栈 = **显式扩范围**，`docs/QUIC-ROADMAP.md:625–631`）⇒ §1.5.2 的中继中立性**不得**引「中继零改动」，改引「v4 单栈行为面未变 + `rl1` 冻结 + v6 为加法」 |

### 0.3 设备与工具盘点（**本棒只读侦察实测**，2026-10-10）

设备 `FMR0224116011480`（HUAWEI ALN-AL00，API 24）：`hdc list targets` 在连；`me.zhaozhe.tier` 进程在（`ps -ef` 可见），VPN 当前 **关**。

| 测量面 | 可用性（实测命令 → 实测输出形态） | 用途 |
|---|---|---|
| 设备 CPU（累计） | `ps -o pid,time,etime,comm -p <pid>` → `64338 00:00:20 01:49:54 [me.zhaozhe.tier]` | **主 CPU 口径**（`getrusage` 类：累计 CPU 秒差分 ÷ 轮墙钟 = **单核占空**，1 核 = 100%） |
| 设备 CPU（分类占比） | `hidumper --cpuusage <pid>` → `Total: 1.58%; User Space: 0.74%; Kernel Space: 0.84%; iowait: 8.36%; …`（**归一化口径/采样窗未定义** ⇒ 只作旁证） | 旁证（§4-L3） |
| 设备网络字节（进程级） | `hidumper --net <pid>` → `Received Bytes:58,046,642,386 / Sent Bytes:62,888,388,626`（累计） | **T2/T3 的设备侧字节交叉校验**（轮首/轮末差分） |
| 设备内存 | `hidumper --mem <pid>`（另有 `--mem-smaps`/`-p`） | 真机进程级 VmRSS/VmHWM 登记（非判据） |
| 设备 CPU 频率 | `hidumper --cpufreq` | 单核贴顶时的旁证（可选） |
| 设备日志 | `hdc file recv /data/app/el2/100/base/me.zhaozhe.tier/haps/entry/files/tailcat-tun.log`（用 `grep -a`）+ `hilog` | 判据行与时间线（`docs/DEVICE-TEST-OHOS.md` §5） |
| UI 自动化 | `uitest dumpLayout` + `uitest uiInput click X Y`（冒烟可用；tier 侧已做「组件 id 全覆盖」，`tier` 提交 `4bb24a0`）；**Wi-Fi 主开关不吃模拟点击** | 点 VPN 开关 / pf 规则页操作；换分辨率后必须重新 dump |
| **耗电（电流/电压）** | **不可得**：`hidumper -s BatteryService` 输出**空**；`hidumper -s 3308` = `DisplayPowerManagerService`（非电池）；`/sys/class/power_supply/` **Permission denied** | ⇒ 耗电维**降级**（§3.2 的登记口径） |
| 设备存储 | `df -h` → userdata 465G / 已用 115G / **可用 350G** | 下载物容量无忧；清理仍走 §2.5 的清理步骤 |
| 浏览器下载物目录 | `/data/app/el2/100/base/com.huawei.hmos.browser/...` **Permission denied**（shell 读不到） | 清理只能走 UI 或如实登记残留（M4/M5 先例） |

**Mac 侧可用面**：`ps -o time=` 差分 / `top -l 1 -pid`（出口进程）、`uptime`（loadavg，PERF-AB §9.15.1 常设纪律）、
`ifconfig -a`（本机地址枚举）、`python3`（计时 server）。

**Mac 本机地址（本棒实测 `ifconfig -a`）**：`en0 = 192.168.3.12/24`（设备同网段，**不可作 TUN 目标**——会被 OHOS 同网段绕行）、
`bridge100 = 192.168.139.3/23`、`bridge101 = 192.168.215.0/24`、`utun4 = 198.18.0.1/15`——后三者**都是本机自持地址**，
是 §1.4(h) TUN 目标的候选。

### 0.4 两树关键差异（影响测量口径的四条）

1. **token 前缀**：旧树 `hmw1`（`EndpointKind` = Direct/Relay 两类），新树 `hmw2`（+`Quic` 类）。**App 侧粘贴面只认 `hmw1`**（tier `entry/src/main/ets/model/TokenCheck.ets:18` `HMW_PREFIX='hmw1'`）⇒ 真机测试**必须走 `--ps host_token` 注入**（该路径走 `probeToken` 探活、无前缀检查；M5 已用 `hmw2` 跑通，`docs/reviews/M5.md:1675`）。**粘贴面拒 `hmw2` = M7/tier 触点**（§8-R11）。
2. **`tunConfig`**：tier App 实发键 = `mtu/out/tzOffsetMinutes/portForwards/diagFdSecs/token/endpointCacheDir/identityDir`（`tier:…/TierVpnExtensionAbility.ets:793–812`），**已停发 `transport`**；旧树 `TunConfigJson` 全键 `#[serde(default)]` + camelCase、非 deny（`git show 4841b20:crates/homeway-core/src/facade/mod.rs:76–95`），且 `dialMs/statsSecs` 缺省化在旧树内建（`DIAL_MS_DEFAULT`/`STATS_SECS_DEFAULT`）⇒ **旧核可吃现 App 配置（已知，非待验证）**；**未覆盖面 = 状态 JSON**：App 读 `readyBy`（旧核给 `wg`，tier 侧 `:377` 有 `'wg'` 回退 ✓）与 `tunStatusJSON.quic` 段（旧核整段缺席）⇒ S1 冒烟须断言 **App 主机卡/状态页正常 + 层 0 全通到 App 面**（§9-S1）。
3. **观测行文**：旧树 = WG 语义（`传输：新栈（wg-native-stack）`/`MIRROR`/`赛跑结算`/`C11 恢复族`），新树 = C20/C21/C18 族（`岛已建连`/`L3 承载 = 岛`/`链路重连完成`）。两臂的「恢复完成」行**不同名**⇒ §5 用「同定义、两行集」的口径（预登记，非结果驱动）。
4. **构建档位不对称（**新发现，必登记**）**：HEAD 有 `[profile.release] lto=true, codegen-units=1`（`Cargo.toml:88–90`），`4841b20` **无** `[profile.release]`（`git show 4841b20:Cargo.toml`）⇒ T2/T6 实际比较的是「WG＋无 LTO」vs「QUIC＋LTO」。**方向对新臂有利**（不会把「通过」变「不通过」），但**归因层必须含「构建档位」层**（§4-L6）；可选对照臂见 §9-S6。

### 0.5 分母替换登记（**门槛表「现役」的落地口径**）

门槛表「真机吞吐」行（`docs/QUIC-ROADMAP.md:732`）的分母写的是「**现役**」。事实：

- 现役 = 生产出口二进制 `/Users/zhaozhe/bin/homeway-rs`（2026-10-07 16:28 构建，`hmw1`、零 `quinn`）；该 **artifact 不可重构建**（无同 commit 构建记录，且是直装产物）。
- ⇒ M6 的「现役」**定义为 `4841b20` 重构建的 WG 栈**（核 + 出口同 commit）。等价性论证：同代 WG 形态（§0.2）、Q-L 仅文档/收口面、R8 的 8s ACK 时钟 / 令牌桶整形 / 发送线程默认 on 全在（`docs/PERF-AB.md:279–311/391–465/962–1052`）；**artifact 级差异 = 构建日 / commit / 档位**，故本条为**分母替换登记**（写进 `PERF-AB` 新节与 `docs/reviews/M6.md`）。
- **可选旁证（不作判据，1 轮）**：用现役二进制起**独立私有实例**（`HOMEWAY_BIN=/Users/zhaozhe/bin/homeway-rs /tmp/m6-old/tools/local-rust-exit.sh start 9`，state `/tmp/homeway-rs-rustexit-9`、端口 42659；**必须用旧树脚本**——新脚本的 `listen+1` 断言会拒 WG 档；客户端/驱动 = 旧树脚本的 `go-client-add 9` 形态或设备 `--ps` 注入）跑 1 轮 256 MiB，证明「重构建 ≈ 现役」的量级。**待实现期验证**：①该二进制能否吃现 App 的 `hmw1` token（同代 WG，预期能）；②该二进制（10-07 构建，早于 Q-H/F0）对 `--stun=` 空值的接受度（Q-I 尾段 F0 之前的形态——若拒，去掉该 flag 或走 config）。跑完立即 `stop 9`。

---

## 1. 2×2 矩阵定义 + 旧栈构建路径

### 1.1 为什么原「核 × 出口」四格不可做（先钉死前提）

- 「WG 核 × QUIC 出口」与「QUIC 核 × WG 出口」**在本仓构成性不存在**：WG 面已随 M5 全删（`tools/check-wg-removed.sh` 十条门）、
  且 token/wire 双双换代（`hmw1`→`hmw2`、RPK + 四帧准入替代 WG handshake；`docs/QUIC-ROADMAP.md:5–10` 的「无兼容包袱」口径）。
  M5 已实测同款结论：matrix 的 L2/L3/L4/L5 四行（Go/Rust 交叉）按「不存在」退役（`docs/QUIC-ROADMAP.md:388–392`）。
  该结论**逐格登记**（不是「未做」）——见 §3.2 第 1 行。
- ⇒ **M6 的 2×2 = 2 栈（配套的核+出口） × 2 路径（直连 / 经中继）**，每格内分 **冷/热** 两态。
  与门槛表两条真机行（**真机吞吐** :732、**中继承载** :738）对应（T3 的判据口径按 :738 原文修正，见 §2.1）。

### 1.2 矩阵定义（**预登记**）

| 格 | 栈（核 × 出口） | 路径 | 冷/热 | 有效轮数 |
|---|---|---|---|---|
| **A-直** | 旧 = `4841b20`（核 `.so` + 出口二进制**同 commit**） | 直连（token 里 Direct 端点在 LAN） | 冷、热 | 热 3+3 / 冷 3+3 |
| **A-继** | 同上 | 经中继（`--dead-direct` token + 本地中继） | 热 | 3+3（16 MiB 轮件） |
| **B-直** | 新 = S1 入册的 HEAD 树（核 `.so` + 出口二进制**同树**） | 直连 | 冷、热 | 热 3+3 / 冷 3+3 |
| **B-继** | 同上 | 经中继 | 热 | 3+3（16 MiB） |

- **腿（WiFi）唯一可测**：蜂窝不可得（§3.1）⇒ 矩阵的第三维（WiFi/蜂窝）**降级为单值 + 差异登记**。
- 冷/热定义照 PERF-AB §9.9（**预登记**）：
  - **冷** = `aa force-stop` App → 30s 无流量空闲 → `aa start`（含 token 注入）→ 点 VPN 开关 → **建连完成后 ≤10s 内开跑**；
  - **热** = 连接建立 ≥60s 且已跑过 ≥1 轮流量后开跑。
- 每轮 = 一次**完整**下载（T2 = 256 MiB；T3 = **16 MiB**，§2.1），轮序 `A,B,B,A,A,B`（平衡轮序、同刻交替）。

### 1.3 旧栈锚 = `4841b20`（理由 + 安全边界 + 为什么不选 `836597d`）

**理由（三条，逐条有据）**：

1. **同代性/无交叉污染**：合回前的 WG-only main 末位（§0.2），**不含任何 QUIC 面** ⇒ 「旧 = WG 形态」在字节级成立；
   核与出口**同 commit 构建**，互认对方的 token/wire（`hmw1` / Direct+Relay 端点 / WG handshake）。
2. **它就是本程序的对照锚**：M5 §0 删码余量实验（路线文件 :637–639）与 M0 体积基线（2,213,744 B，`docs/QUIC-BASELINE.md`）都用它当 WG-only 树 ⇒ M6 沿用同一锚，**不引入第二个未知变量**。
3. **它贴近现役形态**：现役二进制（10-07 构建）`strings` 含 `hmw1`、零 `quinn` ⇒ 现役 = WG 档；`4841b20`（10-08）是其**同日后继**（Q-L 仅文档/收口面），R8 机制全在位。artifact 差异按 §0.5 的分母替换登记处理。

**为什么不用 `836597d`（merge，双栈可切）**：①它引入 **A/B 开关变量**（`HOMEWAY_TRANSPORT`/`tunConfig.transport`/`serve.quic`——正是 M5 删掉的面，也是 `check-wg-removed.sh` ③⑤⑨ 哨兵词表），单变量纪律被破坏；②其 WG 路径**不是现役发布形态**（发布物 = 10-07 的 WG-only 构建）；③好处（`hmw1` 前缀 + 可同一二进制切两臂）对 M6 无价值——真机走 `--ps` 注入、两臂各自的 `.so` 由出包切换。⇒ **仅作 R1 失败时的退路**（登记「锚变更 + A/B 开关变量入册」）。

**安全边界**：

- 不落进任何现役 state 目录：旧栈出口用**旧树脚本自带**的 `/tmp/homeway-rs-rustexit-5`（`--listen 42655`），新栈出口用 HEAD 脚本的 `/tmp/homeway-rs-rustexit-3`（`listen=42653`/公共 `42654`）；两实例互不覆盖、与 41641 无关。
- 旧树一律经 `git worktree add /tmp/m6-old 4841b20`（**只读锚**，不 checkout、不改 main、不 push）；构建产物落 `/tmp/m6-old/target`。
- 回滚 = 停两个私有出口 + 中继 + `git worktree remove /tmp/m6-old` + 删 `/tmp/m6-*`（§9-S8 收尾清单）。

### 1.4 可复现命令序列（**实现棒照抄；谁按什么命令做什么**）

> `$R` = `/Users/zhaozhe/Documents/projects/homeway-rs`；`$HDC` = 带 `-t FMR0224116011480` 的 hdc（`docs/DEVICE-TEST-OHOS.md:13`）。
> **臂 = 旧/新是唯一变量**：除「臂切换」块外，其余命令两臂逐字相同。**除 (c)/(e)/(h) 的臂相关实参**（`<臂的 .so>`/`<TOK>`/`<VPN 开关坐标>`/`<候选>`）**外，本节其余占位全部展开，不留未定义符号**。

#### (a) 旧栈构建（Mac，一次性）

```bash
cd $R && git worktree add /tmp/m6-old 4841b20            # 只读锚
cd /tmp/m6-old && tools/build-app-core.sh                 # 旧核 .so；记 [sym]/[size]/md5 与 git -C /tmp/m6-old rev-parse HEAD
cd /tmp/m6-old && cargo build --release -p homeway-cli    # 旧出口二进制 = /tmp/m6-old/target/release/homeway-cli
```
**待实现期验证**：旧树 `build-app-core.sh` 无 CC 前置（ring 走纯 Rust 垫片 `tools/ring-shim`）⇒ 若 NDK 链接失败，则显式 `export CC_aarch64_unknown_linux_ohos=<NDK>/llvm/bin/aarch64-unknown-linux-ohos-clang` 后重跑并登记。

#### (b) 新栈构建（Mac，一次性）

```bash
cd $R && tools/build-app-core.sh                          # 新核 .so；记 [sym]/[ver]/[size]/md5
cd $R && cargo build --release -p homeway-cli             # 新出口二进制 = $R/target/release/homeway-cli
```
> **顺序硬规则（r28 E20 + r29 L-6）**：若做 §9-S7③（岛侧「出口身份不符」可行动行，改 `crates/homeway-quic/**`）⇒ **必须先落 S7③，再烘焙 B 臂 `.so`**，使 B 臂 = 最终树；**无论走哪条分支都要登记 S7③ 的 diff 行数**——第一分支（先改后烘焙）登记为「B 臂 = 终值树 + 已登记增量」，第二分支（S7③ 后落）同样登记增量并注明「B 臂 ≠ M5 终值树」。**两处常量以 S1 实测入册值（`.so` 字节 + md5 + 12 位 SHA）为唯一真源**，设计稿不写死 SHA/尺寸。

#### (c) 出包（tier 侧，**每个臂各一次**；手册 §1 的既知拦点逃生口）

```bash
cd ~/Documents/projects/tier
# pin 门：核仓 HEAD 必须是 pin 的后代——两臂都走「手拷逃生口」（log-index 门既存红，M1/M5 已登记）
cp -f <臂的 .so> tailcat/libs/arm64-v8a/libclientcore.so
cp -f <臂的 .so> tailcat/src/main/cpp/prebuilt/arm64-v8a/libclientcore.so
md5 -q tailcat/libs/arm64-v8a/libclientcore.so   # 两处必须同字节（记录）
. tools/ohos-env.sh
"$HVIGORW" --mode module -p module=tailcat@default  -p product=default assembleHsp --no-daemon
"$HVIGORW" --mode module -p module=entry@default    -p product=default assembleHap --no-daemon
"$HVIGORW" --mode module -p module=terminal@default -p product=default assembleHsp --no-daemon
$HDC install -r entry/build/default/outputs/default/entry-default-signed.hap \
                tailcat/build/default/outputs/default/tailcat-default-signed.hsp \
                terminal/build/default/outputs/default/terminal-default-signed.hsp
$HDC shell "aa force-stop me.zhaozhe.tier"
# 断言：HSP 内 libs/arm64-v8a/libclientcore.so 字节数 = 臂的 .so；md5 一致
```
> ⚠️ 手拷逃生口 = **M5 已用两次的既存路径**（`docs/reviews/M5.md:1671–1674`）；`*.so` 在 tier `.gitignore` 内，
> 前后 `git -C ~/Documents/projects/tier status --porcelain` 必须**零新增脏文件**（不变式，收尾必查）。

#### (d) 出口与中继（Mac，每臂一次；**实例号固定，防串台**）

```bash
# 旧臂（直连档）：
/tmp/m6-old/tools/local-rust-exit.sh wipe 5 && /tmp/m6-old/tools/local-rust-exit.sh start 5
#   就绪行「serve 就绪」；公共 UDP = 192.168.3.12:42655；state = /tmp/homeway-rs-rustexit-5
# 新臂（直连档）：
$R/tools/local-rust-exit.sh wipe 3 && $R/tools/local-rust-exit.sh start 3
#   就绪行「出口 QUIC 面就绪（单承载；…）」+「quic: 端点就绪（…）」；公共 QUIC = 192.168.3.12:42654
# 中继（两臂共用同一枚；先起 → 取 rl1）：
RELAY_LISTEN=":42781" RELAY_ADVERTISE="192.168.3.12:42781" $R/tools/local-rust-relay.sh start 1
RL1=$($R/tools/local-rust-relay.sh token 1 | grep -oE 'rl1[A-Za-z0-9_=+/-]+' | head -1)
# ⚠️ 切中继档**必须先 stop**：脚本 start 分支在「已在跑」时是 no-op（`tools/local-rust-exit.sh` 的
#    `if our_pid … then … exit 0`；旧脚本同款）⇒ 不 stop 则 `--relay` 永不生效、token 无 Relay 端点。
# 旧臂（中继档）：
/tmp/m6-old/tools/local-rust-exit.sh stop 5
EXIT_EXTRA_FLAGS="--relay $RL1" /tmp/m6-old/tools/local-rust-exit.sh start 5
# 新臂（中继档）：
$R/tools/local-rust-exit.sh stop 3
EXIT_EXTRA_FLAGS="--relay $RL1" $R/tools/local-rust-exit.sh start 3
# 取 token（**分臂脚本**；中继档必须重启出口后**重新取**）+ 中继档改写（**同代二进制**）：
TOK_O=$(/tmp/m6-old/tools/local-rust-exit.sh token 5 | grep -oE 'hmw[0-9][A-Za-z0-9_=+/-]+' | head -1)
TOK_N=$($R/tools/local-rust-exit.sh token 3 | grep -oE 'hmw[0-9][A-Za-z0-9_=+/-]+' | head -1)
TOK_O_RELAY=$(cd /tmp/m6-old && target/release/homeway-cli token "$TOK_O" --dead-direct)
TOK_N_RELAY=$($R/target/release/homeway-cli token "$TOK_N" --dead-direct)
# 断言（改写面）：`token <tok>` 打印的端点类别列表里**只剩 Relay**（两树都有 --dead-direct，
#   `crates/homeway-cli/src/main.rs:156–195` 与 `git show 4841b20:crates/homeway-cli/src/main.rs:118–123`；r28 已复核）
```
**切档顺序锚（r29 必闭合 1，写死）**：**(d) 切档（stop → start 带 `--relay`）→ 取 token → `--dead-direct` 改写 → (e) `aa force-stop` → `--ps host_token` 注入 → 点 VPN 开关**；
中继档**必须重新取 token 并重新注入**（不能复用直连档 token）。

**中继中立性前置冒烟（r28 E22，S1 判据之一）**：先验证「旧出口 × HEAD 中继」互操作——旧臂出口挂 HEAD 中继 + 取 token + 跑 1 轮 1 MiB 下载；
判据 = 中继日志 `注册腿 1` + 出口侧注册行 + 下载完成。**失败 ⇒ 旧臂改用旧树中继**，但旧脚本**硬绑回环且无 env 旋钮**
（`git show 4841b20:tools/local-rust-relay.sh:56–58`：`--listen 127.0.0.1:$RELAY_PORT --advertise 127.0.0.1:$RELAY_PORT`，`RELAY_PORT = 42780+n` 是内部量）⇒
**回退形态写死为（不复用旧脚本）**：

```bash
# 旧树中继（回退形态）：直接用旧树二进制起，监听/公布都指 LAN，端口与 HEAD 中继错开
/tmp/m6-old/target/release/homeway-cli relay --state /tmp/m6-old-relay-1 \
  --listen :42783 --advertise 192.168.3.12:42783     # rl1 由启动 stdout 打出（grep 'rl1'）
# 随后：/tmp/m6-old/tools/local-rust-exit.sh stop 5 && EXIT_EXTRA_FLAGS="--relay <rl1>" … start 5
```
**登记项（r29 必闭合 2）**：回退一旦启用，**中继二进制成为第二个 A/B 变量**（HEAD vs 旧树）⇒ 报告须按 §1.3 同款登记「锚差异 + 变量入册」，并注明两臂的中继端口不同（42781 vs 42783）。

#### (e) 设备侧（每轮）

```bash
$HDC shell "power-shell wakeup"                                   # 必须先唤醒（手册 §3）
$HDC shell "aa force-stop me.zhaozhe.tier"
$HDC shell "aa start -a EntryAbility -b me.zhaozhe.tier --ps host_token '<TOK>'"   # 免点屏注入
$HDC shell "uitest dumpLayout -p /data/local/tmp/ui.json" && $HDC file recv /data/local/tmp/ui.json /tmp/m6-dev/ui.json
$HDC shell "uitest uiInput click <VPN 开关坐标>"                    # 坐标每轮从 dumpLayout 取
```
**建连断言行（分臂，r28 E14 订正）**：
- **旧臂**：`传输：新栈（wg-native-stack）` + `warmup pong: 就绪（判据=wg）` + `attached（数据面已接管`（**不写「L3 承载」**——旧树无该字面）。
- **新臂**：`传输：新栈（QUIC 岛）` + `岛已建连`（含 `L3 承载 = 岛`）+ `warmup pong: 就绪（判据=quic）` + `attached（数据面已接管`。

#### (f) 计时 server（Mac，一次性；两臂共用）

```bash
python3 $R/tools/m6-serve.py 8000 /tmp/m6-ab/file.bin --log /tmp/m6-ab/srv.log
#   绑定 0.0.0.0:8000；逐请求记 TSV：t_first_byte / t_last_byte / 字节 / 客户端地址 / path
#   另供 /ping（~1KB 自动刷新页，§2.4 的「在用档」发生器）
```
**读数过滤规则（r28 E16 + r29 L-11）**：轮标签编进 URL —— `?r=<arm><cond><idx>`（arm ∈ `O|N`、cond ∈ `D|C|R|P`（直连/冷/中继/pf）、idx = 序号），
如 `?r=Nh3` 的 `Nh3`；**离线归集以 `path` 标签为主判据**（每轮唯一、与客户端地址无关）；客户端地址只作辅助——
T1（层 0）的客户端地址 = 设备原生 WiFi IP，T2/T5 为出口转投（**注意 bridge100 型 `TUN_DST` 下出口侧源地址 = Mac 本地地址**，不能单靠地址判形态）。

#### (g) pf 规则装配（T5 轮前，r28 E15）

```bash
# 覆盖装保数据，但规则要重建/核对：用 uitest 在 App「端口转发」页加两条（控件 id = tier 已做全覆盖）：
#   形态 2：127.0.0.1:18081 → 主机:17080      （出口目标 = Mac 127.0.0.1:17080 的 http.server）
#   形态 1：127.0.0.1:18081 → 主机:18081      （出口同端口真监听：Mac 起 18081 服务）
# 形态 3：同一目标改 targetPort=0（等价缺省缺省柄，抽查 1 轮）
# 轮前断言（出口侧）：`port-forward: 127.0.0.1:18081 -> 主机:17080 监听中`（逐条在场）
```
Mac 侧对应起服务：`python3 $R/tools/m6-serve.py 17080 /tmp/m6-ab/file.bin --log /tmp/m6-ab/pf17080.log`（形态 1 再起 `18081` 一份）。

#### (h) TUN 目标选取（S0 判据；**先定后测**）

1. 候选 = `ifconfig -a` 中**本机自持**且**不在设备 WiFi 网段（192.168.3.0/24）**的地址，优先序 `bridge100 → bridge101 → utun4`。
2. 逐候选验证（判据 = **出口判据行**，非界面现象）：
   `$HDC shell "aa start -U 'http://<候选>:8000/ping?r=probe'"`（**待实现期验证**：`aa start -U` 不在手册已验命令面内，失败则改用 `uitest` 点浏览器地址栏）
   ⇒ 出口日志出现 `intercept: tcp transit <候选>:8000 ← <设备隧道 IP>:…（dialok）`（**新臂**）/ 豁免行或 transit 行（**旧臂**，§0.2）
   ⇒ 命中即选定（`TUN_DST`）。
3. 备选（若三者皆不可用）：`sudo ifconfig lo0 alias 100.127.0.1/32`（**需用户输密码**；形态待实测）+ 同 (2) 验证
   （该形态下旧臂 `dst≠100.64.255.1` ⇒ 也走 transit——两臂同形）。
4. 失败兜底（若连备选也不可得）：**主判据降级为 T3 + T5 + T4**，并把「TUN 直连吞吐」按 §3.2 的未验项口径登记（**归属 M7/用户触点**）。
   ⚠️ 该降级决定**必须在任何 A/B 读数产生之前**做出（S0 出结论并落盘 `/tmp/m6-ab/S0.md`），否则视为改口径。

### 1.5 单变量纪律（防「测的不是一个东西」）

1. **每轮断言版本串**：设备核日志 `tailcat tun prepare ver=…` 必须是该臂的 12 位 SHA（S1 入册值），出口日志的就绪行必须在**本轮起点之后**出现一次（实例重启证据）。
2. **中继中立性（订正论证，r28 E22）**：中继二进制两臂共用（**HEAD 构建**），依据 = ①「中继零改动」口径指**该程序期内不改中继的对外行为面**（`docs/QUIC-ROADMAP.md:105` 的 ⑤「中继零改动」）；②`rl1` 段布局**冻结**（`docs/INTEROP-CRITERIA.md` 的 token 登记条 + M5 代码门 M-10 的字节锚）；③M5 S7a 的中继改动 = **v6 双栈加法**（对 v4 单栈客户端行为面不变，`docs/QUIC-ROADMAP.md:625–631`）；**不得**再引附录 E 架构图（r28 指出 v1 稿引错）。**前置冒烟**见 §1.4(d)。
3. **同刻交替**：两臂的出口/中继**同时常驻**，轮次按 `A,B,B,A,A,B`（PERF-AB §1 的交替消时段漂移口径）。
4. **唯一下载物**：每轮 URL 带 `?r=<arm><cond><idx>`（§1.4(f)），Mac 侧字节数必须 = 文件字节数（短读判作废）。
5. **不并行干重活**：真机轮次期间 Mac 不跑 `cargo`/矩阵/ci-local（PERF-AB §9.15.1 的负载混杂教训），逐轮记 `uptime`。

---

## 2. 口径预登记（**本设计的核心；逐项可 falsify**）

### 2.0 口径纪律（先立）

- 主指标 = **累计 CPU 时间（`getrusage` 类）**与**服务端 REQ→END 墙钟**；**墙钟只作副读**（PERF-AB §9.15.1）；
  **`ps RSS` 不作判据**（PERF-AB §1 的口径注 + §3）。
- 一切「同刻 A/B」= **两臂在同一个小时窗口内交替**；绝对数字**必须带时段/层 0 锚**（PERF-AB §10 日间/夜间带教训）。
- **跨期不可互引**：本批读数与 §9.12–§9.17 的 600 MiB/600 MB 口径不同 ⇒ 只作量级对照，**不判**。
- **分母替换登记**（§0.5）：门槛表「现役」≔ `4841b20` 重构建的 WG 栈。
- **预登记后不得改口径**；若因不可抗力改（如 TUN 目标不可得），必须在 `PERF-AB` 新增节与 `docs/reviews/M6.md` **显式登记**
  「口径变更 + 理由 + 原口径读数（若有）」。
- **本设计新增的触发阈值**（r28 E24：PERF-AB 无同口径，不得自称「沿用」）：单核占空 >80% / <50%（§4-L2/L3）、每包 CPU 比值 >1.15×（§4-L4）、作废阈值（§2.4）。**每包长度的口径**：内层满尺寸 1280 B ⇒ 载荷 1252 B（`docs/reviews/M1-S5-evidence.md` 满尺寸口径；PERF-AB §9.15.4 观测均包 1254 B = 含 2 B 差别的历史观测口径，**换算时写明用哪个**）。

### 2.1 测量项总表（T1–T12）

| # | 测什么 | 怎么测（命令/样本/轮序） | 判据 | 证伪条件（⇒ 动作） | 落盘 |
|---|---|---|---|---|---|
| **T1** | **层 0 环境锚**（直连 WiFi 天花板） | VPN **关**，设备浏览器 `aa start -U 'http://192.168.3.12:8000/file.bin?r=L0-<n>'`；3 轮，中位 | 无判据（登记） | 中位 **< 2×** 同批隧道中位 ⇒ 环境带受限：绝对值只作登记，判决改用同刻比值，并换时段复测一次 | `/tmp/m6-ab/l0.tsv` |
| **T2** | **TUN 直连吞吐**（主判据；门槛表「真机吞吐」:732） | 两臂，`http://<TUN_DST>:8000/file.bin?r=…`，**256 MiB**/轮；热态 `A,B,B,A,A,B`（3+3 有效轮）；冷态 `A,B,B,A,A,B`（3+3）；读数 = 服务端 REQ→END 墙钟 + 字节；**设备侧交叉校验** = `hidumper --net <vpn_pid>` 轮首/轮末差分 | **热态：新中位 ≥ 0.95 × 旧中位**；**冷/热：各臂 ≥ 0.70** | 热态比值 **< 0.95** ⇒ 判「不达标」并进 §4 归因；任一臂冷/热 **< 0.70** ⇒ 同 | `/tmp/m6-ab/t2-{hot,cold}.tsv` |
| **T3** | **经中继**（门槛表「中继承载」:738 = **经中继 vs 直连的比值**，绝对值只登记） | 轮件 **16 MiB**/轮（低于 T2：中继每源 200 pps 闸 + 单会话下行字节桶 16 MiB/s，`crates/homeway-core/src/relay/mod.rs:51/64`）；热态 3+3；**任何 A/B 读数前**先做两臂各 1 轮标定（落盘）。**限速形态写死（r29 必闭合 3）**：中继限速 = **编译期内建常量**（每源 200 pps `relay/mod.rs:51/122`、单会话下行字节桶 16 MiB/s `:64`），`homeway-cli relay` **无 `--rate-limit` 旋钮**（`crates/homeway-cli/src/relay_cli.rs:50` 用法行）⇒ **两臂同一 HEAD 二进制、脚本不传任何限速 flag、不留未登记变量** | **主（门槛行）= `新 (relay/direct) ≥ 0.95 × 旧 (relay/direct)`**（比值用 T2 热态中位 ÷ T3 中位，**同臂**）；**附加列** = 新/旧 的 relay 绝对中位；并登记中继侧 `分配腿` 峰值 + 设备侧 `congestion_events`/`lost_packets` | ①比值 < 0.95 ⇒ 不达标 + 归因；②**分辨力不足分支**：两臂中位都落在**各自标定值 ±25% 的并带**内（r29 L-8：标定 = 两臂各 1 轮 ⇒ 平台带 = `min(标定) × 0.75 … max(标定) × 1.25`）**且** |新/旧−1| < 5% ⇒ 判「带内不可分辨」，输出 = **登记 + 门槛行标「未验」**（**不得**自称通过），归属 M7；③`分配腿` 峰值 ≠ 1 ⇒ **登记 + 异常哨兵**（r29 L-1：T3 无迁移，正常值 = 1；N=2 只留给 T11） | `/tmp/m6-ab/t3-relay.tsv`、`/tmp/m6-ab/t3-calib.md` |
| **T4** | App 自带测速（副读） | App 首页测速卡片，每臂 3 轮交替；读数 = 卡片文本（步进 1 MB/s） | **无判据**（副读；**登记**：speedtest 面在 M6 无硬判据） | 副读与 T2 方向相反且差 >2 档 ⇒ 记录为「路径差异线索」（TUN vs 自连），不改判 | `/tmp/m6-ab/t4-app.tsv` |
| **T5** | portfwd 档 bulk（副判据；并补 M5 交下⑤） | 形态 2（出口 `127.0.0.1:17080`）：设备浏览器 `http://127.0.0.1:18081/file.bin?r=P…`，**128 MiB × 3+3 有效轮**（r29 M-1：`§2.4` 的「≥3 有效轮」对中位判据档一律适用；若时间盒紧 ⇒ 1+1 + 按 §2.4 判「未验」，见 §9 取舍序）；形态 1（主机同端口）/形态 3（`targetPort=0`）各 1 轮**抽查**（16 MiB，只出断言行、不判中位） | **新中位 ≥ 0.95 × 旧中位**（形态 2） | < 0.95 ⇒ 不达标 + 归因 | `/tmp/m6-ab/t5-pf.tsv` |
| **T6** | 设备侧 CPU（主 CPU 口径） | 每轮：Δ(`ps -o time= -p <vpn_pid>`) ÷ 轮墙钟 = **单核占空**（1 核 = 100%）；`hidumper --cpuusage` ≥5 次作旁证；**pid 钉身份** = `ps -ef` 全串 + `hidumper --mem <pid>` 的进程名（`me.zhaozhe.tier:vpn`，防 `comm` 截断同名） | **同吞吐带内：新 ≤ 1.10 × 旧**（per-byte：累计 CPU 秒 ÷ 下载字节） | > 1.10× ⇒ 归因（§4-L3/L4）；**若 T2 也不达标 ⇒ 本格即主证据** | `/tmp/m6-dev/cpu-*.tsv` |
| **T7** | 出口侧 CPU | `ps -o time= -p <exit_pid>` 差分 + `top -l 1 -pid` 采样 | 无硬判据；触发条件 = 出口单核占空 >80% ⇒ §4-L2 | 同左 | `/tmp/m6-ab/exitcpu.tsv` |
| **T8** | 耗电 | **降级**（§3.2）：CPU 时间代理（T6）+ 可选电量百分比长窗差分 | 无判据（登记） | 两臂 CPU 代理差 >20% ⇒ 登记「耗电面风险」+ 交 M7 | `/tmp/m6-dev/power.md` |
| **T9** | 内存（真机登记 + lab 终值复跑） | ①真机：`hidumper --mem <vpn_pid>`（建连后 / 热态负载中 / 收工各一次）；②lab 四格复跑 = `tools/quic-ab.sh mem --mode steady\|load\|conns\|conns-load`（口径同 M5 E 棒：`/tmp/quic-ab/m6-final`）；③**单连接格** = 判定沿用 **M5 终值 + 历史读数（+496K/+608K 按 ≤+640K 新门槛达标）**，本批只复跑**同族读数**（五点边际 + 稳态绝对值）作补充证据 | ①登记；②**按新门槛判**：每连接 ≤96K / 32 设备 ≤+3.1MiB / 负载态 ≤64MiB+队列 / 稳态 ±10%；③「单连接 ≤+640K」= **按 M5 终值判达标** + 本批同族读数佐证 | ②任一格越界 ⇒ 不达标（须上报；M5 已全过）；③**失效条件**（内层 MTU / 回程队列默认值 / 连接模型变化）本批未触发 ⇒ 门槛有效；若实现期发现触发 ⇒ 须重测重订 | `/tmp/m6-ab/mem/`、`/tmp/m6-dev/mem.tsv` |
| **T10** | 断线恢复 `T_recv`（门槛表 :734） | 每臂两相位（`kill -9` 出口 + 立刻重启 / 停机 5s），**在用档**（§2.4 `/ping`）；**起点 = 出口 E1 `serve 就绪` 行时刻**（Mac 侧日志，照仪器定义 `crates/homeway-core/tests/quic_ladder_e2e.rs:10/388`）；终点 = 设备侧「隧道恢复可用」首行（行集见 §5.1，分臂）；`T_kill→E1` **单列 `T_downtime`，不计入** | **≤ 3.5 s** | > 3.5 s ⇒ 不达标 + 归因；待机档长尾**单独登记**（M3 口径，`docs/reviews/M3-S8-evidence.md:265–269`） | `/tmp/m6-dev/recover.tsv` |
| **T11** | 路径变更 / 漫游替代（门槛表「换网迁移」:733 的**可达替代**） | ①出口侧：自建 UDP 中继换源端口（M2 手法）⇒ 设备 `tunStatusJSON.quic.migrations ≥1`、出口 `quic.pathChanges ≥1`、设备表 `peer: +` 恒 1；②客户端侧：中继腿打死 ⇒ 落直连腿（`link: via=` 变化） | **连接保持**：无 `世代重建` / 无 `peer: -` 后 `peer: +` / 无 `出口收线`；**中继腿峰值 ≤ N=2**（预登记值，非「本批实测值」——r28 E6） | 出现世代重建或设备表条数增加 ⇒ 不达标；真 WiFi→蜂窝 = **不可得**（§3.1） | `/tmp/m6-dev/roam.tsv` |
| **T12** | 丢包可观测（门槛表 :737） | **真机面 = 结构不可得**（登记）：注入面要求 `max_datagram_size < 1280`，而 MTU 旋钮区间 `[1320,1400]` 内 **1320 ⇒ mds 1282**（`crates/homeway-quic/src/config.rs:26–28` 自陈「区间内产不出 mds<1280 ⇒ 只能走测试缝」）；设备既不可设 `HOMEWAY_QUIC_MTU`、App 也不发 `quicMtuCap`。**本机必过面** = `tools/quic-island-e2e.sh` 复跑 + `mtu_cap` 测试缝臂 | 本机 = 必过（零静默丢弃）；真机 = **登记不可得** | 本机面红 ⇒ 不达标；真机项按 §3.2 固定句法登记 | `/tmp/m6-dev/narrow.md` |

### 2.2 轮序 / 样本 / 时长（**预登记，不可按结果调整**）

| 条件 | 有效轮数 | 轮序 | 每轮时长（估） | 备注 |
|---|---|---|---|---|
| T1 层 0 | 3 | 连续 | 3–9 s | 每轮前后各 1 次 `uptime` |
| T2 热态 | 3+3 | `A,B,B,A,A,B` | 连接热态 ≥60 s → 传输 10–15 s | 两出口同刻常驻 |
| T2 冷态 | 3+3 | `A,B,B,A,A,B` | force-stop + 30 s 空闲 + 建连 + ≤10 s 开跑 | 冷态逐轮重做 |
| T3 中继 | 3+3（先各 1 轮标定） | `A,B,B,A,A,B` | 16 MiB ÷ 实测带（0.3–8 Mbps ⇒ 16–430 s/轮，**以标定值为准；最坏档 ≈1–1.5 h 全档**——r29 残余项，时间盒不足时按 §9 取舍序点名） | 中继常驻；每轮后记 `分配腿` 行 |
| T4 App 测速 | 3+3 | `A,B,B,A,A,B` | ~30 s/轮 | 副读 |
| T5 pf | 3+3（形态 2）+ 1+1（形态 1/3 仅抽查） | 交替 | 同 T2（128 MiB） | 轮前 pf 断言（§1.4-g） |
| T10 恢复 | 2 相位 ×2 臂 | 交替 | ~60 s/相位 | 相 A 立刻重启；相 B 停机 5 s |
| T11 路径变更 | 每臂 1 次 + 客户端腿切换每臂 1 次 | — | ~60 s | 断言行落盘 |

**总下载量估算**：256 MiB×12（T2） + 256 MiB×3（T1） + 128 MiB×4（T5） + 16 MiB×（6+2）（T3/形态抽查） ≈ **4.5 GB**（设备 userdata 可用 350 G；清理见 §2.5）。

### 2.3 汇总与判定流程（**预登记**）

1. 每格算中位；**主判据读 T2 热态**；T3（比值）/T5 为并列判据；T1 只作环境带。
2. 比值一律用**中位比**（PERF-AB §9.8 的「单轮数据不可用于 A/B」）。
3. **离散闸（统一口径）**：同臂同态有效轮 max/min > **1.3** ⇒ 标「带内漂移大」，报告同时给区间与中位，判决附「带内」注。
4. **带内不可分辨条款（r28 E11 + r29 L-7）**：若 |新中位/旧中位 − 1| < 5% 且两臂区间重叠 ⇒ 判「带内不可分辨」，
   **该格的判定出口仍是三态之一**：T2/T5（阈值 0.95）在此情形下**比值必然过线** ⇒ 出口 = **过（附「带内」注）**；
   T3 的比值口径若同时命中「标定并带」⇒ 出口 = **未验（分辨力不足）**。与 §4 归因触发解耦（不达标阈值仍按 0.95 判）。
5. 判定只有三种出口：**过 / 不达标（进归因）/ 未验（差异登记）**；**不允许「差不多」**。

### 2.4 「在用档」发生器 + 轮作废规则 + 时钟对齐

- **在用档**：设备浏览器另开 `http://<TUN_DST>:8000/ping`（1 KB 自动刷新页，meta refresh 3 s）⇒ TUN 持续小流量，
  保证快探的 in-use 窗口有效（`docs/reviews/M3-S8-evidence.md:250–278` 的相位要求）。
- **轮作废（统一口径，r28 E5；任一命中即作废、重跑并登记原因）**：
  (a) 轮首 Mac 1 min loadavg **>4**（`docs/reviews/M3-S8-evidence.md:31`）或轮内 1 Hz 采样**峰值 >6**（`docs/PERF-AB.md:1097–1144` 的 §9.19 作废纪律；**1 Hz 采样器落 `tools/m6-ab.sh`**）；
  (b) 本轮无 `intercept: tcp transit <TUN_DST>:8000 ← …（dialok）`（TUN 轮）或旧臂豁免行；
  (c) 服务端未收满文件字节数 / 无 END 时间；
  (d) 设备核版本串 ≠ 本臂；
  (e) `link: via=` 与档位不符（直连档出现 `via=relay` 或反之）；
  (f) 设备出现 panic/crash 行（hilog 扫描）。
- **有效轮下限（r28 E5 + r29 M-1）**：**中位判据档（T2/T3/T5-形态 2）同臂同态 ≥3 有效轮**才出中位；仅 2 有效轮 ⇒ 出「未验」；
  **单轮抽查档（T5 形态 1/3、T3 标定轮、R-1–R-4 实录）不受本条约束**（只出断言行/实录行）。
- **作废轮留档不删**（`*.void`），报告给作废计数。
- **时钟对齐（r28 E1，T_recv 跨设备/Mac 时钟）**：设备核日志用设备时钟、出口 E1 行用 Mac 时钟 ⇒
  S5 首尾各做一次偏移实测（**主形态 = `$HDC shell "date +%s.%N"` vs Mac `date +%s.%N` 各 10 次取中位**；
  交叉形态 = `/ping` 的**同一次刷新**在两侧日志的落纸时刻差），报告里以「偏移 ± 量级」形式登记，T_recv 计算时扣除偏移。

### 2.5 读数落盘（**路径预登记**）

```
/tmp/m6-old/                     # 旧栈 worktree（只读锚 + 构建产物）
/tmp/m6-ab/                      # Mac 侧编排与读数
  ├─ file.bin                    # 256 MiB 源件（sha256 落盘）；16 MiB/128 MiB 轮件另存
  ├─ srv.log                     # 计时 server TSV（t_first/t_last/字节/客户端/path 标签）
  ├─ S0.md  S1.md  t3-calib.md
  ├─ l0.tsv  t2-hot.tsv  t2-cold.tsv  t3-relay.tsv  t4-app.tsv  t5-pf.tsv
  ├─ loadavg.tsv exitcpu.tsv rounds.md（逐轮：时刻/臂/态/标签/读数/作废标记）
  └─ mem/（quic-ab 产物，T9）
/tmp/m6-dev/                     # 设备侧
  ├─ cpu-*.tsv power.md roam.tsv recover.tsv narrow.md mem.tsv clock-offset.txt
  ├─ tailcat-tun-<臂>-<轮>.log   # 核日志切片（grep -a）
  ├─ ui-*.json  scr-*.jpeg       # uitest 证据
  └─ hilog-<臂>-<轮>.txt
/tmp/m6-exit-logs/               # 两个私有出口 + 中继的 stdout 切片（含判据行）
```
**清理（收尾必做）**：按 pidfile `stop` 两出口 + 中继（r28 E27：先读 pidfile 逐个 `stop`，再查端口）；
`git worktree remove /tmp/m6-old`；删设备下载物（尝试 `uitest` UI 删除，失败则如实登记残留，M4/M5 先例）；
`git status --porcelain` 两仓零新增脏文件。

---

## 3. 蜂窝维度的现实处置 + 未验/降级登记口径

### 3.1 事实（不回锅、不重测）

设备 `rmnet0–11` 无 IPv4、Settings 实读 `enabled=false/clickable=false`（**无 SIM 数据服务**，M1/M2/M3 三轮实测，
`docs/QUIC-ROADMAP.md:320–323`）；出口侧「无蜂窝可达公网端点」。⇒ **蜂窝 A/B 结构上不可做**（不是没做）。

### 3.2 未验/降级项的登记口径（**预登记**：写进报告与 `PERF-AB` 新节的固定句法）

| 项 | 本批状态 | 登记措辞（**照写**） | 归属 |
|---|---|---|---|
| **原「核×出口」交叉两格** | **构成性不存在** | 「『WG 核×QUIC 出口』『QUIC 核×WG 出口』= 交叉构型**不存在**（token/wire 双双换代 + WG 面 M5 全删；先例 = matrix L2–L5 退役）⇒ 2×2 取『2 栈 × 2 路径』」 | — |
| 真机吞吐·**蜂窝腿** | **未验** | 「蜂窝维不可得（设备无 SIM 数据服务：`rmnet0–11` 无 IPv4、Settings `enabled=false`；出口侧无蜂窝可达公网端点）⇒ 本行仅 WiFi 腿成立；**判据面不因缺腿而降级**（沿用 WiFi 腿结论）」 | M7/用户触点 |
| **换网迁移** | **降级为替代** | 「真 WiFi→蜂窝迁移不可得 ⇒ 用**入口可达替代**：(a) 出口侧路径变更（UDP 换源端口)+(b) 客户端中继腿↔直连腿切换；断言 = 连接保持（`migrations` 增 / 设备表条数不变 / 无世代重建）。**真 rebind 迁移未验**（M1/M2 已两轮登记），**归属 M7/用户触点**（需第二网络或有 SIM 环境）」 | M7/用户触点 |
| **耗电** | **降级** | 「设备无可用电流/电压读数（`hidumper -s BatteryService` 空；`/sys/class/power_supply/` 权限拒绝）⇒ 耗电维**以「设备侧累计 CPU 时间/字节」为代理**（T6），电量百分比长窗差分只作可选旁证；**不宣称功耗结论**」 | M7（若有条件） |
| **窄路径丢包可观测（真机）** | **结构不可得 + 登记** | 「真机窄路径不可达：注入面要求 `max_datagram_size < 1280`，而 MTU 区间 `[1320,1400]` 内 1320 ⇒ mds 1282（`crates/homeway-quic/src/config.rs:26–28`）；设备不可设 `HOMEWAY_QUIC_MTU`、App 不发 `quicMtuCap` ⇒ 本机测试缝面必过、真机面登记不可得」 | M7/用户触点（tier 侧旋钮） |
| **服务面 speedtest** | **无判据** | 「M6 不设 speedtest 硬判据（App 卡片读数步进 1 MB/s，只作副读 T4）；服务面功能面 = files/term 冒烟（S2）」 | — |
| **DNS 回复 e2e** | **本批补做**（§6④） | 真机 DNS 计数行对照 + 本机新 e2e 用例 | — |
| **真机 portfwd 形态 1/3** | **本批补做**（§6⑤） | T5 断言行 | — |

### 3.3 门槛表 9 行 → M6 判定项对账表（**报告必须逐行填**，r28 E25）

| 门槛行（`docs/QUIC-ROADMAP.md`） | M6 判定项 | 本批状态（S8 填） | 证据路径 |
|---|---|---|---|
| 每包 CPU（绝对列，lab 档） | （M5 终值已过）本批不重跑；只在归因需要时复跑 `quic-ab.sh cpu` | 未重跑 / 或归因复跑 | `/tmp/quic-ab/*` |
| 线开销 ≤40B/包 | 同上（M5 终值 30.19B 已过） | 未重跑 | — |
| **真机吞吐**（热 ≥0.95× / 冷热 ≥0.70） | T2 | 过 / 不达标 / 未验 | `t2-hot.tsv`、`t2-cold.tsv` |
| **换网迁移** | T11（替代口径） | 降级（真迁移未验） | `roam.tsv` |
| **断线恢复 ≤3.5s** | T10 | 过 / 不达标 / 未验 | `recover.tsv` |
| 体积 ≤3.8MB（product 档） | （M5 终值 0.779× 已过）本批只登记两臂 `.so` 字节 | 过（承 M5） | `S1.md` |
| **内存四格 + 单连接 ≤+640K** | T9 | 过 / 不达标 | `/tmp/m6-ab/mem/` |
| **丢包可观测** | T12 | 本机过 + 真机登记不可得 | `narrow.md` |
| **中继承载**（同刻 A/B 相对） | T3 | 过 / 不达标 / 未验（分辨力） | `t3-relay.tsv` |

---

## 4. 归因路径预登记（**逐层剥洋葱；每层给判据式**）

> 触发条件：§2.3 判「不达标」的任一格；**这是本程序最后一次性能归因机会**（M7 = 生产切换）。
> **M6 只归因 + 登记，不做优化**（沿用 PERF-AB「R5 量化不优化」先例）；若归因指向可低成本修复且用户点头，另开批。
> 表中阈值均为**本设计新增触发阈值**（r28 E24），不是 PERF-AB 既有口径。

| 层 | 测什么（命令/读数源） | 判据式（看到什么读数 ⇒ 落到哪一层） |
|---|---|---|
| **L0 环境锚** | T1 层 0 中位；每轮 `uptime` | 若 层 0 中位 < 2× 隧道中位 ⇒ **本批绝对值不可判**（环境带压缩）；先换时段/换 AP，仍不行 ⇒ 判决只出「比值」并把该限制写进报告首段 |
| **L1 路径形态** | 出口行 `intercept: tcp transit/exempt …（dialok）`、设备行 `link: via=… ep=… rtt=…` | 若新臂 `rtt` > **max(2 ms, 旧臂中位 ×1.20)** **或** `via` 与档位不符 ⇒ **不是性能差，是测错**：该轮作废重跑，不进归因（r28 E24：原 +2ms 固定线在真机 10–22 ms 带内过易触发） |
| **L2 出口侧** | T7 出口单核占空 + 出口日志（`UDP 出站`/`整形观测`/`cc` 行族，`docs/PERF-AB.md:800–815`） | 出口进程单核占空 **<50%** 且无 `tx_dropped`/满缓冲行 ⇒ **出口不是瓶颈**（PERF-AB §9.3 同款先例）⇒ 下潜；**>80%** ⇒ 归因 = 出口发送形态（拍粒度/单 socket 串行） |
| **L3 设备侧单核** | T6：Δ(`ps -o time=`) ÷ 轮墙钟（单核占空）+ `hidumper --cpuusage`/`--cpufreq` 旁证 | 若 vpn 扩展进程单核占空 **>80%** 且吞吐随负载不涨 ⇒ 归因 = **手机侧单核贴顶**（PERF-AB §9.7-bis 第二瓶颈族）；**<50% 而吞吐低** ⇒ 下潜 |
| **L4 逐包成本 / ACK 时钟 / 拥塞** | 每包 CPU = (累计 CPU 秒 ÷ 字节) × **1252 B**（换算口径写明）；**设备侧**丢包/迁移面 = `tunStatusJSON.quic`（`lost_packets`/`congestion_events`/`migrations`/`drops{}`，`crates/homeway-core/src/facade/tun_exec.rs:596–598`）+ 核日志「丢弃 超限=…」行族；**出口侧** = `serve status --json` 的 `quic` 段（28 键：`pathChanges`/`dropTooLarge`/`dropSendBufferFull`/`dropUnregistered`/`dropSrcRejected`/`handshake*`，`crates/homeway-core/src/daemon/proto.rs:387–441`；**该段无 `lost_packets`/`congestion_events`**） | 若「每包 CPU」新/旧 **>1.15×** ⇒ 归因 = 逐包成本（协议栈/系统调用）；若**设备侧** `lost_packets`/`congestion_events` 显著（>0 且与本轮形态匹配）⇒ 归因 = 拥塞/ACK 时钟；两者皆清白 ⇒ 下潜 |
| **L5 拍粒度×空口交织** | 出口 `UDP 出站` 批分布 / 整形观测行；TUN 路径 vs 自连路径（T4/T5）对照 | 若 T2 与 T5/T4 方向相反（TUN 差、自连好）且 L2–L4 清白 ⇒ 归因 = **出口发送拍粒度 × 空口交织**（PERF-AB §9.10 机制族）；若 TUN 与自连同向差 ⇒ 归因 = 承载本体（QUIC 每包成本），登记为「承载替换的固有代价」 |
| **L6 构建档位 / 中继限速平台**（r28 E19/E24 新增） | 档位事实（§0.4-4：旧臂无 LTO、新臂有）；T3 的标定带与判读；可选 **LTO 对照臂**（§9-S6） | 若新臂不达标且 L2–L5 全清白 ⇒ 先排「中继限速平台」（T3 走分辨力不足分支时，任何中继档结论不得外推到直连档）；若差异与「LTO 有无」同向且量级 ≤5% ⇒ 档位解释（用对照臂确认）；否则归因落在承载本体 |

**归因报告的固定产物**：逐层读数表 + 「命中层」结论 + **M7 决策输入**（带修复切换 / 接受差异 / 另开优化批的代价估计）。
**禁止**：用单轮读数定层；用 `ps RSS` 定层；跳过作废规则。

---

## 5. 漫游 / 恢复实录（行样例 + 与门槛对账）

### 5.1 真机可做的四类（**预登记**）

| 编号 | 注入 | 期望（判据） | 实录行（样例格式；**行集在 S1 grep 定稿后写进脚本文本，不得看到读数后挑行**） |
|---|---|---|---|
| **R-1** | 出口 `kill -9` + 立刻重启（在用档） | `T_recv ≤ 3.5s`（定义见 T10：起点 = E1 `serve 就绪`；`T_downtime` 单列）；无世代重建 | **新臂**：`[设备时刻] 链路快探失败（连续 1，原因=探活无回显）` → `链路重连中（原因=探活无回显，第 1 次）` → `赛跑结算：胜出 直连 192.168.3.12:42654（候选 1 个，耗时 33ms）` → `链路重连完成（原因=探活无回显，耗时 2288ms）`（r29 M-2：`原因=` 取值 = `探活无回显`/`连接已断`/`无连接`/`对端不可达`/`连续 N 次抖动升格`/`动作链过长（…）`，`crates/homeway-quic/src/client/ladder.rs:618–622`；**「快探失败」是行名不是原因值**——既有单测逐字钉 `链路重连中（原因=探活无回显，第 1 次）`，`ladder.rs:1037–1040`）；**旧臂（巡检失败族 + C11 `RECOVER` 行，逐字）**：`对端巡检失败 {n}/3: {err}`（`git show 4841b20:crates/homeway-core/src/facade/tun_exec.rs:1848`）→ `巡检失败后阶梯已恢复（不用等 3 连败）`（同文件 :1857）→ C11 的 `RECOVER` 恢复行（`docs/INTEROP-CRITERIA.md:60`；旧树 `session/recover.rs`，**逐字以 S1 grep 定稿**）。**注（r29 L-2）**：前两条属**巡检失败族**（`INTEROP-CRITERIA.md:618` 明写它「非 C1–C17 判据行」）、C11 才是行族 ID——本设计行文按此三分。**删**：旧臂不得用 `warmup pong: 就绪`/`attached（`（旧树这两行只在**新世代装配**时打；R1 恢复不拆世代，r28 E2）/不得用 `快探失败`（旧树无该族） |
| **R-2** | 出口侧路径变更（自建 UDP 中继 + 换源端口） | 连接保持：**设备侧 `tunStatusJSON.quic.migrations ≥ 1`** 或出口 `quic.pathChanges ≥ 1`（r28 E9：**不要写出口 `migrations`**，该键不在出口段）、设备表 `peer: +` 恒 1、无收线行 | 新臂：出口 `quic: 路径变更 dev=… <旧源> → <新源>`（本注入形态的判据行）；设备 `quic: 迁移完成（<旧> → <新>，耗时 …）` **仅在客户端自发起 `rebind()` 时出现**（本注入 = 出口侧变源；r29 L-12；`docs/QUIC-ROADMAP.md:485` 记「客户端 `rebind()` 真迁移未验」）⇒ 判据以**计数**为准、行可选；旧臂：`MIRROR…`/`路径确立…`（WG 档无 migrations/pathChanges 计数 ⇒ 判据降为「无重建 + 表不变」） |
| **R-3** | 客户端腿切换：中继腿打死（直连腿在场） | 连接保持或一次腿级切换；不出现 `peer: -`+`peer: +` | 新臂：`quic: 迁移完成（relay → direct…）` 或 `链路动作选 R（新 QUIC 连接）（原因=<why>；<detail>）`（r29 M-2：实渲染格式 = `链路动作选 {}（原因={why}；{detail}）`，`ladder.rs:534`；动作标签集 = `M（换本地 socket）`/`R（新 QUIC 连接）`）+ `link: via=direct`；旧臂：`路径确立：direct …`（WG 档的 relay→direct 升级行） |
| **R-4** | 瞬时黑洞 1.5s（**中继腿楔子**，负向） | 只记抖动、不动作（与本地负向①同谓词） | 新臂：`quic: 链路探活抖动（…，已复探）` 且 `link: via` 不变、无动作行；旧臂：WG 档同义行（`tools/quic-wedge-proxy.py <listen> <upstream> <ctrl>` 挡在**中继**前，`RELAY_ADVERTISE` 指楔子；**行文以 S1 grep 定稿**） |

### 5.2 与门槛表「断线恢复 ≤3.5s」的对账方式

- **定义唯一（照仪器）**：`T_recv` 起点 = **出口 E1 `serve 就绪` 行时刻**，终点 = 「隧道恢复可用」首行；
  旧臂行集 = 巡检失败族 + C11 `RECOVER` 行（见 §5.1-R-1 的三分注），新臂行集 = {`链路重连完成`、`岛已建连…—— L3 承载 = 岛`}；`T_kill→E1` = `T_downtime` 单列。
  依据 = `crates/homeway-core/tests/quic_ladder_e2e.rs:10/388` 与 M3-S8 的算术（相 A `42.415→44.711` = 2296 ms、相 B `22.041→22.888` = 847 ms，与表列 T_recv 逐位吻合）。
- 待机档（无 TUN 流量）**单独登记**（QUIC 空闲回收 30s + keep_alive 相位；`docs/reviews/M3-S8-evidence.md:265–269` 的待机档反例读数），**不计入 ≤3.5s 判据**。
- 跨时钟偏移按 §2.4 实测登记；报告须给「E1 时刻 / 首失败行 / 恢复行 / T_downtime / T_recv」五列原始读数。

### 5.3 行样例格式（报告里统一用）

```
| 臂 | 相位 | kill(HH:MM:SS.mmm) | E1 serve 就绪(=T_recv 起点) | 首失败行(+Δ) | 恢复行(+Δ) | T_downtime | T_recv | 判据 |
| 新 | A 立刻重启 | 11:02:13.401 | 11:02:13.470 | 链路快探失败 +2.31s | 链路重连完成 +2.31s+… | 0.069s | 2.29s | ≤3.5 ✓ |
```

---

## 6. M5 交接六条 + 真机新发现的承接（逐条点名）

| # | M5 交下 | M6 处置（**本设计定**） |
|---|---|---|
| ① | **内存格**（≤+640K 已批准，`63207a5`） | **用新门槛判**：T9 ②③——lab 四格复跑按新门槛判；「单连接 ≤+640K」= 按 M5 终值（+496K/+608K）判达标 + 本批同族读数佐证；真机只登记进程级 VmRSS/VmHWM。**失效条件照抄**（内层 MTU / 回程队列默认值 / 连接模型变化 ⇒ 重测重订，`docs/QUIC-ROADMAP.md:736`）。 |
| ② | 门槛口径已随 M5 落 | M6 **只引用，不再改**；本设计只**新增**三条登记（蜂窝降级、真机「现役」分母替换 §0.5、窄路径结构不可得）——落 `PERF-AB` 新节 + `docs/reviews/M6.md`，**不动路线文件的门槛表**。 |
| ③ | `§12-C-8`（`:5300` 按退役执行） | **按退役承接**：M6 只做一次**回归确认**（真机 socks/域名目标失败面归因行仍可行动、`CA5` 降级注与实现一致），**不改代码**；若用户改判「承接」⇒ 另立批（不在 M6）。 |
| ④ | **DNS 回复 e2e 未落 ⇒ M6 补** | **补两件**：①**本机 e2e**（新用例：岛 ⇄ DATAGRAM ⇄ intercept DNS 代答 ⇄ **回程**到达岛；断言「无 WG 树 + 应答字节正确 + 计数行在场」；落 `crates/homeway-core/tests/` 的 `#[ignore]` 用例，由脚本驱动）；②**真机对照**：出口 `dns: q=… resp=…` 计数增量 + 设备浏览器域名导航成功。判据 = 两件皆过；否则按未验登记。 |
| ⑤ | 真机 portfwd 形态 1/3 未抽查 | T5 补做（§1.4-g）：**形态 1**（`127.0.0.1:18081 → 主机:18081`，出口同端口真监听）+ **形态 3**（`targetPort=0`）；各 1 轮 + 断言行。 |
| ⑥ | 终值树同树复测 + **旧 token 对新出口失败面无归因** | ①**终值树同树**：B 臂 `.so` 烘焙顺序按 §1.4(b) 的硬规则；S1 记 `.so` 字节/md5/SHA，2×2 的 B 臂即「终值树（或终值树 + 已登记增量）同树复测」。②**旧 token 归因（S7③）**：给岛侧「候选全因 TLS/RPK 失败」补**一条可行动行**（如 `出口身份不符（token 的 RPK 与出口公钥不符——请重新获取 token）`），判据 = 真机实录：旧 token 注入后设备日志出现该行（而非只 `赛跑小结：无胜者`）。**若时间盒紧张 ⇒ 允许整条顺延**（显式登记，M5 先例）。 |

**真机新发现六条（M2）的 M6 承接**：①准入失败归因不回传客户端 → 已随 M3 阶梯/C18 族落地，M6 只在 R-1/T10 取行证；
②出口重启自愈 32/41s → M3 已定位并整改，M6 用**在用档**复验 + 待机档登记；
③UPnP 成功 ≠ 公网端点可用 → 非 M6 面（部署文档，M7）；④设备身份设备持久 → 无动作；⑤表满闸分母随 cap 缩放 → 非 M6 面；
⑥WG 档出口不记「源校验拒」= 观测面差异 → 报告里按「两档观测面差异」如实注（不当行为差异判）。

---

## 7. 插桩策略 + 清理口径（**代码门专项 = 插桩清理**）

### 7.1 允许的插桩（**白名单，越界即代码门高危**）

| 类 | 允许 | 落点 | 约束 |
|---|---|---|---|
| ① 仓外脚本 | `/tmp/m6-*` 的一切（编排、楔子、计时 server 临时变体、采样循环） | 仓外 | 不入库；报告里给命令行与产物路径 |
| ② 入库 harness | `tools/m6-ab.sh`（编排）+ `tools/m6-serve.py`（计时 server） | `tools/` | **harness-only**；两臂命令逐字对称（仅臂变量不同）；门⑤只扫 `tools/**` 的 `*.rs`/`*.sh`（`tools/check-wg-removed.sh:181–189`）⇒ `m6-serve.py` 不受扫描，但**自律适用**（r28 E28） |
| ③ 设备侧 | 零插桩：只用既有 hilog/核日志行 + `hidumper`/`ps`/`uitest` | — | **不允许** push 自建二进制到设备（noexec/SELinux 未知；本批不试验） |
| ④ 产品面（`crates/**`） | **仅一处**：§6⑥-② 的「出口身份不符」可行动行 | `crates/homeway-quic/**` | **声明不做 env 门控临时插桩**（r28 E30：若确需 `HOMEWAY_M6_*` ⇒ 必须**同批登记** `INTEROP-CRITERIA` 的配置常量条 + 收口前删除 + 删后重跑门） |

**门假红纪律（本棒实测的门模式，`tools/check-wg-removed.sh:85/163/227`）**：
入库脚本**不得**出现 `hmw1`、`_wg`、`ring-shim`、`HOMEWAY_TRANSPORT`、`tunConfig.transport`、`serve.quic`、`wgcore::`；
token 抽取一律用**前缀无关** `hmw[0-9]`（`tools/matrix.sh::token_for_link` 的既有口径）；
臂命名用 `OLD`/`NEW`（**不要**用 `*_wg`，`_wg` 是**子串**匹配）；旧臂断言串（`判据=wg`、`传输：新栈（wg-native-stack）`）
**只在 `tools/**` 脚本里出现**（门③ 只扫 `crates/**/*.rs`；放进 `crates/**` 测试 = 击红）。

### 7.2 清理口径（**收口前清零，脚本化**）

```bash
# ① 产品面零插桩（临时 grep；**自身排除**——r28 E29）
grep -rn "HOMEWAY_M6\|M6 临时\|m6_dbg\|m6dbg" crates/ tools/ --exclude=m6-ab.sh --exclude=m6-serve.py || echo "OK: 零插桩"
# ② 门面回归（既有门）
tools/check-wg-removed.sh && tools/check-quic-isolation.sh && tools/check-vocab.sh
# ③ 工作树与残留
git status --porcelain          # 只应有本批的文档/工具改动
git worktree list               # /tmp/m6-old 必须已 remove
# ④ 进程面（先 pidfile 后端口——r28 E27）
for f in /tmp/homeway-rs-rustexit-5/pid /tmp/homeway-rs-rustexit-3/pid /tmp/homeway-rs-rustrelay-1/pid; do [ -f "$f" ] && kill -TERM "$(cat "$f")" 2>/dev/null; done
lsof -nP -iUDP | grep -E '4265[345]|42781' || echo "OK: 私有实例全停"
```
- **零插桩 = 代码门的一票判据**：任一 `crates/**` 的 M6 临时行残留 ⇒ 代码门直接判高危。
- 设备侧：关 VPN、`aa force-stop`、出口/中继全停、下载物清理（或登记残留）；**`kill -9` 白名单** = 仅 `/tmp` 私有实例的 pidfile pid（报告里写死这条）。

---

## 8. 风险与未决

| # | 风险/未决 | 影响 | 处置 |
|---|---|---|---|
| **R1** | **旧栈锚不可构建或不兼容**（旧树 lockfile 与今日 rustc/NDK；旧核 × 现 App 的配置面/状态面） | 旧臂缺失 ⇒ A/B 不成 | S1 判据（旧树 `build-app-core.sh` 三门 + 旧核建连 + **App 主机卡/状态页正常**）；失败 ⇒ **上报主会话**，退路 = `836597d`（双栈可切，须登记「锚变更 + A/B 开关变量入册」）或更早锚，**不得静默换锚** |
| **R2** | **TUN 目标不可得**（无本机自持非 LAN 地址；备选需 sudo） | 主判据失据 | §1.4(h) 的选取 + 备选 + 兜底（兜底决定必须在任何读数前落盘） |
| **R3** | 浏览器行为（弹层/缓存/中断）；`aa start -U` 未验 | 轮作废率上升 | §2.4 作废规则 + `?r=` 标签 + 服务端计时（与点击时刻解耦，PERF-AB §9.14 先例）；`-U` 失败改 `uitest` 点地址栏 |
| **R4** | 设备存储/清理（下载物不可 shell 删） | 残留 | 350 G 可用；清理尝试 + 如实登记（M4/M5 先例） |
| **R5** | 蜂窝不可得 | 矩阵缺一维 | §3.2 固定句法登记 + 归属 M7 |
| **R6** | QUIC 待机档 30s 长尾干扰恢复读数 | 误判不达标 | **在用档** `/ping` 发生器 + 待机档单独登记（M3 口径） |
| **R7** | 中继闸把两臂压到同一平台 | 分辨力不足 | T3 的标定轮 + 显式「带内不可分辨」分支（登记，不自称通过） |
| **R8** | 环境漂移（空口、日/夜带） | 绝对值不可跨期 | 同刻交替 + 每轮层 0 + loadavg 落盘；报告首段写时段口径 |
| **R9** | 插桩残留 | 代码门高危 | §7.2 四条清扫 + 一票判据 |
| **R10** | 「出口身份不符」行改动触岛内失败面；且改它会移动 B 臂 `.so` | 小回归风险 + 破坏「终值树同树」 | §1.4(b) 的顺序硬规则（先改后烘焙）；用例 + 真机实录；可整条顺延（显式登记） |
| **R11** | **tier 粘贴面只认 `hmw1`**（`TokenCheck.ets:18`）⇒ 用户无法粘贴新 token | **M7 生产切换会被卡** | **本批只登记**（真机走 `--ps` 注入）：`docs/reviews/M6.md` + 收口材料点名 tier 触点（与 `connection-lifecycle` 修订稿同批） |
| **R12** | `sudo` 免密不可用（实测 `sudo -n true` rc=1） | 别名备选需用户输密码 | 用户触点（一次性）；优先用本机既有地址（§1.4-h） |
| **R13** | 设备浏览器对 `TUN_DST` 的「同网段旁路」判定不确定 | 首轮可能失败 | 判据 = **出口 dialok 行**（界面现象不作数）；失败即换下一候选 |
| **R14** | 时间盒（2–4 会话日）与工作量冲突 | 报告不完整 | §9 的取舍序：**S1–S6 + S8（报告骨架）优先**，S7 整片可顺延（含 S7① 的 e2e），顺延必须显式登记 |

**需主会话/用户裁决（3 条）**：

1. **§6⑥-②「出口身份不符」行**：做（推荐，小；代价 = B 臂 `.so` 需在改后重烘焙）还是整条顺延登记？
2. **迁移维**：是否由用户提供**第二可达网络**（如手机热点 + 出口可被外网可达）以做真 rebind？不提供 ⇒ 按 §3.2 登记未验。
3. **真机测量窗口**：T2/T3/T10 轮次期间是否保证**独占 Mac + 独占设备**（不并发其它批/重活）？

---

## 9. 实施清单（S1–S8；依赖顺序 + 完成判据 + 读数路径）

**时间盒取舍序（r28 E27 + r29 残余项，预登记）**：若时间盒到点 ⇒ 优先保 **S1–S6 + S8 的报告骨架**（含 `PERF-AB` 新节与代码门），
**S7 整片（含 S7①）可顺延**；**T3 中继档**（最坏档 ≈1–1.5 h）与 **T5 形态 2** 可在**标定/首轮读数落盘后**按「1 档降到最小轮数 + 判未验」压缩
（压缩必须记进报告的门槛 9 行对账表）。顺延/压缩必须在 `docs/reviews/M6.md` 显式登记（写明未做项与归属）。

| 片 | 内容 | 依赖 | 完成判据 | 读数路径 |
|---|---|---|---|---|
| **S1 前置与双栈构建** | 工作树/门复验；建旧锚 worktree；旧/新两套 `.so` + `homeway-cli`；**TUN 目标选取（§1.4-h）**；**中继中立性前置冒烟（§1.4-d）**；臂切换脚本与断言；**S7③ 的先后决定**（§1.4-b 顺序硬规则） | — | 旧核 `.so` `[sym] 20/20` + 字节/md5 入册；新核同；两 `homeway-cli` 可起；`TUN_DST` 选定且**出口 dialok 行**在案；旧出口 × HEAD 中继冒烟过（或改旧中继并登记）；**旧核 × 现 App 在设备上建连成功 + App 主机卡/状态页正常（状态 JSON 面断言）**；**R-1/R-2/R-3/R-4 三处旧臂行文 + 新臂行集** 逐字 grep 定稿并写入脚本常量区（含 `原因=` 取值集） | `/tmp/m6-ab/{S0,S1}.md`、`/tmp/m6-old/*`、`/tmp/m6-dev/s1-smoke.log` |
| **S2 出包 ×2 + 装配** | 两臂各出 HSP（手拷逃生口）+ 安装 + token 注入 + VPN 开关；`/ping` 在用档页；**pf 规则装配（§1.4-g）**；**服务面冒烟（files 浏览 + term 会话，各臂 1 次）** | S1 | 两臂各一次「层 0 全通」链在场（旧：`传输：新栈（wg-native-stack）→ warmup pong（判据=wg）→ attached`；新：`传输：新栈（QUIC 岛）→ 岛已建连 → attached`）；pf 规则断言行在场；files/term 冒烟留证；tier `git status` 零新增脏文件 | `/tmp/m6-dev/arm-{old,new}.md` |
| **S3 层 0 + 2×2 主测** | T1；T2（热/冷）；T3（含标定轮）；T4；T5 | S2 | 每格中位与原始件齐；主判据出「过/不达标/未验」三态之一；作废轮有登记；门槛 9 行对账表填初值 | `/tmp/m6-ab/{l0,t2-hot,t2-cold,t3-relay,t3-calib,t4-app,t5-pf}*` |
| **S4 CPU/耗电/内存采样** | T6/T7/T8/T9 | S3（可与 S5 并行） | 每轮 CPU 差分与 `--net` 字节成对落盘；lab 四格按新门槛判；耗电按降级口径登记 | `/tmp/m6-dev/cpu-*.tsv`、`/tmp/m6-ab/mem/`、`/tmp/m6-dev/{power.md,mem.tsv}` |
| **S5 漫游/恢复实录** | T10（两相位×2 臂）+ T11（两形态）+ R-4（中继腿楔子 1.5s）+ **时钟偏移实测（§2.4）** | S2 | 每类 ≥1 组行样例（§5.3 格式）+ T_recv 判据（含 `T_downtime` 列）；失败/未验按 §3.2 措辞登记 | `/tmp/m6-dev/{recover,roam,clock-offset}.*` |
| **S6 归因（条件触发）** | §4 逐层；只在不达标时启动；若需区分档位 ⇒ **可选 LTO 对照臂**（`/tmp/m6-old` 的 `Cargo.toml` 临时加 `[profile.release] lto=true, codegen-units=1` 重出 `.so` **与出口二进制**（T7 测出口 CPU），**worktree 内改动 + 登记**） | S3/S4 出「不达标」 | 命中层唯一 + 逐层读数表 + M7 决策输入；**不新增优化** | `/tmp/m6-ab/attrib.md` |
| **S7 M5 交下补齐（可顺延）** | ①DNS 回复 e2e（本机新用例 + 真机计数）②pf 形态 1/3（并入 T5）③旧 token 失败面归因行（可顺延） | ①S1；②S2；③S1 | ①本机用例绿 + 真机 `dns:` 计数增量；②两形态断言行；③真机实录该行（或显式登记顺延） | `/tmp/m6-dev/dns-*.log`、`/tmp/m6-ab/t5-pf.tsv` |
| **S8 报告入库 + 代码门 + 收口** | `docs/PERF-AB.md` **新增节（§9.20）** + `docs/reviews/M6.md`（含插桩清理证据 + 两轮意见 + **门槛 9 行对账表**）+ 收口材料交主会话 | 全部（或时间盒取舍序下的必需集） | §7.2 四条清扫全绿；`cargo test --workspace` + clippy + 三目标 check + `build-app-core.sh` 三门 + 隔离门 + WG 残留门 + 词表门全绿；门槛 9 行逐行有状态与证据路径；`PERF-AB` 新节入库；判据登记交实现棒（`INTEROP-CRITERIA.md`，**本设计不写**） | `docs/PERF-AB.md`、`docs/reviews/M6.md` |

**`PERF-AB` 新增节骨架（预登记，节号 = §9.20）**：

```
### 9.20 M6 真机终验（2×2 + 归因 + 漫游/恢复实录；YYYY-MM-DD，设备 FMR0224116011480）
  9.20.0 口径与环境（臂定义 + **分母替换登记** + 层 0 锚 + loadavg 带 + 时段；跨期不可互引声明；四条固定句）
  9.20.1 2×2 主表（4 格 × 冷热；中位/区间/有效轮数/作废计数；轮次原始指针）
  9.20.2 中继档：标定带 + relay/direct 比值 + 「分辨力」判读；分配腿峰值
  9.20.3 CPU / 内存 / 耗电（代理）逐格
  9.20.4 断线恢复 + 路径变更/腿切换实录（五列行样例；时钟偏移登记）
  9.20.5 归因（仅当触发；L0–L6 逐层判据表）
  9.20.6 未验与降级（蜂窝/迁移/窄路径/耗电/服务面 speedtest 的固定句法登记）
  9.20.7 门槛 9 行对账（状态 + 证据路径）
  9.20.8 结论行（每维一句 + M7 决策点）
```

---

## 10. 评审协议自检（设计门 checklist）

| checklist 项 | 本设计的答案 |
|---|---|
| **功能等价面** | 本棒不改产品行为；覆盖：全局代理（T2/T1）、DNS（S7①）、pf（T5 形态 1/2/3）、files/term（S2 冒烟）、恢复（T10/T11）；**speedtest 面显式登记「M6 无判据」**（T4 副读） |
| **边界与错误面** | 轮作废/有效轮下限（§2.4）、离散闸与带内不可分辨（§2.3）、未验固定句法（§3.2）、旧 token 失败面归因（S7③） |
| **并发与生命周期** | 两出口同刻常驻（实例 3/5 分离）；kill/重启相位；`worktree remove` + pidfile 清理；**不碰现役**（含 A/A 旁证的 `kill -9` 白名单） |
| **残留 WG 语义隐含依赖** | §0.2/§0.4 钉死两树差异（豁免腿、端口语义、token 前缀、行文、构建档位）；§1.4(e) 分臂断言（E14 订正）；§7.1 门模式防自伤 |
| **地道 Rust** | 不涉（本棒零代码）；S7③ 须按 typed error 面落（`docs/reviews/M5.md` 的 H-2 先例） |
| **安全面** | 只连本地私有出口；token 只作注入参数（不落库）；不加宽任何身份/准入面；`kill -9` 白名单 |
| **预算与可测性** | 每格中位 + 有效轮下限 + 作废规则；CPU 用累计时间口径；指纹/字节落盘；单轮不足为判（PERF-AB §9.8）；**时间盒取舍序**（§9） |
| **可 falsify** | §2.1 逐项给判据与证伪条件；§4 逐层给判据式；降级/未验有固定措辞与归属；门槛 9 行逐行对账（§3.3） |

---

## 11. 设计门记录（dsh r28 首轮 + r29 窄审）

### 11.1 轮次事实

| 项 | 值 |
|---|---|
| 目录 | **`/tmp/dsh-review/r28.Gpiw2c/`**（`prompt.txt` 6,759 B / **`output.md` 32,632 B = 完整报告** / `stderr.log` 220,184 B = 推理流，未发生正文落 stderr 的归并问题） |
| 命令 / **exit code** | `dsh --profile headless "$(cat prompt.txt)" > output.md 2> stderr.log` ⇒ **`0`**（成败只认 exit code） |
| 评审形态 | 独立读真源 + 双树代码复核（`git show 4841b20:` 对照）+ 只读实测现役二进制；给出**证据级**指控（文件:行） |
| 意见条数 | **30 条**（高 **8** / 中 **13** / 低 **9**）+ 正分 11 条 + 非阻塞建议 3 条 |
| **门结论** | **不通过**（14 条必闭合清单；建议修订后窄审 r29） |

### 11.2 逐条处置表（**认同改 / 不认同给证据**）

| # | 严重度 | 意见（摘要） | 本设计处置 | 落到 |
|---|---|---|---|---|
| **E1** | 高 | `T_recv` 起点被改成 kill 时刻 ⇒ 相 B 构造性必挂；仪器定义 = 出口 E1 `serve 就绪` | **认同**（回源码复核：`quic_ladder_e2e.rs:10/388` + M3-S8 算术逐位吻合）⇒ T10/§5.2/§5.3 改回仪器定义，停机段单列 `T_downtime`，补时钟偏移实测（§2.4） | T10、§2.4、§5.2、§5.3 |
| **E2** | 高 | 旧臂恢复行集不存在（`warmup pong`/`attached` 只在装配期；旧树无「快探」族） | **认同**（`git grep` 复核：旧树无「L3 承载」、无「快探」行；C11 族在 `tun_exec.rs:1848/1857`）⇒ 旧臂行集改**巡检失败族（`tun_exec.rs:1848/1857`）+ C11 `RECOVER` 行** + 行集 S1 定稿 + 删旧臂 `快探` 样例 | §0.2、§5.1 |
| **E3** | 高 | T3 判据与门槛行 :738（**经中继 vs 直连的比值**）不是同一条 | **认同** ⇒ T3 主判据改为「`新 (relay/direct) ≥ 0.95 × 旧 (relay/direct)`」，绝对值降为附加列并按门槛表要求只登记 | T3、§1.1 |
| **E4** | 高 | T3 轮件 256 MiB 与 relay 闸不符；平台阈值 1.5 MB/s 无出处 | **认同**（复核 `relay/mod.rs:51/64`：200pps/源 + 16 MiB/s 单会话下行桶）⇒ 轮件 16 MiB + **标定轮**（读数前落盘）+ 平台带 = 标定并带 ±25% + **限速形态写死**（r29 订正：限速 = 编译期内建常量，CLI **无** `--rate-limit`，两臂同一二进制、脚本不传 flag） | T3、§2.2 |
| **E5** | 高 | loadavg 阈值（>8）与真源（轮首 ≤4 / 峰值 >6）冲突；离散两套规则；无有效轮下限 | **认同** ⇒ §2.4 统一（轮首 ≤4、1 Hz 峰值 >6 作废、≥3 有效轮出中位、离散闸 max/min >1.3 单一口径） | §2.4、§2.3 |
| **E6** | 高 | T11 的 `N = 本批实测登记值` = 自证 | **认同** ⇒ N 预登记为 **2**（M1 登记锚 `M1-S5-evidence.md:221–227` 的 `分配腿 2`） | T11 |
| **E9** | 高 | L4/R-2 读数面指错（出口段无 `lost_packets`/`congestion_events`；`migrations` 在客户端段；出口键 = `pathChanges`/`drop*`） | **认同**（复核 `daemon/proto.rs:387–441` 28 键 + `facade/tun_exec.rs:596–598`）⇒ L4 拆两侧读数面；R-2 改设备侧 `migrations`/出口 `pathChanges` | §4-L4、§5.1-R-2 |
| **E10** | 高 | 真机窄路径**结构不可得**；楔子无按尺寸丢包能力 | **认同**（复核 `config.rs:26–28` 自陈「区间内产不出 mds<1280」；楔子只有黑洞窗）⇒ T12 真机项改「结构不可得登记」+ 删尺寸丢包配方 | T12、§3.2 |
| **E14** | 高 | §1.4(e) 建连断言行旧臂不存在（旧树无「L3 承载」） | **认同**（含 E2 同源证据）⇒ 断言分臂改写（旧 = `传输：新栈（wg-native-stack）` 等） | §1.4(e)、§9-S2 |
| **E13** | 中高 | §1.4(d) 中继命令语法破损/占位未定义 | **认同** ⇒ 全节重写为可粘贴段（rl1 来源 = `local-rust-relay.sh token 1`；四个 token 变量逐条给出）；**r29 补**：中继档 `start` 的 no-op 陷阱 ⇒ 补 `stop` 前置 + 切档顺序锚 | §1.4(d) |
| **E18** | 中高 | 「现役 ≔ 4841b20 重构建」缺 artifact 级证据 | **认同** ⇒ 升格为 §0.5 **分母替换登记**（含现役二进制证据）+ 可选 A/A 旁证 1 轮 | §0.5、§2.0 |
| **E20** | 中高 | 硬编码 SHA/尺寸与 S7③ 冲突（B 臂不再是终值树） | **认同** ⇒ 删硬编码，改「S1 入册值」+ §1.4(b) 顺序硬规则（先改后烘焙/或改述为「终值树 + 已登记增量」） | §1.4(b)、§9-S1 |
| **E22** | 中高 | 中继中立性论证错引（:877–883 = 附录 E）+ 回退路径不完整 | **认同**（复核 `git diff --stat` 确有实质改动）⇒ 论证改引 :105 + `rl1` 冻结 + v6 加法；补**前置互操作冒烟**；**r29 补**：回退形态写死（旧树二进制直起 + LAN 公布 + 第二变量登记） | §1.5.2、§1.4(d)、§0.2 |
| **E11** | 中 | 缺「最低有效轮 / 带内不可分辨」 | **认同** ⇒ §2.3.4 新增带内不可分辨条款 + §2.4 有效轮下限 | §2.3、§2.4 |
| **E7** | 中 | T9「产品形态单连接探针」无仪器/无命令 | **认同** ⇒ 改为「M5 终值判定 + 本批同族读数复跑」（并写明原始口径不可复现的既登记事实） | T9 |
| **E8** | 中 | `hidumper --cpuusage` 归一化/窗口未定义；pid 身份截断 | **认同** ⇒ 主口径改 Δ(`ps -o time=`) ÷ 轮墙钟；`--cpuusage` 降为旁证；pid 钉身份（`ps -ef` 全串 + `--mem` 进程名） | T6、§0.3 |
| **E19** | 中 | 漏「构建档位不对称」（旧无 LTO / 新有 LTO） | **认同** ⇒ §0.4-4 登记 + §4-L6 加层 + §9-S6 可选 LTO 对照臂 | §0.4、§4-L6、§9-S6 |
| **E21** | 中 | 旧核×现 App 兼容面只覆盖两面（漏状态 JSON 面）；且缺省化其实设计期可结案 | **认同** ⇒ §0.4-2 改写（缺省化 = 已知；状态 JSON 面 = S1 冒烟断言行） | §0.4、§9-S1 |
| **E24** | 中 | 归因层阈值是新造；缺「档位/中继平台」层；L1 作废线过紧 | **认同** ⇒ §2.0 声明「新增阈值」+ §4 加 L6 + L1 改 `max(2ms, 20% 中位)` + 1252/1254 口径说明 | §2.0、§4 |
| **E25** | 中 | 缺「门槛表逐行对账」 | **认同** ⇒ §3.3 新增 9 行对账表 + S8 判据含逐行状态 | §3.3、§9-S8 |
| **E26** | 中 | ①服务面 files/term 无旧臂冒烟、speedtest 无判据 ②`--net` 无消费者 | **认同** ⇒ S2 加 files/term 冒烟 + 登记「speedtest 无判据」；`--net` 升格为 T2/T3 设备侧交叉校验 | §9-S2、T2/T3、§3.2 |
| **E27** | 中 | 时间盒与工作量冲突（含 S7① 无顺延阀）；清理只查端口 | **认同** ⇒ §9 加时间盒取舍序（S7 整片含①可顺延）+ §2.5 清理改「先 pidfile 后端口」 | §9、§2.5 |
| **E30** | 中 | env 门控插桩须同批登记 | **认同** ⇒ §7.1④ 声明「本批不做 env 门控插桩」；若确需 ⇒ 登记 + 删后重跑门 | §7.1 |
| **E12** | 低 | 改判成立但「丢面」未进登记表 | **认同** ⇒ §3.2 第 1 行登记「交叉两格构成性不存在」 | §3.2 |
| **E23** | 低 | `836597d` 只在退路里，未作取舍论证 | **认同** ⇒ §1.3 用三条写清「为何不用」 | §1.3 |
| **E16** | 中低 | `aa start -U` / lo0 alias 形态待验；计时 server 过滤规则缺 | **认同** ⇒ 标「待实现期验证」+ 补 path 标签与客户端过滤规则 | §1.4(f)(h) |
| **E15** | 中 | T5 缺 pf 规则注入步骤 | **认同** ⇒ §1.4(g) 新增 pf 装配段 + 轮前断言 | §1.4(g) |
| **E17** | — | 冷热/单变量纪律「基本成立」（正分） | 认同（无需改） | — |
| **E28** | 低 | 门哨兵清单与门实现一致（正分）+ 两点补正 | **认同** ⇒ §7.1 记 `m6-serve.py` 不被扫但自律、`_wg` 为子串匹配 | §7.1 |
| **E29** | 低 | 清理 grep 会自命中 | **认同** ⇒ §7.2 加 `--exclude` | §7.2 |

**不认同项：无**（30 条全部认同并处置；其中 E1/E2/E3/E4/E5/E6/E9/E10/E13/E14/E18/E20/E22 = 13 条改了判据/命令面）。

### 11.3 轮次事实（r29 窄审）

| 项 | 值 |
|---|---|
| 目录 | **`/tmp/dsh-review/r29.C6N6zT/`**（`prompt.txt` 3,917 B / **`output.md` 22,374 B = 完整报告** / `stderr.log` 207,944 B = 推理流） |
| 命令 / **exit code** | 同 r28 ⇒ **`0`** |
| 评审形态 | 不信 §11.2 自述：逐条回源（HEAD 代码 + `git show 4841b20:` 双树 + 门槛表逐行 + tier 只读 + 现役二进制只读实测） |
| **门结论** | **有条件通过**——「14 条必闭合项 **11 条闭合 / 3 条部分闭合（E4/E13/E22）/ 0 条未闭合**；测量体系无结构性缺陷；放行条件 = 5 条最小修法必须在 S1 前落稿（局部文本，不动口径架构）」；新增问题：**高 0** / 中 5 / 低 12 |

### 11.4 r29 复核的逐条处置（**本设计已按此落 v3**）

**5 条放行条件（全闭合）**：

| # | 条件 | 处置（落到） |
|---|---|---|
| 1 | **E13**：中继档 `start` 是 no-op（已在跑即 `exit 0`）⇒ `--relay` 永不生效 | **改**：§1.4(d) 补 `stop` 前置 + 「切档 → 取 token → 改写 → 注入 → 点开关」顺序锚（附证据：`tools/local-rust-exit.sh` 的 `if our_pid … exit 0`，旧脚本同款） |
| 2 | **E22**：旧中继回退形态未写（旧脚本无 env 旋钮 + `127.0.0.1` 硬绑不可达） | **改**：§1.4(d) 写死旧树二进制直起命令（`:42783` + LAN 公布）+ 登记「中继二进制成为第二 A/B 变量」 |
| 3 | **E4**：`--rate-limit` 正文缺失且 §11.2 自述与事实不符（relay 无该 flag） | **改**：T3 正文写死「限速 = 编译期内建常量（`relay/mod.rs:51/64/122`）、`relay` 无 `--rate-limit`（`relay_cli.rs:50`）、两臂同一二进制」；§11.2 的 E4 行同步订正 |
| 4 | **M-1**：T5 `2+2` 与 §2.4「≥3 有效轮」互斥 | **改**：T5 形态 2 = **3+3**；§2.4 有效轮下限**限定适用范围**（中位判据档 = T2/T3/T5-形态 2；单轮抽查档不受约束） |
| 5 | **M-2**：R-1 样例 `原因=快探失败` 不存在；R-3 动作标签不符 | **改**：R-1 两处改 `原因=探活无回显` + 附取值集与单测锚（`ladder.rs:618–622/1037–1040`）；R-3 改 `链路动作选 R（新 QUIC 连接）（原因=…；…）`（`ladder.rs:534`） |

**12 条低项（全处置）**：L-1 T3 判据③ 改「登记 + 异常哨兵（≠1）」（N=2 只留 T11）／L-2 行族标签三分（巡检失败族 vs C11）／L-3 `docs/PERF-AB.md` 路径订正／L-4 门槛表行号订正（真机吞吐 **:732**、中继承载 **:738**）／L-5 引用订正（`QUIC-ROADMAP.md:105`、`DEVICE-TEST-OHOS.md:13`、待机档 `M3-S8-evidence.md:265–269`）／L-6 §1.4(b) 两分支都登记 diff 行数／L-7 §2.3.4 明确三态出口（T2/T5 = 过（带内注）、T3 = 未验）／L-8 T3 标定并带定义（两臂各 1 轮 ⇒ `min×0.75 … max×1.25`）／L-9 S6 的 LTO 对照臂同时重出**出口二进制**／L-10 §1.4 头部占位声明收窄／L-11 读数归集**以 `?r=` 标签为主**（客户端地址只作辅助，bridge100 型目标下源地址 = Mac 本地地址）／L-12 R-2 的「设备 `迁移完成` 行」标注「仅客户端自发起 rebind 时出现 ⇒ 判据以计数为准」。

**r29 残余非阻塞项（已一并落稿）**：1 Hz loadavg 采样器点名落 `tools/m6-ab.sh`；§0.5 的 A/A 旁证补客户端/驱动形态 + 现役二进制对 `--stun=` 空值的接受度（**待实现期验证**）；时钟偏移改「`hdc shell date +%s.%N` vs Mac `date` 各 10 次中位」为主形态；T3 最坏时长写入 §2.2 并进 §9 取舍序；§9-S1 判据扩到 **R-1/R-2/R-3/R-4 四处旧臂行文 + 新臂行集**的逐字定稿。

**不认同项：无**（r29 的 5 条条件 + 12 条低项全部认同并落稿）。

### 11.5 门结论（最终）

- **r28 = 不通过**（14 条必闭合）→ **v2 整改**（§11.2 逐条落点）→ **r29 = 有条件通过**（11 闭合 / 3 部分闭合 / 0 未闭合，高 0）→ **v3 落 5 条放行条件 + 12 条低项残余**。
- **门已过（通过）**：本设计即为 M6 实现棒的施工图（切片 S1–S8 + 口径预登记表 T1–T12 + 归因层 L0–L6 + 门槛 9 行对账）。
- **门后残余（实现期必守）**：①§1.4(b) 的烘焙顺序硬规则（S7③ 与 B 臂 `.so`）；②§2.4 的作废/有效轮纪律；③§7.2 的零插桩清扫；④§9 的时间盒取舍序（顺延/压缩必须登记）。

---

## 12. 主会话裁决（2026-10-10；**后到的切片以本节为准**）

1. **S7③「出口身份不符」可行动行 = 留 M7**（不做于 M6）：理由 = M6 的测量臂（尤其 B 臂 `.so`）必须**冻结**，
   任何 `crates/homeway-quic` 改动都会要求重烘焙并重跑主测；而该行本质是**上线前的可诊断性改进**
   （用户持旧 token 撞新出口时的可行动归因），放在 M7 的生产切换准备期做更合适。
   ⇒ M6 只登记（`docs/reviews/M6.md` 差异登记 + 归因批输入），M7 设计门承接。
2. **迁移维第二可达网络 = 不做**：设备无 SIM（已两轮实测）；真 `rebind()` 迁移归 **M7/用户触点**。
   M6 按设计用**已预登记的替代**（出口路径变更 + 中继腿切换）验「连接保持 + 腿峰值 ≤2」。
3. **测量窗口 = 必须独占**：主测（T2/T3/T5/T6/T10）期间 Mac 上不得有其它重负载（本机生产出口守护是轻量的、
   属常态基准，可留）；每轮 loadavg 与轮作废口径照设计 §2.2/§2.4 执行，**读数一律与轮首/轮末 loadavg 同时落盘**。
4. **门槛表按已批准口径判**：内存格 **单连接 ≤+640K**（2026-10-10 用户批准，失效条件已登记）；体积行 = product 档
   LTO 口径（`.so` 终值 2,958,896 B）。**判据面不因缺腿（蜂窝/真 rebind）降级**——缺腿项按设计 §3 的登记句法
   写「未验 + 归属」，**不得**写成「通过」或静默略过。
5. **S7 整片可顺延**（设计已给时间盒取舍序）：若 M6 主测耗时超预算，S7（DNS 回复 e2e / pf 形态 1·3 /
   旧 token 归因行）可顺延到 M7 前，但**必须在 `docs/reviews/M6.md` 显式登记**。
6. **tier 触点两条**（M7 承接，本棒只登记）：①`hmw2` 在 tier **粘贴面被 `TokenCheck`（只认 `hmw1`）拒**
   ⇒ 真机经 `--ps` 注入不受影响，但用户手工粘贴会失败；②`connection-lifecycle` 修订稿 + `log-index` 重生成。
