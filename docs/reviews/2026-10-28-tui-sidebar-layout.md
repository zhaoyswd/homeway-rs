# TUI 侧栏布局调整（2026-10-28）

本轮按用户六项调整，仅改 TUI 显示/鼠标几何与对应验收工具；不改 core、wire、服务端、
检测引擎、连接或 state。主会话负责 ROADMAP、本地提交及获授权的本机前端更新，子任务
不安装、不重启、不连接生产 state。

## 技术门

**backend 独立技术门通过，无阻塞项。** 源码先确认原侧栏为 `width / 3`、clamp20..38，
左右主机会话各半高，条目步长3行。最终方案与主会话核准一致：

1. 主机/会话标题只显示文字与添加按钮，去掉数量。
2. 主机总面板高度（含标题及上下边框）严格为
   `min((host_count + 1) * 2, (terminal_height - 1) / 2)`，host_count 包含我的电脑。
   一台4行、两台6行；下方会话共用一条边框，获得其余高度。两行条目连续排列，奇数
   内高的最后一行留白，不绘制半条、也不命中下一条；选中项按完整可见条数滚入。
3. 状态 dot 在列表内侧增加一列 padding，与面板标题的首字同为 x+2；两行条目使用
   共同 `ITEM_HEIGHT`，渲染、滚动 offset、鼠标命中和右键菜单目标一致。选中背景保留。
4. 本地主机首行只显示 dot 与“我的电脑”，其它主机继续显示原直连/中继/离线标签及
   IP/延迟/代理信息。40列窗口下本地详情省略冒号，让出口/中继两个开关仍可辨。
5. 会话首行显示 dot 与原始标题；去掉状态标签/装饰方括号，保留用户标题自身字符。
   既有承载会话保护标记前置为 `·自身`，不加括号，禁选/禁关逻辑不变。次行依次显示
   working/idle/blocking、现有真实 agent、现有 cwd basename 项目名；未识别继续显示
   unknown，不伪造检测结果。次行整个物理行的前景色与 dot 相同，包括选中项和填充格。
6. 左宽改为 `floor(width * 3 / 10).clamp(18, 34)`，用 u32 乘算避免 u16 溢出；仍保留
   终端最小可用宽度的既有边界。100列33→30，160列38→34，40列20→18。底栏仍仅一行。

类型/状态与项目名继续由原 `session_item` 消费 LIST/检测字段；未采到 cwd 时仍留空。
会话原始输出、TAB/控制字符投影、Unicode宽字符、历史/observer等路径未改。

## 代码门与定向验证

**UI 独立只读代码门通过，无阻塞项。** 核对了高度公式/共享边框、两行 slot 与命中
offset、奇数余行、标题保真、RGB叠加、小屏两个开关及自身保护。未引入字符串错误、
多余共享状态或逐行移植式抽象。backend 另行回读最终 view 补丁确认技术门。

主会话对冻结 Rust 源码实际分别执行：

- `cargo test -p homeway-cli tui:: --offline`：**75通过，0失败，1 ignored，exit0**。
- `cargo build -p homeway-cli --offline`：**exit0**。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings`：**exit0**。
- `cargo test -p homeway-cli sidebar_layout_preview_artifacts --offline -- --ignored`：
  **1通过，0失败，exit0**，仅生成 `/tmp` 合成渲染工件。
- 新 debug SHA256：`659f5a02e22a152c039ef6292b5c1c1099b689927575ee774be8dbab0f67b215`。

新增三项通过实际 TestBackend/输入事件验证：1/2/多主机及40×10的具体面板高度、dot/
标题列及无本地标签；暗/亮各四种会话状态的整行前景与 dot RGB一致、选中背景不变，
用户标题方括号保留；滚轮后按可见 offset 点击会话第二行、右键菜单关闭对应条目，
奇数主机余行不能命中未显示项。原主机滚动测试也新增第二行/下一条右键命中断言。
既有 self、控制字符与窗口布局回归仍在同批专项中。

## 预览与真实私有交互

`tools/tui-sidebar-previews.py` 只读取 ignored TestBackend 产生的 RGB/cell JSON，通过
现有 snapshot 工具生成 `/tmp/homeway-tui-sidebar-previews/` 下：
`one-dark.png`、`one-light.png`、`many-dark.png`、`many-light.png`、`small-light.png`。
同目录保留文本/JSON。这是暗/亮、一台/多台、小窗口及三态会话的**合成显示证据**，
不把固定示例状态宣称为真实检测采样。转换脚本实际执行 exit0；子任务已查看多主机深色
与40×10浅色，确认 dot/标题对齐、次行着色、共享边框和窄屏双开关。

对应真实工具同步适配：共享 `sidebar_width` 规则，100/120/160列的PTY宽分别69/85/125；
observer/usability鼠标坐标也使用同一 helper；remote 去掉旧“主机 1”标题断言。
local 新增 SGR 点击本地主机第二行、会话第二行后继续向同一 shell 输入 marker，避免
只靠纯渲染测试证明鼠标命中。五个 Python 工具 AST 校验通过，无私有 cache 入库。

主会话对上述完整 SHA 同件实际执行：

```sh
python3 tools/tui-smoke.py --bin target/debug/homeway --snapshot-dir /tmp/homeway-tui-sidebar-live-previews
python3 tools/tui-remote-smoke.py --bin target/debug/homeway
```

两条均 **exit0**。local **18项PASS**：OSC7项目名、创建/输入及69列PTY、真实紧凑
二行SGR点击返回同一shell、十个真实`ls`场景（含TAB/彩色宽字/长文件名/回卷）、85列
resize、自动主题/分离保存、菜单仅关闭第二shell及重新接入/信号退出恢复。remote
**7项PASS**：真实私有出口添加主机/选择/marker、SOCKS64KiB闭环、代理与本地角色菜单
开关/模拟剪贴板、删host保留远端会话。只用私有state/端口，脚本自有资源已清理。

主会话实际查看 one-dark/many-light/small-light 三张合成PNG，确认标题、dot对齐、
状态颜色、面板高度与窄屏符合。无需重复无关core/wire/跨平台/全仓矩阵门。
另查看真实私有交互产物 `/tmp/homeway-tui-sidebar-live-previews/shell-light.png`，确认
实际运行界面的项目名、次行状态色与位置正常。

## 仅换前端的可逆方案

沿用本机 debug 调试授权，固定 `target/debug/homeway` → `/opt/homebrew/bin/homeway`；
服务端无需换装/重启，后台应保持 PID26227。短安装器
`/tmp/tui-sidebar-layout-install.py`，完整 SHA256：
`4b6f74a50e588fc43325a4c6cd634a8bbc16e568a774a6b036210bbb26526ade`。

相对上一已审 `18e867a…946fcf3` 仅三处修改：OLD_SHA 为完整现役 d627、错误文案改为
通用 previous frontend、备份前缀改 `tui-sidebar-layout-20261028-`。继承目标锁、旧件
备份、权限、同目录暂存/fsync、strict签名、owner501:80/mode755、原子rename、异常
原子回退和PID前后一致检查；不调用daemon/launchctl/kill，不访问 state。

UI 独立增量代码门通过：确认安装器只有上述三处差异；SGR(5,3)/(5,6)对应0基(x4,y2)/
(x4,y5)，确实分别命中紧凑布局两个条目的第二行；remote 仍保留真实 CLI 主机清空、
空态与远端会话存活断言。预览工具只转换 fixture，runpy 不运行烟囱 main。
backend 独立增量技术门同样通过：逐字节构造比对确认仅三处变更、AST通过，安装器
file-only原子换/退、PID一致、ps/codesign限定保持；工具PTY几何与真实命中断言一致。

精确命令由主会话在验收后执行：

```sh
/opt/homebrew/opt/python@3.13/bin/python3.13 /tmp/tui-sidebar-layout-install.py --expected-sha 659f5a02e22a152c039ef6292b5c1c1099b689927575ee774be8dbab0f67b215
```

更新只影响之后启动的前端，正在运行的 TUI 需退出重进；没有发版/推送/远端操作。

## 前端更新完成

主会话唯一执行上述安装命令，**exit0、receipt SUCCESS**。子任务随后只读复核
`~/homeway-rollback/tui-sidebar-layout-20261028-22225653d3c7/receipt.json`：old 为完整
d627，new/installed 为上述完整659f；正式 PATH 与 `target/debug/homeway` 的 SHA256
相同。`pid_before=pid_after=26227`、`frontend_only=true`、
`service_restart_requested=false`、`rollback_performed=false`。回退目录0700、旧件
`old`0700、回执0600，旧 d627 已保留。

本轮仅原子更新前端文件，无后台重启、state修改或会话终止；没有新增生产shell/
attach/resize验收。用户退出并重新运行 `homeway tui` 使用新布局。根代理负责 ROADMAP
与同工作单元本地提交。
