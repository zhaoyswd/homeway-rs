## 1. 立线与准备

- [ ] 1.1 ROADMAP.md 登记新功能线（tui-panel，范围/判据/非目标引用本变更与 docs/reviews/TUI-design.md 的 A 案决策），确认与 QUIC 线 U1–U3 触点无冲突；验证 = ROADMAP 出现路线条目且指回 openspec 变更目录
- [ ] 1.2 `crates/homeway-cli/Cargo.toml` 新增 ratatui、crossterm；alacritty_terminal 走 workspace 依赖继承（不重复声明版本）；验证 = `cargo check -p homeway-cli` 通过且 Cargo.lock 无第二个 alacritty_terminal 版本
- [ ] 1.3 三目标交叉 check 前置验证（aarch64-unknown-linux-ohos + musl×2，与 `.github/workflows/ci.yml` 同款命令）；验证 = 三目标 `cargo check -p homeway-cli` 通过，或确定 cfg 门控方案并记入 design
- [ ] 1.4 建 `crates/homeway-cli/src/tui/` 模块骨架（app/backend/keys 空实现 + main.rs 子命令行与 usage 串），`homeway tui --help` 可用；验证 = 子命令分发进入模块并打印 usage

## 2. core 最小公开面（daemon 零改动）

- [ ] 2.1 keyenc/vt 暴露 kitty 模式位换算的最小公开访问器（仅可见性，无行为/wire 变化）；验证 = 单测：模式位 → kitty flags → 编码矩阵覆盖 kitty 开/关两档（评审 D-5）

## 3. 数据通路层（backend）

- [ ] 3.1 主机状态面：`snapshot.get` 初值 + `events.subscribe(link, session)` 增量状态机（既有域，含 cursor 回放 + generation 门），daemon 重启/控制面中断后自动重连 + 全量重快照；验证 = 单测：事件序列驱动语义正确 + generation 失配触发全量重取（评审 R-6/D-7）
- [ ] 3.2 本机条目合成：按 serve 运行状态（serve.status）+ 本地 term socket 可连判据合成；路径 = `<state>/serve/term.sock`（**不复用 term_cli 本地 dial 段的 --state 路径约定**）；验证 = 单测：running+sock 可连 / running+sock 不可连 / stopped 三态下的条目形态（评审 R-8/D-14）
- [ ] 3.3 会话列表轮询器：对选中主机自适应拍（前台 2s/空闲 10s）LIST 短连接，显示排序 lastActiveMs 降序 + 会话名破平，选中以会话名为键，徽章映射（agent∈{shell,unknown}→无 agent，否则 stateV2 四态）；验证 = 单测：内联 LIST JSON 字样（不动冻结夹具）→ 列表模型与徽章 + 重排后选中稳定（评审 R-3/R-4/D-10）
- [ ] 3.4a 终端腿接入与解析：stream.open → HELLO（无 SURFACE/无 RAW_TERMINAL caps）→ ATTACHED/DATA/REPLAY_DONE/STATE 帧解析 → 内嵌 alacritty_terminal 喂入；**接入前 LIST clients 腿满检查**；验证 = 单测：回放字节流喂入后模拟器网格与预期一致 + 腿满时拒绝接入（评审 R-2/D-2）
- [ ] 3.4b 三态断线判定与重拨策略：会话终止（终止通知或 LIST 确认不存在）→ 停拨；裸断 + 腿满/不在 clients → 被腾位 → 长退避不抢占；其余流断 → 链路中断 → 重拔回放；验证 = 单测：三态各自的可观察行为（评审 R-5/D-2）
- [ ] 3.5 输入通路：crossterm 键鼠事件 → 客户端 keyenc 编码 → **DATA 字节帧**（term_cli 同路；不使用 INPUT 帧）；RESIZE 仅交互实例下发（终端焦点交互或自身窗口变化且为活动腿）；验证 = 单测：键事件→字节编码矩阵 + 镜像实例不产生 RESIZE 帧的断言（评审 D-4/D-13）
- [ ] 3.6 选中状态文件面：`<state>/cache/tui-selection.json` 读写（revision 单调 + 主机 id + 会话名，last-writer-wins）+ 300ms mtime watch；文件损坏/缺失视为无持久化选中；验证 = 单测：双写方交替 → 双读方 revision 镜像 ≤1s；损坏文件回落（评审 D-1/D-9）
- [ ] 3.7 读帧→UI 通道：有界通道 + 背压阻塞（不静默丢字节）+ 合帧渲染；验证 = 单测：通道满时读线程阻塞、无字节丢失（评审 D-6）

## 4. 面板 UI 层

- [ ] 4.1 四区布局 + 双焦点模式状态机（面板导航焦点/终端焦点，单一退出组合键，导航键仅导航焦点生效）+ 无选中引导提示；验证 = `cargo test -p homeway-cli tui::app` 焦点切换/透传场景单测（评审 R-1）
- [ ] 4.2 主机列表渲染（名称 + 连接状态 + 本机 local 标签与不可用态）；验证 = 3.2 状态机输出驱动渲染的快照单测
- [ ] 4.3 会话列表渲染（会话名/标题/徽章四态+无 agent，排序与选中稳定）；验证 = 3.3 模型驱动渲染的快照单测
- [ ] 4.4 终端视图渲染：模拟器网格 → pane 映射（宽字符/双列/裁剪/滚轮回看）+ 鼠标事件归属（会话启用鼠标上报则转发，否则滚轮回看）；验证 = 单测：网格样例 cell 断言 + 宽字符用例 + 鼠标归属分支（评审 R-1）
- [ ] 4.5 状态行渲染：主机名 + direct/relay/local 形态 + rtt（端到端探活口径，新鲜度标注）+ 流量（1s 差分速率，「应用流」标注）+ 不可得占位；验证 = 单测：rtt/stats 缺省与本机条目时占位形态（评审 R-8）
- [ ] 4.6 全局唯一接线：本地切换写选中文件、watch 镜像他实例切换、启动恢复 + 回落链（会话消失→同主机最近活跃→第一可用主机→空态）；验证 = 单测：回落链全分支（评审 D-1/R-7）
- [ ] 4.7 终端环境边界：非 TTY/不受支持终端拒绝（非零退出）；任何退出路径（正常/错误/信号/panic）还原终端状态；验证 = 单测：非 TTY 分支 + panic hook 还原（评审 R-9）

## 5. 集成验收（本地矩阵）

- [ ] 5.1 单实例端到端：`tools/local-rust-exit.sh` 起 Rust 出口，`homeway tui` 全流程——主机列表（远端+本机）、徽章、选中会话交互输入、全屏 TUI 应用（vim/htop）渲染、Alt/Fn 键在终端焦点下透传；验证 = 手测清单落盘（见 5.7 smoke 脚本与清单）
- [ ] 5.2 断线与边界三态：kill 隧道进程恢复（链路中断→自动重附）；远端 kill 会话（→停拨+「会话已结束」+回落）；构造腿满后接入（→不触发腾位、呈现腿满提示）；面板腿被腾位（→长退避+提示）；验证 = 手测记录落盘（评审 R-2/R-5/T-4③）
- [ ] 5.3 双实例同显：两个 `homeway tui` 并行——A 切换选中 B ≤1s 镜像、同会话渲染、A 退出 B 不受影响、持久化选中失效回落；验证 = 手测记录落盘（评审 R-7）
- [ ] 5.4 既有客户端共存：面板运行中并行 `homeway term attach`（raw 腿）接入同一会话行为正常；构造 8 腿满（先占满再接面板）验证不挤占；App 行为不变以「面板运行前后 term list/attach 输出一致」间接验证；验证 = 手测记录落盘（评审 T-5）
- [ ] 5.5 轮询成本量化：空转面板 vs 基线（无面板）对照——daemon/出口 CPU、上下文切换、QUIC 流计数；超标则降拍；验证 = 数据落盘 docs/reviews/ 同目录（评审 D-10）
- [ ] 5.6 `tools/tui-smoke.sh`（local-rust-exit + 双实例 + 断线注入）与手测清单工件化（docs/reviews/ 下附录）；验证 = 脚本可一键执行并产出记录骨架（评审 T-6）
- [ ] 5.7 本地门全绿：`tools/ci-local.sh`（含 WG 残留门、矩阵冒烟）+ `cargo test --workspace` + 三目标交叉 check；验证 = 退出码 0（评审 T-4/D-11）
- [ ] 5.8 代码评审门（外部评审 + Go 直译痕迹 checklist），记录入 docs/reviews/；高危项整改或登记豁免；验证 = 评审记录文件存在且意见闭环（硬规则 5/评审 T-1）
- [ ] 5.9 收尾：README/usage 面补充、`openspec validate --strict` 通过、变更归档、按工作单元分 commit；验证 = 归档后 `openspec/specs/tui-panel/spec.md` 生成且 validate 通过（评审 T-4）
