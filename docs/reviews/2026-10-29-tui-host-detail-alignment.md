# TUI 主机第二行与标题/会话第二行对齐（2026-10-29）

## 小设计与用户覆盖旧边界

基线baa78f6。用户明确要求主机item第二行采用与会话item相同的缩进，文字起点与主机
标题名称对齐（不与状态点对齐），并接受内容裁切。这覆盖ce760d7为完整显示offline行
而取消缩进/使用整行宽度的旧选择；字段内容保持不变，不能为保全长行重新扩大侧栏。

最小显示实现：本机exit/relay与远端direct/relay/offline正常详情均加既有两空格缩进，
统一使用标题/会话详情相同的row.x+1绘制区域，使实际文字起点row.x+3。host notice
原本已经在该位置，保持原样。状态点/正文颜色、条目高度、geometry/命中、字段、协议
和输入不变。复用既有布局回归，将完整offline断言改为精确起点与合法右裁切，核暗/浅/
窄屏预览，不新增测试框架或跑网络烟囱。

## 技术评审

非实现者 independent_review 小范围技术门通过，可开工，无阻断。已核标题名称首字与
会话第二行文字均在row.x+3，两项建议与处置：

1. notice已有两空格，不可重复缩进；normal本机/远端各加两格，统一row.x+1及
   width.saturating_sub(1)，字段间单空格保持。**接受**：仅改normal格式与统一Rect。
2. 复用原布局断言核三种文字起点，ASCII按width-3合法右裁；保留边框、颜色、geometry
   与40列约束，不为完整offline扩大侧栏。**接受**：只更新既有sidebar布局回归，没有新test。

## 实现与实际验证

产品仅view.rs：两处normal格式各前加两空格，detail_area统一为原notice已使用的Rect；
notice本身及其它字段/颜色/输入/高度/命中未改变。既有布局回归加入一个session fixture，
逐个暗/浅与本机/direct/relay/offline实例，比较host详情firstnonblank==标题名称首字==
session详情firstnonblank==row.x+3，并将原整行完整显示期望改为三格缩进后的实际右裁切。
原边框、状态点颜色、选择背景及窄窗geometry断言保留。

实际执行：

- rustfmt、`git diff --check`：exit0。
- `cargo test -p homeway-cli tui::view:: --offline`：**exit0，30 passed / 0 failed / 1 ignored**，
  日志`/tmp/homeway-tui-host-detail-alignment-tests.log`。
- `cargo test -p homeway-cli sidebar_layout_preview_artifacts --offline -- --ignored`：
  **exit0，1 passed**，日志`/tmp/homeway-tui-host-detail-alignment-preview.log`。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings`：**exit0**，
  日志`/tmp/homeway-tui-host-detail-alignment-clippy.log`。
- `python3 tools/tui-sidebar-previews.py`：**exit0，11张PNG**，
  日志`/tmp/homeway-tui-host-detail-alignment-render.log`。

预览`/tmp/homeway-tui-sidebar-previews/{many-dark,many-light,small-light}.png`由实现者实际
查看；主会话另独立实际查看同三图：local/remote第二行首字与host名称及session详情
同列，长内容正常右裁，40列不越框。裁切是用户本轮明确接受的显示边界，不改变完整
metadata或字段状态值。没有跑全workspace、QUIC/PTY网络烟囱或新增测试框架。

## 代码评审

非实现者 independent_review 最终代码门通过，**无待修问题**。实读小diff及实际显示
30/0/1 ignored、clippy完成、preview/render日志，并实际查看many-dark/many-light/
small-light三新图：主机第二行与标题名称及会话第二行同列，notice未重复缩进；长字段
按用户授权在原边界裁切，框线/选中色/状态点/面板几何保持，没有输入或协议变化。
没有多余共享状态、锁、无谓拷贝或逐行移植问题，无需追加网络烟囱。

本门为独立静态与证据审查，评审者未自行跑服务或测试。本轮只修改、由主会话更新
ROADMAP并本地中文单元提交；此前v0.3.24发布授权不自动扩展到本轮push/发版/安装部署。
本子任务没有commit/push/tag、构建安装binary或触达生产，未将预览/测试说成PATH已更新。
