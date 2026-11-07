# 独立 WebServe 整改代码门增量复评

- 完成时间：`2026-11-08T02:37:26.591554+08:00`。
- 命令：本仓根目录 `dsh --profile headless`，真实退出码 **0**。
- 原始材料：`/tmp/dsh-review/r5-webserve-remediation.gbb3ld36/{prompt.txt,output.md,stderr.log,exit.txt,started.json}`。
- 输出已全文完整读取；原始output SHA256：`759fc83f7674486c2317b96c7f5c205ea319d6c1a954349f8ee2aeebb50ef491`。
- 仓内正文仅归一化原输出第3,6行末Markdown硬换行空白以通过默认diff检查；原始字节与上述SHA保留，本文不声称归一化正文逐字节相同。结论/问题/建议未改。
- 外评：**整改静态代码门通过；总代码门/交付门有条件通过**。下文完整保留该结论，独立证据补核另列，不能以exit0当代码门通过。

## 完整外评正文

# 结论

**本轮整改的静态代码门：通过。**
未发现 B1/B2/M/L/F 范围内新的阻塞缺陷；r3 原文的 B-1/B-2 两个高严重度阻塞、r4 原文的 F-1 前置阻塞，均有对应代码整改与边界证据。

**总代码门/交付门：有条件通过，不能由本轮静态复核单独宣布无条件全绿。**
条件不在“还没跑全部平台”，而在：主任务的新全 CI/cross 与固定新件复验尚未由本轮独立完成；且 B2 的“仅 control TCP 重连、同 epoch/同代 WSS 全链不断”没有独立故障注入，只能保留为证据边界，不能把 relay restart 或静态 22 项单测冒充该场景。

---

## 一、B1/B2

### B-1：闭合

- `tui/backend.rs:300-304,318` 保存 `no_spawn`；`backend.rs:493-536` 首次 Refresh 的 bootstrap 与 `Worker::client` 共用 `dial_control()`；`backend.rs:604-618` 在 `no_spawn=true` 时只走 `ControlClient::dial_with_timeout`，不再调用 `dial_control_spawn_quiet`。
- `daemon_cli.rs:331-341` 在真实 `start_spawned_process` 入口按 state 计数；`backend.rs:1033-1057` 用真实 UDS 曾监听后关闭、首次 Refresh 等待 bootstrap 完成，断言该 state 的 spawn 计数为 0。
- `tui/process_input.rs:14-103` 新增唯一 poll/read worker，4 槽有界有序 channel；`tui/mod.rs:108-125,168-175` 主循环每拍查 `control_guard.is_closed()`，输入等待最多 25ms；`process_input.rs:87-102` Drop 先关 receiver、置 stop，75ms 有界等待后不再无限 join。
- `webserve/pty.rs:133-152` 真实 Web PTY 固定 `tui --no-spawn --state ...` 且设置 `HOMEWAY_WEB_TUI=1`；普通 TUI 仍走原主线程 `event::poll/read`。
- B1 边界：卡在 crossterm PTY EOF 内部读的 worker **本身不可取消**，依赖该独立 CLI 进程正常析构 `TerminalGuard` 后进程退出、由 OS 终止 worker；这不是库级可取消模型，也不承诺父死亡后仍保留输入线程。该边界已在设计/登记中写明。

### B-2：闭合

- `server/bind.rs:687-704` 将 `detach_leg_by`（只摘 socket/双表）与 `remove_leg_by`（撤销 live leg 和 admission memory 的 lease）分开；`clear_legs()` 只对已认证控制重连调用 `detach_leg_by`，并先从 live leg 重建 memory。
- `bind.rs:500-566` 只对首次认证 remote + 完整 admission marker + 同 id/sid 复用 owner；同 id 但换 remote/cookie、新代、RELEASE、TTL、Drop 均撤销；没有无证跨 remote 继承。
- `bind.rs:604-637` 活腿上的冗余 memory 过期只丢 cache，只有 detached memory 过期才 cancel；`bind.rs:760-784` TTL 对 live remote 不再误踢活腿。
- 生产调用链：`relayleg.rs:559` 仅在控制面认证 OK-MAC/开放模式通过后发 `LegsClear`；`engine.rs:1627` 是唯一生产消费点；没有把“重连即强制撤销所有管理连接”改写成需求。
- 定向日志读取到 `bind 22 passed / 0 failed`，覆盖 40 轮、同代移交、clear/replay、gap RELEASE、错 remote/cookie、network+management 共存、TTL 活腿/缓存分离、sid 精确匹配、32 memory 淘汰和 Drop。
- B2 边界：**没有**“只断 control TCP、不重启 relay、不改变 WSS 全链”的独立集成故障注入；不能把 relay restart 或 clear/replay 单测等价成该场景。静态 owner 生命周期与真实 UDP clear/replay 数据测试支持闭合，但该证据缺口应继续保留。

---

## 二、M1–M5、L1–L9

| 项 | 结论 | 依据与保留边界 |
|---|---|---|
| M-1 | 闭合 | `host.rs:211-223` stop 清 `reason` 并置 phase 0；`host.rs:256-259` disabled 不再被历史 start 失败覆盖；真实占端口 start 失败后 stop 的 Host 测试存在。 |
| M-2 | 闭合 | `http.rs:240-286` 按 `WebClientError` 固定：Authentication/Revoked→401，Capacity→429，Deadline→504，Relay/Unavailable/Closed/Io/Transport→503，Protocol→400；测试含 secret 哨兵不回显。 |
| M-3 | 闭合 | `http.rs:288-305` 的 `terminal_error` 将 PTY ERROR 1..6 映射为 protocol_error/terminal_unavailable/busy/terminal_io/control_unavailable/revoked，固定 1002/1008/1013/1011；`http.rs:509-525` 发送固定 `error` 文本后再关闭。 |
| M-4 | 闭合，保留物理边界 | `credential.rs:89-100` 直接编码到 Zeroizing；`host.rs:177-184` owned String move 进 Value；`daemon/server.rs:341-364,1284-1290` 专用 `SecretFrame` 直接序列化并队列/写后擦；`daemon/client.rs:108-151` `RawRsp` 无 Clone/Debug，pending/notify/未知 corr/成功入队后 rx 丢弃都走 Drop 擦除；`webserve/mod.rs:152-157` 取 owned String 入 Zeroizing 打印。明确不承诺 serde/allocator 重分配/成功交付后调用方 Value/OS/终端全链物理清零。 |
| M-5 | 闭合 | `http.rs:217-231` 先显式拒 `@`；`config.rs` 只调用纯 authority 检查，不导入 Web HTTP 类型；user@127.0.0.1/localhost 负例和真实 early 头测试存在。 |
| L-1 | 闭合 | `http.rs:181-211` guard 在应用层包住所有响应，early 403/400 也走 `secure_response`（no-store/CSP/nosniff）；库在应用层之前拒绝的线级坏报文仍属库边界。 |
| L-2 | 闭合 | `http.rs:365-410` 只对完整 JSON 中唯一、可解析的 `ticket` 做 best-effort discard；坏语法/歧义/未知关联不猜取，回到原有 30s/8 总额度；测试覆盖可识别释放与坏 JSON 不误删。 |
| L-3 | 闭合 | `credential.rs:30-104` 的 `TokenError::{UnsupportedVersion,Malformed}` 对独立 Python oracle 分类；HTTP 固定 400 code/message，不回显原串。 |
| L-4 | 闭合 | `check-quic-isolation.sh:221-301` 覆盖普通同步源码的 axum/hyper/hyper_util/tungstenite 命名空间、manifest 别名负例、CLI 精确 Web 声明、CAPI 依赖闭包和 Web 单线程禁多线程；`http.rs` 是唯一异步例外。未发现新的同步面越界。 |
| L-5 | 闭合 | `relay/mod.rs:540-545` registry 只发布 `verified` 或 `ctl_verified && ctl.is_some()` 的腿；UDP verified 腿保留，ctl-only 断 ctl 后隐藏但已有 assoc 保留；`relay/mod.rs` 的真实 TCP-only/active assoc 测试和 stop 清空测试存在。 |
| L-6 | 闭合 | `credential.rs:110-152` 对同一打开 File 校验普通 0600，再限 54B 读；`credential.rs:154-195` 临时文件显式 0600、sync、rename、父目录 sync，`BeforeRename` 保旧、`DurabilityUncertain` 区分可见提交；Host rotate 的 readback 成功装新但返耐久错误、readback 失败 clear/fail-closed，均有 Host 回归。 |
| L-7 | 闭合 | `daemon/proto.rs:106-142` 新增 `WebFailure`/`BackendErr::Web` 保留 source；`daemon/server.rs:1736-1755` 映射既有控制词表；`webserve/mod.rs:61-91` 分类；`RecoveryRequired` 固定“管理认证已暂停”文案，测试覆盖不泄漏 secret。 |
| L-8 | 闭合 | `host.rs:77-91,133-145` 保存 agent handle，stop 后 join，Drop 亦回收；`ServiceIntake::accept` 本来按 200ms poll 预算返回 WouldBlock，外评“永久阻塞”前提不成立；idle shutdown 真实 join 测试 <1s。 |
| L-9 | 闭合 | `exit/web.rs:70-100` 外层 `tokio::select!` 已用 `conn.accept_bi()`/`conn.closed()` 取消认证 future；真实 TLS 下连续 LegMissing≥3 后客户端 close，1s 内 active=0、lease 全失活、`regs_accepted=0` 的新增反证存在；没有重复加产品 select。 |

---

## 三、F1–F9

| 项 | 结论 | 依据与保留边界 |
|---|---|---|
| F-1 | 闭合 | `app.js:34-195` 已有 authenticating/connecting/ready/closed 状态：ready 前失败回登录并显示具体错误；ready 后 `readOnly()` 禁 stdin/paste、保留输出、按钮变“重新连接”；error/exit 保存后再由 close 使用；r6 真实/受控失败用例覆盖 1008、空 503、受控 error/exit、真实 restart 断链。 |
| F-2 | 闭合 | `app.js:137,149-174` 记录 `sentInput`，异步 close 在非正常/非 exit 时追加“部分输入可能已送达”；同步 send 异常另行 `inputInterrupted`；文档已改为“可能已发送部分输入”。 |
| F-3 | 闭合为口径边界 | `app.js:70-71` 启用 Unicode11 addon；Rust `unicode-width` 与浏览器 Unicode11 不同表，QA 明确只验证回/家/🙂 等样例，不承诺全 emoji/组合字符同宽。 |
| F-4 | 闭合 | 即 M-3：PTY ERROR 1..6 固定分类/关闭码已透传到 WS；浏览器端会收到固定 code/message，而不是统一 target_error+1011。 |
| F-5 | 闭合 | 即 L-1：early 403/400 与所有应用响应统一 no-store/CSP/nosniff；库层线级坏报文边界保留。 |
| F-6 | 闭合 | `app.js:51-56` 对 `response.json()` 做容错空对象，非 2xx 按固定 status 文案；空 503 真实/受控浏览器测试通过。 |
| F-7 | 基本闭合，保留低风险记录缺口 | `http.rs:204` 已加 `form-action 'self'`；`style-src 'self' 'unsafe-inline'` 仍保留，理由是 xterm 运行时样式，不能未经浏览器验证收紧。该例外只在 r4 处置/评审记录中解释，产品文档未说明，且 implementation 记录仍有“严格 CSP”措辞，见新问题 N1。 |
| F-8 | 闭合为“新增覆盖 + 如实声明未覆盖” | `tools/webserve-smoke.py:587-607` 覆盖 UTF16 早拒、UTF8 字节超限、受控 bufferedAmount、真实 80KiB、OSC52 点击、双页面、受控 send 异常；QA:187 明确未测精确 2MiB 接受边界、Firefox/Safari/移动端/Linux 运行面。没有把受控响应/属性当自然网络或真实拥塞。 |
| F-9 | 闭合，保留文档边界 | `app.js:197-208` 捕获点击时 `pending`，仅当同一值仍存在才清除，旧 copy 不会清掉新 OSC52；浏览器剪贴板是全局资源、旧 copy 不可取消，代码不宣称跨目标状态隔离。`docs/WEBSERVE.md:100-104` 未明确“跨标签共享”，见新问题 N3。 |

---

## 四、新问题（均非阻塞）

| ID | 文件:行 | 原因 | 最小建议 | 严重度 | 阻塞 |
|---|---|---|---|---|---|
| N1 | `crates/homeway-cli/src/webserve/http.rs:204`；`docs/reviews/2026-11-08-webserve-implementation.md:32` | `style-src 'self' 'unsafe-inline'` 仍是例外，但解释只在 r4 处置文字中；implementation 记录仍写“严格 CSP”，与当前 CSP 不完全一致 | 在 CSP 旁加简短注释、在 `docs/WEBSERVE.md` 明确“script-src 保持 self；style-src unsafe-inline 为 xterm 运行时样式例外”；不要在未做浏览器验证前收紧 `style-src` | 低 | 否 |
| N2 | `tools/webserve-smoke.py:587-607`；`docs/reviews/2026-11-08-webserve-qa.md:187` | 未覆盖精确 `2MiB` 接受与 `2MiB+1` 字节拒绝；现有是 UTF16 早拒、UTF8 超限和受控 bufferedAmount | 补两条真实浏览器剪贴板用例：精确 2MiB 接受、2MiB+1 在首帧前拒绝；保留“未造真实拥塞”声明 | 低（证据缺口） | 否 |
| N3 | `docs/WEBSERVE.md:100-104`；`app.js:197-208` | 代码已保证旧 copy 不清新 pending，但文档没有说明浏览器剪贴板是全局、跨标签共享，旧 copy 完成后仍可能写全局剪贴板 | 文档加一句“浏览器剪贴板为全局资源，跨标签共享；页面只保证不把旧 copy 结果写入新目标终端/输入” | 低（文档边界） | 否 |
| N4 | `app.js:136,149-154` | `onerror` 会先写通用 `lastError`，`onclose` 优先使用 `lastError`；若某浏览器对服务端 1008/1013 先发 error 再 close，可能遮蔽 close-code 的“鉴权失效/资源忙”具体文案 | 将 close code 的固定文案与通用 `lastError` 分开保存，有 typed close 文案时优先；补一次浏览器事件顺序测试。当前 r6 真实 1008 测试通过，故仅列条件性低风险 | 低/条件性 | 否 |

---

## 五、Rust 形态与类型不变量

- **未发现新的逐行移植痕迹或多余 `Arc<Mutex>`**：B1 用 `no_spawn` 字段、std thread + 有界 `sync_channel`、`Arc<AtomicBool>`，没有把整个 TUI 塞进共享锁；B2 的 `LegOwners`/`AdmissionMemory` 保持私有，detach/revoke 是两个语义不同的方法；M4 的 `RawRsp` 无 `Clone`/`Debug`，`SecretFrame` 是专用队列项，敏感路径没有先复制成通用 JSON。
- `TokenError`/`PersistError`/`WebFailure`/`BackendFailure` 均为 thiserror 类型；`BackendErr::Web` 保留 `#[source]`；错误边界没有退回字符串错误。
- 可见性整体收窄：`Token`/`Identity`/`PersistError` 为 `pub(super)`/`pub(crate)`，`RegistryHandle` 只向 CLI 暴露读面，`WebAuth::clear` 是 quic/web 的窄 API。
- M4 的 `SecretFrame` 借用序列化、`RawRsp` 的 pending/notify 统一 wrapper、`erase_consumed_prefix` 的“先擦前段、再移位、再擦旧尾”符合 Rust 所有权/借用形态；未发现为清零而引入的全局锁或后台常驻线程。
- 明确保留的边界：B1 卡死 reader 依赖进程退出；M4 不保证 serde/allocator/内核/终端全链物理清零；F3 不保证 Unicode 全表同宽；F8 不保证所有平台/精确边界。

---

## 六、证据边界（实读、实测、未覆盖分开）

### 已实读/静态核对

- 全文读取了两份评审原文、r3 独立处置与主实施逐项回应、r4 原文与逐项处置；读取时 `code-review.md`/`qa.md` 仍在追加 r6 段，原 r3/r4 正文未被改写；我以当前工作区快照为准。
- 完整读 `app.js`、`index.html`、`style.css`、`SOURCES.json`；第三方 minified 仅按来源/哈希/语法口径，不深读供应链。
- 当前产品文件与 r6 固定候选 `source.json` 逐文件比对：225 项中仅 `crates/homeway-core/src/daemon/tests.rs` 不同，且 `test-only-delta.json` 明确这是候选构建后新增的真实控制 UDS 测试；其余产品文件（含 `app.js`、`http.rs`、`process_input.rs`、`server.rs`、`client.rs`、`bind.rs`）与候选一致。候选二进制 SHA256 为 `1c955a412687c7ac85e5f5a83fb96dbe82de60edf34296ebe652a1aba3909085`，产品 source SHA 为 `9885b68e3794d1a4a0a95c1d7c385934736a7e01b9e506f07dc86499dc3e9589`。
- 只读读取到定向日志：CLI 254/0、core-Web 16/0 与 18/0、B2 bind 22/0、credential 4/0、WebAuth 19/0、input 2/0、clippy/isolation 最终 exit0；r6 完整 `--browser --extended` exit0、parent focus exit0、三个交叉 `cargo check` exit0。
- 保留的既有失败未隐藏：`ea2afd` 完整轮 exit1、父 EOF 孤儿失败与 sample 仍保留；`r3-fixes/check.exit=101`、`input-tests.exit=101`、`clippy-r3.log` 等早期失败也在目录中，后续 `check-r2`、`input-tests-r2`、`clippy-r4` 才 exit0。不能把最终绿反写成“从未失败”。
- r6 `ci-local.log` 在我读取时没有终止 `exit` 文件/行，不能据此宣称完整 CI 已由本轮验证；r6 三个交叉 `.exit` 为 0，但这是 `cargo check` 日志，不等价全平台运行。

### 本轮未执行

- 未修改、未提交、未启动实例、未重跑 cargo test/clippy/ci-local、未重跑浏览器/40 轮、未做完整跨平台构建或运行。
- 未独立注入“只断 control TCP、同 epoch/同代 WSS 全链不断”的故障；B2 只到静态 owner 分析 + 22 项含真实 UDP clear/replay 数据测试。
- 未测精确 2MiB/2MiB+1 接受边界，未测 Firefox/Safari/移动端/Windows/Linux 运行面，未测原生 IME、所有 emoji/组合字符宽度，未独立下载 npm tarball 重算 SHA512。
- 未把 M4 的局部 Zeroizing 写成物理内存全擦证明；未把 r6 exit0 当成总代码门通过。

---

## 七、最终判定

- **本轮整改静态代码门：通过。** r3 的 B-1/B-2、r4 的 F-1 均已从“阻塞”降为“闭合”；M/L/F 其余项未发现新的阻塞缺陷。
- **总代码门/交付门：有条件通过。** 条件主要是主任务的新全 CI/cross 与固定新件最终复验，以及 B2 未注入的 control-only WSS 全链场景作为保留证据边界；F7/F8/F9/F3/M4 的低风险记录与证据缺口不新设代码阻塞，但不得被写成已全部覆盖。
- **不以 r6 exit0、定向日志绿或本轮静态通过替代上述总门条件。**

## 独立补核与逐项处置（外评之后）

原文已提交主任务转交用户。原评审时尚未取得完整CI退出码，现独立补核条件如下：

- 固定r6 `1c955a412687c7ac85e5f5a83fb96dbe82de60edf34296ebe652a1aba3909085` 的完整
  `--browser --extended` 实际exit0；`/tmp/homeway-web-r6-full-l6ifik1l/{output.log,exit.txt}`。
  真实parent-loss聚焦亦exit0，三张最终截图均独立查看，清理receipt剩余=[]。
- `/tmp/homeway-webserve-final-r6-ti3poq6d/ci-local.exit` 为0；
  `ci-local-receipt.json`明确command/exit0/837.12s。关键日志含workspace CAPI4、CLI256+5integration、
  core976、QUIC314通过，clippy -Dwarnings、20/20导出、OHOS .so2943760B、RRR317s及CI全绿终行。
- 同目录4份cross退出码均为0：OHOS、aarch64 musl、x86_64 musl、Windows MSVC；Finished日志已核。
  这是编译检查，不扩大为跨平台运行验收。
- 候选后仅daemon/tests.rs真实UDS token/RecoveryRequired test-only增量已独立读，且CI日志该用例为ok；
  `test-only-delta.json`记录前后SHA，不改变候选产品行为。
- B2的control-only TCP/WSS全链故障没有额外注入；仍保留该范围边界，已用源码/22项含真实UDP
  clear/replay数据测试证明owner生命周期，不拿relay restart等价替代。

N1/N3为非产品行为的文档补足；N2精确预算缺口与N4条件性浏览器文案优先级须明确列为保留边界，
不伪称已测试或已修；待主任务最终处置后写总门结论。

### 最终四项非阻塞处置

| 项 | 独立处置 |
|---|---|
| N1 | 接受，已在docs/WEBSERVE.md明确script-src仅self、style unsafe-inline为xterm运行时样式例外；implementation记录的“严格CSP”同步改为准确脚本/样式口径。未改http.rs或收紧已验产品策略，候选hash不变。 |
| N2 | 保留已披露证据边界：未实测精确2MiB接受/+1拒绝。现有UTF16早拒、UTF8字节超限、受控buffer预算、跨UTF8的80KiB完整SHA均实际通过；不扩大为所有数值/平台边界覆盖。此项原文明确非阻塞，本批不将未测边界写作通过。 |
| N3 | 接受，docs/WEBSERVE.md明确剪贴板全局/跨标签共享、已发copy不可取消；页面只保证不将旧异步结果自动写入新目标终端/输入，不承诺共享剪贴板隔离。 |
| N4 | 保留条件性低风险：未改冻结产品，真实Chrome首auth1008已通过；其它浏览器若error先于typed close，具体文案可能被通用错误遮蔽。未声称已修或全部事件顺序均实测，既有只读/重新登录仍成立。 |

### 最终独立总代码门结论

**通过，保留已披露的非阻塞边界。**

此结论在完整读取r3、r4、r5原文后给出，原r5的“总代码门有条件通过”原文保持不变。
其主任务证据条件现已独立核实满足：固定r6完整QA/parent焦点实际exit0，CI receipt/退出码0，
四cross退出码0，源码225项逐文件比对仅有已登记并被CI实际执行的daemon/tests.rs测试增量；
固定生产者二进制与QA复制件均SHA256 `1c955a…9085`一致。审计证据
`/private/tmp/homeway-web-vct1cmt4/final-gate-conditions.json`记录实际核对时间和唯一test差异。
独立再次运行OpenSpec strict有效、默认git diff --check实际exit0。

r3的B-1/B-2、r4的F-1及全部M/L/F必改已闭合，无剩余必改。
B2没有额外control-only TCP/WSS全链故障注入、N2/N4、Unicode11差异、跨平台运行与M4物理全擦等
边界明确保留，不冒充已覆盖，也不按外评明示非阻塞的项新设交付阻塞。
N1/N3只改文档，不改变已验产品hash；这不是发布/部署授权。
