# WIN 批设计：发布二进制支持 Windows（x86_64-pc-windows-msvc）+ 产物名 homeway

> 批次：WIN（2026-10-13，用户指令）。本文 = 技术评审载体；代码评审记录追加在文末。
> 产物名 homeway 部分已先行落库（同日 commit，登记 = INTEROP-CRITERIA「判据变更记录」
> 2026-10-13 条）；本文主体是 Windows 移植设计。

## 0. 目标与非目标

**目标**：`homeway`（原 `homeway-cli`）在 `x86_64-pc-windows-msvc` 上编译并可用：

- **relay（中继）**：全功能（QUIC + TCP/UDP 转发，纯套接字面）。
- **客户端域**：统一进程（daemon 控制面 + hosts + carriers：socks/forward/speedtest）、
  `connect`/`files`/`portfwd`/`speedtest`/`token` 直连动词。
- **serve（出口）**：QUIC 端点 + 过境拦截（smoltcp 栈 + reactor）+ DNS 代答 + STUN/网卡面。
- **发布面**：release.yml 增 windows-amd64 产物（zip）；ci.yml 增 windows check 门。

**非目标（明确不做，登记在案）**：

1. **term（终端）**：Windows 不支持——`term` 模块/`term` 动词在 windows 上整体不编译/
   报「不支持」退出（用户指令明示豁免）。wire 层 `STREAM[tag=term]` 常量保留（协议面不变，
   出口侧该流槽位在 windows 不装配）。
2. **windows 测试面**：`cargo test` 在 windows 的跑通是后续批（unix 设计的用例面大；
   本批 CI 门 = `cargo check -p homeway-core -p homeway-cli -p homeway-quic` + 发版构建
   + `--version` 烟囱）。测试文件不动（check 档不编译 tests）。
3. **homeway-capi**：OHOS 专用壳，不进 windows 面（check 门 `-p` 清单不含它）。
4. **TUN 全局代理客户端**：客户端无 TUN 面（M5 后服务走 QUIC STREAM，本就无 TUN）——
   非豁免，是现状。

## 1. 决策记录

### D1 新增依赖：`windows-sys`（仅 `cfg(windows)`，零 unix 波及）

唯一新增依赖。微软官方零开销绑定 crate（windows 目标下的「libc 等价物」），features 按面取用
（WinSock/Console/FileSystem/Time/IpHelper/Foundation/Threading）。挂在
`[target.'cfg(windows)'.dependencies]`，unix 构建零变化。理由：msvc 的 `libc` crate 不提供
socket/poll 系符号（实测），而 reactor/网卡/锁/磁盘/时区面绕不开系统 API；自写 extern "C"
声明比引入官方绑定更不可审计。

### D2 IPC：unix = UDS 原样；windows = TCP loopback + 端口文件

daemon 控制面（`control.sock`）与 serve 侧本地服务面（`files.sock`/`speedtest.sock`/
`term.sock`——term 在 windows 不装配）的协议都是**自定义帧 over 字节流**（无 fd 传递、
无 SCM_CREDENTIALS）⇒ 传输层可换。新模块 `daemon/ipc.rs`：

- `IpcListener::bind(sock_path)`：unix = `UnixListener`（现行为逐字节不变，含 SUN_PATH_MAX
  拒绝与 0600/0700 收紧）；windows = `TcpListener::bind(("127.0.0.1",0))` + 端口写入同目录
  `<名>.port` 文件（如 `control.port`），bind 前清死残留。
- `IpcStream::connect(sock_path)`：unix 直连；windows 读端口文件后连 `127.0.0.1:port`。
- `IpcStream` 提供 `Read/Write/try_clone/set_*_timeout/shutdown`——覆盖现消费面签名。

**鉴权面偏离（登记）**：unix 靠 socket 文件属主（0600 + 0700 目录）；windows TCP loopback
无 per-user 隔离（同机任意本地进程可连）。本批接受该偏离（单用户桌面形态的常规面；端口文件
在 state 目录内）。后续批可加握手 token，非本批。

**范围**：homeway-core 侧（daemon/client/server、files_server、speedtest_server、engine、
dns_stream）换 `Ipc*` 类型。homeway-quic 的 `ServiceIntake` UDS 腿（调试面，主入口 = QUIC
STREAM）在 windows **不装配**（quic 是叶子 crate，不能依赖 core 的 ipc 模块；为一条调试腿
复制一份 IPC 不值）。

### D3 客户端三桥（bridge_host）：windows 不装配

三桥（`<filesDir>/bridge/*.sock`）服务 OHOS App 的 capi 面；windows 桌面无 App 消费者
（`files/speedtest/term --host` 远程形态走控制面 `stream.open`，不经桥）。`facade/term_op.rs`
的 `write_auth`/`BRIDGE_AUTH_LEN` 是三桥共用帮手（files/speedtest 也用）——先搬到
`facade/bridge_auth.rs` 再门控 term_op。桥装配在 windows 返回 None/跳过（capi 不构建）。

### D4 fd 模型：`SysIo` cfg 别名 + WSAPoll

reactor（intercept）的 fd 面收口为模块内 cfg 别名：

- unix：`OwnedFd` + `libc::poll`（现状不动）；
- windows：`std::os::windows::io::OwnedSocket` + `WSAPoll`（winsock 的 poll 同形 API；
  兴趣位 POLLIN/POLLOUT/POLLERR/POLLHUP/POLLNVAL 同名；**POLLRDHUP 若在用，windows 臂
  以 POLLIN 近似**——读面会返回 0/错误，语义不丢）。
- 非阻塞拨号：unix `libc::socket/connect(O_NDELAY)`；windows `WSASocketW` +
  `connect`（非阻塞默认，WSAEWOULDBLOCK = 进行中）+ `ioctlsocket(FIONBIO)`。
- 发送面：unix `send(MSG_NOSIGNAL)`（配合 SIGPIPE 默认处置）；windows `send`（无
  SIGPIPE 语义，天然等价）。

`sysfd.rs` 保持 unix 单源不动；windows 面在各消费 crate 就地小帮手（quic 是叶子、不能依赖
core 的帮手模块——重复 ≤50 行，与 libc 的 per-crate 声明同型）。

### D5 锁：flock → LockFileEx（语义同构）

`session_lock`/`nodestate InstanceLock`/`artifact` 的 flock(LOCK_EX|LOCK_NB) 在 windows =
`LockFileEx(LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY, 全文件域)`；
`ERROR_LOCK_VIOLATION` = 「他者持有」（EWOULDBLOCK 同义映射）。锁文件内容（pid/动词）与
判活语义（进程死自动释放）两边一致。

### D6 本地时间：localtime_r → SystemTimeToTzSpecificLocalTime

`go_fmt.rs`（全仓日志前缀）/`artifact`/`tun_exec`/`relay/logfile`（还要 `tm_gmtoff`）/
`daemon_cli` 两处。收口 `core::systime::localtime(secs) -> Tm`：unix = `localtime_r` 原样；
windows = `GetTimeZoneInformation`（bias，含 DST）+ 手推 civil 字段。输出串零变化。

### D7 egress 网卡面：GetAdaptersAddresses + IP_UNICAST_IF

- `interfaces()`：windows = `GetAdaptersAddresses`（AF_INET；OperStatus=up、IfType=回环、
  前缀长→CIDR、IfIndex 直取——无 `if_nametoindex` 失败面 ⇒ `index_ok` 恒真）。
- 钉卡 `pin_socket_to_iface`：windows = `IP_UNICAST_IF`/`IPV6_UNICAST_IF`（按 index，字节序
  = 网序 u32——winsock 特有口径）。darwin 双族单栈容错 / linux 按名两臂不动。
- `is_virtual_iface` 前缀表：windows 加 `vEthernet`/`Loopback`/`Tailscale`/`WSL`/
  `ZeroTier`/`OpenVPN`/`TAP` 形态（unix 表不动——两端 cfg 各自的表，同函数）。

### D8 停止面：信号 → 控制台事件 + 唤醒对

self-pipe 模式（serve/relay/daemon watch 的 pipe + libc::read）在 windows = 回环 TCP 唤醒对
（`WakePair`：bind 127.0.0.1:0 → listen → connect → accept；写端 send 1B，读端阻塞 recv——
与管道同读阻塞语义）+ `SetConsoleCtrlHandler`（CTRL_C/CTRL_CLOSE/BREAK/SHUTDOWN →
置 AtomicBool + 写唤醒字节；handler 跑在系统线程，写 socket 异步安全）。`serve stop`
（控制面）两平台同路。SIGTERM 形态（`taskkill`）不可捕获——服务管理请走控制面/CTRL，
README 注记。

### D9 daemon 化拉起：setsid → DETACHED_PROCESS

unix `pre_exec(setsid)` 臂不动；windows 臂 = `Command::creation_flags(
DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)` + stdio 重定向 spawn.log（权限位调用点
cfg 短路，NTFS 无 mode 位语义）。launchd 托管检测是 darwin 运行时分支（`cfg!` 运行期值），
windows 下按「未托管」路径走——编译面无碍。

### D10 DNS 上游跟随：/etc/resolv.conf → GetNetworkParams

`dnsproxy` 缺省上游 = 系统 nameserver：unix = 解析 /etc/resolv.conf（mtime 1s 节流跟随，
现状不动）；windows = `GetNetworkParams`（iphlpapi 的 FIXED_INFO.DnsServer）+ 周期重读
（60s——网络变化不落文件 mtime，改轮询；`--dns-upstream` 显式给值时两平台都不跟随）。

### D11 权限位（0700/0600）与磁盘量

- 权限位：`#[cfg(unix)]` 模式位调用 + `#[cfg(not(unix))]` 普通建——仓内已有先例形态
  （logfile/hosts/rltoken/unified_cli 四处），本批把 identity/nodestate/artifact/
  server-state/carriers/daemon_cli/files_server/session_lock 等站点补齐同款。windows 上
  目录隔离靠用户 profile 目录边界（state 缺省在 `~/.config/homeway`）。
- `files_server` 的 statvfs 可用字节 → windows `GetDiskFreeSpaceExW`（f_bavail×f_frsize
  同义）；symlink 面（symlink_metadata/is_symlink）本就是 std 跨平台 API，不动。

### D12 quinn/udp 面的 windows 选项

`exit/socket.rs` 的 `IPV6_V6ONLY` getsockopt、`client/relay_sock.rs` 与 `exit/pump.rs` 的
`SO_SNDBUF/SO_RCVBUF`、`udpbatch.rs` 的双栈 bind——全部经 `windows-sys` 的 winsock 同名
sockopt（常量值与 unix 同源 POSIX 面）；句柄取 `as_raw_socket`。`relay/mod.rs` 的 TCP/UDP
缓冲 setsockopt 同路。`tun.rs` 的 fd 读写/poll：windows 臂 = SOCKET + `ioctlsocket(FIONBIO)`
+ `WSAPoll` + `send/recv`（该面仅 App（OHOS）真用；windows 编译面保通、运行面无 fd 来源）。

### D13 errno 分类面

accept/dial 的 `EMFILE/ENFILE/ENOMEM` 与客户端 `ENETUNREACH` 族：libc crate 在 windows-msvc
**有** errno 常量（CRT errno.h 面），`raw_os_error()` 匹配照编；winsock 错误码（WSAE*）数值
上等于 BSD errno 面（10000 偏移由 winsock 自带、`io::Error::from_raw_os_error` 已覆盖）——
unix 臂语义不动，windows 臂按 winsock 值域自然工作，个别形态（EMFILE 罕见）接受降级为
通用错误路径（本就是 best-effort 分类）。

## 2. 不变式与验收

- **unix 行为零变化**：所有 unix 臂代码原样保留（cfg 分支只在 windows 激活）；本地
  `cargo test --workspace` + ci-local 八步门全绿是每 commit 前置。
- **windows 验收**：本地 `cargo check --target x86_64-pc-windows-msvc -p homeway-core
  -p homeway-cli -p homeway-quic` 零错误（ring 交叉面走 tools/cc-check-shim 垫片——
  OHOS 交叉 check 同配方）；CI windows check 门 + 发版构建 + `--version` 烟囱。
- **判据行**：CLI 用法/可行动文案随调用名 homeway 已登记（2026-10-13 条）；windows 专属
  新文案（term 不支持/端口文件）非判据行（新增面）。
- **体积判据不受波及**：OHOS `.so` 判据在 product 档（NDOS strip），windows-sys 只进
  windows 编译单元——`[profile.release]` 未动。

## 3. 风险与登记

| 风险 | 处置 |
|---|---|
| WSAPoll 与 poll(2) 的边角（timeout=0 语义/POLLRDHUP 缺席） | reactor 只用电平触发的 0 超时轮询（timeout=0 即刻返回——winsock 同义）；POLLRDHUP 若在用按 POLLIN 近似，读面 0/错分流兜底 |
| TCP loopback IPC 的本地多用户面 | D2 登记；后续批可加握手 token |
| msvc libc 符号面窄（无 localtime/socket/poll） | 全部走 D1/D4/D6 收口，不散写 |
| `cargo test` 在 windows 未跑（用例面 unix 化） | 非目标登记；windows 测试面另立批 |
| SetConsoleCtrlHandler 与阻塞读的竞态（handler 线程写 socket 时主线程已收线） | 唤醒对生命周期 = 进程级（不提前 drop），写失败即已停止——幂等 |

## 4. 实现注记（收口时补；差异/钉子如实登记）

1. **unix 行为零变化的载体**：`sysock`/`sysio` 的 unix 臂全部 = 原 libc 直调（包一层
   inline 同形函数）；reactor/relay/engine 的 poll 调用点、egress 钉卡、udpbatch 双栈、
   ctlface 读写均经此收口——unix 侧由全量测试（core 682 + quic 222 + cli 43）+ ci-local
   八步门（矩阵 RRR 全过）背书。
2. **windows-sys 0.59 的实测钉**（都已写进代码注释）：
   - `IP_ADAPTER_ADDRESSES_LH` 需要 features `Ndis`+`WinSock` 双开（联合类型才导出）；
   - `FIXED_INFO` 实名 `FIXED_INFO_W2KSP1`；`WSAData` 实名 `WSADATA`；`MAKEWORD`/
     `TIME_ZONE_ID_*` 未导出（字面常量内联）；`WSAPOLLFD` 的字段名是 `fd`；
     `SOCKET`=usize（本仓 RawSock 取 u64=RawSocket 宽度，调用点收窄 cast）。
   - `GetAdaptersAddresses` 的 IfIndex 藏在 `Anonymous1.Anonymous.IfIndex` 双层联合。
3. **唤醒对/停止管道的 syscall 分派**（实测 bug 修复）：unix 的唤醒对是**管道**——
   socket 系 `send/recv` 对 pipe fd 报 ENOTSOCK ⇒ `wake_signal/wake_drain/stopsig`
   （含 handler 主体——r1 评审 H1 抓漏的一处）在 unix 臂走 `write/read(2)`、
   windows 臂（回环 TCP 对）走 `send/recv`。
4. **intake 承载流**：quic 的 `SvcStream`（unix=UnixStream / windows=回环 TCP）跨 crate
   公开，core 侧 `files_server::intake_conn` 归 wrap 成 `IpcStream`（服务本体只见
   IpcStream）。`ServiceIntake::from_listener/with_quic`（UDS 调试腿）= unix-only；
   windows 装配走 `quic_only`（主入口 STREAM 不缺面）。
5. **本地交叉 check 配方**（ring C 面无 MSVC 时的替代）：
   `PATH="$PWD/tools/cc-check-shim/bin:$PATH" CC_x86_64_pc_windows_msvc=clang
   CFLAGS_x86_64_pc_windows_msvc="-nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem
   $PWD/tools/cc-check-shim" cargo check --target x86_64-pc-windows-msvc -p homeway-core
   -p homeway-cli -p homeway-quic --locked`（垫片 = cc-check-shim 的 stdlib.h 补
   `_byteswap_*` + no-op `lib.exe`；与 OHOS/musl 交叉 check 同纪律——check-only）。
6. **代理（棒子）完成面**：IPC 替换（daemon/files/speedtest/engine/dns_stream）、锁
   （syslock）、权限位 cfg 对（identity/nodestate/artifact/state/carriers）、
   `disk_free_bytes`。portfwd `pf_bind` 的 windows 臂**未接线**（运行时半边整体
   not(windows)，无调用点——评审 M1 后删除，后续批接 windows 化 accept 面时重建）。
   代理登记的 8 条风险点中「EACCES 归入 Held」「accept 循环重写」经 r1 复核接受
   （记录 = docs/reviews/WIN.md）。
7. **残留非目标**：windows 测试面（用例 unix 化）；`homeway term` 在 windows 明确报
   「不支持」；`homeway-capi` 不进 windows 面。

## 5. r1 评审后的登记订正与补登（2026-10-13；记录 = docs/reviews/WIN.md）

- **§D3 偏离**：`facade/bridge_auth.rs`（write_auth 搬家）未落地——四桥消费模块
  （bridge_host/files_op/speedtest_op/term_op）整体同门控，搬家无必要；跨平台件
  落 `bridge_io`（trait + 出口虚拟端口），功能等价。
- **§D4 「重复 ≤50 行」订正**：quic `sysio` 实为 ≈250 行同型复刻（SvcStream/pair
  之外与 core::sysock 近乎逐字）；r1 后 `send` 语义已对齐（unix 带
  MSG_NOSIGNAL）。抽第三叶子 crate = 后续批（登记于 WIN.md 待办 ⑦）。
- **风险表补登（§3 增行）**：
  - 每条服务流一对回环 TCP（windows 泵承载）⇒ 每流一个 TIME_WAIT + 动态端口 +
    3 次额外 syscall；高流速率下动态端口耗尽会把该流打成 `reset(0x22)`
    （表现 = 服务不可用）——量级待压测（WIN.md 待办 ④）。
  - windows 臂 close 带未读数据 ⇒ RST（AF_UNIX 无此面）——错误码/日志面差异
    （FIN/EOF 语义不受影响）；pump 头注已改口径。
- **M4 登记**：windows 守护进程**无 daemon 级优雅停止路径**（控制面只有
  serve.\*/relay.\* 动词；taskkill 不带 /f 对无控制台进程无效）——README 已明写；
  `daemon.stop` = 后续批（WIN.md 待办 ⑥）。
- **§4-12（L1 登记）**：三处 unix 失败路径**有意**的可见改善——artifact/timestamp
  失败回 `{secs}`/`unknown-time(secs)`（旧 = 零值 tm 拼出 `19000100-000000` 形态）；
  TUN 非阻塞化失败日志合并为单 errno 文案（旧分 F_GETFL/F_SETFL 两标签）。
- **§4-8（H3 附带）**：linux-gnu 目标的 clippy 是 CI clippy 门的实际目标面，本地
  mac 全绿不代表它绿——复验配方 `cargo clippy --target x86_64-unknown-linux-gnu
  -p homeway-core -- -D warnings`（ring C 面走 cc-check-shim 同款）。
