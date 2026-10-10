# M7 热修记录：出口 QUIC 面直连 socket 的地址族归一（v0.3.1）

> **性质**：M7 收口后的**部署阻塞级缺陷**热修（U3 现场发现）。改动面 = `homeway-quic` 出口面
> 4 文件（3 改 + 1 注释订正），净 +383/−4 行（含测试）。
> **门记录口径（如实登记）**：本批**未**按「设计棒 + 实现棒 + 两轮 dsh」的整期协议跑——它不在
> 任何期的范围里，是滚动升级 §12 执行中冒出的缺陷。实际执行 = 根因取证（主会话）→ 实现 + 测试
> （子代理）→ **一轮 dsh 外部只读评审**（同时覆盖「修法是否对齐根因 / 有无更简做法」的设计面
> 与「实现/测试/风格」的代码面）→ 逐条处置。⇒ **两道门在本批压缩为一轮，此为显式登记项**。

## 1. 现象与根因

**现象**（2026-10-10 U3 现场，Mac 生产出口 v0.3.0 首次以 QUIC 面起）：
`公共端口明文发送失败（→ 162.159.207.0:3478，20B：Invalid argument (os error 22)）`
⇒ STUN 观测恒失败 ⇒ **公网端点不公布**（token 只剩内网 + 中继腿）。

**根因**：出口 QUIC 面的直连 socket 绑 `[::]`（AF_INET6 双栈），而两处把**裸 IPv4
`SocketAddr`** 交给 `try_send_to`：

| # | 位置 | 目的地来源 |
|---|---|---|
| ① | `ExitSock::send_plain`（STUN 观测出站） | 引擎 `resolve_stun`（刻意只取 v4，对齐 Go `network="ip4"`） |
| ② | `poll_recv` 的明文钩子就地应答 | `pubface` 先 `unmap_v4_in6(src)` 成裸 v4 再回 `PlainOutcome::Reply` |

**Darwin 内核对 AF_INET6 socket + `sockaddr_in` 直接 `EINVAL(22)`**；Linux/OHOS 内核自行转
v4-mapped ⇒ 公开 CI 的 ubuntu+macos 双 runner 里，只有 Darwin 会红（且 M0–M7 的实现棒从未在这台
Mac 上以 QUIC 面跑过**公网端点发布**这条路，故一路未现形）。

**取证**（三条独立证据）：
1. 生产日志（上）；2. 本机最小复现：AF_INET6 socket `send_to` 裸 v4 → `Os { code: 22 }`、
   发 `[::ffff:…]` → 送达；3. 反证：阿里云（Linux）同一路径 `STUN：… 在 stun.cloudflare.com:3478
   眼里是 123.56.218.212:41641` **成功**——平台差异坐实。

## 2. 修法

在**拥有该 fd 的抽象 socket 内**归一（而非在 core 侧按位置打补丁）：

- `ExitSock` 增 `dual: bool`，装配点用 `getsockopt(IPV6_V6ONLY)` 取**运行期**事实
  （v6-only socket 不能 map ⇒ 必须运行期判，且 `IPV6_V6ONLY` 绑后不可变 ⇒ 采样一次安全）；
- 新增 `xmit_addr(dst, dual)`：dual 时 `V4 → V6(v4-mapped)`，**非 dual 一律原样**
  （族不匹配的错误照旧如实上报）；三个发送点（`send_plain` / quinn 直连 transmit / 钩子应答）
  统一经 `tx_addr`；腿路径**不需要**（腿 socket 按远端族建且已 `connect`）；**收侧不归一**
  （unmap 归 core 的 `pubface`）。
- **叶子 crate 纪律**：`homeway-quic` 不得依赖 `homeway-core` ⇒ 就地复刻
  `udpbatch::{is_dual_stack, xmit_addr}`（12 行、语义逐条对齐、双向注释互指）；同 crate 已有
  `FRAME_KIND_QUIC`/`FRAME_MAGIC`/`LEG_RECENT_TTL` 先例。

**为何不是别的位置**（评审者独立评估，结论认同）：并入 core 被叶子纪律排除；即便能拿，钩子回填点
之后仍有 quinn 直连 transmit 要兜底 ⇒ 会变成两处改；改用 `quinn_udp::UdpSocketState` 不成立
（`quinn-udp 0.5.16` 的 `unix.rs:321` 对裸 v4 不做归一，EINVAL 照旧）。

## 3. 代码门（dsh 外部只读评审）

- 评审目录：`/tmp/dsh-review/r1.eOkHE5`（`exit=0`；**易失面**——本文件的 §3 是其结论摘要，按
  评审协议第 4 条不将评审产物入库）。
- 评审者独立做了：① ctypes 直调 libc 复现 EINVAL（与主会话结论同）；② 逐条真跑新增 5 条用例
  （5/5 绿）；③ `clippy -p homeway-quic --all-targets --locked` 干净；④ 跑测试前后
  `git status --porcelain` 恒为同 3 个 ` M`（工作树未被污染）。
- **结论原文摘要**：「修法与根因对齐，位置正确，边界与平台语义保持，**没发现高/中严重度问题**，
  可以合入」——并从「有无更简做法」「漏掉的发送点」「非 dual 语义」「OHOS/Linux 漂移」
  「叶子纪律」「测试是否真钉住」「实现风格」七面各给「看过，没发现问题」。
- **低严重度 8 条（F1–F8）处置**：

| # | 问题 | 处置 |
|---|---|---|
| F1 | 钩子应答失败日志打的是**映射后**地址，与 `send_plain` 失败行口径相反 | **同批改**：另起 `mapped` 变量，日志/归因回到原始 `dst` |
| F2① | core `udpbatch.rs` 注释写 `EAFNOSUPPORT`，实测是 `EINVAL` | **同批改**：订正为实测措辞（纯注释，函数体零改动）+ 反向指针 |
| F2② | 跨 crate 复刻无等价断言（可照 `FRAME_KIND_QUIC` 先例钉） | **登记不做**：要动 island crate 的公开面，性价比不够（语义稳定、12 行） |
| F3 | `dual` 由调用方传参而非 `ExitSock::new` 自取 | **登记不做**：现形态与 `relay/mod.rs` / `udpbatch.rs` 先例一致 |
| F4 | `try_send` 直连分支的归一无用例覆盖 | **登记不做**：该行今日是**防御性 no-op**（quinn 眼里对端恒为 v4-mapped） |
| F5 | 用例 3/4 的 400ms 重试预算是套件里唯一的「紧」预算 | **同批改**：改 `create_io_poller().poll_writable` 落定 + `WAIT` 上界（与用例 5 同形） |
| F6 | `dual` 在排障面不可见 | **同批改**：`Debug for ExitSock` 加字段（判据行零触碰） |
| F7 | 外部线索：某 macOS 版本对 v4-mapped `sendto` 亦异常（正文不可达，未证实） | **登记**：本机实测 mapped 正常；真出现时用例 3/5 会在该 OS 直接变红（可探测） |
| F8 | `normal_races_do_not_trigger_retry_or_gate` 的一次性 flake | **登记不属本批**：与本改动无因果（该例用 v4 回环 ⇒ `dual=false` ⇒ 发送路径与 HEAD 逐字节相同） |

## 4. 证据

- **回退敏感性（实测，非推断）**：把 `tx_addr` 临时短路成恒等后，
  `send_plain_to_bare_v4_dst_is_delivered_from_dual_stack_socket` 报
  `send_plain(→ 127.0.0.1:61844) 失败：Invalid argument (os error 22)`、
  `plain_hook_reply_to_bare_v4_dst_is_delivered_from_dual_stack_socket` 报「8s 内 0 包」
  ——与生产日志同签名；恢复后两条立即转绿（0.06s）。
- `cargo test -p homeway-quic` **216 全绿**（新增 5 条：族矩阵纯函数 / dual 运行期采样 /
  裸 v4 送达 / v4-mapped no-op / 钩子应答裸 v4 送达）；`cargo test -p homeway-core --lib server::`
  217 绿；`cargo clippy -p homeway-quic --all-targets -- -D warnings` 零警告。
- **生产端到端**（v0.3.1 装上线后）：Mac 出口 `STUN：… 眼里是 114.242.60.128:41641` +
  `公网端点：已公布 [114.242.60.128:41641 [2408:…]:41641]`（v4 **与** v6 都恢复）；
  客户端核连本机出口 `probe: ok`、连阿里云出口 `probe: ok`（**钩子应答这条正是修的第二处**）。

## 5. 未做 / 遗留

1. **跨 crate 等价断言**（F2②）——若将来改动 `xmit_addr`，两处复刻有漂移风险（现由双向注释互指）。
2. **`try_send` 分支**（F4）无端到端用例——今日 no-op，若将来出口开始**主动**向裸 v4 目的地发
   transmit（而非应答），该行才变成必需。
3. **F7 的平台线索**——未证实；本机 Darwin 25.5.0 正常。
4. **判据行**：本批零变更（`ExitSock` 的 `Debug` 面非判据行；E-q1/E-q6 未触碰）。
