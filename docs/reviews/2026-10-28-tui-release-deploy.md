# TUI 合并、v0.3.22 正式发布与部署（2026-10-28）

用户明确要求将代码合入主分支并提交、推送、发版、部署。本轮沿用
`AGENTS.md` 与 `DEPLOY-RUST-EXIT.md` 的正式发布纪律：主分支提交通过本地门后推送，
tag 必须指向 `origin/main` 头；GitHub CI 与五目标 Release 成功后，仅用该 tag 官方产物
换装。Mac 与阿里云为本轮正式目标，LAN71 先查可达性，离线不阻塞双出口。

## 范围与分工

- 发布准备 lead 汇总证据和评审记录；版本作者负责版本/CHANGELOG，部署作者负责本机
  独立一次性执行器的最小适配及远端执行方案，两位作者相互完成非作者技术门、代码门。
- 主会话独占实际合并、提交、推送、tag、下载校验、私有实测及生产换装；准备子任务不运行
  cargo、网络、SSH、生产重启或部署。`ROADMAP.md` 由主会话同批维护。
- 既有 TUI 各工作单元已分别提交和评审，本轮总门聚焦新增版本记录、发布条件与部署工件，
  不把先前专项验证改写成本轮官方产物验证。

## 已核实基线

主会话 `git fetch origin --prune` 实际 exit0；`origin/main` 仍为 `deaabd5`，最新 Release
为 v0.3.21（2026-10-23）。本轮确认版本 v0.3.22。待合入 TUI 头为 `3c87d65`，相对
主分支 23 个提交，包含 CLI 与 daemon/term observer、目录采样；不能概括为仅前端更改。

既有专项的最后验证为 TUI **114 passed、0 failed、1 ignored**，CLI check/build/clippy
通过，私有交互 **23 PASS**。这些是既有调试件证据，详见
[`2026-10-28-tui-blocking-notifications.md`](2026-10-28-tui-blocking-notifications.md)，
不替代本轮 `tools/ci-local.sh`、GitHub CI/Release 或官方同件冒烟。

## 两道门与发布条件

**技术门通过**：版本作者和部署作者在编辑前分别提交设计，由对方非作者交叉评审。
版本限 workspace 与 4 个自有包锁定版本同步及完整 CHANGELOG；部署复用已审本机 worker，
只适配官方归档来源/版本和本轮实际旧件，不改独立任务、原子换退、受限备份与状态保全语义。
部署设计另要求私有配置与恢复资料、官方归档/校验表/二进制 SHA 关联、准备及激活双重旧事实
检查。阿里云采用单独小执行器，保持既有角色、HOME、工作目录与零参，进程脱离 SSH。

静态复核 `.github/workflows/{ci,release}.yml` 与 CLI 条件依赖：ratatui/crossterm 仅 CLI
`cfg(not(windows))` 图；Windows TUI 明确不支持，core/capi 不引入该依赖。现有 CI 覆盖
双平台测试、clippy、OHOS/musl 交叉 check 与 Windows `-D warnings` check/运行烟囱；
Release 覆盖 darwin 双架构、Linux musl 双架构、Windows 五目标和 tag 对齐门，无需改工作流。
这仅为静态结论，全部实际门仍须本轮运行，静态审查不替代 CI/Release。

**版本代码门通过**（deployment_plan 非作者）：实读 3 文件最终 diff，独立 tomllib 对比
HEAD，确认只改 workspace 与 capi/cli/core/quic 4 个自有包 0.3.21→0.3.22，依赖、包序、
包数不变，无重解锁。CHANGELOG 30 行覆盖本轮真实功能及 observer/重启/Windows 边界，
无新源码、fixture、判据或工作流改动；Rust 直译痕迹检查对此版本元数据改动不适用。
作者和独立评审者两次核对的冻结文件 SHA256：

| 文件 | SHA256 |
|---|---|
| `Cargo.toml` | `c61f298b1b1797b9904c2f3a4af549c69d1180c2f8e51fa9aafb229abd7d9aa3` |
| `Cargo.lock` | `9f926cef90352fb9f435292825b2ffcd351e468589b125e7fd8f208e69cebb2d` |
| `CHANGELOG.md`（最初TUI摘要，后续补收尸修复见下） | `8f83c644b83139c98521be23e86a4498927e027ebda7cb4892c5047726d31ed4` |

## 只读部署预检

主会话实际核实：本机进程 PID55632、PPID1、UID501，参数仅
`/opt/homebrew/bin/homeway`，在役 SHA256 为
`1390e0ae3a93fb100f58237cfef1febea29f08da817e93b49a28cae6fc4dd59b`。
阿里云 SSH 可达，Linux x86_64，用户 root、`HOME=/root`、Python3.11.2；正式路径
`/usr/local/bin/homeway` 为 UID0:GID0、0755，版本 v0.3.21，SHA256 为
`65a4fe3a6848d9cdfcfd1ad16f4485611c5b220879371bbc3e9f7318204b38d2`，
唯一零参 daemon PID1703776、PPID1703774。新进程脱离 SSH，stdio 独立，不复制旧异常
父进程形态。计划先阿里云，再本机。LAN71 本轮 SSH 仍在 22 端口超时，exit255，
未写远端；该目标离线待更新，本轮不反复重试。

随后主会话只读补核云 PID1703776 的 `/proc` stat 字段22 starttime=`3441616741`；
`/proc/1703776/exe` 为正式路径，运行映像完整 SHA 与磁盘旧件上述 `65a4fe3a…04b38d2`
一致。身份值仅作准备强前提，worker 激活前仍须重核，不因历史 PID 同值省略防复用检查。

进一步只读核对：本机原 launchd plist SHA256
`034bb61e9d60be80f143f4598ebcade25eb23142b0662212590bce95913c2725`，
零参、缺省 state、KeepAlive=true，无 HOME 环境覆盖；config473B、serve/key32B、
tokens74212B 的完整哈希由主会话交给执行器作为准备前提。云原工作目录为 `/opt/homeway`，
serve/relay 均启用，UDP41641 绑定 eth0、UDP/TCP41741，中继控制腿连接 Mac；
config322B、serve/key32B、relay/key32B、tokens990B 均已只读取哈希。
runtime lock 仅取证备份，不作不变判据；凭证台账允许追加，必须保全原字节前缀。

## 本机专用执行器两道门

最终私有脚本 `/tmp/homeway-v0.3.22-release-deploy-20261028/deploy.py`，SHA256
`7cbde3e3026376c73cbeaab1b42ea455396ac698e4678d10b51ef80ecc515a00`，
目录0700、脚本0600。部署作者负责最小适配，version 非作者完成技术门及独立代码门。
相对已审本机 debug worker，新增官方归档/校验表/成员/来源/架构/签名/准确版本绑定与
本轮私有配置，不改原子换退和独立 launchd 的控制语义；旧会话标识常量已移除。

官方来源严格固定为 `/tmp/homeway-v0.3.22-official/darwin-arm64/homeway`，归档与
`SHA256SUMS` 位于同一官方根；归档名必须是 `homeway-v0.3.22-darwin-arm64.tar.gz`，
校验表唯一条目、归档实际 SHA、tar 内唯一常规 `homeway` 成员和 SOURCE 二进制 SHA
逐一关联，Mach-O arm64、严格 codesign、`homeway v0.3.22` 均为准备前置条件。

`prepare --config <私有JSON>` 必须提供 source/archive/checksums/archive_sha256/
new_sha/old_sha/old_pid/resume_file，拒绝额外键、错误哈希或PID类型，配置和恢复文件均须
UID501/0600。主会话填入本轮已复核的旧SHA/PID、GitHub下载后的新SHA及私人恢复文件，
prepare 仍不激活。执行器不访问旧 Codex 会话或在公开仓保存恢复标识。

固定持久备份/回执目录为
`~/homeway-rollback/release-v0.3.22-20261028-r1/`，排他创建；prepare 复制旧件、官方新件、
state、主 plist、校验表、受审脚本、配置与恢复文本。一次性 job 为
`gui/501/me.zhaozhe.homeway-release-v0322-20261028-r1`。恢复入口为该目录的
`deploy.py rollback`，先读 `receipt.json`，不得盲目重新 bootstrap；worker/rollback
只用已保存件，原官方下载目录消失不妨碍恢复。

独立代码门实读完整增量与 worker/rollback；AST 及8项 mock 私有输入/来源校验通过。
root 固定官方路径后，另完成精确来源增量审查与5项 mock gate（精确路径通过；旧前缀、
错误平台、异根归档/校验表拒绝）。这些验证未执行 prepare、worker、rollback 或任何生产命令。
不把该静态/受控校验写成正式换装成功。

## 阿里云专用执行器评审

云脚本与本机脚本分开，上传固定新目录
`/tmp/homeway-v0.3.22-cloud-deploy-20261028/`，官方 Linux 归档、校验表及解包 `homeway`
由本机已校验件 scp 到该目录；备份/持久脚本/回执固定
`/root/homeway-rollback/release-v0.3.22-20261028-r1/`，排他创建。准备仍不切换，激活只启动
脱离 SSH 的专用 worker，daemon 以 root、HOME=/root、cwd=/opt/homeway 零参运行，
stdio 全部独立。旧SHA/PID及 `/proc` starttime 为本轮强前提，按真实可变台账前缀保全，
不将 runtime lock/cache 日志作为不变判据；失败不自动恢复 state 归档。

技术门由 version 非作者给出，要求旧事实+starttime、旧进程停后任一失败均拉回旧件、
detached 子进程身份持久化及精准回退、可变runtime排除。首个候选完整代码门**暂不通过**：
旧TERM超时后不能复用可能继续退出的PID宣称回退完成；spawn 到 PID/starttime 持久化间
也须屏蔽部署中断，避免回退遗失已启动的进程。整改复审又定位旧进程自然退出与 stop
之间的竞态，要求精确目标已消失且无其它正式daemon时幂等成功，防止尚未恢复旧件就早退。
这些未过门候选均未执行。

**首轮独立代码门通过**（version 非作者；实际路径更正后的最终 SHA 见末节）：私有脚本
`/tmp/homeway-v0.3.22-release-deploy-20261028/ali-deploy.py`，完整 SHA256 为
`f622e789cb63fb13a720e122f055dc1a61f524ad7830b3bb7f39d65f759ddaa2`，
脚本0600、父目录0700。上述三项已闭合：pidfd 钉内核进程，核对 starttime/运行映像
SHA 后才发 TERM，回退必要时仅对该精确进程有界 KILL；无关或复用 PID 拒绝。新旧进程均
停净后独立拉起旧件，回退也验证10秒稳定及最终完整就绪；新旧 spawn 的身份落入持久回执
前屏蔽部署中断，子进程恢复正常信号设置。准备实际探测 Python/kernel pidfd 支持，缺失
即安全拒绝，不默默降级为数值 PID 信号。

主会话随后在云只读确认 kernel6.1.0-38-amd64，`pidfd_open(1703776, 0)` 与该句柄的
signal0 检查成功后关闭，未停止进程或写生产文件；这只提前排除系统能力阻塞，prepare
仍须自行重核。

Python3.11 AST 检查通过；作者19项临时/mock fixture由非作者独立重跑通过，另有非作者
4项 ss/版本校验、2项最终 pidfd 强退/复用 fixture通过，不累计旧版验证充当更多测试。
全为私有临时文件与mock，未运行 prepare/activate/worker/rollback 或生产命令。

同目录 `deployment.local.template.json` / `deployment.cloud.template.json` 提供必填配置
结构；云模板已填本轮实际旧 starttime 和关键文件大小/哈希，local模板已填实际旧SHA/PID。
新 binary/归档 SHA 仍须从官方 Release 下载核验后填入；占位值会被脚本拒绝。云 prepare
后从持久目录执行 `ali-deploy.py activate`，恢复先读回执及 `RECOVERY.txt`，必要时该脚本
`rollback`，不重建身份或自动解压覆盖 state。

## 正式换装与验收边界

先在本机下载 v0.3.22 平台归档及同版本 `SHA256SUMS`，校验归档、平台、二进制版本和
完整 SHA，再用官方件于 `/tmp` 私有 state/端口冒烟。Linux 归档由本机 scp 上传阿里云，
远端复核归档/二进制 SHA，沿用 `/usr/local/bin/homeway` 零参、`HOME=/root` 与原工作目录，
不增加自启动项。Mac 沿用 `/opt/homebrew/bin/homeway` 零参、缺省 state 和现有 launchd。

Mac 切换前交付私人恢复指令，先保存旧 binary、plist、全部非 cache 普通 state 文件及
关键字节哈希，备份目录 0700、敏感文件 0600。一次性独立 launchd worker 完成原子换件、
重启与持久回执，避免承载本次控制会话的 homeway 重启中断部署；失败只还原二进制与进程，
不自动覆盖当前 state。恢复后先读回执，禁止盲目重复激活。

在役版本、工件 SHA、角色/监听、身份与配置保全及稳定性须实测；被动 GREETING 只证明
协议能力，不等于真实 TUI/PTY 验收。正式完成证据还需分别记录真实终端输入、surface、
resize/KILL、重复 PTY 回收及中继控制恢复；未做或受限的项目如实记录。

## 本轮实际执行

主会话启动 `tools/ci-local.sh`（日志 `/tmp/homeway-v0.3.22-ci-local.log`），基线门通过，
步骤2 workspace 测试首轮失败：core **927 passed、1 failed、6 ignored**，既有
`term::service::tests::attach_size_applies_to_pty` 触发60秒硬期限。该项早先也曾在
[`2026-10-27-tui.md`](2026-10-27-tui.md) 记录同类超时和隔离复跑通过；历史不替代本轮
复验，本轮首红保留，不能写成一次全绿。后续确切复跑与完整门结果待主会话执行后回填。

version 非作者只读诊断：该测试函数与 `origin/main` 逐字相同，SHA256
`9f831056e8839396670cd2beb4e8c79e412bca44c81f66a0c6611d58e2e3a73f`；
PTY/wire 文件本轮零差异。R7/R8 已记录同形 F3 挂死且未确认根因，本轮形态与历史一致，
但没有本轮线程栈，不能据此断言是环境或排除本轮采样/握手路径。保持60秒硬失败、不新增
ignore、不猜改产品代码；先单例，再同树完整门。若再次失败，须采挂住线程与私有进程证据
定位阶段后处理，单例通过不能代替完整门。

主会话隔离 exact 复跑也实际失败：
`/tmp/homeway-v0.3.22-attach-size-isolated.log` 显示 **0 passed、1 failed**，同一60秒
硬期限，不能据历史偶发形态放行或单纯重跑完整门。发布暂卡真实测试。新增诊断准备仅在
`/tmp`，由单次 probe 自行启动相同 exact cargo，并在自有测试 binary 挂住时自动采线程栈和
其 dscl/shell 子进程信息；生产进程、参数、环境与凭据均不采。probe 先经非作者两门，
实际执行与证据归主会话；未取证前不改产品/测试或减弱期限。

诊断 probe `/tmp/homeway-v0.3.22-attach-size-probe.py`（0600），SHA256
`414eda8deaafc0a879be0e827a27b8ea121194f1ea5df2f362ff1bf757ceb028`。
version 作者、deployment_plan 非作者完成技术/代码门：固定原exact命令/cwd、自spawn
cargo根，唯一直属 homeway_core 测试 binary 出现后10秒计时；只沿 `pgrep -P` 自有树，
采前核 PID/PPID/启动时间/comm 和完整亲缘链，最多测试进程加4个dscl/shell后代并行
sample3秒、工具各8秒限时。目录0700/文件0600，不取全机进程表、args/env或凭据，不
signal测试/生产进程；240秒超限只写 still_running，并与真实cargo返回码区分。
AST及7项纯身份/亲缘fixture通过，准备方未运行 probe main、cargo 或 sample。

主会话实际运行 probe，受限目录
`/tmp/homeway-v0.3.22-attach-size-probe-vqbdvjup/`。最终回执 cargo_returncode=0、
still_running=false、总1.321秒；cargo.log 为与隔离首红相同的
`homeway_core-027277568a4e6614`，exact **1 passed、0 failed，0.85秒**。
早于10秒采样时点结束，samples为空，没有线程栈可定位。前两次60秒失败仍成立；此轮
只能确认 isolated 与 exact-probe 两次均用 `027277568a4e6614` 的非确定复现，不能扩称
与 workspace 构建同件，也不能宣称修复或凭日志归因沙箱。源码、测试及期限均未改。
后续完整正式门仍未由这些单例结果替代。

随后准备完整 ci-local 单轮诊断 wrapper
`/tmp/homeway-v0.3.22-ci-local-probe.py`（0600），SHA256
`1886b8c4dc75f10ebb9d45480e23d4f742bb5aab15792748b28463910e0a1e33`。
非作者增量技术/代码门通过，独立AST与权限检查通过；仅改固定ci-local命令、自有后代
完整core测试名匹配、25秒单次采样、30分钟只观察上限及真实CI返回码字段，沿用原身份/
亲缘/权限/no-signal路线，不新增测试suite或改源码/期限。

主会话实际运行完整retry，证据目录
`/tmp/homeway-v0.3.22-ci-local-probe-z2qzwff9/`，最终 receipt 为
ci_returncode=1、still_running=false、总84.613秒，core再次 **927 passed、1 failed、
6 ignored**，同一测试60秒硬期限。workspace binary 为
`homeway_core-d235264d0c442a0e`，与 isolated/probe 的 `027277568a4e6614` 不同。
采样工具实际exit0，得到该自有测试PID4134的3秒完整线程栈。

栈的测试body停在 `svc.close`（原service.rs5491）；收尾线程为
`finish_session`→`PtyShared::stop_and_wait`→`PtyCore::wait_bounded`→`wait`→`__wait4`，
pump线程在等待PTY互斥锁。至此证据定位收尸阶段，不能继续将失败称作已证实的环境偶发或
再无证据重跑。version 根据现有栈定位最小修复，由deployment_plan独立技术/代码门；
修复及后续完整门未完成前不发布或生产换装。

## PTY 收尸阻塞的最小修复

version 作者根据上述真实栈与本地 portable-pty0.9 源码实现，deployment_plan 非作者完成
技术门。代码事实是 `clone_killer().kill()` 的 Unix 单进程回退仅发 SIGHUP，而原
`wait_bounded` 最后调用无界 `Child::wait()`；样本证实后者已在真实收尾路径阻塞，不能
将构造回归中的PGID失配额外写成样本直接证明的原因。

设计保留原宽限，随后真实SIGKILL并以新增2秒预算继续 `try_wait`，用类型化错误返回超时/
IO失败，不再永久持PTY core锁等待。强杀只在持有Child所有权并确认 `Ok(None)` 时发送，
IO/ECHILD不被旧Option包装误当存活，input取消仍无锁。超时保留Child、done registry和
runtime，由既有pump/原spawn失败cleanup线程在锁外让权后继续受限回收，不新建detached
等待线程；真实收尸后按name/generation统一幂等卸载，writer退出不提前丢失待回收对象。
只有真实status产生D19信号退出-1，既有ENDED语义和wire形态保留。

作者新增HUP忽略且PGID失配的真实子进程回归，以及不退出mock禁止调用无界wait的回归；
测试fixture中跨线程裸PID清理竞态在非作者复核时已改为同owner同步强杀及受限清理。
新预算/重试常量与诊断在 `INTEROP-CRITERIA.md` 同批登记。生产函数与回归候选冻结后，
主会话实际 `cargo check --workspace --offline` **exit0、7.11秒，无warning**。

**最终独立代码门通过**（deployment_plan非作者）：完整读取实现/错误面/所有权及2项
回归，确认业务仅有界try_wait、IO/ECHILD不当活且不伪造退出码；真实强杀有core锁与
Ok(None)守卫，原线程每轮重试释放锁，name/gen幂等回收与writer卸载不变量成立。未新增
多余Arc/Mutex或逐行移植式结构。cfgtest helper/impl/Stdio在测试范围，Windows不装配
整个term模块；这是静态结论，实际cfgtest编译和2新case仍须由主会话验证。

root授权后，CHANGELOG在v0.3.22最小补2行强制终止/有界收尸修复，非作者增量门通过。
最终冻结SHA256为：

| 文件 | SHA256 |
|---|---|
| `crates/homeway-core/src/term/pty.rs` | `ab437f6923c72264638425b24124a5cf6a9d1c62786ede767af3bfcaa59ef5ad` |
| `crates/homeway-core/src/term/service.rs` | `c4f641fa612da9a9563339ededc286d65793c806f639d72aa91682d5bd604773` |
| `docs/INTEROP-CRITERIA.md` | `8153fc22586f67d6c65193be6cc769c0afd7009dbaed414f5cf2211a8117419e` |
| `CHANGELOG.md` | `352d89a76e052fd6b8f2832d0b6d3fc03e552e29a8c2ab04b580aa898c014e56` |

Cargo.toml/lock版本SHA保持前表不变，不能以生产check或独立静态门代替测试。

主会话随后实际运行PTY模块回归，日志 `/tmp/homeway-v0.3.22-pty-fix-tests.log`：
**8 passed、0 failed，exit0**，编译11.08秒、测试0.45秒。新增忽略HUP/PGID失配的真实
子进程强杀回归与pending Child强杀后预算/保留回归均通过。源码/测试继续冻结，随后
主会话用同一已审完整CI wrapper实跑整门，结果如下。

修复后完整CI wrapper的本轮日志为
`/tmp/homeway-v0.3.22-ci-local-probe-i2w6xovd/ci-local.log`。已实际通过workspace测试：
顶层 **1447 passed、0 failed、40 ignored**，未重复计入core内部派生子进程的额外1/0。
单元分别为capi4/0、CLI190/0（1 ignored）、core930/0（6 ignored，33.55秒）、
QUIC294/0；集成合计29通过、33 ignored，doctest0/0。先前失败的
`attach_size_applies_to_pty` 在此完整workspace中实际通过。

随后 clippy `-D warnings` 完成（9.96秒）、OHOS交叉check完成（9.22秒）；OHOS NDK真
link实际完成（56.19秒），导出20/20，注入 `3c87d658d9d7+dirty-rust` 验证通过，
`libclientcore.so` **2,944,208B**。这是本轮私有门产物，不是正式Release件，也未用于手机
部署或tier pin更新。QUIC隔离十一条、向量/SUMS确定性、fuzz种子、词表与WG残留门均已
通过，私有冒烟前置release构建完成（1分36秒）。

第7步RRR基础矩阵实际完成，TOTAL **316秒**：E13测速down881/up876Mbps通过，
100MiB上传/下载F-100MB实际SHA双侧一致通过，E10 transit往返58B和FB-files均通过。
E11按既有独立证据规则为WARN（QUIC dial收口日志节流，绑定M4流收口/泄漏判据及本轮
E10往返），DNS按既有退役形态为SKIP；不把两项记成新实测PASS。矩阵结果已生成
[`matrix-latest.md`](../matrix-latest.md)。等待期间只读脚本/自有日志，未运行新矩阵或
发送信号。core进程超过25秒曾触发预定单次采样，但该进程最终测试全绿，不能据采样
触发本身写成再次挂起。

**修复后完整本地CI实际exit0**：主会话收取的最终receipt为ci_returncode=0、
still_running=false、samples=1，总690.111秒；日志明确
`本地 CI 全绿（2026-10-28 15:59:40；档位：quick）`。本轮执行quick档，未运行
`--full` 的ignored全量/fuzz性能档，不能冒称全部ignored用例已过。
证据链完整保留为：首次完整门失败、isolated失败、exact-probe绿但无栈、完整retry红并
取得真实wait4栈、源修复独立两门、PTY **8/0**、workspace **1447/0**、完整quick门
**exit0**；不将历史失败改写为一次全绿。

## 主分支、GitHub 门与官方产物

主会话提交收尸修复 `960a3d8`，版本与发布准备提交
`36d7ec8aee2a61179a8b960131022cc394688146`，随后从 `tui` 快进 `main` 并推送成功。
工作区干净、HEAD 与 origin/main 同值后才创建并推送 annotated tag `v0.3.22`。

[主分支 CI 40659058316](https://github.com/zhaoyswd/homeway-rs/actions/runs/40659058316)
实际 success，包含 macOS/Linux 测试、clippy、OHOS/双musl/Windows check 与隔离门。
[Release 40660481250](https://github.com/zhaoyswd/homeway-rs/actions/runs/40660481250)
五平台 build 和最终 release job 全部 success。期间未推进 main，以免影响 tag==origin/main
门。公开 [v0.3.22](https://github.com/zhaoyswd/homeway-rs/releases/tag/v0.3.22) 发布时间为
2026-10-28 09:01:47 UTC，非 draft、非 prerelease，五平台归档及 SHA256SUMS 全部齐全。

主会话在 Mac 下载官方 darwin-arm64/linux-amd64 两归档与原始 SHA256SUMS，逐一绑定校验，
再只提取唯一普通文件 `homeway`；Mac 架构、strict codesign、准确版本和 Linux x86_64
static-pie 属性均通过。原始校验表 SHA256 为
`f8e5b75734fa8c06e31abccd8bfbed71d648b264a0a3de3dad8c18d493af11cf`。

| 官方目标 | 归档 SHA256 | 二进制 SHA256 | 二进制大小 |
|---|---|---|---|
| darwin-arm64 | `88d620601def233c79b250b8e815e303e0e47d952f72a9e76f1cf56c0ee96223` | `dfe0b99ad21639f95851120644f95d7f85a0f08b70049ec4cf37d02cf9b90795` | 8,996,744B |
| linux-amd64 | `43005d22cc1e8ec9c782d9f76a001154422aa5897329b737f7ee07d2386e240c` | `4416735b71700d3af25662dc847cdba19053f721d244de21559acc49f9f220b5` | 10,909,856B |

本机官方件实际私有测试全部 exit0：`tools/tui-smoke.py` **23 PASS**、
`tools/tui-remote-smoke.py` **8 PASS**、`tools/tui-selection-smoke.py` **7 PASS**。
日志和截图位于 `/tmp/homeway-v0.3.22-official/`，模拟剪贴板/OSC52仅验证请求字节，
不声明桌面实际接收。各脚本清理自有进程，不访问正式 state。

Linux件由本机 scp 上传阿里云，远端复算归档、二进制、校验表与已审脚本 SHA 均一致。
首次官方私有 smoke 在 production baseline 前置即失败，尚未创建私有实例、prepare 或
换装：原部署模板误记中继密钥为 `relay/key.bin`。随后主会话只读核实真实普通文件为
`relay/relay.key`，root:root、0600、32B，其 SHA256 与原记录
`df7664188129463c820b0852666b6c90967df2f531af24aa374f88fd0b447d5a` 一致；
没有软链或身份变化，不应放宽原软链拒绝策略。路径修正和重跑结果后续据实记录。

deployment_plan 仅修正云部署必选路径、云模板键和 smoke 固定 helper SHA，version
非作者增量技术门/代码门均通过；独立逐字节比较确认恰好三处字面替换，AST/JSON/0600
检查通过，软链拒绝及其余生命周期逻辑未改。最终执行件 SHA256：

| 私有执行工件 | SHA256 |
|---|---|
| `ali-deploy.py` | `3c99ba6a9996db9d7fa4d0f7bc11109de4d1b448fa4566632f723cae5595271b` |
| `cloud-smoke.py`（包含后续ss后缀最小修正） | `a701a3a38e9b8414db5182992a65b7050743c42e7661df2cdf0c87df7cbb66d8` |
| `deployment.cloud.template.json` | `d7d12d41250366cf6a7fa5319f7e6d7705313fb6617c4f8cd12ea39188a8281b` |

Linux 私有验收设计单独通过非作者技术/代码门，除 AST 外独立执行14个 golden grid、
14个合成 v5 snapshot 与3个坏/超限拒绝纯函数检查。实际网络验收仍须 root 实跑，不能
把静态门写成官方产物行为已过。脚本核验生产旧 PID/starttime/关键摘要，私有 HOME、
state、身份与 loopback 高端口；QUIC probe 和共享 LIST 走实际网络，INPUT/surface/
resize/KILL(-2)走同进程共享终端服务，observer仅检查能力，不冒称完整被动行为实测。

修正密钥路径后的云 smoke 实际 exit1，回执
`/tmp/hw22-linux-smoke-hed_2btd/receipt.json`：passes=0，错误为
`private QUIC listener is not exact loopback/high port`；自有 daemon1721666 已清理，
private_processes_exited=true，正式旧进程身份与4项关键原字节均保全，无追加。
产品源码与该官方件日志证实配置 `bind_interface="127.0.0.1"` 按地址绑定并钉lo，
不能误改成仅钉卡的 `"lo"`，后者会采用 wildcard 地址。

原私有进程已退出，未保留其当时 ss 行；root另用同机临时 Python UDP socket 实际核实
相同 `bind(127.0.0.1,随机端口)`+`SO_BINDTODEVICE(lo)` 的 ss 显示为
`127.0.0.1%lo:36015`，随后 socket 自动关闭。由此定位验收脚本的字符串全等前提错误，
并不宣称原私有 PID 的 ss 原行已被保存。修正方案仅接受精确127.0.0.1、可选空/lo接口、
精确已分配高端口，继续拒绝其它地址/设备/wildcard；自有 PID 的 ss 行须落入回执后断言。

上述 ss 后缀修正再次通过 version 非作者增量技术/代码门，独立10例解析、Python3.11
AST/0600检查通过；私有配置和产品不变，ali helper 末段端口解析原本兼容接口后缀，无需
修改。只替换 cloud-smoke.py 后主会话实际重跑，**exit0、PASS、4项**，回执
`/tmp/hw22-linux-smoke-jgmaq4ef/receipt.json`。QUIC准入/path probe通过，随后3轮实际
QUIC共享LIST、shell INPUT/权威surface、91×31 resize/stty、显式KILL既有码-2、进程
退出与PTY master fd归基线均通过；private_processes_exited=true，正式旧PID/starttime/
4项关键原字节前后完全一致。回执保存该官方件自有PID1723179的真实ss监听行，包含
`127.0.0.1%lo:46184`；另有注册腿的临时UDP出站口，不将其称作额外公共服务监听。

官方件私有验证通过后，root实际执行云 prepare，exit0，持久回执
`/root/homeway-rollback/release-v0.3.22-20261028-r1/receipt.json` 为 PREPARED；旧二进制、
state关键常规文件与私有部署资料已持久备份。切换由备份目录中的已审独立worker执行，
激活不重复；先读取该回执再决定恢复动作，不解压state覆盖当前运行态。

本机已审 worker 的 prepare 实际 exit0，持久回执
`~/homeway-rollback/release-v0.3.22-20261028-r1/receipt.json` 为 PREPARED，
activation_started=false；旧件/state/plist/私有恢复资料已备份。此时正式服务仍未切换，
不将备份准备写成部署完成。

## 生产切换续接点

切换前主会话已向用户交付当前Codex会话恢复命令，具体私有会话ID仅保存在用户可见
消息与本机0700备份目录的0600 RESUME.txt，不进入本公开记录或上传云端。
本检查点的云和Mac均为 PREPARED，尚未激活。先云、后Mac，实际结果以各持久receipt
和运行进程/角色/端口/身份校验为准；不得因SSH或当前会话断开而盲目重复activate/bootstrap。
