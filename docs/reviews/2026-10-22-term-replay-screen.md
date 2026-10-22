# term 接入回放屏幕状态修复（2026-10-22）

## 第一道门：技术评审（实现前）

本轮 `reviewer` skill 不在可用目录，依 ROADMAP 评审协议采用实现者方案与评审者检查分列的结构化自评。

| 检查 | 实现者方案 / 评审者结论 |
|---|---|
| 现场边界 | CLI 的实时 DATA 原样写 stdout，并不吞备用屏控制序列。用户现象不能仅凭 Codex 进程名判错；旧规格的主屏用法也不是所有版本的默认行为。 |
| 实证 | 本机 codex-cli 0.163.0 私有 PTY 启动，在 TERM_PROGRAM=Tailcat / Apple_Terminal 下都于输出 byte 69 发 `CSI ?1049h`。没有发送任务、接入或重启生产。 |
| 确定缺陷 | raw attach/reconnect 只发 3J/2J/H 后重放有界字节环；进入备用屏的序列一旦被环覆盖或 replay/epoch 窗口裁掉，客户端会把 TUI 内容画进主屏。完整历史的起点 0 也无条件跳过首换行，可能主动删掉起始切屏序列。 |
| 最小修复 | 字节环维护“已丢弃前缀”的 TermScan 检查点；取回放起点状态时克隆检查点，扫描保留前缀恰好一次。握手在清屏前恢复该起点的主/备用屏，再原样播放历史 h/l。完整历史从 0 原样保留；只有真正截断的窗口才执行残行对齐。 |
| 并发 / 所有权 | 检查点、环和握手计划均在现有服务锁内操作，不增线程/共享锁。初始化序列只发 raw DATA，不入历史、不记入 replayed。不用 ATTACHED 的期末模式反推起点，不依赖 focus nudge 重画。 |
| parser 边界 | 检查点携带跨 append 的 ESC/CSI/OSC 状态；补充 DCS/SOS/PM/APC 字符串跳过与 RIS 模式复位，避免字符串内伪切屏污染检查点。仅恢复屏幕选择，不宣称复原截断字符串/SGR/光标等完整仿真状态。 |
| 本地主屏 | 先选择目标屏再 3J/2J/H；恢复备用屏时不先清掉本地主屏历史。历史内原有切屏序列照常执行，不按 agent 标签强制切屏。 |
| 对齐 / 范围 | 不变更帧格式、原生导出、surface、恢复节奏或生产版本。raw-only 无服务端 VT 时同样从检查点取状态。本批是本地源码修复，不提交、推送、部署。 |

技术门：通过；必须用回归证明截断与完整回放、历史切换顺序、检查点跨块/字符串和 raw-only 路径。

## 第二道门：代码评审与验证

| 评审项 | 结果 |
|---|---|
| 检查点绝对区间 | append 覆盖前先扫描旧环 `[旧 start, min(新 start, 旧 written))`，再扫描本批直接被挤出的前缀；与环头同步前移。单批大于环容量与逐字切断 CSI 均有测试。 |
| 回放起点 | 锁内先算 epoch/replay/残行对齐后的最终起点，再克隆检查点扫描保留前缀。测试特意令起点在备用屏、期末已回主屏，排除误用当前模式；完整历史起点 0 不扫描/删除第一行。 |
| 切屏副作用（评审发现并整改） | 初版用 `1049h/l`，重连时本地已在备用屏，部分终端重复进入可能覆盖保存光标。最终改用只选择屏幕的 `47h/l`，之后沿用 3J/2J/H；历史里的 47/1047/1049 仍原样播放。服务回归追加“本地原已备用屏，重连后退出仍保留主屏内容与 DECSC 槽”。 |
| 计数与并发 | 初始化只写 raw DATA，既有 replayed 仅累计环字节；服务回归将 REPLAY_DONE 的计数与实际回放窗口长度比较。取模式/握手计划仍在同一服务锁内，不增锁或依赖 nudge。 |
| 共享扫描器 | DCS/SOS/PM/APC 载荷跳过与 RIS 清模式同样作用于运行态 TermScan（ATTACHED 模式、焦点 nudge 等消费者）；surface 模式仍以 VT 为真源。scanner 全组通过。 |
| Rust / 直译痕迹 | 复用现有 TermScan 并携带状态；借用环的至多两段字节扫描，不为每次覆盖分配历史副本；无新增 Arc/Mutex、字符串错误、平台 unsafe、接口仿写或包结构映射。 |
| App 边界 | facade/capi 无 TermService/TermScan/OutputRing 消费引用；未改导出、surface 或 App 桥。OHOS capi check 验证编译兼容；未推进 pin、未重编/打包/安装正式核。 |

验证（均本机私有测试，无生产对端）：

- `cargo test -p homeway-core --lib term:: -- --test-threads=1`：**205 passed / 0 failed / 0 ignored**，38.40s。
  包含新增 6 组（ring 3、scan 1、service 2）；SIGPIPE 子进程的同例输出不重复计数。
- `cargo test -p homeway-cli term_cli::tests -- --test-threads=1`：**29 passed / 0 failed / 0 ignored**，1.75s；另集成 target 0 例命中过滤。
- 上述全 term/CLI 之后，代码门将前序 `1049` 改为 `47` 并补重连保存槽断言。**最终树**复跑回放服务 1/0、ring 8/0、scan 7/0，共 **16/0**；未把先前 205 例冒称最终小改后全量重跑。
- 最终 `cargo clippy --workspace --all-targets -- -D warnings`：通过。初跑新测试两处 byte-char-slices 告警已改为字节串并复验，无豁免。
- 最终 `CC_aarch64_unknown_linux_ohos=<DevEco NDK>/llvm/bin/aarch64-unknown-linux-ohos-clang cargo check -p homeway-capi --target aarch64-unknown-linux-ohos`：通过，8.25s。
- 最终 `git diff --check`：通过。
- 主会话跨仓验证：tier 默认 code-map 2 文档 0 不一致、terminal 分册 `--no-strings` 路径门 0 不一致、stale-strings 7 文件 0 陈旧串/16 常量无重复、surface-golden v5/cell2 的共享 golden/hardening/history/input 三组均通过。

代码门：通过。确定修复的是 **attach/自动重连的起始回放** 丢失备用屏进入序列；live 服务回归证明 h/l 字节原样送达，CLI 源码也直接写 stdout。尚未在用户实际终端界面复现/验收原始报告，不把上述构造回归等同于现场闭环。

边界：只恢复主/备用屏选择，不重建被截断的 OSC/DCS 字符串、SGR、光标等完整终端状态；保留既有回放预算、慢腿覆盖与跳过策略，不提供任意并发输出下的原子画面快照。起点为主屏时沿用原有清历史策略；起点为备用屏时先切屏再清屏，保护本地主屏。主屏/inline 应用保留其合法历史，不按程序名强制切屏。未提交、推送、发布或触碰生产。
