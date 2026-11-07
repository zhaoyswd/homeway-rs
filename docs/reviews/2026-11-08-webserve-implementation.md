# 独立 WebServe 实施与验证记录（2026-11-08）

状态：最终独立总代码门通过，保留已披露非阻塞边界。r6固定候选完整QA、定向回归、四目标cross与完整ci-local quick均实际通过；任务全部完成，由主会话更新ROADMAP并安排本地中文实现提交，未发布或部署。

## 范围与实现

按已通过技术门的 independent-webserve 实施：独立 webserve.toml、稳定 hww1 凭证/轮换、仅127.0.0.1固定HTTP/WS入口、目标QUIC管理agent、运行中中继完整注册身份快照、现有TUI的独立PTY，以及内嵌固定版本xterm。HTTP生命周期不控制目标agent；目标无需启动HTTP。旧config.toml、联网token、TLS/RPK/ALPN和既有reg4字节不变。Windows所有webserve命令明确unsupported，不初始化state。

所有启动验证使用显式/tmp state及独立端口，不触碰正式默认state、其它仓、系统PATH、发布或部署。构建只产生本仓gitignore目录产物。

## 已完成的验证

| 项目 | 真实结果 | 日志/证据 |
| --- | --- | --- |
| 变更前workspace test/clippy | 均exit0 | `/tmp/homeway-webserve-baseline.YitVDe/` |
| CLI Web协议/凭证/Host/Origin/票据/真实PTY | 16 passed，0 failed，exit0 | `/tmp/homeway-webserve-unit-final.log` |
| 新管理QUIC及核心相关回归 | 最终取消保护后QUIC311、engine32通过；bind16、PTY6及背压重复10次通过 | 传输负责人保留 `/tmp/homeway-web-transport-tests.log`、`/tmp/homeway-web-pty-tests.log`、`/tmp/homeway-web-pty-backpressure-repeat.log`；取消保护最终日志`/tmp/homeway-web-ladder-fixed-{quic,core,clippy}.log`均exit0 |
| workspace all-targets clippy | exit0 | `/tmp/homeway-webserve-final.jAdEq3/clippy.log`及`clippy.exit` |
| OHOS交叉check | 真NDK、4crate、exit0 | `/tmp/homeway-webserve-final.jAdEq3/aarch64-unknown-linux-ohos.log`及`.exit` |
| Linux-musl两架构交叉check | 两项均exit0 | 同目录 `x86_64-unknown-linux-musl.*`、`aarch64-unknown-linux-musl.*` |
| Windows MSVC交叉check | 3crate、RUSTFLAGS=-D warnings、exit0 | 同目录 `x86_64-pc-windows-msvc.*` |
| 词表/异步隔离 | 冻结词表5族26值、隔离11条均exit0 | `/tmp/homeway-webserve-vocab.log`、`/tmp/homeway-webserve-isolation-final.log` |
| Web wire独立oracle/全目录SUMS | QA独立Python规格oracle及SHA验证通过 | `tools/gen-webserve-vectors.py --check`；最终ci-local复验待回填 |

最终取消保护与前端稳定后的交叉复验四项均exit0，日志在`/tmp/homeway-webserve-final-r2-uowq4cpk/{target}.log`和`.exit`，每份包含精确命令及目标专用环境。初轮日志保留作为阶段证据。ci-local r2的workspace测试已通过（core961、QUIC311；后补3项cfg(test)精确注册/回收测试另行通过），all-targets clippy与各独立门通过，ci-local r2最终整体exit0，793.15秒，RRR矩阵316秒；真NDK链接导出20/20、strip后libclientcore.so=2,943,536B，F-100MB双侧SHA/E10等通过。E11保留既有矩阵豁免、DNS为既有不适用跳过，不写作本批全项新证据。

## 实测发现与处置

1. 初版未认证WS超大frame返回空close1005：修成依据库的Capacity/Protocol类型分别发1009/1002；合法大小但未认证发1008。独立私有烟囱已实测1009，后续保持原64KiB判据。
2. a79候选40轮登录关闭在第25轮目标TUI未于15秒内退出：gateway先清relay assoc导致QUIC Close无路。修为后台有界reaper先关闭/flush客户端，再按关闭时cutoff和epoch/精确AssocKey清理；目标实际拆腿撤销management lease、leaf观察撤销关闭pump。UDP-only没有可靠RELEASE，任意网络分区/关包丢失仍有原QUIC idle兜底；不宣称远端瞬时销账。后续00f候选原条件40轮及资源归基线通过。
3. 初版xterm.js文本处理破坏minified字符串：从固定官方npm tarball按原始bytes重新vendor并核SHA512 integrity，资产SHA记录于SOURCES.json，node --check通过。真实浏览器已创建xterm并完成中文/emoji shell输出。
4. Mac鼠标模式下默认无法强制拖选：显式启用macOptionClickForcesSelection并显示Mac Option/其它平台Shift提示；983候选交独立浏览器聚焦验证。脚本CSP仅self（样式面为xterm运行时样式保留unsafe-inline例外），显式浏览器clipboard动作保持。独立截图进一步发现默认Unicode6把🙂按单列绘制，补固定官方@xterm/addon-unicode11 0.8.0并按[官方用法](https://raw.githubusercontent.com/xtermjs/xterm.js/master/addons/addon-unicode11/README.md)启用Unicode11，资产同样原bytes/integrity/license核验；最终浏览器已实测🙂/CJK width2，独立目视完整字形。Web TUI使用HOMEWAY_WEB_TUI=1，native pbcopy/xclip路径被关闭，OSC52只暂存待用户点复制，不自动读写浏览器剪贴板。
5. 首次ci-local在旧ladder用例RegistrationFailed失败（`/tmp/homeway-webserve-final.jAdEq3/ci-local.log`，exit1）；当前与独立基线exact首跑均通过。另一个受控基线实验确认超时旧Proof晚到可把新连接踢成replaced by newer registration，两者失败签名不同，不宣称复现同一唯一根因。经独立技术核批准，只增加已知inactive取消保护，保留wire、原期限、有效准入行为与旧assert；检查后已开始同步表事务不回滚，不宣称全局原子。判据先登记，补丁后统一回归/CI。

## 固定候选与阶段证据

- 00f阶段：`/tmp/homeway-webserve-candidate-zvna2rfz/homeway`，SHA256 `00f41a57319e826d694e53b596d6743a1ab55c8ee17bc55ad5036a6a22175950`；独立烟囱已过40轮、双目标、11秒后交互、轮换/重启/停止入口保留工作会话。浏览器已真实中文/emoji输出，Mac拖选缺陷随后修复。
- 983浏览器聚焦：`/tmp/homeway-webserve-candidate-fcwlsvyy/homeway`，SHA256 `983eb1500ccb3777777ed72de8b8f14f85dfccd0bf6d2bbfd14634dbb5dd7841`。包含Mac Option修复，不声称完成最终全烟囱。
- 最终统一候选：`/tmp/homeway-webserve-final-r2-uowq4cpk/producer/homeway`，SHA256 `85ae9cfab0be18ec1f423e705ee86504060a1b4e9bfba4a353e0f2a4318c9c0a`。包含普通准入取消保护、Unicode11、Mac Option、前端fetch/WS/clipboard异步代际隔离。构建前后产品source SHA一致`639e3e461fac51d6f524dae72b55bf75a39745a9bf4c3dfaaa7887e0c4504627`；完整receipt/source.json同目录。QA从固定文件验收，避免target/debug被其它Cargo重建替换。

## 最终固定候选独立完整验收

QA独立完整`--browser --extended`实际exit0：`/tmp/homeway-web-smoke-final-6pk4zwl3/output.log`、`exit.txt`。私有state `/private/tmp/homeway-web-zai_gyha`内binary-receipt.json核85ae；完整结果见[独立QA记录](2026-11-08-webserve-qa.md)。

- 40轮每轮目标TUI、UDP FD和原生relay assoc均恢复基线；双出口真实HTTPS/WSS、超过11秒交互、HTTP关闭agent可用、轮换/目标退出/relay重启/票据失效均按原判据通过。
- credential-races.json验证8并发离线成功者一致/8在线全部成功且一致、占用实例锁而无control不可离线写；hot-install.json验证先启动出口再首次token热安装，PID/端口不变、无HTTP配置。
- 真实Chrome完成中文/emoji输入执行、宽字符width2、Mac Option鼠标选字、copy按钮写/读回、paste按钮读取后实际执行、resize、断开后工作会话保留；browser-glyph-widths/browser-observations/browser-clipboard-receipt.json及PNG留存。
- 真实A/B目标切换后受控旧paste/output/error/close不能串入新B，覆盖前端代际隔离，不只核静态页面。
- 独立依赖/资产oracle、SUMS、feature闭包、helper语法复核exit0：`/tmp/homeway-web-deps-final-jybj0ne5/report.json`。

候选构建后仅增加4项`#[cfg(test)]`定向回归，产品代码/85ae行为不变；原source.json保留，增量before/after哈希单独记`test-only-delta.json`，不继续声称全文件哈希无漂移。真实TCP-only完整身份/未认证pending不可见/stop清空、真实端口复用旧cutoff不误删、队列32满拒第33/精确epoch源地址时刻均通过：同目录registry-tests-r2.log（2/0）、registry-capacity-test-r2.log（1/0）、test-only-clippy.log（exit0）。初编译命名空间错误保留registry-tests.log而未覆盖。另leaf真实管理8+1饱和/回收后同场普通TLS+reg4可恢复、只有此时regs_accepted=1，`/tmp/homeway-web-management-recovery-test.log`为1/0 exit0，相关all-targets clippy exit0；原311项全QUIC回归在此test-only补充之前已通过。

旧控制面兼容额外实测：私有/tmp UDS按冻结cp-v1握手，依次对85ae的webserve token/start/status回unknown_op；三命令均实际exit1并明确提示升级正在运行的守护，结束后state无auth/config/spawn文件、不启动第二实例，见同目录legacy-control-receipt.json。Windows在本Mac只有真实MSVC crosscheck及统一cfg unsupported分支验证，未声称Windows原生运行。

## 大粘贴前端增量（保持服务端限额）

85ae原浏览器80KiB clipboard实证发送单帧81932B（含xterm bracketed封装12B）并被1009关闭，见`/private/tmp/homeway-web-zy8jesqh/large-paste-reproduction.json`。此阶段并未完成大粘贴要求。修复仅app.js：整次输入先按UTF16明显超限早拒，再核实际UTF8字节+bufferedAmount≤2MiB，超限发送第一frame前明确拒绝；合格输入按≤65536B顺序发送。所有回调仍绑定本轮epoch/ws/ui；异常显式关闭并提示目标可能已收到部分内容，不声称网络故障下输入原子。

新不可变候选`/tmp/homeway-webserve-final-r4-hjxlvg_q/producer/homeway`，SHA256 `798bc6a15462b0e5a2b36a1999d674559ec85c37ff6286a4ee6d6f6337da685d`；build前后source SHA一致`77e27de03d242307da35c0c99165e3234fd4165850c3c85b6e0fbe45e8dbb496`，receipt/source.json同目录。相对85ae唯一产品增量是app.js输入分帧/预算与中断提示，后端40轮和完整CI证据可复用但不冒充新件已跑同一全烟囱；新浏览器数据路径已实际exit0，日志`/tmp/homeway-web-large-browser-r6-67psrult/{output.log,exit.txt}`；私有state`/private/tmp/homeway-web-3wmirzdn/browser-large-paste.json`核最终798bc候选。目标文件81920B、SHA256 `90a9bddfbfeb0353ffd7ac30131751f419765681a40b6afb57cff5b5359e591b`逐字节一致；实际帧65536+16396B（含12B bracketed封装），🙂位于wire偏移65535并跨分片边界完整还原。超限2097195B clipboard载荷发送0帧，目标4秒额外0B、WS保持OPEN；focus O/I及OSC11控制回复hex独立列明，不混入载荷计数。中文/emoji两列、Option选中复制、短paste执行、resize、A/B旧回调隔离及真实工作session保留也在同轮复验通过。

r4新资产CLI聚焦16测试/clippy与OHOS/musl双架构/Windows四项cross均实际exit0，日志/exit在`/tmp/homeway-webserve-final-r4-hjxlvg_q/`；后端ci-local r2整体已exit0。

## 待收口

- 独立代码门完整原文、退出码、整改和最终通过结论。评审包括Rust惯用形态/逐行移植痕迹、锁内零IO、取消和认证边界、HTTP配额/时限、旧设备回归及CAPI依赖闭包。


## r3/r4整改后的r6最终复证

- 固定件`/tmp/homeway-webserve-final-r6-ti3poq6d/producer/homeway`，SHA256 `1c955a412687c7ac85e5f5a83fb96dbe82de60edf34296ebe652a1aba3909085`；build前后source SHA一致`9885b68e3794d1a4a0a95c1d7c385934736a7e01b9e506f07dc86499dc3e9589`。之后只有daemon/tests.rs新增真实控制UDS凭证/恢复错误用例，test-only-delta.json独立记录，不继续声称全文件未变化。
- ea2阶段最后parent_loss抓到真实TUI孤儿：crossterm PTY EOF内部忙环阻碍原主loop复查control。r6只对no_spawn+HOMEWAY_WEB_TUI=1使用单个ProcessInput（poll/read同worker，4事件有界顺序channel），主loop按25ms收事件并复查control；Drop取消/关receiver并75ms有界join，库内EOF不可取消时主路径仍正常析构Guard后结束独立CLI进程，OS终止其输入线程。普通TUI原同步读取入口未改变。真实parent焦点10轮和全烟囱都通过；独立worker2项回归及workspace clippy实际exit0。
- r6完整`--browser --extended`：`/tmp/homeway-web-r6-full-l6ifik1l/{output.log,exit.txt}`真实exit0，state`/private/tmp/homeway-web-vct1cmt4`。原40轮逐轮PTY/UDP FD/native assoc归基线；credential races/锁边界/先出口后首token热装；双目标/11s/rotate/relay重启/票据/目标失败；父死无新守护/监听；真实中文/宽度2、鼠标复制粘贴/resize、80KiB跨码点/SHA与UTF8超额/受控buffer拒绝；F1失败原因不被close抹掉/只读禁输入/重连；2pages、OSC52显式确认、旧异步回调隔离与会话保留均通过。browser-failures/parent-loss/cleanup-receipt.json等保留，cleanup remaining=[]；独立查看两普通PNG与readonly-failure。
- M4落实RawRsp Drop所有权（pending/notify/未知corr/成功send后rx丢弃）、专用SecretFrame、CLI owned Zeroizing、codec消费/移位尾区与读tmp擦除，保持下一coalesced帧。队列成功send再drop rx、下一帧字节/尾零、Host故障readback/install及失败clear均实际通过；通用serde历史分配/内核/终端/浏览器残余不承诺全擦。RecoveryRequired控制wire明确“管理认证已暂停”，不吞作单纯耐久错误。
- 最终cross四项均exit0，精确命令/环境/日志与.exit在r6目录：OHOS4crate12.31s，musl双架构9.77/9.39s，Windows3crate -D warnings5.92s。
- r6完整ci-local quick真实exit0，837.12秒：workspace CAPI4、CLI256+5integration、core976、QUIC314，all-targets clippy、NDK真链接（20/20导出、strip .so=2,943,760B）、隔离11/向量/SUMS/词表/WG门、release构建与RRR矩阵317秒全部完成。F-100MB双侧SHA、E10、speedtest等通过；E11保留原在册独立证据豁免，DNS为原形态不适用，不将其冒充本轮新全项实测。完整日志/exit/receipt在r6目录，原所有失败日志另存未覆盖。
- 独立feature/资产/oracle/SUMS/helper语法复核exit0：`/tmp/homeway-web-deps-r6-yqpfz8uo/report.json`。
- 最终r5外评`/tmp/dsh-review/r5-webserve-remediation.gbb3ld36`进行中；16项r3与9项r4完整原文保留在各review，逐项处置与边界已登记，5.3必须等正式通过。


## 最终独立总代码门与收口

[最终r5评审记录](2026-11-08-webserve-code-review-r5.md)末节已明确：**通过，保留已披露的非阻塞边界**。r3不通过、r4有条件通过及r5外评有条件通过的完整原文均保持，最终结论来自其后独立核实r6完整QA/CI/四cross/候选hash及唯一test-only增量。

独立审计receipt为`/private/tmp/homeway-web-vct1cmt4/final-gate-conditions.json`；固定r6生产者与QA件SHA256均`1c955a412687c7ac85e5f5a83fb96dbe82de60edf34296ebe652a1aba3909085`。225项源文件中只有已登记并由完整CI执行的daemon/tests.rs测试增量，产品源码与候选一致。N1/N3只补文档，脚本self/样式inline的xterm例外、剪贴板全局跨标签共享/不可取消copy均明确。

保留边界不写作已测：B2未额外注入仅control TCP重连而同epoch WSS全链不断；未实测精确2MiB接受/+1拒绝；其它浏览器特定error/typed close顺序可能遮蔽细分文案；Unicode11与Rust宽表不全等价；Windows/Linux等运行面不由crosscheck代表；M4不承诺serde/allocator/内核/终端全链物理清零。r5原文明确这些非阻塞，未新设交付条件。

OpenSpec任务全部完成，5.3只在QA最终明确总门通过后勾选。代码、测试及验证材料已冻结；生产、系统PATH、其它仓均未改，未commit/push/部署，由主会话完成唯一中文实现commit。
