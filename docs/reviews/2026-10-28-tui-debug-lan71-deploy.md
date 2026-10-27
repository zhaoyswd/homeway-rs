# 当前 debug 部署到 LAN71（2026-10-28）

用户明确要求把当前 debug 也部署到 `192.168.3.71`，本次仅该目标覆盖常规 Release
产物纪律；不推main/tag/Release，不操作阿里云或本机现役。主会话负责远端只读预检、
传输/准备/激活及ROADMAP/提交；子任务只做仓内调查、执行方案/双门与文档，不自行
SSH/SCP、构建或部署，不读取私钥或输出凭证。

## 当前可核事实

本机 `target/debug/homeway` 与已安装PATH同件，完整SHA256：
`c373f951a4fd15dfb2c706b494263ea176b9bd9ffc9876f0948091dfbb5b8442`。
`file`为Mach-O arm64，`otool`最低macOS11.0、系统libiconv/libSystem依赖。该同件本地
TUI81/0、build/clippy及私有local21PASS已在上一工作单元验证，不等于已在71执行验收。

仓内唯一明确71记录在 `docs/DEPLOY-RUST-EXIT.md` §13.4/§13.9补：2026-10-14它是
`zhaozhedeMacBook-Pro-2.local`，原`.8`经DHCP改址，历史darwin-arm64、用户zhaozhe，
PATH为用户可写的`/usr/local/bin/homeway`。当时无launchd、自启动未配，后有按需客户端
守护，serve/relay为false。**这些仅是历史线索，不能当作当前远端事实**，不硬套本机的
`/opt/homebrew/bin`、launchd label、PID、端口或角色。

本机已装aarch64/x86_64 Darwin以及若干Linux/Windows Rust targets，但安装target仅
说明可编译标准库，不证明有真实可部署的跨目标链接工具链。

主会话本轮只读SSH预检已确认：目标确为 `zhaozhedeMacBook-Pro-2`，Darwin arm64 /
macOS26.6.2，UID501、HOME `/Users/zhaozhe`。目标 `/usr/local/bin/homeway` 为普通
非symlink、uid501/gid0/mode755、父目录可写，旧完整SHA为
`007abf0a5464b77fb17f8a2a09b9c403774d701c7ef69aeea37ee121c0ff3813`，版本v0.3.21。
PID53015、PPID1，实际argv为该路径加冗余 `--state /Users/zhaozhe/.config/homeway`，
默认state未迁移；本轮按部署规范以零参新进程保留同一state。未发现homeway永久launchd
plist，gui/501与user/501域可用；SSH PATH未含/usr/local/bin，不能把command-v为空
误判成未安装。

旧进程仅有0600 control.sock，没有local-term/term socket；client角色serve=false /
relay=false，配置端口38241/38341不等于已监听。非cache普通文件只有lock、config.toml、
client/hosts.json、client/identity/devtag、client/identity/master.key。四个客户端关键
文件按字节保全，lock仅备份；没有serve/key或tokens，不由prepare补建。客户端主机表
源码仅持久化id/name/token/addedAt，连接状态/质量另在运行态，endpoint日志在cache。

本轮因此直接复用同一c373 arm64件，不重构建。采用已确认`/usr/bin/python3`3.9.6，
仅将已审执行器摘要函数改逐块SHA兼容，不依赖另一个版本未核的/usr/local/bin/python3。

## 技术分支与待收敛参数

backend独立仓内调查确认以下分支，实际采用哪条以主会话的只读远端预检为准：

- 同为macOS arm64且系统满足最低版本：直接传当前已测c373件，双方复核完整SHA，
  不重编或换成另一件。
- 若为macOS x86_64：复用仓内release工作流已有Apple SDK真cross方法，使用debug
  profile实际编译/链接；目标机核file/签名与版本，再做私有功能验证。不能塞arm64件。
- 若为Linux：必须取得相同源版本、对应arch的真实debug链接产物；仓内既有真musl
  方案依赖完整musl工具链。`cc-check-shim/-nostdlibinc`仅供check，OHOS sysroot也不是
  Linux GNU/musl sysroot，均不可冒充可部署产物。

远端预检应收敛：主机身份/OS/arch、SSH用户UID/GID/HOME、PATH实际目标与symlink/
owner/权限、当前binary SHA/版本、运行PID/argv与控制面generation、默认state、实际
角色/端口、已有启动job与所属domain、可用Python路径/版本。只读字段过滤凭证，不
输出config/token/key原文；若无进程，不用可能自动拉起守护的CLI探针假称只读。

已审原执行器使用`hashlib.file_digest`，需要Python≥3.11；本轮按已确认3.9.6作必要
摘要兼容，不硬套本机解释器路径，不为此次部署安装工具链/运行时或另造框架。

## 最小切换原则

1. 先将payload与已审最小适配脚本传到远端受限暂存目录，核SHA/架构/签名，明确旧件/
   默认state/启动方式与角色；完成可审查计划后，prepare与activation分离。
2. 受限备份旧binary、全部非cache普通state与原启动配置，目录0700、敏感工件0600；
   身份/config/凭证保留，tokens若为追加台账按前缀保全，不创建原本不存在的出口身份。
3. 复用同目录暂存+fsync+原子rename，不覆盖运行中的inode；仅按远端原有形态重启一次，
   不运行install.sh下载官方件或额外建第二个自启动，不擅自启用出口/中继。
4. 执行需脱离SSH生命周期，启动即持久RUNNING receipt，唯一job/目标锁/阶段阻止重复；
   网络断开先查回执，不盲目重传/重启。成功核PATH SHA、实际PID/argv、默认state、
   角色/端口/控制面、已有term能力与critical字节，稳定窗口后复核。
5. 失败仅原子退旧件及按原形态恢复，不自动解压state覆盖当前资料。保留持久receipt/log/
   manifest/恢复说明；不新建生产PTY或把被动能力检查写成完整生产TUI交互。

当前远端身份/启动方式已收敛，backend正在本地/tmp按已审6a4结构制作最小差异执行器，
尚未上传或执行prepare/activation。裸守护按一次TERM有界退出、start_new_session零参
拉起，stdio写0600持久日志；一次性job非keeper、不留永久自启，正常退出后由主会话核
新PID的PPID1。新件才要求local-term/observer，旧件回退不强求原本不存在的能力。
技术/代码最终门与真实执行证据在具体脚本核定后如实补充。

## 冻结执行器与双门

已审6a4结构按远端事实收敛为私有源脚本
`/tmp/homeway-lan71-deploy-20261028/deploy.py`，完整SHA256：
`aaf632a8f83334dad1e9a02db9f93225f3206b3be2a8aa2c9e8c1609f956bb99`。
payload固定为同目录`homeway.debug`，即上述c373完整SHA。不重编产品、不改源代码，
也不在71安装Python；执行器以Python3.9语法静态解析通过（子任务未import或执行脚本）。

**技术门通过**：主实现者独立回读backend作者的完整执行器，核对scope/参数、旧进程
pid+uid+启动时间/argv、4客户端critical与角色身份缺席、锁/备份/权限与原子替换、
bare启动/回退及新旧能力边界。**UI独立代码门通过，无剩余阻塞**，最终SHA一致。

评审发现一项实际高优先问题并完整整改：worker在spawn交接期间阻塞信号，回退时还
临时忽略信号；子进程不能把这些保护继承到正式daemon。最终共享spawn入口按
“持久原子登记本PID/启动时间 → 关闭继承锁 → INT/TERM/HUP/ALRM恢复SIG_DFL →
SIG_UNBLOCK → exec正式零参”执行，正向/回退均覆盖。`ps -ww`避免长argv被截断。
短Python exec入口与最终daemon是同一PID，不是双fork或新增永久服务；继承flock只用于
保证硬死worker窗口也能识别已启动子进程，登记完成后daemon不持部署锁。交接未完成
且子进程仍在时拒绝继续换件/杀进程，防止未知归属。唯一job/receipt/execute锁与死worker
显式恢复门保留；取得安装锁之前的安全拒绝不改service，仍由job日志诊断，不能误当
PREPARED可重复激活。

prepare带umask077，完整备份所有非cache普通state（含取证lock）、旧/新件与原argv/
客户端baseline；没有出口身份文件则要求继续缺席。归档/manifest/恢复文案0600，目录
0700、备份binary0700；不复制本机Codex会话/私钥到71。同目录stage继承gid0，只有元数据
不符时才fchown，prepare仅用本轮自有probe核实际能力并删除它，不假定用户属于wheel。

新件只按既有角色false拉起，不监听38241/38341、不创建生产PTY；要求控制面新PID/
generation、local-term.sock600由新PID持有及被动GREETING version1/featuresff。旧件
回退只验原客户端/控制面能力，不要求本不存在的local-term。前向预算180秒、必要回退
另100秒，TERM退净后才起新进程；稳定10秒再完整复核。最终worker退出后主会话另核
daemon仍活且PPID1，不能把“start_new_session”配置当成已验证结果。

固定持久目录 `~/homeway-rollback/debug-tui-lan71-20261028-r1/`，其中`receipt.json`为
结果、`worker.log`为日志，`manifest.json`记录baseline，`RESUME.txt/RECOVERY.txt`给
操作恢复说明；原state归档不自动解压覆盖。

主会话已在远端独占创建0700暂存目录并开始上传已核payload，尚未切换现役；子任务
所有行为仍仅本地只读调查/脚本制作/静态评审，无SSH/SCP或生产动作。最终脚本通过
双门后才由主会话上传，核SHA后单独执行远端prepare：

```sh
/usr/bin/python3 /tmp/homeway-lan71-deploy-20261028/deploy.py prepare
```

PREPARED工件/权限/哈希复核并提交checkpoint后，主会话唯一activation：

```sh
/bin/launchctl bootstrap gui/501 /Users/zhaozhe/homeway-rollback/debug-tui-lan71-20261028-r1/once.plist
```

所有SSH/SCP与远端写入按正常权限流程由主会话执行，不绕沙箱、不过早重复激活。

主会话实际暂存验收已完成：payload SCP **exit0**，远端完整SHA=c373、Mach-O arm64，
`codesign --verify --strict` **exit0**；payload `--version` **exit0**，真实文案为
`homeway (devel)`，`--help` **exit0**且含tui。新件不是按v0.3.21文案识别，冻结执行器
无CLI版本字符串断言，严格按完整SHA；GREETING的version1仅指协议。此时正式PATH仍为
旧007，root开始上传已签脚本/执行prepare，尚未取得prepare结果，不提前登记成功。

主会话随后已唯一上传最终签核脚本，远端先核完整SHA=aaf632，再设0600并用
`/usr/bin/python3`执行prepare，实际 **exit0、receipt PREPARED**。目标父目录本轮
权限probe已通过，旧PID53015及正式旧007文件未切换/重启。固定结果指针为上文所列
`receipt.json`，激活仍为同目录`once.plist`；此阶段不重复prepare或提前激活。
远端备份工件/manifest的后续只读复核、ROADMAP与checkpoint由主会话完成；子任务只
追加报告，不访问71、不再改脚本/版本或运行测试。
