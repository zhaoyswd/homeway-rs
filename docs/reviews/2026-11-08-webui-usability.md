# WebUI 易用性修复（2026-11-08）

## 技术方案（实施前）

需求：URL 设置稳定 webtoken，刷新后自动登录；连接断开自动重连；终端右键只显示 TUI 菜单。

- 浏览器从 `#webtoken=…` 读取 token 并自动登录，手动登录成功后也将 token 写入同一 fragment。
  HTTP/WS URL 不带 token，不使用 localStorage。主动断开清除保存的 token 与 fragment。
- 使用现有 generation/socket/terminal 归属守卫；每次重连重新 POST 登录换取一次票据，
  从 1s 指数退避到 30s（加小幅 jitter），成功 ready 后重置。登录网络错误、408/429/5xx、
  非协议 WS 断开重试；400/401/403、1002/1009、ready 前1008与明确 TUI exit 停止。
  ready 后1008允许重新认证一次：既有服务端也用1008表示入口停止、远端失活和活性超时；
  token 真的撤销后新的 HTTP401 会停止重试。主动断开与卸载取消登录、计时器和旧连接。
- 每代先只读、清除 pendingClipboard；不缓存或重放断线输入。重建独立 TUI，真实工作会话保留，
  用户需要重新选择工作会话。输入传输中断的提示保留到重连期间。
- 登录与 WS ready 增加20s前端期限，防止连接尝试永远悬挂；旧代回调不能污染新代。
- 只在终端容器 contextmenu `preventDefault`，不停止鼠标传播；保留右键 SGR 鼠标输入。
- 服务端已有20s WS Ping/60s活性期限，QUIC已有10s keepalive；不重复增加心跳协议。
  用户现场断连根因尚未获得日志，不能将75s反代空闲直接认定为根因；私有真实浏览器跨过75s验证。

验证计划：JS语法、CLI Web单测/构建；现有完整私有 WebServe 浏览器烟囱；额外真实浏览器
URL/刷新、空闲90s、传输断开/入口重启自动恢复、主动断开取消、错误token停止、右键TUI菜单与
contextmenu取消验证。所有实例显式 `/tmp` state 与独立端口，不操作生产。

技术评审与最终代码评审使用仓库要求的 reviewer skill（`~/.agents/skills/reviewer/SKILL.md`）；
原始结果及逐条处置、实际测试证据待执行后补记。

## 技术门：结构化自评兜底

外部 dsh `/tmp/dsh-review/webui-design.aeH3MP/` 长时间持续推理而没有最终输出/退出码；
主会话按 ROADMAP 允许的结构化自评兜底，保留进程等待正常退出，不把推理片段当评审结论。

独立评审者角色核查：

1. 对齐完整性：fragment登录/手动成功保存/刷新登录/主动断开清除，HTTP与WS URL无凭据；
   既有hww1字段与wire不改。文档说明URL本身是凭据，不能分享。
2. 边界：首次400/401/403停止；明确revoked/terminal_unavailable/protocol_error停止；
   正常exit及1000停止，网络断开/1011/1013重试；曾ready后无错误说明的1008重认证。
   本机中继目标暂失注册也返回401，因此同一次恢复最多6次401且总窗90s；只有真正ready
   才重新开放预算，失败不续窗，首次错误凭据无此预算。
3. 并发/取消：每次尝试递增generation，清旧socket与登录/ready计时器，旧UI先只读；
   成功登录后dispose旧Terminal再创建新Terminal；手动断开/卸载取消全部退避和请求。
4. 错误面：登录及WS ready各20s，网络错误不永久挂起；1s指数退避上限30s加≤250ms抖动；
   仅连接持续ready≥30s后重置退避，避免反复瞬时ready→close造成1s循环。
5. 与基线漂移：不改既有心跳/QUIC/PTY生命周期、输入帧与预算；右键只取消默认菜单，
   不阻断mousedown/up传播；复制/粘贴仍受generation守卫，不重放任何输入。
6. 资源/验证：每代一WS、一Terminal；定向实测故障与退出、主动取消、错误凭据、90s空闲，
   更新旧烟囱对“断线不重试”的断言，完整执行后再记通过。
7. Rust惯例/逐行移植：本轮只改前端与验收脚本，不引入Rust状态/错误/同步结构。

结论：按以上收紧后的方案可实施；外部技术门尚未完成，不宣称外部通过。

## 外部技术门原结论及处置

随后原 dsh 正常结束，实际 **exit=0**，原文完整读取并保存在
`2026-11-08-webui-usability-design-original.md`。原结论是**暂不通过**，包括1高/7中/2低；
这是对原始方案的评审，不回写成已通过。下面逐条记录实现者判断及落实，最终由独立代码门复核。

| 原意见 | 判断与处置 |
|---|---|
| 1 高：401/1008 混合暂失注册/入口停止，过早停重试 | 认同。仅曾ready后开放90s/最多6次失败预算，覆盖HTTP401及恢复期pre-ready1008；初次错误token与明确revoked仍停止。失败不续窗，超窗不再发新认证请求。保持后端分类与wire，真实入口/中继恢复验收。 |
| 2 中：1008预算/瞬时ready重置/关闭类别未定义 | 认同边界问题。退避仅ready持续至少30s后重置；正常1000/TUI exit及协议/超限停止，1001/1005/1006等异常路径重试，1008恢复中先提示重新认证。采用0..250ms小抖动而非full jitter：保证最少1s、避免零延迟自旋，峰值仍≤约30s；本轮单页总名额仍受原后端门约束。 |
| 3 中：半开感知及旧资源回收 | 认同。保留已有心跳，不扩协议；静默半开浏览器感知没有统一有限上界，文档明确。90s空闲只验保活；离线测试是set_offline加受控close后恢复，不冒充仅offline/黑洞探测。真实重启及TUI/UI回基线另验。 |
| 4 中：close混合职责/多计时器/旧回调 | 认同。分dropConnection/startConnection/close，只有主动close清credential及URL；每次attempt递增generation，retry timer唯一；登录和ready分别使用同一deadline槽，清理统一。Node回归旧登录完成后不能污染新目标，主动取消不重连。 |
| 5 中：fragment写清时机/历史/正常exit后复用 | 认同。成功点为WS ready，使用replaceState；只有主动断开清URL，正常退出或错误保留供手动修正/刷新。独立“重新连接”按钮复用凭据，“断开”始终清除；README/WEBSERVE写明完整URL含管理凭据及历史同步暴露。 |
| 6 中：现有断言冲突/失败路径覆盖 | 认同。旧失败测试调整为先只读不发字节、随后真实重开入口自动恢复；正常exit不重连，503显示固定消息并退避。新Node只测难注入的期限/状态边界/预算/旧登录；真实浏览器另测URL/刷新/右键/重启/受控断开与资源。原剪贴板门不放宽，环境阻断如实记录。 |
| 7 中：部分输入提示被新attempt覆盖 | 认同。inputWarning提升到页面级，等待/认证/失败时保留，ready后再次明确展示后才清；不记录/重放任何输入。 |
| 8 中：200无ticket/redirect/4xx分类 | 认同。200只接受对象且非空string ticket；格式错误停止；redirect采用manual，重定向不会重发body；4xx除408/429与上述有界401恢复外停止；5xx/网络/超时退避。HTTP200 body阶段AbortError重抛，防吞成坏票据永久停。 |
| 9 低：右键监听/区域/xterm选词 | 认同核心正确。仅pane上一次non-passive preventDefault，保留SGR事件；不改xterm默认选词以保留已有选取习惯。主机/会话菜单测试同时验defaultPrevented及真实菜单文字；输出区域维持TUI/应用原鼠标语义。 |
| 10 低：pagehide/pageshow/online | 认同。pagehide清所有计时器/请求/socket，pageshow persisted用保留凭据重建，online仅在retrying加速已有重试。未把移动端/bfcache实际运行验收写成已完成。 |

## 验证记录

- `node tools/webserve-ui-check.cjs`：实际PASS，覆盖URL/手动保存、HTTP与WS错误边界、登录和ready期限、
  HTTP200 body超时、旧登录/取消、六次有界认证恢复、稳定后退避重置及TUI exit。
- `cargo test -p homeway-cli webserve::`：**22 passed / 0 failed**；构建通过。
- 首轮 `--browser --extended`：协议安全、本机Web-only、两目标HTTPS/WSS、40轮资源回基线、
  token轮换与relay重启撤销、30s票据过期及8槽容量均通过；旧浏览器剪贴板阶段被
  `ClipboardGuard` 的“Cannot safely snapshot a promised clipboard format”拒绝，未触碰宿主剪贴板。
  原日志`/tmp/webui-smoke.log`，私有证据`/private/tmp/homeway-web-3cbk6qtf`，不记整轮通过。
- 增加`--browser-recovery`仅执行不访问宿主剪贴板的浏览器失败/恢复专项；原`--browser`门保持。
  所有实例继续由既有私有helper管理，显式`/tmp` state及独立端口。
- `python3 tools/webserve-smoke.py --bin target/debug/homeway --browser-recovery --extended`：实际
  **exit0**。此轮固定私有快照`0d5776ad875f794c8ea8d07a391f90bf78114deae4b448ba0ee64879892e6f75`，
  证据`/private/tmp/homeway-web-r4iazl9w`、日志`/tmp/webui-recovery-smoke.log`。完整既有协议/安全/
  票据/双目标/40循环/父进程突失回收，加本批真实浏览器恢复通过。该件在最终JSON两项补修之前，
  不冒作最终源码快照。
- 最终构建快照`e7bed87d95ddcc5f26666a94516ef2787b23394d4bf6700db064f95a7864beb3`，
  使用`/tmp/webui-final-browser.py`复用仓内Lab、`browser_failure_checks`及`browser_usability_checks`
  起新私有relay/exit/NGINX、重复最终真实浏览器专项，实际 **exit0**；日志
  `/tmp/webui-final-browser.log`、证据`/private/tmp/homeway-web-bybl05lo`。
  该轮`browser-usability.json`实际记录：fragment自动登录/刷新、两种TUI右键及浏览器默认取消，
  同一个WSS socket空闲 **90.047s**、受控close+离线→在线恢复、**3轮**入口restart、relay重新
  注册恢复、取消重试、HTTP请求URL无token，最终TUI进程 **0** / `ui_count=0` / `page_errors=[]`。
  runner另验原工作会话集合仍在、私有日志无完整webtoken；`browser-failures.json`证明双页尺寸
  独立/旧页关闭不影响新页、错误与exit文案、只读输入零字节、1008首认证失败和503退避。
- 最终`cargo test -p homeway-cli webserve::`：**22/0**，`node tools/webserve-ui-check.cjs`、
  `python3 tools/gen-webserve-vectors.py --check`、`git diff --check`均PASS。
- 未执行/不宣称：旧完整浏览器剪贴板/CJK大粘贴/A→B异步剪贴板整轮（见首红环境阻断）、
  本批全仓ci-local/跨平台运行、移动端/bfcache真实恢复、静默半开浏览器自动感知上界。
  现场自动断连具体诱因尚未归因；本批解决的是缺少自动恢复，保活实测仅针对私有已知配置。

## 独立代码门

独立子代理`/root/webui_improvements/code_review`只读检查最终diff与原技术1–10，发现两项中危：

1. HTTP200 JSON null未作形态守卫，`result.ticket`抛TypeError且仍被归入可重试，会无限重试。
2. HTTP200头后body挂起时，`response.json().catch`吞AbortError，误作缺票据永久停止。

两项均认同并整改：所有成功响应必须为对象且非空string ticket，非法成功停止；AbortError
重抛到超时重试，其他JSON解析失败按无票据处理。新增null/数组/原始值/空ticket与body阶段
超时回归，评审者独立执行Node检查PASS。另低危README右键区域措辞改为屏蔽浏览器菜单、
侧栏保留TUI菜单。未改Rust，无新增Arc/Mutex/字符串错误或直译痕迹。

最终独立代码门**通过，无剩余高/中危**。评审者复核原技术1–10全部闭环，独立执行Node及
diff检查，并核对最终本机构建与私有快照哈希一致、浏览器receipt/log支持上述实绩。
最终结论原文见`2026-11-08-webui-usability-code-review.md`。原技术“暂不通过”及本代码门首次
两项中危均保留，不以最终通过抹除。技术门修订方案按结构化自评完成，再由独立代码门逐项
复核；不声称原dsh实施前已批准。

本批仅本地代码与文档；未推送、发版或部署，生产实例与其他仓均未操作。
