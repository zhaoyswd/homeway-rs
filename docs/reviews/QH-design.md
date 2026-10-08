# Q-H CLI 与 daemon 控制面（config 单表 / `--state` 形态族 / daemon 上限与状态诚实）设计文档

> 批次 = Q-H（`docs/REVIEW-ROADMAP.md`「Q-H CLI 与 daemon 控制面」）；条目真源 =
> `docs/reviews/AUDIT-2026-10-07.md`「Q-H CLI 与 daemon 控制面」节 + Q-A 遗留登记 +
> Q-G 批移交/勾选义务 + GAP-AUDIT P1-4（待裁决项，本棒取证裁定）。
> 复验基线 = HEAD `1d2a59f`（Q-G 收口）；复验方法 = 回源码重定位 + **实跑复现**（debug
> 二进制 `cargo build -p homeway-cli`，全部在 `/tmp` 隔离 state 下跑，未触碰生产
> state/两台出口）；Go 语义对照 = `baseline/homeway`（只读 oracle）。
> 本棒**不写产品代码**；本文档 + 设计门记录是本棒唯一交付物。

---

## 0. 复验（证据先行）

### 0.1 方法与实测环境

- 代码侧：逐条回源码重定位（行号以本文档为准，审计行号会漂）；`#[allow(dead_code)]`
  与注释一并读，区分「注释声称」与「代码实际」。
- Go 侧：`baseline/homeway`（`internal/nodeconfig/config.go`、`internal/daemon/unified.go`、
  `internal/daemon/roleops.go`、`clientcore/hostsession/session.go`、`clientcore/facade/{forward,socksmgr,table}.go`、
  `pkg/netpipe/netpipe.go`、`pkg/probe/probe.go`）逐处对照。
- **实跑复现**（本棒独立跑，非转述审计）：`target/debug/homeway-cli`（HEAD `1d2a59f` 重建），
  每次 `--state <mktemp -d /tmp/...>`；跑完已 `kill` + `rm -rf` 全部临时 state（复核
  `ps` 无残留）。命令与结果见 §0.3。

### 0.2 逐条复验结果表

| # | 审计条目（P 级） | 真伪 | 现行源码位置（复验后） | 复核结论 |
|---|---|---|---|---|
| 1 | config 双表 + `process::exit` 在控制面 handler 线程 ⇒ 非法值配置打崩统一进程（P1 ✅） | **真（实测复现两形态）** | 弱表 = `unified_cli.rs:49-118`（`FileServe.tx_shape: Option<toml::Value>`、`peer_ttl: Option<String>`）；预检 = `unified_cli.rs:370-377`（`toml::from_str::<FileConfig>`——**弱表无法发现类型/值域非法**）；严格表 = `serve_cli.rs:15-84` + `assemble` 的 5 处 `exit(1)`（`:283/:290/:316/:344/:351`）；调用链 = `role_op(R::ServeStart)`（`unified_cli.rs:861`）→ `assemble_serve`（`:370-388`）→ `serve_cli::assemble` ⇒ exit 在 **控制面 dispatcher 线程** | 属实且比审计描述的更宽：**启动期**也走 `load_config`（弱表）+ `assemble_serve`（`:1348`，主线程）——同一份 config 被两套表看，弱表放行、严格表 exit。实测证据见 §0.3-A |
| 2 | `--state` 形态族（P1 ✅）：`serve token list\|revoke --state=DIR` 静默落 CWD；`--state` 缺值被吞/吞下一个 flag；`--state=` 空值落 CWD | **真（实测复现 4 形态）** | `serve_cli.rs:580-589`（`token_state_dir` 只认空格形）、`:685-704`（`token_revoke` 同）、`:534-578`（`cmd_serve_token` 分流只认空格形）；`unified_cli.rs:283-290`；`serve_cli.rs:167-176`（`take_val` 无条件吞下一参）；`relay_cli.rs:69-78`（同形 `take_val`） | 属实。**扩面（审计只点 3 处，实测同族共 8 处）**：`daemon_cli.rs:383-394`（`parse_args`：末尾缺值报错，但**下一个 flag 照吞**：`status --state --json` ⇒ `sock=--json/control.sock`，见 §0.3-B）、`daemon_cli.rs:956-1005/1030-1060`（export/import/reset：`normalize_flag_eq` 只归一等号**非空**形，`--state=` 落位置参数）、`term_cli.rs:531-537/1728-1734`（`take` 无条件吞）、`main.rs:666-679`（`cmd_files` 的 `--state`；portfwd/dnstest 不吃该 flag——设计门 C1 订正）、`carriers_cli.rs:70-84`（缺值报错，但下一个 flag 照吞、`--state=` 空值放行） |
| 3 | `relay stop` 后 `relay start` 不可恢复（P1 ✅） | **已由 Q-G F3 修（本批复核：确认已修，不重做）** | `relay_cli.rs:105-188`（`RelayProc`：每实例 `StopPipe::new()`（`:255-257`）、`shutdown` 幂等（`:137-157`）、`Drop` 共用同一收口）；`unified_cli.rs:708-840`（supervisor 按 `relay_epoch` 属主判定 + `exited_flag` 看护 + 退避重建） | **勾选义务完成**：根因（`STOP_PIPE` 单例 + 已关写端复用）已消失；Q-G 手工回归 `relay start→stop→start ⇒ state=running`、broken 行 0、`lsof` PIPE 6→2 在档（`docs/reviews/QG.md` §6.3/§7）。本批建议**在收敛实测里再跑一遍**（§3.4），不重开同一修 |
| 4 | daemon SOCKS `dead` 永不写入 ⇒ accept 烧尽后 status 谎报 on、`socks on` 幂等短路不复建（P1 🔎） | **真（代码面确证：`dead` 只有初始化与读，无写点）** | `socks_srv.rs:70`（字段）、`:82`（`Mutex::new(None)`）、`:101-103`（`dead_reason()` 读）、**全仓无 `dead` 写点**（`grep` 见 §0.3-C）；失效路径 = `socks_srv.rs:151`（`PollAccept::Failed(err) => return Err(err)`）→ `socksmgr.rs:319-329`（线程只 `srv2.close()` + 告警行，**无人把 srv 摘出 EntryRt**）；消费面已按 dead 写就 = `socksmgr.rs:126-132`（`healthy` 幂等判据）、`:240-247`（status `on = srv.is_some() && dead.is_none()`） | 属实。后果链：`dead` 恒 `None` ⇒ ① `status()` 恒 `on=true`（僵尸监听谎报健康）；② `socks on` 同端口幂等短路 ⇒ 永不复建。**修点极小**（写 `dead` 一处 + 单测） |
| 5 | daemon 连接/线程无上限 + 握手无期限；`conn_threads` 只增；`short_host` 字节切片 panic（P2 ✅） | **真** | 无上限 = `server.rs:107-153`（accept 循环零计数）+ `:158-248`（每连接 **4 线程**：reader/writer/dispatcher/joiner）；握面无期限 = `server.rs:390-445`（reader 只按 500ms 读超时打拍，`handshook` 未置位也无任何 deadline——`:396-400` 只查 `is_closed()`）；`conn_threads` = `server.rs:246` push、`:253-265` 仅在 `shutdown()` drain（正常运行期只增）；`short_host` = `carriers/mod.rs:261-268`（`&host[..8]` 字节切片）；可达路径 = `forward.rs:208-224`（`remove` 的 `NoRule(short_host(host))`）、`socksmgr.rs:193-212`（`off` 的 `NoRule`）——`server.rs:856-875`（`forward.remove`）/`:894-910`（`socks.off`）只查非空，**不做 hex 校验**⇒ 任意 UTF-8 host 直达 `short_host` | 属实。`short_host` panic 只带走 dispatcher 线程（workspace 无 `panic=abort`），后果 = 该连接请求不再被服务（前端见超时/断连）；但**panic 本身即缺陷**（Q-D/Q-G 批的「外部可触发 panic 必收」纪律） |
| 6 | 数字回退静默（P2 ✅，`main.rs:268-283` `--hold`、`:558` `--rounds`、`carriers_cli.rs:584`） | **真（三处全中）** | `main.rs:268-271`（`--hold abc`→`unwrap_or(0)`）、`:272-275`（`--probe`）、`:281-284`（`--recover-delay`）、`:556-558`（`--rounds abc`→`unwrap_or(3)`）；`carriers_cli.rs:584`（`socks on --listen abc`→`unwrap_or(0)`）；对照已 fail-fast 的先例 = `carriers_cli.rs:254-257/487-490`（forward add/delete）、`:849-857`（`--streams`）、`main.rs:689-701`（`--rate-limit`） | 属实；Go 侧 `flag` 包对非法数值 = 报错退出（`flag.Bool/Int` 解析失败即 usage+exit 2）⇒ Rust 静默回退**偏离 Go** |
| 7 | `serve --help` 会真停出口（P2 ✅） | **真（形态订正为 `<动词> --help`；实测见 §0.3-D）** | `daemon_cli.rs:408-409`（`"h" \| "help" => {}` **静默吞掉、动作照跑**）；同族站点 = `serve_cli.rs:134-261`（`--help` 落到 `other => 未知参数` exit 2 而非用法）、`relay_cli.rs:55-93`、`serve_cli.rs:534-578`（`serve token list --help` 落进 rest 后照跑）；对照正确先例 = `carriers_cli.rs:88-92`（用法 + `exit(0)`，CA13 已实采） | 属实且**违反既有判据行 CA13**（`--help` → 用法 + exit 0）。`homeway-cli serve stop --help` 实测真写 `config serve.enabled=false`（§0.3-D） |
| 8 | `--upnp=maybe` 静默 true（P2 ✅） | **真** | `serve_cli.rs:193-199`（`Some(_) => Some(true)`） | 属实；Go `flag.Bool` 非法值 = 报错 ⇒ 偏离 Go |
| 9 | `session_lock` 失败仅告警继续（P2 ✅） | **真** | `main.rs:313-328`（`Err(e) => eprintln!("会话锁失败（继续，不阻塞）：{e}")` + `None`）；模块 = `homeway-core/src/session_lock.rs`（Rust 独有防线：防同 identity 并发会话互踢 keypair） | 属实。**Rust 独有防线**（Go 无对应物）⇒ 「静默降级」正是本批禁的形态：拿不到锁 = 防线消失但用户不知 |
| 10 | `try_dial_control` 每次 dial 污染 stderr（P2 ✅） | **真（唯一调用点 = token reveal 回落族）** | `daemon_cli.rs:300-312`（成功路径 `eprintln!("（已连 control.sock：serverVersion=…）")`）；调用点 = `daemon_cli.rs:318`（`token_reveal_with_fallback`：`serve token` / `relay token`）；该文案全仓无第二消费、**不在判据行**（grep 见 §0.3-E） | 属实：`serve token` 每次都在 stderr 打一行诊断噪声（stdout 仍是纯 token） |
| 11 | `resolve_host` 全长 hex 绕过表内校验（P2 ✅） | **真** | `daemon_cli.rs:425-463`（`:430-432`：`want.len()==64 && all hexdigit ⇒ return want`——**不查表、不 canonical 化**）；Go 对照 = `baseline:internal/daemon/host_cli.go:419-427`（全长 64 hex **必须与表内 ID 精确相等**，否则 `主机 %s 不存在`） | 属实；且与第 12 条（host 串未规范化）**同根**：全长大写 hex 会一路传到承载面 |
| 12 | host 串未规范化（P2 🔎：`server.rs:826-843` vs `mod.rs:361-373` 小写级联） | **真** | 级联键规范化只此一处 = `daemon/mod.rs:361-373`（`remove_host` 解码后重编码小写）；其余承载面入口**原样存/查**：`server.rs:826-854`（forward.add）、`:856-875`（forward.remove）、`:894-910`（socks.on/off）→ `daemon/mod.rs:448-470`+`carriers/*`；`decode_peer_id_pub`（`hosts.rs:570-577` + `hex_decode` `:583-592` 用 `to_digit(16)`——**大小写通吃**）⇒ 大写 hex 能过成员检查却存成大写入表，`remove_host` 级联按小写找不到 ⇒ **级联漏删（转发规则+监听器残留）** | 属实。Go 侧不发病是因为 CLI/App 一律用表内 ID（`resolveHostTarget` 只回表内 ID）⇒ Rust 的洞在「全长 hex 直用」分支（第 11 条） |
| 13 | hosts 会话构造失败无重建路径（P2 🔎） | **真（但与 Go 同形 ⇒ 见 §1 F12 裁定）** | `hosts.rs:421-454`（`start_session_for` 失败仅记行；状态面 `host_state_of` `:458-469` 恒 `failed/session_not_built`；重建入口只有 `add_record` 的 token 刷新（`:250-280`）与进程重启）；**Rust 独有缺口**：`:427-429` token 解码失败 = **静默 return（连日志都没有）** | 真；Go 对照 = `baseline:clientcore/facade/table.go:226-270`（同样只记行、不重建；构造期唯一错误源 = 日志打不开）⇒ **无重建路径 = Go 同形，不做**；但 Rust 的「静默 return」是自家缺口，补一行日志 |
| 14 | 半关闭透传无空闲期限（P2 🔎） | **真（但与 Go 同形 ⇒ 不做）** | `carriers/mod.rs:292-322`（`pipe_half_close`：两向各自读直到 EOF，无 deadline）；Go 对照 = `baseline:pkg/netpipe/netpipe.go:32-60`（`io.Copy` 两向，**同样无期限**，头注明示「长连接语义：只要还有一向活着就不拆连接」） | **剔除「缺陷」定性**：与 Go 逐语义同形（加固超出 Go）⇒ 本批不做，登记（§6） |
| 15 | `remove/off` 落盘失败语义与 `add` 不一致（P2 🔎） | **真（但与 Go 同形 ⇒ 不做）** | Rust：`add` 落盘失败回滚（`forward.rs:187-197`）、`remove` 不回滚（`forward.rs:208-224`，先删表关监听再 save）、`socksmgr.rs:193-212`（off 先 `stop_listener` 再 save）；Go 对照 = `baseline:clientcore/facade/forward.go:131-172`（Add 回滚）/`:175-195`（Remove 不回滚）、`socksmgr.go:220-233`（Off 不回滚） | **剔除**：add/remove 的不对称是 **Go 上游原样**（非 Rust 引入）；修它会与 Go 分叉 ⇒ 不做，登记（§6） |
| 16 | `status --watch` view 不随新增主机扩（P2 🔎） | **部分为真、用户可见后果为零 ⇒ 剔除** | CLI 渲染行**已随 `session.added` 扩** = `daemon_cli.rs:1219-1245`（`KIND_SESSION_ADDED` 建行 / `REMOVED` 删行）；view 串只在订阅时取一次 = `daemon_cli.rs:1146-1164`；`view` 的**服务端消费面不存在**（`bus.rs:298` 存、`server.rs:769-776` 回显，全仓无第二读者；`bus.rs:83-85` 注释自陈「B0-2 后续棒消费」）；Go 对照 = `baseline:internal/daemon/status_watch.go:6-13/71`（view **就是**启动时列表视图，显式文档化「不追溯」） | **剔除**：① Go 同形（启动时集合）；② 渲染面已扩；③ view 当前无消费方 ⇒ 无用户可见后果，改它是「造一条没有用户可见后果的代码路径」 |
| 17 | Q-A 遗留：`nodestate.rs:257` 模板注释键 `burst_kib` vs 实际 `burst_kb`（必须做） | **真** | 模板 = `nodestate.rs:255-283`（注释行**复验后为 `:262`**——审计 `:257` 已漂，设计门 C1）；schema = `server/intercept/mod.rs:408-417`（`TxShapeCfg{rate_mbps, burst_kb}` + `deny_unknown_fields`） | 属实。**后果是双重的**：照模板填 `burst_kib` ⇒ ① 前台 `serve` 拒启（严格表读出 unknown field）；② **统一进程被本批第 1 条打成 DEAD**（弱表放行 → `assemble_serve` → 严格表 exit）——即两条缺陷叠加 |
| 18 | Q-G 移交：`detect_launchd_agent` 与 `--state` 不匹配（新立条） | **真** | `daemon_cli.rs:127-146`（按文件名含 `homeway` 泛匹配，**不看 state**）、消费点 `:259-269`（非默认 state 也先等 KeepAlive 4s；KeepAlive 触发会拉起**生产出口**） | 属实（Q-G 手工回归实测打印+白等 4s）。本批收口「对非默认 state 的 KeepAlive 等待」；plist 级精确匹配（多实例/多 label）留 Q-J（`AUDIT` Q-J 节已立条） |
| 19 | N1/L7：前台 `serve`/`relay` 默认 state 与统一进程不一致 | **真** | `serve_cli.rs:270`（`f.state.unwrap_or_else(|| PathBuf::from("."))`）、`relay_cli.rs:288`（同）、`serve_cli.rs:588/686`（token list/revoke 默认 `.`、`State::open(&state.join("serve"))`）；统一进程 = `unified_cli.rs:39-44`（`~/.config/homeway`）；Go 对照 = `baseline:internal/server/cli.go:30`、`internal/relay/cli.go:33`（**前台单角色默认同为 `$HOME/.config/homeway`**；Go 侧是两份本地实现，`internal/cliutil/cliutil.go:13-22` 的 `DefaultStateDir` 供 daemon 族用） | 属实：默认取值下「全形态共用锁」不成立（前台在 CWD 建一套 state，统一进程在 `~/.config/homeway`）。实测：`tools/*.sh` 全部显式 `--state`（§0.3-F）⇒ 改默认不破脚本 |
| 20 | GAP-AUDIT P1-4：客户端「出口能力」打行（C14 族） | **真：C14 在出货形态下缺失** | C14 判据行 = `docs/INTEROP-CRITERIA.md:60`（出处 `baseline:clientcore/hostsession/session.go:280`）；Rust 全仓 **零** `出口能力` 打行（grep 见 §0.3-G）；输入面**已在**（`probe.rs:38-46` `PingResult{build,flags}`、`:140-193` `decode_response` 解析 flags；位值双方一致：DNS=1<<0/GENERIC=1<<1/OBSERVED=1<<2/SEEN=1<<4）；丢弃点 = `session/mod.rs:1022-1052`（`probe_candidates` 只用 `.endpoints`）、`hosts.rs:651-663`（reach 只用 `rtt`）、`homeway-capi/src/lib.rs:262-267`（只用 `build`/`rtt`） | **裁定 = 做**（取证见 §2.1） |
| 21 | Q-G §6.1-8 代办（Q-G 登记「Q-H 面」）：`serve_cli::wait_stop_pipe` 的 `>= 0` 判据 | **真（低危；按期收回）** | `serve_cli.rs:526-530`：`STOP_PIPE.get_or_init(|| (0, 0))`（**未安装即读 fd 0 = stdin**）+ `libc::read(...) >= 0` 判据（EINTR/-1 与「读到 0 字节 EOF」都算「非停止」）；生产两调用点都先 `install_stop_signals()`（`unified_cli.rs:1225`、`serve_cli.rs:478`）⇒ 不可达面为主 | 属实（可达性低：EINTR 需非停止信号中断，且两调用点都 `let _ =` 忽略返回值）——按 Q-G 移交**顺手收口**（EINTR 重试 + 未安装显式错误），不改语义 |

### 0.3 复验实测记录（命令与输出摘要）

**A. 非法配置打崩统一进程（两形态，独立复现 ✅）**

```
# 形态通用前置：临时 state + [serve] enabled=false（保证启动期成功、由 serve start 触发装配）
$ homeway-cli --state $T &            # 统一进程起（控制面就绪）
# ① peer_ttl="abc"
$ printf '[serve]\nenabled=false\npeer_ttl="abc"\n' > $T/config.toml
$ homeway-cli serve start --state $T
homeway: 等待应答超时：等待应答超时          ← 客户端只见超时（审计原话）
cli-rc=1
DEAD-AFTER(复现)                            ← kill -0 失败：统一进程已死
进程自己的 stdout：$T/config.toml: serve.peer_ttl "abc" 非法（时长串，如 "168h"；须 ≥ 0，0 = 关闭 TTL 回收）
# ② tx_shape.rate_mbps="200"
$ printf '[serve]\nenabled=false\n\n[serve.tx_shape]\nrate_mbps="200"\n' > $T/config.toml
$ homeway-cli serve start --state $T
homeway: 等待应答超时：等待应答超时 ；cli-rc=1 ；DEAD-AFTER(复现)
进程自己的 stdout：invalid type: string "200", expected u64
```

两条形态均**复现**：崩溃点是 `serve_cli::assemble` 的 `exit(1)` 落在控制面 dispatcher 线程；
错误文案只出现在统一进程自己的 stdout（重定向进 spawn.log/launchd 日志），**控制面客户端拿不到**。

**B. `--state` 形态族（4 形态实测 ✅）**

```
$ homeway-cli status --state --json
守护进程：未运行（sock=--json/control.sock 不存在/不可连）…    ← --json 被当成 state 值
$ homeway-cli serve token list --state=/tmp/qh-nonexistent-state
台账为空（出口从未铸出 token）…                              ← 等号形被忽略，落在 CWD（并在 CWD 建了 ./serve）
$ homeway-cli serve token list --state=
台账为空（出口从未铸出 token）…                              ← 空值同样落 CWD
$ homeway-cli relay --state --open
（前台中继真起来了——`--open` 被当 state 目录值吞掉；已 kill）
```

**C. SOCKS `dead` 无写点**：`grep -n "dead" socks_srv.rs` ⇒ 只有 `:70`（字段声明）、`:82`（`Mutex::new(None)`）、`:101-102`（读）+ 测试内同名局部变量；`socksmgr.rs` 只有读面（`:129/240/243/246-247`）。

**D. `--help` 不短路（实测 ✅）**

```
$ printf '[serve]\nenabled=true\n' > $T/config.toml
$ homeway-cli serve stop --help --state $T
serve：进程未运行——期望已写为停用（config serve.enabled=false），下次启动不再装配   ← 真执行、无用法输出
$ grep enabled $T/config.toml ⇒ enabled = false（两处）
# 对照（正确先例）
$ homeway-cli forward list --help
用法：forward list [--host <ref>] [--json]    ；rc=0
```

**E. `已连 control.sock` 唯一出处**：`grep -rn "已连 control.sock" docs/ crates/` ⇒ 只有 `daemon_cli.rs:305`（非判据行）。

**F. 工具脚本均显式 `--state`**（设计门 C4 订正取证写法：初版 grep 模式与脚本实际写法不符、命中为空；
结论由评审独立复核成立）：`tools/local-exit.sh:99/132/166`、`tools/local-rust-exit.sh:68/97`、
`tools/local-relay.sh:54`、`tools/local-rust-relay.sh:57`、`tools/matrix.sh:166/169/191/201/226/228`
全部显式 `--state`（无裸 `serve`/`relay` 调用）。

**G. C14 缺失**：`grep -rn "出口能力" crates/` ⇒ 仅 `homeway-core/src/lib.rs:49` 注释（`BUILD_STR` 用途说明）；无任何 `logf`/`println` 打行。

### 0.4 剔除非缺陷（误报就地剔除记录）

| 条目 | 剔除理由（证据） |
|---|---|
| 半关闭透传无空闲期限（审计 P2 🔎） | Go `pkg/netpipe/netpipe.go:32-60` 同样无期限且头注明示「长连接语义」⇒ 与 Go 同形，加固超出（本批不做） |
| `remove/off` 落盘失败不回滚（审计 P2 🔎） | Go `forward.go:175-195` / `socksmgr.go:220-233` 与 Rust 逐语义同形；`add` 回滚也是 Go 原样 ⇒ 不对称是上游继承，非 Rust 缺陷 |
| hosts 会话构造失败无重建路径（审计 P2 🔎） | Go `facade/table.go:226-270` 同样只记行不重建（构造期唯一错误源 = 日志打不开）⇒ Go 同形；**保留** Rust 独有的「token 解码失败静默 return」补日志（F14） |
| `status --watch` view 不随新增主机扩（审计 P2 🔎） | ① Go `status_watch.go:6-13` 同口径（view = 启动时集合）；② CLI 渲染行已随 `session.added` 扩（`daemon_cli.rs:1222-1245`）；③ `view` 全仓无消费方（`bus.rs:83-85` 自陈留待后续棒）⇒ 无用户可见后果 |
| `serve --help` 会「真停出口」（审计原文形态） | 表述订正为 `<动词> --help`（`serve stop --help` 真停；裸 `serve --help` 落前台 serve 的未知参数 exit 2）——**条目本身有效**，仅形态订正，不剔除 |

### 0.5 复验新增项（审计未覆盖，同族）

- **N1**：`daemon_cli::parse_args` / `term_cli`（2 处）/ `main.rs`（portfwd）/ `carriers_cli` /
  export·import·reset 共 **5 个额外站点**同属 `--state` 形态族（§0.2 第 2 条「扩面」）。
- **N2**：`normalize_flag_eq`（`daemon_cli.rs:936-951`）对 `--state=` **空值**不归一 ⇒
  `homeway export --state=` 会把 `--state=` 当**目标文件名**（静默产出一个名为 `--state=` 的文件）。
- **N3**：弱表**写回**（`write_config_enabled`/`update_config`/`config_serve_ddns`，
  `unified_cli.rs:155-238`）会把「serve 角色读不动的坏 config」原样写回（吊销/期望态翻转
  照做）——Go `nodeconfig.Update` 是严格读（坏 config **拒绝写入**）⇒ 写回面同属「双表」缺陷。
- **N4**：统一进程**启动期**同样经弱表（`unified_cli.rs:1239` `load_config`）⇒ 同一份 config
  在启动期与运行期各有一套判定（本批 F1 一元化）。
- **N5**：`relay_cli.rs:37-44` 的 `FileServeIgnore{rename="*"}` 为**死代码**（`parse_relay_section`
  只喂手切的 `[relay]` 文本，`serve` 节永不到这里）——登记，不做（清理收益为零，动它是纯 churn）。
- **N6**（Q-G §6.1-8 收回）：`wait_stop_pipe`（§0.2 第 21 条）。

---

## 1. 修复清单

> 定性三档：**修缺陷**（与 Go 分叉/自相矛盾/外部可触发 panic）、**加固**（Go 同形但
> Rust 形态下有真实资源/诚实性后果，显式登记）、**对齐**（把偏离 Go 的默认/纪律收回）。
> 每条给：方案 / 文件 / 风险 / 测试 / 判据行影响。

### F1（P1｜修缺陷 + 对齐）config **单表收敛**：严格 schema 单源（**含 Go `validateFile` 全量值域**）+ `assemble → Result` + exit 只在最前台

> 设计门 A1/A2 整改后版本：① 单表必须**真的等价 Go `Load`**（含值域，见下表），
> 否则「Go 同形」是假声明；② `ServeStart` 的落地顺序固定为
> 「**严格读（parse+值域）→ 写回 → 改内存 → 装配**」，任何一步 `Err` 都**不改内存/不改文件**。

**方案（四段）**

1. **单表**：以 `serve_cli::FileConfig/FileServe/FileRelay/FileDdns` 为**唯一 schema**
   （它已是 serve 启动的判据面），补 `Serialize`（写回面）与 `#[serde(default = "default_enabled_true")]`
   （`enabled` 缺省 = true，Go `defaultFile()` 同义）；`TxShapeCfg`（`intercept/mod.rs:408-417`）
   补 `Serialize` + 两字段 `skip_serializing_if = "Option::is_none"`。`unified_cli` 删掉自家
   弱表（`:49-118`），改用 `serve_cli` 的类型（`pub(crate) use`）。
2. **纯函数层（无打印/无 exit）**：
   - `serve_cli::load_config_strict(state_dir) -> Result<FileConfig, String>`：缺失 = 默认；
     存在即「TOML 严格解析（deny_unknown）+ **值域校验（下表全量）**」，错误文案带 `config.toml`
     绝对路径 + 字段 + 值域（Go `Error.String` 同形）；**写回面与启动期共用本函数**；
   - `serve_cli::serve_config_of(&FileConfig, state_dir) -> Result<ServeConfig, String>`：
     **映射**（不含值域校验——值域已在校验层；保留 `parse_bind_iface` 等纯映射）；
   - `relay_cli` 侧同构：`relay_config_of(&FileConfig) -> Result<RelayAssemble 参数, String>`
     （`listen` 走 `parse_listen` + 端口 1–65535；`advertise` 透传）——**启动期一并跑**
     （Go `validateFile` 是整文件校验，不分节）；
   - `serve_cli::assemble_result(args) -> Result<ServeConfig, CliErr>`（flag 解析失败 = `CliErr::Usage`
     ⇒ 前台 exit 2；config 层失败 = `CliErr::Config` ⇒ 前台 exit 1）；
   - `serve_cli::assemble(args) -> ServeConfig` 退化为**薄壳**：`Err(Usage)` → eprintln + exit 2；
     `Err(Config)` → eprintln + exit 1。**口径订正（设计门 C9）**：`process::exit` 允许出现在
     **最前台**（flag parser 的 `exit(2)` 与前台壳的 `exit(1)`），**控制面路径与角色装配路径零 exit**
     （`cargo grep` 作为代码门检查项）。

   **值域校验表（= Go `baseline:internal/nodeconfig/config.go:254-292` `validateFile` 全量，逐条对齐）**：

   | 字段 | Go 值域 | Rust 现状（`serve_cli`） | 本批 |
   |---|---|---|---|
   | `serve.listen` | 1–65535（**0 拒**） | `Option<u16>` 收 0 ⇒ engine 绑随机端口（`server/bind.rs`） | **补**（文案：`serve.listen {v} 非法（合法值域 1–65535）`） |
   | `serve.dns_port` | 0–65535 | `Option<u16>` 天然等价 | 已等价（登记） |
   | `serve.bind_interface` | 空/auto/none/off/no、IP 字面量、其余按网卡名**但含 `:/ \t` 拒** | `parse_bind_iface` 任意串都收（`Explicit`） | **补**（`{v:?} 非法（auto / none / 网卡名 / IP 字面量）`） |
   | `serve.public_endpoint` | 空 = 关；非空 = 逗号分隔、逐项 `ip:port` | 无校验（直传 engine） | **补** |
   | `serve.peer_ttl` | 时长串且 ≥ 0 | 已有（`parse_go_duration`，`:311-318`） | 保持（校验点移到 load 层） |
   | `[[serve.ddns]].domain` | 非空、不含 `:/ ` | 已有（`:339-353`） | 保持（同上） |
   | `serve.relay` | 空 = 关；`rl1…` 必须可解码；否则 `ip:port` | **只查非空** | **补**（复用 CLI 侧 `validate_relay_arg` 同口径） |
   | `relay.listen` | `[host:]port`，端口 1–65535 | 只在装配期 `parse_listen`（failed + 退避） | **补**（启动期拒启，Go 同形） |
   | `[serve.tx_shape]` | **Go 无此键**（Rust D-3 扩展） | 类型由 serde 保证 | 保持 + §6-9 登记跨语言差异 |

3. **统一进程接面**：
   - 启动期（`run_unified_state` ③）：`load_config_strict`（整文件，含 relay 段）⇒ `Err` =
     **可行动文案 + exit(1)（Go 同形「拒启」）**；
   - 角色装配 `assemble_serve`（`:370-388`）：`load_config_strict` + `serve_config_of` → `Err`
     **返回 Err**（绝不 exit）；**op 触发路径与 supervisor 重建路径的语义分叉**（Go 同形）：
     - **`Serve`/`Relay` × `Start/Stop/Restart` 六个 op**（`unified_cli.rs:849-980/1015-1140`；
       `Stop` 现状同病——先翻内存 `enabled=false`（`:898`）再写回）：
       **顺序 = 严格读 → 写回（`write_config_enabled`）→ 改内存（`inner.cfg`）→（仅 Start/Restart）装配**。
       任一步 `Err` ⇒ 立即返回 `BackendErr`（**内存未改、文件未改**：写回走严格表 ⇒ 坏 config
       拒写）⇒ 状态面如实（`enabled` = 文件真值、`state` 不前进），客户端拿到可行动错误。
       **修掉现状分叉**：现行序是「先改内存 `enabled=true`（`:853`）→ 写回（`:854`）」，写回失败
       会留下「内存 true / 文件 false」的双面不一致（设计门 A2）；
     - supervisor 重建（`assemble_serve` 在 `bootstrap_serve`/`supervise_serve` 里被调）：
       `Err` ⇒ `serve_reason` + `failed` + 退避重建（Go `failedRole` 同形，机制已在
       `unified_cli.rs:602-639/644-695`）；
   - 角色 op 的重读（`load_config_quiet`，`:138-153`）改成严格读：读不动 config 时返回
     `BackendErr`（Go `lifecycleStart → Update` 失败同形）；
   - **写回**（`write_config_enabled`/`update_config`/`config_serve_ddns`）改走 `load_config_strict`：
     坏 config **拒绝写入**（Go `nodeconfig.Update` 同形；`RolesInner.cfg` 类型随之收敛）。

**文件**：`crates/homeway-cli/src/serve_cli.rs`、`unified_cli.rs`、`relay_cli.rs`（`relay_config_of`）、
`daemon_cli.rs`（离线状态的「config 读取失败」分支，见下）、`crates/homeway-core/src/server/intercept/mod.rs`（仅 `derive`/属性）。

**风险**：① 写回变严格 ⇒ 手编坏 config 时 `serve stop` 也会被拒（**有意**：Go 同形；客户端拿到
可行动错误）；② 启动期坏 config 从「装配后崩」变成「拒启」——同样退出，但**文案可行动 +
发生在任何角色装配前**（Go 同形）；③ 新增值域校验会拒掉现存「能跑但违规」的手编 config
（`serve.listen=0` 绑随机端口、`serve.relay` 垃圾串晚失败、`bind_interface` 带 `:`）——**这是修复目标**
（Go 一直拒），须在 §5.1 登记；④ `Serialize` 往返丢手编注释（既有已登记代价）。

**测试**：见 §3（E2E 形态 ①②③④）；单测：`load_config_strict` 逐字段坏值（上表每行 1 例）+ 好值往返
（写回后严格表仍可解析）+ `assemble_result` 不 exit（`Err` 分类）+ op 顺序单测（写回失败 ⇒
`inner.cfg` 与文件均未变、状态面 `enabled=false`）。

**判据行影响**：**登记 2 条**——① 「非法配置处理路径」（控制面 handler 线程 `exit(1)` →
① 启动期拒启；② op 触发 = 拒绝 + 零副作用；③ 装配期 = `failed` + 退避重建）；② 「config 值域
校验补齐」（上表 5 项新拒）。另：离线 `serve status` 坏 config 文案从静默默认值改为 Go 同形
`config 读取失败：<err>`（`baseline:servegroup_cli.go:411-427`）——作为 CA13 的形态扩展登记。

### F2（P1｜修缺陷）取值 flag 纪律：`--state` 全形态 fail-fast + 取值器**全站点统一**

**方案**：新增 `crates/homeway-cli/src/cli_flags.rs`（`pub(crate)`）：

```rust
/// 取值器（全站点唯一实现）。`allow_dash=false`（默认）时：下一个 token 以 '-' 开头 ⇒ Missing。
pub(crate) enum Val<T> { Ok(T), Missing /*含末尾缺值*/, Empty }
pub(crate) fn take_value(name: &str, inline: Option<&str>, next: Option<&str>, allow_dash: bool) -> Val<String>;
pub(crate) fn take_state(name: &str, inline: Option<&str>, next: Option<&str>) -> Val<PathBuf>; // allow_dash=false
```

- **语义拍板**：`--state --verbose` ⇒ `--state 缺值（下一个是 --verbose）` + exit 2（**不吞**）；
  `--state=` ⇒ `--state 空值` + exit 2；缺值 ⇒ 同一报错。**这是对 Go `flag` 吞值语义的有意偏离**
  （审计要求 fail-fast；理由是 state 指错目录的静默后果 = 打到生产/别的 state，代价不对称）。
- **站点（`--state`，8 个 parser + token 支线）**：`unified_cli.rs:282-290`、
  `serve_cli.rs:161-176/534-589/685-704`、`relay_cli.rs:65-78`、`daemon_cli.rs:378-394`
  （+`:936-951` `normalize_flag_eq` 补空值分支）、`term_cli.rs:531-537/1728-1734`、
  `main.rs:666-679`（`cmd_files` 的 `--state`；portfwd/dnstest 不吃 `--state`——设计门 C1 订正）、
  `carriers_cli.rs:64-84`。
- **同族泛化（设计门 B3 采纳）**：其余**取值 flag**（`--listen/--stun/--stun6/--bind-interface/
  --files-root/--public-endpoint/--relay/--advertise/--ddns/--identity-dir/--endpoint-cache-dir/
  --token/--inject/--host/--timeout/--name/--reason` …）一律走同一 `take_value`（`allow_dash=false`）
  ——**取证**：这些值（路径/地址/域名/标识）以 `-` 开头必然非法，被吞掉表现为「静默错配 + 丢 flag」
  （`serve --stun --verbose` ⇒ stun="--verbose"）。**唯一 carve-out** = `--recover-cause`（自由文本，
  `main.rs:285-288`，`allow_dash=true`）；carve-out 清单写进 §6。
- **term 两站点是放宽（设计门 B12 采纳）**：`term_cli.rs:557-573/1748` 现在对 `--state=DIR` 是
  **硬报错**（未知 flag），接入后变成接受——属放宽，计入 §5.1 影响面。

**风险**：脚本里若有人写 `--state -x`（或 `--stun -y`）会从「静默吞」变「报错」——属期望（可行动）。
**测试**：表驱动单测（`take_value`/`take_state` × 5 形态：空格形/等号形/缺值/吞 flag/空值），
每个站点各 1 例接入断言（含 `serve token list --state=DIR` 正例、term 放宽正例、`--recover-cause -x` carve-out）。
**判据行影响**：登记 1 条（CLI 取值 flag 纪律：从静默取值/吞 flag → exit 2 + 可行动文案；无编号判据行）。

### F3（P1｜勾选复核）`relay stop→start 可恢复`：**已修（Q-G F3），本批不重做**

**方案**：代码面复核（§0.2 第 3 条）+ 收敛实测里跑一条 `relay start → stop → start`
（状态面 `state=running` + broken 行 0），把结果写进 `QH.md`，并在 `AUDIT` 该条上勾选
「Q-G 修根因 / Q-H 复核通过」。**不动代码。**

### F4（P1｜修缺陷）daemon SOCKS `dead` 落账 → 状态诚实 + `socks on` 可重建

**方案**：`socks_srv.rs`：`PollAccept::Failed(err)` 分支先 `self.mark_dead(&err)`（新
`fn mark_dead(&self, reason: &str)`：`*dead.lock() = Some(reason)`）再 `return Err(err)`；
`close()` **不清** `dead`（off 后 `EntryRt` 换新对象自然归零，`socksmgr.rs:199` 注释同义）。
`socksmgr` 无需改（消费面已按 dead 写就）。线程侧告警行文案不变。

**风险**：`dead` 一旦置位，status 从 `on=true` 变 `on=false`（**这是修复目标**）；正常 `off`
路径不置 dead（只走 `close()`）⇒ 无回归。
**测试**（设计门 B7 整改：**不用** `libc::close` 硬关真 fd——会与 `PollListener::close()`/Drop
构成二次 close 同号风险）：给 `PollListener` 加 `#[cfg(test)] pub(super) fn inject_accept_failure(&mut self, msg: &str)`
（下一次 `accept()` 直返 `PollAccept::Failed(msg)`），据此写
`socks_dead_is_recorded_and_rebuildable` ⇒ 断言 `dead_reason().is_some()`、`status()[i].on == false`
且 `err` 非空、随后 `on()` **重建**成功（`dead_reason()` 归 None、`on == true`、端口不变）。
（`Failed` 的生产触发面由 `PollListener` 既有分支覆盖，本测试覆盖的是 serve→mark_dead→消费面接线。）
**判据行影响**：登记 1 条（SOCKS 状态面：accept 烧尽后 `on` 由「谎报 true」→「false + 可行动 err」；
`CA4/CA6` 正常路径行文不变）。

### F5（P2｜加固）daemon 控制面：连接上限 + 握手期限 + JoinHandle 回收

**方案**（Rust 形态下的资源收口；Go = goroutine 天然轻，无对应约束——**显式登记为加固**）：
1. `ServerConfig` 增 `max_conns: usize`（`DEFAULT_MAX_CONTROL_CONNS = 64`——**改名避撞**
   `socks_srv::DEFAULT_MAX_CONNS = 256`，设计门 C3）与 `handshake_deadline: Duration`
   （`DEFAULT_HANDSHAKE_DEADLINE = 10s`）；构造面 4 处同批改（`unified_cli.rs:1422`、
   `daemon/tests.rs:476/712/898`），测试注入走显式字段（结构体字面量，不做 builder——可见性最小）；
2. accept 处（`server.rs:113-125`）超限：**接受后立即关闭** + 记行
   `control: 连接拒绝（并发上限 64）`（不发明新错误码；握手前不发帧）；
3. reader（`server.rs:390-405` 的空转拍）加：`!handshook && now - accepted_at > deadline`
   ⇒ 关闭连接 + 记行 `control: 连接握手超时（10s 未 hello）——断开`；
4. `conn_threads` 在每次 accept 前 `retain(|h| !h.is_finished())`（VD：`is_finished` 不阻塞）。

**风险**：cap 值 64 需高于真实前端数（生产 = 1–3 个前端 + 订阅者 + 瞬时 CLI）；超限是
**关连接**而非拒服务已有连接（既有连接不受影响）。deadline 只打未握手连接（合法前端
hello 在毫秒级）。**已知观测缺口（设计门 C7，登记不做文案改动）**：超限被拒时 CLI 首拨失败
→ `probe_sock` 判 Alive → 回「连不上 …（协议不匹配或守护进程异常；核对版本与 --state 指向）」
（`daemon_cli.rs:228-235`），与真实原因（并发上限）不符——服务端新日志行已可归因，
CLI 文案不动（避免动脚本可匹配文案）。
**测试**：`ServerConfig{max_conns:2}` 注入：第 3 条连接**迅速 EOF**、前两条正常握手；
`handshake_deadline:50ms` 注入：不 hello 的连接被断且日志行出现；joiner 清理：连接起落
后 `conn_threads` 长度回落（`#[cfg(test)] pub(crate) fn conn_threads_len()`，设计门 C8）。
**判据行影响**：新增运维日志行（非判据行）；登记 1 条（daemon 资源上限/期限 = Rust 加固，
接口零变更）。

### F6（P2｜修缺陷）`short_host` 字符安全截断（消 dispatcher panic 面）

**方案**：`carriers/mod.rs:261-268` 改 `host.chars().take(8).collect::<String>() + "…"`
（`len() > 8` 判据同步改 `chars().count() > 8`——保持「短串原样」语义）。
**口径注记（设计门 C5）**：Go `shortHost`（`baseline:clientcore/facade/forward.go:410-415`）是
**字节**截断——对**非法输入**（非 ASCII）会产出非法 UTF-8 前缀；Rust 改字符截断后对非法输入
返回整串。**「与 Go 同串」只对合法 hex（ASCII）成立**，注释里须写明这一限定（非法输入面
Rust 取「不 panic + 整串」的自定义语义）。
**风险**：对合法 hex 输出**逐字节不变**；只影响非法输入（原来 panic）。
**测试**：单测（ASCII ≤8/>8 两档 + 中文 18 字节 / emoji / 恰在字节 8 边界断裂的 3 字节字符）。
**判据行影响**：无（合法输入输出不变）。

### F7（P2｜修缺陷 + 对齐）数字/布尔 flag 静默回退/静默 true ⇒ fail-fast

**方案（F7a 数值）**：`main.rs:268-284`（`--hold`/`--probe`/**`--recover-from`（设计门 B2 补，
`:277-280` 现为 `.parse().ok() → None` 静默关掉注入）**/`--recover-delay`）、`:556-558`（`--rounds`）、
`carriers_cli.rs:584`（`socks on --listen`）改「缺值/非法 = 可行动文案 + exit 2」。
（对照先例：`--streams`/`--rate-limit`/forward `--listen`/`--dial` 已 fail-fast。）

**方案（F7b 布尔族，设计门 B1 采纳——审计 `--upnp` 条的直接同族，且后果更重）**：
新增 `cli_flags::take_bool(name, inline, bare_default) -> Result<bool, String>`，按 Go
`strconv.ParseBool` 集收值（`1/t/T/true/TRUE/True/0/f/F/false/FALSE/False`；裸 flag = true；
其余 = exit 2）。**全站点**：`serve_cli.rs:193-199`（`--upnp`，`Some(_) => true` ⇒ 收口）、
`:252`（`--verbose`）、`daemon_cli.rs:395-399`（`--json/--yes/--stdin/--force/--no-spawn`——
**`--yes=false`/`--force=false` 现在被忽略、越过确认与探测守卫**）、`carriers_cli.rs:85-87`、
`relay_cli.rs:82-84`（`--open/--no-hints`）、`unified_cli.rs:291`（`--verbose`）、
`main.rs`（`--dead-direct/--no-session-lock/--status-json` 等）。
**注**：实现棒先以 `grep -n '=> true' crates/homeway-cli/src/*.rs` 把布尔位站点清单化，
逐站点确认是否收值形态（`--watch` 等「只属某命令」的开关同批处理）。

**风险**：脚本以 `--json=false` 等形态写的「碰巧可用」用法会开始真的生效 false（**修复目标**）；
非法值从「静默 true」变报错。
**测试**：表驱动单测（每 flag：缺值/非法/`=false`/`=0`/`=true`/裸 flag 六档）。
**判据行影响**：登记 1 条（CLI flag 非法值/布尔显式赋值：静默回退或静默 true → exit 2 / 真值生效）。

### F8（P2｜对齐）`--help` 短路：命令组与裸名词收口到 CA13

**方案（设计门 B10 订正覆盖面）**：
- `<动词> --help`（经 `daemon_cli::parse_args`）：`"h" | "help"` 改「打 usage（传入的 usage 串 +
  子命令清单）+ exit 0」⇒ 覆盖 `host add/list/status/delete`、`status`、`serve <动词>`、
  `relay <动词>`、`serve ddns/relay <动词>`；**`-h` 与 `--help` 都收**（现状 `"h" | "help"` 已收）；
- **裸名词**（不经 `parse_args`）：`cmd_host`（`daemon_cli.rs:467-482`，现 `--help` 被当未知动词
  exit 2）、`cmd_serve_group`/`cmd_relay_group`、`cmd_status`、`serve token` 族
  （`serve_cli.rs:534-578`）各加 `--help/-h` 分支 = 用法 + exit 0；
- `serve_cli::parse_serve_flags`/`relay_cli::parse_flags`（前台单角色）同样加 `help` 分支
  （现状 = `未知参数` exit 2）；文案对齐 `carriers_cli.rs:88-92` 先例。
**风险**：`serve <动词> --help` 从「真执行」变「只打用法」——正是修复目标。
**测试**：CLI 级（`serve stop --help` 不写 config、rc=0、输出含用法）+ 单测（裸名词 `host --help`
rc=0；`host add --help` rc=0）。
**判据行影响**：**修复到既有行 CA13**（无需变更登记；在 `QH.md`/登记行注明覆盖面 = 动词形 + 裸名词，
含此前硬报错的 `serve`/`relay` 前台形态）。

### F9（P2｜修缺陷）`session_lock` **IO 失败** ⇒ fail-fast（消静默降级）

**方案（设计门 B11 采纳：分类写清）**：`main.rs:313-328` 只对 `LockError::Io`（`session_lock.rs:39-56`：
建目录/开锁文件/flock 失败）改「打印可行动错误（含 `path` + 建议 `--identity-dir` 指向可写目录 /
`--no-session-lock` 逃生口）+ exit(1)」；`LockError::Held` 现状已是 exit(1)（文案不动）。
**环境边界登记**：只读 HOME/CWD 或特殊 FS 环境从「降级继续（防线消失）」变「明确拒跑」——
计入 §5.1/§6。
**测试**：`session_lock` 单测（不可写目录 ⇒ `LockIo` 分类 + path 字段）；CLI 层以只读 identity
目录注入（`--identity-dir`）断言 exit ≠ 0 且 stderr 含路径——若进程内构造成本高，降级为
「分类单测 + 调用点代码面复核」并在 `QH.md` 登记测试有效性降级。
**判据行影响**：登记 1 条（行为变更：只读环境从降级继续 → 拒跑；`Held` 文案不变）。

### F10（P2｜对齐）`resolve_host` 全长 hex **必须在表内**并 canonical 化

**方案**：`daemon_cli.rs:425-463`：64-hex 分支改为「`decode_peer_id_pub` → 小写 canonical →
与表内 id 比对」；命中返回 **表内 id**，未命中回「没有匹配 … 的主机（host list 看全表）」
（Go `主机 %s 不存在` 的既有 Rust 文案同族）。
**风险**：原先「全长 hex 直用」的脚本调用（打错 id）会从「daemon 报 no_host」变「CLI 就地报
错」——更早更可行动；合法调用输出不变。
**测试**：单测（表内小写/表内大写/表外 hex/长度 64 非 hex 四档）→ 断言 canonical 输出与错误文案。
**判据行影响**：登记 1 条（CA12 同族：CLI 侧 resolve 就地报错）。

### F11（P2｜修缺陷）承载面 host 串 canonical 化（级联一致）

**方案**：`Carriers` 入口（`add_forward`/`remove_forward`/`socks_on`/`socks_off`/`speedtest_*`）
把入参 host 经 `decode_peer_id_pub` 归一为小写 hex 后再委托（解码失败：add/on 保持 `NoHost`
语义，remove/off 保持 `NoRule` 语义——**只规范化，不改错误分类**）。CLI 侧 F10 已保证
正常路径 canonical，本项是**控制面直连客户端**的防线（App/矩阵注入）。
**风险**：大写请求的查/删从「查不到」变「查到」——这是修复目标；不改任何成功文案。
**测试**：单测（大写 hex add → list/off/delete 命中；`remove_host` 级联删除大写历史条目）。
**判据行影响**：登记 1 条（行为变更：大小写等价；CA3/CA6 文案不变）。

### F12（P2｜加固·不做重建环）hosts 会话构造失败：补日志，不加重建

**方案**：`hosts.rs:427-429`（token 解码失败静默 return）补一行 `(self.logf)(…)`；**不加**
重建环（理由见 §0.4 剔除表：Go 同形 + 构造期错误源非瞬态；真要重建走 token 刷新/重启）。
**测试**：单测（坏 token 记录 → 断言日志行 + 状态面 `failed/session_not_built`）。
**判据行影响**：无。

### F13（P2｜修缺陷）Q-A 遗留：默认 config 模板注释键 `burst_kib` → `burst_kb`

**方案**：`nodestate.rs:262`（注释行；设计门 C1：审计行号 `:257` 已漂）注释改 `rate_mbps/burst_kb`；
顺带核对模板注释键表与 schema 的其他键——**补列 `public_endpoint`**（正文 `:276` 有、注释表漏），
`dns_port`/`files_root` 已在表内。
**测试（设计门 C6 采纳）**：① `toml::from_str::<serve_cli::FileConfig>(DEFAULT_CONFIG_TOML)` 必须 Ok
**且**过 `load_config_strict` 的值域层（模板要同时过两层才算「可被严格表接受」——用临时目录把模板
落盘后调 `load_config_strict`）；② 模板文本包含 `burst_kb` 且**不含** `burst_kib`；③ 注释键表逐键
比对 schema 字段名（静态清单断言，防未来漂移）。
**风险**：零（注释）。
**判据行影响**：无（模板文案，非判据行）。

### F14（P2｜对齐）Q-G 移交：launchd 探测只在**默认 state** 时参与等待

**方案**：`daemon_cli.rs:259-269` 的 launchd 分支加前置条件 `state_dir == default_state_dir()`
（判定抽成纯函数 `launchd_relaunch_relevant(state: &Path) -> bool`）；非默认 state 直接走
「自行拉起」并打一行明确提示（`（state=<dir> 非默认 state——不等 launchd KeepAlive，直接拉起）`）。
plist 级精确匹配（多实例/多 label/自定义 state 的 launchd 形态）留 Q-J（`AUDIT` Q-J 节已立条）。
**风险**：把 launchd 托管在**自定义 state** 的部署形态会退化为自拉起（当前生产两形态 =
Mac launchd 零参〔默认 state〕/ 阿里云 nohup+`--state`，均不受影响）；登记该边界。
**测试**：单测纯函数二档（默认 state true / 自定义 state false）。
**判据行影响**：登记 1 条（CA11 形态扩展：非默认 state 的提示行）。

### F15（P2｜对齐）N1/L7：前台 serve/relay 默认 state 对齐统一进程

**方案**：`serve_cli.rs:270`、`relay_cli.rs:288`、`serve_cli.rs:588/686` 的默认值统一
`crate::unified_cli::default_state_dir()`（Go 前台单角色默认同为 `$HOME/.config/homeway`——`internal/server/cli.go:30`/`relay/cli.go:33` 各自的 `defaultStateDir()`，**非** `cliutil` 单源；设计门 C-注记订正口径）。
**风险**：裸 `homeway-cli serve`（无 `--state`）从「CWD」变「`~/.config/homeway`」——**正是修复
目标**；本仓脚本与 matrix 全部显式 `--state`（§0.3-F）⇒ 无工具面回归。
**测试**：单测（抽 `fn default_state_or(f: &ServeFlags) -> PathBuf` 断言默认值来源；
`relay` 同款）。
**判据行影响**：登记 1 条（行为变更：默认 state 对齐；`CA13`/`DC*` 文案不变）。

### F16（P2｜修缺陷）`wait_stop_pipe` 收口（Q-G §6.1-8 收回；设计门 B6 整改）

**方案（拆纯逻辑 + 薄壳，去掉无意义 `bool`）**：
```rust
pub(crate) enum StopWait { Signaled, PipeErr(String) }   // 返回值自此有语义
/// 纯逻辑（可单测）：fd < 0/未安装 ⇒ Err；read 循环：n>0 ⇒ Signaled；n==0(EOF) ⇒ Signaled；
/// EINTR ⇒ 重试；其它 errno ⇒ Err(可读文案)。
fn read_stop_signal(fd: i32) -> Result<StopWait, String>;
pub fn wait_stop_pipe() -> StopWait;   // 壳：未安装 ⇒ 打印可行动错误 + 收工语义
```
- **未安装**（`STOP_PIPE.get()` 为 None 或 fd < 0）**绝不读 fd 0**：打一行可行动错误并按
  `PipeErr` 返回（调用方 = 收到停止处理，走收工——不悬挂）；
- 两调用点（`serve_cli.rs:480`、`unified_cli.rs:1457`）从 `let _ =` 改为 `match`：`PipeErr(e)` ⇒
  `eprintln!` 记行（**行为 = 仍按收工走**，与现状一致，只多一行可归因）。

**风险**：零（EINTR 重试与「未安装不读 stdin」都是纯改善；调用点行为保持收工）。
**测试**：单测 `read_stop_signal`：写 1 字节 ⇒ `Signaled`；关写端（EOF）⇒ `Signaled`；
`EINTR`（`raise(SIGUSR1)` + 空 handler 构造）⇒ 重试后 `Signaled`；未安装路径 ⇒ `StopWait::PipeErr`
（**壳不 exit，可进程内断言**）。
**判据行影响**：无。

### F17（P2｜修缺陷）GAP-AUDIT P1-4 / C14：客户端「出口能力」打行**实装**（裁定 = 做）

**方案**：`session/mod.rs` 启动序列 **C13 之后**（Go 位置 = `hostsession/session.go:252-280`），
起一条一次性探测线程（`std::thread::Builder` + 失败记行——Q-F F7 纪律）：
- **目标（设计门 A3 订正）** = **首个已解析的 token 端点候选**：Rust 对应物是
  `session/mod.rs:274` 的局部 `candidates`（= `domain_eps::split_and_resolve(&ep_refs, …).candidates`，
  静态 IP + 域名首解**合并**列表，Go `cands := resolveCandidates(tok.Endpoints, …)`（`session.go:179`）
  的同义项），取 `candidates.first()`——**不是** `static_cands[0]`（只装 IP 字面量，
  `session/mod.rs:350`；域名-only token 下为空 ⇒ `[0]` 会 panic），**也不是**
  `merged_candidates()[0]`（含学习缓存条目，Go 侧 `cands` 不含缓存）；
  非空性由入口守卫（`session/mod.rs:278-280` `NoCandidates` 早退）保证，实现仍用 `if let Some(_)`
  不索引；若为空则不打行（记 debug 行）；
  **实现建议**：在 `:280` 早退之后立刻 `let probe_target = candidates.first().copied();`（借用早收，
  避免在 C14 站点再借 `candidates`）；
- 探测 = `probe::ping_ex(addr, 16, 5s)`（Go `Ping` = `PingEx(pad=16)` + ctx 5s，**实参同值**）；
- 成功 ⇒ 一行（**逐字**）：
  `出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v`
  （`build` 空 ⇒ `（未标注）`；位映射 = `UDPCAP_DNS(1<<0)`/`UDPCAP_GENERIC(1<<1)`/`OBSERVED(1<<2)`/`SEEN(1<<4)`
  ——与 Go 常量逐位一致（`session.go:309-314`）；`saw` 三态文案同 Go（未实测/有回包/无回包——…）；
  `rtt` 用「Round 到 ms」的 Go Duration 形态（既有 `fmt_duration_go_ms` 同源）；
- 失败 ⇒ `出口能力：参照点探测失败（%v）—— 本机网络到出口的 UDP 不通或出口未应答`
  （`%v` = Rust io 错误文案；**登记为平台文案差异**，与 E20a 的「Rust 按断点分两类」同类纪律）。
- 行文本组装抽**纯函数** `format_outbound_caps_line(&PingResult) -> String` 以便单测。

**风险**：① 每次会话多一发明文探测包（Go 同款、5s 预算、旁路纪律——不参与健康判定）；
② 探测线程 spawn 失败 ⇒ 记行（不静默）；③ 会话收工时探测可能在飞（5s 上限，线程自灭——
与 Go goroutine 同形态，登记）。
**测试**：① 纯函数单测（4 组位组合 + 空 build + rtt 0/1.5ms 的格式）；② 本地 UDP 桩
（`127.0.0.1:0` + 手写 HWR 应答）端到端断言整行同串；③ 失败形态（无应答、50ms 预算）断言失败行文案；
④ **目标选取**（设计门 A3 要求）：域名-only token（`static_cands` 为空）⇒ 打行且目标 = 域名解析地址；
首项为域名的混合 token ⇒ 目标 = 首项解析地址（不 panic、不打到其它端点）。
**判据行影响**：登记 **C14 首次实装**（成功/失败两形态 + `%v` 平台文案差异说明）。

### F18（并入项，不单列）

`status --watch`（§0.4 剔除项 D8）/hosts 重建（F12）/半关闭（§0.4 剔除）均无独立修项；
**唯一附带补丁** = 离线 `serve status` 坏 config 文案（并入 F1 第 3 段）。

---

## 2. 「二选一」类决策的取证与裁定（本棒自裁，逐条给证据）

| # | 决策 | 裁定 | 证据 |
|---|---|---|---|
| D1 | 统一进程**启动期**非法 config：拒启（exit 1）vs 降级为 failed+退避重试 | **拒启（Go 同形）** | `baseline:internal/daemon/unified.go:143-150`（`nodeconfig.Load` err ⇒ `return nil, err`；头注「非法 = 可行动错误拒启，不静默按默认——D2」）；Go 的 `Load` = **整文件值域校验**（`config.go:254-292`）⇒ F1 的严格读必须同量（含 `relay.listen`）；拒启发生在任何角色/control 之前 ⇒ 不构成本批要禁的「打崩运行中的统一进程」 |
| D2 | 动态装配非法 config：`Err` 传播 vs exit | **Err 传播**；且 op 路径「**不改内存/不改文件**」（设计门 A2） | `baseline:internal/daemon/roleops.go:63-95`（`makeServeFactory` 失败 ⇒ `failedRole` ⇒ supervisor 退避；**不 exit**）+ `:154-200`（`lifecycleStart/Stop` 第一步就是 `nodeconfig.Update`，**Update 失败 ⇒ 直接返回错误、角色不动、期望态不写**）⇒ op 触发的非法 config 在 Go 里也是「拒绝 + 无副作用」，只有**装配期**失败才进 failedRole/退避 |
| D3 | `--state` 下一个 token 以 `-` 开头：吞（Go `flag` 语义）vs 报错 | **报错（fail-fast）** | 审计要求「全量 fail-fast」；Go `flag` 包确会吞值，但吞掉的后果 = 静默指向错 state（`--state --verbose` ⇒ 目录名 `--verbose`），代价不对称 |
| D4 | config **写回**用严格表（坏 config 拒写）vs 弱表 | **严格表** | `baseline:internal/nodeconfig/config.go:26-29`（Update 读改写遇损坏 config = 拒绝写入 + 同款错误，MUST NOT 以默认覆盖）；Rust 现弱表会照写回 ⇒ 与 Go 分叉（N3） |
| D5 | `remove/off` 落盘失败是否回滚 | **不回滚（跟 Go）** | `baseline:clientcore/facade/forward.go:175-195`、`socksmgr.go:220-233`（Go 同样先改内存再 save）⇒ 记入剔除表（§0.4） |
| D6 | hosts 会话构造失败加重建环 | **不加（Go 同形）** | `baseline:clientcore/facade/table.go:263-270`（只记行）；构造期错误源 = 日志/IO 类非瞬态 |
| D7 | 半关闭加空闲期限 | **不加（Go 同形）** | `baseline:pkg/netpipe/netpipe.go:14-20`（头注「长连接语义：只要还有一向活着就不拆连接」） |
| D8 | `status --watch` view 随新增主机扩 | **不做（Go 同形 + 无消费方 + 渲染行已扩）** | §0.2 第 16 条证据 |
| D9 | daemon 连接上限值 | **64**（`DEFAULT_MAX_CONTROL_CONNS`） | 生产前端数 1–3（App/CLI/订阅）+ 瞬时命令连接；Rust 每连接 4 线程 ⇒ 64 上限 ≈ 256 线程峰值（< RAM 限制），且 Go 无上限（goroutine）⇒ 这是「Rust 形态下的资源收口」而非契约 |
| D10 | 握手期限值 | **10s** | 合法前端 hello 在毫秒级发出（`daemon_cli::dial_control*` 拨号即握手）；CLI 自身请求预算 `TIMEOUT=10s` ⇒ 与之同量级、不误杀 |
| D11 | GAP-AUDIT P1-4：做 vs 不做 | **做** | §2.1 |
| D12 | launchd：默认 state 才等 KeepAlive vs 现在（不看 state） | **只看默认 state** | 生产两形态：Mac launchd 零参（= 默认 state）/ 阿里云 nohup（自定义 state，无 launchd）；plist 精确匹配留 Q-J |

### 2.1 GAP-AUDIT P1-4 裁定（取证：**做**）

取证三问（按主会话给定口径）：

1. **该判据行是否在出货形态下缺失？** —— **是**。`docs/INTEROP-CRITERIA.md:60` 的 C14
   （出处 `baseline:clientcore/hostsession/session.go:280`）在 Rust 全仓**零实装**：无任何
   `出口能力` 打行（§0.3-G）；Rust 的服务会话（`session/mod.rs`，即 Go `hostsession` 的对应物，
   C3/C13/C16 都在此打行）在 C13 之后直接进入 hint/暖机，没有参照点探测打行。
2. **输入面是否可得？** —— **是**（缺口只在「消费 + 打行」）：`probe::ping_ex` 已解析
   `build`/`flags`（`probe.rs:38-46/140-193`），位值与 Go 逐位一致；只是三个消费点都把
   非端点字段丢掉（`session/mod.rs:1022-1052`、`hosts.rs:651-663`、`homeway-capi/src/lib.rs:262-267`）。
3. **是否只有内部路径不同、样例在档？** —— **否**。C14 行的「真实样例」列是 **Go 侧**采集
   （`出口能力：构建 homewayd-dev ｜ …`），Rust 出货形态（`connect`/daemon 每主机服务会话）
   的用户可见日志里没有这一行。
4. **用户可见后果**：GAP-AUDIT 原文（手机侧诊断）——「这台出口的 UDP 转发到底行不行」在
   Rust 客户端侧无从判断（浏览器 QUIC 超时回落 TCP 才发现）；该行是 tier 排障的既定抓手。

⇒ **裁定 = 做**（F17）。范围限定 = **服务会话打行**（Go 的 C14 出处即 `hostsession`）；
**不**扩到 App 状态 JSON（Go `tunmode` 无该面，`grep` 已核）。失败形态 `%v` 用 Rust 文案 +
登记（不伪造 Go 错误串）。

---

## 3. 非法配置打崩的**实测回归**方案（审计明确要求）

### 3.1 复现形态（**修前**，本棒已实测，§0.3-A）

| 形态 | 步骤 | 修前（实测） | 修后（期望；设计门 A2/B9 订正） |
|---|---|---|---|
| ① `peer_ttl="abc"`（值域错） | 临时 state（serve.enabled=false）+ 起统一进程 → 改 config 注入非法值 → `serve start --state T` | 统一进程 DEAD；CLI `等待应答超时` rc=1 | 进程 **alive**；`serve start` **rc=1 + 可行动文案（含 `config.toml` 路径与 `serve.peer_ttl`）**；**内存/文件均未变**（`serve status`：`enabled=false`、`state=stopped`）；修好 config 后再 `serve start` = `started\|already`（见 3.3） |
| ② `tx_shape.rate_mbps="200"`（类型错） | 同上 | 同①（DEAD） | 同①（文案含 `invalid type: string "200", expected u64`） |
| ③ 启动期非法（enabled=true + 非法值） | 直接起统一进程 | 进程退出（深层 exit 文案只进自己的 stdout） | **拒启**（exit 1）+ 可行动文案（`config.toml` 绝对路径 + 字段 + 值域；Go 同形） |
| ④（判 failed 面）装配期失败 | 合法 config 但 `listen` 指向被占端口 → 起统一进程（期望启用） | 现状 = 装配 Err → `failed` + 退避重建（已在） | 保持：`failed` + reason + 退避重建（Go `failedRole` 同形）——**这条才是「状态面 failed」的形态**；①② 按 D2 走「拒绝 + 零副作用」（Go `lifecycleStart` 同形） |

> **为什么 ①② 不再是 `failed`**：`ServeStart` 的第一步在 Go 是 `nodeconfig.Update`（严格读），
> 非法 config ⇒ **Update 失败 ⇒ 直接返回错误、角色不动**（`roleops.go:154-161`）。Rust 单表化后
> 同序 ⇒ 也走「拒绝 + 零副作用」；`failed + 退避重建` 留给**装配期**失败（形态④）与
> **运行期重建**（角色在跑、config 期间被改坏导致重建失败）——两条语义都要在 `QH.md` 里各留一例。

### 3.2 自动化回归（实现棒交付）

新增 `crates/homeway-cli/tests/qh_config_failfast.rs`（Cargo 集成测试；`env!("CARGO_BIN_EXE_homeway-cli")`）：

```
fn spawn_unified(state: &Path) -> Child        // 起统一进程（stdio → 文件）
fn wait_sock(state: &Path, d: Duration)        // 轮询 control.sock 可连
fn hermetic_config(listen: u16) -> String      // 见下：绝不打真网/不绑固定端口
#[test] config_failfast_does_not_kill_unified() {
    // 形态①：改 config 注入 peer_ttl="abc" → serve start rc=1 + 进程存活 +
    //        serve status 与文件一致（enabled=false/stopped）→ 修好 config → serve start ∈ {started, already}
    // 形态②：同上（tx_shape 类型错）
}
#[test] bad_config_at_startup_refuses_to_start() {
    // 形态③：exit code == 1，stdout 含 "peer_ttl" 与 config.toml 路径
}
```

**hermetic config（设计门 B8 采纳）**：测试「修好后 `serve start` 成功」这一档必须真装配 engine
⇒ config 必须显式给安全值：`listen=<测试探测到的空闲 UDP 端口>`、`bind_interface="none"`、
`upnp=false`、`stun=""`、`stun6=""`、`dns_port=0`、`files_root=<临时目录>`（`ServeConfig::default()`
会绑 41641 + 跑 UPnP/STUN——绝不能在测试里发生）。端口取法 = `UdpSocket::bind("127.0.0.1:0")`
读回端口后释放（小竞争面可接受；测试内串行使用同一端口）。

- 保险丝：`Drop` 里 `child.kill()`；所有等待带 deadline（10s）；**每测试独立 `mktemp` state**
  （不撞已知 flake 表里的固定端口）。
- **修前红证据**：本棒的 §0.3-A 手工实测（两形态 DEAD）即为修前红；实现棒须在提交信息/`QH.md`
  里贴「同形态修后绿」的运行输出。
- 另加手工冒烟（并入收敛实测并记录）：`relay start→stop→start`（F3 勾选）、`serve token list
  --state=DIR`（F2）、`serve stop --help` 不写 config（F8）。

### 3.3 判绿口径（不放松；设计门 B9 订正时序断言）

- 打崩形态消失 = 「控制面 op 拒绝（rc=1 + 可行动文案）」**且**「进程存活」**且**「内存/文件一致
  （状态面 `enabled` 与 config.toml 一致、`state` 不谎报）」三条同时成立；
- **时序不确定处只断言终态**：修好 config 后 `serve start` 的 action 可能是 `started`（无重建在跑）
  或 `already`（`bootstrap_serve` 已按退避自行重建成功）⇒ 断言 `∈ {started, already}` + 终局
  `serve status == running`（或 `serve token` 可取），不断言具体 action 词；
- 启动期形态 = exit 1 + 文案含 `config.toml` 路径与字段名（Go 同形「可行动」）；
- `cargo test --workspace` 全绿（已知 flake 按 ROADMAP 表甄别）+ `cargo clippy --all-targets
  -D warnings` clean。

---

## 4. 测试与验收计划

### 4.1 门（沿用批协议）

设计门（本文档 + 代码路径 → dsh）→ 实现 → 代码门（commit 范围 → dsh）→ 判据登记同批 commit。

### 4.2 逐条单测/集成测试清单（与 F 编号对齐）

| F | 测试 |
|---|---|
| F1 | `load_config_strict` 逐字段坏值（值域表每行 1 例）+ 好值往返；`assemble_result` 分类不 exit；op 顺序单测（写回失败 ⇒ 内存/文件均未变）；E2E（§3.2 形态①②③④） |
| F2 | `cli_flags::take_value/take_state` 表驱动 + 各站点 1 例（含 `serve token list --state=DIR` 正例、term 放宽正例、`--recover-cause -x` carve-out） |
| F4 | `socks_dead_is_recorded_and_rebuildable`（注入 `inject_accept_failure`；§F4） |
| F5 | cap 注入（2 条上限）/ deadline 注入（50ms）/ joiner 清理（`#[cfg(test)] conn_threads_len`） |
| F6 | `short_host` 五档（ASCII ≤8/>8/中文/emoji/边界） |
| F7 | F7a 数值五 flag 三档；F7b 布尔族六档（缺值/非法/`=false`/`=0`/`=true`/裸 flag） |
| F8 | `serve stop --help` rc=0 + config 未变；裸名词 `host --help` rc=0；`host add --help` rc=0 |
| F9 | `acquire` 错误分类（`LockIo` 含 path）+ 调用点复核（CLI 层不可构造时登记降级） |
| F10 | `resolve_host` 四档（表内小写/表内大写/表外 hex/64 非 hex） |
| F11 | 大写 hex 全链（add/list/off/delete + 级联） |
| F12 | 坏 token 记录日志行 + 状态面 failed |
| F13 | 模板过「serde + `load_config_strict` 值域」两层 + 注释键表逐键比对 |
| F14 | `launchd_relaunch_relevant` 二档 |
| F15 | 默认 state 取值单测（`serve`/`relay`/token 族） |
| F16 | `read_stop_signal` 四档（字节/EOF/EINTR/未安装 ⇒ `PipeErr`，壳不 exit） |
| F17 | 行格式纯函数 4 组 + 本地 UDP 桩端到端 + 失败形态 + 目标选取（域名-only/首项为域名） |

### 4.3 回归面（不得破）

- `cargo test --workspace`（已知 flake 按表甄别；**同工作树禁并发跑两个 cargo test**）；
- `cargo clippy --workspace --all-targets -D warnings`；
- `tools/check-vocab.sh`（契约词表）；
- 三目标交叉 `cargo check`（linux-musl / OHOS——本批不触平台相关面，但门不禁）；
- 手工冒烟：`local-rust-exit.sh` 起出口 + Go 基线客户端 `host add`（C3/C6/C16 同串）+ F3 勾选项。

---

## 5. 判据行影响（登记草稿）

### 5.1 `docs/INTEROP-CRITERIA.md`「判据变更记录」拟登记条目

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-H 批落地） | **非法 config 处理路径**（`config.toml` 非法值的后果与文案） | 控制面 handler 线程内 `process::exit(1)`（**打崩统一进程**；客户端只见 `等待应答超时`）→ ① 启动期：**拒启**（exit 1 + `config.toml` 路径/字段/值域可行动文案，Go `Load` 同形）；② op 触发（`serve/relay start\|stop\|restart`）：**拒绝 + 零副作用**（rc=1 + 可行动文案；内存/文件均不变；Go `lifecycleStart→Update` 同形）；③ 装配期失败（合法 config 但绑定失败等）：`failed` + reason + 退避重建（Go `failedRole` 同形） | F1：同一份 config 被弱表（`unified_cli`）/严格表（`serve_cli`）两套判定，弱表放行的类型/值域非法在严格表 exit ⇒ 控制面线程带走整个进程（审计实测两形态） | `serve_cli.rs`/`unified_cli.rs`/`relay_cli.rs`/`daemon_cli.rs`；状态面语义（`stopped` 与 `failed` 的分布变化见 §5.2）；新增 E2E `crates/homeway-cli/tests/qh_config_failfast.rs`；任何按「非法 config ⇒ 进程消失」写断言的脚本须改 |
| 2026-10-08（Q-H 批落地） | **config 值域校验补齐（= Go `validateFile` 全量）** | Rust 严格表只查 `peer_ttl`/`ddns.domain`/未知键 → **补** `serve.listen ∈ 1–65535`（0 拒）、`bind_interface`（含 `:/ \t` 拒）、`public_endpoint`（逗号分隔 ip:port）、`serve.relay`（rl1 或 ip:port）、`relay.listen`（`[host:]port` + 端口 1–65535） | F1：`serve.listen=0` 现状会绑随机端口、`serve.relay` 垃圾串晚失败、`relay.listen` 非法只在装配期失败——Go 一律启动期拒 | `serve_cli.rs`/`relay_cli.rs`；写回面（`nodeconfig.Update` 同形：坏 config 拒写）；**存量「能跑但违规」的手编 config 会被新拒**——须在 `QH.md` 提请注意 |
| 2026-10-08（Q-H 批落地） | **CA13 扩展**（`--help` 短路覆盖面） | `--help` 仅在 speedtest/carriers 族短路 → **`host`/`status`/`serve`/`relay`/`serve token` 命令组（动词形 + 裸名词，含此前硬报错的 `serve`/`relay` 前台形态）同样短路**（用法 + exit 0；`serve stop --help` 不再真停） | F8：审计实测 `serve stop --help` 真写 `serve.enabled=false` | CA13 行（行文不变、覆盖面注明）、`daemon_cli.rs`/`serve_cli.rs`/`relay_cli.rs` |
| 2026-10-08（Q-H 批落地） | **CA13 形态扩展**（离线 `serve status` 坏 config） | `config serve.enabled=<默认 true>`（静默降级）→ `config 读取失败：<err>`（Go `degradedServe` 同形） | F1 附带：Go `servegroup_cli.go:410-427` 如实报读失败 | CA13、`daemon_cli.rs` 离线分支 |
| 2026-10-08（Q-H 批落地） | **C14 首次实装**（客户端「出口能力」行） | 未实装（Rust 客户端零打行）→ 实装（成功行逐字 Go 同串；失败行 `出口能力：参照点探测失败（%v）—— 本机网络到出口的 UDP 不通或出口未应答`，`%v` = Rust io 文案） | F17：GAP-AUDIT P1-4 取证裁定 = 做（C14 缺失实锤） | C14 行、`session/mod.rs`、单测（纯函数 + 本地 UDP 桩）；行内 `%v` 平台文案差异注记（与 E20a 同类） |
| 2026-10-08（Q-H 批落地） | **取值 flag 纪律（`--state` 族 + 泛化）** | 缺值/吞下一个 flag/空值/`--state=` 等号形「静默取值或落 CWD」→ **全形态 fail-fast**（exit 2 + 可行动文案；等号形全站点生效；`serve token list\|revoke` 收下等号形）；**泛化到全部取值 flag**（值以 `-` 开头 ⇒ 报错；carve-out 仅 `--recover-cause`）；**term 两站点从「硬报错未知 flag」变为「接受等号形」**（放宽） | F2：审计实测 4 形态（含 `serve token list --state=DIR` 落 CWD）；设计门 B3/B12 | `cli_flags.rs`（新）+ 8 站点 + 其余取值 flag；CA11/CA13 文案不变（新增非法形态文案） |
| 2026-10-08（Q-H 批落地） | **flag 非法值/布尔显式赋值纪律** | `--hold/--probe/--recover-from/--recover-delay/--rounds/--listen(socks on)` 静默回退默认；`--upnp=maybe` 与 `--json=false/--yes=false/--force=false/--verbose=false` 等**布尔显式值被忽略（恒 true）** → **fail-fast（非法值 exit 2）/ 显式值真生效（Go `strconv.ParseBool` 集）** | F7a/F7b：与 Go `flag` 包语义对齐 + 审计 P2（设计门 B1/B2 扩面） | `main.rs`/`carriers_cli.rs`/`serve_cli.rs`/`daemon_cli.rs`/`relay_cli.rs`/`unified_cli.rs` |
| 2026-10-08（Q-H 批落地） | **N1/L7 默认 state** | 前台 `serve`/`relay`/`serve token list\|revoke` 默认 `.`（CWD）→ `default_state_dir()`（`~/.config/homeway`） | F15：GAP-AUDIT P0-1 余项；Go 前台默认同为 `$HOME/.config/homeway`（`internal/server/cli.go:30`、`relay/cli.go:33` 各自的 `defaultStateDir()`；**非** `cliutil` 单源——设计门 C-注记订正） | `serve_cli.rs`/`relay_cli.rs`；「全形态共用锁」在默认取值下成立 |
| 2026-10-08（Q-H 批落地） | **DAEMON 资源上限/期限（Rust 加固，接口零变更）** | 控制面连接无上限/握手无期限/joiner 句柄只增 → 上限 64 + 握手 10s + 句柄回收；超限/超时各一新日志行；SOCKS accept 烧尽后 `status.on` 由「谎报 true」→「false + 可行动 err」（`socks on` 可重建） | F5/F4：审计 P1/P2；Go 无上限（goroutine）⇒ 显式登记为加固 | `daemon/server.rs`、`daemon/carriers/socks_srv.rs`；新日志行（非判据行）；CA4/CA6 正常路径不变 |
| 2026-10-08（Q-H 批落地） | **CA11 形态扩展（launchd）** | 任何 state 都先等 launchd KeepAlive 4s → **仅默认 state** 等；非默认 state 直接自拉起 + 提示行 | F14：Q-G 移交（临时 state 会等生产出口的 launchd，KeepAlive 触发会把生产出口拉起） | CA11、`daemon_cli.rs` |
| 2026-10-08（Q-H 批落地） | **CA12 同族：`resolve_host` 全长 hex 就地校验** | 64-hex 直用（不查表、不 canonical 化；打错 id 由 daemon 回 `no_host`）→ **表内命中返回表内 canonical id；表外/大写未命中 = CLI 就地报错**（`没有匹配 … 的主机（host list 看全表）`） | F10：Go `host_cli.go:419-427` 全长 64 hex 必须精确命中表内 ID（未命中 `主机 %s 不存在`）；Rust 直用是分叉，且是大写级联漏删的入口 | CA12 同族、`daemon_cli.rs`、`carriers_cli.rs`（forward/socks/speedtest 的 host 解析共用同一 `resolve_host`） |
| 2026-10-08（Q-H 批落地） | **承载面 host 串 canonical 化（大小写等价）** | 大写 hex 能过成员检查却按原串存表 → `remove_host` 级联按小写找不到（**级联漏删：转发规则 + 监听器残留**）；改为入口 `decode→canonical 小写` 后再委托（**只规范化，不改错误分类**） | F11：`daemon/mod.rs:361-373` 只规范化级联一处，`add/remove/off` 原样（设计门复核确认根因成立） | `carriers/mod.rs`（+`forward`/`socksmgr` 入参）、`daemon/mod.rs`；CA3/CA6 成功文案不变；控制面直连客户端（App/矩阵）的输入等价化 |
| 2026-10-08（Q-H 批落地） | **session 锁 IO 失败 fail-fast** | 只读/异常 identity 目录下 `LockError::Io` 仅告警继续（**互踢 keypair 防线静默消失**）→ 可行动错误 + exit 1（`--no-session-lock` 仍为逃生口；`Held` 文案不变） | F9：审计 P2；Rust 独有防线的静默降级 | `main.rs`（connect/files/speedtest/dnstest/portfwd 五调用点）；只读环境从「降级继续」变「拒跑」 |

### 5.2 计数输入集 / 数值语义变化（行文不变）

- `serve.status`/`relay.status` 的 `state` **分布变化**（五态语义不变、输入集移动）：非法 config
  的 op 触发形态由「进程消失」→ `stopped`（拒绝 + 零副作用，Go 同形）；`failed` 保留给装配期失败
  与运行期重建失败——按本表列明。
- `serve.status` 的 `reject`/`dialFail` 等既有计数不受影响；F5 新增的拒绝/超时**不进状态面**
  （只进 daemon 日志）——登记备查。
- 控制面新增日志行（`连接拒绝（并发上限 64）`/`连接握手超时（10s 未 hello）`）——非判据行，登记备查。
- 离线 `serve status` 的 config 读取失败形态（并入 CA13 扩展行）。

### 5.3 明确不变的判据行（除 §5.1 已登记条目外）

DC1–DC13（控制面协议/角色/心跳/收工）、DC17–DC20、CA1–CA11、E 族、C 族（除 C14）——
行文与触发语义不变。**例外（已登记，设计门 A4 订正）**：CA12 同族（F10，全长 hex 报错提前到
CLI 侧）与承载面 host 大小写等价（F11）**属触发/输入语义变化**，以 §5.1 两条登记为准；
`session_lock` 只读环境行为变化（F9）同以 §5.1 登记为准。

---

## 6. 不做与移交登记（防静默漏做）

| # | 条目 | 处置 | 理由 |
|---|---|---|---|
| 1 | 半关闭透传加空闲期限 | **不做** | Go `netpipe` 同形（§0.4） |
| 2 | `remove/off` 落盘失败回滚 | **不做** | Go 同形（D5） |
| 3 | hosts 会话构造失败重建环 | **不做**（只补日志 F12） | Go 同形（D6） |
| 4 | `status --watch` view 扩展 | **不做** | Go 同形 + 无消费方 + 渲染行已扩（D8） |
| 5 | `relay_cli::FileServeIgnore` 死代码清理 | **不做** | 纯 churn（N5）；登记在案 |
| 6 | `serve`/`relay` 单角色形态的 stop 管道结构改造 | **不做** | Q-G §6.1-4 已裁定（单次生命周期、无 restart 面） |
| 7 | `tools/local-rust-exit.sh go-client-add` 的 `our_pid` 形态参数 | **移交**（工具面，Q-G §6.3 已登记） | 不在本批范围（CLI/daemon 控制面）；动手时属独立小项 |
| 8 | launchd plist 精确匹配（多实例/多 label/自定义 state） | **移交 Q-J** | Q-J 范围节已立条「launchd 探测精确化」 |
| 9 | `tx_shape` 键在 **Go nodeconfig 不存在**（Rust 独有扩展） | **登记**（不动） | 跨语言 config 键表差异 = Q-J「通用性」面；本批只保证 Rust 侧单表自洽 |
| 10 | daemon 控制面 `catch_unwind`/线程池化 | **不做** | 与 F5 上限配套已足以收口（每连接 4 线程 × ≤64） |
| 11 | 超限拒绝时 CLI 文案与真因不符（C7） | **登记**（不做文案改动） | 服务端新日志行已可归因；不动脚本可匹配的 CLI 文案 |
| 12 | `--recover-cause` 的自由文本（值可含 `-`） | **carve-out（F2 泛化规则的唯一例外）** | 自由文本形态，值以 `-` 开头合法 |
| 13 | 其余取值 flag 的「吞下一个 flag」（Go `flag` 语义） | **不做 carve-out**（随 F2 泛化一并收口） | 取证：路径/地址/域名/标识类值以 `-` 开头必然非法 ⇒ 吞 = 静默错配 |

---

## 7. 设计门记录（dsh 外部评审）

### 7.1 轮次与结论

- **轮次目录**：`/tmp/dsh-review/r19.eFzARr/`（`prompt.txt` / `output.md` / `stderr.log`）；
  命令 = `cd /Users/zhaozhe/Documents/projects/homeway-rs && dsh --profile headless "$(cat …/prompt.txt)"`
  （前台跑，已捕获退出码）。
- **exit code = 0**（成功；`output.md` 147 行，全量读毕）。
- **评审规模**：A 类（设计错误/高风险）**4** 条 + B 类（欠明确）**12** 条 + C 类（风格/记账）**9** 条
  + 「看过没发现问题」**14** 项（含独立复核 F3/F4/F6/F10/F11/F13/F14/F15/F17 的根因与 Go 对照）。
- **评审总评**（原文摘要）：「大纲（F1–F18 覆盖面对得上审计六项 + 三项移交）站得住，
  F3/F4/F6/F10/F11/F13/F14/F15 的根因与修法我逐条复核为真、可落地；但 **F1 的「单表 = Go 同形」
  这一承重前提、F1 在控制面 ServeStart 的落地顺序、F17 的探测目标选择、以及 §5.1 判据登记草稿**
  各有硬伤，属会导致返工或静默偏离 Go 的问题（A 类 4 条）」；并建议整改优先级
  「A1–A4 → B3/B8/B6/B7/B1 → 其余」。
- **处置总览**：**25 条全部认同并已并入本文档**（A 类 4 = 结构性修订 F1×2、F17、§5；B 类 12；
  C 类 9）；**0 条不认同**（合并项见 §7.3 备注）。**过门结论 = 通过（v2）**。

### 7.2 评审原文摘要（A 类，保留评审编号与严重度）

- **A1（高）**：F1 选定的唯一严格表比 Go `validateFile`（`config.go:254-292`）弱 5 处却宣称
  「Go 同形」——`serve.listen=0`（Rust 现状会绑随机端口）、`bind_interface`/`public_endpoint`/
  `serve.relay`/`relay.listen` 无校验；写回面同理（Go Update 拒写、Rust 会照写）。
  建议：补齐 Go 值域校验 或 显式登记逐条差异（现状 = 未登记的静默分叉）。
- **A2（高）**：F1 在 `ServeStart` 上「先写回、后装配」⇒ 形态②（tx_shape 类型错）写回先失败，
  `serve_reason` 不设、bootstrap 不拉 ⇒ 状态面变 `stopped`（而非设计断言的 `failed`），
  且出现「内存 enabled=true / 文件 false / 状态面 enabled=true」三处不一致；与形态①走两条不同路径。
  建议：固定顺序「严格读 → 写回 → 改内存 → 装配」+ 所有 Err 统一落脚（或改断言与 §5.2）。
- **A3（高）**：F17 目标取错——Go `cands[0]`（`cands = resolveCandidates(tok.Endpoints)`）≠
  Rust `static_cands[0]`（只装 IP 字面量；域名-only token 下为空 ⇒ panic/静默）；C13 用的是
  `merged_candidates()`。建议：改用「首个已解析候选」并写清数据源 + 空表守卫 + 域名形态测试。
- **A4（高）**：§5.1 漏了 F9/F10/F11 三条自述要登记的条目；§5.3「不改触发语义」与 F10/F11 冲突
  （CA12 同族 + 大小写等价）⇒ 违反 AGENTS 硬规则 4（未登记 = 静默破坏对齐）。

### 7.3 逐条处置表（评审编号 → 处置 → 落点）

| 评审编号 | 严重度 | 处置 | 落点 |
|---|---|---|---|
| A1 | 高 | **认同·改设计**（采「补齐 Go 值域校验」路线） | F1 段 2 新增**值域校验表**（9 行逐字段，含 `relay.listen` 整文件校验）+ 风险③ + §5.1 新登记行 |
| A2 | 高 | **认同·改设计** | F1 段 3 明确「严格读 → 写回 → 改内存 → 装配」与 **op 触发 = 拒绝 + 零副作用**；§2 D2 补 Go `lifecycleStart` 证据；§3.1 加形态④（failed 面）并重写修后期望；§3.3 判绿口径改写 |
| A3 | 高 | **认同·改设计** | F17 目标改为 `session/mod.rs:274` 的 `candidates.first()`（Go `cands[0]` 同义项），写明**不是** `static_cands[0]`/`merged_candidates()[0]` + 实现建议（`:280` 早退后取 target）+ 测试④两档 |
| A4 | 高 | **认同·改设计** | §5.1 补 3 行（F9/F10/F11）+ 重写 §5.3（例外指向登记行） |
| B1 | 中高 | **认同·扩面**（布尔族并入 F7b） | F7 新增 F7b（`take_bool` + 全站点清单 + ParseBool 集），§5.1 登记行合并改写 |
| B2 | 中低 | **认同** | F7a 清单补 `--recover-from`（`main.rs:277-280`） |
| B3 | 中 | **认同**（泛化取值器） | F2 增「同族泛化」段 + §6-13（不做 carve-out 的理由）+ carve-out 仅 `--recover-cause`（§6-12） |
| B4 | 中 | **认同**（并入 A1） | F1 值域表 `relay.listen` 行 + 段 2 `relay_config_of`（启动期一并跑） |
| B5 | 中低 | **认同** | F5 段 1 明确 4 处构造面 + 显式字段注入（不做 builder） |
| B6 | 中 | **认同·改设计** | F16 重写：拆 `read_stop_signal`（`StopWait` 枚举）+ 壳不 exit（可进程内断言）+ 测试四档；§4.2 同步 |
| B7 | 中 | **认同** | F4 测试改 `#[cfg(test)] inject_accept_failure`（弃 `libc::close` 硬关） |
| B8 | 中高 | **认同** | §3.2 增 `hermetic_config(listen)`（`bind_interface=none`/`upnp=false`/`stun=stun6=""`/`dns_port=0`/`files_root=tmp`/空闲端口） |
| B9 | 中 | **认同** | §3.1 修后期望 + §3.3：`serve start ∈ {started, already}` + 终态断言，不断言具体 action 词 |
| B10 | 中低 | **认同·订正覆盖面** | F8 重写（动词形 + 裸名词 `cmd_host`/`cmd_serve_group`/`cmd_relay_group`/`cmd_status`/`serve token`；收 `-h`）；§5.1 CA13 行同步 |
| B11 | 低中 | **认同** | F9 重写（只 `LockError::Io` fail-fast；`Held` 文案不动；环境边界入 §5.1/§6） |
| B12 | 低 | **认同** | F2 增「term 两站点属放宽」+ §5.1 影响面注明 |
| C1 | 记账 | **认同·改文档** | §0.2 #17 注「审计 `:257` 已漂 → 现行 `:262`」；#2 的 `main.rs` 站点订正为 `cmd_files`（portfwd/dnstest 不吃 `--state`） |
| C2 | 记账 | **认同** | 交叉引用修正（F2 的 `F20` → F15；F18 的「F16 剔除项」→ §0.4 D8） |
| C3 | 记账 | **认同** | F5/§2 D9 改名 `DEFAULT_MAX_CONTROL_CONNS` |
| C4 | 记账 | **认同** | §0.3-F 证据改为「grep 命中为空」并引用评审独立复核的脚本清单（`local-exit.sh:99/132/166`、`local-rust-exit.sh:68/97`、`local-relay.sh:54`、`local-rust-relay.sh:57`、`matrix.sh:166/169/191/201/226/228`） |
| C5 | 记账 | **认同** | F6 增「Go 同串只对合法 hex 成立」口径注记 |
| C6 | 记账 | **认同** | F13 测试补「过 `load_config_strict` 值域层」+ 注释键表补 `public_endpoint` + 逐键比对断言 |
| C7 | 记账 | **认同·登记** | F5 风险段增「已知观测缺口」（超限被拒时 CLI 文案与真因不符；服务端日志可归因；不动 CLI 文案）+ §6-11 |
| C8 | 记账 | **认同** | F5 测试用 `#[cfg(test)] conn_threads_len()` |
| C9 | 记账 | **认同·改文档** | F1 段 2 订正「exit 只允许出现在最前台（parser/壳），控制面与装配路径零 exit」+ 代码门 `grep` 检查项 |

**备注（不认同 0 条）**：§0.3-F/C4 的订正为「证据命令写法」问题（评审独立复核后的脚本清单成立，
结论不变）；C7 我选择「登记不做文案改动」（理由：`daemon_cli.rs:228-235` 文案已含核对指引，
且脚本可匹配面不宜漂）——该取舍已写入 §6-11。

### 7.4 过门结论（v2）

- A 类 **4/4** 已改为结构性修订（F1 值域表 + op 顺序、F17 目标、§5 登记表重写）；
  B 类 **12/12** 已并入（F2 泛化、F7b 布尔族、F4/F5/F16 测试形态、F8 覆盖面、F9 分类、§3 hermetic/时序）；
  C 类 **9/9** 已改文档或改设计；评审的「看过没发现问题」14 项与本棒复验结论一致（无冲突）。
- **残留风险（转实现棒/代码门）**：① 值域校验补齐会让部分存量手编 config 从「能跑」变「拒启/拒写」
  （Go 一直拒）——实现棒须在 `QH.md` 显式提示；② E2E 的端口探测存在小竞争面（测试内串行 + 空闲端口）；
  ③ `read_stop_signal` 的 EINTR 档在 macOS/Linux 行为一致性（实现棒跑双平台 CI 断言）。
- **判据行影响清单已按 A4 补齐**（§5.1 共 13 行），随实现同批 commit。
