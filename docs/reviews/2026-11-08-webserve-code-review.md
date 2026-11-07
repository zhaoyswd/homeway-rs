# 独立 WebServe 代码门（完整外评与处置）

- 实际完成时间：`2026-11-08T01:39:11.464613+08:00`。
- 评审命令：本仓根目录 `dsh --profile headless`，真实退出码 **0**；不是以 output 文件存在判断成功。
- 原始文件：`/tmp/dsh-review/r3-webserve-code.mprbwvox/{prompt.txt,output.md,stderr.log,exit.txt,started.json}`。
- 输出全文已完整读取，SHA256 `aecfa6823f30b13955e134bdde6b1e7aaa7cf0ca3462f27b16889ccafe9dbc54`。下节原文逐字保留。
- 范围：规划基线 bd00c0c 到本轮未提交实现，包含意向新增与未跟踪材料。评审运行中新增的最终 app.js 大粘贴增量由独立前端补门另评；最后8+1管理回收后普通准入test-only由本评审者逐行补核。
- **外评结论：不通过；B-1/B-2 为高严重度阻塞。** 退出码0只说明评审命令成功，不代表代码门通过。

## 外评原文（完整）

# 代码门结论

**不通过（当前工作区不能判为代码门通过）。**

我按 `bd00c0c..当前工作区` 重新读源码判断，不沿用两轮技术门结论。技术门 B1–B8 的代码落点大体可见：B1/B2/B3/B5/B6/B7 基本闭合；**B8 的 `--no-spawn` 仍未覆盖全部后台拨号路径**，且管理 owner 在“控制面重连/同代重登”路径上存在 lease 误撤销风险。这两项属于高严重度阻塞，必须先整改并复评；其余为可随批整改的中低项。

本次只做静态只读检查：未改文件、未提交、未启动实例、未跑完整测试/浏览器/CI。另执行了资产 SHA256、Node 语法解析、独立 Python 向量 oracle 和 fixtures 全目录 `shasum -c`，结果见后文“证据边界”。

---

## 一、阻塞问题

### B-1（高，阻塞）TUI 的 `--no-spawn` 没有覆盖冷启动 bootstrap 的自动拉起路径

**位置**

- `crates/homeway-cli/src/tui/backend.rs:487-519`，尤其 `:498`
- `crates/homeway-cli/src/tui/backend.rs:313-320`、`:592-599`
- `crates/homeway-cli/src/tui/mod.rs:107-108`

**问题**

`Backend::start_with_no_spawn` 只把 `no_spawn` 传给三个 `Worker`，`Backend` 本身没有保存该字段。`Backend::send(Command::Refresh)` 在首次 Refresh 时另起一个 bootstrap 线程，无条件调用：

```rust
crate::daemon_cli::dial_control_spawn_quiet(&state, "homeway-tui-bootstrap")
```

该函数内部是 `no_spawn=false` 的自动拉起路径。TUI 主循环只在每轮顶部检查 `control_guard.is_closed()`，因此存在窗口：control 在检查之后、首次 `poll_refresh()` 之前关闭，子 TUI 就可能拉起一个新的统一进程，然后自身因 control guard 关闭而退出，留下未被父 Web PTY 管理的守护进程。若 `<state>/webserve.toml` 仍为 enabled，这个新进程还可能重新启动 HTTP 入口。

这直接违反设计的 B8 和 `spec.md` 中“每条管理界面连接必须拥有禁止自动拉起守护的界面进程；control 不可达时仅退出本界面”的要求。`web_pty_tui_command_is_fixed_no_spawn_and_missing_control_has_no_side_effect` 只测了 `Ui::spawn` 缺 control 的情况，没有覆盖 TUI 首次 Refresh 的这个路径。

**建议**

- 在 `Backend` 中保存 `no_spawn`，bootstrap 线程与 `Worker::client` 共用同一个 `dial_control(no_spawn)` helper；`no_spawn=true` 时只能 `ControlClient::dial_with_timeout`，失败只产生“本机服务未就绪”并让界面退出，不调用 `dial_control_spawn_quiet`。
- 补一条真实测试：启动 Web PTY 后杀掉/关闭 control，让 TUI 的首次 Refresh 发生，断言没有新统一进程、没有新监听、TUI 自行退出。
- 如果暂时不能改，必须明确登记豁免；但按现有 spec 不能宣称 B8 闭合。

**严重程度：高；阻塞：是。**

---

### B-2（高，阻塞）控制面重连时的 `clear_legs` 会取消活动管理 lease，使同代重登无法真正移交 owner

**位置**

- `crates/homeway-core/src/server/bind.rs:459-463`（`clear_legs`）
- `crates/homeway-core/src/server/bind.rs:467-470`（`remove_leg_by` 无条件 `lease.cancel()`）
- `crates/homeway-core/src/server/bind.rs:319-337`（同代移交只在 `old.remote == remote` 时清旧 owner）
- `crates/homeway-core/src/server/relayleg.rs:559`（控制面重连先发 `LegsClear`）
- 对应测试：`crates/homeway-core/src/server/bind.rs:142-157`（只覆盖同一 remote 的 `register`，不覆盖 `clear_legs`）

**问题**

`clear_legs` 的逻辑是“保存 `admitted_generations` → 逐腿 `remove_leg` → 恢复 memories”。但 `remove_leg` 会走到 `remove_leg_by`，对每条腿执行：

```rust
for lease in leg.owners.management.values() { lease.cancel(); }
```

`ManagementLease` 是共享 `Arc<AtomicBool>`；取消后，即使随后把同名 `admitted_generations` 恢复，memory 中的 lease 也已经是 false。控制面 TCP 重连后 `LegsClear` + 同代 `SESSION` replay 时，目标上的 `Held.lease` 观察到 false，`exit::web::run` 的 `invalidated` 会以 `0x75` 关闭管理流；浏览器侧的 `WebClient` 随之失活。这与设计 §5.1“同代重登移交 owner、不得误删仍有效拥有者”不一致，也使 `tasks 3.4` 的“同代移交”没有真实覆盖。

另外 `register_leg_with_admission` 只在 `old.remote == remote` 时清旧管理 owner；同 id、同 marker、但 relay data port 变化的同代重登会走 `remove_leg_by` 取消旧 owner，而新腿仍从 memory 克隆同一组 lease，结果仍是“新腿上的 owner 已死”。这类场景当前没有测试。

**建议**

- 将“拆腿资源”和“撤销 lease”分开：`clear_legs`/同代重登应保留仍存活的 `ManagementLease`，只移除 socket/绑定；等 generation 变化、目标明确 stop、或 lease 自身失活时才 cancel。
- 或者把 owner 存成 `conn_id -> liveness` 的独立表，腿只引用 conn_id；腿重建不取消 lease，由 lease 的 RAII 决定。
- 补测试：`LegsClear` 后同代 replay 仍保留活动管理 session；同 id/new remote 同代重登；network owner + management owner 共存；控制面重连不误杀 management。
- 如果设计本意是“控制面重连就撤销所有管理连接”，应修改 design/tasks 并登记，不得保留“同代移交”表述。

**严重程度：高；阻塞：是。**

---

## 二、非阻塞问题

| ID | 位置 | 问题 | 建议 | 严重 | 阻塞 |
|---|---|---|---|---|---|
| M-1 | `crates/homeway-cli/src/webserve/host.rs:155-161`、`:211-223` | `webserve.stop` 置 `phase=0` 但不清 `reason`。若此前 start 失败（`reason=Some`），stop 后 status 仍返回 `"failed"` 而不是 `"disabled"`，且 `enabled=false` | stop 时清 `reason`；或在状态机中明确 `disabled` 优先于历史 reason | 中 | 否 |
| M-2 | `crates/homeway-cli/src/webserve/http.rs:215-223`（`host.rs:256-259`） | `Error::Remote(_)` 全部映射 401；目标 `Capacity` 应为 429，目标 `Unavailable/Relay/Deadline` 应为 503/504。`protocol.md:68` 明确要求 429/503 | 按 `WebClientError` 变体映射：Authentication/Revoked→401，Capacity→429，Unavailable/Relay/Deadline→503/504 | 中 | 否 |
| M-3 | `crates/homeway-cli/src/webserve/http.rs:414-420`；`pty.rs:87-94`；`protocol.rs:81-86` | PTY 的 ERROR code（1 协议/2 禁用/3 忙/4 IO/5 控制面/6 撤销）在 WebSocket 层被丢弃，统一发 `target_error` + 1011；spec 要求已认证后可区分终端禁用、资源忙、目标失联 | 将 code 映射为固定 WS error code/message 或受控脱敏文本；补 code 1/3/5/6 的浏览器边界测试 | 中 | 否 |
| M-4 | `crates/homeway-cli/src/webserve/host.rs:129-134`；`mod.rs:115-121` | `Token::encode()` 的 `Zeroizing<String>` 被 `json!({"token": &**token})` 复制进普通 `serde_json::Value` String，控制面响应和 CLI 端都留下不可擦除副本，违反 C5/C7“token 字符串使用可擦除缓冲” | 为 token 响应使用专用 secret 响应类型，序列化后清零；至少限制并登记该副本 | 中 | 否 |
| M-5 | `crates/homeway-cli/src/webserve/config.rs:71-82` | `Host` 的本地分支只检查 `Authority::host()`/port，不拒绝 userinfo；`user@127.0.0.1:18080` 这类非法 authority 可能被接受。spec 要求非法 authority 拒绝 | 拒绝 authority 中的 userinfo/`@`，或先用 URL/Authority 完整校验；补 `user@127.0.0.1` 负例 | 中 | 否 |
| L-1 | `crates/homeway-cli/src/webserve/http.rs:181-191` | Host/query 的 early 403/400 在加 header 之前返回，不带 `no-store`/CSP；`protocol.md:72` 要求所有响应含 no-store | 提取统一响应包装，early 返回也加 no-store/CSP | 低 | 否 |
| L-2 | `crates/homeway-cli/src/webserve/http.rs:297-305` | 首帧 JSON 解析失败时直接 1002，没有 `gateway.discard` 解析出的 ticket；首帧失败未释放票据，票据最长滞留 30s（占用 8 名额之一） | 解析失败时 best-effort 提取 ticket 并 discard；或先解析 ticket 字段再判其余 | 低 | 否 |
| L-3 | `crates/homeway-cli/src/webserve/credential.rs:36-48` | 所有 token 错误都是 `Error::Invalid(&'static str)`，没有 `UnsupportedVersion` / `Malformed` 类型；与 protocol 的错误分类不完全对应，也不利于调用方区分 | 增加 `TokenError` enum（UnsupportedVersion/Malformed），再映射 HTTP 状态 | 低 | 否 |
| L-4 | `tools/check-quic-isolation.sh:250-264` | “唯一异步源码白名单”只对非 `http.rs` 文件禁 `tokio/quinn/rustls/async`，不禁 `axum::`/`hyper::`/`tungstenite::`。当前代码干净，但门不是 fail-closed；未来普通 CLI 文件可只导入 axum 类型而绕过 | 将 `axum::|hyper::|hyper_util::|tungstenite::` 加入普通同步面负例；保留 `http.rs` 的 QUIC/TLS/多线程禁用 | 低 | 否 |
| L-5 | `crates/homeway-core/src/relay/mod.rs:545`、`:1425-1444` | ctl-only 腿在控制连接关闭但仍有 active assoc 时，`ctl_verified` 不清、腿不摘；registry 仍发布该路由直到 `leg_timeout`，新登录会路由到不支持新 assoc 的目标。测试只覆盖“无 active assoc 时 drop ctl 即删” | 快照对 ctl-only 要求 `leg.ctl.is_some()`，或控制关闭时清 `ctl_verified`；补 active-assoc 关闭测试 | 低 | 否 |
| L-6 | `crates/homeway-cli/src/webserve/credential.rs:85-98`、`:139-140` | `atomic_write` 只 fsync 临时文件，rename 后未 fsync 父目录；`read` 不校验 0600。掉电后 rename 可能回退，读路径接受宽松权限 | rename 后 fsync 父目录；read 检查/告警 mode 非 0600 | 低 | 否 |
| L-7 | `crates/homeway-cli/src/unified_cli.rs:804` | `Host::operation` 的 typed `Error` 被 `BackendErr::Other(e.to_string())` 字符串化，控制面错误类型/可操作性丢失，属于 AGENTS 反对的字符串错误边界 | 为 webserve 建立 `BackendErr` 映射，保留 code/类别 | 低 | 否 |
| L-8 | `crates/homeway-cli/src/webserve/host.rs:77-91` | Web agent 线程没有 `JoinHandle`，`Host::shutdown` 只置 stop，不 join；若线程阻塞在 `intake.accept()`，只能靠进程退出或下一次 intake 唤醒 | 保存 handle，shutdown 关闭 intake/唤醒并 join；或明确进程级退出即可 | 低 | 否 |
| L-9 | `crates/homeway-quic/src/exit/web.rs:178-200` | `LegMissing` 重试循环只等 oneshot 和 sleep，不监听 `conn.closed()`；客户端在重试期断开时，Held/lease/名额最多保留到 5s 总期限。设计要求连接断开也取消 | 在重试 loop 内 select `conn.closed()`，或让外层取消信号传入；补重试期断开测试 | 低/中 | 否 |

---

## 三、按①–⑫逐项判断

| 项 | 结论 | 依据/缺口 |
|---|---|---|
| ① 需求闭环/角色独立启停/web-only 不启出口 | **基本通过** | 独立 `webserve.toml`；`webserve start` 自动拉起统一进程但默认 `serve/relay=false`；`webserve stop` 不停止出口/中继；已有 config 不改。M-1 状态语义和 B-1 的 no-spawn 仍需修 |
| ② 稳定离线 token/锁/热装/rotate | **基本通过** | 离线锁下稳定读写；运行期经 control；`WebAuth` 共享句柄支持热装；rotate 撤销 grant。M-4/L-3/L-6；未复跑 8 并发/占锁测试 |
| ③ token 非 VPN 授权/不任意拨地址/身份钉定/目标认证 | **通过（静态）** | `Token` 不含网络 secret；远端只查本机 relay registry 的完整 pubkey；`WebDial` 只拨 registry 实际地址；TLS RPK pin；目标 HMAC+exporter 认证。L-5 是路由陈旧边界，不构成认证绕过 |
| ④ 精确 wire/独立向量/无旧协议污染 | **通过（静态）** | `W1/WC/WP/WA`、hww1、PTY 帧与 fixtures/oracle 一致；我实测 `gen-webserve-vectors.py --check`、68 项 `shasum -c` 通过；W1 在 `legacy_why` 前分流，旧 reg4/ALPN 未改。L-3 是错误类型缺口 |
| ⑤ 首 stream/管理握手与 generation 回执 | **基本通过** | 首流 5s 绝对期限；nonce/exporter；Held 8；`LegMissing` 20ms 同代重试；`Confirmed` 后才 Accept；额外 bidi/DATAGRAM 处理在源码可见。B-2、L-9 仍需处理 |
| ⑥ 票据单用/统一限额/会话隔离 | **部分通过** | ticket 随机 32B、一次性、30s、清理；login/ticket/active 统一 8；未认证 WS 另 8；目标侧 Held 8。M-2/M-3/L-2 使错误可区分性和容量状态不完整 |
| ⑦ Host/Origin/proxy trust/CSP/认证前资源与超时大帧 | **基本通过** | 只绑 127.0.0.1；Host/Origin 精确 allowlist；不信任 Forwarded；CSP/no-store/nosniff；头/体/总期限；WS 64KiB；slowloris/大帧测试可见。M-5/L-1；close code 的个别语义可再核对 |
| ⑧ cancel/backpressure/RELEASE/lease/owner/network-owned/relay epoch/cutoff | **不通过** | reaper 先 Drop WebClient 再 release、cutoff/epoch/AssocKey、32 队列、目标 lease 兜底等方向正确；但 **B-2 的 `clear_legs`/同代 lease 误撤销**直接破坏 owner 生命周期 |
| ⑨ PTY 进程/FD/错误和关闭收尾/工作会话保留 | **不通过（因 B-1）** | PTY 非阻塞 poll、2MiB、SIGHUP 500ms→SIGKILL 500ms、真实 PTY 测试和 reaper 方向正确；但 `--no-spawn` 未覆盖 bootstrap，且没有父/control 死亡的无孤儿实测 |
| ⑩ 浏览器异步跨目标/clipboard/CJK/鼠标 resize/Windows | **基本通过** | app.js 完整读过：generation 代际、fetch abort、socket/terminal 校验、copy/paste 代际、OSC52 仅暂存、Unicode11、Mac Option、resize、CSP 页面结构均合理；真实 Chrome 证据在 QA 记录中。未复跑；未覆盖两页面同时打开、真实 CJK 输入、OSC52 点击路径 |
| ⑪ CAPI 依赖隔离/CLI current_thread/兼容默认配置/helper | **基本通过** | CAPI 仅 core，无 axum/hyper；CLI 只有 `webserve/http.rs` 使用 current_thread；默认 config false；helper 私有 `/tmp` 校验。L-4 是门本身的缺口；未复跑 `cargo tree`/旧基线兼容 |
| ⑫ 是否地道 Rust | **基本通过** | thiserror、`#[non_exhaustive]`、secret newtype、`Zeroizing`、窄锁/窄句柄、借用解析、无大 `RolesInner` 塞 HTTP/PTY，整体不是逐行移植。M-4/L-3/L-7 以及 `Host::status` 的超长 `json!` 是形态/边界改进点；未发现系统性 Arc/Mutex 滥用 |

---

## 四、看过且明确未发现问题的方面

以下均为静态阅读结论，不等于已复跑：

- **wire/夹具**：`fixtures/webserve/*` 与 `tools/gen-webserve-vectors.py` 独立、无 Rust 调用；token/管理/终端向量长度、字节序、错误负例合理；`fixtures/SHA256SUMS` 68 项实测校验通过。
- **旧协议保持**：W1 在 `hr-reg4` 的 legacy 判定前分流；旧 reg4、TLS/RPK、无 ALPN、relay 信封/控制帧、服务 tag 的代码路径未被改写。
- **管理不创建设备**：`exit/web.rs` 只做 auth、bridge 确认和 intake，不调用 `table.register`/`bridge.bind`；DATAGRAM 在未绑定连接上丢弃；额外 bidi 关闭。
- **token 与联网凭证隔离**：`WebSecret`/`WebCredential` 与网络 `TokenSecret` 分离；管理认证不构造 `IslandCredential`；hww1 不含网络 secret。
- **目标最终认证**：relay 只按完整 pubkey 路由；entry 不本地 HMAC 判成功；`WebClient` pin 目标 RPK；目标 `WebAuth` 用 exporter/nonce/HMAC 验证。
- **relay 身份快照**：`Handover/CtlConn` 保留已验完整公钥；pending 不可见；ctl-only 可见；stop/control close 清理；`web_registry_*` 测试覆盖关键边界。
- **关闭顺序**：reaper 先 `drop(client)` 再提交 `release_association`；cutoff 在关闭时捕获；队列满有可见记录；目标 lease 兜底，不以队列成功代替资源回收。
- **PTY 收尾**：每界面独立 PTY/尺寸；写读在 worker 线程；关闭 master、SIGHUP、500ms 后 SIGKILL、500ms wait/reap；不向用户工作会话发信号。
- **浏览器页面**：token 不进入 URL/cookie/localStorage；输入清空；终端输出只交 xterm；OSC52 不自动读/写剪贴板；CSP 只加载自身资源；Mac Option 强制选中；Unicode11 启用。
- **依赖/平台**：CAPI Cargo 只依赖 core；CLI Web 依赖限定在非 Windows 目标；Windows `webserve` 在写 state 之前明确 unsupported；统一进程默认 `serve/relay=false` 已订正。
- **helper/私有验收**：`local-rust-*` 的 UNIFIED 分支只使用 `/tmp` 私有 state，路径 resolve 后校验 `/tmp` 父目录；smoke 不存在生产 state 访问。
- **准入取消保护**：`Reg4Request::is_active`、core 触表前检查、`wait_engine_reply` closed 优先、`Bridge.bind` 锁内最后检查均有代码和测试；边界“检查后已开始的同步事务不回滚”保留，未错误声称全局原子。

---

## 五、证据边界（不要把测试绿当静态结论）

我实际执行且只读的检查：

- `git status` / `git diff bd00c0c` 文件盘点；
- 资产 SHA256 对 `SOURCES.json`，全部匹配；`node --check` `xterm.js`/`fit.js`/`unicode11.js`/`app.js` 通过；
- `python3 tools/gen-webserve-vectors.py --check` 通过；
- `cd fixtures && shasum -a 256 -c SHA256SUMS` 通过，68 行；
- 全量阅读自有 `webserve` Rust、app.js/index/style；第三方 minified 只核来源/hash/语法，未深读。

我未执行、不能据此证明：

- 未跑 `cargo test`、clippy、`ci-local`、OHOS/musl/Windows 交叉；
- 未启动任何实例，未复跑 85ae 的 `--browser --extended`，未复跑 40 轮资源循环；
- 未独立下载 npm tarball 复核 `integrity` SHA512；只验证 `SOURCES.json` 的 SHA256 与本地字节；
- 未用旧基线二进制实测 `webserve.toml` 回退；
- 未实测 B-1 的 control 死亡、B-2 的控制面重连、M-3 的错误码、M-2 的 429/503；
- 未实测两个浏览器页面同时使用、真实 CJK 输入、OSC52 点击复制、Windows/musl 运行面。

因此，提供的 `2026-11-08-webserve-qa.md` 和 `/tmp/.../output.log` 只能作为已知证据上下文，不能替代上述静态缺口。

---

## 六、建议的最小整改顺序

1. **B-1**：把 `no_spawn` 纳入 `Backend` 的统一拨号入口，彻底删掉 bootstrap 的 `dial_control_spawn_quiet`；补 control 死亡/父进程死亡测试。
2. **B-2**：把 `clear_legs`/同代重登的“拆腿”和“撤销 lease”分离；补 `LegsClear` + replay、同代新 remote、network + management owner 测试；明确控制面重连语义。
3. **M-1/M-2/M-3**：修状态 reason、HTTP 429/503、PTY ERROR code 透传；这三项直接影响可观察性和用户排障。
4. **M-4/M-5/L-2/L-6**：token 副本、Host authority、malformed ticket、原子写目录 fsync；属于安全/可靠性边界。
5. **L-4/L-5**：补隔离门和 ctl-only 快照门；当前代码未越界，但门禁应 fail-closed。
6. 整改后重跑：相关单测、clippy、ci-local 适用项、三平台交叉，以及固定候选的完整浏览器/40 轮烟囱；再给代码门结论。

**总评**：架构方向是对的，管理面没有把 HTTP/PTY 塞进核心，认证和资源模型也基本成立；但当前“代码完成”还不能等同于“代码门通过”。B-1 是明确的 no-spawn 边界破坏，B-2 是 owner/lease 生命周期与已通过设计不一致，建议按 **不通过，整改后复评** 处置。


## 独立处置记录

原文已交主任务转交用户；以下为外评之后的独立判断，不改写上面的外评结论。

当前总代码门：**通过（2026-11-08，保留已披露非阻塞边界）**。最终结论见[r5评审记录末节](2026-11-08-webserve-code-review-r5.md)；r3/r4/r5外评原文与历史不通过/有条件通过状态均保留，最终通过来自其后独立补核完整QA、CI/cross和固定候选。
B/M/L各项待主实施者逐条回应后登记，不将非阻塞意见默认为已修。

### 逐项独立初核（整改前，不改写原文）

| ID | 独立判断与证据 | 处置状态 |
|---|---|---|
| B-1 | 认同。Backend仅Worker持no_spawn，首次Refresh确有独立spawn_quiet路径。必须封闭全部拨号并验control关闭不拉起。 | 阻塞，待修复/复核 |
| B-2 | 认同clear_legs主问题：保存memory不能复活已cancel的共享lease。同代同remote对账需保护存活owner。原文附带跨remote继承则需保持认证remote+generation边界；当前was_admitted/owners本就按remote查，不能为迁移盲目复制。 | 阻塞，待probe最小修复/复核 |
| M-1 | 认同，stop未清历史reason。 | 待lead处置 |
| M-2 | 认同，Remote类型化Capacity/Unavailable被401吞掉，与protocol429/503不符。 | 待lead处置 |
| M-3 | 认同，HTTP WS层应保持固定脱敏ERROR分类与忙1013。 | 待lead处置 |
| M-4 | 认同存在普通JSON String副本；应区分可控持久secret与现有通用控制协议的瞬时序列化副本。需要具体最小擦除措施和真实能力边界，不能用Zeroizing局部宣称全链清零。 | 待lead具体方案 |
| M-5 | 认同显式拒绝userinfo/@并补负例。 | 待lead处置 |
| L-1 | 认同所有响应no-store约定，应统一包装早返回。 | 待lead处置 |
| L-2 | 部分认同。能识别到有效ticket时可discard；语法破损/未知ticket不能安全知道归属，未经认证WS还没有绑定目标。禁止通过正则猜取，需明确无法识别时由既有30秒/8总额度回收。 | 待lead明确实现/记录 |
| L-3 | 认同TokenError枚举有利类型分类，保持脱敏即可。 | 待lead处置 |
| L-4 | 认同门应覆盖axum/hyper等依赖命名空间，带负例校准。 | 待lead处置 |
| L-5 | 认同ctl-only应要求当前control存在；UDP verified腿不受影响。 | 待probe处置 |
| L-6 | 认同父目录fsync增强持久化。read权限应结合web目录0700和文件0600明示处理，避免读取后才宽松通过。 | 待lead处置 |
| L-7 | 部分认同。Host内部已thiserror类型化；通用控制BackendErr本来需要协议code边界，Web映射应保留可行动类别，不以字符串代替内部错误链。 | 待lead处置 |
| L-8 | 不认同“accept可无限阻塞、需下一次intake才退出”的前提：exit/intake.rs:283文档和wait_ready明确ACCEPT_POLL_BUDGET到点WouldBlock，Host循环复查stop。因此未证永久线程泄漏；保存handle并join仍是可做的收尾增强。 | 待lead是否增强/证据 |
| L-9 | 不认同“断线只等5秒”的前提：exit/web.rs:86认证外层select一直等conn.accept_bi；quinn关闭后该future返回Err，立即return并drop认证future/Held，cancel活位。可用LegMissing期间断线定向测试补直接证据。 | 待probe定向证据 |

### 最后test-only补核

已独立逐行读取 `exit/tests/web.rs:271` 新增
`web_management_saturation_recovers_before_ordinary_reg4_registration` 和测试桩默认拒绝/显式网络准入开关。
8条实际管理流活跃、第9Capacity，Drop后active=0且lease全失活，随后同端点真实TLS H4/C4/P4/A4，
regs_accepted才从0到1。定向1/0与leaf clippy原日志已完整读取：
`/tmp/homeway-web-management-recovery-test.log`、`/tmp/homeway-web-management-recovery-clippy.log`。
这是此压力序列后可恢复证据，不声称TLS识别W1前资源完全隔离，也不声称Envelope桩代替真实中继CLADM。

### B-2 / L-9 整改复核进行中

B-2已独立读最小detach/revoke实现及clear/replay、多manager+network、clear间隙RELEASE/取消、
错remote/cookie、Drop、32memory淘汰测试。保持首次认证remote+原generation，未采用外评附言的
无条件跨remote继承；clear的唯一生产调用是已认证控制重连对账，需求未改成重连强制撤销。

独立复核还发现并要求修复一个整改中新回归：AdmissionMemory的TTL清理若无条件cancel共享lease，
活跃超过90秒的管理连接也会因冗余cache过期被踢。现源码已区分活腿与detached memory，
活腿仍持owner时只丢cache，只有detached memory过期才cancel；新增
`web_lease_live_leg_outlives_redundant_memory_ttl_but_detached_memory_expires`。
此处待最终21项实际日志和clippy后再给B-2结论，未把此前20/0当新修改已过。

L-9定向反证已完成：真实TLS下连续>=3次LegMissing后客户端close，1秒内active=0且全部lease失活，
管理模块15/0实际成功（`/tmp/homeway-web-b2-l9-web-all.log`，单测实际0.17秒由执行者记录）。
这与外层`conn.accept_bi()`在关闭后Err使认证future/Held被Drop的源码一致。
**L-9原“最多保留到5秒”前提不成立；保留新增回归，不增加重复取消机制。**

B-2 TTL修订最终证据已收：`/tmp/homeway-web-b2-bind-ttl-final.log` 21/0，
`/tmp/homeway-web-b2-l9-clippy-ttl-final.log` 实际exit0，完整日志已独立读取。
**B-2此轮独立源码/定向回归复核通过**，保持固定firstremote/generation并补齐cache TTL回归。
总代码门尚未通过：仍待B-1、M/L其余处置、新固定候选全QA及最终聚焦增量外评。

B-2最终std边界还补了memory.id与传入sid精确相等：旧CLADM marker不能配不同session id继承；
对应负例与最终22/0日志 `/tmp/homeway-web-b2-bind-sid-final.log` 已独立全读。
L-3/L-6的credential源码及4/0日志也已独立核：typed TokenError对独立oracle分类、同File0600/限54B读取、
文件fchmod/sync/rename/父目录sync，BeforeRename与DurabilityUncertain语义不混淆；
`WebAuth.clear` 的旧grant不复活19/0日志已读。最终owned clippy日志
`/tmp/homeway-web-b2-credential-clippy-final.log` 实际exit0。
此核查不替代Host在线故障读回/install/clear回归，M-4与L-6在线闭环仍待lead证据和新候选QA。


### 主实施逐项回应（2026-11-08，整改已落，最终复评尚未通过）

| ID | 判断与实际最小整改 | 已有定向证据与剩余验证 |
| --- | --- | --- |
| B-1 | 认同。Backend保存no_spawn，首次Refresh bootstrap及全部Worker共用dial_control；true只允许直接拨号。 | CLI真实UDS曾监听后关闭、首次Refresh；等待bootstrap完成，并在实际start_spawned_process入口按私有state计数断言0，不依赖test exe是否能启动daemon。cli-tests.log 254/0；新件parent_loss由QA另验，二者不混同。 |
| B-2 | 认同clear/replay主问题；部分接受跨remote建议，绝不跨remote继承认证。probe分detach/revoke、固定remote/完整marker/sid对账；明确RELEASE/换代/过期/Drop仍撤。 | bind22/0及TTL活腿/缓存分离、sid、gap RELEASE、同remote replay、跨remote不继承；原文不被改写，新件真实重连QA待跑。 |
| M-1 | 认同，stop清reason，disabled不再被历史start错误覆盖。 | 真实占端口start失败后stop的Host测试通过。 |
| M-2 | 认同，按WebClientError固定分类401/429/503/504；Protocol400，不透内部target错误。 | HTTP分类用例含Io secret哨兵不回显，CLI254/0；新件QA继续核。 |
| M-3 | 认同，PTY ERROR1..6映射固定protocol_error/terminal_unavailable/busy/terminal_io/control_unavailable/revoked，关闭1002/1008/1013/1011。 | 六类固定分类与脱敏测试通过；新件真实WS/browser错误状态补验。 |
| M-4 | 认同存在可控副本并整改。Token编码直接写Zeroizing；Host将owned String移入Value而非json宏clone；server立即移入Zeroizing专用借用应答、直接写最终SecretFrame，队列丢弃/写后擦；client Rsp body与读缓冲擦、未交付Web token擦；CLI取owned String入Zeroizing输出。 | core专用frame原控制JSON语义、未交付擦与非Web结果不变三项通过。进一步以RawRsp守卫覆盖pending/notify/未知corr以及send成功后rx超时丢queue；codec消费前缀/移动尾区及临时读块已擦。serde内部/历史重分配/内核/终端缓冲仍不承诺物理全链清零；不是全擦或物理内存证明。 |
| M-5 | 认同，authority先显式拒@；格式校验只在http.rs内，config不导入axum命名空间。 | user@127.0.0.1/localhost负例及真实HTTP early头测试通过。 |
| L-1 | 认同，guard所有应用响应含统一no-store/CSP/nosniff，early403/400同路。 | 真实坏Host/带query应答断言通过。HTTP库在应用层之前拒绝的线级坏报文仍由库处理，不自研HTTP。 |
| L-2 | 部分认同。完整JSON中唯一可解析字符串ticket可discard，尺寸/字段错与首帧文本超长亦释放；坏语法/无法识别/歧义ticket不正则猜取。 | 测试可识别失败立刻释放；坏JSON保持原30s/8总额度，显式discard后归0。 |
| L-3 | 认同，TokenError::{UnsupportedVersion,Malformed}，HTTP固定400 code，无原token/JSON体。 | 独立token正反oracle与redacted错误测试通过。 |
| L-4 | 认同，普通同步源码禁axum/hyper/hyper_util/tungstenite，manifest亦覆盖别名package且CLI只精确原声明。Web仍禁QUIC/TLS/多线程；URL authority纯检验迁回http.rs。 | 每一命名空间/manifest别名负例自校准，十一条隔离门实际exit0；CAPI未新增Web/async闭包。 |
| L-5 | 认同，ctl-only快照要求当前ctl存在；UDP verified腿不变。 | 真实TCP-only建立active assoc后关闭ctl，已有assoc保留而新登录路由消失，stop清空，core-web16/0。 |
| L-6 | 认同，读同File普通0600校验后限长读；rename后父目录fsync。typed BeforeRename保旧，DurabilityUncertain明确新bytes可见但不保证断电耐久。 | credential4/0；Host故障测试rename前旧disk/grant不变，rename后readback成功装新grant但仍返durability错误；readback亦失败才WebAuth.clear failclosed，修IO后重新获取恢复。绝不宣称所有失败全回滚。 |
| L-7 | 部分认同。原内部thiserror存在，但跨BackendErr字符串化丢类；新增WebFailure类别+source链，server映射既有控制词表与固定文案。 | source链保留/固定wire错误不泄漏secret测试通过；不新增控制错误值域。 |
| L-8 | 不认同永久阻塞前提：ServiceIntake已有poll预算返回WouldBlock；接受收尾增强，Host保存agent handle，stop后在预算返回处join，Drop亦回收，无额外detach线程。 | 无任何intake连接的idle shutdown真实join测试<1s通过，不以进程退出冒充线程回收。 |
| L-9 | 不认同原前提：外层accept_bi关闭会取消整个认证future/Held。无重复select产品改动。 | probe LegMissing期间客户端断开定向<1s回收已实测，管理leaf19/0，非等5秒期限。 |

日志：`/tmp/homeway-webserve-r3-fixes-7qzo_yzt/{cli-tests.log,core-web-tests.log,clippy.log,isolation.log}`均真实exit0；254 CLI、16 core-Web、workspace all-targets clippy与隔离11绿。probe日志`/tmp/homeway-web-b2-bind-sid-final.log`、`/tmp/homeway-web-credential-tests-final.log`、`/tmp/homeway-web-credential-auth-tests-r2.log`及相关clippy保留。本节仍非独立复评通过，固定新候选完整QA/CI/cross及最终复评待回填。


### r6最终固定候选与实施补证（代码门仍等最终r5外评）

- 固定件`/tmp/homeway-webserve-final-r6-ti3poq6d/producer/homeway`，SHA256 `1c955a412687c7ac85e5f5a83fb96dbe82de60edf34296ebe652a1aba3909085`；source.json/receipt同目录。候选构建后只增daemon/tests.rs的真实控制UDS回归，test-only-delta.json单列哈希，不声称全文件哈希未漂移。
- B1补充：ea2阶段真实parent_loss抓到crossterm PTY EOF内部忙环，使主loop原control_guard/STOP无机会复查。仅Web child（no_spawn且HOMEWAY_WEB_TUI=1）用ProcessInput单worker把poll/read隔离到有界4事件顺序channel，main每≤25ms仍查control；Drop关receiver+取消后75ms有界join，库内部不可取消时主路径正常析构TerminalGuard并结束独立CLI进程，OS终止该输入线程，普通TUI入口保持原读取。真实parent焦点10轮和r6全烟囱都过，非只用unit模拟。
- M4补充：RawRsp无Debug/Clone并包pending、notify、未知corr，队列成功入队后receiver丢弃也Drop擦；消费前缀及移位尾区擦除保护coalesced下一帧，fill临时读块与客户端buf是Zeroizing。新测试明确成功send→drop rx、下一frame字节/尾零、固定RecoveryRequired wire不隐去“认证已暂停”。前两类实测core-Web18/0，最后真实UDS回归加入最终CI。
- r6独立完整QA`/tmp/homeway-web-r6-full-l6ifik1l/{output.log,exit.txt}`真实exit0，state`/private/tmp/homeway-web-vct1cmt4`内binary-receipt、credential-races、hot-install、resource-cycles、browser-glyph-widths、browser-large-paste、browser-observations、browser-failures、browser-clipboard-receipt、parent-loss与cleanup-receipt.json；cleanup remaining=[]。两普通PNG与readonly-failure均经独立目视。覆盖原40轮、控制父死无新守护/监听、F1失败/只读与重连、2页、OSC52确认、80KiB/UTF8超额/受控buffer分支及原交互。
- 新依赖/独立oracle/SUMS/helper语法及CLI/CAPI feature图均实际exit0：`/tmp/homeway-web-deps-r6-yqpfz8uo/report.json`。最终完整CI/cross在r6目录进行，r5正式外评`/tmp/dsh-review/r5-webserve-remediation.gbb3ld36`仍运行；本节不替代最终通过结论。


r6最终CI/跨检补证：`/tmp/homeway-webserve-final-r6-ti3poq6d/ci-local-receipt.json`实际exit0、837.12s，workspace CAPI4/CLI256+5integration/core976/QUIC314、clippy/NDK真链接/20导出/size2,943,760B、隔离11/独立向量/SUMS/词表/WG/release及RRR317s均完成；E11仍是既有矩阵豁免，DNS为既有不适用。四目标cross均实际exit0，同目录每target.log/.exit：OHOS12.31s、musl两架构9.77/9.39s、Windows -D warnings5.92s。真实控制UDS `web_token_secret_and_recovery_categories_roundtrip_over_real_control_socket`已在完整workspace执行通过，RecoveryRequired固定wire确含“管理认证已暂停”，source secret无回显。5.2已按实际结果完成，5.3仍等最终r5正式复评结论。
