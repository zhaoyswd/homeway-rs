# TUI 快捷指令正文整体居中（2026-10-29）

## 设计（实现前）

现弹窗外框已居中64×16，正文却固定从inner左上角绘制，四行命令后是详情标题/可滚动段落。
本轮保持弹窗位置/尺寸/标题及事件行为，只把可见正文整组居中：用ratatui Line.width真实
Unicode展示宽度取命令与详情自然最大行宽，限在现有inner内，组内保持共同左边界。
以四行命令、可选详情标题和实际可见详情行数算整组高度，水平/垂直居中。长详情按组宽
换行，填满剩余可用高度并保留↑↓滚动；40×10 inner为34×6，原四行命令+标题+一行详情
不加额外留白。极小尺寸全用饱和减法和原裁切，不越框、不panic。

拟启用已锁定ratatui的unstable-rendered-line-info feature，仅用Paragraph.line_count计同
renderer换行高度，避免自写Unicode/单词换行器，不加依赖或升级版本；正文offset限制到实际
换行有效范围。短详情不会拉满弹窗，长详情可撑满可用宽/高；滚动时组位置不随offset跳动。

复用既有显示/modal/历史滚动回归与10图预览，必要时仅补一张长详情预览。不跑真实网络/
PTY烟囱，不改core/协议/连接恢复/输入事件或行为常量，不安装发布部署。

## 技术评审

按 ROADMAP fallback，由非实现者 independent_review 只读 draw_commands、既有modal/滚动
回归及已锁定的 ratatui 声明执行。技术门通过，可开工，无阻断，两项原始建议与处置：

1. width==0/height不足时先饱和或跳过详情，组高只算实际可见内容，40×10保留原6行容量，
   外框/标题/背景不变。**接受**：content.is_empty提前返回，剩余尺寸饱和计算。
2. Paragraph计行与最终渲染使用同一组宽与Wrap{trim:false}，offset限制到实际行数减可见行数；
   短/无详情归零，滚动不改变组位置。**接受**：同一个Paragraph计行后带scroll渲染，高度与
   居中位置不取决于offset；锁定版复用真实renderer，不另写换行器/不改事件路径。

## 实现与实际验证

仅改 CLI Cargo.toml 的现有 ratatui feature、view.rs 显示函数及既有测试/预览。Cargo.lock
没有变化，没有新增包/版本升级。整体宽度是正文最大展示行宽与可用宽度的较小者；整体
高度是4行命令加可选详情标题和实际可见详情行数。所有行保持共同左边界，整块居中；
长详情按现有renderer换行，超过可用高度才滚动，末页不留全空内容。没有输入/modal、
网络恢复、core/协议或行为常量改动。

实际结果：

- rustfmt、`git diff --check`：exit0。
- 首轮 `cargo test -p homeway-cli tui::view:: --offline` 实际 **exit101，28 passed / 2 failed /
  1 ignored**，日志 `/tmp/homeway-tui-command-content-center-tests-initial.log`。两项是既有
  断言仍要求正文紧贴旧top坐标，以及原40段详情在准确计高后已全部可见、不需向下滚动。
  仅更新既有回归：查真实首行位置、核整块上下左右留白均衡/共同左边界，并用真正超出
  viewport的160段详情验证↑↓及END_OF_DETAIL真实末页；不放宽事件/边界断言。
- 修后同显示命令实际 **exit0，30 passed / 0 failed / 1 ignored**，日志
  `/tmp/homeway-tui-command-content-center-tests.log`。覆盖中文真实展示列宽、正常/40×10、
  外框与原背景、原终端geometry/光标/modal输入隔离、↑↓滚动/真实末页、极小窗口不panic。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` 实际 **exit0**，日志
  `/tmp/homeway-tui-command-content-center-clippy.log`。
- `cargo test -p homeway-cli sidebar_layout_preview_artifacts --offline -- --ignored` 实际
  **exit0，1 passed**，日志 `/tmp/homeway-tui-command-content-center-preview.log`。
- `python3 tools/tui-sidebar-previews.py` 实际 **exit0，11张PNG**，日志
  `/tmp/homeway-tui-command-content-center-render.log`。沿用原10例，另补一张长详情例；
  commands普通例使用短详情显示居中效果，长详情独立保留完整滚动压力。

预览目录 `/tmp/homeway-tui-sidebar-previews/`：

- `commands-dark.png` / `commands-light.png`：正常短正文整组双向居中，行内共同左边界。
- `commands-small-light.png`：40×10完整四行命令、详情标题和一行详情，不强留空白。
- `commands-long-light.png`：长详情撑满可用区，边框/标题保留且滚动区可用。

实现者实际查看上述四图；主会话另独立实际查看同四图，确认短内容居中、块内对齐可读，
40×10六行不出框，长详情满可用区的布局合理。本轮为纯显示排版，没有运行全TUI网络/
QUIC或PTY烟囱，没有构建安装新binary、触生产、发布或部署。

## 代码评审（实现后）

非实现者 independent_review 代码门通过，**无高/中/低待修项**。评审者实读 Cargo/view diff、
既有modal/滚动回归更新及最终30/0/1 ignored日志，实际查看 dark、small-light、long-light
三图；结论：短正文整组双向居中且共同左边，长详情满可用区，40×10原六行容量完整，
外框标题/背景不变。Line.width 使用Unicode展示宽，Paragraph同组宽同Wrap计行/渲染，
scroll真实末页clamp、短/无详情归零，极小contentempty先返回。锁定ratatui仅开feature，
没有包版本升级、事件/geometry或共享状态变化，无逐行移植问题。

该门是独立静态/证据审查，评审者未自行运行测试或服务。原28/2与最终30/0均据实保留，
没有将旧断言失败抹作全程一次通过。主会话负责 ROADMAP 与本地中文单元提交，本子任务
没有提交、安装、推送、发版或部署。
