# Q-H 批记录：CLI 与 daemon 控制面（config 单表 / `--state` 形态族 / daemon 上限与状态诚实）

> 批次 = Q-H（`docs/REVIEW-ROADMAP.md`「Q-H CLI 与 daemon 控制面」）；实现规格 =
> `docs/reviews/QH-design.md`（v2，设计门 dsh `r19.eFzARr` exit=0，25/25 认同并入）。
> 本文件 = 实现棒记录（生产 config 预检 + 设计门 r19 摘要 + 代码门 r20 + 逐条处置 +
> 测试/判据证据 + 非法配置实测回归）。
> 实现基线 = HEAD `1d2a59f`（Q-G 收口）。

---

## 0. 生产 config 预检（**第一步，动码之前**；只读，绝不修改/重启）

**预检结论：Mac 生产出口 config（`/Users/zhaozhe/.config/homeway-rs/config.toml`）
在新严格 schema + 五项补齐值域下【全部通过】——不会被拒启。无阻塞项。**

方法：只读读取两份本机 state 的 `config.toml`，按 `QH-design.md` §1 F1 的新校验面
（serde 严格表 `deny_unknown_fields` + Go `validateFile` 全量值域）逐项静态判定；
rl1 token 可解码性经**隔离 /tmp state 副本** + 既有 CLI 校验面（`serve relay set`
的 `validate_relay_arg`）实测确证（全程未触碰生产 state）。

### 0.1 Mac 生产出口（launchd `me.zhaozhe.homeway-exit`，`--state /Users/zhaozhe/.config/homeway-rs`）

| 检查项 | 生产值 | 新校验规则 | 判定 |
|---|---|---|---|
| TOML 严格解析（键表） | `[serve]` enabled/listen/relay；`[relay]` enabled | 全键在 schema 内（deny_unknown 不命中） | **通过** |
| `serve.listen` | `41641` | 1–65535（0 拒） | **通过** |
| `serve.bind_interface` | 缺省（= auto） | 空/auto/none/off/no/IP 字面量/网卡名（含 `:/ \t` 拒） | **通过** |
| `serve.public_endpoint` | 缺省 | 空 = 关 | **通过** |
| `serve.peer_ttl` | 缺省（默认 168h） | 时长串且 ≥ 0 | **通过** |
| `serve.relay` | `rl133ZkGIEpRj…`（rl1 token） | rl1 必须可解码 | **通过**（隔离副本实测 `rc=0`，token 解码成功） |
| `serve.ddns` | 无 | 非空且不含 `:/ `（无条目不触发） | **通过** |
| `relay.listen` | 缺省（= `:41741`） | `[host:]port`，端口 1–65535 | **通过** |
| `[serve.tx_shape]` | 无 | 类型由 serde 保证 | **通过** |

### 0.2 本机另一 state（`/Users/zhaozhe/.config/homeway`，默认 state 根）

同法只读逐项核对：`listen=41641` ✓；`bind_interface="auto"` ✓（Go 允许字面 auto）；
`peer_ttl="168h0m0s"` ✓（`parse_go_duration` 解析 168h+0m+0s）；`dns_port=5300` ✓；
`files_root=""` ✓（空 = $HOME）；`relay="rl1cMrX74AEx…"` ✓（隔离副本实测解码成功）；
`[relay] listen=":41741"` ✓；`advertise=""` ✓。**全部通过**。

### 0.3 阿里云出口

**不可达，未检**——本机无其 state 副本/挂载面（`find ~` 未命中任何阿里云 state 或配置工件；
其配置在远端主机上）。按硬规则，本棒未做任何远端触达。

---

## 1. 实现清单（F1–F17；文件/函数级）

| # | 落点 | 做了什么 |
|---|---|---|
| **F1** | `crates/homeway-cli/src/serve_cli.rs`（重写 config 段）、`unified_cli.rs`、`relay_cli.rs`、`crates/homeway-core/src/server/intercept/mod.rs` | **单表**：`FileServe/FileDdns/FileRelay/FileConfig` 升为 `pub(crate)` + `Serialize` + 手写 `Default`（`serve.enabled` 缺省 true = Go `defaultFile` 同义）；`TxShapeCfg` 补 `Serialize` + `skip_serializing_if`；`unified_cli` 弱表与 `relay_cli` 分节私表（`FileServeIgnore`/`parse_relay_section`，N5 死代码）**删除**。**纯函数层**：`load_config_strict`（缺失 = 默认；存在 = TOML 严格解析 + `validate_file` 全量值域，文案 = `路径: 字段：值域`）+ `serve_config_of` + `relay_config_of` + `assemble_result`（`CliErr::Usage/Config`，不 exit）；`assemble` 退化为前台薄壳（Usage→exit 2 / Config→exit 1）。**统一进程接面**：启动期 `load_config_strict` 失败 = 「config 非法——拒绝启动（先修复再启动）：…」+ exit 1；`assemble_serve` 零 exit（严格读 + 映射 + 装配，Err 上抛）；六个角色 op 顺序固定「**严格读 → 写回 → 改内存 →（start/restart）装配**」，任一步 Err = 拒绝 + **零副作用**；`restart` 不变期望态（Go `lifecycleRestart` 不写 config）故只严格读不回写；写回面（`write_config_enabled`/`update_config`/`config_serve_ddns`）全部走严格读（坏 config 拒写）；离线 `serve/relay status` 坏 config = `config 读取失败：<err>` |
| **F2** | 新文件 `crates/homeway-cli/src/cli_flags.rs` + 8 个 parser 站点 | 唯一取值器：`split_flag`/`take_value`（`allow_dash=false` 时下一个 token 以 `-` 开头 ⇒ Missing）/`take_state_or_exit`/`take_num_or_exit`/`take_bool`；`--state` 全形态 fail-fast（缺值含「下一个是 --json」、`--state=` 空值、等号形全站点生效）；泛化到全部取值 flag（serve/relay/daemon/carriers/term/main 全域；唯一 carve-out `--recover-cause`）；`daemon_cli::normalize_flag_eq` 删除（N2：`--state=` 空值不再当目标文件名）；term 两站点与之收编（放宽，旧形态是硬报错） |
| **F3** | 不改码 | `relay start → stop → start` 复核（Q-G F3 根因已修）：实测 `started → stopped → started`，`relay status` 终态 `state=running`，daemon-events 的 broken 路径行计数 **0**（见 §3.3） |
| **F4** | `daemon/carriers/socks_srv.rs`、`carriers/mod.rs`（`PollListener` 注入缝） | `PollAccept::Failed(err)` 分支先 `self.mark_dead(&err)` 再上抛；`close()` 不清 dead（off 换新对象自然归零）；测试注入缝 `inject_accept_failure`（不硬关真 fd——设计门 B7）；单测 `socks_dead_is_recorded_and_rebuildable`（dead 落账 → status `on=false` + err 可查 → `on()` 重建成功、端口不变、dead 归零） |
| **F5** | `daemon/server.rs`、`unified_cli.rs`、`daemon/tests.rs` | `ServerConfig` 增 `max_conns`/`handshake_deadline`；`DEFAULT_MAX_CONTROL_CONNS = 64`（改名避撞 socks 的 `DEFAULT_MAX_CONNS`）+ `DEFAULT_HANDSHAKE_DEADLINE = 10s`；accept 超限「接受后立即关闭 + 记行」；reader 500ms 拍加未握手超期断开 + 记行；每次 accept 前 `reap_conn_threads`（`is_finished` 不阻塞）+ `#[cfg(test)] conn_threads_len()`；三单测（cap 2 注入 / 80ms 期限注入 / 句柄回落） |
| **F6** | `daemon/carriers/mod.rs::short_host` | 字符截断（`chars().take(8)` + `…`，判据 `chars().count() > 8`）；口径注记：与 Go 同串只对合法 hex（ASCII）成立，非法输入取「不 panic + 整串」自定义语义；单测五档（ASCII 两档/中文/3 字节边界/emoji） |
| **F7** | `cli_flags.rs` + `main.rs`/`carriers_cli.rs`/`serve_cli.rs`/`daemon_cli.rs`/`relay_cli.rs`/`unified_cli.rs` | F7a：`--hold/--probe/--recover-from/--recover-delay/--rounds/socks on --listen` 全部 fail-fast（此前静默回退）；F7b：`take_bool` 按 Go `strconv.ParseBool` 值集，全站点布尔旗（`--upnp/--verbose/--json/--yes/--force/--stdin/--no-spawn/--watch/--open/--no-hints/--dead-direct/--no-session-lock/--status-json/--speedtest/--hold(speedtest)`）显式值真生效、非法值 exit 2 |
| **F8** | `daemon_cli.rs`/`serve_cli.rs`/`relay_cli.rs`/`main.rs` | `parse_args` 的 `h/help` 从「静默吞掉」→ 用法 + exit 0；裸名词 `host`/`status`/`serve`/`relay`/`serve token` 组各加 help 分支；serve/relay 前台 parser 加 help；`connect/files/dnstest/portfwd/speedtest(直连)/token` 的 args[0] `--help` 收口（E2E 断言 6 形态 rc=0 + config 未变） |
| **F9** | `main.rs::session_lock_or_exit` | 只对 `LockError::Io` fail-fast（可行动文案 + exit 1）；`Held` 文案不动；`--no-session-lock` 仍是逃生口；单测 `unwritable_dir_is_lock_io_with_path` |
| **F10** | `daemon_cli.rs::resolve_host` | 64-hex 分支改「`decode_peer_id_pub` → 小写 canonical → 与表内 id（case-insensitive）比对」；命中返回表内 id；未命中就地 `没有匹配 … 的主机（host list 看全表）`；单测四档 + 名称/前缀回归 |
| **F11** | `daemon/carriers/mod.rs`（`canonical_host_hex`） | `add_forward`/`remove_forward`/`socks_on`/`socks_off`/`speedtest_{start,status,cancel}` 入口 canonical 化（decode 失败保持原串 ⇒ 错误分类不变）；单测大写全链（add→list→delete / on→off→级联删净 / 坏 hex 仍 NoHost） |
| **F12** | `daemon/hosts.rs::start_session_for` | token 解码失败补日志行（`会话构造失败：台账 token 解码失败（重新 host add 刷 token 可修复）`）；不加重建环（Go `facade/table.go` 同形） |
| **F13** | `homeway-core/src/nodestate.rs` + `serve_cli.rs` 测试 | 模板注释 `burst_kib → burst_kb`；注释键表补 `public_endpoint`；测试：模板过 serde + `load_config_strict` 两层 + `burst_kb` 存在/`burst_kib` 不存在 + 注释键表逐键清单断言 + 全键可接受 |
| **F14** | `daemon_cli.rs` | `launchd_relaunch_relevant(state)`（纯函数，仅默认 state）+ 非默认 state 直接自拉起并打提示行；单测二档。**边界见 §5.2 残余**（本机 Mac 生产形态即「launchd + 自定义 state」，设计门对该部署形态的假设与实机不符——已上报主会话） |
| **F15** | `serve_cli.rs`/`relay_cli.rs` | 前台 `serve`/`relay`/`serve token list\|revoke` 默认 state = `default_state_dir()`（`~/.config/homeway`）；单测 `default_state_matches_unified` |
| **F16** | `serve_cli.rs` | `StopWait{Signaled, PipeErr}` + `read_stop_signal_with`（纯逻辑：fd<0 ⇒ Err 不读 fd 0；n>0/EOF ⇒ Signaled；EINTR 重试；其它 errno ⇒ 可读文案）+ `wait_stop_pipe` 壳不 exit；两调用点（serve_cli/unified_cli）改 `match` 记行；单测四档（含 EINTR 注入缝） |
| **F17** | `session/mod.rs`（C14 打行）+ 测试 | C13 之后一次性探测线程（`probe::ping_ex(target, 16, 5s)`；spawn 失败记行）；目标 = `candidates.first()`（Go `cands[0]` 同义，**不是** `static_cands[0]`/`merged_candidates()[0]`）；`format_outbound_caps_line` 成功行逐字 Go 同串、`format_outbound_caps_fail` 失败行（`%v` = Rust io 文案，平台差异登记）；单测 4 例（纯函数含 C14 真实样例逐字 / 本地 UDP 桩端到端 / 无应答失败行 / 目标选取域名-only 与首项为域名） |

> 附带修正（实现期发现，登记在案）：`term_cli` 的取值/白名单按**原形**（含 `-d`/`--detach-key` 前缀）比对；
> `-o`（files get）补「下一个是 flag/末尾 ⇒ 缺值」fail-fast；`cmd_token` 的布尔三旗收编（`=false` 真生效）。

---

## 2. 设计门记录（r19，转述设计文档 §7）

- **轮次目录**：`/tmp/dsh-review/r19.eFzARr/`；**exit = 0**；A 类 4 + B 类 12 + C 类 9 = **25/25 全部认同并入 v2**（0 不认同）。
- 关键结构性修订（已在本棒实现中体现）：F1 值域表全量 + op 顺序「严格读 → 写回 → 改内存 → 装配」与零副作用 / F17 目标改 `candidates.first()` / §5 判据登记补 3 行（F9/F10/F11）/ §3 hermetic 测试配方与判绿口径。

---

## 3. 测试与实测证据

### 3.1 全量门

| 门 | 结果 |
|---|---|
| `cargo test --workspace` | **全绿**（代码门整改后复跑 2 轮全绿）：homeway-core lib **616 passed / 0 failed / 4 ignored**（+19s；含代码门补的 drip/F12 两测）；homeway-cli bin 33 passed；**新增 E2E `qh_config_failfast` 5 passed**；向量/夹具族（fuzz_replay 1、identity 3、r2 3、r5 4、relay 2、token 3、vocab_dump 1、clientcore 4）全绿；**0 failed**。**flake 甄别（一次红）**：代码门首次全量跑时 `daemon::tests::server_bad_frame_gets_goodbye_and_disconnect` 红（**ROADMAP 已知 flake 表在册**：「读日志行时序；同一二进制重复单跑红绿翻转」）——甄别三证：① **隔离复跑 5/5 绿**；② **全量复跑 2 轮全绿**（同一二进制）；③ 与改动面无交集：本批对 `server.rs` reader 的改动在**握手后**走 `deadline=None` 的纯委托路径（`read_head_deadline(None)`/`read_body_buf_deadline(None)` 与旧实现逐语句同形），失败面是该测试自身的日志行时序。判 **flake**（登记在案，不算回归） |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean（无告警）** |
| `tools/check-vocab.sh` | **PASS**（Rust 声明 5 单元 / 26 值；ledger sha256 一致；缺席表 4 项在册） |
| 跨目标编译（CI 等价面） | `cargo check --target x86_64-unknown-linux-gnu --all-targets -p homeway-core -p homeway-cli` **通过**（代码门 H1 的回归面：修前 `libc::__error` 会让 ubuntu `--all-targets` 编译失败） |
| 并发纪律 | 全程单实例跑（未并发两个 `cargo test`；已知固定端口 flake 未触发） |

新增测试清单（本批）：`cli_flags::tests`（5）、`serve_cli::tests`（8：strict 缺失/逐字段坏值/assemble_result 分类/--state 形态/布尔显式/默认 state/stop 信号四档/模板两层+键表）、`unified_cli::tests`（+2：parse_unified_args 形态与 verbose=false、**op 零副作用**）、`daemon_cli::tests`（2：launchd 纯函数、resolve_host 四档）、`daemon::tests`（+4：cap/期限/句柄回收/**慢滴水期限**〔代码门 M2〕）、`daemon::hosts::tests`（+1：**坏 token 记行 + failed/session_not_built**〔代码门 M5〕）、`carriers::tests`（2：short_host 五档、大写全链）、`socksmgr::tests`（+1：dead 落账与重建）、`session_lock::tests`（+1：LockIo 分类 + path，root 下跳过〔L3〕）、`session::upgrade_streak_tests`（+4：C14 四例，目标选取经 `c14_probe_target` 纯函数钉接线〔M6〕）、`tests/qh_config_failfast.rs`（5：E2E，含 `--token=` 等号形与 `--force=false` 断言〔M4〕）。

### 3.2 非法配置实测回归（审计明确要求：两形态**复现 → 修后同形态验证**）

**修前红**（设计棒实测，`docs/reviews/QH-design.md` §0.3-A，HEAD `1d2a59f`）：形态①
`peer_ttl="abc"` 与形态② `tx_shape.rate_mbps="200"` 经 `serve start` 都使**统一进程 DEAD**、
CLI 只见 `等待应答超时`、错误只在统一进程自己的 stdout。

**修后绿**（本棒 `/tmp` 隔离 state 实跑，`target/debug/homeway-cli`，跑完杀进程 + 清目录）：

```
====== 形态① peer_ttl=abc ======
homeway: bad_request：config 非法（拒绝 start，先修复）：<T>/config.toml: serve.peer_ttl："abc" 非法（时长串，如 "168h"；须 ≥ 0，0 = 关闭 TTL 回收）
cli-rc=1
UNIFIED-ALIVE                       ← 修前 = DEAD
config-file-unchanged=YES           ← 零副作用（逐字节比对）
serve：enabled=false state=stopped listenPort=0     ← 状态面如实、不谎报

====== 形态② tx_shape.rate_mbps="200" ======
homeway: bad_request：config 非法（拒绝 start，先修复）：<T>/config.toml: TOML parse error at line 11, column 13 … invalid type: string "200", expected u64
cli-rc=1
UNIFIED-ALIVE
serve：enabled=false state=stopped …

====== 修好后 serve start ======
serve：started          （rc=0；设计口径 ∈ {started, already}）
serve：enabled=true state=running listenPort=58621
```

**启动期形态（③）**：坏 config 直起统一进程 ⇒ exit 1 + `homeway: config 非法——拒绝启动（先修复再启动）：…
config.toml: serve.peer_ttl …`（E2E `bad_config_at_startup_refuses_to_start` 断言退出码与文案）。
**装配期（④）**：合法 config + 端口占用 = `failed` + reason + 退避重建（既有机制，未改；F1 起 ①② 不再落此面）。

**E2E 自动化**（`crates/homeway-cli/tests/qh_config_failfast.rs`，随 `cargo test --workspace` 跑）：
`config_failfast_does_not_kill_unified`（两形态：rc=1 + 进程存活 + 文件逐字节不变 + 状态面 stopped + 修好后 started/running）、
`bad_config_at_startup_refuses_to_start`、`serve_stop_help_does_not_touch_config`、
`state_flag_forms_failfast_and_eq_form_accepted`、`numeric_and_bool_flags_failfast`。

### 3.3 手工冒烟（/tmp 隔离 state；跑完无残留进程、无残留目录）

- **F3 勾选复核**：`relay start → stopped → started`，`relay status` = `enabled=true state=running listen=:63700`；
  `daemon-events.log` 的 broken 路径行（`退避 … 后进程内重建`）计数 **0** ⇒ Q-G F3 根因确实已修，**本批不重做，勾选通过**。
- **F8**：`serve stop --help` rc=0、无用法外副作用，config 未被改写（`enabled = true` 仍在）。
- **F2**：`status --state --json` ⇒ `--state 缺值（下一个是 --json）`；`status --state=` ⇒ `--state 空值（= 后为空）`；`serve token list --state=<dir>` 落在指定目录（不落 CWD，E2E 断言）。

---

## 4. 判据行登记（随本批同批 commit）

`docs/INTEROP-CRITERIA.md`「判据变更记录」：**登记 14 行**（设计拟 13 行 + 实现棒补 1 行
`short_host` 行为注记——合法 hex 逐字节不变，属「非判据行行为注记」但显式留痕防漂移）：

非法 config 处理路径 / config 值域校验补齐 / CA13 扩展（`--help` 覆盖面）/ CA13 形态扩展（离线坏 config）/
**C14 首次实装** / 取值 flag 纪律 / flag 非法值与布尔显式赋值 / N1·L7 默认 state / DAEMON 资源上限与期限 +
SOCKS dead（Rust 加固）/ CA11 形态扩展（launchd）/ CA12 同族（`resolve_host` 就地校验）/
承载面 host 大小写等价 / session 锁 IO 失败 fail-fast / short_host 字符语义。

「计数输入集/数值语义变化」表另加 **2 行**：① `serve.status`/`relay.status` 的 `state` 分布移动
（非法 config 的 op 触发：进程消失 → `stopped`；`failed` 保留给装配期/运行期重建失败）；
② 控制面新增 additive 日志行（连接拒绝/握手超时/非默认 state 提示/C14 失败行——不进状态面）。

---

## 5. 不做项 / 残余 / 需上报项

### 5.1 不做项（与设计 §6 对齐）

半关闭透传空闲期限（Go 同形）/ `remove`·`off` 落盘失败回滚（Go 同形）/ hosts 会话构造失败重建环
（Go 同形，只补日志 F12）/ `status --watch` view 扩展（Go 同形 + 无消费方）/ serve·relay 单角色 stop 管道改造
（Q-G 已裁）/ launchd plist 精确匹配（**移交 Q-J**）/ `tx_shape` 键跨语言差异（登记）/
daemon `catch_unwind`/线程池化（F5 上限已够）/ 超限拒绝时 CLI 文案与真因不符（登记，不动脚本可匹配文案）/
`--recover-cause` 为取值泛化的唯一 carve-out（登记）。

### 5.2 残余与需上报项（主会话裁决）

1. **【需上报】F14 的部署形态假设与实机不符（行为收窄面，非阻塞）**：
   设计门 D12 的理由写作「Mac launchd 零参（= 默认 state）/ 阿里云 nohup（自定义 state）」，
   但本机在册 launchd 代理 `me.zhaozhe.homeway-exit` 的 `ProgramArguments` 是
   **`--state /Users/zhaozhe/.config/homeway-rs`（自定义 state）**（只读核验）。F14 落地后，
   该 state 下**守护进程不在跑时** CLI 不再等 KeepAlive 4s，而是立即自拉起（launchd 的
   KeepAlive 也会同时拉，二者由实例锁裁决；落败方退出，会出现 10s 节流的 KeepAlive 重试抖动，
   且最终在跑的可能是不受 launchd 托管的 spawn 实例）。守护在跑（常态）时无差异。
   缓解选项（未自行拍板）：① 维持设计（Q-J 做 plist 级精确匹配）；② 把判据放宽为
   「默认 state **或** plist 内容提及该 state」→ 保持生产形态旧行为。**请主会话裁决**；
   本棒已按设计实现（未自改判据）。
2. **`relay.listen` 接受集**：新校验复用装配期 `parse_listen`（IP 字面量 + `:port`），
   比 Go `net.SplitHostPort` 略严（Go 还接受 `localhost:41741` 之类主机名）。当前行为 = 与既有装配面一致、
   只是**更早报错**（不新增拒绝面）；跨语言差异登记在此（如需放宽可随 Q-J 通用性批处理）。
3. **`serve.relay` 端口 0**：Go `netip.ParseAddrPort` 允许 `ip:0`，Rust 的 CLI 口径拒 0
   （沿用既有 `validate_relay_arg`；方向 = 更严，不新增拒绝面之上再收紧——属登记差异）。
4. **C14 探测线程生命周期**：会话收工时探测可能仍在飞（5s 上限，线程自灭；与 Go goroutine 同形态）。
5. **F5 已知观测缺口（设计门 C7，沿用登记）**：超限被拒时 CLI 首拨文案与真因（并发上限）不符；
   服务端新日志行可归因，CLI 文案不动。
6. **term 放宽**：`term … --state=DIR` 从「硬报错未知 flag」变「接受」（已登记影响面）。
7. **测试技术偏差（如实登记）**：F16 的 EINTR 档用**注入缝**（`read_stop_signal_with`，代码门 H1 后为
   `Result<usize, io::Error>` 形、不碰 errno）而非设计建议的 `raise(SIGUSR1)`+空 handler——
   进程级信号在 `cargo test` 并发下有误伤面，且 macOS `raise` 线程定向不保证命中在途 `read`
   （会造出永远通过的假测试）；注入缝钉的是同一段重试逻辑。代码门后该缝还要满足「linux 面可编译」。
8. **F11 只保证新写入 canonical（代码门 L5）**：盘上遗留的旧大写 host 条目既不在装载时归一化，
   也不会被 `remove_host` 级联摘掉（迁移面留后续；本批从**入口**消除新产生路径）。
9. **`-o` 的新拒绝面（代码门 L6）**：`files get x -o -foo` 在 Go 可用（无条件收下一 token）、Rust 现拒；
   实值影响≈0（仅文件名形态），已随取值 flag 登记行一并列出。
10. **`peer_ttl` 接受集窄于 Go（代码门 L8，pre-existing）**：Rust 只收 `数字+{s,m,h}`，
    Go `time.ParseDuration` 还收 `300ms`/`1.5h`/`1h30m`——**不是本批新增拒绝**（同一函数原本就是门）；
    「值域表 = Go validateFile 全量」的声明对该字段为「逐条对齐**既有**口径」。
11. **控制面 serve 线程的 `exit(1)`（代码门 A1，pre-existing）**：`unified_cli.rs` 的 control 角色
    listener 永久失败仍是「记行 + exit(1)」（启动期 fail-fast 形态，非 config 输入触发）。
    F1 的「控制面路径零 exit」指**config 触发**的路径（已消灭）；grep 门按此口径执行。
12. **role_op 内 `spawn(...).expect`（代码门 A2）**：沿用 §6-10 裁定（不做 catch_unwind）；
    线程创建失败（EAGAIN 级）会带走**该请求的答复**，非整进程。
13. **两套等号形入口并存（代码门 A4）**：`carriers_cli` 用既有 `expand_flag_eq`，其余站点走
    `cli_flags::split_flag`；行为一致、合并留后续小项。
14. **写回无 CAS 重试（代码门 A5，pre-existing）**：Go `nodeconfig.Update` 有 8 次 CAS 重试；
    Rust `write_config_enabled` 是 read-modify-write（最后写者赢）。本批只把写回收进严格表。
15. **F5 队列/句柄面**：`conn_threads` 只在 accept 路径回收（idle 时已结束句柄会滞留到下一次接入）；
    上限判据用 `conns`（并发在世数），句柄数不参与裁决。

### 5.3 行为收窄提示（给用户看；`INTEROP-CRITERIA` 登记行同义）

> **存量「能跑但违规」的手编 config 会被新校验拒绝**（Go 一直拒）：`serve.listen=0`（旧行为绑随机端口）、
> `serve.relay` 垃圾串（旧行为运行期晚失败）、`bind_interface` 带 `:`/`/`、`public_endpoint` 非 `ip:port`、
> `relay.listen` 非法/端口 0、`[serve.tx_shape]` 类型错。**好处**：非法配置从「打崩统一进程/静默降级」
> 变为「启动期拒启 / 控制面 op 拒绝 + 零副作用」，文案带 `config.toml` 路径 + 字段 + 值域。
> **本机两台 state 已在动码前逐项预检通过（§0），生产出口无起不来风险**；阿里云出口不可达未检
> （如其在用同样「违规」写法，重启前建议按 §0 清单自查）。

---

---

## 6. 代码门（dsh 外部评审 r20）记录

- **轮次目录**：`/tmp/dsh-review/r20.r0xcC5/`（`prompt.txt` / `output.md` 103 行 / `stderr.log`）；
  命令 = `cd /Users/zhaozhe/Documents/projects/homeway-rs && dsh --profile headless "$(cat …/prompt.txt)"`（前台跑，已捕获 **exit = 0**）。
- **评审规模**：高危 **1** + 中 **6** + 低 **8** + 记账 **5**；「看过没发现问题」**22 项**（含独立复核单表收敛、值域表逐行、stop/restart 零副作用、F4 dead 链、F5 上限语义、F16 语义、F17 逐字符对齐、越界检查、E2E 隔离等）。
- **总评（原文摘要）**：「F1 值域表逐行 vs Go `validateFile` … 与 `config.go:254-292` 顺序与语义一致」「单表收敛是真的」「F17 成功/失败两条格式串与 `session.go:259-281` **逐字符**同串」；唯一硬伤 = H1（linux 面编译失败）。

### 6.1 逐条处置表

| 编号 | 严重度 | 处置 | 落点 |
|---|---|---|---|
| H1 | 高危 | **已改**：EINTR 注入缝改 `Result<usize, std::io::Error>`（不再碰 errno；`libc::__error` 在 linux 不存在 → ubuntu `--all-targets` 编译失败）。并加跨目标 check 证据 | `serve_cli.rs::read_stop_signal_with` + 测试；验证 = `cargo check --target x86_64-unknown-linux-gnu --all-targets` 通过 |
| M1 | 中 | **已改**：`serve/relay start` 的严格读/写回**先于** `already` 短路（Go `lifecycleStart` 先 `Update` 再判 before，`roleops.go:161-176`）——「在跑 + 坏 config」由 `already` 改为**拒绝** | `unified_cli.rs`（两 op） |
| M2 | 中 | **已改**：握手期限移进**读侧**（`read_head_deadline`/`read_body_buf_deadline` 带期限；握手后 = `None` 纯委托，逐语句同形）——「每 <500ms 1 字节」的慢滴水不再绕过；新增单测 `handshake_deadline_beats_slow_drip`（并钉「断开时刻 ≤2s」防读超时伪造） | `daemon/server.rs` + `daemon/tests.rs` |
| M3 | 中 | **已改**：C13 的 `·学习` 判定集 `static_cands` → **token 序已解析候选**（= Go `cands`）；行文不变，已加「计数输入集」登记行 | `session/mod.rs` C13 块 + `INTEROP-CRITERIA.md` |
| M4 | 中 | **已改**：① `cmd_portfwd` 改 `split_flag` 驱动（删死代码，等号形生效）；② `cmd_speedtest_dispatch` 认 `--token=`；③ `files get/put` 的 `--force/--quiet` 与 `portfwd --no-session-lock` 走 `take_bool_or_exit`（显式值真生效）。E2E 加三条断言 | `main.rs` + `tests/qh_config_failfast.rs` |
| M5 | 中 | **已改**：补 F12 单测（坏 token 记录 → 断言日志行 + `failed/session_not_built`） | `daemon/hosts.rs` tests |
| M6 | 中 | **已改**：抽 `c14_probe_target(&[Candidate])` 纯函数，站点经它取目标；目标选取单测改钉该函数（含「空表 ⇒ None」） | `session/mod.rs` + 测试 |
| L1 | 低 | **已改**：main.rs 六个动词形态的 `--help` 判定由 args[0] → **任意位置**（`wants_help`） | `main.rs` |
| L2 | 低 | **已改**：E2E 启动期测试超时分支先 `kill+wait` 再 panic（不留持锁进程） | `tests/qh_config_failfast.rs` |
| L3 | 低 | **已改**：`unwritable_dir_is_lock_io_with_path` 在 `geteuid()==0` 下跳过（防 CI root 假红） | `session_lock.rs` |
| L4 | 低 | **不改·说明**：F13 的「注释键表逐键比对」按设计门 C6 口径即**静态清单断言**（无反射面）；清单漂移会因 ④ 段的全键可接受断言而暴露 |
| L5 | 低 | **改注释 + 登记**：F11 只保证**新写入 canonical**；盘上遗留旧大写条目不迁移（残余 §5.2-8） | `carriers/mod.rs` 测试注释 + §5.2 |
| L6 | 低 | **登记**：`-o` 的新「下一个是 flag ⇒ 缺值」拒绝面 vs Go（Go 无条件收下一 token）；已把 `-o` 列入判据登记行的取值 flag 清单 | `INTEROP-CRITERIA.md` 取值 flag 行 + §5.2-9 |
| L7 | 低 | **已改**：`serve.ddns.domain` 查**未 trim 原串**（对齐 Go `ContainsAny`；`" x"` 拒） | `serve_cli.rs::validate_file` |
| L8 | 低 | **登记**：`peer_ttl` 接受集窄于 Go `time.ParseDuration`（不收 `300ms`/`1.5h`）——pre-existing、非本批新增拒绝 | §5.2-10 |
| A1 | 记账 | **登记**：控制面 serve 线程的 listener 永久失败 `exit(1)` 属启动期 fail-fast（非 config 输入触发）；本批消灭的是**config 触发**的 exit——grep 门口径按此声明 | §5.2-11 |
| A2 | 记账 | **登记**：role_op 内 `spawn(...).expect("线程创建不可失败")` 沿用既有裁定（§6-10 不做 catch_unwind）；失败只带走该请求答复，非整进程 | §5.2-12 |
| A3 | 记账 | **已改文档**：判据登记行口径订正为「值域错误 = `路径: 字段：值域`（Go 同形）；TOML 语法/类型错误 = toml crate 原文」 | `INTEROP-CRITERIA.md` |
| A4 | 记账 | **登记**：`carriers_cli` 仍用既有 `expand_flag_eq`（等号形入口），其余站点走 `cli_flags::split_flag`——两套并存、行为一致，去重留后续小项 | §5.2-13 |
| A5 | 记账 | **登记**：写回面无 Go `Update` 的 8 次 CAS 重试（最后写者赢）——pre-existing，本批只把写回收进严格表 | §5.2-14 |

> **高危处置复核**：H1 已改并加 CI 等价面证据（linux `--all-targets` check 通过）；M1–M6 全部改码并各有测试/登记；
> L1/L2/L3/L6/L7 改码或登记；其余为登记/说明。**无豁免未登记项**。