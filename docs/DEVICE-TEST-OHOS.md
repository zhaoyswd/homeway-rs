# OHOS 真机验证操作手册（homeway-rs × tier App）

> **定位**：可复用的**操作手册**（不是设计、不是判据真源）。把 M1 真机验证（2026-10-09）的实操
> 知识从易失的 `/tmp/m1dev-res/` 与一次性子代理报告里固化下来，供 M2 起的各期复用。
> **实操事实出处**：`/tmp/m1dev-res/SUMMARY.txt`（读数）、`/tmp/m1dev-res/round.sh`（脚本）、
> tier `tools/tailcat/README.md` / `docs/agents/{build,verification}.md`；line 号/命令形态均回源码复读。
> **本文件不含任何新判据**——判据行真源仍是 `docs/INTEROP-CRITERIA.md`。

## 0. 设备与纪律（**开工前先读**）

| 项 | 值 |
|---|---|
| 设备 | `FMR0224116011480`（HUAWEI ALN-AL00，API 24；`hdc list targets` 确认在连） |
| bundle | `me.zhaozhe.tier`（App + VPN 扩展进程 `me.zhaozhe.tier:vpn`，核 `.so` 在扩展进程内） |
| HDC | `/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/toolchains/hdc`（下称 `$HDC`，加 `-t FMR0224116011480`） |

**纪律（硬约束）**：① **只连本地私有出口**（`tools/local-rust-exit.sh start <n>` / `tools/local-rust-relay.sh`），
**绝不碰现役出口**；② 装机是**覆盖装**：会替换设备上同 bundle 的 App，并**拆掉用户正在跑的隧道**
⇒ **开工前先问用户，或先记录设备现状**（本地出口在跑几台、主机表条数、VPN 开关状态）；
③ 本仓不写 tier 跟踪文件（`docs/agents/log-index.md` 等由 tier 侧触点重生成）。

## 1. 出包（核 → HSP/HAP）

```bash
cd ~/Documents/projects/tier
HOMEWAY_RS=/Users/zhaozhe/Documents/projects/homeway-rs tools/tailcat/build-core.sh  # ① 核（必须最先）
#   注：M0–M3 的 worktree `…/homeway-rs-quic` 已合回 main 主检出 ⇒ 用 `/…/homeway-rs`（worktree 只作历史）
. tools/ohos-env.sh                                    # ② 注入 node/JBR/SDK 与 $HVIGORW
"$HVIGORW" --mode module -p module=tailcat@default  -p product=default assembleHsp  --no-daemon
"$HVIGORW" --mode module -p module=terminal@default -p product=default assembleHsp  --no-daemon
"$HVIGORW" --mode module -p module=entry@default    -p product=default assembleHap  --no-daemon
```
- `$HVIGORW` = `/Applications/DevEco-Studio.app/Contents/tools/hvigor/bin/hvigorw`（由 `tools/ohos-env.sh` 导出）；
  签证书 = `build-profile.json5` 的 `signingConfigs.default`（`~/.ohos/config/default_tier_*.{cer,p7b,p12}`，debug）。
- 产物：`tailcat/build/default/outputs/default/tailcat-default-signed.hsp`、`terminal/…/terminal-default-signed.hsp`、
  `entry/build/default/outputs/default/entry-default-signed.hap`。
- **必须 `assembleHsp`**：核 `.so` 由 HSP 打包，只跑 `assembleHap` 设备上的核**不会更新**。
- **pin 门**：`build-core.sh` 要求核仓 `HEAD == tools/tailcat/homeway-rs.pin` 或**其后代**（祖先语义；
  回退/分叉硬失败）。钉定/脏检出两条门的细节与逃生口见 script 头注释（正式出包不走逃生口）。

**已知拦点（M1 实测，2026-10-09）**：tier `docs/agents/log-index.md` 陈旧 ⇒ 公共门红
（`✗ docs/agents/log-index.md 不是最新的`），**门在 `cp` 之前** ⇒ `tailcat/libs/` 与 `prebuilt/` 保持旧产物
（fail-closed，不是半更新）。该门对 **main 检出（`4841b20`）亦红** = 既存问题，非 QUIC 专属。
- **临时绕过（仅限本地试验，必须留痕）**：核 `.so` 此时已在 `<核仓>/target/aarch64-unknown-linux-ohos/release/libclientcore.so`
  （homeway-rs 侧三道门已过），手拷到两处交付点：
  ```bash
  cp -f <核仓>/target/aarch64-unknown-linux-ohos/release/libclientcore.so \
        ~/Documents/projects/tier/tailcat/libs/arm64-v8a/ && \
  cp -f <同上> ~/Documents/projects/tier/tailcat/src/main/cpp/prebuilt/arm64-v8a/
  # 并记录：wc -c（字节）+ md5/md5sum 两处一致性（M1 实测 4,660,320 B / 两处一致）
  ```
- **正式路径**：tier 侧触点跑 `tools/docs/gen-log-index.sh` 重生成并提交（**本仓不做**）。

## 2. 安装

```bash
$HDC install -r \
  entry/build/default/outputs/default/entry-default-signed.hap \
  tailcat/build/default/outputs/default/tailcat-default-signed.hsp \
  terminal/build/default/outputs/default/terminal-default-signed.hsp
# M1 实测的等价形态（先推再装）：$HDC shell "bm install -p /data/local/tmp/mod"（覆盖装，返回 install bundle successfully）
$HDC shell "aa force-stop me.zhaozhe.tier"
```
- 首次启动系统会弹「是否允许使用 VPN？」——需在屏上点「允许」（无人值守用 `uitest uiInput click`，坐标自查）。
- release 签名包不能覆盖装在 debug 包的设备上（`bm` 报 9568336）⇒ 需先卸载（会清 App 内配置与 token）。

## 3. 免点屏注入 token（M1 实测可用）

> ⚠️ **本节为 M1/M5 历史实录**（含 `quic: ` 前缀、`token 端点 = :42651(WG)/:42652(QUIC)` 双端口形态）——
> **现行口径见 §8**；本节串按历史原样保留（登记 = `docs/INTEROP-CRITERIA.md`「判据变更记录」）。

```bash
$HDC shell "power-shell wakeup"          # 必须先唤醒屏幕，否则 aa start 报 10106102
TOK=$(tools/local-rust-exit.sh token 1)  # 本地私有出口的 token（核侧从 stdout/`serve token` 取）
$HDC shell "aa start -a EntryAbility -b me.zhaozhe.tier --ps host_token '$TOK'"
```
- `--ps host_token <token>` = 免点屏把 token 交给 App（等价「添加主机」粘贴）；换档/换 token 前先 `aa force-stop`，
  再带 `--ps` 重启（M1 的换档链实测如此）。
- 出口就绪判据行（本地出口日志 `/tmp/homeway-rs-rustexit-1/stdout.log`）：
  `quic: 端点就绪（[::]:42652，migration=true，initial_mtu=1400，datagram 缓冲 1048576B）`；token 端点 = LAN `:42651`(WG) / `:42652`(QUIC)。

## 4. UI 自动化（`uitest`）

```bash
$HDC shell "uitest dumpLayout -p /data/local/tmp/ui.json" && $HDC file recv /data/local/tmp/ui.json /tmp/ui.json  # 取控件坐标
$HDC shell "uitest uiInput click <X> <Y>"                                                                        # 模拟点击
```
- M1 实测（该机 1086×2340 量级）：VPN 开关 = 控件 `home-vpn-switch`（点击生效，实测坐标 (1086,778)）；
  首页测速卡片 (929,1332)；**换分辨率/系统版本后坐标必须重新 `dumpLayout` 自查**。
- **已验拦点**：Settings 内**导航点击正常**（说明注入本身可用），但 **Wi-Fi 主开关不吃模拟点击**
  （两次点击坐标取 Toggle 中心仍不改变状态）⇒ 「WiFi→蜂窝」这类**系统级开关切换只能人工**（或换注入面）。
- 屏幕超时/亮度类 override 用完要 restore（M1 已 restore）。

## 5. 日志面

> ⚠️ **本节为 M1/M5 历史实录**（含 `quic: ` 前缀、已删除的 `transport: 本世代 L3 承载 = quic`）——
> **现行口径见 §8**；本节串按历史原样保留（登记 = `docs/INTEROP-CRITERIA.md`「判据变更记录」）。

| 面 | 位置 / 命令 |
|---|---|
| 核日志（设备沙箱，隧道路径全判据） | `/data/app/el2/100/base/me.zhaozhe.tier/haps/entry/files/tailcat-tun.log` → `$HDC file recv <该路径> /tmp/tailcat-tun.log`（**用 `grep -a`**，文件含长行/二进制） |
| App/扩展日志 | 同目录 `tier-app.log`（另有 `tailcat-service.log`） |
| 系统面（崩溃/panic 扫描） | `$HDC shell hilog`（可按 tag 过滤，核/扩展相关 tag 含 `TierVPN`；M1 全量 89,978 行扫描） |
| 出口侧（本地私有实例） | `/tmp/homeway-rs-rustexit-<n>/stdout.log`；中继 `/tmp/homeway-rs-rustrelay-<n>/…` |

**QUIC 档判据速查（M1 实测行的稳定子串，供 grep）**：`transport: 本世代 L3 承载 = quic` /
`quic: 岛已建连` / `warmup pong: 就绪（判据=quic）` / `quic: 隧道面已附加（fd=` / `attached（数据面已接管` /
`link: via=direct ep=` / 出口侧 `peer: + dev=` + `quic: 连接采纳 dev=` + `intercept: tcp transit …（dialok）` /
`dns: q=… resp=…`。收工链无「已收工」字面量（以 `quic: 岛收工` / `正在回收` 结尾）。

## 6. 一轮验证的最小闭环（照 M1 的 `round.sh` 形态）

1) 起本地私有出口（`tools/local-rust-exit.sh start 1`）→ 2) 装机（§2）→ 3) `power-shell wakeup` + 免点屏注入 token（§3）
→ 4) `uitest` 点 VPN 开关（§4）→ 5) 拉核日志 + 出口日志核对判据（§5）→ 6) 收工：关 VPN 开关、停本地出口、
`git -C tier status --porcelain` 与开工前对照（零新增脏文件）。

## 8. 现行口径（M7 起）

> **本节 = 排障时该 grep 的串**（M7=生产切换期；§3/§5 的历史串与此处冲突时**以本节为准**）。
> 承载换代（M5：WG → QUIC 单承载）后的形态一次写清，避免照 §3/§5 的旧串排障。

**① 出口侧（`<state>/cache/debug.log` / 本地实例 `stdout.log`）**

| 判据 | 现行串（逐字） |
|---|---|
| E1 就绪 | `serve 就绪：quic=:41641（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true tokens=N key=<出口公钥前 12 hex>…`（**字段名 `quic=`**，此前为 `wg=`） |
| 公共端口 | **公共端口 = QUIC 端口**；显式配置 `serve.quic_listen`（缺省 = `listen+1`）⇒ 生产口径写死 `quic_listen = 41641`；**退让**时 `cache/quic_listen_port.txt` 写实际值 + 一行 `⚠️ QUIC 监听端口 … 被占用 —— 改用 …` |
| E-q6 出口 QUIC 面 | `出口 QUIC 面就绪（单承载；migration=true，initial_mtu=1400）` |
| E-q1 端点就绪 | `端点就绪（[::]:41641，migration=true，initial_mtu=1400，datagram 缓冲 1048576B）`（**无 `quic: ` 前缀**） |
| E-q2 准入 | `连接采纳 dev=<8hex> tun=<隧道 IP> ← <源地址>`；路径变化：`路径变更 dev=<8hex> <旧> → <新>` |
| E4 DNS 面 | `dns 代答就绪：tunnel=100.64.255.1:53（UDP+TCP）resolve=100.64.255.1:5300（TCP）upstream=…`；计数行 `dns: q=N qtcp=N resp=N filter=N trunc=N fallback=N fail=N drop=N malformed=N aaaa-mixed=N fakeip=N`（**60s 周期**） |
| E19 身份（连续性锚） | `后端身份：标签 <16hex> ｜公钥 <12hex>…`（换代前后**逐字不变**——身份根 = `serve/key.bin`，复用不轮换） |
| 中继角色 | `中继就绪：[::]:41741（token 模式（中继 ID …）；…）`（**双栈**；v0.2.x 的 `0.0.0.0:41741` 单栈形态已退役）+ `中继控制面：TCP 0.0.0.0:41741 就绪` |
| E25 收线 | `出口收线（连接数 N → 0，用时 …）` |
| 服务面 | `intercept: tcp transit …（dialok）`（L3 过境）/ `peer: + dev=…` / `peer: ~ dev=… refresh` |

**② 设备侧核日志（`tailcat-tun.log`）——单承载行集**

- `传输：新栈（QUIC 岛）`（**不再有** `wg-native-stack`）
- `岛已建连（候选 N 个，胜出 直连 <ip:port>，耗时 …ms）—— L3 承载 = 岛`
- `warmup pong: 就绪（判据=quic）` → `attached（数据面已接管 fd=…，L3 直通）` → `link: via=direct ep=<ip:port> rtt=…ms`
- 恢复族：`链路快探失败（…）` / `链路重连完成（原因=…，耗时 …ms）`（**T_recv ≤ 3.5s**，在用档）/ `路径变更` / `迁移未确认`
- 收工：`岛收工`（**无**「已收工」字面量）

**③ 已退役、不得再出现的串**（出现即回落世代或误读）

- `transport: 本世代 L3 承载 = quic`（A/B 开关行，M5 删除；已是 e2e **禁串**）
- `quic: ` 前缀形式的端点/迁移行（M5 批量去前缀）；`wg=` 字段；`回落 WG` / `尝试 WG 兜底` / `按承载分档`
- `hmw1` 客户端 token（**存量一律失效**——撞到就是「版本拒 + 换代归因」两段式报错：哨兵
  `homeway/token: 不支持的 token 版本: hmw1` + 归因「存量 token 已失效…请重新索取后重新粘贴」）
- WG 档服务腿 `intercept: tcp exempt`（服务面改走 `STREAM[tag]`）

**④ token / 端口速查（M7 起）**：客户端 token 前缀 **`hmw2`**（中继凭据 `rl1` 不变）；本地 Rust
出口实例 `n` 的**公共端口 = QUIC 端口 = 42651+n**（不再是「WG 4265n + QUIC 4265n+1」双端口）；
取 token 用 `tools/local-rust-exit.sh token <n>`（**守护在跑时输出是裸 token 一行、不在跑时带
`serve token：` 前缀**——两态通吃用 `sed -n 's/^serve token：//p;/^hmw[0-9]/p'`）。
