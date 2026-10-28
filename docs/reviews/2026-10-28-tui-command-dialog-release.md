# TUI 对齐与居中指令弹窗：v0.3.23 发布部署（2026-10-28）

用户明确要求“提交发版部署”，授权本轮版本提交、推送 main、在 origin/main 头打 tag、
等待 GitHub Release、下载校验官方产物以及正式部署。版本与部署工件由发布准备子任务
完成；主会话独占真实构建、网络、SSH、生产、ROADMAP 更新及提交。

## 范围与基线

本轮产品提交为 `14cf217`，准备开始时 main 相对 origin/main=`f48ce0b` ahead 1，
工作树干净。会话空态与 footer 对齐标题列，主提示/help/README 统一 Ctrl+B，快捷指令
局部居中最大 64×16，40×10 时 36×8；保持既有模态隔离、执行关闭、详情滚动与 PTY
geometry。不改 core/RPC/wire/NAPI 产品行为、fixture、判据或行为时间常量。v0.3.22 tag
之后已有 core 测试断言/诊断提交，本版 CHANGELOG 明确说“行为变化”，不声称 tag 间
没有任何 core 测试文件差异。

产品单元两门与实际调试件验证见
[`2026-10-28-tui-command-dialog.md`](2026-10-28-tui-command-dialog.md)：完整 TUI
114 passed / 0 failed / 1 ignored，CLI build/clippy exit0，同一调试件私有 PTY 23 PASS。
这些历史专项证据不能替代本轮完整本地门、GitHub CI/Release 或官方同件冒烟。

## 评审与执行分工

已读取仓库指定的 `/Users/zhaozhe/.agents/skills/reviewer/SKILL.md`。外部 dsh 技术门在
本线程实际 exit1，写 `/Users/zhaozhe/.dsh/profiles/headless/cordis.yml` 被 EPERM 拒绝，
原始证据 `/tmp/dsh-review/r1.uzAiaG/` 及产品单元 review 保留。按 ROADMAP 的 fallback，
版本作者 `release_0323/version` 和部署作者 `release_0323/deploy` 相互独立完成设计
技术门与最终代码门；不重试提权、不改 profile。

版本范围仅 workspace 版本、Cargo.lock 四个自有包版本与 CHANGELOG 新节。部署范围仅
已审执行器的新 run 适配，原模板只读；所有实际官方新件 SHA、现役 PID/starttime、配置
及关键字节事实由主会话重新核实后填入。旧记录不作本轮现役证明。私有 RESUME/sessionID
只由主会话写入本机私有配置，不进公开仓、不上传云。

## 发布门与部署边界

静态复核本轮没有改变 `.github/workflows/{ci,release}.yml`。CI 仍包含双平台 workspace
测试、Linux clippy、OHOS/musl 三目标交叉 check、QUIC 隔离和 Windows check/最小烟囱。
Release 仍为 darwin 双架构、Linux musl 双架构、Windows 共五目标，并在构建前强制
tag==origin/main；五目标与 release job 成功后才进入正式换装。

主会话须实际运行 `tools/ci-local.sh` 无参数 quick 门（八步，含 OHOS 真 link 档位、
vectors/SUMS、种子、词表、WG 残留、RRR smoke），并记录真实返回码与失败。不得以既有
专项、静态审查或不相关单例代替完整门，也不扩大本轮范围修改无关时序测试。

正式部署只用 `/tmp/homeway-v0.3.23-official/` 中由本机下载并核验 SHA256SUMS 的 tag
官方件。Linux 件从本机 scp 上传并远端复算；阿里云不直连下载。正式 Mac 与云 daemon
保持 PATH 二进制、零参、默认 state、原 HOME/工作目录；测试显式 `/tmp` 私有 state。
先备份 binary/state，保全身份/config/凭证，凭证台账只允许追加且须保全旧字节前缀。

Mac 前向与回退均沿用原单一 job 的 bootout→严格确认不存在→原 plist bootstrap，
不得回到 kickstart-only，不重签、不改原 plist/state 或系统安全设置。保留独立 worker、
prepare/activate 分离、限时、原子换退、私有恢复文件与持久 receipt。云保留 pidfd 加
starttime 身份防复用、有界清理/回退、真实 `relay/relay.key` 路径，以及 ss 合法 zone
解析下严格的绑定地址/端口判据。LAN71 本轮主会话实际 SSH `true` exit0，已恢复可达，
新增为同 tag 官方件目标；旧离线结论不沿用。该机坚持原 bare client 模式与默认 state，
保留 host/device/master，不创建出口身份或常驻 launchd 项。

## 版本两道门（完成）

部署作者在实施前独立核对 `14cf217` 与前版 CHANGELOG，技术门通过；版本作者随后仅
修改三个文件。部署作者实读最终完整 diff，独立用 tomllib 对比 HEAD，核定 workspace
及 capi/cli/core/quic 四自有包 0.3.22→0.3.23，四 member 仍 `version.workspace=true`，
其它锁数据、包序/包数与依赖均不变，无重解锁。CHANGELOG 新增9行，旧 v0.3.22 起的
历史字节完全保全；新节区分本版新增显示行为和既有保留能力。

非作者对“仅产品行为变化”的末句增量复读后通过最终代码门，无待处置项。
作者与非作者的解析、结构/历史字节检查及 `git diff --check` 均实际 exit0；这些检查
不执行 cargo。逐行移植痕迹 checklist 对本次版本元数据/Python工件不涉及新的 Rust
实现，无新增 Arc/Mutex、字符串错误或仿写接口。

| 文件 | 冻结 SHA256 |
|---|---|
| `Cargo.toml` | `13e7381187caaf3f78abb69d620e5b7be3c0757a63a7103350666868feb63625` |
| `Cargo.lock` | `a4ae1814ff406680cc1aa215b5cc3678a5b425a82ca3892c25505bec94027515` |
| `CHANGELOG.md` | `2f1b68ed92dd7eadd05226beb7b99628028b796a6ba7888daec2c4ab0a07219a` |

## 主会话本轮只读预检

root 实际 fetch/查询确认 origin/main=`f48ce0b`、最新 GitHub Release=v0.3.22。
Mac 零参 daemon PID46739、PPID1、UID501，正式 binary UID501/GID80/mode0755，
版本 v0.3.22，完整 SHA256
`dfe0b99ad21639f95851120644f95d7f85a0f08b70049ec4cf37d02cf9b90795`。
原 plist SHA256 `034bb61e9d60be80f143f4598ebcade25eb23142b0662212590bce95913c2725`；
config473B、serve/key32B、tokens75926B、theme5B 均由 root 重新只读核实。Mac 旧事实
由 root 注入私有配置，prepare/activate 仍须重新核对，不跳过前提检查。

云重新核实唯一零参 PID1723928、starttime3483165197、cwd=/opt/homeway，正式路径
UID0/GID0/mode0755、版本 v0.3.22，磁盘和实际运行映像完整 SHA256 均为
`4416735b71700d3af25662dc847cdba19053f721d244de21559acc49f9f220b5`。
四项 critical 由 root 当轮只读复核后交部署作者绑定，不使用旧部署的历史值：

| 云关键文件 | 大小 | SHA256 |
|---|---:|---|
| `config.toml` | 322 | `b35c634e173aec3a10e503c5b67ab0833b07533ac4d6e9b7b58856d2a16f44a8` |
| `serve/key.bin` | 32 | `c245f4bb15a6ecb49ab9fba3fc68d89109d35d97df5bde2a2f5dd4962b69457e` |
| `relay/relay.key` | 32 | `df7664188129463c820b0852666b6c90967df2f531af24aa374f88fd0b447d5a` |
| `serve/tokens.jsonl` | 990 | `ada0dde61fc0d744acb20497a5323789a09eb3265f92b0f9a1815fbdcc368941` |

LAN71 本轮 root 实际重新核实 macOS26.6.2/arm64、Python3.9.6、HOME=/Users/zhaozhe，
正式 `/usr/local/bin/homeway` 为普通文件、UID501/GID0/mode0755，SHA256
`c373f951a4fd15dfb2c706b494263ea176b9bd9ffc9876f0948091dfbb5b8442`，CLI/control
均 devel。唯一零参 PID83724、PPID1、UID501，generation
`e3a15a171124e7b931ff5bc7a1cebed8`；serve/relay enabled=false/stopped，无永久用户
LaunchAgents 项。该机原 bare client 模式与当前默认 state 作为本轮前提，不沿用旧
v0.3.21 的 PID53015/冗余 --state argv。

local-term.sock 为真实 socket、UID501/GID20/mode0600，lsof 实证归 PID83724；仅被动
GREETING opcode0x0c、len5、version1/features0xff，hello_sent=false，没有创建 PTY。
serve/key.bin、serve/tokens.jsonl、正确的 relay/relay.key 均实际不存在。四项客户端
critical 均0600，本轮原字节如下：

| LAN71 关键文件 | 大小 | SHA256 |
|---|---:|---|
| `config.toml` | 3036 | `5c4aa5f943d89b415496177dc90ba71e1bebf434b494dd522d170cc17eb4a4b6` |
| `client/hosts.json` | 398 | `4e699aee72390146e1b563cf13f3bb77d9b6530e49e4a502dfb55c105c634b87` |
| `client/identity/devtag` | 8 | `702cc3651ac605fd6e763b10a9ff7d30c4b60cd5907ac0ed61206d5706e91cd6` |
| `client/identity/master.key` | 32 | `825dc0fe91c03b250ad57b5c07349524eb95b26aec78551510d676a3d216a463` |

## Mac 与云执行器两道门（完成）

原模板三 SHA 与前版冻结值逐一匹配。非作者先通过最小适配技术门，最终选择不扩 schema、
不改 runpy 全局衔接，仅替换版本/run/路径/旧版本/旧SHA-PID和 smoke 的 helper SHA。
Mac 复用前版成功 r2 的 bootout/严格缺席/bootstrap 路径；云保留修正后的 zone 解析、
`relay/relay.key`、pidfd/starttime 和有界清理/回退。旧模板未覆盖或改写。

新本机目录 `/tmp/homeway-v0.3.23-release-deploy-20261028-r1/` 权限0700；脚本0700，
diff/validation/template JSON 为0600。相对各自旧模板，三个脚本分别为 +6/-6、+12/-12、
+6/-6，版本作者独立实读完整 diff 并逐字核对允许替换后全文一致，代码门通过，无阻断。

| 新目录内脚本 | 冻结 SHA256 |
|---|---|
| `deploy.py` | `abda4296942acb23b92433e3da7d4ca065ed44c48001cbd15575e738a7cd9811` |
| `ali-deploy.py` | `201616886b77c80073bf8d740c9997cc0c075f7d8b0ca588d895c20fa33bab14` |
| `cloud-smoke.py` | `6075a27c6d27707c1cf67e3f281695d828ab89c81cb7c33c5b924dc3ca97340a` |

本轮作者实际完成三个 AST/compile、三个全文替换白名单、三个 AST 控制结构相等检查，
Mac refresh 两路径/严格缺席4结果 mock、云 zone 10个纯 endpoint 例均通过。非作者独立
compile/全文白名单/权限/schema/helper SHA 链核对通过，另抽取函数作 Mac refresh 两路径、
严格缺席5结果和 zone 8例检查均通过。全为本轮受控 mock/纯静态校验，无 launchctl、
socket、Linux 子进程、prepare/worker/rollback 或生产执行，不累加前版 mock 充当本轮实测。

`mac-config.template.json` 保持原8键：source/archive/checksums/archive_sha256/new_sha/
old_sha/old_pid/resume_file；新官方 hash 与私人恢复文件为 null，必须由 root 真实填入。
`cloud-config.template.json` 保持原7键：source/archive/checksums/archive_sha256/new_sha/
critical/old_starttime，fresh critical 与 starttime 已填，新官方 hash 为 null。
准备方未生成 `deployment.json`，占位配置不能通过脚本校验。

Mac 准备命令为 `/opt/homebrew/opt/python@3.13/bin/python3.13` 执行新目录 `deploy.py`
的 `prepare --config <root私有JSON>`。持久备份固定
`/Users/zhaozhe/homeway-rollback/release-v0.3.23-20261028-r1/`，准备与回执复核后唯一激活：

```sh
/bin/launchctl bootstrap gui/501 /Users/zhaozhe/homeway-rollback/release-v0.3.23-20261028-r1/once.plist
```

云 stage 固定 `/tmp/homeway-v0.3.23-cloud-deploy-20261028-r1/`。root 从本机 scp
官方 archive/SHA256SUMS/homeway 和两个云脚本，复核后在该目录生成0600 deployment.json，
先 `/usr/bin/python3 <stage>/cloud-smoke.py`，再 `/usr/bin/python3 <stage>/ali-deploy.py prepare`。
持久准备结果复核后唯一激活：

```sh
/usr/bin/python3 /root/homeway-rollback/release-v0.3.23-20261028-r1/ali-deploy.py activate
```

任何恢复先检查对应持久目录的 receipt/RECOVERY，不盲目重复激活；仅必要时执行该目录
受审脚本的 rollback，不自动解压 state 覆盖当前资料。以上为冻结命令与执行边界，真实
prepare、私有 smoke 或 activate 的结果仍由 root 后续实际执行并登记。

## LAN71 执行器两道门（完成）

部署作者先给出官方产物最小适配设计，版本作者实读旧成功 bare worker 后技术门通过：
复用原零参客户端切换/回退，Python3.9 逐块 SHA，新增两 hash 私有配置与完整官方
provenance（固定 source/archive/SUMS、唯一常规 homeway 成员、归档与二进制 hash、
arm64/codesign/准确新版本关联），prepare 复制配置供持久 worker/spawn/rollback 读取。
fresh 旧 PID83724/zeroarg/c373、四项客户端 critical 和实际角色身份缺席均按本轮事实
绑定；不增加常驻 keeper 或任何出口身份，不携带本机 Codex sessionID。

首个代码候选没有放行、没有执行。独立评审提出并由作者接受三项：

1. ABSENT 应将误路径 relay/key.bin 替换为实际 relay/relay.key，不能保留未经本轮实核
   的旧误路径作 fresh 硬前提。
2. 当前旧 c373 已有 local-term；回退就绪必须按 manifest.old_local_terminal=true 核对
   GREETING 与 socket owner，不能沿用旧 v0.3.21 缺席能力的豁免。准确官方版本的三项
   断言仍仅用于新件，旧件 devel 不会被错误要求为 v0.3.23。
3. 初版静态报告把26个未改函数加1个 class 误计为27个函数。非作者计数断言先失败，
   当次 fixture 尚未开始；修正验证报告口径，不把该次失败记作通过。

三项最终整改均由非作者复读确认，增量代码门通过，无剩余阻断。初版 d0fd49c6…1031dbf
作废，未用于 prepare/activation。新目录
`/tmp/homeway-v0.3.23-lan71-deploy-20261028-r1/` 及脚本0700，其余 diff/validation/template
为0600。相对旧模板 +95/-20；旧模板完整 SHA 末次复核仍
`aaf632a8f83334dad1e9a02db9f93225f3206b3be2a8aa2c9e8c1609f956bb99`，未覆盖旧件。

| LAN71 新工件 | 冻结 SHA256 |
|---|---|
| `deploy.py` | `78009017cc2257ecaa7149a9a2d2802bc671de3a7f70cde3872c9f68318c0ec1` |
| `deployment-config.template.json` | `8e409735b8c8e1ac9496ea3a5a8c8d29a6a0d0ebf6f1694598b7399febd1ddbd` |

作者本轮实际 Python3.9 AST/compile、26个原函数加1个 class 逐字不变、worker 仅 payload
名替换的结构核对通过；私有 fixture 为 config 1通过+3拒绝、provenance 1通过+9拒绝、
critical 1通过+2拒绝，三个新/旧有term/旧无term运行态 mock 最终通过。作者首轮运行态
mock 记录漏 ppid 字段而失败，补齐 harness 的 ppid=1 后三分支通过，产品脚本没有因该
harness 失败再改动。架构/签名/版本命令均 mock，没有执行二进制或生产命令。

非作者独立实际 Python3.9 AST/compile/权限/26函数逐字/原 spawn 链及 worker payload
名差异核对通过；自有临时 fixture 为 provenance 1通过+7拒绝（含签名失败）、config
1通过+3拒绝、critical 1通过+1拒绝。运行态三个能力分支及一项新版本错误拒绝全通过，
old devel 不调用官方版本探针。此前计数断言失败和作者漏 ppid 的 mock 失败均分列，
不把失败轮冒称为这些 fixture 已通过；外部 file/codesign/版本命令均 mock，没有真实
socket、生产或网络执行。

root 在官方校验完成后，把同一 darwin-arm64 homeway、同版 archive、SHA256SUMS 与最终
脚本逐文件 scp 到上述远端新 stage，独占0700目录；不上传 Mac RESUME 或任何私人
sessionID。只填同目录 deployment.json 的 new_sha、archive_sha256 两键（UID501/0600），
模板的 null 不能通过校验。prepare 复制配置到持久目录，worker/spawn/rollback 默认
读取保存件，不依赖原 stage 存续。准备与唯一激活为：

```sh
/usr/bin/python3 /tmp/homeway-v0.3.23-lan71-deploy-20261028-r1/deploy.py prepare
/bin/launchctl bootstrap gui/501 /Users/zhaozhe/homeway-rollback/release-v0.3.23-lan71-20261028-r1/once.plist
```

持久 receipt 位于 `/Users/zhaozhe/homeway-rollback/release-v0.3.23-lan71-20261028-r1/`。
恢复必须先读 receipt 并确认原一次性 worker 已死/未运行，再按同目录 recovery.plist
进行显式恢复，不重复 activation。RESUME 只写该机 receipt 与通用 recovery 说明，
没有本机私人会话标识，准备方未读取 root 的 Mac 私有 RESUME。

最终由 root 另起独立 SSH 验收正式 SHA/v0.3.23、零参/UID501/PPID1、client false角色、
四项身份原字节、正确三项出口身份继续缺席、被动 term、无永久 LaunchAgents 项。
这些远端正式验收均仍须真实执行，静态门不替代远端成功回执。

## 本轮实际结果

版本与三个目标工件两门均完成，准备方未运行 cargo、网络、SSH、生产操作或提交，
未修改 ROADMAP。实际只读预检如上，root 完整 `tools/ci-local.sh` **exit0**，日志
`/tmp/homeway-v0.3.23-ci-local.log` 的终行实际为“本地 CI 全绿（2026-10-28 21:22:58；
档位：quick）”。顶层去除 filtered_out 重复汇总后 workspace **1447 passed / 0 failed /
40 ignored**，core **930/0/6 ignored、31.77秒**；clippy9.48秒、OHOS check8.80秒、
真 link/20导出/QUIC隔离/向量/SUMS/fuzz种子/词表/WG门与 release build 均通过。

RRR smoke **TOTAL316秒**，F 的100MB传输 SHA 双侧一致，E10往返58B，FB-files通过；
既有 E11 独立证据 WARN 与 DNS形态 SKIP 原样保留，不误称这两项也取得独立成功证据。
本轮运行的是 quick，**没有运行 --full**，ignored 用例不计为通过。

GitHub CI/Release、官方下载校验、官方同件私有烟囱及三个目标的真实 prepare/activation/
独立验收结果由主会话实际完成后追加。当前不代表已推送、发布或部署成功。

## 主分支CI阻塞与测试夹具修正

root实际提交并推送发布准备`19876125ee02a6d5dd61e21026d5d72f11955399`。
[CI40696331454](https://github.com/zhaoyswd/homeway-rs/actions/runs/40696331454)首轮Ubuntu
完整测试及全部check/clippy/隔离门成功，Mac CLI190/0，但core未改sentinel用例在KILL
后等OK时UnexpectedEof；保存日志后仅重跑失败Mac job一次，第二轮CLI快照预算夹具
对合法关闭的第二请求unwrap(None)而panic，尚未跑core，不能把它记作sentinel复验绿。
两轮原始日志分别保留，不归因环境，不盲目第三次重跑同提交，也未在红门上打tag。

独立两门后最小修改仅cfg(test)：快照夹具预握手并严格验证两个请求共用总截止时间，
用timeout与服务端真正EOF双判据区分错误逐请求重置；旧KILL失败只加有界opcode诊断，
不猜改产品。根因候选/完整边界及两轮原红/探针首批目录创建失败均据实记录在
[`2026-10-28-tui-snapshot-budget-fixture.md`](2026-10-28-tui-snapshot-budget-fixture.md)。
root实际重新运行workspace **1447/0、40 ignored，exit0**，workspace全targets clippy
exit0；旧关闭用例独立串行16/16通过，仅作该批未复现证据，不宣称KILL候选已修。
版本与部署工件保持冻结，随后另提交test-only修正，重新运行GitHub完整门后才能打tag。

## GitHub通过、公开Release与官方件

root提交并推送test-only修正`bc57b2de607d89903ac5da0e82d722de96c35c2d`。
[CI40702069099](https://github.com/zhaoyswd/homeway-rs/actions/runs/40702069099)实际全success，
含macOS/Ubuntu完整测试、clippy、OHOS/双musl/Windows check与隔离门；未删改原首红记录。
工作区干净、HEAD=origin/main=bc57b2d后唯一创建并推送annotated tag v0.3.23。
[Release40702917361](https://github.com/zhaoyswd/homeway-rs/actions/runs/40702917361)五平台
build及最终release全success，期间未推进main。

公开[v0.3.23](https://github.com/zhaoyswd/homeway-rs/releases/tag/v0.3.23)发布时间
2026-10-28 14:13:37 UTC，非draft/非prerelease，五平台归档及SHA256SUMS附件齐全。
root在Mac下载两实际部署架构的原始归档和SUMS，逐一核SHA、唯一常规homeway成员后
提取；Mac arm64/codesign strict/准确版本、Linux x86_64 static-pie属性均通过。
校验表SHA256为`c6e3218611255c3a64cd6566237aeec0d0e5390ef757f2734146c4fd5a7802b5`。

| 官方目标 | 归档SHA256 | 二进制SHA256 | 大小 |
|---|---|---|---|
| darwin-arm64 | `c97a2cbd7d7ad4dc21b697c86a7ddf55d629780aa6a10d2f6d7772d68c2a86bb` | `66831ecc79c93723846eb6e9b693129a06d2c4380d0ee3516614b4233c11c929` | 8,996,744B |
| linux-amd64 | `9d291282d88799b352c74487f3f2b3e4a26f99225ab5f25d57bb793bb0d8ca0a` | `126cbbddd4d1ff0ceb76c7920685d83d03ca6de254f3d03bbf8670564816b1e9` | 10,909,856B |

同一官方Mac件实际本地TUI **23PASS/exit0**、选区复制 **7PASS/exit0**，原日志与截图在
`/tmp/homeway-v0.3.23-official/`。远程TUI首轮 **0PASS/exit1**，等待HW_REMOTE_SHELL
超时、截图仅有prompt，原remote-smoke.log保留；不能因其它烟囱通过而省略这项失败。
只读/purefake调查证实工具的泛化terminal_ready可把居中弹窗文字或旧prompt误当就绪，
原host阶段也可命中dialog输入文字且依赖固定0.2秒。此为已证谓词漏洞，本轮到底在哪个
节点早判尚无原始逐帧证据；仅修工具同步后再实跑，不猜改或重签已发布产品。

Linux件和Mac件分别由本机scp到云/71私有stage，远端逐一复算二进制/归档/SUMS/已审
脚本SHA一致，71额外strict签名和v0.3.23准确版本通过，私有配置均0600，不传本机
私有RESUME。云官方件真实私有smoke **PASS/exit0、4项**，回执
`/tmp/hw23-linux-smoke-vkodvwgx/receipt.json`；准入/path probe和3轮QUIC共享LIST、
INPUT/surface/resize/KILL(-2)/进程与PTY回收均过。root随后独立读回执确认自有进程全部
退出，正式4项critical前后摘要/大小完整一致且追加0B。此阶段尚未prepare/activation。

## 官方远程复验完成与切换检查点

工具的同步漏洞按独立两门仅修`tools/tui-remote-smoke.py`，最终SHA
`cab07f0578e74a556f60055566bfe4ccaec85b4b49a58f918dede037113cd506`。只用当前frame的
精确目标host/selected/online、权威新session与精确title/输入cursor作屏障，原15/25秒
窗口不增加、首marker只输入一次，并查精确文件字节与当前screen双证据。产品、版本与
共享helper不变，作者pure42+2与非作者pure29例分列，不混作实际运行。

root使用原66831官方件单次实际复验 **8PASS/exit0**，日志
`/tmp/homeway-v0.3.23-official/remote-smoke-sync-r1.log`。原0PASS日志保留，未宣称
原始具体早判节点已因果证明。完整两门与运行证据见
[`2026-10-28-tui-remote-smoke-sync.md`](2026-10-28-tui-remote-smoke-sync.md)。至此同件
私有门为Mac本地23/远程8/选区7 PASS、Linux4 PASS，全部实际exit0。

验证完成后，root实际执行三目标prepare均exit0，并另读持久receipt确认PREPARED、
activation_started=false。二进制与state/身份已备份，三目标均尚未信号/换件：

| 目标 | 持久receipt | 原PID | 官方新SHA前缀 |
|---|---|---|---|
| 本机Mac | `~/homeway-rollback/release-v0.3.23-20261028-r1/receipt.json` | 46739 | 66831ecc |
| 阿里云 | `/root/homeway-rollback/release-v0.3.23-20261028-r1/receipt.json` | 1723928 | 126cbbdd |
| LAN71 | `~/homeway-rollback/release-v0.3.23-lan71-20261028-r1/receipt.json` | 83724 | 66831ecc |

当前Codex恢复命令已向用户交付，本机私有RESUME另存Mac持久备份0600，不进入公开仓
或上传云/71。先阿里云、再71、最后本机；每目标唯一激活，断线先读对应receipt，不盲目
重启或解压state覆盖当前资料。后续真实切换/独立验收结果据实追加。

## 阿里云与LAN71正式切换完成

root推送检查点`e908236`后唯一激活云独立worker1736214。云持久回执实际
**SUCCESS**，2026-10-28 14:54:55.491 UTC、16.962秒，未回退；PID1723928→**1736221**，
starttime3485105265，generation `c7eaefcaba67a3ea14acfcf557ea9321`，正式磁盘及运行映像
为官方Linux完整SHA `126cbbddd4d1ff0ceb76c7920685d83d03ca6de254f3d03bbf8670564816b1e9`。

worker稳定窗/最终就绪后，root另起独立SSH再核准确版本v0.3.23、唯一零参进程及映像/
starttime/cwd/HOME、control PID/generation，serve/relay均running，UDP41641及
UDP/TCP41741齐全。4项critical原字节完整一致、追加0B；local-term.sock root/0600、
ss归新PID所有，被动GREETING v1/features0xff/hello_sent=false，不创建正式PTY。

云独立复核通过后，root唯一bootstrap71持久once.plist，worker30492。71回执实际
**SUCCESS**，14:59:17.399 UTC、17.576秒，未回退；PID83724→**30500**，generation
`41b36f3faa24a55d0dcfe1a5f6ddb0e0`，正式PATH官方arm64完整SHA
`66831ecc79c93723846eb6e9b693129a06d2c4380d0ee3516614b4233c11c929`，版本v0.3.23。
worker检查时ppid仍为30492，这不能冒充已脱离；root随后第二独立SSH实际确认同一PID
**PPID1/UID501**，零参/defaultstate、文件gid0/mode755，control版本及generation一致。
一次性job not running/runs1/exit0、永久LaunchAgents仍无homeway项。

71仅客户端形态保持，serve/relay disabled/stopped；4项config/hosts/devtag/master关键
文件完整SHA/大小不变，正确3项出口/中继身份继续缺席，local-term600归30500所有，
被动GREETING v1/featuresff/hello_sent=false。未在正式state创建shell或做完整TUI交互。
此时本机Mac仍PREPARED，旧46739/v0.3.22未切换；两远端成功不能提前写成三目标完成。
