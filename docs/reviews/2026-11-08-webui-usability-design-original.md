已只读核对 `HEAD f3e4555`。产品代码无未提交改动，工作区只有方案文档 `docs/reviews/2026-11-08-webui-usability.md` 未跟踪。本轮未改文件、未启动实例、未接触生产/默认 state/其他仓，也未运行浏览器烟囱；读取了方案、`app.js`、`http.rs`、`host.rs`、`pty.rs`、`protocol.rs`、`config.rs`、`tools/webserve-smoke.py`、`docs/WEBSERVE.md` 与 README 相关段。

## 结论

方向正确：fragment 不进 HTTP/WS 请求、复用 generation/socket/terminal 守卫、不重复造心跳、右键只 `preventDefault` 不阻断鼠标，都是合理的最小方案。

但按现方案**暂不建议直接开工**：有一个高风险边界会被“401/1008 停止重试”直接打断，且自动重连的生命周期、重试预算和测试断言还不够明确。普通网络 close 的自动重连可以成立；入口重启、中继重启/目标重新注册、静默半开连接这三类场景目前不闭合。

| 需求 | 判断 |
|---|---|
| URL `#webtoken` / 刷新保留 | 请求面成立；URL 历史暴露、写入/清除时机需补 |
| 断线自动重连 | 普通 close 成立；入口/中继重启会被 401/1008 提前永久停止；半开连接待实测 |
| 右键只显示 TUI 菜单 | 核心做法正确；需确认 TUI 菜单只在侧栏，且监听器生命周期要处理 |

## 一、高风险

### 1. [高] 401/1008 不是“凭证失效”的可靠信号，现方案会在入口/中继重启时永久停

**位置**：方案 `:11-13`；`host.rs:301-305`、`:496-501`、`:529`；`http.rs:264-268`、`:528-530`。

**问题**：
- `host.rs:301-305` 中，目标 token 在 relay 未运行或 `registry.lookup` 失败时返回 `Error::Unauthorized`，最终映射成 HTTP 401。这不是凭证被撤销，而是目标暂时不可达/未重新注册。
- `host.rs:496-501` 中，`Gateway::login` 在入口正在停止时也返回 `Unauthorized`；`http.rs:165-171` 的 `Server::drop` 先置 stop 再 join，旧 HTTP listener 仍有短暂窗口接单。浏览器 post-ready 1008 后重新 POST 登录，可能正好命中旧 server，拿到 401。方案会把它当成永久失败。
- `host.rs:529` 的 `consume` 在 stop 时返回 `Unauthorized`，`http.rs:528-530` 转成 pre-ready 1008；如果新 WS 又命中旧 server，方案“ready 前 1008 停止”也会永久停。
- `tools/webserve-smoke.py:1064-1091` 已经证明 relay restart 会让旧管理连接关闭、目标随后重新注册；但现方案的 401 停止规则会在目标重新注册前就放弃。

**建议**：
- 优先做服务端分类：401 只保留给真正的凭证认证失败/撤销；目标暂不可达、入口停止改为 503，或 WS 用可重试的 `1012 service restart` / 固定 error code。这样客户端规则可以保持简单。
- 若坚持 client-only：对“曾经 ready 过”的 401/1008 做有界重试，例如 5 次/60 秒后再停；首次手动登录 401 仍立即停。不要用“401 必为撤销”作为唯一认证边界。
- 必补测试：入口 restart 竞态、relay restart、目标重新注册、token rotate。测试要重复多次，不能只跑一次正常 restart。

## 二、中风险

### 2. [中] 1008 重试预算和 backoff 重置语义未定义，存在永久停或 1 秒循环两端风险

**位置**：方案 `:10-13`；`http.rs:288-305`、`:528-530`；`app.js:128-148`。

**问题**：
- “ready 后 1008 允许重新认证一次”是**每次连接**、**每次 outage** 还是**每页**？如果 ready 后立即重置，`terminal_unavailable`（code 2）每次 ready 后再次 1008，就可能 1 秒循环；如果整页只允许一次，后续真正的 60 秒活性超时又不能恢复。
- “成功 ready 后重置”backoff 在连接刚 ready 即断时会退化为 1 秒循环，叠加登录 60 次/分钟、票据/界面总数 8 的容量限制，容易制造 429/积压。
- `1000/1001/1005` 正常关闭、用户主动 `close()`、TUI exit 后的 close 未明确归类；现有烟囱用 `w.close()` 模拟失败，若按“非协议断开重试”处理会误重连。
- 1008 一律显示“认证或授权已失效”，对活性超时/入口停止是误导。

**建议**：写一张显式转换表，至少包含 `phase × close code/error code × wasClean × everReady`；重试预算按 outage 计，ready 后稳定一段时间（如 30 秒）才重置 backoff；正常关闭/主动 close/TUI exit 不重试；1008 文案分“正在重连/凭证失效”两阶段；jitter 用 full jitter 而非“小幅”。

### 3. [中] “已有心跳”不等于客户端可感知断线；ready 后半开连接恢复上界未验证

**位置**：方案 `:18`；`http.rs:528-533`；`crates/homeway-quic/src/exit/transport.rs:52,56,121`；`app.js:128-154`。

**问题**：
- 已确认服务端有 20 秒 WS Ping、60 秒 `last` 活性期限，QUIC 有 10 秒 keepalive/30 秒 idle；**不重复增加心跳协议是对的**。
- 但浏览器 JS 看不到 Ping/Pong，ready 后只能依赖 `onclose`/`onerror`。半开链路可能等到服务端 60 秒关闭或 TCP 超时才触发；若 close 帧丢失，可能更久。
- 代理先断浏览器侧时，服务端旧 endpoint/TUI 可能到 60 秒活性期限才回收；浏览器此时已重新登录并新建第二个 TUI，短时间内可能占用多个 permit，旧 TUI 还可能仍挂着工作会话。
- 现有 nginx 烟囱 `proxy_read_timeout 75s`（`tools/webserve-smoke.py:474`）+ 20 秒 Ping，90 秒空闲测试只能证明“Ping 能保活代理”，不能证明客户端能发现半开。

**建议**：把 60 秒/TCP 超时写成恢复上界；补 `context.set_offline(true/false)`、代理 kill/restore、入口 restart 的真实测试；检查旧 TUI 被回收、`ui_count` 和 permit 回到基线。若现场断连正是静默半开，单靠 onclose 不够，需要客户端可见的 app-level ping/pong（会动协议/判据）或明确接受该上界。

### 4. [中] 自动重连的生命周期/取消/代际边界没有落到现有 `close()` 结构

**位置**：方案 `:9`、`:13-16`；`app.js:15-32`（`close()`）、`:34-58`（submit/login）、`:187-190`（catch 调 `close()`）、`:196`（断开按钮）。

**问题**：
- 现在 `close()` 同时承担“主动断开”“登录失败清理”“TUI exit 后的重新连接”。若按方案把清 token/fragment 放进 `close()`，登录瞬时失败、自动重试、TUI exit 后点“重新连接”都会把 token 丢掉。
- `generation` 现在只在手动 submit/close 时递增；自动重试如果不递增，旧 WS/旧 terminal 回调可能污染新 attempt；如果递增但没有单一 retry timer，`onclose`、20 秒 ready deadline、登录超时可能各自排一个重试。
- 用户主动 abort 与 20 秒 timeout abort 需要通过 generation 或独立标志区分，否则会把“用户断开”当“超时重试”，或反过来。
- 旧 fetch/旧 timeout/旧 notice 必须在新 attempt 中全部失效；现有 A/B 测试只覆盖 WS/paste，没覆盖 timer/login。

**建议**：拆成 `startAttempt()`、`dropTransport()`、`logout()` 三个动作；每次 attempt 捕获自己的 generation；全局只保留一个 retry timer；所有 timer/fetch/notice 回调先判 attempt；`close()` 只清 transport，`logout()` 才清 token/fragment；补“旧 timer/旧 login 响应不污染新连接”和“主动断开取消重试”测试。

### 5. [中] fragment 写入/清除时机与“主动断开”语义未明确，URL 历史暴露没有文档约束

**位置**：方案 `:7-8`、`:13`；`app.js:15-32`、`:122-126`、`:196`；`docs/WEBSERVE.md:94-104`；`README.md:314-318`。

**问题**：
- fragment 不会发给 server，这点成立；但 token 会留在地址栏、浏览器历史和可能的同步/书签中。关闭标签页不会清除，方案只写了“主动断开清除”，没有文档警告“URL 含完整管理凭证”。
- “手动登录成功后写 fragment”没有定义成功点是 HTTP 200+`ticket`，还是 WS ready；若只到 HTTP 200，WS 失败时 refresh 仍可重试；若必须 ready，首次瞬时失败后 refresh 会丢 token。
- token 被 rotate/撤销后，fragment 里的旧 token 是否清除？临时 401（relay/入口重启）不能清，否则丢掉有效 token；真正 401 又应该清。当前 401 没有区分。
- TUI exit 后按钮变成“重新连接”（`app.js:134`），点击走 `close()`（`:196`）。如果 `close()` 清 token，用户就无法复用原 token 重连，只能重新输入。
- 不使用 localStorage 正确；但关闭标签页后 URL 仍在历史里，安全性需要在文档里说清。

**建议**：明确写入点/清除点；用 `history.replaceState` 而非新增历史项；仅主动 logout 或确定的凭证失败清除，临时失败保留并提示；TUI exit 后的“重新连接”不清 token；在 `docs/WEBSERVE.md`/README 加“URL 含完整 webtoken，勿分享/截图/作为长期书签”的警告；测试 refresh、disconnect、invalid/rotated token、history 行为。

### 6. [中] 验证计划与现有烟囱断言冲突，且缺关键失败路径

**位置**：方案 `:21-23`；`tools/webserve-smoke.py:808-812`、`:837-839`、`:793-803`、`:1105-1115`。

**问题**：
- `:808-812` 现有“真实 restart 后仅只读、显示部分输入提示”的断言，在自动重连成功后会失效。
- `:837-839` 受控空 503 现在期望立即显示“目标管理能力暂不可用”；自动重连会把 503 当可重试，状态会变成正在重试。
- `:793-803` 用受控 `w.close()` 模拟 close；自动重连对 1000/1005 的归类会决定该测试是否仍成立。
- 缺少 20 秒 login/ready deadline、backoff 上限/jitter/重置、1000/1001/1005、401/1008 边界、relay restart/target 重新注册、旧 timer/login 污染、重复自动重连后的 TUI/PID/UI count/UDP FD 回收、fragment 不进请求、右键 native menu 代理等覆盖。

**建议**：同步改现有断言，不要把“现有烟囱通过”写成自动重连已验收；增加受控 `/api/login` 401/408/429/503/500/200-no-ticket/hang 测试、真实入口 restart/relay restart/offline 测试、资源基线测试、URL token 不进入请求/日志/artifacts 的测试。右键用 `defaultPrevented` + TUI 菜单文本验证，不要试图断言 headless 原生菜单。

### 7. [中] 部分输入提示可能被自动重连状态覆盖

**位置**：方案 `:15`；`app.js:128-147`（onclose 消息）、`:155-174`（`sentInput`/`inputInterrupted`）。

**问题**：
- `sentInput`/`inputInterrupted` 是单次 attempt 的局部变量；新 generation 会丢失。
- `status` 是单行 `notice`，自动重连的“正在认证/正在重连”很容易覆盖“若刚发送输入，目标可能已收到部分内容”。
- 现有条件还用 `event.code !== 1000` 抑制提示；自动重连遇到正常关闭/exit 时可能把部分输入警告一起吞掉。

**建议**：把部分输入警告提升为会话级独立状态（独立 banner 或独立 status 行），新 attempt 不清，直到用户确认或重连成功且明确展示；补“发送输入后断网/restart”的真实测试。

### 8. [中] HTTP 登录结果分类未覆盖 200 无 ticket、3xx、413

**位置**：方案 `:10-11`；`app.js:46-58`。

**问题**：
- 200 但没有 `ticket`、或 200 但 JSON 解析失败，现在会抛错；如果归入“网络错误”重试，会无限循环。
- `fetch` 默认跟随重定向；307/308 理论上可能把含 token 的 POST body 重发到重定向目标。对含凭证的登录请求应更严格。
- 方案只列了 400/401/403 停止；413、404、405 等也应明确永久停止，不能落入泛化重试。

**建议**：`200 + string ticket` 是唯一成功；200 无 ticket/解析失败永久停；3xx 永久停或 `redirect:"error"`；4xx 除 408/429 外停止；5xx/网络错误/timeout 才重试。补对应受控测试。

## 三、低风险

### 9. [低] 右键方案核心正确；需注意 xterm 自身 handler、监听器生命周期和 TUI 菜单区域

**位置**：方案 `:17`；`assets/xterm.js` 的 `contextmenu` handler；`pty.rs` 的 TUI 启动；`crates/homeway-cli/src/tui/terminal.rs:50-63`；`tui/view.rs:1593-1689`、`:1690-1713`。

**问题**：
- 已核对：xterm 自己在 `.xterm` 上监听 `contextmenu`，但**不调用 `preventDefault`**；TUI 启用了 mouse capture，右键在 hosts/sessions 区域会打开 TUI 菜单。因此在 `#terminal` 上 `preventDefault` 且不 `stopPropagation` 的方向正确，未发现功能性阻塞。
- xterm 的 handler 仍会 focus/选中词；macOS `rightClickSelectsWord` 默认 true。
- 监听器应 non-passive、且只挂一次；不要随每次重连的 Terminal 创建重复添加。
- **区域语义**：TUI 菜单只在 hosts/sessions 区域打开（`view.rs:1593-1689`）；terminal 输出区域右键会转发给应用（`:1690-1713`）。如果需求是“xterm 输出区右键也要弹 TUI 菜单”，光靠前端 `preventDefault` 不会实现，需要 TUI 侧改。请确认用户指的是整个终端容器还是侧栏菜单区域。
- 键盘 contextmenu 也会被抑制；登录表单区域仍保留浏览器菜单。

**建议**：在 pane 上 `{passive:false}` 挂一次；可设 `rightClickSelectsWord:false` 避免选词副作用；测试 `defaultPrevented` + TUI 菜单出现 + SGR 右键事件；确认 terminal 输出区是否也要求 TUI 菜单。

### 10. [低] 卸载/恢复边界

**位置**：方案 `:13`；`app.js:220`。

**问题**：`beforeunload` 在移动端/bfcache 下不可靠；当前只 `socket.close()`，没有取消 pending login、retry timer、20 秒 deadline。`online` 事件也没有用于立即重试。

**建议**：用 `pagehide`/`pageshow` 配合 stopped 标志，卸载时 abort/clear timer；可选 `online` 时若处于重试等待则立即触发一次；补 bfcache 恢复测试。

## 四、看过未发现问题

- **fragment 不进请求**：`http.rs:181-191` 拒绝 query；`app.js:76` 的 WS URL 只使用 `location.host + /ws`；浏览器不会发送 fragment。fragment 方案在请求面成立。
- **心跳不重复造**：`http.rs:528-533` 的 20 秒 Ping/60 秒活性、`transport.rs:52,56,121` 的 10 秒 QUIC keepalive 都存在；不新增心跳协议是正确的。客户端可观测性边界见第 3 条。
- **右键核心路径**：xterm 不阻止默认、TUI 启用 mouse capture，`preventDefault` 不影响独立 mousedown/up SGR 事件；方案本身正确，细节见第 9 条。
- **现有代际守卫**：`app.js:52,79,104,130` 的 `generation` + `socket === ws` + `terminal === ui` 是好的基础；`close()` 清理 pendingLogin/pendingClipboard/旧 socket/旧 terminal 也在。自动重连需要扩展，见第 4 条。
- **输入预算与不重放**：`app.js:155-186` 的 2MiB 预算、64KiB 分帧、断线不缓存，`http.rs:360-362` 的 64KiB WS 上限，`pty.rs:21,401` 的 2MiB PTY 缓冲一致；未发现重放路径。
- **票据/容量**：`host.rs:490-517` 的一次性 30 秒票据、`host.rs:477-489` 的 60 次/分钟、`pty.rs:24-45` 的 8 个 permit、`http.rs:105-135` 的回收节奏清楚；“每次重连重新登录”是正确的最小方案。容量与窗口的测试见第 2/3/6 条。
- **验证纪律**：方案明确 `/tmp` state 与独立端口、不碰生产，符合要求；未发现该部分问题。

## 五、开工建议

1. **先修第 1 条**：确定 401/1008 的分类。优先服务端区分“真认证失败”和“入口/目标暂时不可达”；若坚持纯前端，至少对 ready 后的 401/1008 做有界重试。
2. **写清重试状态表**：覆盖 manual login、auto reauth、pre-ready、ready 后 1008、TUI exit、用户断开、卸载、1000/1001/1005；明确 per-outage 预算和 backoff 重置条件。
3. **拆 token 生命周期**：明确 fragment 写入点、清除点、TUI exit 后“重新连接”是否保留 token，以及临时 401 与真撤销的区别。
4. **同步改烟囱**：现有 restart/503/close 断言需要更新，否则“跑现有 smoke”会得到错误结论。
5. **同批补文档**：`docs/WEBSERVE.md`/README 增加自动重连、新 TUI 需重新选择工作会话、URL 含 webtoken 的安全提示、右键区域说明。

总评：**方案方向可行，但按现方案暂不通过实施前技术门**。第 1 条是硬阻塞；第 2/4/5 条需要在开工前明确，否则很容易实现成“普通断网可重连，入口重启或输 token 时永久停”的半成品。
