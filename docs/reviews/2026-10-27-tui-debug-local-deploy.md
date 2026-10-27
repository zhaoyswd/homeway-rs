# 本机 TUI debug 部署与双门（2026-10-27）

用户明确要求把当前 debug 部署到本机，并先交付会话恢复指令。这是本次对 AGENTS 官方
Release 产物纪律的特定覆盖：仅本机 PATH `/opt/homebrew/bin/homeway`，不推送、打 tag、
发布或操作阿里云，不把本地 debug 描述为官方 Release。私人会话标识与恢复命令只写入
私有回退目录，不进入本报告。

## 核验事实与技术门

- 新件 `target/debug/homeway` SHA256：
  `e19c014d5c986ed9c653631e0e9e3764d2c393e338d5c1ffa4111bb44fa259ea`；
  `codesign --verify --strict` 通过。版本号仍为 v0.3.21，以完整 SHA 和 observer 能力验收。
- 现役 PATH 件 SHA256：
  `007abf0a5464b77fb17f8a2a09b9c403774d701c7ef69aeea37ee121c0ff3813`，
  与官方 v0.3.21 相符；PID2084，自2026-10-26启动，UID501，owner501:80、mode755。
- 现有 job 为 `gui/501/me.zhaozhe.homeway-exit`；ProgramArguments 仅正式路径一个元素，
  WorkingDirectory 为默认 `~/.config/homeway`，KeepAlive/RunAtLoad=true，保持 plist 原样。
  期望角色 serve=true、UDP41641，relay=false；不改身份、配置或凭证。

**技术门通过**（backend 独立评审）：prepare 与 activation 分离，prepare 只读现役并备份；
切换由独立 launchd 一次性 job 执行，脱离当前 PTY/Codex 工具进程；不依赖 nohup descendants。
按目标路径全局 flock 串行换装，唯一 job/运行目录和 receipt 阶段阻止重复激活。

## 可审查执行器与代码门

私有源脚本：`/tmp/homeway-debug-deploy-ikze83u6/deploy.py`，SHA256：
`6a4c0cccdcf9f31a8c6830d7a84089d51646d49975b0671f39258b5979ed1041`。
AST/import 与 `/tmp` 目录 fsync 检查通过；准备阶段未调用 worker、bootstrap 或 kickstart。
脚本复制到私有持久目录后，launchd 使用绝对 Python 路径及该副本运行。

**代码门通过**（UI 独立评审，backend 对恢复路径交叉复核）：

1. 先备份旧 binary、现有 launchd plist、所有非 cache 普通 state 文件；runtime lock 亦作为
   forensic 副本保留，但不作为换装后字节不变判据。socket 不打包；非 cache symlink 出现即
   中止准备，不跟随到其它目录。state.tar.gz 为持久恢复副本，目录0700，归档/凭证/回执/
   rollout与恢复文本0600，旧件备份0700。多层备份目录、归档及父目录均 fsync。
2. config/key 等关键文件逐字节和长度不变；tokens.jsonl 允许末尾新增，必须保全已捕获
   原始前缀，不把预期台账追加或网络端点变化当失败。只记录长度/hash，不输出凭证。
3. 同目录 O_EXCL 暂存、SHA/签名复核、fsync 后 os.replace，不覆盖正在执行的 inode。
   rename 与“已换件”标记之间屏蔽 TERM/HUP/INT/ALRM，避免已替换却误走切换前失败。
4. 成功路径仅一次 main-job kickstart；验收新 PID、正式零参路径、控制面 PID/generation、
   serve/relay 状态、UDP41641归属新PID、local-term.sock属主及 GREETING surface/observer
   能力（不发送HELLO、不新建/resize生产会话）。PID逐秒稳定10秒后再完整核验 ready/角色。
5. 启动即写持久 RUNNING receipt，阶段原子写+fsync。前向预算180秒，必要回滚另有100秒，
   中断越过普通 ready 重试进入回滚处理。换件前失败不重启；换件后失败原子退旧 binary+
   kickstart并核验，保留当前 state，不自动解压归档覆盖。
6. manual rollback 与 worker 共用目标全局 flock，不留永久 rollback 锁阻止重试。非终态
   显式恢复仅在旧 worker PID 已死、once job不运行、锁后receipt未变且现场只为本轮旧/新
   hash时允许；活 worker或未知 job 状态拒绝。旧件+原PID仍活则只记切换前失败，不重启；
   已 ROLLED_BACK 再次恢复为无副作用退出。

首轮两处问题（rename信号窗口、manual永久锁/回执覆盖）与后续“已死worker非终态恢复”
均已整改，最终独立增量评审签的是上述完整 SHA。没有把静态评审当实际部署成功。

## 准备与激活边界

主会话先正常提权执行（只准备，不切换）：

```sh
/opt/homebrew/opt/python@3.13/bin/python3.13 /tmp/homeway-debug-deploy-ikze83u6/deploy.py prepare
```

持久工件目录固定为：
`~/homeway-rollback/debug-tui-e19c014d-20261027-ikze83u6/`。
其中 `receipt.json` 给出 PREPARED/后续终态，`worker.log` 为执行日志，`manifest.json` 为
脱密 hash/角色清单，`RESUME.txt`/`RECOVERY.txt` 为私人恢复指令；备份和新件也在此。

准备回执与工件校验通过、主会话提交部署前 checkpoint 后，最后独立激活命令为：

```sh
/bin/launchctl bootstrap gui/501 /Users/zhaozhe/homeway-rollback/debug-tui-e19c014d-20261027-ikze83u6/once.plist
```

恢复后先读 receipt、once job状态及正式件 SHA/PID；不得因等待结果而重复 bootstrap 或
kickstart。终态包括 SUCCESS、FAILED_BEFORE_SWITCH、ROLLED_BACK、ROLLBACK_FAILED。
**prepare 已由主会话按正常提权执行，exit0，receipt=PREPARED，activation_started=false。**
随后只读复核持久脚本 SHA 与上述最终签门值相同，目录0700；state归档、plist、manifest、
receipt、rollout、恢复文本、once.plist与worker.log均0600。备份覆盖 config.toml、runtime
lock、serve/key.bin、serve/tokens.jsonl及旧tui-theme，未排除非cache普通文件。
部署前 checkpoint 已由主会话提交为 `a077ba9`，随后主会话唯一一次执行 activation。

## 实际完成记录

持久 receipt 在 **2026-10-27 22:16:10 +08:00**（14:16:10Z）进入 **SUCCESS**，
总耗时 **27.36秒**，`rollback_performed=false`。子任务随后只读复核回执、正式文件 SHA
及一次性 job：job `state=not running`、`runs=1`、`last exit code=0`，未再次激活或重启。

| 检查项 | 实际结果 |
|---|---|
| 正式进程 | PID **2084→17589**，`/opt/homebrew/bin/homeway` 零参、默认 `~/.config/homeway`，10秒窗口PID稳定，末次完整就绪检查仍为同PID。 |
| 正式 binary | SHA256 与冻结 debug **e19c014d…fa259ea** 完整相同；实际复核完整值见本报告上方。版本文本仍为 v0.3.21，不用版本号冒充产物身份。 |
| 控制面/角色/端口 | welcome generation更新，daemon.status.pid=17589；serve=running、relay=stopped；UDP41641由新PID持有。 |
| 本地终端能力 | 新PID持有local-term.sock，被动GREETING `version=1, features=0xff`，包含surface/protocol/observer；`hello_sent=false`。 |
| 关键文件 | config473B、serve/key32B、旧tui-theme5B逐字节与长度均保全；tokens.jsonl原**70313B**前缀SHA不变，验收时只追加**857B**，未覆盖/恢复state。runtime lock不作不变判据。 |
| 回退/恢复 | 未触发回滚；旧官方binary、plist、state.tar.gz、manifest及私人rollout/恢复文本保留在0700回退目录，敏感工件0600。 |

验收范围是正式路径身份、零参同state、控制面/角色/端口、被动本地终端能力与关键文件
保全，**没有在生产state新建shell、attach/resize现有会话或执行完整TUI交互/网络矩阵**。
TUI交互证据仍为上一批同debug件的私有state测试，不能将GREETING检查改称生产交互验收。
只部署本机，未发布/推送/tag，阿里云未操作。一次性job保留已退出记录供审计，没有bootout。
