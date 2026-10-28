# 官方 v0.3.23 远端 TUI 冒烟同步诊断（2026-10-28）

root 已正式公开 v0.3.23（源 `bc57b2d`），CI40702069099与五目标
Release40702917361全部成功。Mac 官方二进制 SHA256 为
`66831ecc79c93723846eb6e9b693129a06d2c4380d0ee3516614b4233c11c929`，
架构/严格签名/准确版本及原生归档校验通过；同件私有local23PASS、selection7PASS，
均exit0。此记录只调查工具同步，不修改已发产品、tag、版本、预算或正式部署状态。

## 首次远端失败与证据边界

root 实际 `tools/tui-remote-smoke.py` 首轮 **0PASS、exit1**，日志
`/tmp/homeway-v0.3.23-official/remote-smoke.log`。脚本273行等待HW_REMOTE_SHELL超时，
终态屏幕为tui-f206ddbc的shell prompt、主机表含直连tui-remote，没有marker。
finally已运行并停止脚本自有实例。单张终态不能确定首次早输入/目标误同步发生在哪一拍，
不能据此宣称产品缺陷、环境根因或此前远端全绿。root当时尚未prepare/activate任何目标。

只读检查及非作者纯fake复现已确认工具存在不充分前提：

- 添加主机后的screen_wait("tui-remote")可匹配仍在dialog中的输入值，不能证明dialog
  已关闭或主机快照已到UI；随后固定drain(0.2)不能证明已选择remote。
- terminal_ready不带title时可以命中旧会话；它只要求首行[X]、内容非空、内容不含
  “正在接入会话”，没有排除居中遮罩。
- 非作者直接提取现terminal_ready源码作100×30 fake：pending空frame含title/[X]时False，
  叠居中commands文字后True；旧会话在title=None时True，精确新title时False。无真实PTY、
  socket或进程执行。
- 产品select_session在connected之前已设active/title/[X]，空Surface作为Some绘制，不保证
  总有等待文案；send_input要求connected且online，remote的online由UI快照非Offline
  决定。可见背景/框架不能当作可输入证据。

这些是明确的工具谓词漏洞，与首次失败相容；尚未证明该次实际链路中的精确原因。
不改产品或增加timeout，不盲重跑，不重签或覆盖已发tag。

## 两道门与最小方案

version负责工具作者，deploy负责独立技术门与代码门；dsh既有profile写EPERM按ROADMAP
独立fallback。**技术门已通过**，最小方案仅改tools/tui-remote-smoke.py，sharedhelper、
产品、timeout不变：

- 用当前frame主机面板内精确主机行及物理x2的selected bold确认目标已选；此属性来自
  view的选中专属modifier，主题无关。不用dialog字段或背景常驻主机名作完成证据。
- 明确commands/add-host overlay缺席；n前读目标权威LIST为baseline，n后必须恰有一个
  新session name，禁止用任意旧prompt或标题替代新身份。
- 输入前要求精确新session标题、右侧terminal内未hidden的可见cursor及无overlay。
  Surface仅ready时给cursor，视图仅focusTerminal且非modal时显示；因此框架/遮罩文字
  不能冒充surface就绪。本烟囱使用默认shell，不推广为任意隐藏cursor的应用约定。
- 最终代码门补查发现仅有cursor/title仍漏掉online前提。增量方案将wait_new_shell绑定
  本阶段的expected host：在shell_ready最后一次drain后，再纯读同一frame确认该精确
  host已selected，remote为直连/中继、local为我的电脑；不接受离线、错host或任意target，
  不以n之前的一次检查代替输入前条件。旧2f06752候选未用于真实工具主流程。
- marker命令只发送一次，保留拆分printf避免命令回显本身命中完整marker，同时要求私有
  markerfile精确字节b'HW_REMOTE_SHELL\n'与current screen marker双证据，合并在原25秒
  单窗口，避免两个25秒串成50秒；新session与ready同样合并在原15秒单窗口。没有重复
  输入或延长预算掩盖失败。
- local_roles也按本地权威LIST新name等待；删除host后等真正selected local，不再因
  “我的电脑”常驻背景提前通过。其它功能判据、私有实例生命周期与finally清理保持。

## 最终代码门（完成）

非作者实读最终完整diff，独立核对仅remote脚本+83/-12；shared tui-smoke helper、
产品crates、版本与锁文件无差异。最终SHA256：

`cab07f0578e74a556f60055566bfe4ccaec85b4b49a58f918dede037113cd506`

代码门两项发现均闭合：marker双证合并原25秒窗口而非串成50秒，最终shell就绪还在
最后一次drain后纯读同一frame的expected host/selected/online，local与remote不混用。
保留新name+ready单15秒窗口，首marker只send一次，不重发、不增加既有timeout。
菜单/代理/角色/传输/复制/会话存续与自有生命周期判据未放宽；无新增产品Rust实现，
逐行移植检查不涉及新增Arc/Mutex、字符串错误或产品接口仿写。

最终候选的作者实际42个纯fake例及2个同帧读取顺序例全部通过；非作者独立29个纯pyte/
LIST/barrier/marker例全部通过，覆盖遮罩+旧prompt、缺少新name、错精确标题、hidden或
sidebar cursor、错selected host、离线拒绝、直连/中继及local接纳、terminal_ready的
drain期间online↔offline转换、权威LIST找到新name后缓存，以及6种精确marker字节/
当前屏幕组合。两方计数分列，不累加中间候选25+9等旧计数充当最终验证。

AST parse/compile、SHA、函数scope、sharedhelper完整字节比对及git diff --check均
实际通过。新增诊断只含安全names/count/current screen，不打印host原dict/token，
markerfile只在脚本自有新/tmp目录。所有上述运行均为静态/纯fake，没有真实TUI、PTY、
socket、网络、工具主流程或生产执行，不能据此写成官方远端8流程已通过。

## root实际同件复验入口（待执行）

工具已冻结；root使用原官方66831ecc…3c11c929件单次复验并保留首红日志，命令为：

```sh
python3 tools/tui-remote-smoke.py --bin /tmp/homeway-v0.3.23-official/darwin-arm64/homeway > /tmp/homeway-v0.3.23-official/remote-smoke-sync-r1.log 2>&1
```

若失败，从新日志的具体阶段/target/title/cursor证据定位，不盲重跑或扩大到产品修改；
若通过，只能确认更严同步工具下同官方件流程成立，仍不将首红的精确历史早判节点说成
已因果证明。root独占发布主报告/ROADMAP、网络、真实复验及生产；准备方未运行cargo、
网络或生产、未提交、未修改这两份根记录。真实结果由root完成后追加。

## root同官方件实际结果

root按上述命令单次运行最终工具，**exit0、8 PASS**，新日志
`/tmp/homeway-v0.3.23-official/remote-smoke-sync-r1.log`。添加/选择真实远程主机、远程
shell一次marker及精确文件、默认cwd/cd、SOCKS的65,536B QUIC echo、停止代理/删除
主机而保留远程会话、本地出口/中继启停不杀本地shell、分离恢复TTY和自有进程收尾均过。
使用的仍是首次同一官方arm64二进制66831ecc…3c11c929，产品/sharedhelper/既有timeout
未变。原remote-smoke.log的0PASS不覆盖或改写，结果只证明更严格同步下该官方件流程
成立，不反推首红具体哪一步早判已经取证，也不将purefake计数充当此8个实际流程。
