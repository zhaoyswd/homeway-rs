# TUI 主机留白与真实目录修复（2026-10-28）

用户纠正：主机列表需要在实际条目之后留出一个两行 item 的内容空白，边框另算；
会话次行字段之间应恰一个空格，且默认 shell 的初始目录与 `cd` 后目录应真实显示。
本轮不再把“可显式发 OSC7”或缺测留空作为普通 shell 的完成证据。

## 根因与技术门

旧主机高度 `2 * (host_count + 1)` 的额外两行实际被上下边框占用，没有内容留白。
修为 `min(host_count * 2 + 2 + 2, body_height / 2)`：条目、空白、边框分别计数，
一台6行、两台8行，半屏上限优先。共享边框/两行 item 命中策略不变；剩余空白点击
无动作，窄窗口仍只显示完整条目。真实 local 鼠标脚本首会话第二行 SGR 行号同步为8。

目录缺测的链路已由主实现者与 backend 独立核实：

- PTY 默认启动登录 shell，`cmd.cwd(HOME)` 只决定启动动作，不是后续可持续目录采样。
- `TermScan::pwd_path` 仅解析 OSC7；旧 LIST 仅在有 VT 且 OSC7 非空时输出可选 `cwd`。
- TUI 只对 LIST.cwd 取 basename，没有可靠的远端 PID→目录来源。因此前端排版或猜标题
  无法修复；之前显式发送 OSC7 的烟囱只证明上报链路，不能证明默认 shell。

主会话对现役659f控制件实际执行
`python3 tools/tui-cwd-smoke.py --bin /opt/homebrew/bin/homeway`，仅私有 `/tmp` state。
脚本捕获默认 shell 原始回放未见 OSC7，取得真实初始 PWD `/Users/zhaozhe` 后，等待
LIST.cwd 8秒仍缺测，**预期红、exit1**。未执行到 cd/后续 OSC7 阶段；这不是新件回归
失败。finally 回收自有PTY/守护，随后只读进程核对无 `homeway-tui-cwd-*` 私有实例残留。

主会话确认必要 core 范围后采用最小方案：复用既有1秒 sampler，在服务锁外读取本会话
shell PID 的真实 OS cwd；macOS 为 libc/libproc `PROC_PIDVNODEPATHINFO`，Linux 为
`/proc/<pid>/cwd`。按名字、会话generation、PID与存活状态校验后缓存，LIST只读缓存，
不做进程 I/O；采样不依赖可选 agent 检测开关。有效 OSC7 继续优先，缺测时使用本代最近
成功的 OS 目录；仓内没有可复用的持久 launch cwd hint，不伪造常驻 HOME 回退。
raw-only 无 VT 也可返回真实目录。已有可选 JSON 字段/帧/op/版本不变，行为变化在
`docs/INTEROP-CRITERIA.md` 的判据变更记录登记。

前端次行固定三项：`状态` + 一个 ASCII 空格 + `类型` + 一个 ASCII 空格 + `目录basename`。
当前目录不可用时明确显示 `—`，不删字段；保留目录自身合法空格，根目录显示 `/`。
现有类型/状态识别、dot颜色/选中背景与承载会话保护不变。

## 独立双门与验证边界

backend 负责 native cwd/服务缓存与核心回归，主实现者负责 TUI/工具，并独立回读 core 的
锁外采样与本代回写路径；UI 对两者做独立只读代码评审。**技术交叉门与最终独立代码门
通过，无阻塞项**：macOS
完整返回大小、零初始化和有界NUL校验；Linux只读软链接；LIST不探测OS；gen/pid/done/
stopped门及检测关闭/raw-only路径符合授权。没有多余共享状态、无新依赖或字符串错误，
无逐行移植式接口。核心两项必要回归包括自有进程的真实初始/cd目录与服务缓存/优先级：
gen/pid错配动态验证，done/stopped与同名代隔离另由源码核对，不虚称新增了真重建场景。
backend 另行反向核对其未参与编写的 view/README/私有烟囱：两行内容空白、单空格三字段、
合法目录空格/root basename、default-shell无OSC7观测与新鼠标行号均通过。

主会话对冻结源码已实际分别执行：

- `cargo test -p homeway-core cwd --lib --offline`：**2通过，0失败，exit0**。
- `cargo test -p homeway-cli tui:: --offline`：**76通过，0失败，1 ignored，exit0**。
- `cargo test -p homeway-core term:: --lib --offline`：**219通过，0失败，exit0**。
- `cargo build -p homeway-cli --offline`：**exit0**。
- `cargo clippy -p homeway-core -p homeway-cli --all-targets --offline -- -D warnings`：
  **exit0**。
- 新 debug SHA256：`fbc2419f81daaec041cc336b7c338090ba2fcfdea8ec448f0fe16a21b0a4a025`。

主会话对上述同 SHA 实际执行 cwd 烟囱：normal **4项PASS、exit0**；raw-only
**4项PASS、exit0**，初始目录、含空格cd、最后显式OSC7优先及私有收尾均完成。
detect-off 首轮初始目录通过、cd后的真实 LIST 路径也通过，随后要求完整显示
`unknown unknown cwd project` 的断言超时，**exit1**。该行含缩进29列，100列窗口的
侧栏文本区域仅27列，正常右裁两列；这不是服务端cwd丢失。保留本轮失败事实，工具
新增失败时当前屏幕/PNG输出和可选 `--project-name`，默认改用可容纳的合法空格名
`a b`；不改Rust或二进制裁剪行为。同 SHA detect-off复核结果后续单列，不抹掉首轮。

后续同 SHA 实际复核已完成：detect-off 短合法空格名 `a b` **4项PASS、exit0**；另显式
保留长名 `--project-name 'cwd project'` 控制 **exit1** 并输出当前屏幕，证实真实内容
正是 `unknown unknown cwd proje`，最后两列被窄侧栏裁切，LIST真值在之前已匹配。
实屏与文本位于 `/tmp/homeway-tui-cwd-clipping-control/cwd-failure.{png,txt}`。不改变
产品裁切规则，也不把首轮错误断言抹成单轮全绿。

raw-only 的 OSC7 优先项不是共用PASS文字：`preferred()` 位于所有模式共用路径，
真实等待 LIST.cwd 等于 `/osc-priority` 后才输出通过。`pump_loop` 始终喂 TermScan，
其后才按是否有VT选择仿真；因此无VT仍能解析OSC7，本轮LIST不再用VT gate屏蔽它。
raw-only初始/cd/OSC7均为实际LIST证据，不宣称存在TUI surface。

同件 remote 烟囱 **8项PASS、exit0**，包含默认远端初始目录、普通cd后LIST与TUI同步、
合法目录空格；原真实QUIC/SOCKS与删host保留会话验证继续通过，私有资源收尾。

裸 Linux/Windows cross-check 首次分别缺 musl gcc、Windows C头 `assert.h`，均停在
ring C环境，未进入本轮Rust cfg检查。backend 找到仓内原有成功的 `cc-check-shim`
配方，由主会话复用 NDK clang/headers 做 check-only，不临时安装工具链、不把它声称
为目标平台真实链接或运行。最终平台/剩余实际烟囱结果在取得真实exit后回填。
Linux 已按仓内 NDK clang + cc-check-shim 配方 **check exit0**；Windows 同配方也
**check exit0，5.48秒**。这两项仅目标cfg编译，未宣称Linux/Windows原生运行。
主会话已查看normal与detect-off的真实私有PNG，确认一台主机后的两行内容留白、
单空格三字段与真实目录显示正确；产物位于 `/tmp/homeway-tui-cwd-previews/` 与
`/tmp/homeway-tui-cwd-detectoff-previews/` 的 `cwd-no-osc7.{png,txt}`。

新增 `tools/tui-cwd-smoke.py` 不修改 shell rc 或 prompt，创建真实默认登录 shell；
独立 raw CLI 捕获原始回放并验证无 OSC7，普通用户层 printf 将 shell 自己的 PWD 写入
私有文件，对照物理规范化路径与服务 LIST，再验证 TUI basename；随后 cd 到含空格的
私有目录，最后才显式发 OSC7 验证其优先级。`--detect-off` 和 `--raw-only` 复用同一
场景。macOS `/tmp`→`/private/tmp` 的逻辑/物理路径差别在测试 oracle 中规范化，不影响
服务实际数据。嵌套finally保证任一关闭动作出错仍收尾自有守护。

remote 烟囱也增加默认远端 shell 的初始目录/cd闭环：私有出口服务 LIST 真值与通过
QUIC取得的TUI目录一起验证，保留原代理与删host不杀会话断言。原local烟囱继续覆盖
真实 `ls`/Unicode/TAB、单会话第二行鼠标命中、原OSC7路径、分离/重接。
主会话最终对同SHA再执行local烟囱 **18项PASS、exit0**：新主机高度下SGR row8真实
命中第二行并继续同shell输入、十个真实`ls`、OSC7旧链路、resize、分离/重接、右键关闭
对应会话和TTY恢复全部完成，私有资源收尾。

本轮需新版后台才能为现有无OSC7会话补目录，不能仅换前端生效。生产部署/恢复准备、
权限审批与重启均由主会话在最终验收后安排；子任务不碰正式 state、不安装/重启，
不启动提权 cargo 或真实烟囱。无发布/推送或远端生产变更。

## 后置部署准备

复用已审 `6a4c0cccdcf9f31a8c6830d7a84089d51646d49975b0671f39258b5979ed1041`
完整执行器，仅替换5项全局常量：NEW为上述fbc完整SHA、OLD为现役659f完整SHA、
OLD_PID26227、独立RUN_ID/worker label。私有源脚本
`/tmp/homeway-tui-cwd-deploy-20261028/deploy.py`，SHA256：
`ac96917d2ea863867091bf671aec4523e62831acd32b6ea1d4ba09a9ccd50cfd`。
UI独立增量代码门已核对逐字差异、AST、payload哈希及所有Label/回退路径，**通过**。
backend独立部署技术门同样**通过，无阻塞**：逐字节验证仅五常量变化，原独立worker、
备份/critical前缀、全局锁/原子回退与dead-worker显式恢复结构完全继承，功能/生产
readiness证据边界明确。

固定持久目录 `~/homeway-rollback/tui-host-padding-cwd-20261028-r1/`：`receipt.json`
为结果，`worker.log`为日志，`RESUME.txt`为私人会话恢复指令，不在公开仓记录其内容。
保留原守护plist、当前/新binary、全部非cache普通state文件与0600归档；lock可进入
备份但不参与保全判定，tokens只允许原前缀完整后追加。prepare仅备份，不替换/重启。
之后独立一次性launchd worker原子换件、单次正常kickstart，校验新PID/正式路径SHA/
默认state/角色端口/控制面/greeting、critical原字节与tokens前缀，并稳定10秒复验。
失败自动原子回旧件并重启，或按已审的死worker显式恢复入口继续；不覆盖当前state。

精确准备命令由主会话在验收后单独执行：

```sh
/opt/homebrew/opt/python@3.13/bin/python3.13 /tmp/homeway-tui-cwd-deploy-20261028/deploy.py prepare
```

主会话先交付恢复命令、核查PREPARED工件并提交checkpoint后，才执行唯一activation：

```sh
/bin/launchctl bootstrap gui/501 /Users/zhaozhe/homeway-rollback/tui-host-padding-cwd-20261028-r1/once.plist
```

cwd没有新增协议featurebit，生产不创建PTY或注入命令验证目录；以新PID加载同SHA已验证
工件关联私有真实cwd功能证据，greeting检查只证明协议/observer能力，不宣称完整生产
TUI/cwd交互实测。

主会话已唯一执行上述prepare，**exit0、receipt PREPARED**，时间
2026-10-28 00:54:30 +08:00（16:54:30Z），`activation_started=false`、旧PID26227。
子任务随后只读复核：持久deploy.py仍为已审ac969完整SHA，旧659f/新fbc备份完整SHA
吻合；回退目录及两binary0700，receipt/manifest/state归档/RESUME/once.plist0600。
state-files逐文件大小/摘要与manifest一致，原plist摘要也一致；lock纳入备份而不作为
不变关键文件，config/key/token旧前缀等保全证据已写manifest。此时尚未activation，
未替换正式文件或重启；主会话将先提交产品与部署前checkpoint，再唯一激活。
