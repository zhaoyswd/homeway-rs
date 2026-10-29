# TUI 主机两行状态显示（2026-10-29）

## 范围与设计

标题统一保留状态点与主机名称，移除远端的【状态】前缀。远端正常第二行依次显示
`direct` / `relay` / `offline`、`12ms`（无有效测量为 `--ms`）、`proxy:on` / `proxy:off`，
字段间恰好一个空格，不再显示 IP。本机第二行显示真实出口及中继开关，例如
`exit:on relay:off`；没有新增 daemon 或代理状态。

保留运行期 ready/link 决定状态、非负 RTT/未知测量、proxy 有无的现有含义。状态点颜色、
条目两行高度、选择与鼠标命中、通知覆盖及提示色、面板几何不变。
100 列时侧栏外宽 24、行宽 22；正常第二行使用完整 22 列，取消旧的内边距及两空格缩进，
完整容纳 `offline --ms proxy:off`。通知仍使用既有缩进和绘制区域。40 列及长 RTT 沿现行
边界裁切，不缩写状态或推断开关。只删除 TUI 不再使用的 endpoint 数据，core/RPC 不变。

拟将 HostItem 的自由文本 detail 改为 RTT 类型数据，在显示层统一格式化；同步 README
当前界面说明并更新现有布局/预览夹具。不涉及生产 state、安装、发布、推送或部署。

## 技术评审（实现前）

已读取仓库指定 `/Users/zhaozhe/.agents/skills/reviewer/SKILL.md`，实际尝试 dsh 技术门。
本轮外评持续扩展推演 core 既有语义，未及时产出结论；经主会话明确指令终止，原始
prompt/output/stderr 位于 `/tmp/dsh-review/r1.onfot1x_/`。SIGTERM 后 wrapper 返回 0，
但 output.md 为空，**本次外评未完成，不能认定通过**，不以 stderr 推理内容当正式评审意见。

按 ROADMAP fallback，由非实现者 `independent_review` 只读三处 TUI 源码及本设计执行独立技术门。
技术门通过，无阻断，原始三项建议如下：

1. 正常第二行用整 row 宽度（100 列时 22），notice 保持原 Rect 与缩进；补既有布局断言
   完整 `offline --ms proxy:off` 及边框不被覆盖。
2. 删除 Host.endpoint 时仅去掉 TUI 携带字段/赋值与对应旧断言，保留 host_from_snapshot 的
   ready/via 及 rtt>=0 过滤，HostItem 用 Option<i64>，不复制已格式化字符串。
3. 现有 fixture 替换 IP detail 并分别覆盖 proxy on/off、本机开关；更新 README，保留状态点
   颜色、选择背景和原 geometry 命中。没有新增共享状态/锁或 Rust 直译痕迹风险。

实现者接受全部建议。正常行使用全宽，通知区域独立沿用旧值；删除无用 TUI endpoint，
保留 ready/via/RTT 过滤；扩展既有布局回归并更新预览。

## 实现与实际验证

修改限于 TUI 的 view/mod/backend 三文件及 README：标题统一状态点与清理后主机名，正常
详情改为完整 22 列的英文三字段；本机仅真实出口/中继开关。HostItem 携带 Option<i64> RTT，
backend 不再复制不使用的 endpoint。未改 core、控制协议、行为常量或判据文档。

实际执行结果：

- rustfmt 三处 Rust 文件、`git diff --check`：exit0。
- `cargo test -p homeway-cli tui:: --offline`：**exit0，114 passed / 0 failed / 1 ignored**；
  日志 `/tmp/homeway-tui-host-status-tests.log`。复用并扩展既有布局回归，验证暗/浅两主题下
  本机两种开关组合、direct 的 12ms/on 与 0ms/off、relay 的未知 RTT、offline 的未知 RTT，
  精确内容、原色状态点、选择背景及左右边框；通知仍在原缩进处并使用提示色。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings`：**exit0**；
  日志 `/tmp/homeway-tui-host-status-clippy.log`。
- `cargo test -p homeway-cli sidebar_layout_preview_artifacts --offline -- --ignored`：
  **exit0，1 passed**；日志 `/tmp/homeway-tui-host-status-preview.log`。
- `python3 tools/tui-sidebar-previews.py`：**exit0，生成10张 PNG**；
  日志 `/tmp/homeway-tui-host-status-render.log`。预览目录 `/tmp/homeway-tui-sidebar-previews/`。
  实现者实际查看 `many-dark.png`、`small-light.png`：100×30 的四种主机标题均无状态标签，
  本机 `exit:on relay:off`、远端 `direct 12ms proxy:on` / `relay 48ms proxy:off` /
  `offline --ms proxy:off` 完整显示，离线行恰好到右边框；40×10 保持原侧栏几何与裁切。

主会话另实际查看 `many-dark.png`、`many-light.png`、`small-light.png` 三图：100 列三种
远端状态行完整且无 IP、本机 `exit:on relay:off` 正确、右边框保留；40 列仅沿原边界裁切。

纯显示更改，未跑全 workspace 或真实 QUIC/PTY 烟囱；没有构建安装新 binary、访问生产
state、发布或部署，不声称当前 PATH 二进制已更新。

## 代码评审（实现后）

非实现者 `independent_review` 最终代码门通过，**无高/中/低问题，无待处置项**。
评审者实读四文件 diff 及本记录，核对 TUI 114/0/1 ignored、clippy 完成与预览产物，
实际查看 `many-dark.png` / `small-light.png` 两图，结论如下：

- 100 列 offline 末尾 `proxy:off` 完整且边框保留；标题无状态标签、远端三字段单空格、
  本机 exit/relay 英文开关正确，40 列按既有几何裁切且不覆盖终端。
- notice 沿用原 Rect、两格缩进和提示色；状态点颜色、选择背景保持，ready/via 及 rtt>=0
  过滤逐字保留，endpoint 没有残留引用。
- Option<i64> RTT 减少预先格式化字符串；没有新增锁/Arc、接口仿写、无谓拷贝或逐行移植痕迹。
  README 与既有布局测试、预览均同步，修改符合本单元范围。

评审者明确这是静态代码门，未自行运行服务或测试；实际执行结果来自前述实现者日志。
主会话负责更新 ROADMAP 及本地单元提交，本子任务未提交代码。
