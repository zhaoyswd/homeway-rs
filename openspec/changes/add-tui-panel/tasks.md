## 1. 立线与准备

- [x] 1.1 ROADMAP.md 登记新功能线（tui-panel，范围/判据/非目标引用本变更与 docs/reviews/TUI-design.md 的 A 案决策），确认与 QUIC 线 U1–U3 触点无冲突；验证 = ROADMAP 出现路线条目且指回 openspec 变更目录
- [x] 1.2 编译探针：homeway-cli 侧构造 core 公开的 `SessionVt` 并调用 `key_options()/encode_key()`（5 行级探针测试）；验证 = 探针通过 ⇒ core 零改动定稿（design D2），tasks 2.1 保持跳过；失败 ⇒ 回头补 core 公开面并改回 design
- [x] 1.3 `crates/homeway-cli/Cargo.toml` 新增 ratatui、crossterm；alacritty_terminal 走 workspace 依赖继承（不重复声明版本）；验证 = `cargo check -p homeway-cli` 通过且 Cargo.lock 无第二个 alacritty_terminal 版本
- [x] 1.4 三目标交叉 check（aarch64-unknown-linux-ohos + musl×2，与 `.github/workflows/ci.yml` 同款命令）；验证 = 三目标 `cargo check -p homeway-cli` 通过，或确定 cfg 门控方案并记入 design
- [x] 1.5 建 `crates/homeway-cli/src/tui/` 模块骨架（app/backend/keys 空实现 + main.rs 子命令行与 usage 串），`homeway tui --help` 可用；验证 = 子命令分发进入模块并打印 usage

## 2. core 面（预期跳过：仅当 1.2 探针失败）

- [ ] 2.1 仅当 1.2 探针失败时：core 补 kitty 模式位换算的最小公开访问器（仅可见性，无行为/wire 变化）；验证 = 单测：模式位 → kitty flags → 编码矩阵覆盖 kitty 开/关两档；探针通过则保持跳过

## 3. 数据通路层（backend）

- [x] 3.1 主机状态面：`snapshot.get` 初值 + `events.subscribe(link, session)` 增量状态机（既有域，含 cursor 回放 + generation 门），daemon 重启/控制面中断后自动重连 + 全量重快照（**不复刻 `status --watch` 的 cursor_stale → exit(1)**）；验证 = 单测：事件序列驱动语义正确 + generation 失配触发全量重取而非退出（r2 N-12③）
- [x] 3.2 本机条目合成：serve 运行状态（非 running 四态 stopping/stopped/failed/absent 统一归不可用）+ 本地 term socket 可连判据；路径 = `<state>/serve/term.sock`（**不复用 term_cli 本地 dial 段的 --state 路径约定**）；验证 = 单测：running+sock 可连 / running+sock 不可连 / 非 running 各态下的条目形态（r2 N-12②）
- [x] 3.3 会话列表轮询器：活动态自适应拍（活动 ≤2s / 非活动 10s，活动态 = 最近 30s 有用户输入或外层焦点在面板）、显示排序 lastActiveMs 降序 + 会话名破平、选中以会话名为键、徽章映射（agent∈{shell,unknown}→无 agent，否则 stateV2 四态）；验证 = 单测：内联 LIST JSON 字样（不动冻结夹具）→ 列表模型与徽章 + 重排后选中稳定（r2 N-7）
- [x] 3.4a 终端腿接入与解析：stream.open → HELLO（无 SURFACE/无 RAW_TERMINAL caps 位，**尾随块携带全零 caps 块以便 client_id 生效**，尺寸取 LIST 当前 cols/rows 对齐）→ ATTACHED/DATA/REPLAY_DONE/STATE 帧解析 → 内嵌 `SessionVt` 喂入；**接入前 LIST 腿满检查 + 稳定 client_id**；验证 = 单测：回放字节流喂入后网格与预期一致 + 腿满拒绝接入（上限常量 8 来源写死，服务端覆盖为已知限制）+ 服务端观察到同一 client_id 走同实例替换（旧腿被 REPLACED）+ HELLO 尺寸等于 LIST 尺寸断言（r2 N-2/N-3/N-4 / r3 B/G）
- [x] 3.4b 四分支断线判定与重拨策略（mock link/session 状态 + LIST 快照序列驱动，不做 clients 自证）：会话不在 LIST ⇒ 终止停拨；链路离去/形态不可用（本机 = socket 重连失败）⇒ 自动重拨（**退避 0.5s 起指数至 10s 上限，链路恢复事件重置**）；腿满 ⇒ 退避 10–30s 待空位不抢占；其余 ⇒ 按链路中断重附；验证 = 单测：四分支谓词各自的可观察行为 + 谓词依序短路 + 失败重拨间隔下界断言（r2 N-1 / r3 E/F）
- [x] 3.5 输入通路：crossterm 键鼠事件 → `SessionVt`/keyenc 编码 → **DATA 字节帧**（不使用 INPUT 帧）；粘贴按会话括号粘贴位包裹（外层 2004 由面板启用）、焦点事件按会话上报位注入（导航焦点期间按失焦处理）；RESIZE 仅交互实例首次交互后下发（镜像实例 NEVER）；验证 = 单测：键事件→字节编码矩阵 + 粘贴/焦点按位分支 + 导航焦点失焦注入 + 镜像实例不产生 RESIZE 帧断言（r2 N-8/N-4 / r3 L）
- [x] 3.6 选中状态文件面：`<state>/cache/tui-selection.json` **临时文件 + rename 原子写**、读方按内容指纹判定变化、运行期解析失败/消失 = 忽略本次读保持当前选中、仅启动走回落链；300ms mtime watch；验证 = 单测：并发写不产生半截读、坏文件运行期忽略/启动期回落、指纹去重（r2 N-5）
- [x] 3.7 读帧→UI 通道：有界通道 + 背压阻塞（不静默丢字节）+ 合帧渲染；验证 = 单测：通道满时读线程阻塞、无字节丢失

## 4. 面板 UI 层

- [x] 4.1 四区布局 + 双焦点模式状态机（面板导航焦点/终端焦点，单一退出组合键，导航键仅导航焦点生效）+ 无选中引导提示；验证 = `cargo test -p homeway-cli tui::app` 焦点切换/透传场景单测
- [x] 4.2 主机列表渲染（确定序：本机优先 + 名称升序同名按加入先后；local 标签与不可用态）；验证 = 3.2 状态机输出驱动渲染的快照单测（r2 N-6②）
- [x] 4.3 会话列表渲染（会话名/标题/徽章四态+无 agent，排序与选中稳定）；验证 = 3.3 模型驱动渲染的快照单测
- [x] 4.4a 终端视图渲染：`SessionVt` 网格 → pane 映射（宽字符头格+尾占位跳过实证/裁剪/颜色与属性映射）；验证 = 单测：网格样例 cell 断言 + 宽字符用例（TestBackend 实证：宽字形头格写符号、尾占位不写——真终端自持两列）
- [x] 4.4b 鼠标事件归属与转发（随 runtime 接线；会话启用鼠标上报则转发，否则面板内处理）
- [x] 4.8 滚轮回看：SessionVt 无滚动偏移面（display_offset 恒 0，`vt.rs:49` 实证）——需 core 最小公开面（design D7 探针先例同路径：滚动访问器 + 按 offset 取行），先在 docs/reviews/TUI-design.md 登记再动 core
- [x] 4.5 状态行渲染：主机名 + direct/relay/unknown 占位/local 形态 + rtt 读数与采样年龄（>120s 标陈旧）+ 流量（1s 差分速率，「应用流」标注）+ 不可得占位；验证 = 单测：rtt/stats 缺省、陈旧采样、本机条目的占位形态（r2 N-10/N-12①）
- [x] 4.6 全局唯一接线：仅用户发起的切换写选中文件（回落不写回）、watch 镜像他实例切换、选中变化（含镜像）计为活动态触发并立即重取 LIST/重附、启动恢复 + 回落链（会话→同主机最近活跃→第一可用主机〔确定序〕→空态；链路中断等可恢复异常不触发主机回落）；验证 = 单测：回落链全分支 + 回落不写回断言 + 可恢复异常留守断言（r2 N-6 / r3 D/N）
- [x] 4.7 终端环境边界：非 TTY 或终端初始化失败（`TERM` 缺失/`dumb`/能力初始化出错）拒绝（非零退出）；可捕获退出路径（正常/错误/可捕获信号/panic）还原终端状态；验证 = 单测：非 TTY 与 TERM=dumb 分支 + panic hook 还原（r2 N-14 / r3 M）

## 5. 集成验收（本地矩阵）

- [ ] 5.1 单实例端到端：`tools/local-rust-exit.sh` 起 Rust 出口，`homeway tui` 全流程——主机列表（远端+本机）、徽章、选中会话交互输入、全屏 TUI 应用（vim/htop）渲染、Alt/Fn 键在终端焦点下透传；验证 = 手测清单落盘（见 5.6 smoke 脚本与清单）
- [ ] 5.2 断线与边界场景（按 3.4b 谓词逐支验证）：kill 隧道进程恢复（链路中断→自动重附）；远端 kill 会话（→停拨+「会话已结束」+回落）；构造腿满后接入（→不接入、呈现腿满提示）；面板腿被让位（→10–30s 退避、空位恢复）；验证 = 手测记录落盘（r2 N-1）
- [ ] 5.3 双实例同显：两个 `homeway tui` 并行——A 切换选中 B ≤1s 镜像、同会话渲染、A 退出 B 不受影响、持久化选中失效回落且回落不写回；验证 = 手测记录落盘（r2 N-6）
- [ ] 5.4 既有客户端共存：面板接入前后 surface 腿（core 测试的 `caps::SURFACE` 客户端构造先例）收到的快照族无全屏风暴（HELLO 尺寸对齐生效）；面板运行中并行 `homeway term attach` 行为正常；构造 8 腿满（先占满再接面板）验证不硬接；验证 = core 面单测断言 + 手测记录落盘（r2 N-4/N-13）
- [ ] 5.5 轮询成本量化（数值门，基线校准后定稿并记录修订）：**活动态**（有输入窗口内）daemon CPU 增量 ≤2% 单核、每秒上下文切换增量 ≤50、QUIC 新建流 ≤35/min（2s LIST 拍 + 重连余量）；**非活动态** QUIC 新建流 ≤8/min（10s 拍）；快照拍走 control.sock 不计入 QUIC 预算（计入 CPU/ctx 项）；验证 = 对照数据落盘 docs/reviews/ 同目录（r2 N-13 / r3 C）
- [x] 5.6 `tools/tui-smoke.sh`（local-rust-exit + 双实例 + 断线注入）与手测清单工件化（docs/reviews/ 下附录）；验证 = 脚本可一键执行并产出记录骨架
- [x] 5.7 本地门全绿：`tools/ci-local.sh`（含 WG 残留门、矩阵冒烟）+ `cargo test --workspace` + 三目标交叉 check；验证 = 退出码 0
- [ ] 5.8 代码评审门（外部评审 + Go 直译痕迹 checklist），记录入 docs/reviews/；高危项整改或登记豁免；验证 = 评审记录文件存在且意见闭环（硬规则 5）
- [ ] 5.9 收尾：README/usage 面补充、`openspec validate --strict` 通过、变更归档、按工作单元分 commit；验证 = 归档后 `openspec/specs/tui-panel/spec.md` 生成且 validate 通过
