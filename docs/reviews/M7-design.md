# M7 设计（生产切换与文档收束 · WG → QUIC 换代收官期）

> **规格真源** = `docs/QUIC-ROADMAP.md` 的「M7 生产切换与文档收束」节（范围**照抄不扩缩**）+ 「用户触点
> 清单」+ 「M7 决策点（已裁）」+ 「每期执行协议」/「评审协议」。
> **方法真源** = `docs/DEPLOY-RUST-EXIT.md`（B 批换装/滚动升级形态）+ `docs/DEVICE-TEST-OHOS.md`
> （真机操作手册）；**读数真源** = `docs/reviews/M6.md`（M6/M6.5/M6.6/M6.7 四节）+ `docs/PERF-AB.md` §9.20
> （含 §9.20.9–11）；**基线口径** = `docs/QUIC-BASELINE.md`（L-1 档位）+ `docs/BASELINE.md`（冻结锚 `d4148f6`）。
>
> **本棒 = 设计棒**：只产出**可执行方案 + 回滚预案 + 失败预案 + 文档收束清单**，**不写产品代码、不碰生产出口**。
> 工作目录 = 主检出 `/Users/zhaozhe/Documents/projects/homeway-rs`（分支 `main`），开工 HEAD = **`cc4916c`**，
> 工作树干净（`git status --porcelain` 空）。
> **隔离声明（全程有效）**：①**现役出口绝对未碰**（只做只读侦察：读二进制/版本串/plist/state 布局/日志）；
> ②`homeway`/`tier`/`baseline/` 只读（tier 仅读 `homeway-rs.pin` 与 `TokenCheck.ets`）；
> ③**OHOS 设备未动**（只引记录）；④未 push / 未发 tag / 未建 PR；⑤未动 worktree `homeway-rs-quic`；
> ⑥本棒只写本文件（`docs/QUIC-ROADMAP.md` / `docs/INTEROP-CRITERIA.md` 一字未改）。

---

## 0. 结论摘要

| 维 | 结论 |
|---|---|
| **切换顺序（本设计核心）** | **兜底出口（阿里云）先升 → 设备核升级 → 主力出口（Mac）升**。**约束是硬的**：不变式 I（设备须始终有一条「核与出口同代」路径）⇒ **核升级必须夹在两次出口升级之间**（充分必要）；**「兜底先升」是优选解**（升兜底期间设备零扰动 + 该阶段回滚对设备零影响），「主力先升」为备用路径（代价 = 核仍旧代时多做一次人工切主机）（§3.3）。 |
| **代际矩阵** | **交叉两格均硬失败**（G1 核 × G2 出口 / G2 核 × G1 出口）；不变式 = **任意时刻设备至少有一条「核与出口同代」的路径**（§3.2）。 |
| **回滚最小单元** | **核 + 出口 + token 三者同代**（`hmw1` 存量 token 一律失效 ⇒ **换出口必须同批换 token**）。回滚件**必须持久化**（M6 归档在 `/tmp` 的旧件属易失面，本设计定「转存 + 校验」为切换前置，§4.0-C）。 |
| **新发现（本棒，阻塞级）** | **QUIC 公共端口缺省 = `serve.listen + 1`**（`engine.rs:150-152`）⇒ 生产 `listen = 41641` 且未配 `quic_listen` 时新出口会监听 **41642**。**本设计裁定：显式 `quic_listen = 41641`**（端口不变、防火墙零变更）；代价 = 该键是 M1 新键 ⇒ **回滚必须还原 config**（旧二进制 `deny_unknown_fields` 会拒启，§4.0-D）。 |
| **失败预案** | **13 条**（F1–F13，逐条判定信号 + 动作）+ **最坏情况四选项**（按「回退手段序」从小到大，§5）。 |
| **tier pin 取值** | = **C1 发版 commit**（= 两台出口二进制 + 设备核 `.so` + Release tag 的同一构建源；`S1–S3` + 版本 bump，过 S4 门后推 main 并打 tag），**不是**当前 HEAD `cc4916c`；S5 的文档-only 后代 commit 不后移 pin。理由与自查命令见 §7.1 / §4.0-A。 |
| **判据登记待补** | **7 条**（**设计门 r30 高危 3 纠正**：原先自陈「1 条」为假——M5 有 5 条自陈欠登、M6.5 是**有产品改动**的批（非纯测量）、M6.7 另有 1 条新增观测行）。逐条见 §9。 |
| **M6 顺延项** | **11 条**逐条处置：**本期做 4**（T11 替代注入 / S7① DNS e2e / S7③ 旧 token 归因行 / T9+内存账复跑）、**转交用户触点 2**（S7② pf 形态 1·3 / 蜂窝与真 rebind）、**登记不做 5**（T5 / T3 / 待机档 / T8 耗电 / T12 窄路径真机）（§10）。 |

---

## 1. 目标与范围（照抄路线文件，勿扩缩）

**目标**：两台生产出口滚动升级（用户触点）+ tier pin 前进 + 文档收束。

**范围**（逐条 = 本设计的章节落点）：

1. **滚动升级两台生产出口**（`DEPLOY-RUST-EXIT.md` 形态；**含回滚步骤与失败预案**）→ §4 / §5；
2. **tier 触点**：pin 前进 + `connection-lifecycle` 定稿交付 + App 侧适配清单 → §7；
3. **文档收束**：README / AGENTS「技术底座」更新（boringtun→quinn、ring-shim 退役）+ CHANGELOG +
   本路线文件标记收官 → §8。

**判据**：生产真机烟囱全绿；文档指针一致（AGENTS 速查表 / README / 本文件）；判据登记全。

**评审**：设计门 = 本文件（切换步骤 / 回滚步骤 / 失败预案）→ dsh；代码门 = `docs/reviews/M7.md`
（文档一致性）+ 主会话终检。**退出口**：换代完成；路线文件标记收官。

**非目标（明写防扩张）**：

- **不做**「M6.7 剩余 11% 速率控制」的修复（`M6.md` §12.8.8 列三条候选，均须**另开批**）——M7 是在
  已知差异（T2 热 **0.893×** / T6 **1.36×**，`PERF-AB` §9.20.11）之上做**可回退的生产切换**；
- **不做** relay 协议改动（中继零改动红线）；**不做** tier 产品结构改动（只出清单与草案）；
- **不重做**已归档术语体系（除 §8 点名的行）。

---

## 2. 现状侦察（**只读**，逐条可复现）

### 2.1 本机生产出口（Mac / launchd）

| 项 | 值（命令） |
|---|---|
| 二进制 | `/Users/zhaozhe/bin/homeway-rs`，**7,119,496 B**，`--version` = **`homeway-cli v0.2.3`**，sha256 `98f337e54c889d0fa0e72d8e8921bc2139f717e7a314f7b900197f865e80e620`（`shasum -a 256`） |
| 进程 | pid **33667**，`/Users/zhaozhe/bin/homeway-rs --state /Users/zhaozhe/.config/homeway-rs`（`ps -ef`）；launchd `me.zhaozhe.homeway-exit`（`launchctl list` 第二列 `-9` = 上次退出码，当前 running） |
| plist | `~/Library/LaunchAgents/me.zhaozhe.homeway-exit.plist`：`KeepAlive/RunAtLoad=true`；`ProgramArguments=[bin, --state, ~/.config/homeway-rs]`；stdout+stderr → `~/.config/homeway-rs/cache/exit.log`；`WorkingDirectory=~/.config/homeway-rs` |
| state 布局 | `config.toml`（`[serve] enabled=true listen=41641 relay="rl1…"`；`[relay] enabled=false`；其余缺省）/ `serve/{key.bin(32B), tokens.jsonl(33 行), files.sock, term.sock, speedtest.sock}` / `cache/{events.log,debug.log,exit.log,listen_port.txt,public_endpoint.txt}` / `control.sock` / `lock` / `client/` / `relay/`（空） |
| **身份（连续性锚）** | `后端身份：标签 d07c57dd5bde1fa7 ｜公钥 8aee740b7220…`（2026-10-07 16:28 最后一次启动，与 10-05 首次换装**逐字节同**）；`serve 就绪：…key=8aee740b7220…`；**`key.bin` 语义 = 「存在即复用——升级不轮换」**（`crates/homeway-core/src/server/state.rs:3`） |
| 端点公布 | `cache/public_endpoint.txt` = `114.242.60.128:41641` + `[2408:8207:2518:2550:…]:41641`（双栈各一行）；`cache/listen_port.txt` = `41641` |
| 设备面 | `debug.log` 末行族：`peer: ~ dev=aca645d3 refresh (idle=5m1s) n=2/32` ⇒ 设备在用连接（devTag `aca645d3`）在册；`serve 就绪` 行 `tokens=28` |
| **回滚件在位度** | `~/bin/` 有 `.bak-v0.2.0 / .bak-v0.2.1 / .bak-v0.2.2`（历史滚动留存），**没有 `.bak-v0.2.3`** ⇒ 现役件本身即唯一 v0.2.3 副本 ⇒ 切换前**必须**先自拷一份（§4.0-C） |

### 2.2 第二台生产出口（阿里云，serve + relay 双角色）

| 项 | 值（来源 + 只读探测） |
|---|---|
| 主机 | `123.56.218.212`（公网 v4；`ping -c 2` = **10.9 ms 中位，0% 丢**，2026-10-08 本棒实测）；内网 `172.17.12.106/20`（`DEPLOY-RUST-EXIT.md` §2） |
| 二进制 | `/usr/local/bin/homeway-rs` = v0.2.3 linux-amd64（sha `2dabce88…`，`DEPLOY-RUST-EXIT.md` §11）；**无自启**（nohup 形态，重启机器需重跑命令行，§2） |
| state | `/opt/homeway/data-rs`（config：`[serve] enabled=true listen=41641 files_root="/opt/homeway/served"`；`[relay] enabled=true listen=":41741" advertise="123.56.218.212:41741"`） |
| 端口 | UDP 41641（serve）+ UDP/TCP 41741（relay，同一 pid）；relay 控制面 TCP 41741 **只读探测可达**（`nc -z -G 3 123.56.218.212 41741` succeeded） |
| 备份 | `/usr/local/bin/homeway-rs.bak-v0.2.0/1/2` + `/opt/homeway/backups/*.tar.gz`；**同样无 `.bak-v0.2.3`** |
| 中继互注 | Mac `serve.relay = "rl1…"`（阿里云 Rust 中继）；阿里云侧判据行 = `中继：后端 d07c57dd5bde1fa7 注册成功（腿 114.242.60.128:41641）`——**后端标签 = Mac 出口身份派生 ⇒ 换代后应逐字不变**（身份连续性锚，§4.2 判据） |

### 2.3 设备 / App 侧形态

| 项 | 值 |
|---|---|
| 设备 | `FMR0224116011480`（ALN-AL00），bundle `me.zhaozhe.tier`（`docs/DEVICE-TEST-OHOS.md` §0） |
| **tier pin（现值）** | `cbd45f0439e660fd75ee83d5953c745959efba82`（= v0.2.2 核；`git log -1` 确认存在）= **G1 核**；设备现装的 `.so` 即此树的产物 |
| **粘贴面拦点** | `tier/entry/src/main/ets/model/TokenCheck.ets:18` `const HMW_PREFIX = 'hmw1';` ⇒ **`hmw2` 串在 App UI 粘贴被本地语法校验拒**（M6 真机即走 `--ps host_token` 免点屏注入绕过，`M6-design` §66） |
| 出包门 | tier `build-core.sh` 的公共门（`docs/agents/log-index.md` 陈旧 ⇒ 红）⇒ M5/M6 两次走**手拷逃生口**（`.so` → `tailcat/libs/arm64-v8a/` + `tailcat/src/main/cpp/prebuilt/arm64-v8a/`） |
| 旧核回滚件 | M6 归档 `/tmp/m6-ab/artifacts-old/libclientcore.so` = **2,348,864 B**（= `4841b20` 树，G1 核）+ `homeway-cli` 9,098,432 B；**在 `/tmp`（易失）** ⇒ §4.0-C 要求转存 |

### 2.4 新代产物形态（本仓 main）

| 项 | 值 |
|---|---|
| HEAD | `cc4916c`（工作树干净） |
| 核 `.so` | 最近实测 = **2,968,016 B**（M6.7 修复后；md5 `2a348ff13deeeaa5c59fc649531c7fbc`）；M5 终值 2,958,896 B；判据 ≤3,800,000 B ⇒ 余量充足 |
| 出口二进制 | 本机 `target/release/homeway-cli` = **8,761,040 B**（非 strip、`--version` = `homeway-cli (devel)`，因 `HOMEWAY_CLI_VERSION` 未注入）；M5 同档读数 8,758,816 B / M6 臂 8,759,072 B（**不设判据**） |
| 版本注入 | `option_env!("HOMEWAY_CLI_VERSION")`（`crates/homeway-cli/src/main.rs:32-36`）；release 流水线用 `HOMEWAY_CLI_VERSION=<tag>` 注入并烟囱断言 `--version` 含 tag（`.github/workflows/release.yml:58-70`）⇒ **发版路径要求 tag 与构建字符串一致** |
| 判据行形态（新代，供切换后核对） | E1 `serve 就绪：quic=:41641（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true tokens=N key=8aee740b7220…`（**L-2 已登记**：`wg=`→`quic=`，其余字段逐字不变）；E-q1 `端点就绪（[::]:41641，migration=true，initial_mtu=1400，datagram 缓冲 1048576B）`；`出口 QUIC 面就绪（单承载；migration=true，initial_mtu=1400）`；E-q2 `连接采纳 dev=… tun=… ← …`；E19 `后端身份：标签 … ｜公钥 …`（**`quic: ` 前缀已随 L-6 批量删除**） |

---

## 3. 代际定义与代际矩阵（**本设计的第一性依据**）

### 3.1 两代定义

| 代号 | 出口 | 核（App 侧 `.so`） | token |
|---|---|---|---|
| **G1（旧代）** | v0.2.3 及以前（WG 单承载：`boringtun` + 自管候选/镜像/腿表；E1 行 `wg=:41641`） | pin ≤ `cbd45f0`（WG 核：`wtransport`/`wgcore`/`session` 阶梯） | `hmw1`（定长布局；`EndpointKind` ∈ {Direct, Relay}） |
| **G2（新代）** | 本仓 main（QUIC 单承载：quinn + rustls RPK；E1 行 `quic=:41641`） | main 的 `capi` 产物（QUIC 岛 + `facade`；`.so` 2.9x MB） | `hmw2` 段容器（+`rpk` 段 + `EndpointKind::Quic`） |

### 3.2 代际矩阵（四格，逐格给证据）

| 核 \ 出口 | **G1 出口**（v0.2.3） | **G2 出口**（main） |
|---|---|---|
| **G1 核**（pin ≤ cbd45f0） | ✅ **全通**（当前生产态；`DEPLOY-RUST-EXIT.md` 全程实录） | ❌ **硬失败**（两层，任一即死）：①出口只铸 `hmw2`，旧核 `PREFIX='hmw1'`（`git show cbd45f0:…/token.rs:34`）⇒ 版本拒；②**即便绕过①**，旧核 `EndpointKind::from_wire` 是「**非 1 一律按 `Direct` 收**」（宽松，不报错）⇒ 它把 `Quic`(type=2) 端点**误认成直连/WG 端点**、对 QUIC 端口发 WG 握手，必死（叠加出口已无 WG 面） |
| **G2 核**（main） | ❌ **硬失败**：①旧出口只铸 `hmw1`，新核 `TokenError::UnsupportedVersion`（**存量 token 一律失效**，`token.rs:21-23`）；②新核只走 QUIC 承载（WG 数据面已不存在） | ✅ **全通**（M5/M6/M6.7 真机与 e2e 已验） |

**证据链（逐条可回源码/记录）**：

1. `crates/homeway-core/src/token.rs:4-23`：`hmw2` 段容器为唯一可读版本；`hmw1…` 与任何其它 `hmw*` ⇒ `UnsupportedVersion`（登记 = `INTEROP-CRITERIA` 的 **L-12**）。
2. `crates/homeway-core/tests/quic_wg_e2e.rs:1-12`：正向 = 「铸出的 token 带 `rpk`、端点**全是 Quic 类**（`Direct` 零命中）」
   ——**该正向用例标 `#[ignore]`（需外部本地出口），非常跑面**；常跑的是「存量 WG-only token **可见失败**」负向用例
   （设计门 r30 低-18 注记，防后误解为已在 CI 面验过）。
   另注：`hmw2` **仍接受 `Direct`(wire 0) 端点**（宽松 `from_wire` 是保留解析面；本代不产出该形态）。
3. M5 删除面：`wgcore`/`wtransport`/`boringtun`/`ring-shim`/`session/recover` 旧档全删（净删 ≈ −13,269 行；`check-wg-removed.sh` 十条常驻门）。
4. matrix 行集退役裁决（`docs/QUIC-ROADMAP.md` C4 裁决②）：`L2_L3_RETIRED=(L2 L3 L4 L5)`——「Go 出口 × Rust 客户端」等交叉构型**不存在**；同口径外推：**WG 核 × QUIC 出口 / QUIC 核 × WG 出口亦不存在**。
5. 身份面**不构成**第三个变量：出口 RPK 私钥种子 = **`HKDF(后端静态私钥, "homeway/quic-rpk")`**（`crates/homeway-quic/src/exit/mod.rs:157-159`），后端静态私钥 = `serve/key.bin`（复用不轮换）⇒ **换代不换身份根**；E19 标签 / E1 `key=` 在换代前后应逐字同（§4.2/§4.3 判据）。

### 3.3 顺序约束与「谁先谁后」的推导

**不变式 I（安全线）**：切换全程任意时刻，设备至少有一条「**核与出口同代**」的可达路径。
违反 I = **设备侧隧道全断**（不是降级，是断）。

**由 I 推出的充要条件**：在「两台出口都要升」的前提下 ——

- 核**不能最后**升：第 2 台出口升完到核升完之间，旧核零可用出口 ⇒ 违反 I；
- 核**不能最先**升：核升完到第 1 台出口升完之间，新核零可用出口 ⇒ 违反 I；
- ⇒ **核升级必须夹在「第 1 台出口升级」与「第 2 台出口升级」之间**（充分必要，与哪台先升无关）。

**⇒ 两种可行顺序**（本设计选①）：

| 顺序 | 逐步推演 | 评价 |
|---|---|---|
| **① 兜底（阿里云）先 → 核 → 主力（Mac）** | 升阿里云后：设备仍走 Mac（G1），**零扰动**；核 + 贴阿里云新 token 后：设备走阿里云 G2；升 Mac 期间设备走阿里云 G2 | **优选**：①升阿里云期间设备**完全无感**（V9）；②核升级的验证有阿里云 G2 做对照；③**该阶段回滚对设备零影响**（Mac 从未动过） |
| ② 主力（Mac）先 → 核 → 兜底（阿里云） | 升 Mac 后：设备必须**人工切到阿里云 G1**（多一次切主机 + 该窗口内设备只有远程出口）；核升级后设备切回 Mac G2；最后升阿里云 | 可行（同样满足 I）但代价更高：多一次人工切主机 + 核升级窗口靠远程出口 |

> **设计门 r30 中-5 纠正**：v1 稿把①写成「被矩阵逼出的**唯一**解」，论证偷换了命题（用「两台都先升」去否证「任一先升」）。
> 正确表述 = **充要条件是「核夹在两次出口升级之间」**；**①是优选而非唯一**。②作为备用路径**列在案**
> （适用情形：阿里云窗口/ssh 不可得而 Mac 窗口可得；代价见上表）。
> **第三台临时出口（真零中断滚动）方案**：已评估 = **不做**（需新身份/新 token/新端口/安全组变更，
> 且第三台本身就是未验证变量，成本 > 收益）。

**推论（回滚粒度与手段序）**：不变式 I 同样约束回滚 ⇒ **回滚的最小单元 = (设备核 + 至少一台出口 + 至少一枚同代 token)**。
由此得三条纪律与一个**手段序**：

1. **回退出口必须同批回退 token**（`hmw2` 对新核 / `hmw1` 对旧核，不可跨代）；
2. **核已升级时「只回退一台出口」不成立**（G1 出口对 G2 核无用）；
3. **回退手段序（代价从小到大，设计门 r30 中-6 补）**：①**设备切到另一台 G2 出口**（一次粘贴，零出口动作、零核回退）
   → ②单台出口回退 **且必须同批**核回退 → ③双台出口回退 + 核回退 + 两枚旧 token。§4.3 回滚与 §5 最坏情况按此序写。

---

## 4. 切换方案（逐台、可回滚；**全部动作 = 用户执行**）

### 4.0 共同前置（每台出口都做；A 步之前一次做完）

**A. 发版/取件（用户触点）——发版点 = C1（**设计门 r30 中-13 钉死**）**

> **C1 = 发版 commit**：`S1–S3 全部落地` + **`Cargo.toml`（0.2.3→0.3.0）与 `Cargo.lock` 四处版本号 bump**
> + S4 门通过 ⇒ 推 main ⇒ **打 tag `v0.3.0`**。**tag 不得早于 S1–S3**（否则部署的二进制不含 S1 的
> 旧 token 归因行，与「上线前可诊断性」自相矛盾）。S5（文档收束）的**文档-only 后代 commit 不改变产物**，
> 因此 **pin = C1**，不必随 S5 后移（§7.1）。

```
# 发版前自检（在 C1 commit 上）
git -C /Users/zhaozhe/Documents/projects/homeway-rs status --porcelain   # 期望空
grep -n '^version' Cargo.toml Cargo.lock | head -6                        # 期望 0.3.0（四处）
git log --oneline -1                                                      # = C1
# 发版（用户触点；纪律门要求 tag == origin/main 头）
git tag -a v0.3.0 -m "传输层换代：WG → QUIC 单承载（M0–M7）" && git push origin v0.3.0
# 方式②（无 tag 应急）：本机构建 + scp —— 代价 = 失去 CI 冒烟与 SHA256SUMS 资产，须人工记 sha256
#   darwin-arm64: HOMEWAY_CLI_VERSION=v0.3.0 cargo build --release --locked -p homeway-cli
#   linux-amd64 statically: CI 配方（musl + clang + tools/cc-check-shim），见 .github/workflows/release.yml
```

**期望输出**：Release 五 job 全绿；`SHA256SUMS` 双端 `--check` OK；`homeway-rs --version` = `homeway-cli v0.3.0`。
**失败分支**：CI 红 ⇒ 按失败 job 修（**不改 tag**，删 tag 重打：`git push --delete origin v0.3.0`）。

**B. 配置面变更（**两台都要**）：显式 `quic_listen = 41641`**

```toml
[serve]
enabled = true
listen = 41641
quic_listen = 41641     # ← 新增行（缺省 = listen+1 = 41642，见下方理由）
relay = "rl1…"
```

**理由（新发现，阻塞级）**：QUIC 公共端口 = `serve.quic_listen`，**缺省 `serve.listen + 1`**
（`crates/homeway-core/src/server/engine.rs:148-152`；`nodestate.rs:257-258` 的配置模板同述，**M5 起这是出口唯一的公共 UDP 端口**）。
不显式化 ⇒ 端口变 41642 ⇒ ①阿里云安全组须开新端口（公网面变更）；②路由器/NAT 与 `public_endpoint.txt` 全部换号；
③`docs/DEVICE-TEST-OHOS.md`/排障文档既有 41641 口径全失效。**显式化 ⇒ 端口、防火墙、NAT、文档口径全部不变。**

**C. 回滚件持久化（**不落 `/tmp`**；切换前置硬项）**

| # | 件 | 落点（建议） | 校验 |
|---|---|---|---|
| 1 | 两台出口现役二进制 | `~/bin/homeway-rs.bak-v0.2.3`（Mac）/ `/usr/local/bin/homeway-rs.bak-v0.2.3`（阿里云） | `shasum -a 256` 与现役件**逐字节同**；Mac 期望 `98f337e5…` |
| 2 | 两台 `config.toml` | `config.toml.bak-pre-m7`（同目录） | `diff` 应只差 `quic_listen` 行（升级后对照） |
| 3 | plist（Mac） | `~/Library/LaunchAgents/me.zhaozhe.homeway-exit.plist.bak-pre-m7` | `plutil -lint` OK |
| 4 | **两枚旧 `hmw1` token**（设备侧现存两主机条目） | 用户私密记录（不进仓） | `serve token` 逐字抄；Mac 现值为 `hmw1iu50C3Ig…`（4 端点） |
| 5 | **G1 核 `.so`**（2,348,864 B） | 从 `/tmp/m6-ab/artifacts-old/` **转存**到持久目录（如 `~/homeway-rollback/`） | `shasum -a 256`；两处交付点手拷时 md5 一致（`DEVICE-TEST-OHOS.md` §1 逃生口） |
| 6 | 旧 HSP 重建配方 | `docs/DEPLOY-RUST-EXIT.md` §12（本批新增节） | — |
| **7** | **两台 state 全量 tar**（**设计门 r30 中-7 补**）：含 `serve/key.bin` / `serve/tokens.jsonl` / `serve/revoked.jsonl`（若有）/ **`relay/relay.key`** / `config.toml` | Mac `~/homeway-rollback/state-mac-pre-m7.tar.gz`；阿里云 `/opt/homeway/backups/data-rs-pre-m7.tar.gz`（先例：`DEPLOY-RUST-EXIT.md` §9「13 条目」/ §10「data-rs-pre021」同口径） | `tar tzf … \| wc -l` 条目数留证；`key.bin`/`relay.key` 在包内（`tar tzf \| grep`） |

> **为什么第 7 件是硬项**：`key.bin` 丢失 = 身份轮换（`state.rs:3`「存在即复用」的反面：**换了就全变**）
> ⇒ 所有 token 作废、设备派生身份变化；`relay/relay.key` 丢失 = Mac 的注册腿与设备的中继兜底腿同时失效。
> 先例（B 批两次换装、三次滚动升级）每次都做 state tar，本设计 v1 遗漏（r30 中-7 纠正）。

> **纪律**：`/tmp` 会被系统清理；M6 收口把旧臂产物留档在 `/tmp/m6-ab/artifacts-old/` 属**易失面**——
> 本设计把「转存 + 校验」列为 A 步之前的硬前置（缺任一件 = 不做切换）。

**D. 回滚的配置面陷阱（**必读**）**

`serve` 配置表是 **`#[serde(deny_unknown_fields)]`**（`crates/homeway-cli/src/serve_cli.rs:35`），而 `quic_listen`
是 **M1 新键**（旧二进制 v0.2.3 不认识）⇒ **回滚到 v0.2.3 时必须同时还原 `config.toml`（删掉 `quic_listen` 行），
否则旧二进制拒启退出**（同款先例已在册：`serve.quic` 键退役时「旧文件里的该键点名报错 + 拒启」，
`serve_cli.rs:1519-1544`）。⇒ 回滚步骤一律写「**换二进制 + 还原 config + 启动**」三件套，不许只换件。

**E. 台账跨代可读性（**设计门 r30 中-16 补；已在本棒核，回滚链条上唯一可能致命的未知量**）**

新出口会把 `hmw2` 行 **append** 进 `serve/tokens.jsonl`；回滚后 v0.2.3 启动时会 `secrets()` 读**整张台账**
（含新代追加行：多 `rpk` 键 + `kind:2` 端点）。**核验结论 = 可读、安全**：

- v0.2.3 的 `TokenRecord`（`git show da95496:crates/homeway-core/src/server/state.rs:100`）**无 `deny_unknown_fields`**
  ⇒ 多出的 `rpk` 键被忽略；
- `endpoints` 在旧结构里是 `serde_json::Value` 原样透传；`from_wire` 宽松（非 1 ⇒ `Direct`，
  `git show cbd45f0:…/token.rs:82-88`）⇒ 新代 `kind:2` 行不会让旧出口 `BadTokenLine` 拒启。
- **可复跑校验（列入 §6 清单第 13 项）**：`cp -a <state>/serve /tmp/rollback-probe && <bak-v0.2.3> serve token --state /tmp/rollback-probe` ⇒
  期望 **rc=0** 且打印 `hmw1…`（旧末行）。

**F. 凭证卫生（设计门 r30 低-25 补）**

`hmw2`/`hmw1` 是**凭证**：①注入前后 `set +o history`（或注入后 `history -D` 清行）；②token 只经 shell 变量传递、
**不落任何报告/commit/评审原文**（`DEPLOY-RUST-EXIT.md` 的掩码先例：只留前缀 + 尾 4）；③不从
`cache/unified-stdout.log`/`exit.log`（无轮转件）抄 token（那里可能有多轮历史明文），一律用 `serve token` 现取；
④切换完成后核对 `~/.zsh_history` 无残留。

### 4.1 阶段一：阿里云出口 → G2（serve + relay 双角色）

**切换前记录（在同一会话内取，供回滚与对账）**

```bash
# 在阿里云主机（用户执行）
/usr/local/bin/homeway-rs --version                     # 期望 homeway-cli v0.2.3
uname -m                                                # 期望 x86_64（≠ 拿 linux-arm64 件，DEPLOY §10 的 Exec format error 教训）
sha256sum /usr/local/bin/homeway-rs                     # 期望 2dabce88…（与 DEPLOY §11 一致）
grep -a "后端身份\|serve 就绪\|中继就绪" /opt/homeway/data-rs/cache/{events.log,unified-stdout.log} | tail -5
#   落盘到 /tmp/m7-gates/aliyun-pre.txt（供 V3/V22 机械 diff，勿肉眼比对）
#   记录：后端身份标签 / key=… / tokens=N / 中继 ID（回滚后须逐字同）
# 旧 token 抄存（**注意**：serve token 输出为三行中文，须剥前缀；运行中读取安全——该命令不取实例锁）
/usr/local/bin/homeway-rs serve token --state /opt/homeway/data-rs | sed -n 's/^serve token：//p'
#   ↑ 期望单行 hmw1…；立即存入用户私密记录（§4.0-C-4/§4.0-F）
cp /usr/local/bin/homeway-rs /usr/local/bin/homeway-rs.bak-v0.2.3
cp /opt/homeway/data-rs/config.toml /opt/homeway/data-rs/config.toml.bak-pre-m7
# V15 前置：放一个已知 sha256 的样本（阿里云 files_root=/opt/homeway/served 是空目录，DEPLOY §2 已如实登记）
head -c 1048576 /dev/urandom > /opt/homeway/served/m7-probe.bin && sha256sum /opt/homeway/served/m7-probe.bin
```

**换装（顺序 = 先停进程再换件，`DEPLOY-RUST-EXIT.md` §9 的 `Text file busy` 教训）**

```bash
pkill -x homeway-rs
until ! pgrep -x homeway-rs >/dev/null; do sleep 1; done          # 等退净（宽限 drain 数秒）
ss -lunp | grep -E '41641|41741'                                  # 期望：无输出（端口已释放）
scp homeway-cli-v0.3.0-linux-amd64.tar.gz root@123.56.218.212:/tmp/   # 上传（或用户惯用通道）
ssh root@123.56.218.212 'cd /tmp && tar xzf homeway-cli-*.tar.gz && sha256sum -c SHA256SUMS && \
  install -m 0755 homeway-cli /usr/local/bin/homeway-rs && /usr/local/bin/homeway-rs --version'
# 期望：SHA256SUMS 校验 OK；--version = homeway-cli v0.3.0
# 编辑 config：加 quic_listen = 41641（§4.0-B）
cd /opt/homeway && HOME=/opt/homeway/served nohup /usr/local/bin/homeway-rs \
  --state /opt/homeway/data-rs >> /opt/homeway/data-rs/cache/unified-stdout.log 2>&1 &
```

**验证点（逐条 = 判据行 + 期望输出）**

| # | 判据 | 期望（`unified-stdout.log`/`debug.log`） |
|---|---|---|
| V1 | E1 就绪 + **端口未退让（设计门 r30 高-2 改写为可判定）** | E1：`serve 就绪：quic=:41641（配置端口；被占用会自动退让）tunnel=… files=7802 term=7724 speedtest=7803 dns=true tokens=N key=<与升级前同串>…`（**注**：E1 打的是**配置值**，退让与否不由它判定）+ **两条硬判**：`cat /opt/homeway/data-rs/cache/quic_listen_port.txt` == `41641` **且** `grep -c '被占用' <日志>` == 0（退让时的原文 = `⚠️ QUIC 监听端口 41641 被占用 —— 改用 <实际>；token 里的端口以公布/签发为准`，`engine.rs:616-621`） |
| V2 | QUIC 面就绪 | `出口 QUIC 面就绪（单承载；migration=true，initial_mtu=1400）` + `端点就绪（…:41641，migration=true，initial_mtu=1400，datagram 缓冲 1048576B）` |
| V3 | **身份连续（强判据，机械 diff）** | `diff <(grep -o 'key=[0-9a-f]*' /tmp/m7-gates/aliyun-pre.txt) <(grep -o 'key=[0-9a-f]*' <新日志>)` **无输出** 且 E19 行 `后端身份：标签 … ｜公钥 …` 前缀逐字同——不同 = state 读错/身份轮换 ⇒ **停** |
| V4 | relay 角色 | `中继就绪：[::]:41741（token 模式（中继 ID <与升级前同 ID>）；…）`（**L-9 已登记**：通配形态 `[::]:`）+ `中继控制面：TCP 0.0.0.0:41741 就绪` |
| V5 | 端口形态 | `ss -lunp` 三行同 pid：`udp 41641` / `udp 41741` / `tcp 41741` |
| V6 | 公网端点 | `公网端点：已公布 [123.56.218.212:41641]`（v6 无 = 该机无 v6，预期）；`cache/public_endpoint.txt` 内容与端口一致 |
| V7 | 新 token 铸出 | `客户端 token（粘进 App 的「添加主机」即可；2 个端点）：hmw2…`（`serve token --state /opt/homeway/data-rs` 复核同串；端点含 QUIC 类） |
| V8 | **中继互注零人工** | 阿里云 `中继：后端 d07c57dd5bde1fa7 注册成功（腿 …）`（**标签 = Mac 出口身份派生 ⇒ 应与升级前逐字同**）；Mac 出口 `中继：注册成功（腿 41641 → 123.56.218.212:41741）` |
| V9 | 设备侧无感 | 设备仍在用 Mac（G1）出口：Mac `debug.log` 持续 `peer: ~ dev=aca645d3 refresh`；设备核无新断线行（本阶段**不应**触碰设备） |

**回滚（任一条不过）**

```bash
pkill -x homeway-rs; until ! pgrep -x homeway-rs >/dev/null; do sleep 1; done
cp /opt/homeway/data-rs/config.toml.bak-pre-m7 /opt/homeway/data-rs/config.toml   # ← 必做（§4.0-D）
cp /usr/local/bin/homeway-rs.bak-v0.2.3 /usr/local/bin/homeway-rs
cd /opt/homeway && HOME=/opt/homeway/served nohup /usr/local/bin/homeway-rs \
  --state /opt/homeway/data-rs >> /opt/homeway/data-rs/cache/unified-stdout.log 2>&1 &
# 验证：V1' `serve 就绪：wg=:41641（…）…key=<同串>` + `中继就绪：…:41741（…）` + Mac 侧 `中继：注册成功`
#       设备侧仍走 Mac（G1）——本阶段回滚对设备**零影响**（这是「兜底先升」的直接收益）
```

### 4.2 阶段二：设备核 G1 → G2（+ 贴阿里云新 token）+ **此时**补 T11 与 S7①

**前置检查（不变式 I）**：阿里云已是 G2 且 V1–V9 全绿；Mac 仍是 G1（设备当前主力，故本阶段开始时设备**必须**
仍在 Mac 上，切换后转 Aliyun）。

**出包（G2 核）**

```bash
# 正式路径（tier 触点；需先重生成 log-index，§7.3-⑤）
cd ~/Documents/projects/tier && HOMEWAY_RS=/Users/zhaozhe/Documents/projects/homeway-rs tools/tailcat/build-core.sh
#   注：pin 前进后 HEAD==pin 语义满足；pin 未前进时用其「祖先语义」放行
. tools/ohos-env.sh
"$HVIGORW" --mode module -p module=tailcat@default  -p product=default assembleHsp  --no-daemon
"$HVIGORW" --mode module -p module=terminal@default -p product=default assembleHsp  --no-daemon
"$HVIGORW" --mode module -p module=entry@default    -p product=default assembleHap  --no-daemon
# 逃生口（正式路径被 log-index 门拦时，照 M5/M6 先例；必须留痕）
cp -f <核仓>/target/aarch64-unknown-linux-ohos/release/libclientcore.so \
      ~/Documents/projects/tier/tailcat/libs/arm64-v8a/ && \
cp -f <同上> ~/Documents/projects/tier/tailcat/src/main/cpp/prebuilt/arm64-v8a/
wc -c + md5sum 两处一致（期望 2,9xx,xxx B，与 build-app-core.sh `[size]` 同值）
```

**安装与注入**

```bash
$HDC install -r entry/…/entry-default-signed.hap tailcat/…/tailcat-default-signed.hsp terminal/…/terminal-default-signed.hsp
#   前置（设计门 r30 中-12）：装机前记录**主机卡条数**与两枚 token 的出处（§4.0-C-4 + §4.1）
#   失败分支：release 签名包不能覆盖装在 debug 包设备上（bm 报 9568336）⇒ 需先卸载 ⇒ **App 内配置与 token 被清空**
#              ⇒ 按 §7.3-7 重建两个主机条目（两枚 token 已在 §4.0-C-4/§4.1 抄存）→ F13
$HDC shell "aa force-stop me.zhaozhe.tier"          # 必须先强停（HostStore 缓存旧 token，DEPLOY §6-2）
$HDC shell "power-shell wakeup"
# 取新 token（**单行提取 + fail-closed**；serve token 输出三行中文，直接 $() 会带回中文与端点行）
set +o history
TOK=$(ssh root@123.56.218.212 '/usr/local/bin/homeway-rs serve token --state /opt/homeway/data-rs' | sed -n 's/^serve token：//p')
case "$TOK" in hmw2*) ;; *) echo "token 抓取失败（拿到：${TOK:0:8}…）——停"; exit 1;; esac
$HDC shell "aa start -a EntryAbility -b me.zhaozhe.tier --ps host_token '$TOK'"
# 首次启动/换签名后系统会弹「是否允许使用 VPN？」⇒ 需在屏上点「允许」（uitest uiInput click，坐标 dumpLayout 取）
# 打开「全局代理」开关（uitest dumpLayout 取坐标，DEVICE-TEST-OHOS.md §4）
```

**验证点（核侧日志 = 设备沙箱 `tailcat-tun.log`；出口侧 = 阿里云 `debug.log`）**

| # | 判据 | 期望 |
|---|---|---|
| V10 | 代际切换标志 | 核：`传输：新栈（QUIC 岛）`（**不再**出现 `wg-native-stack`） |
| V11 | 岛建连 | `岛已建连（候选 N 个，胜出 直连 123.56.218.212:41641，耗时 …ms）—— L3 承载 = 岛` |
| V12 | 准入与保活 | `warmup pong: 就绪（判据=quic）` → `attached（数据面已接管 fd=…，L3 直通）` → `link: via=direct ep=123.56.218.212:41641 rtt=…ms` |
| V13 | 出口侧采纳 | 阿里云 `连接采纳 dev=aca645d3 tun=100.64.x.x ← …` + `peer: + dev=aca645d3 pub=… ip=100.64.x.x n=1/32` |
| V13b | **装机面（设计门 r30 中-12 补）** | VPN 授权弹窗已点「允许」（`uitest dumpLayout` 判据）；主机卡两条目在位；App 侧 host 文本 = `quic:123.56.218.212`（additive 端点数形态，功能无影响） |
| V14 | **层 0 全通** | 设备浏览器经隧道渲染页面（T1 口径）；出口 `intercept: tcp transit …（dialok）` 成串 + `dns: q=… resp=…` |
| V15 | 服务面四件（**前置 = §4.1 的样本与目标**） | ①files：列表含 `m7-probe.bin` 且**下载件 sha256 = 样本 sha256**（§4.1 已抄）；②term：会话 attach + 键输入落纸（`touch /tmp/m7-term-marker` 形态）；③speedtest 一轮；④portfwd 抽查 1 形态（**目标点名**：阿里云本机 `127.0.0.1:<临时 nc -l 端口>`；判据 = 出口端命中 + 载荷一致） |
| V16 | 断线恢复 | 阿里云 `pkill -9` + 立启 ⇒ 核 `链路快探失败（…）` → `链路重连完成（原因=…，耗时 …ms）`，**T_recv ≤ 3.5s** |
| V17 | 收工链 | 关 VPN 开关 ⇒ 核 `岛收工` + 出口 `出口收线（连接数 1 → 0）`（E25） |
| V18 | 零 panic | `hilog` + `tailcat-tun.log` + faultlog 三面零命中（M5 先例） |
| V19 | **T11 替代注入（本期做，见 §10；设计门 r30 中-17 点名器具）** | ①**本机可自动档**（S2a）：跑/扩展 `crates/homeway-quic/src/exit/tests.rs:293-330` 的 `rebind_keeps_connection_and_surfaces_path_change`（判据 = 服务端 `path_changes ≥ 1` **且** `connections == 1` 且连接未被关）；②**真机档**（S2b，U2 窗口）：`tools/local-rust-relay.sh` 起本地中继做腿 → 经中继腿建连后 **`pkill` 打死中继腿** ⇒ 岛落直连腿（判据 = `路径变更 dev=… ` 行 + `migrations` 增 / 设备表条数不变 / 无世代重建；器具 = 现有 relay 脚本 + `pkill`，**无需新写脚本**） |
| V20 | **S7① DNS 回复 e2e（本期做）** | 直接对隧道 DNS（`dnsAddresses` = 出口 IP :53/:5300）发查询并经隧道回包（本机轮 = S2a；真机轮 = S2b；`dns: q=N resp=M` 计数对账） |

**回滚（设备侧）**

```bash
# ① 装回 G1 HSP（旧 .so 两处手拷 + 重建 HSP + 覆盖装，见 §4.0-C-5/6 的归档件）
# ② 重新注入旧 aliyun hmw1 token（§4.0-C-4 存档）——G1 核 + G1 出口 + hmw1 三者同代
$HDC shell "aa force-stop me.zhaozhe.tier" && $HDC shell "power-shell wakeup"
$HDC shell "aa start -a EntryAbility -b me.zhaozhe.tier --ps host_token '<旧 hmw1 token>'"
# ③ 设备主机卡切回 Mac（G1 出口，本阶段 Mac 从未动过 ⇒ 也可直接用 Mac 旧 token 兜底）
# 期望：核日志回到 `传输：新栈（wg-native-stack）` + `warmup pong: 就绪（判据=wg）` + attached
```

> **本阶段回滚的收益点**：Mac（主力）**全程是 G1 且未动** ⇒ 设备退回 G1 后**立刻**有可用出口，
> 「设备全断」窗口 = 0。这是把兜底出口排在最前面换来的（§3.3 候选③）。

### 4.3 阶段三：Mac 出口 → G2（launchd）

**前置检查（不变式 I）**：设备核已是 G2 且在用阿里云 G2（V10–V18 全绿）⇒ 「升最后一台出口」的条件满足。
**若该条件不成立（核仍 G1）⇒ 禁止本阶段**（否则设备零可用出口）。

**切换前记录**

```bash
/Users/zhaozhe/bin/homeway-rs --version                     # 期望 v0.2.3
shasum -a 256 /Users/zhaozhe/bin/homeway-rs                 # 期望 98f337e5…（本设计 §2.1 已录）
grep -a "后端身份\|serve 就绪" ~/.config/homeway-rs/cache/exit.log | tail -2 >> /tmp/m7-gates/mac-pre.txt   # d07c57dd5bde1fa7 / 8aee740b7220
/Users/zhaozhe/bin/homeway-rs serve token --state ~/.config/homeway-rs | sed -n 's/^serve token：//p'      # 旧 hmw1…（4 端点）
cp /Users/zhaozhe/bin/homeway-rs /Users/zhaozhe/bin/homeway-rs.bak-v0.2.3
cp ~/.config/homeway-rs/config.toml ~/.config/homeway-rs/config.toml.bak-pre-m7
cp ~/Library/LaunchAgents/me.zhaozhe.homeway-exit.plist ~/Library/LaunchAgents/me.zhaozhe.homeway-exit.plist.bak-pre-m7
```

**换装（**照三次先例**：临时件 + `mv` 原子换名 + `launchctl kickstart -k`；设计门 r30 中-8 纠正 v1 的 `bootout`/`bootstrap` 形态）**

```bash
# 加 config 行 quic_listen = 41641（§4.0-B）
# 取 Release darwin-arm64 产物（换名前不要停进程；mv 原子换名不撞运行中二进制的写锁）
curl -LO https://github.com/zhaoyswd/homeway-rs/releases/download/v0.3.0/homeway-cli-v0.3.0-darwin-arm64.tar.gz
tar xzf homeway-cli-v0.3.0-darwin-arm64.tar.gz && shasum -a 256 -c SHA256SUMS
install -m 0755 homeway-cli ~/bin/homeway-rs.new && ~/bin/homeway-rs.new --version   # 期望 v0.3.0
mv -f ~/bin/homeway-rs.new ~/bin/homeway-rs
launchctl kickstart -k gui/$(id -u)/me.zhaozhe.homeway-exit
launchctl list | grep homeway-exit                           # 期望 pid 非 0
```

> **备选（v1 形态，保留但注明后果）**：`launchctl bootout` → 换件 → `launchctl bootstrap`。因为 plist 是
> **`KeepAlive=true`**，`bootstrap` 失败时 launchd 会**反复拉起失败进程（crash-loop）**，而非「进程未起」；
> 若走该形态，必须补「`bootstrap` 失败 ⇒ 出口处于**无守护**态」的后果句与止血步（F2）。

**验证点（同 V1–V9 的 Mac 版；新增两条）**

| # | 判据 | 期望 |
|---|---|---|
| V21 | V1/V2/V3/V7 的 Mac 版 | `serve 就绪：quic=:41641（…）…tokens=28 key=8aee740b7220…`（**`tokens` 首次启动与升级前同值**，见下注）；`端点就绪（…:41641，…）`；`后端身份：标签 d07c57dd5bde1fa7 ｜公钥 8aee740b7220…`（**逐字不变**，机械 diff 同 V3）；新 token `hmw2…`（含内网 v4 + 公网 v4/v6 + 中继端点） |
| V22 | V4/V6 的 Mac 版 | `绑卡：自动挑到 en0（index=…）` + `绑卡看护：… 钉在 en0`（**L-13 措辞**：不再写「WG socket」）；v4/v6 `公网端点：已公布 […]`（UPnP 仍可能 fail-closed，属既有形态） |
| V23 | 设备切回主力 + 全通 | 贴 Mac 新 `hmw2` token ⇒ V10–V18 在 Mac 上复跑全绿（层 0 / files / term / speedtest / portfwd 抽查 / 断线恢复 / 收工链） |
| V24 | 观察期 | ≥30 min：`grep -cE "ERROR\|panic\|FATAL"` = 0；`peer: ~ … refresh` 稳定；中继腿 `注册腿 1` |

> **注（`tokens` 计数，设计门 r30 低-20 订正）**：E1 的 `tokens=` 取装配时刻的未吊销行数（`engine.rs:976`），
> 而新 token 的 append 发生在装配**之后** ⇒ **首次以 G2 启动仍打印 28**（与升级前同值）；**下次重启起 +1**。
> 台账 append-only 且 `serve token` 读**末行** ⇒ 升级后读到的必是新 `hmw2`；**旧 `hmw1` 只能在升级前抄存**
> （§4.0-C-4）——这是「回滚必须提前留 token」的技术原因。

**回滚（按「回退手段序」从小到大；设计门 r30 中-6 补）**

```bash
# ① 首选：设备切到**另一台 G2 出口**（阿里云）——一次粘贴，零出口动作、零核回退
#    判据 = V10–V18 在阿里云复跑全绿（U2 已给验证过的 hmw2 token，在手）
# ② 其次：**单台出口回退 + 同批核回退**（Mac 换回 v0.2.3 且设备装回 G1 核 + 贴 Mac 旧 hmw1 token）
launchctl kickstart -k gui/$(id -u)/me.zhaozhe.homeway-exit   # 先停旧件前无需停（mv 形态）：直接换件
cp ~/bin/homeway-rs.bak-v0.2.3 ~/bin/homeway-rs
cp ~/.config/homeway-rs/config.toml.bak-pre-m7 ~/.config/homeway-rs/config.toml   # 删 quic_listen（§4.0-D）
launchctl kickstart -k gui/$(id -u)/me.zhaozhe.homeway-exit
# 期望：`serve 就绪：wg=:41641（…）…key=<同串>` + 中继注册 + v6 公布；同时装回 G1 核 + 贴旧 Mac token
# ③ 最后：双台出口回退 + 核回退（§5 最坏情况②）
```

### 4.4 收尾（阶段三之后）

1. **校验三件同源**：两台出口 `--version` = `v0.3.0`；两台 `.so` = 同一 build（md5 在册）；tier pin =
   实际部署 commit（§7.1 自查命令）；
2. **观察期 24h**：出口 `debug.log` 的 `quic:` 族（迁移/丢弃/准入）与 `peer: ±/~` 形态、设备侧
   `link:`/`链路重连` 行；异常按 §5 分档；
3. **清理**（可选，用户决定）：`~/bin/homeway-rs.bak-v0.2.0/1/2`（历史件）可留；**`.bak-v0.2.3` 与
   `config.toml.bak-pre-m7` 必须留到观察期结束**；
4. **文档收束落地**（§8 清单 + 本文件标记收官 + `docs/reviews/M7.md`）。

---

## 5. 失败预案（逐条：**判定信号 + 动作**）与最坏情况

| # | 场景 | 判定信号 | 动作 |
|---|---|---|---|
| **F1** | 上传/换件中断（scp 断、tar 校验不过） | `sha256sum -c` 非 0 / `--version` 非 v0.3.0 | **旧进程未停则不动**（换件在停机后做）；重传或换通道；校验不过 = 取件面问题，**不得**先停进程 |
| **F2** | 新核起不来（fail-fast） | **launchd 形态（KeepAlive=true）**：`exit.log` **重复同名报错**（约 10s 一轮）+ `launchctl list \| grep homeway-exit` 第二列**非 0**（= crash-loop，**不是**「进程未起」）；nohup 形态：进程不在 + stderr 点名（`serve.quic_listen 非法` / `未知键 xxx` / 权限 / state 锁定） | **首步止血**：`launchctl bootout gui/$(id -u)/me.zhaozhe.homeway-exit`（停 crash-loop）→ 按报错修 config（**先比对 `config.toml.bak-pre-m7`**）→ 再启动；修不动 ⇒ **回滚三件套**（二进制 + config + 启动） |
| **F3** | **端口退让**（41641 被占） | 判定信号原文 = `⚠️ QUIC 监听端口 41641 被占用 —— 改用 <实际>；token 里的端口以公布/签发为准`（`engine.rs:616-621`）；**辅证** = `cache/quic_listen_port.txt` ≠ 41641（该文件写**实际**端口；`cache/listen_port.txt` 同值）。**注意 E1 打的是配置值，不能用来判退让**（r30 高-2） | `lsof -nP -iUDP:41641` 找占用者（旧进程没退净？）→ 杀净 → 重启；**不得带退让端口上线**——真错位面 = **入站防火墙/NAT 映射（41641）与实际监听口不一致 ⇒ 已公布公网端点不可达**（token 端点用的是实际口，`engine.rs:2157-2163`） |
| **F4** | 隧道不通（设备连不上新出口） | 核 `岛已建连` 缺席 / 出口无 `连接采纳`；卡在准入前后 | 分档排查：①token 端点端口是否 = 41641；②公网可达性（`nc -zu` / STUN 公布行）；③中继腿（重新铸 token 取带中继端点者兜底）；④设备该主机条目是否为**新** token。四档皆过仍不通 ⇒ 回滚 |
| **F5** | **token 失效**（持旧串/串贴错） | 核侧报 `token 非法（hmw1…）：版本不符` / `UnsupportedVersion`；出口零 `连接采纳` | `serve token` 取新 `hmw2` → **`aa force-stop` 后重注入**（HostStore 缓存旧值，必须强停） |
| **F6** | 新核起来但数据面异常（panic/黑洞） | 设备核 `panic`/`abort`/`SEGV` 行；faultlog 命中；`attached` 后无流量 | **先回滚设备核**（装回 G1 核 + 贴 G1 出口旧 token）——**这是唯一不需要动出口的回滚**（Mac 仍 G1 时）；同时取三面日志（hilog/tun.log/faultlog）留证 |
| **F7** | **回滚后仍不通** | 旧核 + 旧出口 + 旧 token 三者同代仍失败 | 逐项复核「回滚三件套」清单：①二进制 sha 与 `bak` 逐字节同；②config 已还原（无 `quic_listen`）**且旧二进制能起来**；③token 是 `hmw1` 且为该出口所铸 **+ 旧二进制能解析现有台账**（新代追加行含 `rpk`/`kind:2`——已验可读，§4.0-E 的可复跑校验）；④端口 41641；⑤设备 `force-stop` 后重注入；⑥plist 指向未变。仍不通 ⇒ **停用该主机**（设备切另一台同代出口），保留现场（不删 state/日志）后上报 |
| **F8** | 中继失联 | 阿里云 `注册腿 0`（`中继统计：注册腿 0`）；Mac `中继控制面` 退避重连行 | 核对 `rl1` 凭据与 `relay.key` 未变（**state tar 第 7 件在位**，§4.0-C）；`launchctl kickstart -k` 触发 Mac 侧重连；阿里云侧 `pkill` 温重启弱化态 |
| **F9** | 半代态误配（设备切到异代出口） | 切主机后立刻失败（token 版本拒 / 无候选） | **设计内行为**（§3.2 矩阵）：切回同代出口；复核 App 主机卡两枚 token 的代际（G2 核只用 `hmw2`） |
| **F10** | 生产性能不达预期 | 设备侧 TUN 吞吐低于登记值（0.893×）或用户卡顿 | 已登记差异（`PERF-AB` §9.20.11）；**不在本批修**（候选批见 `M6.md` §12.8.8）；紧急程度高 ⇒ 按回退手段序（§3.3 推论-3）处置 |
| **F11** | 切换期用户/设备不在场 | 无法完成核升级与 token 粘贴 | **停在「兜底已升、主力未升」的半代态**：设备仍走主力 G1，**生产可用**（这是允许的停留态）；核升级与主力升级**必须**在用户在场窗口内连续完成 |
| **F12** | 公网端点不可得（Mac UPnP 已知被本地网络隐私拒） | `公网端点：暂不公布 —— …` 行 | 生产既有形态（v6 + 中继兜底）；确认新 token **含中继端点**（兜底重试铸出）后照常进行 |
| **F13** | **装机面失败**（设计门 r30 中-12 补：签名不匹配须卸载） | `bm` 报 **9568336**（release 包不能覆盖装 debug 包）⇒ 需先卸载 ⇒ **App 内配置与 token 被清空**；或 VPN 授权弹窗未点「允许」⇒ 隧道起不来 | 卸载后重装 ⇒ 按 §7.3-7 **重建两个主机条目**（两枚 token 已在 §4.0-C-4/§4.1 抄存）+ 重新开「全局代理」+ 点授权：判据 = V13b |

### 最坏情况：两台都换完、且设备核也已换（发现致命问题）

**先说结论**：此时**没有「只回退一处」的解法**——不变式 I 决定「回退」的最小单元是
**(设备核 + 至少一台出口 + 至少一枚同代 token)** 三者同代。按**回退手段序**（§3.3 推论-3）从小到大：

| 选项 | 步骤 | 代价 | 评价 |
|---|---|---|---|
| **① 设备切另一台 G2 出口（最先试，最小动作）** | 设备 App 主机卡切到阿里云（`hmw2` 在手）→ 复跑 V10–V18 | **零出口动作、零核回退**，仅一次粘贴 | **首选**：绝大多数「U3 引入的新代问题」在这一步就恢复（r30 中-6 指出 v1 稿跳过了这一手） |
| **② 单台回退 + 核回退** | ①设备装回 G1 核（+ G1 HSP）；②**只把 Mac 换回 v0.2.3**（+ 还原 config + 启动 + 设备贴 Mac 旧 `hmw1`）；③阿里云保持 G2（设备不用它） | 窗口内设备短暂不可用（换核 + 换 token ≈ 5–10 min）；阿里云处「无人使用的新代」态 | 次选：动作面小（只动一台出口 + 核），回到「Mac G1 × 设备 G1」的**已验证生产态** |
| ③ 双台回退 + 核回退 | ①②同上 + ③阿里云也换回 v0.2.3 + 贴旧 token | 两台都在窗口内停摆，动作面最大；阿里云需 ssh 窗口 | 仅在①仍不通（Mac 侧另有故障）时使用 |
| ④ 不回退，转修复批 | 保持 G2，按 `M6.md` §12.8.8 候选（自定义 CC / 出口整形 / BBR 类）开新批 | 需新设计门 + 真机复测周期；期间带着已知差异运行 | 用于「非致命差异」；**致命**（数据面不可用/安全问题）**不适用** |

**终局性说明（必须写清楚）**：M5 已把 WG 面整件删除（净删 ≈ −13,269 行）⇒ **不存在「产品内一键回退」**；
`revert M5` 是巨型回退（且与 M6/M6.7 的全部读数面冲突）⇒ **选项①的「回滚件」是本项目唯一的可控回退资产**，
这也是 §4.0-C 把「回滚件持久化 + 校验」列为硬前置的原因。

---

## 6. 上线前的最后核验清单（**切换前必须全绿**；由 M7 实施棒跑并留证）

| # | 项 | 命令 | 期望（本次基线读数见括号） |
|---|---|---|---|
| 1 | 工作树干净 + 发版 commit 钉定 | `git status --porcelain`；`git log --oneline -1` | 空；= 发版 commit X（与 tag / pin 一致） |
| 2 | **`tools/ci-local.sh` 八步** | `tools/ci-local.sh` | 八步全绿（①基线门 ②`cargo test` ③clippy+OHOS check/真 link+隔离门 ④向量门 ④.5 fuzz 种子门 ⑤词表门+WG 残留门 ⑥release 构建 ⑦矩阵冒烟 RRR）〔M6.7 读数 2026-10-10 13:48:15 全绿〕。**备注（r30 低-21）**：第 7 步依赖 `bin/homeway-go`（gitignore 面）——干净 clone 先 `tools/local-exit.sh start 1` 触发构建，否则第 7 步红 |
| 3 | `cargo test --workspace` | 直跑 | 677 passed / 0 failed / 4 ignored + 各集成 target ok（`homeway-quic` lib = 194 passed）〔M6.7 §12.8.7〕 |
| 4 | clippy | `cargo clippy --all-targets -- -D warnings` | 0 告警 |
| 5 | 三目标 `cargo check` | host + `aarch64-unknown-linux-ohos`（真 sysroot）+ musl 双架构（CI 配方：`CC=clang` + `-nostdlibinc` + `tools/cc-check-shim`） | 0 error（两处**预存**警告在册：`go_fmt.rs` 的 `libc::time_t` deprecated、`homeway-capi` musl 上 cdylib 不支持） |
| 6 | 隔离门 | `tools/check-quic-isolation.sh` | 十一条全绿 |
| 7 | **WG 残留门** | `tools/check-wg-removed.sh` | 十条全绿（含门自校准 4 正 2 反；含 `hmw1`/`ring-shim`/`_wg` 禁复活哨兵） |
| 8 | 词表门 | `tools/check-vocab.sh` | PASS（5 单元 / 26 值；ledger sha256 一致） |
| 9 | 核三道门 | `tools/build-app-core.sh` | `[sym] 20/20` / `[ver] <HEAD>-rust` / `[size]` ≤3.8MB（记实测值，期望 ≈2.96–2.97 MB = 0.78×） |
| 10 | **e2e 四件套**（各先 `wipe` 实例） | `tools/quic-island-e2e.sh` / `quic-ladder-e2e.sh` / `quic-pf-e2e.sh` / `quic-wg-e2e.sh` | rc=0 ×4 |
| 11 | 指针一致（`PERF-AB` ↔ `QUIC-BASELINE` ↔ 门槛表） | 人工比对：体积 `.so`（同档 LTO）、每包 CPU 绝对列、内存五格 + M6.7 的 +3 MiB 显式缓冲；**门槛数值三处一致**（单连接 ≤+640K / 每设备 ≤96K / 32 设备 ≤+3.1MiB） | 同值同档（**跨档不得互引**，L-1）；`QUIC-BASELINE.md` 的「现役手机形态 = wg-shim」「M1/M6 门槛 256K/64K/+2MB」两处**必须**按 §8-17 更新（否则此项**会假绿**，r30 中-9） |
| 12 | **M6 顺延项处置状态** | `grep -n "本期做\|转交\|登记不做" docs/reviews/M7-design.md`（§10）+ 完成项的读数 | **11 条**各有显式去向：本期做 4 条已完成（T11 / S7① / S7③ / T9+内存账），其余按 §10 表转交或登记 |
| 13 | 回滚件在位（**七件**） | §4.0-C 七件逐项 `shasum` / `diff` / `tar tzf` | 七件齐 + 校验过 + §4.0-E 的台账跨代可读校验 rc=0（缺一不做切换） |
| 14 | 生产面侦察刷新 | §2 的只读侦察**重跑一遍**（launchd pid / 版本 / sha / state 形态） | 与 §2 记录一致（若已被改动 ⇒ 先查明再切换） |
| 15 | 凭证卫生 | `grep -c 'hmw1\|hmw2' ~/.zsh_history`（切换后）；评审/记录原文 token 掩码 | 0 命中 / 只有前缀 + 尾 4（§4.0-F） |

---

## 7. tier 交付物清单（三类；**全部为用户触点执行**）

### 7.1 ① pin 前进（`tier/tools/tailcat/homeway-rs.pin`）

| 项 | 值 |
|---|---|
| **新 SHA 取值** | **= C1 发版 commit**（§4.0-A：`S1–S3` + `Cargo.toml`/`Cargo.lock` 版本 bump，过 S4 门后推 main 并打 tag v0.3.0）。**不是**当前 HEAD `cc4916c`——实施批会追加提交，收口后以 `git rev-parse HEAD` 为准。**S5 的文档-only 后代 commit 不改变产物 ⇒ pin 不自然后移**（r30 中-13）。 |
| 现值 | `cbd45f0439e660fd75ee83d5953c745959efba82`（G1 核 = v0.2.2 树） |
| 理由（三条） | ①**同源纪律**：代际矩阵要求核与出口同代，**同代的最强形态是同一 commit 构建**——pin 指向构建源即可用 `build-core.sh` 复现设备上跑的核；②**可核验**：pin 与 tag（v0.3.0）指向同一 commit ⇒ Release 产物的 SHA256SUMS 与设备核字节可双向对账；③**防漂移**：pin 一旦落后于部署 commit，下一次 `build-core.sh` 会造出「比线上一代新」的核（祖先语义只保证 ≥，"更新"不保证"相同"）⇒ 出包面引入未验证差异。 |
| 执行（用户） | `echo <X> > tier/tools/tailcat/homeway-rs.pin && git -C tier add -A && git -C tier commit -m "pin: homeway-rs 前进到 <X>（M7 QUIC 单承载）"` |
| 自查（用户） | `cd ~/Documents/projects/tier && git rev-parse HEAD --abbrev=8 && cat tools/tailcat/homeway-rs.pin` + 部署期 `git -C <核仓> rev-parse HEAD` ⇒ **三者同值** |
| 纪律 | 本仓**不改** tier 跟踪文件（M7 只出本条清单）；pin 前进 = 用户触点（AGENTS 速查 / QUIC-ROADMAP 用户触点清单） |

### 7.2 ② `connection-lifecycle` 定稿交付（tier `docs/agents/connection-lifecycle.md`）

| 项 | 内容 |
|---|---|
| **终稿位置** | ①**主体** = `docs/reviews/M3.md` **附录 A**（§3 整节替换稿 + §9 常量追加 + §10 速查追加 + §11 待办两条）；②**M3 收口时的两处实质订正** = `docs/reviews/M3.md` §7（流面行 `recv 4 MiB（每流）+ conn_recv 8 MiB（连接级聚合上界）`；env 增列 `HOMEWAY_QUIC_RECV_WINDOW`）；③**M4 订正** = `docs/reviews/M4.md` §5-9（C11 触发集**世代限定**：`l3_on_island()==true` 的世代零 C11 族行，改产 `quic: 恢复下推`；回落世代仍走原路）。 |
| **交付形态** | **整节替换 §3**（WG 档阶梯 → QUIC 档：快探 → 复探 → M/R 阶梯 → B 门）+ **§9 常量表**追加（流面/快探参数 + env 清单）+ **§10/§11** 追加（速查与待办）。 |
| **摘要（QUIC 档连接策略）** | ①**快探**（缺省 700ms 预算）判活；②失败 ⇒ **复探**（×2）；③仍失败 ⇒ **M（迁移未确认 ⇒ Rebind）优先**，Rebind 成功即保连接（不拆世代）；④Rebind 不可行/失败 ⇒ **R（重连/重赛跑）**；⑤**B 门**（连续 2 次 R 失败 **且** 窗 ≥10s）才上报生命周期层（世代重建/不健康）；⑥**T_recv ≤ 3.5s**，起点 = 出口 E1 `serve 就绪` 行；⑦**在用档** 250ms 拍 / **待机档** 60s 拍——**待机档不承诺 3.5s**（M3 真机 32.06s 长尾，口径已文档化）。 |
| **为什么必须交付** | 该文件是「连接行为常量」真源（AGENTS 速查表引用 `tier:docs/agents/connection-lifecycle.md`）；M3 已把**恢复阶梯整体重写**（QUIC 档不再有 R1/R2/R3 语义）⇒ 不定稿则 tier 文档与实现**永久不一致**，后续 App 侧开发会按旧阶梯写逻辑。 |
| **不做的后果** | tier 文档失真 + 后续会话按旧阶梯排障（`RECOVER` 族行在 QUIC 档已不存在，读到必是回落世代） |

### 7.3 ③ App 侧适配清单（逐条：**改哪里 / 为什么 / 不做的后果**）

| # | 项 | 改哪里 | 为什么 | 不做的后果 |
|---|---|---|---|---|
| 1 | **`TokenCheck` 前缀 `hmw1` → `hmw2`**（**P0，必须**） | `tier/entry/src/main/ets/model/TokenCheck.ets:18`（`HMW_PREFIX='hmw1'`）+ 其三处报错文案（含「请确认这是 homeway token」分支） | M5 S5t 起 token 载体 = `hmw2` 段容器（L-12 登记）；出口铸出的串**只有** `hmw2` | **用户自助面失效**（App UI 手工粘贴被本地语法校验拒）⇒ 只能靠 `--ps host_token` 注入（**M7 切换本身可经该路径完成，故 U1–U3 不以本条为前置**；r30 低-22 订正 v1 的「切换卡死」措辞）——长期不修则每次换 token 都要走设备调试通道 |
| 2 | **`tunConfig.transport` 停发**（P2，建议） | tier 侧 `tunConfig` 组装点（M1 登记的「配置新键六个」之一） | M5 **L-7** 已删该键（`HOMEWAY_TRANSPORT` / `serve.quic` / `tunConfig.transport` 三键 = 死旋钮） | **核侧非 deny ⇒ 不报错**（`docs/reviews/M5.md` §7-4）⇒ 仅冗余，无功能影响；但键面残留会误导后续会话认为可选承载 |
| 3 | **`exit-transit-intercept` 的 MUST 退役 + 健康判据换源**（P1，**须用户点头**） | tier `openspec/specs/exit-transit-intercept/`（回环同端口豁免的 MUST 条） | M5 删豁免面；QUIC 档服务流走 `STREAM[tag]`（files/term/speedtest/dial/probe），不再经拦截层回环豁免路径 | spec 与实现不一致：开门红/误导开发；且该 MUST 原文同时是**恢复阶梯的健康判据来源**（死端口回 RST）——不换源则阶梯健康判据悬空（M5 设计 §12-**C-7** 明列「须用户点头」） |
| 4 | `connection-lifecycle.md` 定稿落库 | 见 §7.2 | 同上 | 同 §7.2 |
| 5 | **`log-index.md` 重生成**（P1，出包门前置） | tier `tools/docs/gen-log-index.sh` 重跑 + 提交 | 公共门「`docs/agents/log-index.md` 不是最新的」会阻断正式出包路径（M1 实测，门在 `cp` 之前 = fail-closed） | `build-core.sh` 正式路径红 ⇒ 只能走**手拷逃生口**出包（M5/M6 两次先例，须留痕）；长期走逃生口 = 校验面缺失 |
| 6 | 主机卡端点显示 `quic:` 前缀（P3，观感） | App host-card 渲染（数据源 = 核 `probe_json.rs::endpoints_of`，`EndpointKind::Quic => format!("quic:{}")`） | additive 的端点数形态（M1 设计 §3.6 登记，App 侧只透传字符串） | 卡片显示 `主机 quic:192.168.3.12`（M6 真机原文）；功能无影响 |
| 7 | **旧主机条目替换**（P0，切换动作） | 设备 App 主机列表：两枚 `hmw1` 条目在核换代后**不可用**（版本拒） | 代际矩阵（§3.2）；`aa force-stop` + 重注入是唯一可靠换 token 路径（HostStore 缓存） | 用户误切到死主机 = 表现为「连不上」（F9 场景） |
| 8 | pin 前进 | 见 §7.1 | 同源纪律 | 出包面可能造出未验证的核 |

---

## 8. 文档收束清单（逐文件逐节：**改什么 / 从 → 到**）

> 规则：**历史记录不动**（`docs/reviews/M*`、`DEPLOY-RUST-EXIT.md` §0–§11 的旧实录保留），
> 新增/更新只落在「现行口径」面与**新节**；`docs/QUIC-ROADMAP.md` 与 `docs/INTEROP-CRITERIA.md`
> 是**主会话/实现棒触点点**（本棒不改，只列清单）。

| # | 文件 · 节 | 从 → 到 |
|---|---|---|
| 1 | `README.md` · 首段 bullet 1「三角色同一二进制」 | 「既是出口（**WG 端点** + 过境拦截…）、也是客户端（**直连/中继腿 + 恢复阶梯**）」→「既是出口（**QUIC 端点**（quinn/rustls RPK）+ 过境拦截 + files/term/DNS 等服务）、也是客户端（**QUIC 岛**：并行赛跑 / rebind 迁移 / 快探阶梯）、也是中继（**信封转发 + 准入，零 QUIC 依赖**）」 |
| 2 | `README.md` · 首段 bullet 3「行为对齐」 | 「wire 字节 / 行为常量 / 判据行与 Go 基线（冻结锚 `d4148f6`）逐一对齐」→ 加限定：「**保留面**（relaywire / term / files / 控制面）与 Go 基线对齐；**WG/token 承载面**已随 M5 换代（`hmw2` + QUIC 单承载，无兼容包袱，登记见 `INTEROP-CRITERIA` L-12/L-1…）」 |
| 3 | `README.md` · 「架构」代码块 | `wtransport/`（自管 UDP Bind：候选镜像/采纳/漫游/腿表）→ 删；`wgcore/`（smoltcp + boringtun）→ 删；改列 `homeway-quic/`（QUIC 岛：连接管理 / 控制流 / 流分发 tag）+ `facade/`（App 桥面，保留）；`│ WG（UDP，直连或经中继）` → `│ QUIC over UDP（一条连接 = 一台设备；直连或经中继，中继零解析）`；出口 `server/ WG 端点 + 动态 peer 表（token 凭证）` → `server/ QUIC 端点（单 UDP 端口，migration=true）+ 设备表（RPK 钉定 + 四帧准入）`；出口服务面补 `QUIC STREAM[tag] → files/term/speedtest/dial/probe` |
| 4 | `README.md` · 「构建」 | 补 product 档口径：「OHOS `.so` 判据 ≤3,800,000 B（**product 档 = LTO + `codegen-units=1` + NDK strip**）；同档判、跨档不互引（L-1）」 |
| 5 | `README.md` · 「文档地图」表 | 加三行：`docs/QUIC-ROADMAP.md`（**传输层换代程序真源（M0–M7，已收官）**）/ `docs/QUIC-BASELINE.md`（三基线：体积/CPU/内存）/ `docs/DEVICE-TEST-OHOS.md`（真机操作手册）；「基线锚定（冻结 `d4148f6`）」→「基线锚定（**冻结 `d4148f6`，只读 oracle**）」 |
| 6 | `README.md` · 「快速开始」 | `tools/local-exit.sh`（Go 基线出口）行加注「**历史 oracle（Go 已退役）**」；主力本地链路改指 `tools/local-rust-exit.sh`（单公共端口 = QUIC 端口） |
| 6b | `README.md:87` · token 行（**r30 中-15 补**） | 「token 是**凭证**…（前缀 **`hmw1`**；中继凭据前缀 `rl1`）」→ 「（客户端 token 前缀 **`hmw2`**〔M5 起段容器；存量 `hmw1` 一律失效〕；中继凭据前缀 `rl1`）」 |
| 6c | `README.md:19-22` · 「架构」`session/` 行（**r30 中-15 补**） | 「`session/` 连接生命周期与恢复阶梯 **R1–R3**」→「**快探 → 复探 → M(migrate/Rebind) → R(重连) → B 门**（QUIC 档；旧 R1–R3 阶梯随 WG 面退役，tier 文档见 `connection-lifecycle` 定稿）」 |
| 7 | `AGENTS.md` · 硬规则 4「对齐三件套」 | 加一句：「**WG/token 承载面的字节对齐义务随 M5 换代终止**（`hmw2` 载体、QUIC 单承载；保留面 = relaywire/term/files/控制面仍在对齐面内；判据政策与登记照旧）」 |
| 8 | `AGENTS.md` · 「技术底座」第 1 条 | 「wireguard-go→**boringtun 0.6**（ring 0.16 无 OHOS 支持 ⇒ `tools/` 内 vendored ring 垫片 `[patch.crates-io]`）」→「**QUIC 承载：quinn 0.11 + rustls 0.23（ring 0.17 provider）**；`boringtun` / `tools/ring-shim` / `[patch.crates-io]` **已退役（M5）**」 |
| 9 | `AGENTS.md` · 「技术底座」smoltcp 条 | 「netstack→**smoltcp 0.14**（features `socket-tcp-cubic` + `reno`）」→ 加角色说明：「**客户端 stackb 退役**；出口 intercept 保留 smoltcp（服务『任意目的地址』全局代理，与承载无关）」 |
| 10 | `AGENTS.md` · 「已实测锚点」 | 「App 侧 `.so` ≈1.9MB（`libclientcore.so`…Go 核 9.7MB 的 0.20×）」→「OHOS `.so` = **2,958,896 B（M5 终值）** / 2,968,016 B（M6.7 修复后），判据 ≤3.8MB ⇒ **0.779×**；出口二进制单独量 8,758,816 B（不设判据）」 |
| 11 | `AGENTS.md` · 「速查」表 | 加三行（同 #5 三文件）；`docs/BASELINE.md` 行补「（**冻结**：锚 `d4148f6`，只读 oracle）」；`基线/向量/词表/矩阵冒烟` 行的门清单补 `check-wg-removed.sh`（WG 残留门）〔注：该门清单在**硬规则 1**（`AGENTS.md:15-16`），不在速查表（r30 中-15 更正节号）〕 |
| 11b | `AGENTS.md:43-44` · **工程原则 4**（**r30 中-15 补**） | 「**行为对齐不放松**：wire 字节/常量/判据行/时间窗仍必须与 Go 逐一对齐」→ 加限定：「**保留面**（relaywire / term / files / 控制面 / 判据行政策）与 Go 对齐；**WG/token 承载面**的对齐义务随 M5 换代终止（登记 = L-1/L-12）」 |
| 11c | `AGENTS.md:22`（硬规则 3 现役出口纪律）与 `:75`（速查「起本地 Go 出口」）（**r30 中-15 补**） | 加限定「Go 出口 = **历史 oracle（Go 已退役）**，仅供对照复现；现役本地链路 = `tools/local-rust-exit.sh`」 |
| 12 | `docs/BASELINE.md` · 顶部冻结声明段后 | 追加一段「**换代后对齐义务范围**」：「Go 基线 = 只读历史参照/oracle；**token 与 WG 承载面**的对齐义务随 QUIC 换代（M5）终止——`fixtures/vectors/token.json`（Go 冻结向量）已退役，代之以本仓自产 `token_hmw2.json`；仍在对齐面内的 = term/files/控制面/relaywire 等保留面 + 判据行政策（登记制）」 |
| 13 | `docs/BASELINE.md` · 「关键输入 sha256」表 | `contracts/ledger.jsonl` 行加注「**仍有效**（词表门引用）」；补一行 `fixtures/vectors/token.json` = **已退役（M5 S5t）**〔注：该表是**汇总行**形态（非 `ledger.jsonl` 逐条表），措辞按实际表形写（r30 中-15 更正）〕 |
| 14 | `CHANGELOG.md` · 新节 `## v0.3.0（2026-10-XX）` | 新增（**破坏性**）：传输层换代 WG → QUIC——①单承载 QUIC（quinn + rustls RPK，`migration=true` + MTU1400/DPLPMTUD）；②**token 换代 `hmw1` → `hmw2` 段容器（存量 token 一律失效；客户端与出口必须同代）**；③RPK + 四帧准入（Hello/Challenge/Proof + 刷新）与设备表语义；④服务流 `STREAM[tag]`（files/term/speedtest/dial/probe）；⑤WG 面全量退役（`wgcore`/`wtransport`/`boringtun`/`ring-shim`/旧恢复阶梯；净删 ≈13,269 行）；⑥构建档位改 LTO + `codegen-units=1`（`.so` 2,958,896 B = 0.779×）；⑦配置新增 `serve.quic_listen`（缺省 = `listen+1`）——**回滚到 v0.2.x 须删除该键**；⑧真机读数（T2 热 0.893×（**未达 0.95×，已登记**）/ T6 1.36× / 断线恢复 2.447s·0.918s / 内存五格过 / 体积 0.779×） |
| 15 | `Cargo.toml` + `Cargo.lock` · `version` | `0.2.3` → `0.3.0`（`Cargo.toml` 1 处 + `Cargo.lock` 4 处 = **C1 发版 commit 的一部分**，不得留到 S5；CLI `--version` 由 `HOMEWAY_CLI_VERSION` 注入但版本面必须一致） |
| 16 | `docs/DEPLOY-RUST-EXIT.md` · 新增 `## 12. 滚动升级 v0.3.0（QUIC 单承载，2026-10-XX）` | 新节（**历史节 §0–§11 不动**）：三阶段实录（阿里云 → 设备核 → Mac）+ 代际矩阵表 + 回滚三件套 + `quic_listen` 配置面 + **旧「中继就绪」样件（`:122`，单栈 `0.0.0.0:41741`）的更新说明**（现行 = `[::]:41741`，**L-9 影响面已点名本文件**；历史正文不动，新节给现行串）〔r30 中-15：该样件在 §2 正文（`:94-151`），无 `### 2.3` 子节，节号按实况写〕 |
| 17 | `docs/QUIC-BASELINE.md` · §1 / §3 / §4（**r30 中-9 扩为三条**） | ①**§1 体积表**追加 M6.5/M6.7 行（`.so` = 2,966,608 / 2,968,016 B，同档 LTO）并标注「M5 终值 2,958,896 B 行保留」；②**§2/§3/§4 的旧参照措辞更新**：`wg-shim` 行「**现役手机形态**」→「**历史对照臂（WG 已退役）**」，「M1 门槛参照：QUIC ≤ 现役 WG+shim ×1.0」→ 标注「**已作废**（WG 臂构成性不可得，M5 起改绝对列）」；③**§4 内存基线**追加 M6.7 的客户端显式缓冲条（`SO_RCVBUF=2 MiB`/`SO_SNDBUF=1 MiB`，设定值 **+3 MiB**，内核口径 ×2），并把「**M1/M6 门槛：单连接 ≤+256K / 每设备 ≤64K / 32 设备 ≤+2MB**」标注为「**已被门槛表取代**：≤+640K / ≤96K / ≤+3.1MiB（用户 2026-10-08、2026-10-10 两次批准）」+ 指针到 `docs/QUIC-ROADMAP.md` 门槛表
| 18 | `docs/DEVICE-TEST-OHOS.md` · **§8 新节「现行口径（M7 起）」+ 就地警示行**（**r30 高-4 改写**） | **v1 稿要直接改 §3/§5 的旧串，与 `INTEROP-CRITERIA.md:587-590`（M5 批）「**历史文档不追改**：`DEVICE-TEST-OHOS.md` 里的旧判据串按历史原样保留」直接冲突** ⇒ 改为：①**新增 §8「现行口径（M7 起）」**：写清新串（`端点就绪（…）` **无 `quic: ` 前缀**、E1 = `quic=:`、**公共端口 = QUIC 端口 = 42651+n**、单承载行集 `传输：新栈（QUIC 岛）`/`岛已建连`/`L3 承载 = 岛`/`判据=quic`/`岛收工`；**并删去已退役串** `transport: 本世代 L3 承载 = quic`——该串已是 e2e 的**禁串**（`crates/homeway-core/tests/quic_wg_e2e.rs:114`/`quic_island_e2e.rs:556`））；②在 **§3/§5 各加一行警示**「⚠️ 本节为 M1/M5 历史实录，现行口径见 §8」；③**例外登记**（把上述两节从「不追改」范围里划出/或声明「历史串保留 + 新增现行节」）= 列入 §9 待补面，由实现棒同批登记 |
| 18b | `docs/DEVICE-TEST-OHOS.md:26`（**r30 中-15 补**） | `HOMEWAY_RS=/Users/zhaozhe/Documents/projects/homeway-rs-quic`（M0–M3 的 worktree）→ **main 主检出路径** `…/homeway-rs`（program 已合回 main；worktree 只作历史） |
| 19 | `docs/QUIC-ROADMAP.md`（**主会话触点**）· M7 行 + 「下一步指针」+ 附录 C/D | M7 状态 → **已完成**（附三阶段实录指针）；「下一步指针」改写为**程序收官** + 遗留登记（「M6.7 剩余 11% 速率控制」= 后续独立批候选，未开工）；附录 C 指针加 `M7-design.md`/`M7.md`；附录 D 风险表补「生产切换可回退面 = 回滚三件套（§4.0-C）」 |
| 20 | `docs/INTEROP-CRITERIA.md`（**实现棒触点**） | 见 §9 待补清单（M6.7 的 socket 缓冲条 + 必要时 T_recv 口径条） |
| 21 | 本文件（`docs/reviews/M7-design.md`）· 末尾 | 收口时补「**收官标记**」：门结论 + 切换实录指针（→`docs/reviews/M7.md` + 新 §12 of DEPLOY）+ 遗留清单 |

---

## 9. 判据登记终检（`docs/INTEROP-CRITERIA.md`）

**方法**：按批逐期扫描登记表（`### 登记表`）+ 计数输入集表（`### 计数输入集 / 数值语义变化`），
与各期记录点名的变更集对账。

| 期 | 应登条目（来源） | 状态 |
|---|---|---|
| **M0** | 词表面零改动（`check-vocab.sh` PASS）+ 基线与附录 A 订正 | ✅ 在册（`QUIC-BASELINE.md` 落地；判据行零变更，无需登记） |
| **M1** | S3/S4 批 **17 条 + 计数输入集 3 行**；S6 代码门补登 **7 + 1 行**；`_wg` 档逐字节回退 | ✅ 在册（含 token 端点数/`link:`/阶梯触发集/N-c 四字段/配置新键六个/实现口径偏离四条） |
| **M2** | 四帧准入面：E8/E9/`quic: 丢弃…未登记`/`准入被拒` 两分档/`flood_refused`+`retry_sent` | ✅ 在册 |
| **M3** | S7 **32 行** + S9 整改 **5 行** + 差异 **1 行** + 数值语义 **3 行** | ✅ 在册（含流面窗 4 MiB / 连接级 8 MiB / `链路重连完成` 耗时口径 / `ReadError` 分类） |
| **M4** | 主表 **17 行** + 追加更正 1 + 数值语义 3（含 NAPI 下推**世代限定** C11） | ✅ 在册 |
| **M5** | **L-1…L-14**（体积档位/E1 `quic=`/E10-E12 值域/E5·E14·E17/行族删除+E23/`quic: ` 前缀批量删/承载三键退役/fixtures 处置/中继双栈 R1/三小修/**token 换代 L-12**/E21 措辞/X1 盲打删）+ S4 四条 + E 棒 H-2/M-7 两条 | ⚠️ **主体在册**，**5 条自陈欠登未落**（见待补 2–6）：C8 值域收窄仅主表就地注记、`SessionSnapshot::stats` 来源换岛、matrix E10/E11 证据换源、`--dead-direct`/`--loopback-only` 语义升承载无关、`daemon::StreamConn` 写停滞 30s→10s（来源 = `M5.md` §8-2/§2 与 C2 棒 §2，**自陈「归 D 棒 S5」但 D 棒未落**） |
| **M6** | 纯测量（`M6` 主节：真机 2×2 + 归因） | ✅ **无需登记**（无产品代码改动；插桩 0 残留） |
| **M6.5**（**订正**） | **不是纯测量批**：①回程批化（泵抽干 ≤16 + 写线程整流 + 包级上限 2048）②热路径小件（上报口提升 / 两处锁→原子）③**TUN 读面阻塞化**（`tun.rs`：`TUN 读面形态：…`/`TUN fd 读阻塞化失败（…）` 两条**永久回运行**）④**写路径语义偏离**（tun 队列满 ⇒ 阻塞在 `write` 内，`tun.rs:271-274` 注释自陈「登记在案」） | ❌ **待补 1 条**（见下） |
| **M6.6** | 纯差分测量（插桩已删、`.so` 回干净字节） | ✅ **无需登记** |
| **M6.7** | 1 条产品改动（`ClientSock::open` 显式 socket 缓冲）+ **内存账 +3 MiB** + **1 条新增观测行**（`岛 socket 缓冲（SO_RCVBUF/SO_SNDBUF）：设定 {want}B，读回 {got}B…`，`relay_sock.rs:303-310`） | ❌ **待补 2 条**（见下） |

**待补清单（7 条；设计门 r30 高-3 把 v1 的「1 条 + 1 条口径」纠正为 7 条）**

> **登记纪律**：全部按 **append-only**（在「判据变更记录」表尾补行）落；**不得回改既有行**；
> 责任期栏如实写「M5/M6.5/M6.7（M7 补登）」。新增行若属新观测行，须同时给 **ID**（照 L-11 对
> `E-q6`/`E25`/`N-e` 的先例）并在 `tools/check-wg-removed.sh` 的**ID 空间门**（⑩）里各恰一条。

| # | 待补条目 | 从 → 到 / 内容 | 原因 | 影响面 |
|---|---|---|---|---|
| 1 | **M6.5 TUN 读面形态（新行 + 语义偏离）** | 无 → 有：`TUN 读面形态：{}（M6.5 逐段成本整改：阻塞读消「read+poll」系统调用对）`（每世代装配一次）+ 失败回退行 `TUN fd 读阻塞化失败（F_GETFL/F_SETFL: …）—— 保持非阻塞读 + poll 形态`；**写路径**：tun 队列满时写线程**阻塞在 `write` 内**（原 `WouldBlock` → `POLLOUT` 有界等待） | M6.5 逐段成本整改（读线程 6.11→2.29 s/轮） | `crates/homeway-quic/src/tun.rs`（`:250-258/:279/:290/:271-274`）；读面/写面排障读者；回退条件 = 不清 `O_NONBLOCK`（单行可逆，M6.5 §12.5-2） |
| 2 | **M5 C8 值域收窄（补登记表条目）** | `warmup pong: 就绪（判据=%s）` 的 `%s`：`{wg,quic}` → **`{quic}`**（`判据=wg` 形态随 WG 面退役） | 现仅主表就地注记（`INTEROP-CRITERIA.md:57` `**M5 收窄**`），违反「变更须入登记表」政策 | C8 行读者（`tools/matrix.sh`/`local-rust-exit.sh` 等）；`facade/tun_exec.rs` |
| 3 | **M5 `SessionSnapshot::stats` 来源换岛**（数值语义） | 同键位，来源 = WG 会话统计 → **岛的 QUIC 服务流字节面** | `M5.md` §5/§8-2 自陈交 D 棒 S5 未落 | `tunStatusJSON` 的 stats 读者；`facade/quic_stream.rs` |
| 4 | **M5 matrix E10/E11 证据换源** | E10/E11 的验收证据从「WG 腿」→「Rust 客户端 `--dial` 走 QUIC dial 腿」 | 同上（`M5.md` §8-2/§2-8） | `tools/matrix.sh` 断言；E10/E11 行读者（L-4 已改行文，本条补证据面） |
| 5 | **M5 `--dead-direct` / `--loopback-only` 语义升承载无关** | 「只改 `Direct` 端点」→「`Direct ∪ Quic`」两族同改（矩阵中继段注入缝） | 只改 `Direct` 在新承载下静默失效（`M5.md` §2-4） | `homeway-cli` 注入缝读者、`tools/matrix.sh`（现全表仅 1 处顺带提及，无登记条目） |
| 6 | **M5 `daemon::StreamConn` 写停滞上界 30s → 10s**（数值语义） | 控制面流腿写停滞窗：30s（WG `TunnelConn`）→ **10s**（`HostStream` 的 `write_with_backoff`） | `M5.md` §2-7 自陈「登记（口径变化）」未落表 | 控制面流腿排障读者；`daemon/**` |
| 7 | **M6.7 socket 缓冲（常量 + 新观测行 + 内存账）** | 无 → 有：`ClientSock::open` 显式 **`SO_RCVBUF=2 MiB` / `SO_SNDBUF=1 MiB`**（`setsockopt` 失败不致命 + 读回实际值）；新增行 `岛 socket 缓冲（SO_RCVBUF（下行接收）\|SO_SNDBUF（上行发送））：设定 {want}B，读回 {got}B…`；**内存账 +3 MiB 设定值**（内核口径 ×2） | 真机根因修复（内核接收缓冲溢出 ⇒ `RcvbufErrors` ⇒ 丢包被当拥塞 ⇒ 包率腰斩；修后 T2 热 11.27→20.14 MB/s） | 内存矩阵/门槛表（+3 MiB）、`crates/homeway-quic/src/client/{relay_sock.rs}`、T9 内存复跑读数、`QUIC-BASELINE.md` §4（§8-17） |

**口径可选条（第 8 条，登记与否取决于裁定）**：*T_recv 口径* —— 现行 = **两列并报**（主列「快探/巡检首失败行 → 恢复行」+
副列 E1 口径，`M6.md` §5 与 `PERF-AB` §9.20.4 已双列在册）。若 M7 决定**统一为单列**，须按 R1 形态登记；
**若维持两列则无需登记**。

**不入 `INTEROP-CRITERIA` 的（说明防误判）**：门槛表数值终值（T2/T6/内存/体积）属**门槛表**面
（`docs/QUIC-ROADMAP.md` 主会话触点），不进判据行登记表；M6/M6.6 系读数的数值语义变化不涉任何判据行行文。

---

## 10. M6 顺延项处置（逐条：**本期做 / 转交用户触点 / 登记不做** + 理由）

来源 = `docs/QUIC-ROADMAP.md`「M6 顺延项」+ `docs/reviews/M6.md` §7 偏离表 / §10 / §12.8.8。

| # | 项 | 处置 | 理由 / 落点 |
|---|---|---|---|
| 1 | **T5**（pf bulk 中位判据） | **登记不做** | ①它是**副判据**（`M6-design` 未设硬门槛）；②pf **功能面**已由 M4 真机 6 场景（回环命中 / LAN 命中 / 删主机 / 热替换 / 占用端口 / 无服务 7ms 归因）+ 本机 e2e 5 用例覆盖；③复现需先在 App 「端口转发」页做 UI 装配（M6 已证时间盒不可得，`M6.md` §7-2）⇒ 成本/收益不符 M7（生产切换期）；④**转交**：上线观察期内若用户报 pf 性能，按 §5-F10 走；读数口径沿用 `tools/m6-ab.sh`。 |
| 2 | **T11**（路径变更 + 腿切换） | **本期做（S2a 本机 + S2b/U2 真机）** | 它是**门槛行「换网迁移」的替代口径**（真 rebind 不可得，`M6-design` §12-2）且 M6 **连替代也未做** ⇒ 上线前必须有一条「迁移/切换不拆世代」的实测；器具已点名（本机 = `exit/tests.rs` 的 rebind 用例；真机 = `local-rust-relay.sh` + `pkill`），**不需新写脚本**（r30 中-17）。判据 = `path_changes`/`migrations` 增 + 连接/设备表条数不变 + 无世代重建。 |
| 3 | **S7① DNS 回复 e2e** | **本期做（S2a 本机 + S2b/U2 真机）** | ①本机即可做（隧道 DNS = 出口 IP :53/:5300，核侧 `dnsAddresses` 不变）；②它是 M5 交下的四项之一（`M6.md` §10-④）；③`dnsproxy` 在 M5 换了入口链（DATAGRAM）⇒ 有真回归风险，属上线前必查面。判据 = 经隧道查询 + `dns: q=N resp=M` 对账。 |
| 4 | **S7② pf 形态 1·3**（回环 / 分流打开态） | **转交用户触点** | 形态 1（出口回环：`http://127.0.0.1:<port>`）依赖 **tier 侧 `exit-transit-intercept` 的回环同端口豁免**——该 MUST 的退役/换源 = **M5 设计 §12-C-7「须用户点头」**（§7.3-③ 同批）；形态 3（分流打开态）需在 App 内改分流设置（人工）。⇒ **不点头则保留豁免（本机不可测形态继续保留）**，点头则两件事同批（spec 修订 + 真机复验）。 |
| 5 | **S7③ 旧 token 归因行**（M6 §12-1 留 M7） | **本期做（S1，小改）** | M6 明示「留 M7 设计门承接」，且本质 = **上线前可诊断性**：用户持旧 `hmw1` token 撞新出口时，须有一条**可行动**归因（而非「登记失败/连接关闭」）。落点 = 客户端 token 解析错误面（`TokenError::UnsupportedVersion` 的用户可见归因：点名「存量 token 已失效，请重新粘贴主机输出的新 token」）+ 必要时出口侧准入拒绝行的 why 字段；**判据行影响 = 新增/改写一条 C 族归因行 ⇒ 必须同批登记**（追加至 §9 登记面）。 |
| 6 | **T9 单连接复跑 + M6.7 内存账** | **本期做（S3）** | ①M5 终值按修订门槛判过但 M6 未复跑（`M6.md` §9.2/§10-①）；②M6.7 的 +3 MiB socket 缓冲**改了内存账**（`M6.md` §12.8.8-3 明列「需在 T9 四格复跑登记」）；③M6 的每连接边际 96.00K vs M5 65.60K 差异**未定因**（M6 §9.2 列 M7 复核项）。命令 = `tools/quic-ab.sh mem --mode steady/load/conns`（lab 档，须独占机器）。完成判据 = 四格读数 + 与 M5/M6 同表并列 + 差异结论（定因或登记未定因）。 |
| 7 | **T3 旧臂（relay/direct 参照臂）** | **登记不做**（措辞按 r30 低-24 收紧） | 旧枝在 LAN 下 2s 内把中继腿升级直连（`M6.md` §3.3 逐字证据）⇒ **LAN 拓扑下构成性不可得**；**U3（升 Mac）之后「永久不可得」**（不再有 G1 出口可跑）。⇒ 处置 = 登记「未验（参照臂不可得）→ **U3 后永久不可得（旧代退役）**」；**切换前若用户能提供非 LAN 拓扑（第二网络/蜂窝），仍可补测一次**（唯一窗口）。 |
| 8 | 待机档长尾 | **登记不做** | T_recv 门槛只承诺**在用档**；待机档 60s 拍是**设计内**（M3 已文档化 32.06s 长尾，`M3.md` §7-3）⇒ 复测不改变任何判据；落点 = `connection-lifecycle` 定稿的「待机档不承诺 3.5s」句（§7.2）。 |
| 9 | **蜂窝腿 / 真 rebind 迁移** | **转交用户触点** | 环境不可得（设备无 SIM 数据服务：`rmnet0–11` 无 IPv4、Settings `enabled=false`；出口侧无蜂窝可达公网端点）× 两轮登记；判据面**不因缺腿降级**（M6 §6 固定句法）。⇒ 上线后由用户在有 SIM / 第二网络的条件下补验；**上线风险与缓解写进 §11-R1**。 |
| **10** | **T8 耗电（r30 中-10 补）** | **登记不做** | `M6.md` §6 固定句法已定「设备无可用电流/电压读数 ⇒ 耗电维**以设备侧累计 CPU 时间/字节为代理**（T6），**不宣称功耗结论**」；M6 代理读数 = 新臂最差格 1.12 核 vs 旧 0.96 核（+17%，<20% 登记线但接近）⇒ M7 不复测、不改判据；上线后若用户体感耗电异常 ⇒ 按 §5-F10 处置 |
| **11** | **T12 真机窄路径丢包可观测（r30 中-10 补）** | **登记不做** | 结构不可得（注入面要求 `max_datagram_size < 1280`，而 MTU 区间 `[1320,1400]` 内 1320 ⇒ mds 1282；设备不可设 `HOMEWAY_QUIC_MTU`、App 不发 `quicMtuCap`）⇒ **本机测试缝面已有常跑用例**（`mtu_cap_below_inner_mtu_marks_narrow_path`）+ 设备侧 `drops{}`/`lost_packets`/`congestion_events` 全 0 旁证（M6 §3.12）；归 tier 侧旋钮（用户触点） |

---

## 11. 风险与未决

| # | 风险 / 未决 | 影响面 | 缓解 / 处置 |
|---|---|---|---|
| **R1** | **真机蜂窝迁移从未验证**（WiFi→蜂窝 rebind 不可得；替代注入本批补做） | 用户外出切网时是否掉线/是否新增设备条目 | ①M1/M2 已验「出口侧路径变更」+ 协议迁移原生（quinn `rebind()` + `migration=true`）；②中继腿兜底（`via=relay` 已验）；③本批补 T11 两级替代注入（§10-2）；④观察期重点看 `路径变更`/`迁移未确认`/`链路重连` 三族行 |
| **R2** | **T2 热 0.893×（TUN 吞吐差 11%）带上线** | 用户感知（大文件/TUN 全局代理吞吐） | 已登记（`PERF-AB` §9.20.11）；自连面（files/term/speedtest）无损；修复候选属**另开批**（`M6.md` §12.8.8）——M7 不扩范围 |
| **R3** | **回滚件易失**（G1 核 `.so` 仅在 `/tmp`；两台无 `.bak-v0.2.3`；state 未备份） | 回滚能力 = 0 | §4.0-C **七件**持久化 + 切换前置硬校验（清单第 13 项，含 §4.0-E 台账跨代可读校验） |
| **R4** | **config `quic_listen` 与旧二进制互斥** | 回滚时旧出口拒启 | §4.0-D：回滚 = 三件套（二进制 + config 还原 + 启动）；清单第 13 项含 `config.toml.bak-pre-m7` |
| **R5** | 端口退让（41641 被占）静默换号 | token 端口/安全组/文档口径错位 | §5-F3：上线判据加「端口未退让」硬项（V1）；`cache/listen_port.txt` 复核 |
| **R6** | 阿里云开机不自启（nohup 形态） | 云主机重启后出口不在线 | 既有形态（与 Go 时期同口径，`DEPLOY` §2）；本批不改（若用户要 systemd 化 = 另立触点） |
| **R7** | 「半代态长期停留」（兜底已升、主力/核未升） | 设备侧长期单出口可用 | 允许的停留态（§5-F11），但**应用户要求尽快完成**；若停留 > 数日 ⇒ 复核兜底出口的健康行 |
| **R8** | M6.7 剩余 11%（外承载缺按带宽速率控制） | 同 R2（根因层） | 登记为**后续独立批候选**（候选三选一 + 判据变更流程），路线文件「下一步」承接 |
| **R9** | `log-index` 门未修（tier 侧） | 正式出包路径不可用（走逃生口） | §7.3-⑤ 列为 tier P1 交付项；M7 自身出包可继续走逃生口（先例两次，须留痕） |
| **R10** | 设备侧一次「出口进程死亡」未复现（M3 §7 随访仪器） | 排障面 | 观察期用 `tools/m3-s9-bulk.sh` 仪器随访；本批不改代码 |

---

## 12. 实施清单（切片 S0–S5 + C1 发版点 + 依赖顺序 + 完成判据 + 用户触点标注）

> 时序：**S0（前置）→ {S1, S2a, S3 可并行} → S4（门）→ C1（版本 bump + 推 main + 打 tag v0.3.0，= 发版点）
> → 用户触点切换（U1 → U2〔含 S2b〕→ U3）→ S5（文档收束 + 收官）**。
> 「用户触点」列 = 需用户显式点头/执行的动作。

| 片 | 内容 | 依赖 | 完成判据 | 用户触点 |
|---|---|---|---|---|
| **S0** | **前置核验 + 回滚件持久化**：清单 §6 十五项全绿（`ci-local` 八步 / 测试 / 三目标 / 三门 / e2e 四件 / 指针一致）+ §4.0-C **七件**转存校验 + §4.0-E 台账跨代可读校验 + §2 生产面侦察刷新 | — | 清单全绿留证（`/tmp/m7-gates/`）+ 七件校验输出在册 | 否 |
| **S1** | **旧 token 归因行**（M6 §12-1 承接）：客户端 `UnsupportedVersion` 的用户可见归因 + 判据登记 | S0 无（可并行） | 单测（旧串 ⇒ 点名归因）+ `INTEROP-CRITERIA` 登记条 + e2e 负向（`quic-wg-e2e.sh` 的存量 token 负向升级为「点名归因」断言） | 否 |
| **S2a** | **T11 本机档 + S7① 本机档**：跑/扩展 rebind e2e 用例（`path_changes ≥1` + `connections == 1`）；DNS 回复 e2e（本机，经隧道查询 + `dns: q/resp` 对账） | S1（同树构建） | 两用例绿 + 读数入库（`docs/reviews/M7.md`） | 否 |
| **S2b** | **T11 真机档 + S7① 真机档**（**⊂ U2 窗口**，r30 中-14 拆片） | U1 全绿（阿里云 G2 可用） | V19（腿打死落直连：`路径变更` 行 + 设备表条数不变 + 无世代重建）+ V20（真机 DNS 轮）；计入 U2 完成判据 | **是**（设备占用） |
| **S3** | **T9 内存复跑 + M6.7 内存账登记**：`quic-ab.sh mem` 四格 + `ClientSock` 缓冲条目入 `INTEROP-CRITERIA` + `QUIC-BASELINE.md` §4 | S0 无 | 四格读数 + 与 M5/M6 同表 + 96.00K vs 65.60K 差异结论 + 登记条落地 | 否（须独占机器） |
| **S4** | **代码门 + 本设计收口**：`docs/reviews/M7.md`（文档一致性专项 + 两轮意见 + 处置表）+ 主会话终检；**同时落 §9 待补 7 条** | S1–S3 | 门记录入库；本文件补「门结论」小节；登记表 7 条 append-only 落地 | 否 |
| **C1** | **发版 commit**：`Cargo.toml`（0.2.3→0.3.0）+ `Cargo.lock` 四处 bump ⇒ 推 main ⇒ **打 tag `v0.3.0`** | S4 | `git status` 空；`tag == origin/main 头`；Release 五 job 全绿 + `SHA256SUMS --check` OK；`--version` = v0.3.0 | **是** |
| **U1** | **阶段一：阿里云出口 → G2**（§4.1） | S4 + C1 | V1–V9 全绿（含身份连续性 V3 / 端口未退让 V1 / 中继互注 V8） | **是**（生产出口 + 云主机） |
| **U2** | **阶段二：设备核 → G2 + 贴阿里云 token**（§4.2，含 S2b 的 V19/V20） | U1 全绿 | V10–V20 全绿；回滚路径（G1 核 + Mac 旧 token）就绪 | **是**（设备 + App 出包/装机；**pin 前进**） |
| **U3** | **阶段三：Mac 出口 → G2 + 贴 Mac token**（§4.3） | U2 全绿（**不变式 I 复核**：核已 G2；且阿里云 G2 token 在手） | V21–V24 全绿 + 观察期 30 min 无 ERROR/panic | **是**（生产出口 + launchd） |
| **S5** | **文档收束 + 收官标记**：§8 全清单落地（README/AGENTS/CHANGELOG/BASELINE/DEPLOY §12/DEVICE-TEST §8/QUIC-BASELINE）+ `QUIC-ROADMAP.md` M7 收官（主会话）+ 本文件收官标记 | U3 | §8 表逐项打勾；指针一致性自检（AGENTS 速查 ↔ README ↔ 路线文件三处互指）；`docs/reviews/M7.md` 记录切换实录 | **是**（tier 侧三项：pin 已前进、`connection-lifecycle` 落库、App 适配清单；`QUIC-ROADMAP` 收官 = 主会话） |

**判据面零漂移纪律**：S1–S3 任何改动若触判据行 ⇒ **必须**同批登记 `docs/INTEROP-CRITERIA.md`；
S5 的文档改动**不得**改判据行（只改现行口径面，历史实录不动）。

---

## 13. 设计门自检（评审协议 checklist 逐项）

| checklist 项 | 本设计的覆盖 |
|---|---|
| **功能等价面**（全局代理 / 文件 / 终端 / 端口转发逐项） | §4.2 V14–V15 + V23（切换后逐项烟囱：层 0 / files / term / speedtest / portfwd 形态）；pf 形态 1·3 的等价面登记在 §10-4（tier 触点） |
| **边界与错误面** | §5 十二条失败预案（含 token 失效 / 端口退让 / 半代态 / 回滚后仍不通）+ 代际矩阵的**两条硬失败边**（§3.2） |
| **并发与生命周期** | 不变式 I（§3.3）在**每一步**的复核点（U1/U2/U3 前置）；App 侧 `force-stop` 纪律（HostStore 缓存，§4.2/§4.3）；世代不重建判据（V19） |
| **残留 WG 语义隐含依赖** | ①`quic_listen` 缺省 = `listen+1`（WG 端口语义残留，§4.0-B）；②`serve.listen` 在新代**仅作端口基线**（§2.4 已验证：`listen_port` 只被 `quic_listen_port()` 消费）；③E1 `key=`/E19 标签的身份连续性（§3.2-5）；④`hmw1` 存量令牌**无兼容**（§3.2）；⑤**`hmw2` 仍接受 `Direct`(wire 0) 端点**（宽松 `from_wire` 是保留解析面，本代不产出该形态）；⑥**端口退让是静默换号**（仅一行 ⚠️）⇒ M7 用硬判据封住（V1 的两条硬判据，r30 低-19） |
| **地道 Rust** | 本设计不涉代码形态；S1/S3 的实现面沿用既有纪律（thiserror + Result / newtype / 借用优先） |
| **安全面（回滚期的凭证/身份代际）** | §3.3 推论（回退必须同批回退 token）+ §4.0-C-4/C-5（旧 token 与旧核归档）+ §4.3 注（台账 append-only 与 `serve token` 读末行 ⇒ **旧串只能提前抄存**）+ F7（回滚后仍不通的逐项复核含「token 代际」） |
| **预算（体积/性能/内存）可测性** | §6 清单 9/11（体积 `.so` 判据 + `PERF-AB`↔`QUIC-BASELINE` 指针一致）+ S3（内存四格复跑 + +3 MiB 入账） |
| **可测性** | 每条切换验证点都给了**判据行 + 期望输出**（V1–V24）；失败预案每条都有**判定信号**（可 grep 的行） |

---

## 14. 设计门记录（dsh r30）

- **轮次目录**：`/tmp/dsh-review/r30.odswwj/`（`prompt.txt` / `output.md` 37,527 B / `stderr.log` 178,656 B）
- **命令**：`dsh --profile headless "$(cat prompt.txt)"`（工作目录 = 主检出 `main`）
- **退出码**：**`exit=0`**（正文落在 `output.md`；`stderr.log` 为推理流，按 M5 r22 先例不做二次归并）
- **意见条数**：**25 条**（**高 4 / 中 13 / 低 8**）
- **门结论（评审给出）**：**有条件通过**——7 条阻塞条件（C1–C7，须在对应阶段开工前落文档）+ 12 条建议同批修；
  **明确不需要**改切换顺序、回滚最小单元或门槛数值，**不需要**新增产品代码（本棒）
- **本棒处置**：**认同 25 / 部分认同 0 / 不认同 0**（全部已按意见改本文件；逐条见下表）

### 14.1 逐条处置表

| # | 严重度 | 位置（意见编号） | 处置 | 落点 |
|---|---|---|---|---|
| 1 | 高 | `serve token` 输出三行中文，`TOK=$(…)` 抓串坏（高 1） | **认同** | §4.1/§4.2 的取 token 命令改为 `\| sed -n 's/^serve token：//p'` + `case "$TOK" in hmw2*)` fail-closed 断言；§4.0-F 补「运行中读取安全（该命令不取实例锁）」 |
| 2 | 高 | 「端口未退让」不可由 E1 判定；F3 信号串不存在（高 2） | **认同** | §4.1 V1 改为两条硬判据（`quic_listen_port.txt == 41641` + `grep -c '被占用' == 0`）；F3 信号照抄源码原文（`engine.rs:616-621`）+ 后果改写为「入站防火墙/NAT 与实际监听口不一致 ⇒ 公网端点不可达」；§13 补静默换号条 |
| 3 | 高 | 「其余 M0–M6 已全量在册」为假：≥6 条欠登；M6.5 被误判为纯测量（高 3） | **认同**（已回源码/记录复核：`tun.rs` 两条永久行 + 写阻塞语义；`relay_sock.rs:303-310` 新观测行；`M5.md` §8-2/§2 五条自陈欠登；`INTEROP-CRITERIA` 无 `SessionSnapshot`/写停滞条目、`loopback-only` 仅 1 处顺带） | §9 待补清单 **2 → 7 条**（+ 口径可选第 8 条）；§0/§6-12 计数同步改写 |
| 4 | 高 | §8-18 改 `DEVICE-TEST-OHOS` 与「历史文档不追改」条款冲突（高 4） | **认同**（条款原文 `INTEROP-CRITERIA.md:587-590` 已点名该文件） | §8-18 改为「**新增 §8 现行口径节 + §3/§5 就地警示行 + 例外登记列入 §9**」三件套；§8-18b 补 `:26` 的 worktree 路径 |
| 5 | 中 | §3.3「唯一解」论证偷换（候选集不完备）（中 5） | **认同** | §3.3 重写为「**充要条件 = 核夹在两次出口升级之间**；①兜底先升 = **优选**（三条理由）；②主力先升 = 备用路径（代价列明）；第三台临时出口方案已评估不做」 |
| 6 | 中 | 回滚应先试「设备切另一台 G2 出口」（中 6） | **认同** | §3.3 推论新增「**回退手段序**」（①切另一台 G2 → ②单台回退+核回退同批 → ③双台回退）；§4.3 回滚段与 §5 最坏情况表同步改写 |
| 7 | 中 | 回滚件漏 state 全量（`key.bin`/`tokens.jsonl`/`relay.key`）（中 7） | **认同**（B 批两次换装 + 三次滚动升级均做 state tar） | §4.0-C 增**第 7 件**（两台 state tar，含 `relay/relay.key`，条目数留证）；§6 清单第 13 项改「七件」 |
| 8 | 中 | Mac 换装形态偏离先例 + F2 与 `KeepAlive=true` 矛盾（中 8） | **认同**（plist 实测 `KeepAlive/RunAtLoad=true`；DEPLOY 三次均用 `mv` + `kickstart -k`） | §4.3 换装改「临时件 + `mv` + `launchctl kickstart -k`」，`bootout/bootstrap` 降为备选并注明 crash-loop 后果；F2 判定信号改「重复报错 + `launchctl list` 非 0」+ 首步 `bootout` 止血 |
| 9 | 中 | `QUIC-BASELINE.md` 现行口径与门槛表冲突 ⇒ §6-11 会假绿（中 9） | **认同** | §8-17 扩为**三条**（§1 体积补行 / §2·§3 `wg-shim` 措辞 + 作废标注 / §4 门槛标注被取代 + 指针）；§6 清单第 11 项要求三处一致 |
| 10 | 中 | §10 漏 T8/T12；三处计数互相矛盾；T3 两处处置不一（中 10） | **认同** | §10 补 **T8/T12** 两行；计数统一为 **11 条（本期做 4 / 转交 2 / 登记不做 5）**；T3 单处处置（措辞按低-24 收紧为「U3 后永久不可得」） |
| 11 | 中 | V15 在阿里云缺前置（`files_root` 空目录 / portfwd 无目标）（中 11） | **认同** | §4.1 前置加「放 `m7-probe.bin` + 记 sha256」；V15 点名样本 sha256 与 portfwd 目标（`127.0.0.1:<临时 nc 端口>`） |
| 12 | 中 | 设备装机失败分支缺失（签名不匹配须卸载 / VPN 授权）（中 12） | **认同** | §4.2 加装机前置记录 + 卸载清数据的后果句 + 授权弹窗步骤；新增 **V13b** 与 **F13** |
| 13 | 中 | 发版时序：tag 不能早于 S1–S3；版本 bump 不能排到 S5（中 13） | **认同** | 新增 **C1 发版点**（S1–S3 + 版本 bump + S4 门 → 推 main → tag）；U1 依赖改「S4 + C1」；§8-15 移出 S5 并点名 `Cargo.lock` 四处；§7.1 注明 pin = C1 且 S5 文档 commit 不后移 pin |
| 14 | 中 | S2 完成判据落在 U2 却排在 S4 之前（中 14） | **认同** | S2 拆 **S2a（本机）/ S2b（⊂ U2）**；§12 时序图与 U2 完成判据同步 |
| 15 | 中 | §8 漏 5 行（README:87 / README:19-22 / AGENTS:43-44 / AGENTS:22+75 / DEVICE-TEST:26+99）+ 三处节号/表形描述不准（中 15） | **认同**（逐条回读原文确认） | §8 增 **6b/6c/11b/11c/18b** 五行；修正 §8-11（门清单在硬规则 1）、§8-13（BASELINE 表为汇总行）、§8-16（§2 正文 `:122`，无 `### 2.3`）；DEVICE-TEST `:99` 的退役串改为「删去（已是 e2e 禁串）」 |
| 16 | 中 | 台账跨代可读性未提（回滚链条唯一可能致命的未知量）（中 16） | **认同**（本棒复核：`git show da95496` 的 `TokenRecord` 无 `deny_unknown_fields`；旧 `from_wire` 宽松） | §4.0 新增 **E. 台账跨代可读性**（结论 + 可复跑校验）；F7 第③项补该点；§6 清单第 13 项含该校验 |
| 17 | 中 | V19 注入手段未点名可执行器具（M6 自陈未做，仓内无脚本）（中 17） | **认同**（仓内确有可复用 e2e 用例 + relay 脚本） | V19 改为两条可照抄路径（本机 = `exit/tests.rs:293-330`；真机 = `local-rust-relay.sh` + `pkill`），并标注真机轮属 S2b |
| 18 | 低 | 旧核是「宽松误认 Quic 为 Direct」而非「不认」；被引正向 e2e 是 `#[ignore]`（低 18） | **认同**（`git show cbd45f0:…/token.rs:82-88` 确认宽松） | §3.2 矩阵该格改写为两层死因 + 标注 `#[ignore]`；Evidence ② 同步 |
| 19 | 低 | §13 漏「新核仍接受 `Direct` 端点」+ 退让静默性（低 19） | **认同** | §13「残留 WG 语义」行补 ⑤⑥ 两条 |
| 20 | 低 | `tokens` 计数注记方向错（首次 G2 启动仍 28）（低 20） | **认同** | §4.3 V21 + 注记改写（装配时读 / append 在其后；下次重启 +1） |
| 21 | 低 | 缺 `uname -m` 对账 / `ci-local` 第 7 步依赖 `bin/homeway-go`（低 21） | **认同** | §4.1 加 `uname -m`；§6 清单第 2 项加备注 |
| 22 | 低 | §7.3-1「切换卡死」与 `--ps` 绕过矛盾（低 22） | **认同** | §7.3-1 后果改为「用户自助面失效」+「U1–U3 不以本条为前置」 |
| 23 | 低 | V1 样例混入 Mac 参数 + 「同串」不机械（低 23） | **认同** | V1 改通用字段样例；V3 改 `diff <(grep -o 'key=…')` 机械判定；§4.1/§4.3 统一落 `/tmp/m7-gates/<host>-pre.txt` |
| 24 | 低 | T3「永久不可得」应在 U3 之后才成立（低 24） | **认同** | §10-7 措辞收紧（LAN 下构成性不可得 / U3 后永久不可得 / 切换前非 LAN 拓扑可补测一次） |
| 25 | 低 | 新 token 的暴露面无处置（history / hdc 参数）（低 25） | **认同** | §4.0 新增 **F. 凭证卫生**（`set +o history` / 变量传递 / 掩码 / 不从无轮转日志抄）；§6 清单加第 15 项 |

### 14.2 门结论（本棒判定）

**过门**：设计门结论 = **有条件通过**，条件 **C1–C7 全部已在本文档内落地**（逐条对应上表 1/2/3/4/7+16/13/6+8），
另 12 条建议同批改毕，**不认同 0**。⇒ **M7 可开工**，实施按 §12 的 S0–S5 与 C1/U1/U2/U3 时序推进；
**U1/U2/U3 开工前**须由实施棒复核 §6 清单与 §4.0-C 七件在位（缺一不开工）。

> **门不过不得开工**纪律执行情况：首轮即**有条件通过**（非阻塞），条件全部为**文档内可判定项**（无「需新实验」类条件）
> ⇒ 本棒一轮收敛，按 M5 收口纪律（边际项收敛即停）不再追加轮次。
