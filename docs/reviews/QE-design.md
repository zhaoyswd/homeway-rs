# Q-E 出口服务修复（files / DNS / UPnP / speedtest）设计文档

> 批次：Q 批整改（`docs/REVIEW-ROADMAP.md` §Q-E；顺序 Q-A → Q-B → Q-C → Q-I 前段 → Q-D → **Q-E**）。
> 第 1 棒（设计）产出，**不写产品代码**。
> 真源：`docs/reviews/AUDIT-2026-10-07.md`「Q-E 出口服务」节 + 总表 P0-4；仓规 `AGENTS.md`。
> 基线：`git HEAD = 6b7237f`（main）。**本文所有行号 = 复验时（本 HEAD）实测值**，实现以符号定位为准。
> **范围边界（硬）**：`deliver_udp53` 的 send 失败计数已由 Q-B（F10）处置 ⇒ 只登记不重做；
> DNS TTL 缓存 / socket 缓冲复用 / files 拷贝 = **Q-I 尾段**（本批之后）⇒ 不设计；
> UPnP 协议面扩展（IGD:2 / `AddAnyPortMapping` / ST 兜底 / 钉卡降级）与 `if_nametoindex` 完整语义
> = **Q-J** ⇒ 不设计。本批 = **鲁棒性/安全性修复**（沙箱、上限、期限、名额、错误分类）。
> 两台生产出口（Mac launchd / 阿里云）、只读仓（`~/Documents/projects/tier`、`~/Documents/projects/homeway`）、
> `baseline/` 内容、`tools/tailcat/homeway-rs.pin` 全程不碰。
> **状态**：**v3 —— 设计门已过**（dsh `r9.0mrmgB`，**exit=0**）。v2 为送审稿；v3 并入评审全部
> 处置（3 条阻塞项 S1/G1/R1 + 全部中/低项，**0 条不认同**，见 §5）。

---

## 0. 复验（证据先行）

### 0.1 方法

逐条回源码重定位（审计行号在 Q-B/Q-C/Q-D 后有漂移；`intercept/dnsface.rs` 被 Q-B 改过，
`speedtest_server.rs` / `upnp.rs` / `dnsproxy.rs` 未被后续批触碰，行号基本稳定）。
🔎 项逐条判真伪；与 Go 基线（`baseline/homeway`，只读 oracle）语义**逐处对照**——本批多条
「缺陷」实际是「Go 同构」或「Go 也缺」，这类必须与「Rust 移植偏差」区分开，否则把 Go 的
既有风险误当本仓引入。`deliver_udp53` 一项按边界先核 Q-B 是否已处置。
**🔴 v3 订正**：v2 的 §0.1 有两处定性错误（第 5 行「Go 也缺」、第 9 行「Go 无体积闸」）与一处
复验错误（speedtest 名额泄漏），均经设计门指出并**独立复跑/复读源码证实**后订正（见 §0.2 注、
§5.3）。教训：Go 语义对账**必须逐符号读到位**，不能凭印象（`pkg/files/proto.go` 的
`MaxRequestLine` / `readLineLimited` 就在同一文件里）。

**Go 基线对照的关键结论（贯穿全批）**：

| 面 | Go 基线行为 | Rust 现状 | 定性 |
|---|---|---|---|
| DNS 上游读循环期限 | `conn.SetDeadline(now+budget)` = **绝对期限**，`continue` 不续命（`pkg/dns/server.go:415`） | `set_read_timeout(Some(budget))` = **per-syscall**，`continue` 续命（`dnsproxy.rs:645/649-676`） | **Rust 移植偏差**（绝对↔per-syscall 语义混淆） |
| DNS 并发模型 | goroutine-per-query，上界 = `MaxInFlight` 256（`server.go:215-231`） | 固定 worker 池 `WORKERS = 2` + 256 槽队列（`dnsproxy.rs:39/405-441`） | **Rust 独有形态**（H3 整改引入），并发度差 2 个数量级 |
| files 路径沙箱 | `os.OpenRoot` 逐分量解析（`server.go:69/285/313/336/492/553`） | `canonicalize` ENOENT → `Ok(candidate)` 原样放行（`files_server.rs:307-315`） | **Rust 移植偏差（P0）** |
| files 上传上限/水位 | **无**（写到终止帧为止，`server.go:509-524`） | **无**（`files_server.rs:490-503`） | **两者都缺**（加固超出 Go 基线，非偏差） |
| files **服务端**请求行上限 | **有**：`MaxRequestLine = 64*1024` + `readLineLimited`（累积中判负，回 `invalid_arg`「请求行超过 %d 字节」）——`pkg/files/proto.go:39/144-146/159-175` | 常量已存在（`files.rs:34`）但服务端 `read_line`（`files_server.rs:538-548`）**漏用** ⇒ 无界读 | **Rust 移植偏差**（v2 误判为"两者都缺"，v3 订正） |
| files/speedtest accept 错误 | `Serve` 返回 err，**调用方** `ln.Close()` + 日志（`serve.go:459-465/530-537`） | `Err(_) => return Ok(())` **静默**（`files_server.rs:162`、`speedtest_server.rs:137`） | **Rust 移植偏差**（吞掉错误与日志） |
| speedtest 单连接硬超时 | `conn.SetDeadline` 绝对期限（Go `Server.serveConn`） | `set_io_deadline` **只在会话开头调用一次**（`speedtest_server.rs:170-171`） | **Rust 移植偏差** |
| speedtest busy 吞输入窗 | `SetReadDeadline(now+1s)` 绝对（`speedtest.go:434-440`） | `set_read_timeout(1s)` per-syscall + 无总期限（`speedtest_server.rs:495-501`） | **Rust 移植偏差** |
| speedtest 在册表 | `connreg.Registry` 按 id `Remove(id)`（`pkg/connreg/connreg.go:40-44`） | `release()` **清整表**（`speedtest_server.rs:76-79`） | **Rust 移植偏差** |
| speedtest 限额 | `Limits{MaxConns,ConnTimeout,MaxWarmup,MaxWindow,MaxBlock,SendBlock}` + `SetLimits`（`speedtest.go:74-104/121-131`） | 全硬编码常量（`speedtest_server.rs:26-36`） | **Rust 移植偏差**（且硬超时面不可测） |
| UPnP http 调用 | ctx 盖住**拨号**+读+体（`upnp.go:64-68/219-230`）；体积闸 **`io.LimitReader`**：描述文件 `1<<20`、SOAP `1<<16`（`upnp.go:73/235`，**静默截断**） | `TcpStream::connect` 无超时（`upnp.rs:285`）+ `read_to_end` 无上限（`:309`） | **Rust 移植偏差**（v2 漏写体积闸半句，v3 订正） |
| UPnP SSDP 应答采纳 | 首应答即信，来源不校验；要求 `LOCATION` **非空**（`upnp.go:155-164`） | 同（`upnp.rs:237-247`），但空 LOCATION 也采纳（`:239`） | **Go 同构 + 一处移植偏差**（N4）；过滤属加固 ⇒ 登记偏离 |
| UPnP 加映射 | 先删后加（`upnp.go:249-253`），枚举失败/残缺 fail-open（`:585-601`） | 同（`upnp.rs:433-435/623-645`） | **Go 同构**（本批做加固，登记为偏离） |
| UPnP 枚举次数 | 每轮 **3 次**全表枚举（`FindOurMapping` + `CleanMappings` + `selectExternalPort` 各一次） | 同（`upnp.rs:601/613/617`） | **Go 同构**（F9a 的一次缓存 = **优化，偏离 Go**，须登记） |
| UPnP 缩租 | **一个 8s ctx 贯穿**全部候选（`serve.go:728`），SSDP 取 `min(ctx, 5s)`（`upnp.go:140-144`） | `UPNP_SHRINK_BUDGET` 是**每候选**预算、无全局界，且 SSDP 腿自带 5s 不受预算约束（`engine.rs:878-891`、`upnp.rs:200-252/392-395`） | **Rust 移植偏差** |
| DNS TCP 腿拨号 | `net.Dialer{Timeout: budget}`（`server.go:449`） | `TcpStream::connect` 无超时（`dnsproxy.rs:682`） | **Rust 移植偏差**（新增项 N7） |

### 0.2 逐条复验结果表

| # | 审计条目（摘要） | 真伪 | 现行位置（HEAD `6b7237f`） | 结论 |
|---|---|---|---|---|
| 1 | **P0-4** files 沙箱不存在的叶子直返 candidate | ✅ **成立** | `files_server.rs:307-315`（`Err(_) => Ok(candidate)`）、`:365-375`（mkdir 直接 `create_dir`）、`:437-451`（write 直接 `File::create(part)`） | 修（F1）：`rel_path` 是六动词唯一入口 ⇒ 一处收口 |
| 2 | P1 files 客户端响应行卡 64KB | ✅ **成立** | `files.rs:324-345`（无条件套 `MAX_REQUEST_LINE`，常量 `:34`）、`:427`（`call()` 读响应行）；`facade/files_op.rs:132-153` 已修同族（`read_line_capped(false)`，调用点 `:110/:163/:658`） | 修（F2）：对齐 `files_op` 口径 |
| 3 | P1 DNS 上游读循环无绝对期限 + worker 仅 2 | ✅ **成立**（continue 处在 `:652/:656/:658/:660` **四处**，v2 写"三处"已订正） | `dnsproxy.rs:645`（一次 `set_read_timeout`）、`:649-676`（`loop`）、`:666`（`deadline` 只被 TC→TCP 消费）、`:39`（`WORKERS = 2`） | 修（F4）：**per-attempt** 绝对期限（见 R1）+ worker 数 + TCP 腿期限 + 回投容量 |
| 4 | P1 accept 错误静默退出 + spawn 名额泄漏 | ⚠️ **前半成立 / 后半仅 files 成立** | files：`:162` 静默退出 + `:170-185`（`fetch_add` 后 `.ok()` ⇒ **真泄漏**）；speedtest：`:137` 静默退出成立，但 `admit` 在**被 spawn 的闭包体**内（`serve_conn:150-166`）⇒ spawn 失败时闭包不执行、`live` 从未加过 ⇒ **无名额泄漏**（v2 误判，v3 订正） | 修（F5）：错误分类退避（两模块）+ files 侧名额回滚（两模块统一 RAII，兼为 F6b 提供 id） |
| 5 | P1 🔎 上传无大小上限 | ✅ **复核成立**（**非** Rust 偏差） | `files_server.rs:472-510`（`receive_upload` 累计无界；单帧受 `MAX_CHUNK` 256KB 约束，总量无约束） | 修（F3a）：磁盘水位门（Go 亦无 ⇒ 加固，须登记行为差异） |
| 6 | P2 speedtest 硬超时实为 per-syscall | ✅ **成立** | `speedtest_server.rs:170-171`（各一次）、`:384-394`（helper；全仓唯一调用点 = `:171`） | 修（F6a）：每次触达 socket 的读写前按绝对期限收敛 + `Limits` 可注入 |
| 7 | P2 `release` 清整表 / `close_all` 置 live=0 | ✅ **成立** | `speedtest_server.rs:65-74`（`admit`）、`:76-79`（`release` 清 `conns` 全表）、`:82-87`（`close_all`） | 修（F6b）：`release(id)` 按会话收口 |
| 8 | P2 `read_frame_header` 丢类型字节（`:507`） | ✅ **成立**（**措辞订正**：丢字节的是 `read_frame_bounded:505-512`，`read_frame_header:533-542` 正常返回 `(typ, len)`） | `speedtest_server.rs:505-512`（`let _ = typ;` + `n > 512` 即报错 ⇒ 载荷留流里错位） | 修（F6c）：按类型流式吞完载荷；busy 路径吞输入总期限 = **1s**（对齐 Go，见 R8） |
| 9 | P2 UPnP `http_call` 无界读 + 不校验 Content-Length + 滴流可永久挂 | ✅ **成立**（并追加：拨号无超时 = N8；Go 有分调用体积闸 = v2 漏写） | `upnp.rs:276-326`（`connect:285`、`set_read_timeout(Some(remain)):294`、`read_to_end:309`） | 修（F7）：分调用上限（desc 1MiB / SOAP 64KiB = Go 同值）+ 长度一致 + 绝对期限 + 拨号期限 |
| 10 | P2 UPnP SSDP 首应答即信 | ✅ **成立**（并追加：空 LOCATION 也被采纳 = N4；Go 要求 `loc != ""`） | `upnp.rs:233-248`（`Ok((n, _))` 丢来源；`:239` `if let Some(loc)` 未判空） | 修（F8） |
| 11 | P2 UPnP 先删后加 + 三连枚举 + 共享预算 | ✅ **成立** | `upnp.rs:433-435`（无条件 `delete_mapping`）、`:601-617`（三处各枚举一次）、`:30/:32`（40s / 8s） | 修（F9/F10） |
| 12 | P2 🔎 DNS 无缓存 / 每查询新 socket / 64KB 全零 / 上游整表 clone | ⚠️ **部分成立、拆分处置** | `dnsproxy.rs:643-644`、`:648`、`:255/261/271` | TTL 缓存 = **Q-I 尾段**；「每查询新 socket」= Go 同形 + 防投毒属性 ⇒ **剔除缺陷定性**；零初始化/整表 clone = **Q-I 尾段** |
| 13 | P2 🔎 `deliver_udp53` 忽略 send 失败 + tx metadata 64 槽 vs 在途 256 | ⚠️ **前半已由 Q-B 处置 / 后半成立** | `dnsface.rs:295-298`（**Q-B F10 已改**：返回 `bool`，调用方 `intercept/mod.rs:1861-1863` 计 `udp_drop`）；`dnsface.rs:140-144`（rx/tx 各 64 槽 + 各 64KB） | 前半 **登记不改**；后半进 F4d（**rx/tx 双侧**，见 R3） |

**误报剔除 / 定性订正记录**：

1. **剔除「每查询新 socket」的缺陷定性**（条目 12 前半）——Go 同样每查询新建 socket
   （`server.go:409` `net.DialUDP`），且两侧注释都写明这是**防投毒**设计（随机源端口抬高 off-path
   伪造门槛）。审计把它与性能面并列时未区分，本批**按设计保留**。
2. **订正条目 8 的函数归属**——丢字节的是 `read_frame_bounded`（`read_frame_header` 正常）。
3. **订正条目 5 的定性**——上传无上限成立，但 Go 基线同样无上限 ⇒ **不是移植偏差**，是加固项。
4. **订正条目 4 的 speedtest 半边**（设计门 E1 指出，本棒复读 `speedtest_server.rs:150-166` 确认）
   ——`admit` 在被 spawn 的闭包内 ⇒ spawn 失败不会泄漏 `live`；files 半边（`fetch_add` 在 accept
   循环里）**确实泄漏**。
5. **订正 §0.1 第 5 行（设计门 G1 指出，本棒复读 `pkg/files/proto.go:39/144-146/159-175` 确认）**
   ——Go **有**服务端请求行上限 `MaxRequestLine = 64*1024` + `readLineLimited`（累积中判负、回
   `invalid_arg`「请求行超过 65536 字节」）；Rust 常量早已存在、只是服务端漏用 ⇒ F3b 是**恢复
   对齐**，不是"新增加固"。
6. **补 §0.1 第 9 行的体积闸半句（设计门 R7/G5）**——Go 用 `io.LimitReader`（desc `1<<20` /
   SOAP `1<<16`）静默截断；Rust 无任何上限。
7. **`deliver_udp53` 不重复设计**——Q-B F10 已处置（`dnsface.rs:292-298` 的注释即现场）。

### 0.3 复验新增项（审计未覆盖，同族缺口）

| # | 新增项 | 位置 | 定性 | 处置 |
|---|---|---|---|---|
| N1 | files **服务端**请求行无上限（**v3 订正：属移植偏差，Go 有 64KB 门**） | `files_server.rs:538-548`（`serve_conn:237` 与 `serve_busy:214` 共用） | 移植偏差（上限缺失 ⇒ 无界内存） | 进 F3b（对齐 Go 文案与回帧语义） |
| N2 | speedtest busy 吞输入循环无总期限（per-syscall 续命） | `speedtest_server.rs:495-501` | 移植偏差（Go 是 1s **绝对**期限，`speedtest.go:434-440`） | 进 F6c |
| N3 | UPnP `cands` 未去重（Go 有 `tried`） | `upnp.rs:646-656` vs Go `upnp.go:614-616` | 移植偏差 | 进 F9c |
| N4 | SSDP 空 `LOCATION` 被采纳（Go 要求非空） | `upnp.rs:239` vs Go `upnp.go:161` | 移植偏差 | 进 F8 |
| N5 | 缩租候选循环无全局期限，且 **SSDP 腿自带 5s 不受预算约束** ⇒ 最坏 ≈ N×(5s+8s) | `engine.rs:878-891`、`upnp.rs:220/392-395`、调用点 `serve_cli.rs:482`、`unified_cli.rs:912/939/1470` | 期限缺失（拖住停机路径；launchd `ExitTimeOut` 20s 内会 SIGKILL） | 进 F10（期限**穿透到 SSDP**） |
| N6 | speedtest `Limits` 未移植（Go 有 + `SetLimits` ⇒ 硬超时/上限不可注入、不可测） | `speedtest_server.rs:26-36` | 移植偏差（且是测试缝缺口） | 进 F6a（含 `SendBlock`） |
| N7 | DNS TCP 腿拨号无超时（OS 默认，macOS 可达 ~75s） | `dnsproxy.rs:682` vs Go `server.go:449` | 期限缺失 | 进 F4b |
| N8 | UPnP `http_call` 拨号无超时 | `upnp.rs:285` vs Go `upnp.go:64-68/219-230`（ctx 盖拨号） | 期限缺失 | 进 F7 |

`dnsface.rs:182`（每调用 64KB 零初始化）不在本批：QI.md §6 登记为「下一批第一靶点」，其语义
（缓冲复用）属 **Q-I 尾段**明文清单 —— 见 §6 移交登记。

### 0.4 Go `os.Root` 符号链接语义**实测**（独立复跑，供 F1 定规）

v2 曾写「本机无 `os.Root` 可复现」⇒ 设计门指出 module cache 内有 Go 1.24.5 toolchain。
**本棒离线复跑**（`GOTOOLCHAIN=go1.24.5`，`~/go/pkg/mod/golang.org/toolchain@v0.0.1-go1.24.5.darwin-arm64`;
脚本与输出留档 `/tmp/qe-rootcheck/main.go`）：

| 形态（root 内条目） | `Root.Stat` | `Root.OpenFile(x+"/newleaf", O_CREATE)` |
|---|---|---|
| ① `l_rel_in -> sub`（相对、根内、目标存在） | **OK** | **OK（文件落在根内）** |
| ② `l_abs_in -> <root>/sub`（**绝对、根内**） | `path escapes from parent` | `path escapes from parent` |
| ③ `l_rel_out -> ../outside`（相对、根外） | `path escapes from parent` | 同 |
| ④ `l_abs_out -> <outside>`（绝对、根外） | `path escapes from parent` | 同 |
| ⑤ `l_dangle_in -> sub/notyet`（相对悬空） | `no such file or directory` | `no such file or directory` |
| ⑥ `l_dangle_out -> <outside>/notyet`（绝对悬空） | `path escapes from parent` | 同 |

**三条规则**（F1 按此实现）：**(a) 目标为绝对路径的符号链接一律拒**（即便落在根内）；
**(b) 相对目标经词法规整后越出根 ⇒ 拒**；**(c) 悬空链接 ⇒ ENOENT**（Rust 侧统一 `not_found`）。
根外目录事后校验：`outside entries: 0`（六形态全未写出根外）。
⇒ **v2 的两处悬置作废**：现状 Rust 对形态②（绝对 + 根内）**放行**，比 Go 宽松 ⇒ F1 顺带对齐；
`§6`「根内符号链接语义待取证」条目删除。

---

## 1. 修复清单

> 编号规则：F&lt;序号&gt;&lt;子项&gt;。每条给「问题 / 方案 / 涉及文件 / 风险 / 测试 / 判据行影响」。
> 目标 = 不改变任何 wire 字节与判据行行文（除登记项）；行为变化一律进 §4 登记。

> **实现订正（Q-E 第 2 棒；代码门 r10/r11 复核）**：①符号链接链深度上限取 **8**
> （= Go `rootMaxSymlinks`，`os/root.go:70`；本文原写 40）；②ENOENT 合并分支增
> 「剩余队列含 `..` ⇒ `not_found`」guard——修前词法回升会拼回**未复核**尾部
> （`l -> gone/../evil/leaf` + `evil -> 根外` 可写出根外；代码门 H1 已用真实 crate 复现，
> 负例 `rel_path_dotdot_after_missing_component_rejected` 钉死）。处置记录见
> `docs/reviews/QE.md`。

### F1（P0-4）files 路径沙箱：单点收口 + 逐分量复核（Go `os.Root` 同规则）

**问题**：`rel_path`（`files_server.rs:288-316`）在 `canonicalize` 失败（ENOENT = 叶子不存在）时
直接 `Ok(candidate)` 放行；`mkdir`/`write` 随后对候选的**父目录**直接 `create_dir`/`File::create`。
⇒ 根内符号链接 `link -> /etc` + 不存在叶子 `link/newfile`：整字路径从未被解析，写入落到根外。
经隧道拿到 token 的设备即可写出共享根（`files_root` 缺省 = 出口用户 HOME）。
附带（§0.4 实测）：现状对**绝对目标的根内符号链接**也比 Go 宽松（Go 一律拒）。

**方案**（新增 `fn resolve_in_root(&self, trimmed: &str) -> Result<PathBuf, Response>`；
`rel_path` 的 `canonicalize` 快路径**整体替换**为该 walk，六动词共用）：

1. 分量工作队列（`VecDeque<OsString>`，初值 = `trimmed` 的分量），基点 `base = root`（恒已核验在根内）。
   逐分量：
   - `symlink_metadata(base/comp)`：
     - **ENOENT** ⇒ 该分量及其后分量均不存在；返回 `base.join(剩余分量)`（**绝不返回未经复核的
       原 candidate**）；
     - 其它 Err ⇒ 交 `map_io_err` 归类；
     - **是符号链接** ⇒ `read_link`：
       - 目标**绝对** ⇒ `not_found`（§0.4 规则 a）；
       - 目标**相对** ⇒ 目标分量**拼回队列头**（按语义替换该分量）并递归处理（深度上限 40，
         超限 ⇒ `not_found`）；目标里的 `..` 按词法弹栈，**弹出根外 ⇒ `not_found`**（规则 b）；
       - `read_link` 失败（悬空/竞态）⇒ `not_found`（规则 c；§0.4 实测 Go 对悬空相对链接亦
         回 ENOENT ⇒ **对齐，不是"更严"**）；
     - 其它（普通文件/目录）⇒ `base = base/comp` 前进（**中途是普通文件不在此报错**：让最终
       `create/open` 落 `ENOTDIR` ⇒ `op_failed`，与 Go `mapOSErr` 同类，见 S4）。
2. `not_found` 语义不变（逃逸类统一 `not_found`，不泄漏根外信息；对齐 Go `mapOSErr` 的
   `escapes from parent`/`outside of root` 归类，`server.go:255-268`）。
3. `rel_path` 的 `..` 段拒绝 / NUL 与控制字符拒绝 / 前导 `/` 剥离（绝对形式视为根内相对）全部保持。

**关键点**：`rel_path` 是六动词唯一入口（`files_server.rs:343/358/366/379/413/438`）；`write` 的
`.tierpart` 与目标同目录（`path.with_file_name(...)`），基点已核验 ⇒ 临时文件同样在根内。
**去掉 canonicalize 快路径**后返回值不再是实址（而是"词法解析后的根内路径"）——与既有
`Ok(candidate)` 分支同形态，下游（`file_name()`/`with_file_name()`）不受影响。

**涉及文件**：`crates/homeway-core/src/files_server.rs`（`rel_path` 拆 `resolve_in_root` + 新测试）。

**风险**：
- 行为收紧（**目标**）：此前可写的若干越界形态改 `not_found`；绝对目标 + 根内符号链接由
  「放行」改「拒」（对齐 Go，登记）。
- **TOCTOU 残余**：复核与创建之间另一写者可替换已核验分量。裁定 D5：**不做** `openat2`/
  `O_NOFOLLOW` 改造（macOS 无 `openat2`；威胁模型 = 经隧道的设备，本机 FS 写者不在模型内）⇒ 登记
  残余（**不得**写成"已消除 TOCTOU"）。
- 性能：每 op 多 O(分量数) 次 `lstat` + 每次符号链接一次 `read_link`（文件操作非热路径）。
- **残余登记**（本批不改，见 §6）：`stat` 的 `entry.name` 取实址 basename 而 Go 取**请求** basename
  （`link -> sub` 时 Rust 回 `sub` / Go 回 `link`）；`write` **穿透**符号链接（写目标文件）而
  Go 的 `rename` **替换**链接本身。

**测试计划**：
- 纯函数级（`rel_path`）：① `link -> <outside>` + 不存在叶子 ⇒ `not_found`；② `link -> <outside>/<不存在>`
  **悬空** ⇒ `not_found`；③ `link -> sub`（相对根内、存在）+ 不存在叶子 ⇒ 放行且落在 `root/sub/...`；
  ④ **绝对目标 + 根内** ⇒ `not_found`（§0.4 规则 a）；⑤ 链接链 `l1 -> l2 -> <outside>` ⇒ `not_found`；
  ⑥ 循环链接（`a -> b`、`b -> a`）⇒ `not_found`（深度上限）；⑦ 分量中途是**普通文件**
  （`file/leaf`）⇒ `op_failed`（**非** `not_found`，防被归进 ENOENT 分支）；
  ⑧ 深层不存在 ⇒ 放行；⑨ `/etc/passwd`（前导 `/`）⇒ 视为根内相对（既有）；⑩ `..`/控制字符 ⇒ `invalid_arg`（既有）。
- **端到端负例（审计点名）**：真 UDS 对 + `FilesServer`，根内建 `link -> <tmp outside>`；
  逐动词打 `link/newfile`（write）/`link/newdir`（mkdir）/`link/x`（stat/read/download/list）
  ⇒ 全部 `ok=false`，且**断言 outside 目录内容一字未增**。
- 正例回归：`sub/newdir/u.bin` 上传/下载往返仍绿（`six_verbs_over_ud_pair` 保持）。

**判据行影响**：无编号判据行变更（E14 行文不变）。行为差异登记（§4.3.1）：此前放行的越界形态与
「绝对目标 + 根内」形态改 `not_found`（Go 基线本就拒绝，§0.4 实测）。

---

### F2（P1）files 客户端响应行去 64KB 上限

**问题**：`files.rs` 的 `read_line_opt`（`:324-345`）对**所有**读行无条件套 `MAX_REQUEST_LINE`
（64KB），而它唯一的调用面是**客户端读响应行**（`call():427`、`handshake():291`、`upload_at():694`）
——客户端根本不读请求行（服务端才有请求行，见 F3b）。后果：服务端内联上限 16MB
（`files_server.rs:24`）与 `list` 大目录 JSON 都可超 64KB ⇒ 合法响应被客户端判死。

**方案**：`read_line_opt` 去掉 64KB 判负（`:331-333` 与 `:336-338`），与
`facade/files_op.rs:132-153` 的 `read_line_capped(false)` 口径一致；`read_line` 文档注释同步
（「无上限——响应面」）。`MAX_REQUEST_LINE` 常量保留（服务端 F3b 使用；`files_op` 亦引用）。
**理由（v3 改写，采纳 R6）**：①**契约一致性**——对端是用户自己的出口，`files_op`（App 侧同一
协议的消费者）早已是"响应行不设界"，core 客户端要与它同口径；②原 64KB 门是**误移植**（服务端
请求行门套到客户端响应行上），它并不是"客户端内存界"的设计意图。
**残余登记**：客户端内存随对端响应行增长（与 `files_op` 同暴露面；`read_line` 原本也无期限）。

**涉及文件**：`crates/homeway-core/src/files.rs`（`read_line_opt` + 注释 + 新测试）。

**风险**：同上（残余已登记，D4）。

**测试计划**：合成流（`Stream::from_rx`）喂 200KB 响应行（大 `entries`）+ 12MB `text` 行 ⇒
`call()` 均应成功返回（修前第二条必报「行超过 64KB 上限」）；既有
`frame_parse_boundaries_with_arbitrary_chunking` 保持绿。

**判据行影响**：无（客户端 files 面无编号判据行）。行为差异登记（§4.3.2）：>64KB 响应由「报错」
→「正常返回」。

---

### F3（P1/P1🔎 + N1）files 服务端上限族：上传水位 + 请求行上限（对齐 Go）

#### F3a 上传磁盘水位（P1🔎，审计「配额/水位缺失」）

**问题**：`receive_upload`（`files_server.rs:472-510`）只累计不设界；单帧上限 `MAX_CHUNK`
（256KB）不约束总量。经隧道可达 ⇒ 可写满出口磁盘。

**方案**（裁定 D1：**水位优先，不做每文件硬上限**）：

- `fn avail_bytes(dir: &Path) -> Option<u64>`：`libc::statvfs` 取 `f_bavail × f_frsize`
  （macOS/Linux/OHOS 同 API；失败 → `None`）。**注入缝（M3）**：`FilesServer` 持
  `avail: Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>`（`open()` 用真 statvfs，测试注入固定值；
  每次检查一次间接调用，非热路径）。
- 常量 `UPLOAD_RESERVE_BYTES: u64 = 1 << 30`（1 GiB）。
- **起始门**（创建 `.tierpart` 之前）：`avail < RESERVE` ⇒ 拒；`req.size > 0 && avail < req.size + RESERVE`
  ⇒ 拒（快路径，免走完流量）。
- **进行中门**（`receive_upload` 内每累计 8 MiB 复查）：`avail < RESERVE` ⇒ 中止 + 复用既有
  `Err(resp)` 清理路径删 `.tierpart`（`:465-468`）。
- 纯函数 `quota_verdict(avail: Option<u64>, declared: i64, written: u64, recheck: bool) -> Verdict`
  承载判定（三态可测）。
- 拒绝码用**既有词表** `op_failed` + 可行动文案（「出口磁盘可用空间不足（剩余 X MiB < 保留
  1 GiB）——上传被拒」）。**不新增错误码**（族④词表冻结，App `filesErrorMessage` 表同源）。
- `statvfs` 失败 → **fail-open** + 首 3 次节流告警（新日志，进 §4.2 additive）。

**理由（D1 详证见 §2）**：①「写满磁盘」的后果由水位唯一覆盖（每文件上限挡不住 N 个小文件累积）；
② Go 无任何上限 ⇒ 每文件硬上限会挡合法大文件（判据线有 100MB put/get 实测）且无 Go 可对齐；
③水位是「拒收新写入」而非「删已有」。

#### F3b 服务端请求行上限（N1；**对齐 Go，非新增加固**）

**问题**：`files_server.rs:538-548` 的 `read_line` 用 `read_until(b'\n')` 无界累积；
客户端文档口径与 Go 都是 64KB（Go：`MaxRequestLine = 64*1024` + `readLineLimited` 累积中判负，
回 `invalid_arg`「请求行超过 %d 字节」，`proto.go:39/144-146/159-175`），Rust 服务端漏用。

**方案**：`read_line` 增加上限（`MAX_REQUEST_LINE`，**累积中判负**——不只判已成形行）；
超限 ⇒ 回 `invalid_arg`，**文案逐字对齐 Go**「请求行超过 65536 字节」（Go 的 `%d` = 65536，
App 可见 msg 同串）⇒ 之后收线。`serve_busy` 的「吞一条请求行」同口径：**读出错/超限 → 回
`invalid_arg` 再收线**（对齐 Go `ServeConn` 的 `WriteLine(errorResponse(rerr))`，`server.go:151-155`），
**不是** v2 写的"直接收线"。

**测试计划**：UDS 对喂 100KB 无换行 + 换行 ⇒ 服务端回 `invalid_arg`（msg 逐字断言）+ 收线；
正常 60KB 请求仍通；busy 路径超长行 ⇒ 回 `invalid_arg`（非 busy）。

**判据行影响**：无编号判据行（E14 行文不变）；§4.3.3 登记改为「**与 Go 对齐**（补 Rust 漏用）」。

**F3 合计涉及文件**：`crates/homeway-core/src/files_server.rs`。

---

### F4（P1 + N7 + P2 后半）DNS：绝对期限 / worker 数 / TCP 拨号 / 回投容量

#### F4a 上游 UDP 读循环 **per-attempt 绝对期限**（审计 P1 主体；**v3 按 R1 修订**）

**问题**：`exchange`（`dnsproxy.rs:631-677`）在 `set_read_timeout(Some(budget))` 之后进入 `loop`，
四处 `continue`（`:652` 短包、`:656` ID 不匹配、`:658`/`:660` question 不一致）重新 `recv`
——`SO_RCVTIMEO` 是 **per-syscall**，每次 `recv` 都拿到新的一整份 `budget`；`deadline` 参数只在
TC→TCP 分支被消费（`:666`）。⇒ 上游持续灌不匹配包即可让该 worker **永不返回**；配合
`WORKERS = 2`（`:39`）⇒ 全通道瘫痪。

**方案（关键：重设的是本腿预算，不是全局 deadline）**：
- `exchange` 入口：`let attempt_deadline = Instant::now() + budget;`（`budget` = 调用方给的
  **本腿上试预算**，即 `per_try = min(remain/剩余腿数, MAX_PER_TRY)`，末次腿不吃上限，`:594-604`）。
- 循环每轮：`remain = attempt_deadline - now`，零即返回 `None`；否则 `set_read_timeout(Some(remain))`
  后再 `recv`（对齐 Go `conn.SetDeadline(time.Now().Add(budget))`，`server.go:415`）。
- `deadline`（全局 2.5s）**仍只喂 TC→TCP 分支**（`:666`）——不改变 `MAX_PER_TRY` 与
  「按序试下一个上游 + 末位兜底」的既有语义（v2 用全局 `deadline` 重设会让单个坏上游吃干整条
  查询预算、架空 `MAX_PER_TRY` 与上游回退序，并顺带改变 `fallback` 计数 —— **已按 R1 修订**）。
- 同仓先例：`egress::probe_with`（`egress.rs:462-470`，注释即 M7 现场）。

#### F4b DNS TCP 腿拨号期限（N7）

`exchange_tcp`（`:681-708`）改 `to_socket_addrs` + 逐地址 `TcpStream::connect_timeout(addr, remain)`
（`remain` 由 `deadline` 现算，零即返退回 UDP 截断应答）；Go 是 `net.Dialer{Timeout: budget}`（`server.go:449`）。

#### F4c worker 数（审计「worker 数评估」）

`WORKERS: 2 → 64`，并提为 `DnsConfig.workers`（默认 64、`filled()` 兜底；**M3**：现有 ~7 处
`DnsConfig{..}` 字面量统一改带 `..Default::default()`）。依据：①需求 = 到达率 × 上游时延
（手机整页解析突发 ~50 查询/100ms、慢上游 800ms/次 ⇒ 数十并发才不排队）；②资源上界
64 × 512KB 栈 = 32MB 虚存、常驻 ~1MB；③与既有 DNS 面容量同阶（`dnsface::MAX_TCP_CONNS = 64`）。
被否：`WORKERS = 256`（= Go 全等并发）——256 条常驻线程对出口进程不划算，且修掉 F4a 后 worker
不再会被**永久**占死，worker 数只决定稳态吞吐。残余：全死上游下稳态 ≈25.6 qps（Go ~102 qps）⇒ §6。

#### F4d 回投容量与在途/worker 一致（审计 P2 后半；**v3 按 R3/R4 修订：rx+tx 双侧**）

**问题**：`dnsface::attach`（`:140-145`）给 `udp53` 的 rx/tx 各 64 槽 + 各 64KB。
- **tx**：`drain_dns`（`intercept/mod.rs:1841-1866`）每拍排空回投通道，超过容量即 `send_slice` 失败
  ⇒ `deliver_udp53` 返 false ⇒ `udp_drop`（**应答丢失**，客户端超时重试）。
- **rx**（v2 误判，v3 订正）：smoltcp 0.14 `socket/udp.rs:522-529` 在 `rx_buffer.enqueue` 失败时
  `net_trace!("buffer full, dropped incoming packet")` —— **静默丢包且无计数**，不是"延后"
  （已读源码确认）。F4c 把并发从 2 提到 64 却把 rx 留在 64 槽/64KB = **自造一个静默丢弃面**。

**方案**：rx/tx **双侧**对齐到在途上限：
`UDP_TX_META = UDP_RX_META = MAX_IN_FLIGHT (256)` 槽；
`bytes = MAX_IN_FLIGHT × MAX_DNS_PAYLOAD_53 + MAX_DNS_PAYLOAD_53`（= 257 × 1232 = 316,624 B，
**+1 包余量**：smoltcp `PacketBuffer` 要求连续窗口并有 padding，`storage/packet_buffer.rs:80-115`
⇒ 恰好 256 满额包仍可能 `Err(Full)`）。
**依据改写（采纳 R4）**：必要性来源 = **tx 容量 ≥ workers × max payload**（64 × 1232 = 78,848 > 64KB
——这才是 F4c 之后必须同时扩 tx 的原因）；取 256 是"驱动线程停摆窗口内积压上界 = 在途上限"的
保守上界（v2 的"单拍 256 条 ⇒ 丢弃率 79%"把在途上限当单拍完成率，**已删除**）。
rx 侧额外登记：**smoltcp 的 rx 溢出无观测面**（本批不引入新计数；容量的作用是降低触发概率）⇒ §6 残余。

**涉及文件（F4 合计）**：`crates/homeway-core/src/server/dnsproxy.rs`（`exchange`/`exchange_tcp`/
`DnsConfig.workers`/`spawn`）、`crates/homeway-core/src/server/intercept/dnsface.rs`（rx/tx 容量常量
+ 注释）。**`DnsFaces` 结构体形态、`deliver_*` 签名、pending 表语义不动**（不与 Q-I 尾段撞面，
摩擦说明见 §6）。

**风险**：①worker 上升 ⇒ 对同一 nameserver 的并发从 2 → 64（Go 是 256；家用路由器 DNS 转发可承受）；
②rx/tx 各 +252KB 内存；③`udp_drop` 数值下降属预期（登记）。

**测试计划**：
- **F4a 负例（核心回归）**：fake 上游线程持续回**错误 ID** 的包（灌包）；**按生产形态构造**
  （`budget = 200ms`，`deadline = now + 2500ms`）⇒ 断言 `exchange` 在 `≤ 2×budget` 内返回 `None`
  （修前永不返回；若误用全局 deadline 重设，用例会跑满 2.5s ⇒ **红**，语义被钉死）。
- F4a 正例：正常 fake 上游仍回正确应答（既有 `forward_against_local_fake_upstream` 保持绿）。
- F4b：黑洞 TCP 地址（不 accept 的端口）⇒ `≤ budget + slack`（阈值 ≥2× 余量）返回。
- F4c：注入 `workers`（如 64）+ **并发**应答的 fake 上游（按请求起线程，避免 responder 串行化污染
  测量）⇒ 16 条查询总耗时 **相对断言**：`< 1/2 ×` 单 worker 形态耗时的理论值（并留 ≥2× 余量）。
- F4d：`DnsFaces::attach` 后向 UDP 面灌 200 × 1232B ⇒ `deliver_udp53` 全 true（**tx 容量用例**）；
  rx 侧用「同一栈 socket 连续注入 >64 条查询」断言不丢（或至少断言容量常量 == 在途上限）。
  说明：容量用例**不**支撑"流水线"论断（单拍完成率受 worker 数限制）；若需端到端断言，另立一条
  `submit_udp → 池 → drain_dns` 的集成用例（按需，非判绿前提）。

**判据行影响**：E22 行文不变；§4.2 登记（`fail` 与 `fallback` 的输入集、`udpDrop` 三输入）。

---

### F5（P1）files/speedtest：accept 错误分类 + 名额回滚 + 共享可停循环

**问题**：①`serve_stoppable` 把**任何** accept 错误当「监听已关」静默退出（`files_server.rs:162`、
`speedtest_server.rs:137`）——`EMFILE/ENFILE/ENOBUFS/ECONNABORTED/EINTR` 都是**瞬态**，一次瞬时错误
就永久摘掉该服务（socket 文件还在 ⇒ 客户端恒 `ECONNREFUSED`，直到重启出口）。Go 也退出接受循环，
但**至少** `ln.Close()`（Go 的 `UnixListener.Close` 会 unlink 路径）+ 打日志（`serve.go:459-465/530-537`
「别留一个文件在、无人收的监听点」）。
②**files** 侧 `:170-185`：`fetch_add(1)` 判在册后 `Builder::spawn(...).ok()` 吞掉 spawn 失败
⇒ 名额永久泄漏，16 次后所有连接恒回 busy（**永久 red**）。**speedtest 侧无名额泄漏**
（`admit` 在闭包体内，spawn 失败闭包不执行、`live` 从未加过——v3 订正，见 §0.2 注 4），
但其在册账同样缺 id 载体（F6b 需要）。

**方案**：
1. **错误分类（纯函数，抽一处、两模块共用 —— M2）**：
   ```rust
   pub(crate) enum AcceptAction { Retry, Backoff, Fatal }
   pub(crate) fn classify_accept_err(e: &std::io::Error) -> AcceptAction
   ```
   放在 `files_server.rs`（与既有共享 UDS 件 `listen_local_service` 同址），speedtest `use` 之。
   - `WouldBlock` → `Retry`（200ms 轮询，既有形态）；
   - `raw_os_error() ∈ {EMFILE, ENFILE, ENOBUFS, ENOMEM, ECONNABORTED, EPROTO, EINTR}` → `Backoff`：
     **不退出**，节流日志（首 3 次 + 每 100 次）+ 退避 200ms → 上限 1s；
     **E2 记录**：`ENETDOWN/EHOSTUNREACH/EOPNOTSUPP` 等会归 `Fatal`——UDS 上基本不可达，注释里
     写明理由；`EMFILE` 与"线程 spawn 也会失败"同源 ⇒ 1. 的退避与 2. 的回滚是一对（见下）。
   - 其它（`EBADF/EINVAL/ENOTSOCK/…` = 监听面真没了）→ `Fatal`：记行 + 返回 `Err`。
2. **名额回滚（RAII 守卫先建后 spawn）**：在册账做成守卫，在 `spawn` **之前**构造并随闭包 move 进
   线程 —— `Builder::spawn` 失败时闭包被 drop ⇒ 守卫 Drop 自动回收（无需 error-path 补丁）：
   - files：`ConnReservation { conns: Arc<AtomicUsize> }`（判定与构造同处）；**修两条路径**：
     spawn 失败（真实泄漏）+ 会话线程 panic（守卫兜底）；
   - speedtest：`live` 由 `Reservation` 驱动，`admit` 只分配**会话号** + 登记连接副本
     （`conns: Vec<(u64, UnixStream)>`）⇒ 为 F6b 的 `release(id)` 提供 id 载体。
3. **可停 accept 循环抽共享件（M2 + T3 一并解）**：
   ```rust
   pub(crate) fn serve_stoppable_accepts<A, H>(
       accept: A,               // A: FnMut() -> io::Result<UnixStream>
       on_conn: H,              // H: FnMut(UnixStream)
       stop: Arc<AtomicBool>, logf: &Logf, tag: &'static str,
   ) -> io::Result<()>
   ```
   files / speedtest 各自传入自己的 accept（`ln.accept()` 的薄包装）与 `on_conn`（spawn 会话线程）；
   错误分类、退避、节流日志、stop 优先全部在这一个循环里。**测试缝** = `accept` 可注入 ⇒
   「一次 `EMFILE` 后服务仍受理下一条连接」可测（T3）。
4. **不做**（有据，D6）：Fatal 退出时主动 unlink socket 文件——用户可见后果与 `ECONNREFUSED` 相同；
   与"另一实例已重绑同名 socket"有竞态；关机路径的 identity 校验型 unlink 已由引擎承担
   （`engine.rs:801/931-943`），且 `listen_local_service` 对死残留有判别（`files_server.rs:585-589`）、
   桥侧三态归一（`bridge_host.rs:344-351` → `files_op.rs:380` 归 `bridge_down`）。

**涉及文件**：`crates/homeway-core/src/files_server.rs`（分类器 + 共享循环 + 守卫）、
`crates/homeway-core/src/speedtest_server.rs`（改吃共享循环 + 守卫 + id）、
`crates/homeway-core/src/server/engine.rs`（两个 spawn 闭包补「退工」日志并按 `Err` 记行）。

**风险**：①停止位优先于 accept 的既有语义保持（共享循环里先判 stop）；②`EINTR` 通常由 std 内部
重试，纳入 `Backoff` 是保险；③行为差异（瞬态错误不再退工）须登记。

**测试计划**：
- `classify_accept_err` 纯函数：`WouldBlock` / `EMFILE` / `ENFILE` / `ENOBUFS` / `ECONNABORTED` /
  `EINTR`（`io::Error::from_raw_os_error(libc::…)`）/ `EBADF` / `InvalidInput` 逐案。
- **循环行为（T3）**：`serve_stoppable_accepts` 注入脚本化 accept（`EMFILE` ×2 → 一条真
  `UnixStream::pair`）⇒ 断言 `on_conn` 收到该连接、循环未退出、退避发生过（**修前该用例红**：
  现状一次错误即 return）。
- 守卫回滚：`ConnReservation` 构造后直接 drop ⇒ 在册计数回 0（等价 spawn 失败路径）；
  speedtest `Reservation` 同（断言 `live()` 归零）。
- 停止位：置 stop 后循环 200ms 内返回。

**判据行影响**：E13/E14/E17 行文不变；新增日志行（additive）登记；行为差异登记（瞬态 accept
错误不再摘服务；Go 会退出接受循环并由调用方 Close + 日志）。

---

### F6（P2）speedtest：绝对硬超时 / `release(id)` 收口 / busy 路径收口

#### F6a `Limits` + 绝对硬超时（含 N6/R8）

**问题**：`set_io_deadline`（`:384-394`）全程只在 `:171` 调一次 ⇒ 一次成功的慢读就把 30s 延长
⇒ 滴流客户端可占住并发槽任意长（「硬超时」名不副实）。限额全硬编码（Go 有 `Limits` + `SetLimits`
⇒ 不可注入 ⇒ 硬超时面**无法测**）。

**方案**：
- 移植 `Limits { max_conns, conn_timeout, max_warmup, max_window, max_block, send_block }`
  （**含 `send_block`**——Go `speedtest.go:81/100-102/220`，缺它会与"Go 同形/可注入"表述不符）
  + `with_limits(...)`；`Limits` 覆盖生产常量并供测试注入短超时。
- 硬超时绝对化：`arm_io(&conn, deadline)`（现算 `remain`、零即退）调用点**写清**（R8）：
  ① 每次**可能触达 socket 的读**前（`serve_conn` 首帧读、`serve_send` 每帧头读前）；
  ② 每次 `BufWriter::flush()` 前（**真正到达 socket 的写**——`serve_recv` 走 BufWriter，
  逐帧 arm 只打到 memcpy，R8 已指出）；③ `write_control` 前的直接写。每点一次 `setsockopt`（UDS ~µs）。

#### F6b `release(id)` 按会话收口

`conns: Vec<(u64, UnixStream)>`（会话号 → dup 句柄）；`release(id)` 只删自己那条；`close_all()`
语义保持（drain 全表 + shutdown + `live = 0`，此时 `live=0` 是**事实**）。`ConnGuard { conns, id }`
在 spawn 前构造（id 由 `admit` 填）、随闭包 move（与 F5.2 同一形态）。

#### F6c busy 路径与帧类型收口（含 N2/R8）

**问题**：①`read_frame_bounded`（`:505-512`）丢类型字节、`n > 512` 直接报错 ⇒ 载荷留流里，后续
吞输入从载荷中间对帧（错位）；②吞输入循环（`:495-501`）per-syscall 续命 ⇒ 永不返回（每 busy 连接
一条线程）。Go：帧吞窗 `now+2s`、循环 **`now+1s` 绝对期限**（`speedtest.go:427-440`）。

**方案**：①`read_frame_bounded` 按 `HEADER` + 类型处理：`TYPE_REQUEST` 才走 busy 回帧语义，
其余类型仍**把声明载荷流式吞完**（64KB 复用缓冲，超 `MAX_BLOCK` 即收线），保证帧边界不错位；
②`reply_then_close` 两段各自绝对期限：帧吞 `now+2s`、循环 `now+1s`（`BUSY_DRAIN_BUDGET = 1s`，
**对齐 Go 值**；v2 的 2s 已订正）。

**涉及文件**：`crates/homeway-core/src/speedtest_server.rs`。

**风险**：①`arm_io` 的 `setsockopt` 成本（UDS 可忽略）；②`ConnGuard` 带 id 后 `close_all` 仍须
覆盖"闭包外"的在册项——由表结构保证。

**测试计划**：
- F6a：注入 `conn_timeout = 300ms` + 滴流客户端（每 100ms 1 字节）⇒ 会话 `≤ 1s` 收线（阈值 ≥3×，
  修前一直活着）；**正例**：正常短会话（如 200ms 窗口）仍在宽松 timeout（2s）内完成。
- F6b：两条真实会话（`role=send` 挂住），一条 FINISH 收口后 `close_all()` ⇒ 另一条被断；
  `live()` 逐条 release 后准确归零。
- F6c：busy 路径喂「非 request 首帧 + 大载荷」⇒ 不错位、`≤ ~3.5s`（2s+1s+余量）返回、回帧到达；
  滴流客户端在 busy 路径下按期释放线程。

**判据行影响**：E13 行文不变；§4.2 登记「会话时长上界收紧」（滴流不再续命）。

---

### F7（P2 + N8）UPnP `http_call`：分调用上限 + 长度一致 + 绝对期限 + 拨号期限

**问题**：`http_call`（`upnp.rs:276-326`）：①`read_to_end` 无字节上限（`:309`）；②不校验
`Content-Length`；③读超时 per-syscall（`:294` 一次）⇒ 滴流可永久挂（缩租跑在停机主线程）；
④拨号 `TcpStream::connect`（`:285`）无超时（N8）。Go：ctx 盖拨号+读+体，体积闸
**`io.LimitReader`**——描述文件 `1<<20`、SOAP `1<<16`（**静默截断**，`upnp.go:73/235`）。

**方案**：
- **分调用上限（对齐 Go 值）**：`http_call(..., max_resp: usize)`；`igd_from_location` 传
  `HTTP_MAX_DESC = 1 << 20`，`Igd::soap` 传 `HTTP_MAX_SOAP = 1 << 16`。
- **超限行为 = 报错（**有意偏离 Go 的静默截断**，登记）**：截断会把 `GetGenericPortMappingEntry`
  的 Fault body 切掉 `>713<`/`SpecifiedArrayIndexInvalid` ⇒ 误判「表尾」或"清单残缺"，静默改变
  映射表语义；报错是诚实且可归因的（三个新错误变体，见下）。**依据**来自 `list_mappings`
  的表尾判定（`upnp.rs:485-498`）。
- **长度一致**：解析 `Content-Length`（既有 `header_value`）；声明 > 上限 ⇒ 立即报错（不等体）；
  读完若声明存在且实收 ≠ 声明 ⇒ 报错。
- **绝对期限**：成环读（8KB 复用缓冲），每轮按剩余重设读超时，到点报错（滴流防护）。
- **拨号期限**：`to_socket_addrs` + 逐地址 `connect_timeout(addr, remain)`；零即报错。
- **错误类型（M1，仓规：不用字符串错误）**：`UpnpError` 增 `RespTooLarge { limit }` /
  `ContentLengthMismatch { declared, got }` / `BudgetExhausted`（thiserror；`Display` 文案与日志
  消费面同串，additive 归因不变）。非 2xx 文案保持既有形态（`HTTP 非 2xx：… body=…`）。

**涉及文件**：`crates/homeway-core/src/server/upnp.rs`（`parse_http_url`/`header_value` 纯函数面不动）。

**风险**：①SOAP 上限 64KiB 与既有 `/ctrlu/…` 私有路径机型（本仓注释登记的实测形态）无冲突
（SOAP 应答本就小）；②`Content-Length` 一致性校验是**新增严格度**（非标机型可能不符）⇒ 登记；
③降级门：若真机验收发现误杀，可退化为「只按上限 + 绝对期限，不比长度」。

**测试计划**（模块内 mock HTTP）：正常（200 + 长度相符）⇒ Ok；`Content-Length: 8 MiB` 只发头
⇒ 立即 Err（不等体）；声明 100 实发 50 ⇒ Err（长度不符）；滴流（`deadline = now+300ms`，每 200ms
1 字节）⇒ `≤ 1s` 返回 Err（修前可无限挂）；黑洞地址 + `deadline = now+200ms` ⇒ `≤ 600ms` Err；
上限内的大体（SOAP 恰 64KiB）⇒ Ok（边界）。

**判据行影响**：无编号判据行；§4.2 登记新增错误串（additive）；§4.3 登记三条行为差异（体积闸
从无到有且超限行为不同 / Content-Length 校验新增 / 拨号期限收紧）。

---

### F8（P2 + N4）UPnP SSDP：来源过滤 + 非空 LOCATION

**问题**：`ssdp_location`（`upnp.rs:200-252`）取首个带 `LOCATION` 头的应答即返回，`from` 被丢弃
（`:237-247`）；空值 `LOCATION:` 也会被采纳（`:239`）。同 LAN 上任何主机都能把出口的 UPnP 控制面
指向任意地址（出口随后抓描述文件并发 SOAP ⇒ LAN 内 SSRF + 映射表扰动）。Go 同样不校验来源
（`upnp.go:155-164`）但要求 `LOCATION` 非空。

**方案**：新增纯函数
```rust
fn ssdp_response_ok(from: &SocketAddr, status_and_headers: &str) -> bool
```
判定：①来源为**私网或环回** IPv4 单播（排除 unspecified/multicast/broadcast/公网）；
②首行是 `HTTP/1.1 200` 或 `HTTP/1.0 200`；③`LOCATION` 存在且**非空**（对齐 Go）。
不满足者**继续读**（不返回、不报错），直到 3 次重试/期限耗尽（重试语义保持）。
**不加**「LOCATION 主机必须与候选 IP 同网段」：UPnP 真机形态本地不可测（既登记），同网段假设在
双网段/桥接机型有误杀风险 ⇒ 保守取三条件。**期限穿透见 F10**（`ssdp_location` 改收 `deadline`）。

**涉及文件**：`crates/homeway-core/src/server/upnp.rs`。

**风险**：真实网关若从不回 200 状态行会被误杀（三条件取最宽松合规形态 + 保留重试）；公开地址
LAN（云主机/公网 /29）会漏配 ⇒ 真机验收项，已登记可回退。

**测试计划**：`ssdp_response_ok` 五案（公网源+合规正文 ⇒ false；私网源+`HTTP/1.1 200`+非空
LOCATION ⇒ true；私网源+`HTTP/1.1 404` ⇒ false；私网源+`LOCATION:` 空 ⇒ false；私网源+无 LOCATION
⇒ false）；既有 `header_value_parse` 保持绿。

**判据行影响**：无编号判据行；§4.3 登记（非私网/畸形应答不再被采纳；Go 只查非空 LOCATION）。

---

### F9（P2 + N3）UPnP：轮级先加后删 + 所有权门 + 枚举一次 + 候选去重

**问题**：
1. `add_port_mapping`（`:433-447`）**无条件**先 `delete_mapping`——fail-open（枚举失败/残缺/含归属
   不明条目，`:623-645`，`verify=false`）时该端口可能是**别人的**映射，先删会删别人（Go 同形，
   `upnp.go:249-253` 把安全性托付调用方核验，fail-open 路径同样有此风险）。
2. `ensure_port_mapping`（`:579-619`）**三次全表枚举**（`find_our_mapping` → `clean_mappings` →
   `select_external_port` 各一次；**Go 也是三次** ⇒ 本项是优化不是对齐）。
3. `cands`（`:646-656`）未去重（Go 有 `tried`），`prefer ∈ [internal_port, +9]` 时同候选申请两次。
4. **轮级序仍是"先删后加"**（T1 指出）：`clean_mappings` 会删掉**所有** `Ours` 条目——含
   `find_our_mapping` 刚选为 `prefer` 的那条 ⇒ 我们当前的映射仍在轮级先被删。

**方案**：
1. **枚举一次**：`list_mappings` 结果封 `MappingTable { list: Vec<UpnpMapping>, complete: bool }`；
   `find_our_mapping(&table)` / `clean_mappings(&mut table, skip: u16, …)`（删成功的条目从快照剔除）/
   `select_external_port(&table, …)` 全吃快照。枚举 3 → 1（省 ≤2×(N+1) 次 SOAP 往返/轮）。
2. **轮级先加后删（T1 采纳 a）**：`clean_mappings` 新增 `skip_external_port`，传 `prefer` ⇒
   **prefer 那条在候选申请成功前不删**（路由器"就地更新"形态下零真空；718 形态下只剩一个往返的
   窗口，且加不回去时**原映射仍在**）——这才是真正的轮级"先加后删"。
3. **先加后删 + 所有权门**：`add_port_mapping(..., allow_evict: bool)`：
   先 `add_with_lease(3600)`（失败退 0，既有语义与日志保持）；冲突（`>718<`/`ConflictInMappingEntry`）
   且 `allow_evict` ⇒ `delete_mapping` + 重试；冲突且 `!allow_evict` ⇒ 直接 `Err`（换下一候选，
   **不动**既有映射）。`allow_evict = verify && 快照中该 ext 条目存在且 classify == Ours`。
   **残余（B2）**：路由器若对同名映射**静默覆盖**（不回 718），"先加"仍会顶掉别人的映射
   （Go 先删同样顶掉；本设计的改进只在路由器回 718 时成立）⇒ 登记，不得写成绝对保证。
4. **候选去重**：按 Go `tried` 形态去重（顺序仍 prefer → internal_port → +1…+9）。
5. **缩租同门**：`re_add_short_lease`（`:569-573`）改「先 add(300) → 718 才 delete + add(300)」
   （调用方已核验 ours ⇒ `allow_evict = true`）；失败时保持原租期（日志如实"缩租失败"）。

**涉及文件**：`crates/homeway-core/src/server/upnp.rs`。

**风险**：①`clean_mappings` 签名变（`&mut table` + `skip`）⇒ 调用方与测试同步；
②fail-open 下冲突端口被跳过（**更强**的保守行为）⇒ 登记；③「表说空闲、路由器报 718」不再强删
（Go 会删）⇒ 登记；④既有注释「Ours：先删后加即幂等重建」需同步改写（防注释-实现不符）。

**测试计划**（既有 mock IGD 扩展）：
- **枚举次数**：表长 N ⇒ 一轮后 `GetGenericPortMappingEntry` 计数 == **N+1**（第 N+1 次回 713 表尾；
  修前 3(N+1)）**并显式断言读到表尾标记**（T2）。
- **718 门**：mock 对「ext 已有他人映射」的 AddPortMapping 回 718 ⇒ `allow_evict=false` 时
  **`DeletePortMapping` 计数 == 0**，流程换下一候选；`allow_evict=true` 时 delete+add 成功。
- **轮级序（T1）**：mock 记录**整轮** SOAP action 序列 ⇒ 断言 `prefer` 端口上首调用是
  `AddPortMapping`（且在该端口的 `DeletePortMapping` 之前；无 718 时整轮不得出现该端口的 delete）。
- **去重**：`prefer == internal_port` + 该候选被拒 ⇒ `AddPortMapping` 尝试次数 == 1（修前 2）。
- **缩租**：ext 已存在 ⇒ AddPortMapping(718) → DeletePortMapping → AddPortMapping(300)，最终租期 300；
  「加不回去」形态 ⇒ 原映射仍在。

**判据行影响**：UPnP 日志族文案不变；新增 additive 行（未经核验不删/让位、缩租失败归因）；
§4.2 登记「每轮全表枚举 3(N+1) → N+1 次往返」（**优化，偏离 Go**）。

---

### F10（N5 + R2）UPnP 期限：SSDP 穿透 + 全局预算（两处调用点）

**问题**：①`shrink_upnp_lease`（`engine.rs:878-891`）对候选逐个 `discover_igd(cand, 8s)`——8s 是
**每候选**预算且无全局界；②更糟的是 `ssdp_location`（`upnp.rs:220`）**自带 5s 独立期限、不收预算**
（`discover_igd` 先跑 SSDP 再 `igd_from_location(&loc, budget)`，`:392-395`）⇒ 每候选最坏
**5s + 8s**，停机上界 ≈ N×13s（v2 的"N×8s"低估）；③`ensure_port_mapping` 同形（每候选
`discover_igd(cand, 40s)` ⇒ 最坏 N×45s），而 **Go 是同一个 ctx 贯穿全部候选**
（`publicendpoint.go:141` 40s / `serve.go:728` 8s，SSDP 取 `min(ctx, 5s)`，`upnp.go:140-144`）。

**方案**：
- 期限**穿透到 SSDP 腿**：`ssdp_location(local_ip, deadline)`——内部期限 = `min(deadline, now+UPNP_TIMEOUT)`；
  `discover_igd(local_ip, deadline)` 同参（描述文件抓取吃同一 deadline）。
- **全局预算**（对齐 Go 的单 ctx）：`ensure_port_mapping` 入口算
  `UPNP_TOTAL_BUDGET` 全局 deadline，`shrink_upnp_lease` 用 `UPNP_SHRINK_TOTAL_BUDGET = 8s` 全局；
  候选循环 = `pick_igd_before(cands, deadline, discover)`（`discover` 作**闭包参数**注入 ⇒ 可测），
  每轮按剩余收窄，到点即止。
- `ShrinkOutcome` 显式返回（`NoIgd / NoMapping / Shrunk / Failed`），engine 侧按结果打 additive 日志
  （静默跳过变可见）。

**涉及文件**：`crates/homeway-core/src/server/upnp.rs`（`pick_igd_before` / `ssdp_location` 签名）、
`crates/homeway-core/src/server/engine.rs`（两处调用点）。

**风险**：预算收紧后「多候选 + 慢网关」可能更早放弃缩租（映射保持 1 小时租期，自动过期，非安全面）
⇒ 登记（方向是**更接近 Go**）。

**测试计划**：①`pick_igd_before` 注入"睡 100ms 后失败"的 discover + 10 候选 + deadline 250ms ⇒
调用数 ≤3、总耗时 ≤ ~450ms（**全局**期限生效）；②**真 `discover_igd` 期限用例**（R2 要求）：
黑洞 IP + 短 deadline ⇒ 总耗时受界（`≤ deadline + 1 个 syscall 上界`），证明 SSDP 腿被穿透
（不注入假 discover 时也受界）。

**判据行影响**：无编号判据行；§4.3 登记停机时长上界由「N×(5s+8s)」→「8s」。

---

## 2. 「二选一」类决策的取证与裁定

### D1 上传配额口径：磁盘水位（选定）vs 每文件硬上限 vs 两者

- **取证**：①审计描述的后果是「可写满出口磁盘」——每文件上限挡不住 N 个文件累积，水位直接覆盖；
  ②Go 基线**无任何上限**（`server.go:509-524`），判据线已有 100MB put/get 实测 ⇒ 每文件上限会改动
  合法工作流且无 Go 可对齐；③出口文件根是**共享根**（缺省 = 用户 HOME），磁盘打满会连带打挂出口。
- **裁定**：做水位（起始门 + 每 8MiB 进行中门，保留量 1 GiB），不做每文件上限；拒绝码用既有
  `op_failed`（词表冻结）。

### D2 DNS worker 数取值：64（选定）vs 2（现状）vs 256（Go 全等）

- **取证**：需求 = 到达率 × 上游时延 ⇒ 数十并发；Go 的 256 是 goroutine（廉价），Rust 用原生线程
  不应对齐到 256；修掉 F4a 后 worker 不再会被**永久**占死，worker 数只决定稳态吞吐。
- **裁定**：64（`DnsConfig.workers` 可注入）。残余：全死上游下 ≈25.6 qps（Go ~102）⇒ §6。

### D3 UPnP 先加后删的所有权门 `allow_evict`

- **取证**：Go 把安全性完全托付调用方核验，但 fail-open 跳过核验却仍删除（两侧同洞）；
  审计点名「先删会删别人的映射」。
- **裁定**：`allow_evict = verify && 快照条目 classify == Ours`；代价 = 陈旧表 + 718 时改为让位
  （Go 会强删）⇒ 登记。**残余（B2）**：静默覆盖型路由器下"先加"仍会顶掉（改进仅对 718 型成立）。

### D4 files 客户端响应行是否另设数值上限

- **裁定**：不设上限，与 `facade/files_op.rs:132-153`（`read_line_capped(false)`）口径逐字一致
  （审计明确要求）。**理由改写（R6）**：契约一致性（对端 = 自己的出口；App 侧消费者早已如此）
  + 原 64KB 门是误移植；**不**用"任何上限都会拒合法响应"式的伪论证。残余登记 §6。

### D5 沙箱 TOCTOU 残余：是否上 `openat2`/`O_NOFOLLOW` 全量重写

- **取证**：`openat2(RESOLVE_BENEATH)` 仅 Linux；macOS 无对应能力；威胁模型 = 经隧道的设备。
- **裁定**：**不做**；F1 关掉审计的洞（含 §0.4 的绝对目标规则），TOCTOU 登记残余（措辞不得写"已消除"）。

### D6 accept Fatal 退出时是否主动 unlink socket 文件

- **裁定**：**不做**（F5 只做错误分类退避 + 名额回滚）。理由见 F5.4 + E3（评审核对认同）。

---

## 3. 测试与验收计划

**门（沿用批协议）**：
```bash
cargo test --workspace                # 全绿（含新增用例）
cargo clippy --all-targets -- -D warnings   # 无新告警
```

**新增/修改用例清单（按修复项）**：

| 修复项 | 用例 | 断言要点 |
|---|---|---|
| F1 | `files_server::tests::rel_path_*`（10 案，含悬空/链接链/循环/绝对目标/`file/leaf` 码位） | 逐形态 |
| F1 | `symlink_escape_no_write_outside`（**负例，审计点名**） | 六动词全失败 + 根外目录**零新增** |
| F2 | `files::tests::response_line_over_64k_accepted` | 200KB entries + 12MB text 响应行成功 |
| F3a | `quota_verdict_*`（三态）+ `avail_bytes_smoke` + `upload_rejected_when_low_disk`（注入 avail） | 放行/起始拒/中途拒；拒时无 `.tierpart` 残留、目标未变 |
| F3b | `request_line_over_64k_rejected` + busy 路径同案 | `invalid_arg` + **msg 逐字**（「请求行超过 65536 字节」）+ 收线；60KB 仍通 |
| F4a | `exchange_absolute_deadline_under_poison_upstream`（**生产形态**：budget=200ms / deadline=+2500ms） | 灌包下 `≤2×budget` 返回（误用全局 deadline ⇒ 红） |
| F4b | `tcp_leg_connect_bounded` | 黑洞 TCP 端口 `≤ budget + 余量` |
| F4c | `worker_pool_concurrency`（注入 workers + 并发应答 fake） | 相对断言（≥2× 余量） |
| F4d | `udp_tx_capacity_matches_inflight` + `udp_rx_capacity_matches_inflight` | 200 × 1232B / >64 条注入不丢 |
| F5 | `classify_accept_err_*`（8 案） | 归类正确 |
| F5 | **`serve_stoppable_accepts_survives_transient_error`**（注入脚本化 accept） | `EMFILE`×2 后仍受理下一条（**修前红**） |
| F5 | `conn_reservation_rolls_back_on_drop`（两模块） | 在册计数/live 归零 |
| F6a | `conn_timeout_is_absolute_under_dribble`（注入 300ms）+ 正例 | 滴流 `≤1s` 收线；正常会话宽松窗内完成 |
| F6b | `close_all_cuts_running_sessions_after_release` | 另一会话被断；live 精确 |
| F6c | `busy_path_drains_frame_and_bounded` | 非 request 首帧 + 大载荷不错位、`≤3.5s` |
| F7 | `http_call_cap_and_deadline_*`（5 案） | 超限立即拒 / 长度不符拒 / 滴流按期崩 / 黑洞拨号按期崩 / 边界 Ok |
| F8 | `ssdp_response_ok_*`（5 案） | 来源/状态行/LOCATION 三条件 |
| F9 | `enumerate_once_per_round`（断言 **N+1**） | 枚举计数与表尾标记 |
| F9 | `evict_only_when_verified_ours` | `allow_evict=false` ⇒ `DeletePortMapping` 计数 == 0 |
| F9 | **`round_level_add_before_delete`**（整轮 action 序列） | `prefer` 端口首调用是 AddPortMapping |
| F9 | `candidates_deduped` | 尝试次数 == 1 |
| F10 | `pick_igd_honors_global_deadline` + **真 `discover_igd` 期限案** | 调用数 ≤3、总耗时受界；SSDP 被穿透 |

**判绿 / 证伪条款**：
- F1：负例**必须**先在现状下复现（红）再实现（绿）；若负例在现状下不红 ⇒ 复验结论有误，回退重核。
- F4a：证伪条件 = 灌包形态下 `exchange` 越过 `2×budget`（期限未生效）**或**用例在"改用全局 deadline"
  的错误实现下仍绿（语义未被钉死）。
- F5：证伪条件 = 脚本化 accept 用例在现状下不红（说明没测到"瞬态错误摘服务"）。
- F9：证伪条件 = 枚举次数仍是 3 的倍数 / `allow_evict=false` 仍发 DeletePortMapping /
  整轮序列在 prefer 端口出现 delete 先于 add。
- **墙钟断言纪律（T4，参考 `wgcore::stackb` flake 前科）**：阈值留 **≥2× 余量**，或改用**相对/事件
  计数**断言（如"2 worker 形态必 ≥1.6s"的反向形态）；正例用宽松 timeout（如 2s），不贴目标值。

**不做实测的部分（如实登记）**：UPnP 真机（SSDP 组播本地被 macOS 拒，本仓已登记）；
水位门的真"磁盘写满"注入（用注入 avail 值 + 真 statvfs 冒烟替代）。

---

## 4. 判据行影响（v3 补齐 F2/F3a/F6c/F7/F4a-fallback/F4d 三输入）

### 4.1 编号判据行（行文**不变**）

| 判据行 | 本批影响 |
|---|---|
| **E14** `files 就绪：root=%s (rw) sock=%s（隧道IP:%d 经拦截层转投）` | 行文不变；F5 改变其后 accept 路径的**寿命语义**（§4.3.7） |
| **E17** `speedtest 就绪：sock=%s（…）` | 行文不变（同上） |
| **E13** speedtest 受理/结算行 | 行文不变；数值语义变化进 §4.2（会话时长上界收紧） |
| **E22** `dns: q=… drop=… malformed=…` | 行文不变；`fail`/`fallback` 输入集与 `drop` 容量语义进 §4.2 |
| **E4** `dns 代答就绪：…upstream=%s` | 不变（`Upstreams::list` 语义未动） |
| E10/E11/E12、C 族、R 族、DC 族、CA 族 | 不涉及 |

**无编号日志族**（UPnP / files accept / speedtest busy / files 水位）⇒ §4.2 + §4.3 登记。

### 4.2 计数输入集 / 数值语义变化（`INTEROP-CRITERIA.md` 登记草稿，行文不变）

| 日期 | 条目 | 从 → 到（数值语义） | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-E） | **E22** `dns: … fallback=… fail=…` | ① `fail` 输入集扩大：上游持续灌不匹配包/短包时，此前 worker 被永久占用、该查询永不结束（不计 `fail`、无应答）；现在按**本腿预算的绝对期限**结束 ⇒ 计 `fail` 并回 SERVFAIL。② `fallback` 输入集随 ① 上升（坏上游被按期判负后，后续腿与兜底才真正被尝试；此前是"永久挂住"）——**单腿预算（含 `MAX_PER_TRY=800ms`）与上游回退序不变** | F4a：per-syscall → **per-attempt** 绝对期限（对齐 Go `SetDeadline(now+budget)`） | E22 数值、DNS 单测、`dnsproxy.rs` |
| 2026-10-08（Q-E） | **`udpDrop`**（intercept 计数） | ① **输入集不变**（三个来源：DNS 回投 tx 写失败 `intercept/mod.rs:1861-1863`、`udp_send_to_client` 栈 tx 满 `:1519-1521`、`out_udp` 超限 `:2617-2619`）；② 数值下降：DNS 回投 rx/tx 容量 64 槽/64KB → 256 槽/316,624B（F4c 把 worker 2→64 使 tx 侧 64KB 先撞墙，故容量必须同比扩） | F4c+F4d：回投容量与 worker/在途对齐 | `udpDrop` 数值、`serve.status` intercept 段、`dnsface.rs` |
| 2026-10-08（Q-E） | **E13** 会话时长 | 滴流客户端此前每次成功读续命（30s 硬超时形同虚设）⇒ 现在绝对 30s 上界；正常会话（≤5s 预热 + ≤15s 窗口）不变 | F6a：硬超时绝对化 | E13 数值（时长字段）、speedtest 单测 |
| 2026-10-08（Q-E） | **UPnP 映射表枚举次数**（`GetGenericPortMappingEntry` SOAP 往返，非判据行） | 每轮 `3 × (N+1)` → `(N+1)` 次往返（N = 表长；第 N+1 次取表尾 713） | F9a：枚举一次缓存（**优化，偏离 Go 的 3 次**） | 路由器负载、UPnP 轮次耗时；日志行文不变 |
| 2026-10-08（Q-E） | **新增观测行（additive）** | 无 → 有：`files`/`speedtest` accept **瞬态错误退避**行（首 3 + 每 100）、accept **Fatal 退工**行、spawn 失败回滚行、files **水位检查失败 fail-open 告警**（首 3 + 每 100）、UPnP「枚举一次 / 未经核验不删（让位）」行、缩租 `ShrinkOutcome` 归因行（含"缩租失败"）、`http_call` 三条新错误串（超限/长度不符/期限耗尽） | F3a/F5/F6c/F7/F9/F10：静默路径改可观测 | 各日志族读者；非编号判据行 |

### 4.3 已知口径注记（登记草稿）

1. **【Q-E，2026-10-08】files 路径沙箱逐分量复核（F1，行为差异）**：`canonicalize` 快路径换成
   逐分量 walk（Go `os.Root` 同规则，§0.4 实测）——**(a)** 目标绝对的符号链接**一律** `not_found`
   （即便落在根内：现状 Rust 放行、Go 拒；本批对齐）；**(b)** 相对目标越界拒；**(c)** 悬空链接
   `not_found`（Go 实测亦回 ENOENT）。此前「不存在叶子原样放行」可在根外落文件（P0-4）。
   残余：复核与使用点分离的 TOCTOU 未收口（无 `openat2`，macOS 不可用；威胁模型为经隧道设备）；
   `stat` 的 `entry.name`（实址 basename vs Go 请求 basename）与 `write` 穿透符号链接语义（vs Go
   `rename` 替换链接）**均维持既有**，未在本批改动。
2. **【Q-E，2026-10-08】files 客户端响应行不设上限（F2，行为差异）**：>64KB 响应由「报错」→
   「正常返回」；与 `facade/files_op.rs` 的 `read_line_capped(false)` 同口径（原 64KB 门是服务端
   请求行上限的误移植）。残余：客户端内存随对端响应行增长（对端 = 用户自己的出口）。
3. **【Q-E，2026-10-08】files 服务端请求行上限（F3b，**与 Go 对齐**）**：由「无界读」→
   「`MAX_REQUEST_LINE` 64KB 累积中判负 ⇒ `invalid_arg`「请求行超过 65536 字节」（Go 同串）+ 收线」；
   `serve_busy` 路径同口径（读出错 → 回 `invalid_arg` 再收线）。**这是补 Rust 漏用（移植偏差修复），
   不是新增偏离**。
4. **【Q-E，2026-10-08】files 上传磁盘水位（F3a，行为差异）**：目标文件系统可用空间 <
   `UPLOAD_RESERVE_BYTES`（1 GiB）时拒收新上传（起始门 + 每 8MiB 进行中门），拒绝码 = 既有
   `op_failed`（**不新增码**）。Go 无任何上限（照收）⇒ **加固超出 Go 基线**；并发两条上传可越
   保留量 ≤8MiB 窗口；`statvfs` 失败 fail-open。
5. **【Q-E，2026-10-08】UPnP `http_call` 上限/长度/期限（F7，行为差异）**：①体积闸从无到有
   （**分调用**：描述文件 1 MiB / SOAP 64 KiB = Go 同值），但**超限报错**而 Go 是
   `io.LimitReader` **静默截断**（理由：截断会切掉 `>713<` 表尾判定体 ⇒ 静默改变映射表语义）；
   ②`Content-Length` 一致性校验为**新增严格度**（Go 无）；③拨号与读的**绝对期限**（Go ctx 同义）。
6. **【Q-E，2026-10-08】SSDP 应答来源过滤（F8，行为差异）**：只采纳「私网/环回来源 +
   `HTTP/1.1 200`/`HTTP/1.0 200` + 非空 LOCATION」（Go 只查非空 LOCATION、不校来源）；
   未采纳者继续读、不中断重试；公开地址 LAN 会漏配（真机可回退）。
7. **【Q-E，2026-10-08】UPnP 加映射的所有权门与轮级序（F9，行为差异）**：①轮级「先加后删」——
   `clean_mappings` 跳过 `prefer` 条目 ⇒ 候选申请成功前**不删**我们当前的映射；②仅当「枚举完整
   且快照明确属于我们」才允许先删后加（幂等重建）；fail-open 与「表说空闲但路由器报 718」改为
   **让位下一候选、不删既有映射**（Go 会强删）；③缩租改「先 add(300) → 718 才 delete + add(300)」；
   **残余**：路由器若**静默覆盖**同名映射（不回 718），"先加"仍会顶掉别人的（Go 先删同样如此）。
8. **【Q-E，2026-10-08】UPnP 期限穿透与全局预算（F10，行为差异）**：`ssdp_location`/`discover_igd`
   收 `deadline`（内部取 `min(deadline, 5s)`）；`ensure_port_mapping` 40s / 缩租 8s 均为**全局**
   预算（对齐 Go 单 ctx）。停机路径（`serve_cli.rs:482`、`unified_cli.rs:912/939/1470`）最坏时长由
   N×(5s+8s) 降到 8s。
9. **【Q-E，2026-10-08】accept 错误分类（F5，行为差异）**：`EMFILE/ENFILE/ENOBUFS/ENOMEM/
   ECONNABORTED/EPROTO/EINTR` 从「当监听已关、静默退出」→「退避重试 + 节流日志」；Go 的接受循环
   遇错即退出（由调用方 Close + 日志）。Fatal（EBADF/EINVAL/…）仍退工，但现在**记行**。

### 4.4 判据变更记录（正式登记行）

本批**无编号判据行的「从 → 到」行文变更** ⇒ 「判据变更记录」表**不新增行**；§4.2/§4.3 分别落到
该文件既有「计数输入集 / 数值语义变化」表与「已知口径注记」节（与代码变更同批 commit）。
**说明（G3）**：Q-C 曾把一条**非判据行**的中继运维日志放进「判据变更记录」表；本批不照走该形态——
本批的非判据日志变更全部是 **additive 观测行**（无既有行文被改），按政策归「计数输入集/数值语义
变化」表的 additive 行 + §4.3 已知口径注记即可，避免登记表与判据行变更混编。

---

## 5. 设计门记录（dsh 外部评审）

> 姿势：`~/.agents/skills/reviewer/SKILL.md` 固定姿势；仓根 `/Users/zhaozhe/Documents/projects/homeway-rs`
> 下**前台**跑 dsh，prompt 与结果全落临时文件；**成败只认 exit code**。

### 5.1 结论

- 轮次目录：**`/tmp/dsh-review/r9.0mrmgB`**（`prompt.txt` / `output.md` / `stderr.log` 三件套留档）。
  - `output.md` = 193 行 / 33,100 B（**已用 Read 工具全文读完**，非截断转述）。
- **exit code = 0**（`dsh --profile headless`（前台）→ `echo "exit=$?"` ⇒ `exit=0`；stderr.log 为
  推理流，无错误行）。
- **过门结论：通过**。评审提出 **3 条阻塞项**（S1 悬空符号链接分支未定义 / G1 服务端请求行上限
  定性错误 / R1 期限语义错层）+ **10 条中优先**（S2/R2/R3/R4/R7/E1/G2/G6/T1/T3）+ **17 条低**
  （共 **30 条独立意见**；`R5≡G1`、`G5≡R7` 两条为交叉引用不单列）；
  **0 条不认同**——全部经回源码/回 Go 基线独立复核后认同并已并入 v3（§5.3）。
- 评审独立做的事（增强可信度）：自行读全部 Rust 模块与 Go 快照；**自行用 Go 1.24.5 toolchain
  实跑 `os.Root`** 复核符号链接语义（并以 `io.LimitReader` 行号核对体积闸，§0.4 由本棒
  **独立复跑**证实）；逐行核对复验表的 13 行定性（其中 2 行被纠正）。

### 5.2 评审原文摘要（逐条；按评审者分节与编号）

**A. §0.1 对照表独立复核**：13 行中 12 行成立；**第 5 行不成立**（「Go 半边错，定性随之错」）；
**第 9 行 Go 半边写漏**（「偏差被低估」——Go 有 `io.LimitReader` 描述 1<<20 / SOAP 1<<16）。

- **S1【高/阻塞】** F1 方案第 2 条 + 测试计划：**悬空符号链接（dangling symlink）分支未定义，
  且按字面实现会保留同一类逃逸**。`canonicalize` 对目标不存在的链接返回 ENOENT，若把该 ENOENT
  并入「分量 ENOENT ⇒ 返回基点 + 剩余分量」，会把 `link` 当"不存在"放行 ⇒ 形如
  `link -> /tmp/outside/newfile` 即根外落文件。**关键点：设计未给该分支的判定，也未给对应负例。**
  建议：显式规定并落测试（绝对目标 ⇒ not_found / 相对目标词法规整后越界 ⇒ not_found / 悬空 ⇒
  等价写法"canonicalize 失败且是符号链接 ⇒ 一律 not_found"，或按 Go 三规则）。
- **S2【中】** §F1 风险第 2 条 + §6 末行：**「本机无 os.Root 可复现」前提不成立**——module cache 有
  `toolchain@v0.0.1-go1.24.5`，离线可跑。实测：相对根内链接放行（`Stat` 成功、`OpenFile("inrel/newfile")`
  成功且落在根内）；**绝对根内链接 ⇒ `path escapes from parent`**；越界/悬空全拒。⇒ 本仓对「绝对
  目标但落在根内」比 Go **宽松**，是既有可判定差异；设计把它挂"待 Q-J 取证"代价是 §4.3.1 文字说
  得比事实宽。
- **S3【低/记录】** `resolve_in_root` 返回实址 ⇒ `stat("link")` 的 `entry.name` = 实址名而 Go 用
  `fi.Name()`（请求 basename）；`write` 走实址 = 更新目标文件而 Go `rename` 替换链接本身。既有行为，设计未提。
- **S4【低】** 测试清单缺：符号链接链（`l1 -> l2 -> 根外`）、组件中途普通文件（`file/leaf` 应落
  `op_failed`）、悬空相对链接。
- **S5【低/看过】** TOCTOU 残余与 D5 裁定一致、可接受；提醒登记文字别写成"已消除 TOCTOU"。
- **R1【高/阻塞】** §F4a + §4.2 首行 + §3 用例：**把「per-syscall → 绝对期限」修成了「per-try →
  全局期限」**。Go 的 `SetDeadline(now+budget)` 里 `budget` = **本腿试预算**（Rust 的 `per_try`），
  全局 `deadline` 只喂 TC→TCP。按 v2 写法：①单个坏上游能吃掉整条查询预算，`MAX_PER_TRY=800ms` 与
  「按序试下一个 + 末位兜底」被架空；②`fallback` 输入集随之上移而 §4.2 只登记了 `fail`；③与
  "对齐 Go" 的登记断言不符。附：用例若 `deadline` 也传 200ms ⇒ 恒绿、测不到偏差。
  建议：入口算 `attempt_deadline = now + budget`，循环按它收窄；用例按生产形态（budget=200ms /
  deadline=+2500ms）以 `≤2×budget` 判绿红。
- **R2【中】** §F10：「全局 8s」在现调用链上**不可达**——`ssdp_location` **不收 budget**、自带
  5s 独立期限，之后才轮到 `igd_from_location(budget)` ⇒ 停机上界是「≈8s + 5s」，现状最坏也不是
  N×8s 而是 ≈N×(5+8)s（N5/§4.3.6 都低估 5s/候选）；且 §3 用例注入**假 discover** 恰好绕开真 SSDP
  腿 ⇒ 结构上测不到该洞。建议：期限穿到 SSDP 腿，或如实写「≤ deadline + SSDP 单腿上界」，并补一条
  走真 `discover_igd` 的期限用例。
- **R3【中】** §F4d rx 半边 + §6：「**rx 溢出 = 延后不是丢弃** 是错的」——smoltcp 0.14
  `socket/udp.rs:522-529`：`rx_buffer.enqueue` 失败即 `net_trace!("buffer full, dropped incoming packet")`
  = **静默丢包、无计数**。F4c 把 worker 2→64 却把 rx 留在 64 槽/64KB = **新增静默丢弃面**。
  建议：rx 同口径抬到 256 槽 / 315392B，或至少登记「rx 满 = 静默丢弃（smoltcp 不计数）」。
- **R4【中】** §F4d 量化 + §4.2 第 2 行：① 「在途上限 256 ⇒ 单拍完成 256」把在途当单拍完成率
  （单拍 ≤ worker 数）；79% 丢弃只在驱动线程停摆数分钟下成立；② `udpDrop` 有**三个**输入
  （DNS 回投 / `udp_send_to_client` 栈 tx 满 / `out_udp` 超限）而登记只归因一个；③ smoltcp
  `PacketBuffer::enqueue` 要求**连续窗口**并会 padding（`storage/packet_buffer.rs:80-115`）⇒ 第 256
  个满额包仍可能 `Err(Full)`，要给 ≥1 包余量。建议：依据改成「tx 容量 ≥ workers × max payload」，
  `UDP_TX_BYTES` 给 1 包余量，登记三输入。
- **R5【高/阻塞】** = G1（定性错误，登记方向反了）。
- **R6【低】** §F2/D4：结论可接受，但"任何 <100MB 上限都可能拒合法响应"不成立（同论证可推"任何
  上限都该去掉"）；真实理由是契约一致性 + `files_op` 已如此 + 原 64KB 门是唯一内存界。另 F2 的
  行为差异在 §4 里**没有落点**。
- **R7【中】** §F7：Go 的体积闸是**分调用**的（desc 1<<20 / SOAP 1<<16）且 `io.LimitReader` **静默
  截断**；设计的统一 1MiB + 报错 ⇒ SOAP 面比 Go 宽 16×、行为不同；Content-Length 一致性校验是
  Go 没有的新严格度。这些在 §4.3 里没有条目。建议：拆两个常量或说明合并理由；登记三条差异。
- **R8【低】** §F6a/F6c：① `serve_recv` 走 `BufWriter` ⇒ "每帧写前 arm" 多半只是 memcpy，真正决定
  阻塞的是 `flush()`；② `BUSY_DRAIN_BUDGET=2s` 与 Go 的 1s 不同值（应取 1s）；③ `Limits` 漏 Go 的
  `SendBlock`。
- **E1【中】** §0.2 条目 4 括注 + §F5 问题②：**「speedtest 同形（admit 已 live+=1，spawn 失败无人
  回滚）」与代码不符**——`admit` 在 `serve_conn` 临界区内，而 `serve_conn` 是**被 spawn 的闭包体**
  ⇒ spawn 失败闭包不执行 ⇒ `live` 根本没加过 ⇒ **不存在名额泄漏**（files 半边成立）。属复验结论错，
  按批协议应订正。
- **E2【低/看过】** errno 集与判定可接受（UDS errno 域窄；三目标 `libc` 常量可用；`EINTR` 归
  Backoff 的保险对）；提醒 `ENETDOWN/EHOSTUNREACH/EOPNOTSUPP` 会归 Fatal（UDS 基本不可达，可注释）；
  并指出 **EMFILE 正是线程 spawn 也会失败的时刻** ⇒ 守卫回滚是好协同。
- **E3【低/看过】** D6（Fatal 不 unlink）裁定可接受，并给出四条支撑（引擎 `remove_sock_own` 身份
  校验、`listen_local_service` 死残留判别、`bridge_host` 三态归一、客户端可见后果相同）。
- **G1【高/阻塞】** §0.1 第 5 行 + §0.3 N1 + §F3b + §4.3.3：**Go 有服务端请求行上限**
  （`proto.go:39 MaxRequestLine = 64*1024`、`:144-146 ReadRequest → readLineLimited`、`:159-175`
  累积中判负并回 `invalid_arg`「请求行超过 %d 字节」）；引用的 `proto.go:203-215` 其实是 `ReadFrame`；
  `br.ReadBytes('\n')` 只存在于客户端。Rust 常量已存在且客户端在用，**只有服务端漏用** ⇒ F3b 是
  **恢复对齐**，§4.3.3 登记方向要改；文案应**逐字对齐 Go**（「请求行超过 65536 字节」）；`serve_busy`
  路径应「回 invalid_arg 再收线」而非直接关。
- **G2【中】** §4.2/§4.3/§4.4 整体：**登记清单不穷尽**——缺 F2、F7（三条新错误串）、F6c
  （非 request 首帧吞完 + busy 窗值）、F3a（statvfs 失败告警行）、F4a 对 `fallback` 的影响、
  F4d 的 `udpDrop` 三输入。落位方式本身与政策一致。
- **G3【低/看过】** 「无编号行文变更 ⇒ 登记表不新增行」与政策及 Q-B/Q-D 先例一致；建议加一句说明
  为何本批不照 Q-C 的形态。
- **G4【低】** §4.2 第 1 行：登记文字正确但漏 `fallback`；另 `continue` 处数写"三处"**实为四处**
  （`:652/:656/:658/:660`）——§0 自称"行号=实测值"，请订正。
- **G5【低】** 并入 R7。
- **G6【中/流程】** 文档头「状态：v2——设计门已过」与 §5 现状**矛盾**（§5 是回填位、`/tmp/dsh-review`
  下 r9 只有本轮自身）⇒ **没有任何已完成轮次支撑"已过"**，且 §5.5 自称"不预设结论"而现状恰是先写了
  结论。建议：状态行改"v2（待设计门）"，本轮处置后写 v3 并**完整回填 §5** 再提交。
- **M1【低】** §F7：新增三种错误做成字符串 `UpnpError::Io(io::Error::other(...))` 与仓规
  「错误一律 thiserror 类型」相抵。建议加三个变体（Display 与日志消费面同串）。
- **M2【低】** §F5：分类器 + 退避 + 节流日志在两模块各写一份属无谓重复（本仓已有
  `files_server::listen_local_service` 被 speedtest 复用的先例）。建议抽一处导出。
- **M3【低】** §F4c/§F3a：`DnsConfig` 是 pub 结构、~7 处字面量构造需同步；F3a 的"可注入 avail"
  没给缝。建议统一 `..Default::default()`；明说 avail 的注入方式。
- **B1【低/基本没问题】** §0/§6：边界登记足够硬；两处摩擦值得写进 §6（F4d 与 Q-I 尾段改同一
  `dnsface.rs` 的相邻函数；F3a/F3b 会碰 Q-I 尾段"files 拷贝与分配"的同一批代码——顺序成本非返工）。
- **B2【低】** §6：所有权门的残余没登记——**静默覆盖**语义的路由器下，`allow_evict=false` 的"先加"
  仍会顶掉别人的映射（改进只在回 718 时成立）。
- **B3【低/看过】** §6 残余四条登记到位（配合 G2 补齐条目）。
- **T1【中】** §F9：①**轮级序仍是"先删后加"**——`clean_mappings` 会删掉**所有** `Ours` 条目（含刚
  被选为 `prefer` 的那条）⇒ "不再有映射真空"只在缩租与 718 分支成立；②用例 `add_before_delete_order`
  只断言函数内部首调用 ⇒ **轮级仍是 delete→add 时照样绿**（典型"测了但测不到病"）。建议：让
  `clean_mappings` 跳过 `prefer`，或明说轮级仍是 clean 先删；用例改断言**整轮**序列。
- **T2【低】** §F9 枚举断言：表长 N 时 mock 实收 **N+1** 次（第 N+1 次回 713）；"== N"会让实现者
  改 mock 凑断言。建议断言 `== N+1` 并显式断言读到表尾标记。
- **T3【中】** §3 F5 段：F5 只测纯函数与守卫 drop ⇒ **"瞬时错误不再摘服务"没有任何用例**；同类
  缺口：F1 悬空链接负例、F10 真 SSDP 期限、F4d 流水线级。
- **T4【低】** §3 墙钟阈值（≤400ms/≤600ms/≤800ms/≤2.5s）余量偏紧（本仓有 `wgcore::stackb` flake
  前科）。建议 ≥2× 余量或改相对/事件计数断言；正例用宽松 timeout。
- **§8 看过、明确没发现问题**：§0.1 表 12/13 行定性（除第 5 行）逐符号核对成立；F1 单点收口成立；
  F2 前提成立；F3a 的 OHOS 可行性（`statvfs` 三目标可用、`libc` 无条件依赖）与起始门位置成立、
  `op_failed` 选择正确；F4a 的 `<12B` 与 `budget.is_zero()` 同语义、`egress::probe_with` 类比成立；
  F4d 现状值（64/64KB、1232、64、`drain_dns` 全排空）核对无误；F5「停止位优先」、F6b/F6a/F6c、F7
  四条现状、F8、F9 三次枚举与无条件 delete、F10 四处调用点均成立（仅 `unified_cli:912` 是收尾线程内
  串行、不阻塞调用方，措辞可微调）；D1/D2/D3/D5 取舍自洽；F8 三条件保守可接受（提示公开地址 LAN 会漏配）。
- **§9 总评原文**：「**不能按现状过设计门**……有 **3 条必须先解决的阻塞项**——① S1……② G1……
  ③ R1……**中优先级**（应随本轮一并改）：G2 登记补齐、R3 rx 静默丢弃、R4 F4d 依据改写、R2 F10 的
  SSDP 期限穿透、T1 F9 轮级先删后加与用例、E1 speedtest 名额泄漏误判订正、R7 F7 的 Go 分调用体积闸、
  G6 §5 回填（本轮意见处置后写 v3，删除"已过"的预写结论）。其余为低优先级记录项；上述处置并入 v3
  并回填 §5（含本轮 `r9` 路径与 exit code）后，本设计门可过。」

### 5.3 逐条处置表

| # | 意见（摘要） | 处置 | 落点 |
|---|---|---|---|
| S1 | 悬空符号链接分支未定义（高/阻塞） | **认同** | F1 方案 1 显式三规则（绝对拒 / 相对越界拒 / 悬空 `not_found`，深度上限 40 防循环）；测试加悬空 + 链接链 + 循环三案；§0.4 实测佐证 |
| S2 | 「Go os.Root 无法取证」前提不成立（中） | **认同（并独立复跑证实）** | §0.4 新增实测节（六形态表 + toolchain 路径 + 脚本留档）；F1 去掉 canonicalize 快路径、**顺带对齐「绝对目标一律拒」**；§6「待取证」条目删除；§4.3.1 文字订正 |
| S3 | `entry.name`/写入穿透语义与 Go 不同（低） | **认同（登记不修）** | §4.3.1 末句 + §6 残余（本批不改；需要 openat/renameat 面，另行排期） |
| S4 | 缺三条负例（低） | **认同** | §3 F1 用例 ⑤⑥⑦（链 / 循环 / `file/leaf` ⇒ `op_failed`） |
| S5 | TOCTOU 登记措辞（低） | **认同** | §F1 风险 + §4.3.1：明确"未收口"，禁用"已消除" |
| R1 | 期限语义错层（高/阻塞） | **认同** | F4a 改 **per-attempt** 绝对期限（`attempt_deadline = now + budget`）；§4.2 E22 行补 `fallback`；用例按生产形态（budget=200ms / deadline=+2500ms），并把"误用全局 deadline ⇒ 红"写成证伪条件 |
| R2 | 全局 8s 不可达（SSDP 未穿透）（中） | **认同** | F10 期限穿透 `ssdp_location`/`discover_igd`（`min(deadline, 5s)`）+ **两处调用点**都改全局预算（缩租 8s / 公网端点 40s，对齐 Go 单 ctx）；N5/§4.3.8 数字订正为 N×(5s+8s) → 8s；§3 补**真 discover_igd** 期限用例 |
| R3 | rx 面静默丢弃（中） | **认同（本棒读 smoltcp 源码证实）** | F4d 改 **rx+tx 双侧** 256 槽 / 316,624B（含 +1 包余量）；§6 补"smoltcp rx 溢出无观测面"残余；删掉"rx = 延后"错误论断 |
| R4 | F4d 依据与量化（中） | **认同** | F4d 依据改「tx 容量 ≥ workers × max payload（64×1232=78848 > 64KB 才是必要性来源）+ 停摆窗口上界 256」；`+1` 包余量；§4.2 补 `udpDrop` **三输入**；删"单拍 256 条/79%"；容量用例定位改为"容量用例"、不支撑流水线论断 |
| R5 | = G1 | **认同** | 同 G1 |
| R6 | F2 理由与登记落点（低） | **认同** | §F2 理由改写（契约一致性 + 误移植 + 残余）；§4.3.2 新增 F2 条目 |
| R7 | F7 上限分调用 + 超限行为（中） | **认同** | F7 拆 `HTTP_MAX_DESC = 1<<20` / `HTTP_MAX_SOAP = 1<<16`（Go 同值）；**超限报错**（不静默截断，理由 = 会切掉 `>713<` 表尾判定体）；§4.3.5 登记三条差异 |
| R8 | arm 点/BUSY 值/SendBlock（低） | **认同** | F6a arm 点写清（读 + `flush()` 前 + 直写前）；F6c `BUSY_DRAIN_BUDGET = 1s`（对齐 Go）；`Limits` 补 `send_block` |
| E1 | speedtest 名额泄漏误判（中） | **认同** | §0.2 条目 4 订正（files 真泄漏 / speedtest 无）；§0.2 注 4；F5 问题② 与「收益」改写（files：spawn 失败 + panic 兜底 / speedtest：id 载体 + RAII 统一）；§5.4 证据 |
| E2 | errno 集与判定（低） | **认同（记录）** | F5 方案 1 加 ENETDOWN/EHOSTUNREACH/EOPNOTSUPP 归 Fatal 的注释；点明 EMFILE 与 spawn 失败同源 ⇒ 退避与回滚是一对 |
| E3 | D6 裁定（低） | **认同（无改动）** | F5.4 + D6 引用评审核对结论（四条支撑已并入 D6） |
| G1 | Go 有请求行上限，定性反了（高/阻塞） | **认同（本棒复读 `proto.go` 证实）** | §0.1 第 5 行改「Rust 移植偏差（服务端漏用既有 64KB 上限）」；§0.3 N1 定性同步；F3b 改「累积中判负 + 回 `invalid_arg`」、文案**逐字**「请求行超过 65536 字节」、busy 路径「回 invalid_arg 再收线」；§4.3.3 改「与 Go 对齐」 |
| G2 | 登记清单不穷尽（中） | **认同** | §4.2 补 F4a-`fallback` / F4d 三输入 / F3a 告警 / F6c / F7 新错误串 / F5 与 F9/F10 additive 行；§4.3 补 F2/F6c(1s 窗)/F7 三条 |
| G3 | §4.4 加说明（低） | **认同** | §4.4 末段（为何本批不照 Q-C 形态） |
| G4 | continue 处数与 `fallback`（低） | **认同** | §0.2 条目 3 与 F4a 订正为**四处**（`:652/:656/:658/:660`）；`fallback` 并入 R1/§4.2 |
| G5 | 并入 R7 | **认同** | §0.1 第 9 行 + §0.2 注 6 |
| G6 | §5 回填与状态行（中/流程） | **认同** | 本文头部状态行改 **v3（设计门已过，`r9.0mrmgB` exit=0）**；§5 全节完整回填（本节即回填产物）；`§5.5` 并入 §5.1–5.3 |
| M1 | UPnP 错误用类型（低） | **认同** | F7 增 `UpnpError::RespTooLarge / ContentLengthMismatch / BudgetExhausted`（thiserror，Display 与消费面同串） |
| M2 | 分类器重复（低） | **认同（并扩为共享循环）** | F5 方案 1/3：`AcceptAction`/`classify_accept_err` + `serve_stoppable_accepts`（可注入 accept）抽到 `files_server`，speedtest 复用（同时解 T3） |
| M3 | DnsConfig 字面量 / avail 注入缝（低） | **认同** | F4c 注 `..Default::default()`；F3a 明说注入缝（`FilesServer.avail: Arc<dyn Fn(&Path)->Option<u64>>`） |
| B1 | 同文件摩擦登记（低） | **认同** | §6 补两条摩擦（`dnsface.rs` 相邻函数 / Q-I 尾段 rebase 成本） |
| B2 | 静默覆盖型路由器残余（低） | **认同** | F9 方案 3 残余 + §4.3.7（不得写成绝对保证） |
| B3 | 残余登记确认（低） | **认同（无改动）** | §6 |
| T1 | 轮级仍是先删后加 + 用例测不到病（中） | **认同（采纳"clean 跳过 prefer"版）** | F9 方案 2（轮级真先加后删）+ 方案 1 `clean_mappings(&mut table, skip, …)`；§3 用例 `round_level_add_before_delete`（整轮 action 序列）；§4.3.7 ① |
| T2 | 枚举断言应 N+1（低） | **认同** | §3 F9 用例断言 `== N+1` + 显式断言读到 713 表尾 |
| T3 | 「瞬态错误不摘服务」无用例（中） | **认同** | F5 方案 3 的可注入 accept 缝 + §3 用例 `serve_stoppable_accepts_survives_transient_error`（修前红，写入证伪条款） |
| T4 | 墙钟阈值紧（低） | **认同** | §3 新增「墙钟断言纪律」段（≥2× 余量 / 相对或事件计数 / 正例宽松窗） |

### 5.4 不认同项

**无。** 本轮 **30 条独立意见**（3 高 / 10 中 / 17 低；`R5≡G1`、`G5≡R7` 交叉引用不单列）
**全部认同**，其中 **4 条**（S2、R3、G1、E1）经本棒
**独立复核**确认评审判定正确、**v2 原文有误**，证据如下：

1. **S2**：本棒离线复跑 Go 1.24.5（`GOTOOLCHAIN=go1.24.5`，脚本 `/tmp/qe-rootcheck/main.go`），
   六形态输出与评审一致（绝对根内链接 `path escapes from parent`；相对根内放行且 `newleaf` 落根内；
   悬空回 ENOENT；`outside entries: 0`）⇒ §0.4。
2. **R3**：读 `~/.cargo/registry/src/.../smoltcp-0.14.0/src/socket/udp.rs:521-529`——
   `match self.rx_buffer.enqueue(size, metadata) { Ok(buf) => …, Err(_) => net_trace!("buffer full,
   dropped incoming packet") }` ⇒ **静默丢弃**确认，v2 的"延后"论断错误。
3. **G1**：读 `baseline/homeway/pkg/files/proto.go:39/144-146/159-175`——`MaxRequestLine = 64*1024`
   与 `readLineLimited`（累积中判负，`Errf(CodeInvalidArg, "请求行超过 %d 字节", max)`）确认存在。
4. **E1**：读 `crates/homeway-core/src/speedtest_server.rs:124-166`——`admit` 位于 `serve_conn`
   临界区内，`serve_conn` 是 `spawn` 的闭包体 ⇒ speedtest 无名额泄漏（files 侧的 `fetch_add`
   在 accept 循环内，泄漏成立）。

> 说明（评审者亦提出、本棒确认）：R6 中「任何 <100MB 上限都可能拒合法响应」的写法属**伪论证**，
> 已换成契约一致性理由；这属"评审指出本设计论证质量不足"，同样认同。

### 5.5 过门结论

- 3 条阻塞项（S1/G1/R1）**已全部并入 v3**（F1 三规则 + 负例；F3b 改对齐 Go；F4a 改 per-attempt 期限）；
- 10 条中优先（S2/R2/R3/R4/R7/E1/G2/G6/T1/T3）**已全部并入 v3**；
- 17 条低项（S3/S4/S5/R6/R8/E2/E3/G3/G4/M1/M2/M3/B1/B2/B3/T2/T4）**已全部处置**（改设计或登记；
  G5 为 R7 交叉引用）；
- **0 条不认同**；无遗留待裁项 ⇒ **设计门通过**，可进第 2 棒（实现）。

---

## 6. 不做项与移交登记（防「静默漏做」）

| 项 | 归属 | 理由 |
|---|---|---|
| `dnsface.rs:182` 每调用 64KB 零初始化（**QI.md §6 登记的「下一批第一靶点」**） | **Q-I 尾段**（清单明文含「DNS TTL 缓存 + socket/缓冲复用」） | 本批边界：缓冲**复用**属性能批；本批只做 DNS 的期限/并发/容量。**Q-I 尾段须以本条为第一靶点**（Q-I 前段登记「下一批第一靶点」的顺位在 Q-E 不接手后落到尾段） |
| DNS TTL 缓存（`dnsproxy.rs` 无缓存） | Q-I 尾段 | ROADMAP/审计明文 |
| DNS 每查询 64KB 零初始化缓冲 / `Upstreams::list` 整表 clone / files 收发拷贝 | Q-I 尾段 | 分配与缓冲复用面 |
| 「每查询新 socket + 随机事务 ID」 | **不做（非缺陷）** | Go 同形（`server.go:409`）且是防投毒属性 |
| `openat2`/`O_NOFOLLOW` 逐段打开（沙箱 TOCTOU） | **不做**（D5） | macOS 无 `openat2`；威胁模型不含本机 FS 写者；残余登记（§4.3.1） |
| accept Fatal 退出时 unlink socket 文件 | **不做**（D6/E3） | 用户可见后果与 `ECONNREFUSED` 相同；与"他人重绑"竞态；关机路径已有 identity 校验型 unlink |
| 上传每文件硬上限 | **不做**（D1） | Go 无对齐面；水位已覆盖后果；判据线有 100MB 合法流 |
| 沙箱对符号链接的 `entry.name` 语义与 `write` 穿透语义 | **不做**（S3 登记） | 既有行为（非本批引入）；要对齐需 openat/renameat 面，另行排期 |
| UPnP 协议面（IGD:2 / `AddAnyPortMapping` / ST 兜底 / 钉卡降级）、`if_nametoindex` 完整语义 | **Q-J** | 邻批边界 |

**本批残余（新增，如实登记）**：
1. 水位门是**检查点采样**（起始 + 每 8MiB）+ `statvfs` 失败 fail-open ⇒ 极端形态退化为无水位；
   并发两条上传可越保留量 ≤8MiB 窗口。
2. DNS 全死上游下稳态吞吐 ≈25.6 qps（worker=64 vs Go 并发 256）⇒ 接受的差异。
3. **smoltcp rx 溢出 = 静默丢弃且无观测面**（本批靠容量对齐降低触发概率；不新增计数）。
4. **UPnP 静默覆盖型路由器**下 `allow_evict=false` 的"先加"仍会顶掉别人的映射（改进仅对回 718 的
   路由器成立）。
5. SSDP/UPnP 三项加固的全部真机形态**本地不可验证**（本仓既定限制）⇒ 真机验收若发现误杀，按
   §4.3 登记条目回退（尤其：公开地址 LAN 会漏配）。
6. `files` 客户端响应行无上限（D4）；沙箱 TOCTOU 未收口（D5）。
7. **与邻批的同文件摩擦（B1）**：① F4d 改 `dnsface::attach`，与 Q-I 尾段的 `DnsFaces::service`
   64KB 缓冲是**同文件相邻函数**（不撞同一行，但同批交错改需注意 rebase）；② F3a/F3b 改
   `receive_upload`/`read_line`，正是 Q-I 尾段"files 拷贝与分配"要碰的代码——**顺序成本（Q-E 在前），
   非返工**。
