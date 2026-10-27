# TUI 启动进度提示完成修复（2026-10-27）

用户反馈进入 TUI 后底栏一直显示“正在连接本机服务”。源码确认：`App::new` 设置该
notice，成功的控制面 Snapshot 与会话列表响应却均未完成它。因此该文案本身不能证明
连接失败；即使后台成功、会话为空，旧前端也会一直显示连接中。本轮仅修改 TUI 前端，
无 core、wire、服务端或 state 变更，不重启后台。

## 技术门

**backend 独立技术门通过，无阻塞项**。最终回读冻结补丁及真实启动脚本后确认：将两个既有进度文案集中为常量，仅在接受成功
事件后、且当前 notice 仍精确等于对应进度时完成，不引入通知系统重构。

- Snapshot 成功仅完成初始“正在连接本机服务”，显示本机服务已连接及选择/创建引导；
  它不代表某台主机的会话读取完成，不能结束“正在读取主机会话”。
- Sessions 成功沿用现有 target 与 query epoch 校验，再完成初始连接或会话读取进度。
  旧目标、旧 epoch 或失败响应均不能完成当前读取。
- 其它错误、在途创建/接入、已确认结果及既有部分成功/刷新警告处理保持原有所有权，
  周期成功刷新不会以通用“就绪”覆盖它们。

## 代码门与验证

UI 独立只读代码评审**通过，无阻塞项**：确认完成点位于成功分支，Sessions 的旧目标/
epoch 检查仍在前，Snapshot 不会误结束 host reading；无新错误类型、无无谓共享状态、
无逐行移植式接口或复制。backend 另行确认脚本检查当前真实 footer，未拿累计输出假过，
且私有 state/零会话/TTY 恢复/finally 清理边界完整。真实 PTY 结果在下节记录。

新增三个回归直接从真实 `App::new` 开始，不预置假的就绪 notice：

- 首次成功 Snapshot + 空 Sessions 经 TestBackend 绘制，连接进度结束、创建引导保留；
  同时覆盖 Sessions 先于 Snapshot 的反序。
- 选主机后的读取进度仅由当前 target/epoch 的成功列表结束；Snapshot 及过期响应不结束它。
- 实际错误事件、在途创建状态及已确认操作结果不被周期 Snapshot/Sessions 成功覆盖。

主会话对冻结源码分别执行（子任务没有启动合并提权命令或等待审批）：

- `cargo test -p homeway-cli tui:: --offline`：**72通过，0失败，exit0**。
- `cargo build -p homeway-cli --offline`：**exit0**。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings`：**exit0**。
- debug SHA256：`d627cbb481661ace247c6e5c38243dbfd04cdf48c151ff105aa979a4804415dd`。

真实启动验收脚本 `tools/tui-startup-smoke.py` 复用现有 PTY/pyte 工具；仅 `/tmp` fresh
state，显式关闭 serve/relay，确认控制面及 local-term socket 与真实 CLI LIST 空列表后
进入 TUI。等待空态引导并跨两轮周期刷新，检查最终 footer 连接/读取进度已完成且保留
创建提示；正常分离后确认 TTY 恢复、守护仍活着且会话仍空，finally 清理自有进程。
该检查使用真实后台与真实 TUI，不将 TestBackend 当作生产连接实测。

主会话对上述同 SHA 调试件实际执行：

```sh
python3 tools/tui-startup-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-startup-previews
```

**exit0，三项 PASS**：真实私有空态成功完成连接/列表读取进度；正常分离恢复 TTY，
守护仍活且会话为零；私有进程/state 已清理。跨两次刷新后的实际 footer 为
“已就绪 · 选择会话，或点击 + 创建 shell · ^B q退出”。主会话查看
`/tmp/homeway-tui-startup-previews/startup-empty.png`，确认空列表、中央创建引导与正确
底栏；同目录保留文本快照。此项是私有实例交互，不连接正式 state。

## 前端更新方案

主会话延续已授权本机 debug 调试决定更新 PATH 前端；子任务不安装、不重启、不连接正式
state。当前正式文件为旧 B3，后台 PID26227。短安装器
`/tmp/tui-startup-status-install.py` 的完整 SHA256：
`18e867a12f0dc96f9745600faef9c000e16e8b3e78d221588cd472c4b946fcf3`。

backend 与 UI 独立复核：相对上一已审 `48c30dff…d4b87564` 仅将旧件校验 SHA 从 e19
改为完整 B3。目标级锁、0700回退目录/旧件、0600回执、同目录暂存 copy/fsync、strict
签名核验、owner501:80/mode755、原子 rename 与目录 fsync、异常原子退文件、前后正式
零参 PID 一致等门全部保留；不调用 homeway/launchctl/kill，不访问 state。沿用旧
`tui-empty-history-*` 备份目录前缀仅为内部命名，真实路径以回执 `backup` 为准；一条
旧件不符的错误文案仍称 e19，但硬校验已为 B3，不影响验收。

精确更新命令由主会话执行：

```sh
/opt/homebrew/opt/python@3.13/bin/python3.13 /tmp/tui-startup-status-install.py --expected-sha d627cbb481661ace247c6e5c38243dbfd04cdf48c151ff105aa979a4804415dd
```

只替换后续启动使用的 PATH 文件；既有后台与已运行 TUI 保持原映像，用户需退出并重新
进入 TUI。无需重启后台或杀会话；无发版/推送/远端变更。

## 前端更新完成

主会话唯一执行上述安装命令，**exit0、receipt SUCCESS**。子任务随后只读复核
`~/homeway-rollback/tui-empty-history-20261027-850a9df4a8ce/receipt.json`：
old 为完整 B3，new/installed 为上述完整 d627；正式 PATH 与 `target/debug/homeway`
的 SHA256 一致。`pid_before=pid_after=26227`、`frontend_only=true`、
`service_restart_requested=false`、`rollback_performed=false`。回退目录0700、旧件
`old`0700、回执0600，旧 B3 保留在该目录。

仅原子更新前端文件，后台未重启、state 未访问、会话未杀；无新增生产 shell/attach/
resize 验收。重新进入 `homeway tui` 使用修复前端。根代理负责 ROADMAP 与本地提交。
