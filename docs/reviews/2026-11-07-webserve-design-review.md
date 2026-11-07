# 独立 WebServe 技术门外评原文（2026-11-07）

状态：**外评有条件通过，技术门尚未放行**。B1–B8 为阻塞开工项，需方案修订及独立复核；此处不将 OpenSpec strict 通过或 dsh exit 0 当作方案过门。

- 基线：`9a844f8`；评审时仅规划产物未提交，无产品代码改动。
- 外评工具：`dsh --profile headless`，在本仓根目录执行。
- 原始材料：`/tmp/dsh-review/r1-webserve-design.ndsa6qdk/{prompt.txt,output.md,stderr.log,exit.txt}`。
- **实际退出码：0**；已完整读取 output 全部 176 行，下面原文逐字保留。
- 只读评审，未启动生产实例，未执行 cargo 测试或构建。

## 外部 reviewer 原文

## 结论

**有条件通过（当前版本不能直接开工）**。架构方向可行，但有一个硬冲突和数个关键接口/状态未定义；下列 **B1–B8 为阻塞开工项**，全部处置并复审后才放行。P1/P2 是门禁与源码事实订正，C 系列可在实现阶段落实但必须进验收。

评审边界：基线 `9a844f8`，未提交内容仅 `openspec/changes/independent-webserve/` 与 `docs/reviews/2026-11-07-webserve-design.md`；未发现产品实现。`openspec validate independent-webserve --strict` 通过。本轮只读，未改代码/文档，未启动生产实例，也未运行 cargo 测试/构建。

---

## 一、阻塞开工项

### B1. `[webserve]` 进入严格配置表会破坏旧二进制与回退（高）

- **位置**：`openspec/changes/independent-webserve/design.md:26,95`；`crates/homeway-cli/src/serve_cli.rs:232-237`；`crates/homeway-cli/src/unified_cli.rs:55-69,147-171`。
- **问题**：`FileConfig` 是 `deny_unknown_fields`，而 `write_config_file` 会把整个 `FileConfig` 重新序列化。若按设计把 `[webserve]` 加进 `FileConfig`，则任何 `serve start`/`relay start`/`webserve start` 都可能写出该节；基线 9a844f8 的旧二进制随后会因未知字段拒绝启动，`design.md:95` 的“回退关闭 webserve 即可”和“兼容旧严格配置表”没有实现路径。即便 `enabled=false`，旧二进制仍会拒绝。
- **建议**：把 webserve 配置放到独立严格文件（如 `<state>/webserve.toml`），不进入旧 `FileConfig`；或至少用 `Option<FileWebserve>` + `skip_serializing_if` 保证未使用时永不出现在 `config.toml`。补一条基线兼容测试：`serve start`、`webserve token`、`webserve start/stop` 后，用基线二进制仍能 `load_config_strict`。
- **严重程度**：高（阻塞）。

### B2. 管理认证、`hww1`、终端帧缺少精确 wire 规格与夹具（高）

- **位置**：`design.md:34,60,78`；`tasks.md:7,12,13`；`docs/INTEROP-CRITERIA.md:4,599`；`fixtures/` 无 webserve 向量。
- **问题**：设计只给了字段和语义，没有给字节布局：`hww1` 的版本/字段顺序/校验算法/编码；管理握手的 magic `W1` 后各帧长度、版本、nonce/proof 帧格式、错误码/关闭码；PTY 的 OPEN/INPUT/RESIZE/OUTPUT/CLOSE 的 opcode、版本、长度和错误帧。两侧无法独立实现，也无法做逐字节回归；这与本仓“wire 字节以 `fixtures/` 为真源、判据变更须登记”的纪律冲突。
- **建议**：增加一节协议附录，固定所有帧长、端序、版本和错误码；新增 `fixtures/webserve/*`、`SHA256SUMS` 与正反向量；在 `docs/INTEROP-CRITERIA.md` 登记新判据；补错 magic/错版本/错长度/重放/超时测试。
- **严重程度**：高（阻塞）。

### B3. `RelayProc` 没有暴露实际 UDP 地址/端口（高）

- **位置**：`design.md:50`；`crates/homeway-cli/src/relay_cli.rs:126-137,260-300`；`crates/homeway-core/src/relay/mod.rs:400-424,457-468`。
- **问题**：`RelayProc` 只有 stop/join/exited 等字段，`assemble_relay` 的 `on_ready` 只打日志；实际绑定端口由 `Relay::run` 内部决定，且 `listen_with_fallback` 在端口占用时会 +1…+9 甚至随机端口。设计要求“固定拨本机中继实际 UDP 端口（wildcard 归一 loopback）”，但 webserve 当前拿不到这个实际地址，只能读到配置里的 `relay_listen`，端口退让时必然拨错。
- **建议**：给 `RelayProc` 增加就绪通道/字段，返回实际 UDP `SocketAddr`；把 wildcard 归一成 loopback 后供 `WebClient` 使用；`relay status` 也显示实际地址。补端口占用退让、`:0`、wildcard 三类测试。
- **严重程度**：高（阻塞）。

### B4. 登录票据容量、连接所有权与撤销生命周期互相矛盾（高）

- **位置**：`design.md:38,52,54,62,70`；`specs/webserve/spec.md:19`；`crates/homeway-core/src/server/bind.rs:242-246`。
- **问题**：设计说票据“绑定已认证远程连接，最多 32 张”，又说“管理连接全局最多 8 个”。若每张票据持有连接，32 张票就是 32 条目标管理会话，超过 8；若 8 是上限，32 张票就不可能都有效。设计还缺：票据过期/WS 首帧失败/OPEN 失败后谁关闭连接；中继 stop/restart、目标 token 轮换时，中继侧已持有但未消费的远程票据如何撤销；`8 张未认证 WS` 与 `8 个 WS/终端` 是否重叠。`spec.md:19` 要求轮换撤销未使用票据，但中继侧票据不在目标“同一撤销信号”内。
- **建议**：统一配额为“未消费票据 + 已消费终端会话 ≤ 8”，32 只作为短时无连接票据上限或明确等待队列；票据 RAII 持有连接，过期/消费失败/WS 失败即关闭；票据带中继 epoch 与目标连接 generation，中继 stop/restart 或目标连接关闭时原子清除；补 32 票据/8 会话、过期、WS 失败、中继重启、目标轮换测试。
- **严重程度**：高（阻塞）。

### B5. `confirm_client` 无成功回执，且中继腿注册与管理 HELLO 存在竞态（高）

- **位置**：`design.md:7,62,66`；`crates/homeway-core/src/server/bind.rs:238-250`；`crates/homeway-core/src/server/engine.rs:1692,1853-1865`。
- **问题**：`confirm_client` 当前返回 `()`，腿或 generation 不匹配时静默 `return`；管理要求“确认成功才 Accept”，但中继的 `LegRegister` 走控制面、管理 HELLO 走 QUIC 数据面，二者无顺序保证。正常远程登录可能因腿尚未登记而瞬时失败；也没有把“确认失败”作为显式回执返回给管理连接。`confirm_client` 的 `formal >= 32` 还与普通客户端共享额度。
- **建议**：改成类型化结果（成功/腿不存在/代际不符/配额满）；在 10 秒登录预算内做有界等待或重试，或增加“腿已就绪”握手；只有确认成功才写 Accept；补腿注册与 HELLO 交错、错代际、formal 满、超过 10 秒仍可交互的测试。
- **严重程度**：高（阻塞）。

### B6. HTTP/WS 缺总期限与帧上限，现有“有界”不完整（高）

- **位置**：`design.md:54,59,78`；`tasks.md:19`。
- **问题**：设计有 32 HTTP 连接、8 在途登录、5 秒首帧、4KiB 登录体，但没有 HTTP 头/体读取期限、请求总期限、keep-alive 期限；慢速头发送可长期占满 32 个连接。`axum` 的 WebSocket 默认帧/消息上限远大于 64KiB，设计只说“管理流单帧 64KiB”，没有约束 WS 传输层，客户端可在首帧前或之后发大帧。
- **建议**：显式设置 `DefaultBodyLimit::max(4096)`、HTTP header/body/request 总超时、WS `max_frame_size`/`max_message_size`（64KiB）、空闲/慢消费者超时；所有名额 RAII 归还；补 slowloris、超大帧、首帧前大帧、慢消费者测试。
- **严重程度**：高（阻塞）。

### B7. “可选管理装配”与“无论 HTTP 是否启用都接入”冲突，首个 token 的热接入未定义（高）

- **位置**：`design.md:28,60`；`crates/homeway-cli/src/unified_cli.rs:1341-1385`；`crates/homeway-core/src/server/engine.rs:786-796`。
- **问题**：出口 `ExitQuicConfig` 在 `ServeEngine` 启动时一次性装配。设计一边说“增可选管理装配”，一边说“统一进程无论 HTTP 是否启用均接入管理 worker”，又说“未铸 Web 凭证时明确拒绝”。如果没有 `web/auth.key` 时传入 `None`，目标先启动出口、后离线执行 `webserve token`，运行中的出口不会出现管理 worker，用户要求的“出口无需 HTTP 也能经中继管理”在首次取 token 的常见顺序下失效。
- **建议**：明确为“管理 worker 恒装配，持有一个可空但可热替换的 `WebAuth` 活句柄”；或增加 `webserve.install/rotate` 控制操作，在运行中安装新 Web 凭证。补“先启出口、后取 token、不启 HTTP，仍可远程 TUI”的测试。
- **严重程度**：高（阻塞）。

### B8. 子进程 `homeway tui --state` 会自动拉起统一进程，破坏“不隐式第二守护”与 kill/wait 边界（高）

- **位置**：`design.md:76,88`；`crates/homeway-cli/src/tui/backend.rs:585-593`；`crates/homeway-cli/src/daemon_cli.rs:421-497`。
- **问题**：管理 worker 直接 exec `tui --state`，而 TUI 的 `Worker::client` 在 control.sock 不可用时调用 `dial_control_spawn_quiet`，会自行拉起统一进程。正常父进程持锁时会被锁挡住，但父进程退出/重连窗口下，子 TUI 可能拉起同 state 的新进程；管理 worker 只 kill/wait 自己的子进程，可能留下孤儿守护。这与设计“不试图启动同 state 第二守护”“不向统一进程或其他进程组发信号”不一致。
- **建议**：给 TUI 增加 `--no-spawn` 或 `HOMEWAY_NO_SPAWN` 并让管理 worker 强制使用；子 TUI 在 control 不可达时直接退出；明确 kill 先用 SIGHUP、再在预算内 SIGKILL 的流程；补父进程死亡、control 断开、无孤儿进程测试。
- **严重程度**：高（阻塞）。

---

## 二、方案与源码一致性与门禁记录

### P1. 评审文档仍是占位，task 1.1 不能勾选（中，门禁阻塞）

- **位置**：`docs/reviews/2026-11-07-webserve-design.md:3,18-20`；`tasks.md:3`。
- **问题**：文档只有“待独立技术评审”，没有实际 reviewer 原文、退出码和问题处置；`openspec validate` 通过不等于技术门通过。
- **建议**：把本次评审原文、退出码、B1–B8 处置结果和复审结论写入该文档；在处置完成前不勾选 1.1，不进入产品实现。

### P2. “全新普通启动默认启用出口”与源码不符（中）

- **位置**：`docs/reviews/2026-11-07-webserve-design.md:15`；`crates/homeway-core/src/nodestate.rs:322,339`；`crates/homeway-cli/src/unified_cli.rs:1217`；`crates/homeway-cli/src/serve_cli.rs:22-37`。
- **问题**：实际生成模板是 `[serve] enabled=false`、`[relay] enabled=false`，`FileServe::default` 也是 false；评审文档却写“全新普通启动默认启用出口”，`unified_cli.rs:1217` 的事件串也写“serve.enabled=true”。这让 Web-only 的迁移理由和“显式关闭配置”的必要性建立在错误前提上。
- **建议**：订正评审文档和这条日志为 false；Web-only 仍保留显式初始化作为防御即可，不需要改变既有默认。补一个“生成默认配置 + 普通启动不装配出口/中继”的断言。

---

## 三、实现阶段必须落实的检查项

**C1. 中继快照的 ctl-only 身份边界（中）**  
位置：`design.md:70`；`crates/homeway-core/src/relay/mod.rs:172-178,1337-1345`。  
问题：`Leg` 同时有 `verified`（UDP PROOF）和 `ctl_verified`（TCP 控制面）；控制面先建的腿 `pubkey=[0;32]`。设计只写“完整 pubkey→label”，未说明 ctl-only 是否可见、是否可用于 Web 登录。  
建议：快照只收 `verified && pubkey != [0;32]`；明确“仅控制面注册不算完整 Web 身份”；按 `max_legs` 有界，stop/drop 用 RAII 清空，补 pending/ctl-only/过期/stop 测试。

**C2. 中继不得替目标做最终认证（中）**  
位置：`design.md:50,72`；`spec.md:26`。  
问题：中继能看到 `hww1` 中的 secret；设计没有明确禁止中继用本地 HMAC 先行判定成功。若中继先验 secret，就违反“最终认证在目标”，且目标轮换/吊销后中继仍可能放行。  
建议：写明中继只按完整 pubkey 做路由，secret 视为不透明；必须等目标 HMAC/Accept 后才返回票据成功；补“pubkey 对、secret 错”“目标已轮换”“目标拒绝”三类测试。

**C3. 管理预认证洪泛与普通准入共享资源（中）**  
位置：`design.md:86`；`crates/homeway-quic/src/exit/mod.rs:1044-1065`。  
问题：`W1` 未认证连接仍占 `conn_cap`/`handshake_cap`；8 个管理会话上限在认证之后，未认证洪泛仍可拒绝普通设备。设计把“不拖累普通准入”写成了验证目标，没有结构性隔离。  
建议：增加独立的未认证管理名额/预留额度，或如实登记共享上界；用普通设备打满 + W1 洪泛测试，确认设备表零新增且普通准入不被长期饿死。

**C4. `webserve status`/`start --port`/`restart` 状态语义（中）**  
位置：`design.md:26,30`；`crates/homeway-cli/src/unified_cli.rs:480-488,798-852,1164-1191`。  
问题：现有 `role_state` 只有 running/stopped/failed，没有设计要求的 `starting`；`ServeStart` 在已运行时先写配置再返回“already”，不会换监听端口。`webserve start --port` 在运行中可能配置改了但实际监听仍旧；`status` 也无法区分“HTTP 停用但出口管理 agent 可用”。  
建议：定义 `WebserveStatusResult`（HTTP/管理 agent/中继快照/实际 loopback/origin/可行动错误），补 `starting`；明确同端口/different 端口行为（already 或 restart）；`restart` 重读 `[webserve]` 并重新 bind；补状态机测试。

**C5. 精确异步门与 axum/tokio 特性（中）**  
位置：`design.md:42`；`tools/check-quic-isolation.sh:221-245,440-441`；`tasks.md:21`。  
问题：设计只说白名单 `webserve/http.rs`，但 axum 的 handler、WS、静态文件等若分文件会触发 CLI 同步面扫描；axum 版本/features 和 CLI tokio 的精确 features 未写。现有门 ⑨ 只检查 workspace/island 的 `rt-multi-thread`，不检查 CLI 依赖图。  
建议：显式固定 axum 版本和最小 features（如 `http1`/`tokio`/`ws`，按需 `json`），列出完整异步文件白名单并做正反自校准；用 `cargo tree -e features` 断言 CLI 图无 `rt-multi-thread`；验证 `-p homeway-capi` 闭包无 axum/xterm/CLI Web 依赖。

**C6. PTY 阻塞读写、取消与 kill/wait 预算（中）**  
位置：`design.md:78`；`crates/homeway-core/src/term/service.rs:2306-2402`；`docs/INTEROP-CRITERIA.md:1362`。  
问题：设计只说“取消读写、关闭 master、有界 kill/wait”，没有说明阻塞 read/write 如何被唤醒。仓库已有 2026-10-21 Linux PTY 背压取消教训：关闭 master 不等于唤醒阻塞写；若在异步 runtime 上做阻塞 PTY 读写，会卡住整个 HTTP/WS。  
建议：复用既有 PTY 非阻塞/POLLOUT/POLLIN + 取消检查模式；只在线程 worker 做阻塞 I/O；SIGHUP→500ms→SIGKILL 的有界收尸；明确 `TERM=xterm-256color` 等受控环境；补阻塞写、HUP、输出延迟、逐字节顺序、kill/wait 上界测试。

**C7. `hww1` 中 secret 的拷贝与擦除（中）**  
位置：`design.md:34`。  
问题：设计对 `web/auth.key` 提出非 Copy/Drop 擦除，但 `hww1` 本身包含 secret；CLI 字符串、JSON 请求体、票据和日志路径若用普通 `String` 会留下副本。  
建议：用 `Secret`/`Zeroizing` 缓冲持有 `hww1` 和请求体，解析借用，避免无谓 clone；Debug/错误/日志显式脱敏；补“日志/错误不含 secret”的测试。

**C8. 公开 origin 的默认端口与 Host/HTTP2 authority（低）**  
位置：`design.md:46`。  
问题：配置 `https://example.com:443` 时浏览器 `Origin` 通常是 `https://example.com`，`Host` 也可能省略 443；若反向代理用 HTTP/2 回源，`Host` 可能表现为 `:authority`。当前设计写“精确 origin/authority”，但没写归一和 HTTP/2 处理。  
建议：用 URL/authority 解析并归一默认端口；同时读 `Host` 与 `uri.authority()`；文档明确反代是否强制 HTTP/1.1；补 443/非 443、localhost、IPv4 回环测试。

**C9. 管理连接上的额外 bidi/DATAGRAM（中）**  
位置：`design.md:62`；`crates/homeway-quic/src/exit/mod.rs:1101-1102`。  
问题：每个连接仍会 spawn `conn::datagrams`；管理连接没有设备绑定，DATAGRAM 会被丢弃计数（可接受），但管理分支不会启动 `serve_streams`，额外 bidi 不会主动 reset，恶意客户端可在 `max_bidi` 内占住未读流。  
建议：管理分支显式监控/重置额外 bidi，或直接关闭连接；测试额外 bidi、普通 tag、DATAGRAM 均不创建设备、不进入普通服务。

**C10. 判据真源与任务验收补齐（中）**  
位置：`docs/INTEROP-CRITERIA.md:4,599`；`tasks.md:21`；`ROADMAP.md`。  
问题：B2 的新协议属于 wire 变更，但 tasks 4.4 只写“纳入判据真源”，没有具体登记行、夹具和验收命令；也没有把 B4/B5 的竞态和票据生命周期纳入 5.1。  
建议：先登记判据变更，再写代码；在 tasks 2.2/3.3/4.2/5.1 增加竞态、票据过期、中继重启、无孤儿进程等验收项；完成后更新 ROADMAP。

**C11. 平台边界与旧守护错误映射（中）**  
位置：`design.md:77,93`；`crates/homeway-cli/src/main.rs:112-120`；`crates/homeway-core/src/daemon/vocab.rs:30-130`。  
问题：没有逐命令定义 Windows/OHOS/musl 行为（是全部 capability unavailable，还是 token/status 可用）；旧统一进程收到 `webserve.*` 会返回 `unknown_op`，设计没有明确映射为“升级守护”，也没有防止自动拉起第二进程。  
建议：给出 webserve 子命令平台矩阵；Windows 明确报不支持；旧 daemon 的 `unknown_op` 映射为可行动升级错误且不 auto-spawn；CI 覆盖 Windows/OHOS/musl check。

**C12. Rust 形态与锁边界（中）**  
位置：`crates/homeway-cli/src/unified_cli.rs:281,794`；`AGENTS.md` 工程原则。  
问题：若把 webserve HTTP/PTY/ticket 状态塞进 `RolesInner` 的单一 `Mutex`，或在 `role_op` 持锁期间做 bind/HTTP/PTY 操作，会阻塞角色操作；新代码若继续用 `String` 错误、复制 token、用大 `Arc<Mutex>` 包住异步面，会违反本仓 Rust 纪律。  
建议：拆成窄句柄（`Arc<WebAuth>`、`Arc<TicketStore>`、`Arc<RelaySnapshot>`），不在 `inner` 锁内做 I/O；新错误用 `thiserror`；`hww1`/帧解析借用；明确没有逐行移植痕迹。该项不阻塞设计，但必须进代码评审清单。

---

## 四、看过且未发现问题的方面

- **现有 TLS/RPK/ALPN 事实**：`exit/rpk.rs` 确实未设 ALPN；`dangerous()` 只在 RPK 钉定文件，且有 `verify_tls13_signature_with_raw_key`；设计“保持 TLS/RPK、无 ALPN、不新增 SkipVerify”与源码一致。看过，未发现方向性问题。
- **首流分流位置**：`exit/conn.rs` 的 `accept_bi` + 两字节 `FrameHead` 结构上可以在 `hr-reg4` 前插入 `W1` 分支；`ExitInbound` 是 `#[non_exhaustive]`，新增 Management 变体可行。看过，未发现不可实现的结构障碍；具体 wire 见 B2，确认回执见 B5。
- **中继完整身份来源**：`relay/mod.rs` 的 `Leg` 确实保存 `pubkey`、`verified`、`ctl_verified`，驱动线程私有表可生成 pubkey→label 快照；pending 表独立。看过，未发现取不到完整 pubkey 的结构性障碍；ctl-only 边界见 C1。
- **管理/设备准入分层**：设计明确不调用 `DeviceTable/register` 和 `bridge.bind`，`confirm_client` 只做腿确认；这条分层是对的。看过，未发现把管理连接注册成设备的必然路径；共享配额见 B4/B5/C3。
- **TUI 与工作会话**：`homeway tui` 通过 `control.sock`/`local-term.sock` 访问现有 TermService；`App` drop 只关 attach，不发送 KILL；`LocalTerm` 在角色开关之外启动，Web-only 本机 TUI 有宿主。看过，未发现“关页面必杀工作会话”的结构性问题；子进程 no-spawn 见 B8，PTY 取消见 C6。
- **HTTP/WS 与反代**：仅 127.0.0.1、用户反代 HTTPS/WSS、Host/Origin 精确允许、不信任 `Forwarded`/`X-Forwarded-*`、票据不落 cookie/localStorage、静态资源内嵌、严格 CSP，方向正确；未发现扩大信任面的设计。端口归一/HTTP2 authority 见 C8，总期限见 B6。
- **CAPI/平台依赖**：把 axum/xterm/HTTP 限制在 CLI Unix 图，core/quic 保持 std 同步面，方向正确；`homeway-capi` 不需要新增 Web 依赖。看过，未发现依赖倒挂；精确特性/门禁见 C5，平台矩阵见 C11。
- **既有联网协议字节**：设计明确不改 ALPN、token、`hr-reg4`、中继信封和服务 tag；现有网络准入回归要求保留。看过，未发现方案要求改动既有 wire。
- **Rust 形态**：方案复用 `RelaySock`、`pump`、`TermService`、`RoleHost`，没有明显逐行移植；类型化认证/票据/取消的方向符合本仓纪律。看过，未发现系统性过度设计；锁/错误/复制见 C12。
- **OpenSpec**：`openspec validate independent-webserve --strict` 通过；proposal/spec/tasks 结构完整。看过，未发现 OpenSpec 语法/结构问题。

---

## 五、最小整改清单

1. **B1**：把 `[webserve]` 移出旧 `FileConfig`（独立 `webserve.toml` 优先），或保证未使用时永不写入；补基线二进制兼容测试。
2. **B2**：补 `hww1`、管理握手、PTY 帧的精确 wire 规格 + `fixtures/webserve/*` + `INTEROP-CRITERIA` 登记。
3. **B3**：让 `RelayProc` 暴露实际 UDP `SocketAddr`，wildcard 归一 loopback，补端口退让/`:0` 测试。
4. **B4**：统一票据+会话配额，票据 RAII 持有连接，过期/失败/中继重启/目标轮换原子撤销；补 32 票/8 会话测试。
5. **B5**：`confirm_client` 返回类型化结果，增加腿就绪的有界等待/重试，确认成功后才 Accept。
6. **B6**：补 HTTP 头/体/总期限与 WS `max_frame_size`/`max_message_size` 64KiB，补 slowloris/大帧测试。
7. **B7**：管理 worker 恒装配可热替换 `WebAuth`，或增加控制操作安装首个/轮换 Web 凭证；补“先启出口、后取 token、无 HTTP”测试。
8. **B8**：给 TUI 增加 `--no-spawn`/`HOMEWAY_NO_SPAWN` 并由管理 worker 强制使用，补父进程死亡/无孤儿测试。
9. **P1/P2**：把本次评审与真实 reviewer 退出码写入 `docs/reviews/2026-11-07-webserve-design.md`，订正“默认启用出口”的事实与 `unified_cli.rs:1217` 日志；未完成前不勾选 task 1.1。

B1–B8 全部处置并复审后，技术门才可放行；C 系列可在实现阶段逐项落实，但必须进入对应 tasks 的验收，不能以“运行通过”替代边界测试。


## 独立评审子任务的处置判断（与原文分开）

- **认同 B1**：独立严格 `webserve.toml` 是最直接且完整的旧二进制回退方案，需在全部规划产物同步。
- **认同 B2**：开工前应固定新协议字节、错误码、限额和夹具制作任务；实现阶段再生成和验证真实向量，不可伪造已验结果。
- **认同 B3**：实际端口必须从运行态取得，不能以配置端口代替；公开 `:0` 是否允许沿用现有配置契约，私有端口零用例不扩大产品输入。
- **认同 B4**：统一票据与活跃界面额度，建立连接关闭/轮换/中继 epoch 的撤销闭环。
- **认同 B5**：提升中继腿需要类型化结果和明确时序，不能以入队成功冒充准入成功；有界等待不能将旧 generation 偷换成当前 generation。
- **认同 B6**：有界连接数不等于有界占用时长，HTTP 与 WS 的线级限额、绝对期限需同时明确。
- **认同 B7**：已启动出口后首次获取凭证、不启 HTTP，是必须明确并真实验收的自然路径；共享可空 live 认证句柄可解决，无需重启出口。
- **认同 B8**：Web 宿主 TUI 必须禁止自动拉起守护，父连接/父进程丢失只退出界面；回收自有 TUI 不应触达用户工作会话。
- **认同 P1/P2**：技术门记录需留实证；默认配置实际为出口/中继关闭，旧事件串与规划陈述应订正。
- **认同 C1 问题，建议采用另一实现**：完整认证的 TCP 注册也应能参与路由；在 `Handover`/`CtlConn` 保留握手已验的完整 pubkey，并发布合法有效注册快照，比一律排除 ctl-only 更符合“所有有效注册主机”的需求。不改变线上控制帧。
- **认同 C2–C12**：应映射到设计/任务和代码门清单。其中 C3 必须如实表述共享 TLS 握手前资源边界，不宣称认证前完全隔离；C8 可明确强制 HTTP/1.1 回源，避免无需求的 HTTP/2 扩面。

本记录不修改规划、不标任务完成。实施者须修订规划并给逐项锚点，再由独立评审者复核放行。
