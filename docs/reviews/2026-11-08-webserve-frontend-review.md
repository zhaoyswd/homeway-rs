# 独立 WebServe 最终前端聚焦外评

- 命令：本仓根目录 `dsh --profile headless`，真实退出码 **0**。
- 原始目录：`/tmp/dsh-review/r4-webserve-frontend.qzc5Fe/`；保留 prompt.txt、output.md、stderr.log、exit.txt、started.json。
- 输出全文已完整读取；原文 SHA256：`d736d967ad81221e52e1d029f56d1537052b47922e4a6e0d0c8f79e10ba2259c`。
- 仓内正文仅归一化结论行末两个Markdown硬换行空格，以通过默认diff空白检查；结论/意见未改。未经归一化的原始字节仍在上述output.md，SHA为上一行所列，本文不声称归一化正文与原输出逐字节相同。
- 外评结论：**有条件通过**，F-1 为中等阻塞；命令 exit0 不等于代码门通过。
- 该轮对象为798bc最终前端；只补充完整r3代码门，不替代后端门。评审期间其它并行修复不自动纳入本报告验证。

## 外评原文（完整）

# homeway-rs 独立 WebServe 最终前端聚焦代码门

**结论：有条件通过。**
前端数据通路、代际隔离、剪贴板权限边界和资产打包未发现高危/安全阻断；但 WS 失败/断连状态机有 1 项中等阻塞条件，另有 1 项部分输入提示、1 项 Unicode 宽度口径和若干 HTTP/错误可观察性问题需同批处理。**这不代表整体代码门通过，也不替代 r3 / 完整后端代码门。**

## 一、边界与独立结论

- 基线 `bd00c0c`，当前工作区未提交内容包含本批新增文件。
- 完整阅读：`assets/{app.js,index.html,style.css,SOURCES.json}`；`app.js` 170 行全读。
- 交叉阅读：`http.rs`、`protocol.rs`，并仅按接口需要核对 `pty.rs`、`config.rs`、`host.rs`、`tui/clipboard.rs`。
- 第三方仅检查 xterm/fit/unicode11 的必要接口、来源声明、SHA256 和许可，未泛审供应链。
- 本轮只读：未修改源码/文档、未提交、未启动实例、未重跑测试、未操作生产。
- 未读取 `/tmp/dsh-review/r3-webserve-code.mprbwvox/` 的未完成输出，未等待/打断该进程。**本报告补充 r3 对最终 798 前端增量的覆盖，不替代后端代码门。**

## 二、候选、资产与证据核对

当前工作区资产与最终候选逐字节一致：

| 对象 | 结果 |
|---|---|
| `app.js` | `74643a51…70f6f`，与 `798bc` `source.json` 一致 |
| `index.html` / `style.css` / `SOURCES.json` | 与 `source.json` 一致 |
| `http.rs` / `protocol.rs` | 与 `source.json` 一致 |
| 最终候选二进制 | `/tmp/homeway-webserve-final-r4-hjxlvg_q`，receipt 为 `798bc6a1…685d` |
| 浏览器证据 | `exit.txt`=0；80KiB 逐字节/Hash、>2MiB 零粘贴载荷、A/B 旧回调、CJK/🙂 width2、宿主剪贴板未变、cleanup 残留为空 |

第三方实际文件 SHA256 **全部匹配** `SOURCES.json`：

- `xterm.js` `1f991ac…7495`，`xterm.css` `ba8e698…f2b6`，license 匹配；
- `fit.js` `bdaefa3…7089`，license 匹配；
- `unicode11.js` `b0c3be5…48d0`，license 匹配。

`integrity` 字段未在本轮独立下载 npm tarball 重算，受网络/沙箱限制；本轮只证明“本地实际字节与声明 SHA256 一致”，不把它当作完整供应链审计。

## 三、逐条问题

| ID | 位置 | 问题 | 建议 | 严重程度 | 是否阻塞 |
|---|---|---|---|---|---|
| **F-1** | `app.js:92-106`，另 `:125-135` | `onerror`、服务端 `error`、`exit` 的通知会被随后的 `onclose` 无条件覆盖为“连接已断开；工作会话保留”。没有 `ready`/`closed` 状态：WS 首帧/握手失败时 form 已隐藏、终端为空，仍显示会话保留；断连后 terminal 仍可聚焦，输入被 `onData`/`sendInput` 静默丢弃，必须点“断开”才能重新登录。 | 增加明确状态机（authenticating/connecting/ready/closed）；保存 `lastError`；未 ready 时直接回登录并显示具体错误；ready 后仅保留输出、禁用 stdin/加遮罩；区分正常退出、鉴权失败、目标撤销和网络断开。 | 中 | **是（本聚焦门条件项）** |
| **F-2** | `app.js:66,99-103,115-123` | `inputInterrupted` 只在 `ws.send` 同步抛异常时置位；WebSocket 网络中断通常走异步 `onclose`，不会抛异常。因此大粘贴刚发出后断网，页面只显示普通“连接已断开”，不会提示目标可能收到部分输入；这与 `docs/WEBSERVE.md:103` 的“传输中断可能已发送部分输入，页面会提示”不完全一致。 | 记录“本连接已发送过非空输入”；异常关闭时保守提示“若刚发送输入，目标可能已收到部分内容”；或增加输入批次 ACK（更重）；否则应缩窄文档承诺。 | 中 | 否（须合入前处理或缩窄文案） |
| **F-3** | `app.js:56-57`；`unicode11.js:1`；`Cargo.lock:2324` | 浏览器启用 Unicode11 addon，而 Rust 侧依赖 `unicode-width 0.2.2`（声明 Unicode 17）。两套宽度表不同版；Unicode 11 宽表在 `U+1F972`（129394）处留空，xterm `wcwidth` 会返回 1，而 Rust 17 口径下这是 emoji presentation 字符。现有浏览器只验证了回/家/🙂，不能据此宣称所有 CJK/emoji/组合字符与 Rust VT 一致。 | 若要求严格对齐，生成与 Rust 同表的 xterm Unicode provider；否则明确记录“Unicode11 口径”并加已知差异测试，不把“width2”扩大为全量 emoji 承诺。 | 中 | 否（但不得写“全量一致”） |
| **F-4** | `pty.rs:87-94`；`http.rs:412-420` | 管理流 PTY 的 ERROR code 1..6（协议/禁用/忙/IO/控制面/撤销）在 WebSocket 层被折叠成固定 `target_error` + 1011。设计要求的“已认证后可区分终端禁用、资源忙、目标失联”没有到浏览器；前端也无法给出可行动提示。 | 保留脱敏固定文案，但按 code 映射不同 WS error/close 语义；补 code 1/3/5/6 的浏览器边界测试；与 F-1 的状态机同批处理。 | 中 | 否 |
| **F-5** | `http.rs:181-191`、`:197-205`；`protocol.md:72` | Host 不合法、query 存在时的 early 403/400 在统一添加响应头之前返回，缺少 `no-store`、CSP、`nosniff`；协议要求所有响应含 no-store。 | 提取统一响应包装，early reject 也走同一头集合；补负例断言。 | 低 | 否 |
| **F-6** | `app.js:39-47`；`http.rs:181-191` | 前端对所有 HTTP 状态都直接 `response.json()`；Host/Origin 的 403、408/413、500 等可能是空 body，解析失败后只显示 JS 的 `Unexpected end of JSON...`，不是可行动错误。 | 容错解析：`json().catch(()=>({}))`，`!response.ok` 时按 status + 固定文案，不再依赖 JSON 一定存在。 | 低 | 否 |
| **F-7** | `http.rs:201` | CSP 为 `style-src 'self' 'unsafe-inline'`；脚本仍是严格 `script-src 'self'`，无 CDN，但“严格 CSP”表述并不完全成立。 | 验证 xterm 是否确实需要 inline style；若不需要则收紧；若必须，记录例外，并补 `form-action 'self'` 等收口指令。 | 低 | 否 |
| **F-8** | `tools/webserve-smoke.py:581-604`；`app.js:111,127` | >2MiB 实测使用纯 ASCII，命中的是 `data.length > inputLimit` 早拒分支，**没有实际覆盖“UTF-8 字节 + bufferedAmount”分支**；也没有精确 2MiB/2MiB+1、异步发送中断、OSC52 点击复制、Firefox/Safari/Windows/Linux、原生大粘贴或双页面同时使用。测试通过不能证明这些边界。 | 增加 emoji 重载输入（UTF16<2MiB、UTF8>2MiB）、精确 2MiB、模拟 bufferedAmount/`ws.send` 异常、OSC52 点击路径和跨浏览器矩阵；明确记录未覆盖平台。 | 低（验收证据缺口） | 否 |
| **F-9** | `app.js:146-156`，另 `:67-80` | `navigator.clipboard.writeText` 不可取消：旧目标复制请求可能在切到新目标后才完成，内容仍写入浏览器共享剪贴板；另若复制等待期间收到新的 OSC52，旧 success 可能清掉新 `pendingClipboard`。未发现写入新目标终端或新目标 input 的路径。 | 捕获点击时的 pending 值，仅在同一值仍存在时清除；把“浏览器剪贴板是全局的、跨标签共享”写入已知边界；不宣称跨目标剪贴板隔离。 | 低 | 否 |

### F-1、F-2 的具体判断

- **F-1 是本聚焦门唯一硬条件。** 成功路径不受影响，但“失败/重连路径”是本轮重点，目前既丢失具体错误，又留下一个看似可用、实际静默丢弃输入的终端；按最终放行标准应修。
- **F-2 的同步异常提示本身是准确的**：`catch` 置 `inputInterrupted` 后再 `ws.close()`，并提示“目标可能已收到部分内容”；问题在于它只覆盖同步异常，不覆盖更常见的异步断链。不要把它宣传成“所有传输中断都有提示”。
- **超限预检本身没有半截副作用**：`sendInput` 在 `for` 循环之前完成总预算检查，拒绝路径不会调用 `ws.send`；这一点是静态可证的。

## 四、已核查且未发现问题的方面

### 1. 输入预算、字节与分帧

- `bytes.length + ws.bufferedAmount <= 2MiB` 的检查顺序正确：先判单次输入是否超过上限，再判“当前待发送 + 本次输入”是否超过；`inputLimit - bytes.length` 不会下溢。
- 合格输入按 `offset += 65536` 顺序分帧，每帧最多 65536B；`Uint8Array.subarray` 保持连续字节序。
- 服务器 `protocol.rs` 的 INPUT 上限为 65536，`pty.rs` 以原始字节 `extend` 并顺序写入 PTY，不在帧边界做 UTF-8 解码；因此跨 UTF-8 码点分帧在协议/PTY 语义上是允许的。
- xterm 的 `Utf8ToUtf32` 带 `interim` 状态，服务器 OUTPUT 跨消息切断码点时，浏览器解码仍可续接。QA 的 80KiB 粘贴中 emoji 跨 65536 边界、目标原始 81920B/SHA 一致，是实测支持。
- 客户端拒绝超限时不会发第一帧；发送循环中的同步异常会关本轮连接并提示可能部分送达。**异步断链提示问题见 F-2。**

### 2. 代际与异步隔离

- `generation` + `socket === ws` + `terminal === ui` 的组合能拦住旧 fetch、旧 WS open/message/error/close、旧 terminal data/binary/resize。
- 旧 paste Promise 在 `await` 后同时检查 `terminal` 和 `generation`，不会把旧目标文本送入新目标。
- `close()` 会 abort pending login、清空 pendingClipboard、关闭旧 socket、dispose 旧 terminal；A/B 实测的旧 paste/output/error/close 未污染新目标。
- 未发现旧目标输入进入新目标、旧目标输出写入新 xterm、旧状态覆盖新状态的可利用路径。**旧 copy 的异步全局剪贴板副作用见 F-9；失败状态机见 F-1。**

### 3. 剪贴板与 OSC52

- OSC52 handler 只把内容放入 `pendingClipboard`，不调用 `navigator.clipboard`；读取请求 `?` 被忽略；编码长度和 32768B 解码上限存在。
- 浏览器 `copy`/`paste` 只在按钮点击时调用剪贴板 API；没有自动写、自动读、localStorage 或 cookie 路径。
- 服务端 PTY 固定设置 `HOMEWAY_WEB_TUI=1`，`tui/clipboard.rs` 因此走 OSC52/终端路径，不调用 `pbcopy`/`xclip`/`xsel`；`application_write` 拒绝非 WRITE 的读取类载荷。
- 本轮证据显示宿主 pasteboard `changeCount` 未变；这只覆盖本次 macOS headless Chrome，不推广到所有平台。

### 4. Unicode、resize、CSP、资产

- Unicode11 addon 已加载并激活；实测回/家/🙂 均为 2 列，Mac Option 拖选、resize、真实 copy/paste 通过。
- `index.html` 只加载 `/xterm.js`、`/fit.js`、`/unicode11.js`、`/app.js` 和本站 CSS；未发现 CDN、远程字体或远程脚本。
- CSP 的脚本面为 `script-src 'self'`，无 inline script；`frame-ancestors 'none'`、`base-uri 'none'`、`object-src 'none'` 存在。
- 所有第三方实际文件 SHA256 与 `SOURCES.json` 匹配；xterm/fit/unicode11 的 MIT 许可已随仓保存。
- **宽度口径问题见 F-3；style-src 放宽见 F-7。**

### 5. HTTP/WS 接口配合

- 前端 auth 帧字段为 `type/ticket/cols/rows`，与 `http.rs` 的 `Auth` 一致；resize 为 `type/cols/rows`，与 `Control` 一致；方向为客户端 binary INPUT、服务端 binary OUTPUT、文本仅 auth/resize/error/exit。
- 线级 frame/message 上限 65536，前端分帧 65536，PTY INPUT/OUTPUT 上限 65536，PTY 每向缓冲 2MiB，四者没有发现字节/方向矛盾。
- 首次 WS 帧 2KiB/5s、未认证 WS 8、总界面 8、TCP 32、票据 30s、一次性、HTTP/1.1、禁 keep-alive、Host/Origin allowlist 均在代码中可见。
- Token 不进入 URL、cookie、localStorage；票据只存在页面内存并发送后删除；页面文本走 `textContent`，终端输出只交给 xterm，未发现 XSS 注入路径。
- **HTTP early header 缺失见 F-5；错误码折叠见 F-4；非 JSON 错误解析见 F-6。**

### 6. 简洁性与多余设计

- `app.js` 170 行，没有引入额外状态库或前端框架；分帧、预算检查、代际判断集中在少量函数内，整体可读。
- 服务端 WS reader 有界 channel、发送 10s 超时、终端背压、PTY 2MiB CAP、reader abort、endpoint drop 等回收路径方向正确。
- 未发现明显的过度设计；`data.length` 的 UTF-16 早拒与后续 UTF-8 精算有冗余，但前者用于避免超大字符串先编码，属于合理防御。

## 五、实测事实、静态证明与未覆盖点

| 类别 | 内容 |
|---|---|
| **本轮核读到的实测证据** | 798 候选/source 资产匹配；80KiB 粘贴目标原始 81920B、SHA `90a9bd…591b` 一致；65536+16396 两帧；emoji 跨 65535 边界；>2MiB 页面拒绝且 0 粘贴载荷帧；A/B 旧 paste/output/error/close 被拒；CJK/🙂 width2；Mac Option/resize/copy/paste；宿主剪贴板未变；cleanup 剩余为空。 |
| **静态证明** | 2MiB 检查发生在任何 send 之前；64KiB 分帧与协议/PTY 上限一致；PTY 原样字节写；xterm UTF-8 解码有跨消息状态；旧 fetch/WS/terminal/paste 均有 generation/identity guard；OSC52 不自动写/读；`HOMEWAY_WEB_TUI=1`；CSP 脚本仅 self；资产 SHA256 匹配；HTTP/WS 字段和方向一致。 |
| **未覆盖/不能由本轮证明** | UTF-8 字节+bufferedAmount 分支、精确 2MiB/2MiB+1；异步断链半截提示；F-1 的错误状态机；OSC52 点击复制与超限/坏 base64；两个标签页同时使用；Firefox/Safari/Windows/Linux/移动端；全量 emoji/组合字符宽度；npm tarball 的 SHA512 integrity；完整后端协议/资源/准入；r3 或完整代码门结论。 |

## 六、最终意见

1. **F-1 必须先修**，否则本聚焦门只能记为“有条件通过”，不能记为最终通过。建议与 F-2、F-4 在同一批处理，因为它们共同决定“错误是否可见、是否可行动”。
2. **F-3 必须缩窄声明**：Unicode11 不是 Rust `unicode-width 0.2.2` 的同表实现，不能宣称所有 emoji/CJK 宽度一致。若要强一致，需要做 provider 对齐。
3. F-5、F-6、F-7、F-8、F-9 可作为低风险整改项；其中 F-8 是验收证据缺口，不应把现有测试通过写成对所有边界/平台的覆盖。
4. 前端核心数据路径可判为通过：超限无半截发送、64KiB 分帧、跨 UTF-8 码点字节流、PTY 原始字节语义、代际隔离、OSC52 显式确认、宿主剪贴板隔离、self assets/CSP 脚本面均有静态或实测支撑。

**本报告是 798 最终前端增量的独立补充门，不是整体代码门通过；r3/完整后端代码门的结论不由本报告改写。** 本轮未修改源码、未提交、未启动实例、未重跑测试。


## 本轮实现方逐项判断与处理计划

以下为完整原文读取后给出的判断，不改写上节外评意见；本报告作者未改前端产品源码。

- **F-1 认同**：`app.js` 的 `onclose` 无条件调用 notice，且只恢复 connect.disabled，没有使terminal不可输入或返回登录。建议以少量连接状态与保留最后错误修复，避免引入状态库；由前端负责人处理并补失败/关闭回归，完成后重新聚焦复评。
- **F-2 认同**：同步 `send` catch不能证明异步断链已经完整送达。采用保守提示即可，不引入ACK/自动重放；不得把浏览器send完成当目标应用完成。
- **F-3 认同版本差异与证据范围**：当前明确选择Unicode11仅验证具体CJK/emoji样例，不能推导和Rust宽度表全量一致。建议记录真实表版本及已知差异，不以单个样例扩大保证；若产品要求完全同表需另行生成provider并验证，不能本轮口头宣告解决。
- **F-4/F-5 原评审位置的问题成立，但存在并行整改**：本轮报告落盘时当前http.rs已出现terminal_error按固定code映射及统一headers包装。此处不能将外评先前读取的旧代码结论当新代码复核结论；须由完整代码门复核相应整改和测试。
- **F-6 认同**：空/非JSON HTTP失败可能仅暴露JSON解析错误。应容错读取并保留固定状态文案。
- **F-7 部分认同**：脚本CSP确为self且不加载CDN；style-src unsafe-inline是显式例外，不能称所有指令均无inline。xterm运行时设置样式，不应未经浏览器验证便收紧；先明确脚本与样式策略边界，再验证可收紧指令。
- **F-8 认同证据缺口**：现有真实ASCII超限主要覆盖UTF16早拒，静态检查正确不等于实测UTF8/bufferedAmount分支。增加对应边界定向用例；本轮不把未测试浏览器/平台标通过。
- **F-9 部分认同**：点击授权的旧copy已启动后，浏览器API不可取消，完成可作用于共享剪贴板；这是浏览器全局资源边界，未发现其流入新目标终端。清pendingClipboard可用点击时捕获值比对修复，避免覆盖后续OSC52请求；文档应准确限制“跨目标隔离”的范围。

优先顺序：先修F-1并补失败/重连证据；同批处理F-2/F-6/F-9轻量提示与异步值保护；确认并行F-4/F-5整改；F-3/F-7精确记录口径，F-8补实际输入预算边界。最终通过取决于整改后独立复评，不由本文件提前放行。
