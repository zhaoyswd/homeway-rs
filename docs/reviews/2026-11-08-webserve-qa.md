# 独立 WebServe 私有验收记录

本记录只写实际执行结果。所有实例均由私有 helper 的显式 UNIFIED 模式在 `/tmp` 创建，
测试前复制固定二进制到独立私有目录并核 SHA；未改 PATH、生产实例或正式 state。
helper 默认 legacy 行为保留，HTTPS 反代只停止测试创建的 NGINX PID。

## 验收工具与固定协议

- `tools/gen-webserve-vectors.py --check` 是独立 Python 标准库规格 oracle，不调用 Rust 编解码。
  token、管理 HMAC/握手和终端帧正反向量已入 `fixtures/webserve/`；68 个全目录 SUMS 校验实际 exit 0。
- `tools/webserve-smoke.py` 使用独立 RFC6455 测试 peer 做协议边界，并使用真实 Chrome、xterm.js、
  NGINX HTTPS/WSS、目标 TUI/PTY 做交互验收；不以截图或 WS peer 代替浏览器行为。
- 主机剪贴板所有格式只在内存保存，内容不进文件或日志。983 阶段实际观测 Chrome headless
  copy/paste 后宿主 pasteboard changeCount 未变；该事实仅适用于本次真实运行，不作所有平台承诺。
- 前端源按 SOURCES.json SHA256 对账并使用 Node 只做语法解析；HTTP 内嵌的库字节也必须相同。

## 已发现并保留的失败

| 阶段与件 SHA 前缀 | 实际结果 | 原始证据 |
|---|---|---|
| `a65a4dfa` 首轮 | exit 1：未认证 WS 65537B 帧实际空 Close（1005），要求 1009 | `/tmp/homeway-web-smoke-r1-7u6puhoi/output.log`；独立同件复现 state `/private/tmp/homeway-web-jd5z9e78` |
| `a79dfad6` 完整轮 | exit 1：第25轮页面关闭后目标 TUI 在15秒内未回收；前24轮资源恢复 | `/tmp/homeway-web-smoke-r2-oiuhknzd/output.log`，state `/private/tmp/homeway-web-y06vr_8v` |
| `a79dfad6` 普通浏览器 | 打包 xterm.js 语法错误，未注入的页面也无 Terminal；Node 独立解析失败 | `/tmp/homeway-web-js-syntax-j70hwl2w`；`/private/tmp/homeway-web-yeict10_/browser-failed.png` |
| `00f41a57` Mac浏览器 | 原生强制选中缺省未启；实证 xterm 默认只支持 Mac Option+配置开关 | `/private/tmp/homeway-web-oh5ma55i/browser-failed.png` |
| `983eb150` 浏览器 | 实际文本/复制粘贴已通过，但 🙂 cell width=1，与 Rust VT 两列不一致 | `/private/tmp/homeway-web-xiblnbm4/browser-glyph-widths.json` |

产品分别修正显式 WS 关闭码、管理取消/RELEASE/PTY 回收、原始 npm 字节、Mac Option 选中及
Unicode11。每次修复后用新 SHA 复验；没有放宽 40 轮、关闭码或回收期限。

QA 自身也纠正了几项问题，未归咎产品：Ratatui 会在汉字间插游标 CSI，故改为 pyte/xterm
真实屏幕解析；term list JSON 为 `{sessions:[...]}`；Playwright wait_for_function 的内部 eval
被严格 CSP 拒绝，改为 Python 轮询 page.evaluate，未放宽 CSP；粘贴后需等实际回焦再发送 Enter。

## 阶段真实通过（不冒充最终件）

- `a65a4dfa` 聚焦实际 exit 0：本机 TUI 创建 shell/输入、页面关闭保留会话；真实 HTTPS/WSS
  经中继打开无 HTTP 出口，11秒后仍可交互。`/tmp/homeway-web-focus-4o_djltr/output.log`。
- `00f41a57` 轮次实际完成40轮原生 assoc/UDP FD/TUI 恢复、轮换/中继重启、真实30秒票据过期、
  第9名额拒绝；整轮在 browser 前因 QA JSON 解析错误 exit 1，不算整轮通过。
  `/tmp/homeway-web-smoke-r3-u59nm0rn/output.log`，资源逐轮证据
  `/private/tmp/homeway-web-5umxlsdy/resource-cycles.json`。
- `983eb150` 浏览器聚焦实际 exit 0：HTTPS 登录、CJK/emoji文本、Mac Option选中、真实页面
  copy/paste、resize、真实新工作会话保留；page_errors 为空。该件 emoji 宽度仍是一列，后续修复。
  `/tmp/homeway-web-browser-r5-cs1kco3f/output.log`；截图及 clipboard receipt 在
  `/private/tmp/homeway-web-xiblnbm4/`，已实际查看截图。
- `983eb150` 凭证边界聚焦实际 exit 0：offline 8 并发中1成功、7个锁拒，成功者凭证一致；
  live RPC 8并发全部成功且一致；占实例锁但无 control 时不写 auth/config、不起服务。
  `/tmp/homeway-web-credential-r1-eeg2e3pz/output.log`。
- 依赖闭包阶段实际 exit 0：CLI 无 rt-multi-thread，CAPI 无 axum/hyper/tungstenite；oracle、
  SUMS、ci-local/helper 语法均通过。`/tmp/homeway-web-deps-x5z9radn/report.json`。

## 最终统一候选

候选：`/tmp/homeway-webserve-final-r2-uowq4cpk/producer/homeway`，SHA256
`85ae9cfab0be18ec1f423e705ee86504060a1b4e9bfba4a353e0f2a4318c9c0a`。
本轮包含普通准入取消 guard、Unicode11、前端异步代际隔离及以上修复。

完整命令：`python3 tools/webserve-smoke.py --bin <该固定件> --browser --extended`。
实际于 2026-11-08 完成，整轮 **exit 0**；退出码在
`/tmp/homeway-web-smoke-final-6pk4zwl3/exit.txt`，完整日志在同目录 `output.log`。
finally 清理已返回；随后只读检查本轮固定 binary/nginx 路径，`cleanup-receipt.json` 记录 remaining=[]。
所有服务仅为本轮私有实例。

本轮 state `/private/tmp/homeway-web-zai_gyha` 中的独立证据：

- `binary-receipt.json`：生产者和私有复制件 SHA 与上述固定候选一致。
- `credential-races.json`、`hot-install.json`：离线/在线并发、占锁禁写及已运行出口首次凭证热安装。
- `resource-cycles.json`：实际40轮登录/OPEN/关闭，每轮 TUI 回收、UDP FD 基线与中继原生 assoc=0。
  此处不把集成 FD 观察声称为直接读取正式腿内存计数；正式 owner/腿计数另有 core 私有单测。
- `browser-glyph-widths.json`：实际汉字和 🙂 均为两列；`browser-tui.png` 已逐张实际查看，
  汉字与 emoji 完整，页面复制、粘贴及 shell 输出可见。
- `browser-observations.json` 与 `browser-rapid-reconnect.png`：真实 A/B 出口快速切换，B 会话已渲染；
  在旧 A socket 对象上受控触发迟到 output/error/close，并释放旧 paste Promise 后，新 B 未接收旧输入、
  输出与状态未被串改。这是实际切换加受控迟到回调，不声称全部为自然网络延迟。
- `browser-clipboard-receipt.json`：实际 copy/paste 测试已到达；宿主 pasteboard 无变化，
  本次观测 headless clipboard 隔离。原内容仅在内存，未写日志或文件。

整轮还实际通过：web-only 本机、真实 HTTPS/WSS 无 HTTP 出口、多目标错误凭证和未知目标隔离、
11秒后交互/工作会话保留、轮换及中继重启撤销、停中继本机仍用、真实30秒票据过期、
第9名额拒绝、恶意 Origin/Host、慢头/首帧超时/大帧关闭码、浏览器鼠标/复制粘贴/resize，
关闭浏览器后真实新工作 session 保留，服务日志无完整测试 WebToken。

最终依赖闭包与工具真源复核亦实际 exit 0：
`/tmp/homeway-web-deps-final-jybj0ne5/report.json` 记录当前 Cargo manifest/lock SHA；
CLI feature 图无 rt-multi-thread，CAPI 图无 axum/hyper/tungstenite，独立 oracle、全目录 SUMS、
ci-local 与两 helper 的 zsh 语法均成功。

最终代码评审另行记录，不能用测试通过或 dsh exit 0 替代门结论。

## 大粘贴审中补充与最终浏览器候选

85ae 整轮的短中文粘贴不能证明大粘贴。审中实际补做 80KiB 浏览器剪贴板，
`/private/tmp/homeway-web-zy8jesqh/large-paste-reproduction.json` 记录81920B载荷被xterm包装为
81932B单帧，服务端真实1009关闭。该次runner exit1还包含QA预期恰81920且无终端应答的
过窄断言；没有将此exit伪称通过。此缺口由客户端整次2MiB预检+≤65536B顺序分帧修复，
服务端64KiB上限未放宽。

最终浏览器候选 `/tmp/homeway-webserve-final-r4-hjxlvg_q/producer/homeway`，SHA256
`798bc6a15462b0e5a2b36a1999d674559ec85c37ff6286a4ee6d6f6337da685d`；
相对85ae只有自有app.js产品增量（长度早拒、分帧、发送异常关本轮连接及部分输入提示）。
后端40轮/协议/资源验收沿用同源后端85ae的实测，未声称此新SHA重跑全部后端矩阵。

实际两出口HTTPS/WSS浏览器聚焦整轮 **exit0**：
`/tmp/homeway-web-large-browser-r6-67psrult/{output.log,exit.txt}`，state
`/private/tmp/homeway-web-3wmirzdn`，复制件SHA见 `binary-receipt.json`。

- 80KiB载荷含CJK/emoji，目标raw文件81920B逐字节一致；SHA256双方均为
  `90a9bddfbfeb0353ffd7ac30131751f419765681a40b6afb57cff5b5359e591b`。
  粘贴帧65536+16396（含12B bracketed paste包装）；🙂起点wire byte65535，跨65536分帧，
  目标原始UTF8不损坏。全部帧大小及边界见 `browser-large-paste.json`，原始目标文件保留。
- 超限载荷2097195B带唯一标识，**0粘贴载荷帧**，目标继续raw观察4秒额外0B，原文件无增量，
  WS保持OPEN。真实点击按钮伴随focus-out `1b5b4f`、focus-in `1b5b49`，以及TUI发出OSC11
  背景色查询的确切答复 `1b5d31313b7267623a313031302f313431342f313931391b5c`；
  仅这三个精确控制串被区分，所有观察帧都入receipt，不把控制应答隐去或当成粘贴载荷。
- 相同新件重验了中文/emoji两列、Mac Option、页面短文本copy/paste、resize、A/B实际切换与
  受控旧paste/output/error/close、关闭保留真实新增工作session；两张截图均已独立查看。
- `browser-clipboard-receipt.json` 记录本次宿主未变；`cleanup-receipt.json` 私有残留=[]。

新增QA工具初轮失败均保留：`/tmp/homeway-web-large-browser-final-ygelqavw`、
`/tmp/homeway-web-large-browser-r2-xh2qi35k`、`/tmp/homeway-web-large-browser-r3-208qogho`、
`/tmp/homeway-web-large-browser-r4-gde6d34r`、`/tmp/homeway-web-large-browser-r5-2a4352hy`。
原因分别为insert_text换行归一需单行启动、误判外层TUI bracketed模式、终端自动应答计数等
QA前提；最终使用目标raw文件和确切WS字节交叉判定，没有放宽产品限额或数据完整性要求。

## r3/r4整改候选ea2afd：真实父进程突失失败（未通过）

候选SHA `ea2afd6d4726840c1848984e8af6898f421a33abbbcb3a0c1b5418a525de5f98`，
实际完整 `--browser --extended` **exit1**：
`/tmp/homeway-web-r3-remediation-full-tychpwx1/{output.log,exit.txt}`，state
`/private/tmp/homeway-web-ttd_xfax`。

在最终父突失项前，原40轮/所有server边界及真实browser均已PASS，包括实际OSC52先暂存后点击确认、
80KiB跨UTF8完整SHA、ASCII早拒/UTF8字节超限/受控bufferedAmount分支、两真实页面独立尺寸与关闭、
受控error/exit后实际close保原因、真实WebHTTP restart断链只读与部分输入提示、真实1008未ready回登录、
受控空503固定文案。新增M1失败后stop、M5/L1早返回也已经在各自阶段通过。
这些阶段通过不能包装成本轮整体通过。

最后 `parent_loss_check` 只SIGKILL全新私有web-only父PID，没有工作session保活要求；
真实TUI PID28451在8秒后仍为PPID1 live RNs+，非zombie。采样
`parent-loss/tui-sample.txt` 显示主线程在crossterm event::poll内部Unix try_read/read的PTY EOF路径，
使外层control_guard关闭检查无法再次运行。没有新自动拉起的daemon；仍有旧孤儿TUI，属于新发现。
此项与Backend首次Refresh spawn-counter特异测试不同，后者已通过也不能掩盖这里的实际失败。

采样/FD证据保留后，核对PID完整argv仅清理该私有孤儿；
`parent-loss-cleanup.json` 记录最终私有进程remaining=[]。生产实例未操作。
待最小修复及新候选真实复验，最终代码门仍未通过，尚未启动最后聚焦增量外评。

## 最终r6候选：完整实际通过

固定候选 `/tmp/homeway-webserve-final-r6-ti3poq6d/producer/homeway`，SHA256
`1c955a412687c7ac85e5f5a83fb96dbe82de60edf34296ebe652a1aba3909085`。
真实完整 `--browser --extended` **exit0**：
`/tmp/homeway-web-r6-full-l6ifik1l/{output.log,exit.txt}`；state
`/private/tmp/homeway-web-vct1cmt4`。binary-receipt确认私有复制件SHA；finally清理后只读ps检查
`cleanup-receipt.json` remaining=[]。未更换PATH/正式state，未操作生产。

父突失先独立聚焦exit0：`/tmp/homeway-web-r6-parent-focus-nep0o7bb`，再纳上述整轮重验。
修复只在`--no-spawn`且Web标记的独立TUI命令隔离唯一crossterm输入worker，channel4有界顺序；
主loop持续检查control，正常析构App/TerminalGuard，卡EOF worker不无限join，随独立进程结束。
本轮父PID33611/旧TUI33647在SIGKILL父后退出，连续10次无替代进程/监听，见`parent-loss.json`。
这证明常见父/control突失收尾；不把它冒充Backend首次Refresh竞态的spawn-counter特异回归。

本轮完整重验：

- 原40轮每轮assoc/UDP FD/TUI基线、无HTTP两出口、先启动再首次token热装、离线/在线并发和占锁禁写、
  轮换/中继重启撤销、停中继仍能本机、真实30秒票据期限、第9额度拒绝、Host/Origin/慢头/首帧/大帧。
- `browser-large-paste.json`：80KiB原始文件81920B逐字节与SHA
  `90a9bddfbfeb0353ffd7ac30131751f419765681a40b6afb57cff5b5359e591b`一致；
  frame65536+16396，🙂wire起65535跨65536完整。ASCII UTF16早拒、UTF16=1048599但
  UTF8=2097177的emoji字节超限、显式受控bufferedAmount=2MiB三分支均0粘贴载荷；
  8秒raw目标额外0B。每项所有排除的focus/OSC11确切控制字节hex均保存；buffered分支是受控属性
  验证，不声称已造真实拥塞。
- 实际shell经TUI发送OSC52，剪贴板在点击前保持标记不变、点击复制后才得到目标文本；
  普通中文/🙂观测2列、Option鼠标、短文本copy/paste、resize、A/B实际切换与受控迟到回调通过。
- `browser-failures.json`：两真实页面114x38/149x48互不影响；受控error/exit消息后实际close仍保原因；
  真实WebHTTP restart断链后只读、0后续输入和保守部分输入提示；受控同步send异常；
  篡改首auth票据得到真实server1008后回登录；受控空503用固定文案。受控部分明确单列。
- 正常关闭页面后term list确认真实新增工作session仍存在。异常关闭测试只验证不主动KILL、只读与重连提示，
  不据页面文案声称崩溃目标上的session仍活。
- M1真实占用端口start失败后stop回disabled/reason null；M5 userinfo Host拒绝，L1坏Host/query均no-store。
- `browser-tui.png`、`browser-rapid-reconnect.png`、`browser-readonly-failure.png` 三张均独立逐张查看。
  `browser-clipboard-receipt.json` 本次宿主未变；不外推所有浏览器的剪贴板隔离。

最终依赖/真源复核实际exit0：`/tmp/homeway-web-deps-r6-yqpfz8uo/report.json`记录最新manifest/lock SHA；
CLI feature无rt-multi-thread、CAPI无axum/hyper/tungstenite，oracle、68项SUMS、ci-local和两helper语法通过。

未扩大声称：本轮交互仅macOS Chrome；未测Firefox/Safari/移动端/Linux运行面、原生IME和精确2MiB接受边界。
宽度只验证上述字符，浏览器Unicode11并非Rust新表全量同宽。没有注入“只断TCP控制、同epoch同代WSS全链不断”
的专门集成故障；B2通过源码与22项含真实UDP clear/replay数据的定向测试证明owner边界，不拿relay restart替代。
M4不承诺serde/allocator/内核/终端全链物理清零。

功能QA已完成；总代码门仍须最后聚焦外评完整结果与主任务CI/cross，不能以此exit0替代代码评审。
