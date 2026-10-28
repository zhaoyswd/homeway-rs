# TUI 左侧宽度再缩约五分之一（2026-10-28）

用户要求在当前宽度上再缩窄约1/5。仅将原有`clamp(18,34)`后的侧栏期望宽度乘`4/5`，
然后沿用`min(area.width.saturating_sub(18))`的右侧留宽保护；40×10最小窗口门不变。
常见宽度由34→27、30→24、24→19、18→14列，整数向下取整。主机高度、分割线、item内容、
颜色、输入/会话/backend均不改，绘制和鼠标命中继续共用同一Geometry。

同步既有布局断言中的终端起点（100列30→24、40列18→14）；不添加重复实现公式的测试。
实现已rustfmt，`git diff --check`实际exit0。

## 独立评审

非作者实读最终diff与draw邻域，技术门/代码门均通过，无阻塞：先经过40×10最小窗口门，
侧栏14..27列，乘法最大136不溢出；40列时右区仍27列（含共享边线），标题/添加按钮与
状态点可见边界安全，裁切及鼠标命中仍由共享geometry决定。主机高度/backend/输入无改动，
没有新增共享状态、拷贝或Rust逐行移植式接口。评审者未运行cargo或安装产品。

安装适配器亦由非作者独立实核diff恰OLD_SHA及备份前缀两处，完整SHA与下文一致；原锁、
签名、备份、原子换退、PID不变要求保留。技术/代码静态门通过，不代替真实安装结果。

## 本机debug文件更新

沿用本会话已经授权的本机debug文件更新，不重启后台、不访问正式state。以下为主会话
实际执行结果，适配器静态门与真实运行证据分列。

最小适配器`/tmp/tui-sidebar-width-install.py`，SHA256
`611a5bc6693c68d36cd8f66ffcc801cb2c7616d1223d24c67e3c06aa4959e9e2`，
相对已审`/tmp/tui-selection-copy-install.py`仅OLD_SHA改为当前完整
`6d8b05b3f50699963a524e164bc0f308e0776ca7a2b43ea32fc35e3f33aaf647`和备份前缀改为
`tui-sidebar-width-20261028-`。固定目标`/opt/homebrew/bin/homeway`、必填新SHA、锁/旧件备份、
签名/fsync/同目录原子rename、条件文件回滚与前后唯一零参守护PID不变要求均沿用。

主会话实际执行`cargo test -p homeway-cli tui:: --offline`，exit0，**94 passed、0 failed、
1 ignored**，日志`/tmp/homeway-tui-sidebar-width-tests.log`。另行运行已有ignored布局预览
exact测试，exit0、1 passed；`python3 tools/tui-sidebar-previews.py`实际exit0，生成五张预览。
主会话查看one-dark、many-light及small-light，布局、按钮、左右边线正常，窄屏内容合理裁切。
`cargo build -p homeway-cli --offline`实际exit0（0.07s）。本轮未重复clippy或真实交互烟囱，
不将上轮结果冒充本轮运行。

最终已测debug工件SHA256为
`53fb8678c93918c8ff7cee675567f7d533e8adca1e8781d3406b6880efc15d8b`。
主会话实际执行安装器，exit0、receipt **SUCCESS**，后台PID **55632→55632**，
未重启、未回滚。旧件与回执备份目录为
`/Users/zhaozhe/homeway-rollback/tui-sidebar-width-20261028-5e78968ff0f0/`。
已运行的TUI需退出后重新进入，才能使用本次更窄的左侧布局。

LAN71此前两次SSH连接超时；本轮不重试、不上传或换装，不能写成远端已部署。
