# M0「骨架与依赖面」设计文档

> 批次：WG → QUIC 传输层换代程序 M0（真源 `docs/QUIC-ROADMAP.md` 的「M0 骨架与依赖面」节）。
> 第 1 棒（设计）产出。**本棒不写产品代码**（实现是第 2 棒）——本文件是第 2 棒的输入契约。
> 基线：工作树 `/Users/zhaozhe/Documents/projects/homeway-rs-quic`（分支 `quic`），
> `HEAD = 082e120`，`git status` 干净（复验时刻 2026-10-08 20:00 前后）。
> **行号均为本 HEAD 实测值**，实现以符号定位为准。
> **范围边界**：本批只做 ①依赖落地 ②QUIC 岛骨架 ③`tools/quic-ab.sh` 转正 ④三基线入册；
> **不改产品行为、不接线、不删 WG 面**。主检出 `~/Documents/projects/homeway-rs`、
> `~/Documents/projects/homeway`、`~/Documents/projects/tier`、`baseline/` 全程只读。
> **状态**：v2（设计门已过 @ `/tmp/dsh-review/r10.25ZI6g/`，2026-10-08——结论、原文摘要、
> 逐条处置与不认同项见 §11；v2 已并入全部高危/中等意见与软条件）。

---

## 0. 复验（证据先行）

### 0.1 工作树与绿基线

| 项 | 实测 | 证据 |
|---|---|---|
| 工作树 | 分支 `quic`，`HEAD 082e120`，`nothing to commit, working tree clean` | `git status` / `git log --oneline -3` |
| `cargo test --workspace` | **exit 0**；12 个测试二进制全 ok；累计 **724 passed / 0 failed / 17 ignored**（`homeway-core` lib 659 passed + 4 ignored，102.85s；其余为 tests/ 与 cli） | `/tmp/m0-baseline-test.log` |
| `cargo clippy --workspace --all-targets -- -D warnings` | **exit 0**（零告警） | `/tmp/m0-baseline-clippy.log` |
| 未跑的门（本棒不可跑） | `tools/check-vocab.sh` / 向量门 / 矩阵冒烟需 `baseline/` 与 `bin/`——**worktree 不带 gitignore 目录**（`baseline/`、`bin/` 在 worktree 中不存在） | 见 §6 的替代取证 |

### 0.2 源码接缝重定位（HEAD 082e120）

| 接缝 | 位置 | 说明 |
|---|---|---|
| `Cmd` enum | `crates/homeway-core/src/wgcore/mod.rs:125` | 成员：`SetOnHint :167`／`UdpOpen :185`／`TunAttach :209`／`TunPacket(Vec<u8>) :215`／`Stop :225` |
| `Client` struct | `wgcore/mod.rs:1069` | `cmd_tx`(std unbounded mpsc) + `wake_wr: Arc<Mutex<Option<i32>>>` + `handle: Mutex<Option<JoinHandle>>` + `snapshot: Arc<Mutex<Snapshot>>` |
| `Client::start` | `wgcore/mod.rs:1086` | 装配 + 起驱动线程 |
| 驱动线程 spawn | `wgcore/mod.rs:1166-1168` | 线程名 **`homeway-wg`** |
| `Client::send`（唤醒） | `wgcore/mod.rs:1187-1196` | `cmd_tx.send` 后向 `wake_wr` 写 1 字节（self-pipe） |
| 驱动循环 | `wgcore/mod.rs:1588` | `poll(2)` on {UDP fd, wake pipe}，超时 = `min(poll_delay, 250ms)` |
| `attach_tun` | `wgcore/mod.rs:901` | 注册 fd（所有权在扩展，引擎**从不 close**）+ 起读线程 `homeway-tun-read`（`:920`） |
| `tun_read_loop` | `wgcore/mod.rs:1764` | 阻塞读 → `Cmd::TunPacket` |
| `stop` / `stop_within` | `wgcore/mod.rs:1518` / `:1540` | 有界版到点 detach + 收割线程 `hw-engine-reap`（`:1561`） |
| `Drop for Client` | `wgcore/mod.rs:1580` | 无界 `stop()` 兜底 |
| `CLIENT_CLOSE_BUDGET` | `wgcore/mod.rs:63` | `Duration::from_secs(2)`（收尾预算，5 处显式调用） |
| `stop_within` 调用点 | `facade/tun_exec.rs:678/:1070/:1125`、`session/mod.rs:741/:1423/:1440` | 世代收尾链 |
| `finish_generation` / `begin_generation` | `facade/tun_shared.rs:140` / `:196` | 世代守卫（幂等、`done`/attach 通道同过守卫） |
| `mark_unhealthy_if_current` | `facade/tun_shared.rs:121` | 带世代守卫的不健康标记（岛收尾要走它） |
| `wait_done` | `facade/tun_shared.rs:177` | 有界等待（返回是否已收尾） |
| `gen_loop` | `facade/tun_exec.rs:878` | 世代线程主体；早段收尾守卫 `EarlyFinish`（`:893-907`） |
| `Client::start` 调用点 | `facade/tun_exec.rs:1100`（生产）／`:745`、`:2136`（测试） | M1 岛构造点的参照位置 |
| `spawn_derived` | `facade/tun_exec.rs:1468` | 派生线程统一 spawn + 失败记行 |
| `patrol_loop` / `Finish` | `facade/tun_exec.rs:1661` / `:1454` | 巡检 / 收尾守卫 |
| **收工顺序（实测）** | `facade/tun_exec.rs:1449-1456` | `pf.stop_all()` → `bridge.stop()` →（`_finish`）`client.stop()` → 缓存终写 → `finish_generation`（§3.5 契约照此同序） |
| `TunExecutor` trait | `facade/mod.rs:138` | `Send + Sync`；M1 起薄封岛的同步面 |
| `lock_unpoison` / `join_bounded` / `log_spawn_failed` | `syncutil.rs` | 线程卫生单源（岛直接复用） |
| `sysfd::pipe_cloexec` | `sysfd.rs` | self-pipe 单源（岛**不再**需要，见 §3.3） |
| `[profile.*]` | **仓内零命中** | ⇒ dev/release 均 `panic = "unwind"`、`lto = false`（见 §3.6） |

### 0.3 复验订正（两处，均不改变结论）

1. **`wg-ring` 臂的 ring 版本口径订正**：`/tmp/quic-lab/results/SUMMARY.md` 与路线文件附录 A 把
   「WG + ring」臂写成 **ring 0.17**；实测该臂 `Cargo.lock` = **ring 0.16.20（crates.io 真版，
   依赖 `cc`/`libc`，走上游 asm）**——0.17 是 QUIC 臂（rustls 侧）的 ring。结论不变
   （两条臂都是上游 asm vs RustCrypto 垫片的对比），但引用时必须写 0.16.20，否则会误导
   「M5 删 WG 后 ring 0.17 的 asm 是否被验过」这类判断。
2. **aws-lc 入图路径订正**：路线文件写「quinn 0.11 默认 features 会拉 `rustls-aws-lc-rs`」；
   实测真正的开关是 **rustls 自身的默认 features（含 `aws_lc_rs` + `prefer-post-quantum`）**；
   quinn 默认 features 拉的是另一串：`platform-verifier`（`rustls-platform-verifier` → jni/openssl-probe）
   与 `bloom`。**对照实验（设计门复核后补测，两组 scratch workspace）**：

   | 配置 | `aws-lc` 计数 | 说明 |
   |---|---|---|
   | A：`quinn = "0.11"`（默认全开）+ `rustls` 关默认（`["ring","std"]`） | **0** | quinn 的 rustls 依赖本身是 `default-features = false`（`quinn/Cargo.toml`、`quinn-proto/Cargo.toml`、`rustls-platform-verifier` 三处同） |
   | B：`quinn` 关默认 + `rustls = "0.23"`（默认全开） | **5**（链 = `aws-lc-sys ← aws-lc-rs ← rustls ← {我方 crate, quinn, quinn-proto, rustls-webpki}`） | 唯一开关在我们自己的 rustls 声明 |

   ⇒ **aws-lc 的唯一开关 = 本仓 `rustls` 声明必须 `default-features = false`**；`quinn` 关默认
   是另一件事（去 `platform-verifier`/`bloom`/`log`）。原稿「少任何一条都会拉 aws-lc」**不成立，已订正**。
3. **CPU 基线的原始证据链订正**（设计门复核发现，影响 §5.2 的「来源」列与越期判据）：
   `/tmp/quic-lab/results/m-*.out`、`m-q1280-*.out`、`q-mtu1400.out` **全部只有 11 字节**
   （`PORT xxxxx`）——复现矩阵的客户端 JSON 只打屏幕、**未落盘**。因此 4.85 / 10.69 / 14.64 / 12.5
   这四个数**只存在于手写的 `results/SUMMARY.md`**；真正留存的原始 CPU 证据是 `peak_probe.sh`
   的客户端日志（`/tmp/pk-*-cli.out`，N=300k）：

   | 臂 | SUMMARY（矩阵口径） | `/tmp/pk-*-cli.out`（N=300k） | 偏差 |
   |---|---|---|---|
   | `wg-ring` | 10.69 | 10.910 | +2.1% |
   | `wg-shim` | 14.64 | 14.835 | +1.3% |
   | `quic` | 12.5 | 13.241（**payload 1162** = MTU1200 口径） | +5.9% |
   | `raw` | 4.85 | 无第二来源 | — |

   ⇒ 两条后果写进设计：①§5.2 的「来源」列按实际证据写；②§4.5 的「±10%」判据带宽**有实测依据**
   （同机同档轮间/口径偏差实测已达 5.9%），且 M0 复测语义 = **以本 harness 实测值重新登记基线，
   旧值只作量级对照**（见 §4.5/§5.2）。

### 0.4 复现命令（可直接跑）

```bash
# 依赖面探针（scratch，非仓内文件）：/tmp/m0-probe —— workspace 形状 = boringtun 0.6 + ring-shim patch
cd /tmp/m0-probe && CARGO_TARGET_DIR=/tmp/m0-probe/target cargo tree -i ring@0.17.14
# 三目标 check（本机 clang 配方；详见 §2）
CC_aarch64_unknown_linux_ohos=clang \
CFLAGS_aarch64_unknown_linux_ohos="-nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem /tmp/m0-shim" \
CARGO_TARGET_DIR=/tmp/m0-probe/target cargo check --target aarch64-unknown-linux-ohos
```

---

## 1. 依赖矩阵

### 1.1 版本与 features（逐条说明「为何要」）

| crate | 版本（本机解析） | 声明 | 为何要这些 features |
|---|---|---|---|
| **quinn** | 0.11.12 | `default-features = false`, `["rustls-ring", "runtime-tokio"]` | `runtime-tokio` = `tokio/time` + `tokio/rt` + `tokio/net`（quinn 的 tokio 后端；我方 runtime 面由它带上）；`rustls-ring` = `dep:rustls` + `proto/rustls-ring`（`rustls?/ring` + `proto/ring`）⇒ 明确选 ring 后端。**默认 features 全关**：`platform-verifier`（拉 `rustls-platform-verifier`→OS 信任库/jni/openssl-probe，M2 走 RPK 钉定，OHOS 上无意义）、`bloom`（服务端 CID bloom 过滤，我方不需要）、`log`（tracing 通道；本仓 logf 自有一切判据行，不引第二条日志通道）。 |
| **rustls** | 0.23.45 | `default-features = false`, `["ring", "std"]` | `ring` = `dep:ring` + `webpki/ring`（AEAD/HKDF/ECDHE/Ed25519 由 ring 0.17 提供；M2 的 RPK 校验也走它）；`std` = `webpki/std` + `pki-types/std` + `once_cell/std`（std 环境必需）。**默认 features 必须关**：默认含 `aws_lc_rs` + `prefer-post-quantum` ⇒ 拉 `aws-lc-sys`（BoringSSL 派生 + cmake，OHOS 不可行）。`logging`/`tls12` **不需要**（QUIC 恒 TLS1.3；logf 自有）——实测最小集 `["ring","std"]` 在 host 与 OHOS 目标下均编译通过（§2.4）。 |
| **tokio** | 1.53.2 | `default-features = false`, `["rt", "time", "sync", "macros"]` | `rt` = `runtime::Builder::new_current_thread` + `block_on`（岛的宿主形态）；`time` = 岛内预算/节拍；`sync` = 命令通道 `tokio::sync::mpsc::unbounded_channel`（§3.3）；**`macros` = `tokio::select!`（驱动循环的 `select!` 由 `cfg_macros!` 门控——实测无 `macros` 时 `error[E0433]: cannot find \`select\` in \`tokio\``；启用它**只为 `select!`**，仍不用 `#[tokio::main]`，单线程不变量不受影响）。**不启** `rt-multi-thread`（单线程是结构不变量，不启用即编译期约束）、`io-util`、`full`。`net` **不直接声明**：岛内不直接用 `tokio::net`/`AsyncFd`（quinn 自管 socket；唤醒走通道而非 self-pipe），该 feature 由 quinn 的 `runtime-tokio` 统一带入；若 M1 真需要 `AsyncFd` 再显式加 `net` 并登记。 |
| **ring** | 0.17.14 | **不直接声明**（间接：`quinn-proto` / `rustls` / `rustls-webpki` 三方引用） | OHOS 实测可编译（`target_os="linux"` + `target_env="ohos"` ⇒ 命中 ring 的 aarch64-linux asm 路径）；**M0 不做 ring 升级/垫片退役**，只确认共存。 |
| boringtun | 0.6.0 | 不变 | WG 路径（M1–M4 作 A/B 对照；M5 删除）。 |
| `[patch.crates-io] ring` | `tools/ring-shim`（0.16.20） | 不变 | 仅供 boringtun；M5 随 WG 删除收口。 |
| **bytes**（M0 不引入） | — | M1 起必需 | `quinn::send_datagram` 只收 `bytes::Bytes`，且 **quinn 不 re-export `bytes`**（`quinn/src/lib.rs` 的 `pub use` 只有 `rustls`/`udp`）⇒ M1 发 DATAGRAM 时须显式声明 `bytes = "1"`。M0 骨架不发 datagram，**本批不引入**（防"声明了不用"）。 |

依赖面**只进新 crate `homeway-quic`**：`homeway-cli` / `homeway-capi` 零新增；`homeway-core`
只加一条 `homeway-quic`（路径依赖）——**这是「禁止 async 泄漏进同步面」的构造性保证**（§3.4）。

### 1.2 Cargo.toml 草案（第 2 棒照此落地）

```toml
# ---- 根 Cargo.toml ----
members = ["crates/homeway-core", "crates/homeway-cli", "crates/homeway-capi", "crates/homeway-quic"]
exclude = ["fuzz", "tools/quic-ab"]   # 原为 ["fuzz"]；探针是独立 workspace，不污染主依赖图

[workspace.dependencies]
quinn = { version = "0.11", default-features = false, features = ["rustls-ring", "runtime-tokio"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
tokio = { version = "1", default-features = false, features = ["rt", "time", "sync", "macros"] }

# ---- 新 crate：crates/homeway-quic/Cargo.toml（岛的宿主）----
# 纪律：本 crate 是**叶子**（leaf-ward）——**不得**依赖 homeway-core（否则环 + 边界失效）
[package]
name = "homeway-quic"
version.workspace = true
edition.workspace = true

[dependencies]
quinn.workspace = true
rustls.workspace = true
tokio.workspace = true
thiserror.workspace = true

# ---- crates/homeway-core/Cargo.toml：[dependencies] 追加（**只此一行**）----
homeway-quic = { path = "../homeway-quic" }
# 注：homeway-core / homeway-cli / homeway-capi 一律**不得**出现 quinn/rustls/tokio 名字
#     ——homeway-core 里写 `use tokio::…` 会直接 E0433（构造性约束，§3.4 层 0）
```

**为何岛单独成 crate**（设计门 1.2/1.3 采纳）：只有把 tokio/quinn/rustls 声明在 `homeway-quic`
而不是 `homeway-core`，「同步面写不出 async 代码」才从**文本纪律**升级为**编译期事实**——消费侧
`homeway-core` 无法命名任何 async 类型（E0433）。代价：①`syncutil` 的 `lock_unpoison` /
`log_spawn_failed` 在岛侧自持（≈25 行，两份极小重复，不引第三个 crate）；②M1 起边界只传 std 类型
（`homeway-core` 的 `Candidate` 在边界上转成岛的裸 `SocketAddrV4`/岛侧 newtype，密钥材料以岛侧
newtype 承接——具体形态留给 M1/M2 设计门，M0 不预置）；③CI 的 `-p` 清单加 `homeway-quic`。

`[patch.crates-io] ring = { path = "tools/ring-shim" }` **保持原样**。

### 1.3 共存结论（双 ring）与实测证据

`/tmp/m0-probe`（= workspace 形状：`boringtun 0.6` + `[patch] ring = tools/ring-shim` + quinn + rustls + tokio）：

```
cargo tree -i ring@0.17.14              cargo tree -i ring@0.16.20
ring v0.17.14                           ring v0.16.20 (/Users/…/homeway-rs-quic/tools/ring-shim)
├── quinn-proto v0.11.19                └── boringtun v0.6.0
├── rustls v0.23.45
└── rustls-webpki v0.103.15
```

- **两个 ring 并存成立**：`Cargo.lock` 双条目（0.16.20 无 `source` 行 = 路径补丁；0.17.14 = registry），
  同一次 `cargo check` 里两者都真正编译过（输出含 `Compiling ring v0.16.20 (…/tools/ring-shim)` 与
  `Compiling ring v0.17.14`）。
- **无 `links` 冲突**：ring 0.17.14 用 `links = "ring_core_0_17_14_"`（**带尾下划线**，`ring-0.17.14/Cargo.toml:18`），
  垫片无 `links` 键 ⇒ 不违反「同名 links 只能出现一次」。（原稿写成 `ring_core_0_17_14` 是笔误，设计门已订正。）
- **无 patch 未使用告警**：cargo 输出零 warning（`[patch]` 被 boringtun 真用到，故不会报
  "Patch … was not used in the crate graph"）。
- **无 `aws-lc`**：最小集下 `grep -c aws-lc Cargo.lock = 0`；对照组（`quinn = "0.11"` + `rustls = "0.23"`
  全默认）`cargo tree -i aws-lc-sys` = `aws-lc-sys v0.45.0 ← aws-lc-rs v1.18.1 ← rustls ← {我们的 crate,
  quinn, quinn-proto, rustls-platform-verifier}`（见 §0.3 订正 2）。

**M0 不做**：不实删 `tools/ring-shim`、不换 boringtun、不改 `[patch]`（M5 收口）。

---

## 2. 交叉编译与 CI 方案（本设计门最硬的未知面）

### 2.1 问题本体（实测复现，非推测）

加真 ring 0.17 后，`cargo check --target <交叉三元组>` **仍会执行 ring 的 build script 并编 C/汇编**
（`cargo check` 只跳过 Rust 后端的 codegen 与链接，不跳过 build script）。实测缺 CC 时的报错：

```
error occurred in cc-rs: command did not execute successfully … : "cc" … "--target=aarch64-unknown-linux-ohos" … -c …/ring-0.17.14/crypto/curve25519/curve25519.c
cargo:warning=…/include/ring-core/check.h:27:11: fatal error: 'assert.h' file not found
```

同时**实测否证了「靠 `.cargo/config.toml` 的 linker 顶替 CC」**：在探针里放了一份与仓内同形的
`.cargo/config.toml`（`linker = <NDK clang>`）后仍失败，命令行还是系统 `cc` ⇒ **`.cargo/config.toml`
的 linker 不被 cc-rs 采用，CC 必须显式设**。

### 2.2 三目标配方（本机实测全绿）

ring 的 `build.rs` 自带一条**上游认可的「无目标 sysroot 交叉」路径**：

```rust
// ring-0.17.14/build.rs:598-606
if (target.arch == WASM32) || (target.os == "linux" && target.env == "musl" && target.arch != X86_64) {
    if compiler.is_like_clang() { c.flag("-nostdlibinc"); c.define("RING_CORE_NOSTDLIBINC", "1"); }
}
```

`RING_CORE_NOSTDLIBINC` 让 ring 用 `__builtin_trap` 与手写 memcpy/memset 替代
`<assert.h>`/`<string.h>`（`include/ring-core/check.h:26`、`crypto/internal.h:360-390`）。据此得到
**统一配方**（`clang` 需能从 PATH 取到；本机 = Apple clang 15，CI = ubuntu-latest 自带 clang）：

```zsh
SHIM="$REPO/tools/cc-check-shim"      # 仓内新增（1 个 15 行头文件，见 2.3）
# ① OHOS：ring 的特例不覆盖 env=ohos ⇒ 显式给同款 flags
CC_aarch64_unknown_linux_ohos=clang
CFLAGS_aarch64_unknown_linux_ohos="-nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem $SHIM"
# ② aarch64-musl：ring 自动加（只需让 cc-rs 认出 clang）
CC_aarch64_unknown_linux_musl=clang
# ③ x86_64-musl：ring 显式排除 x86_64 ⇒ 显式给同款 flags
CC_x86_64_unknown_linux_musl=clang
CFLAGS_x86_64_unknown_linux_musl="-nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem $SHIM"
```

实测结果（`/tmp/m0-probe`，`rust-toolchain.toml` 钉 1.99.0 与仓一致）：

| 目标 | 缺 CC | 本配方 |
|---|---|---|
| `aarch64-unknown-linux-ohos` | ✗ `assert.h not found` | **✓ Finished** |
| `aarch64-unknown-linux-musl` | ✗ `failed to find tool "aarch64-linux-musl-gcc"` | **✓ Finished** |
| `x86_64-unknown-linux-musl` | ✗ `failed to find tool "x86_64-linux-musl-gcc"` | **✓ Finished** |
| `--target aarch64-unknown-linux-ohos` + `CC=<NDK clang>`（真 sysroot） | — | **✓ Finished**（真路径；本轮设计门**独立复现**，复现命令与产物见 §0.4 同款写法，仅把 CC 换成 NDK 包装 clang、**不带** `-nostdlibinc`） |

「真编译过」的物证（不是空壳成功）：三目标的 `ring-*/out/*.o` 是**真 ELF 目标对象**——
`file` 输出 `ELF 64-bit LSB relocatable, ARM aarch64` / `ARM aarch64` / `x86-64`，并各自产出
`libring_core_0_17_14_.a`。

### 2.3 为什么需要一枚 `tools/cc-check-shim/stdlib.h`

x86_64 路径上 clang 自带的 `immintrin.h → xmmintrin.h → mm_malloc.h` 里有 `#include <stdlib.h>`，
`-nostdlibinc` 后无 sysroot ⇒ 报 `'stdlib.h' file not found`。垫片只声明不定义：

```c
/* check-only 垫片：clang 自带 mm_malloc.h 需要 <stdlib.h>；对象永不参与链接（cargo check 门）。 */
#ifndef HW_CHECK_SHIM_STDLIB_H
#define HW_CHECK_SHIM_STDLIB_H
#include <stddef.h>
void *malloc(size_t __size);
void free(void *__ptr);
#endif
```

**纪律**：该垫片与 `-nostdlibinc` 只用于 **check-only 门**；**绝不允许进 `.cargo/config.toml` 的
`[env]`、也绝不允许进任何真实构建路径**（真构建必须用真 toolchain，见 2.5）。

### 2.4 判据语义（如实登记，防「门做假」）

本门的语义 = **「Rust 侧类型/cfg 在三个目标三元组下可编译」+「ring 的 C/汇编前端为该目标真产出
对象」**；**不覆盖链接期与 sysroot 差异**（`cargo check` 不链接）。三个目标的 Rust 侧 cfg 差异
（`target_env="ohos"`/`"musl"`、`target_os="linux"`）都真实参与编译，这是本门的主要价值。

真目标**实构**（链接 + 产物）由两处覆盖，不依赖本门：
1. 本机 `tools/build-app-core.sh`（NDK clang，真 sysroot，`cargo build --release --target aarch64-…-ohos`）；
2. tier `tools/tailcat/build-core.sh`（App 出包路径；只读核对：它**委托**调用 `./tools/build-app-core.sh`
   ——故核侧修 ① 即覆盖 App 出包，无需改 tier）。

**链接期挂门（设计门 3.1 采纳，M0 硬判据）**：本门不覆盖链接，而全仓唯一能发现「OHOS 不可链接」
的路径原先是**手动**跑 `build-app-core.sh`。改造：
- **M0 硬判据**：`tools/build-app-core.sh` 必须出 `.so` 且三道门（符号 20/20、版本注入、体积行）全过，
  产物体积增量入册（§5.1）——写进 §7 判据表与 §10 交付清单；
- `tools/ci-local.sh` 步骤 3 扩为「(a) 三目标 check（含 OHOS）+ (b) **真 OHOS link 构建**：
  调 `tools/build-app-core.sh`；NDK 不在 ⇒ 显式 `SKIP` 并打印档位（fail-loud，不静默跳过）」。

另：`rustc --print cfg --target aarch64-unknown-linux-ohos` 实测 `target_os="linux"` /
`target_env="ohos"` —— 这解释了 ring「0.17.9+ 支持 OHOS」的真实机制（命中 aarch64-linux asm，
OHOS 由 NDK clang 的 sysroot 提供 libc），也说明 **OHOS 不落进 ring 的自动特例**，故必须显式给 flags。

### 2.5 本机既有构建路径的**必修项**（否则直接断）

| 脚本 | 现状 | 必修 |
|---|---|---|
| `tools/build-app-core.sh` | `cargo build --release --target aarch64-unknown-linux-ohos -p homeway-capi`，**未设 CC** | 加 `export CC_aarch64_unknown_linux_ohos="${NDK}/llvm/bin/aarch64-unknown-linux-ohos-clang"`（**不给 `-nostdlibinc`**——真构建走真 sysroot）。这是 M0 最危险的一条：不修 ⇒ App 出包（tier 触点）当场断。 |
| `tools/ci-local.sh` 步骤 3 | `cargo check --target aarch64-unknown-linux-ohos …`，**未设 CC** | 同上导出 CC（NDK 在 ⇒ 真路径；NDK 不在 ⇒ 退到 §2.2 配方并在输出标注档位） |
| `.cargo/config.toml` | 只有 `[target.…ohos] linker` | 加注释说明「linker ≠ CC，cc-rs 不读 linker；CC 由脚本导出或 CI env 给」；**不加 `[env]`**（路径机器相关，且会污染 CI） |
| 根 `.gitignore` | 现含 `/target`、`/baseline`、`/bin`、`*.so`、`*.a`、`.DS_Store`、`/identity`、`fuzz/{corpus,corpus.seeds,artifacts,target,coverage}/` 等（**根 `/target` 只管根目录**） | 加 `/tools/quic-ab/**/target/`（带尾斜杠，避免误伤同名文件）与 `/tools/quic-ab/certs/*.der`（`*.so`/`*.a` 不覆盖 DER）；探针 target 不入库 |

### 2.6 CI（`.github/workflows/ci.yml`）改动

`cross-check` job 的 matrix 三目标保持，step 增加目标相关 env 与两条 fail-closed 断言（`clang` 由
ubuntu-latest 镜像自带；缺则显式装）：

```yaml
      - name: 交叉 check 的 C 侧前置（clang）
        run: which clang || sudo apt-get install -y clang
      - name: 交叉 check（`-p` 清单含新 crate）+ `-nostdlibinc` 落点断言
        run: |
          set -eo pipefail
          cargo check -v --target ${{ matrix.target }} --locked \
            -p homeway-core -p homeway-cli -p homeway-capi -p homeway-quic 2>&1 | tee /tmp/cc.log
          # 设计门 3.2：每条含 -nostdlibinc 的命令行都必须来自 ring（防未来 C 依赖静默继承）
          bad="$(grep -- '-nostdlibinc' /tmp/cc.log | grep -v -- '-ring-' || true)"
          [ -z "$bad" ] || { echo "!! -nostdlibinc 落到非 ring 的 C 依赖上："; echo "$bad"; exit 1; }
        env:
          CC_aarch64_unknown_linux_ohos: clang
          CFLAGS_aarch64_unknown_linux_ohos: -nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem ${{ github.workspace }}/tools/cc-check-shim
          CC_aarch64_unknown_linux_musl: clang
          CC_x86_64_unknown_linux_musl: clang
          CFLAGS_x86_64_unknown_linux_musl: -nostdlibinc -DRING_CORE_NOSTDLIBINC -isystem ${{ github.workspace }}/tools/cc-check-shim
```

**不需要**：DevEco NDK、musl 交叉工具链、任何 apt dev 包、任何大体积下载。新增 job = 源码门
`check-quic-isolation`（§3.4 层 3；含公面 `dangerous()` 断言）。

**被否的备选**（写清理由，防将来重走）：①CI 下载 OpenHarmony 公共 SDK（1–2GB/次、外部依赖不可控、
公开仓不宜）；②apt 装 musl 交叉链（x86_64 有 `musl-tools`，aarch64-musl 无官方包）；
③把所有 C 依赖换纯 Rust（ring 不可替换，且会引入自研密码学风险）。

---

## 3. QUIC 岛结构定稿

### 3.1 模块位置与文件边界（**独立 crate** —— 设计门 1.3 采纳）

```
crates/homeway-quic/            # 新 workspace member（叶子：**不得**依赖 homeway-core，否则成环）
  Cargo.toml                    # quinn / rustls / tokio / thiserror（见 §1.2 草案）
  src/lib.rs                    # 公面 + 契约文档（async 边界 / 生命周期 / 纪律）；只做 `pub use` 重导出
  src/cmd.rs                    # Cmd / IslandReply / IslandErr / IslandSnapshot（**全 std 类型**）
  src/driver.rs                 # 专用线程 + current_thread runtime + 命令循环 + 停止/收割 + `IslandTx`
                                #   （**唯一允许出现 tokio::/quinn::/rustls:: 的文件**——含 `IslandTx` 的私有字段）
  src/sync_util.rs              # 岛侧自持 lock_unpoison / log_spawn_failed（≈25 行；不引第三个 crate）
  src/tests.rs                  # 单测（#[cfg(test)]；走公面）
```

（`IslandTx` 定义放 `driver.rs` 而非 `cmd.rs`，是为了让「唯一含 tokio 类型的文件」可被 grep 门精确断言——
见 §3.4 层 3 第 2 条。）

**为什么不是 `homeway-core/src/quic/`**（原稿形态，设计门 1.2/1.3 指出其不构成构造性防线）：
只有把 quinn/rustls/tokio 声明在 `homeway-quic`，`homeway-core` 里 `use tokio::…` 才会直接
**E0433**——「同步面写不出 async 代码」从文本纪律升级为编译期事实（§3.4 层 0）。
`homeway-core` 侧只在 M1 加一处薄适配（读 `Cmd`/`IslandSnapshot`、装 unhealthy 回调）。

可见性纪律：公面 `pub`（M1 起被 `homeway-core` 消费）；`pub(crate)` 仅用于已被公面调用链触达的件。

### 3.2 宿主形态与「与今日 `wgcore` 单驱动线程同构」说明

| 面 | `wgcore::Client`（今日） | QUIC 岛（M0 定稿） | 同构性 |
|---|---|---|---|
| 线程 | 1 枚专用线程（`homeway-wg`） | 1 枚专用线程（`homeway-quic`） | **同** |
| 驱动 | `poll(2)` on {UDP fd, self-pipe} + 定时（`POLL_CAP=250ms`） | `current_thread` runtime + `select!`（命令 / 定时 / quinn I/O） | 形异神同（唯一允许的差异） |
| 命令通道 | std `mpsc` unbounded + 写 1 字节唤醒 | `tokio::sync::mpsc` unbounded（newtype 包住 sender；**不需要 self-pipe**） | **同**（投递不阻塞、无需回执） |
| 回执 | 每命令一条 `Sender<Result<…>>` | 同 | **同** |
| 状态 | `Arc<Mutex<Snapshot>>` 轮询 | `Arc<Mutex<IslandSnapshot>>` | **同** |
| 停止 | `Stop` 命令 + 有界 join + 到点收割线程 | 同（收割线程名 `hw-quic-reap`） | **同** |
| 阻塞 IO | 驱动线程内 `poll`/`recvfrom` | 岛内**只做非阻塞**（quinn 自管 socket）；TUN fd 的**阻塞读留在专用 std 线程** | **同**（TUN 读线程不在 runtime 内） |
| 向外通知 | `SetOnHint` 回调（驱动线程执行） | `Cmd::SetOnEvent`（同纪律：只允许内存操作/通道投递） | **同** |

**唯一一处机制偏离**：唤醒原语从「self-pipe + `AsyncFd`」改为「tokio unbounded 通道自带的 waker」。
净减的**只有唤醒 fd 一族**不变量（fd 的 open/close 时序、「不得留在 Option 里无人关」、`POLLHUP`
忙转、「可读但通道已排空」的竞态——`wgcore` 的 `stop_within` 收割线程正是为第一条而存在）。
**不是「零收割期资源」**（原稿措辞已订正）：到点 detach 后岛线程照样持有 `current_thread` runtime +
quinn `Endpoint`（内含 UDP fd + 缓冲），与 `wgcore` 的 detach 残余同档——残余清单见 §8.1。
另：「所有 sender 掉光 ⇒ `recv()` 返 `None`」在实践里不是主退出路径（同步面的岛句柄持有 sender），
主路径是 `Cmd::Stop`／stop 位；通道形态的真实收益 = **把 wake 管换成 runtime**、少一类 fd 生命周期面。

机制已实测（`/tmp/m0-probe/tests/island_boundary.rs`）：①runtime 之外可建通道且 `UnboundedSender:
Send + Sync`；②非 runtime 的 std 线程 `send` 能唤醒 `block_on` 里的 `recv`（实测时延 ≈120ms 的注入
睡眠，非轮询）；③sender 全 drop ⇒ `recv()` 返 `None`（收工自然退出）。

### 3.3 命令通道协议

**M0 骨架成员**（本批真落地、真被测；不带未构造变体，避免 dead_code/clippy 面）：

```rust
/// 同步面 → 岛。纪律：投递不阻塞（unbounded）；带 reply 的命令由岛在事件到点时应答。
pub enum Cmd {
    /// L3 直通 attach（M1 起语义 = wgcore::attach_tun：fd 所有权在扩展，岛**从不 close**）。
    TunAttach { fd: i32, mtu: u32, reply: IslandReply<()> },
    /// 应用出站包（热路径，**无 reply**——与 wgcore 同形；队列上限与丢弃计数是 M1 项）。
    TunPacket(Box<[u8]>),
    /// 收工（幂等；重入无害）。
    Stop,
}

pub type IslandReply<T> = std::sync::mpsc::Sender<Result<T, IslandErr>>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IslandErr {
    #[error("岛已收工（通道已断）")]
    EngineGone,          // 岛线程 panic / 退出 ⇒ 同步面绝不挂死（收尾一律映射到这里）
    #[error("隧道面已附加")]
    TunAlreadyAttached,
}
```

```rust
/// M0 = 空占位（只为冻结形态；M1 起增补字段/方法）。岛不发起回调。
pub struct IslandSnapshot {
    pub attached: bool,
    pub packets_in: u64,   // TunPacket 投递计数（丢弃/超限计数是 M1 项）
}
```

**M0 不引入 `IslandEvent`**（无成员的空 enum 只会在用例里空转——原稿三处引用它属形态未定义，
设计门 6.3 已订正）：事件回调面留待 M1 设计门引入时一并定形态。

**M0 不再新增 `Cmd` 成员**——`enum` 未构造变体会触发 `dead_code`（`-D warnings` 下即红），且 M1–M4
的命令面应由各期设计门定稿。下列**声明面**只作为形态契约（各期落地时照此风格增补；新增成员必须
写明「带 reply 与否 + 理由」）：

| 期 | 计划成员 | 关键字段/形态 | 语义 |
|---|---|---|---|
| M1 | `Connect{…,reply}` / `Migrate{…,reply}` / `SetCandidates{…}` | 候选集、预算、`SocketAddrV4`（**std 类型**——`homeway_core::Candidate` 在边界处转换，§3.4 层 0） | 并行赛跑、采纳、`rebind()` 迁移 |
| M1 | `SetOnUnhealthy{h}` | 回调在**岛线程**执行；只允许内存操作/通道投递（`homeway-core` 侧实现 = `mark_unhealthy_if_current(gen, reason)`） | panic 分类**即时**可达（§3.6） |
| M1 | `DatagramRejected{n}`（事件） | 计数 | 超限丢弃**可观测**（不静默——M1 判据） |
| M2/M3 | `SetOnEvent{h}` | 同 `SetOnUnhealthy` 纪律 | 采纳/迁移/断开事件（替代 `SetOnHint`） |
| M3 | `StreamOpen{tag,reply}` / `StreamWrite{id,data,reply}` / `StreamRead` / `StreamClose` | `StreamTag` newtype（1..5）、`StreamId` newtype、`WriteOut`（背压时原样带回，照 wgcore 的 `WriteOut`） | 服务流（files/term/speedtest/dial/probe） |
| M3 | `Probe{reply}` | — | 巡检探活（替代 `path_probe`） |

`Box<[u8]>`（而非 `Vec<u8>`）是**形**上的收窄（长度定稿后不再增长；交 quinn 时可零拷贝进
`bytes::Bytes`），**行**与今日同（wgcore 也是「一次投递一包」）。

### 3.4 「禁止 async 泄漏进同步面」的具体机制

**层 0（构造性，因果层）**：岛 = 独立 crate `homeway-quic`（§3.1）；`homeway-core` 只依赖它、
**不声明** quinn/rustls/tokio ⇒ 同步面里写 `use tokio::…` 直接 **E0433**。更强的一条：**岛公面若
暴露 async 类型，消费侧 `homeway-core` 会编译不过**（无法命名该类型）——泄漏在消费侧自证。
残余窄缝（交代码门逐条过眼）：不命名即传递（pub 类型作泛型/闭包实参）；M0 公面只有
`Cmd`/`IslandTx`/`IslandErr`/`IslandSnapshot`/`Island` 五个类型，逐个核对成本 ≈0。

**层 1（公面类型清单）**：岛公面只出现 std 类型 + 岛自己的 newtype；唯一例外是**私有字段**
`IslandTx(tokio::sync::mpsc::UnboundedSender<Cmd>)`（定义在 `driver.rs`，§3.1）。`IslandTx` 三条封印
契约（写进模块头）：**不实现 `Deref`、不提供取内件的 accessor、`Clone` 返回 `IslandTx`**（防后续提交把封印打开）。

**层 2（可搬运性断言——如实标注它不检出泄漏）**：`fn assert_send_static<T: Send + 'static>()`
对 `Cmd`/`IslandTx`/`IslandErr`/`IslandSnapshot` 各跑一遍 + `IslandTx: Send + Sync`。**定位**：
所有 tokio/quinn 类型都满足 `Send + 'static` ⇒ 本层对「公面夹带」检出率为 0，它保的是「同步面搬得动」。
另加 `IslandErr` 明文契约：变体不得携带非 std 载荷、`source()` 不得返回 quinn/rustls 错误
（M0 两变体均无载荷，趁现在冻结）。

**层 3（源码门，补充 + fail-closed）**：新增 `tools/check-quic-isolation.sh`，断言：
- `quinn`/`rustls`/`tokio` 只出现在 `crates/homeway-quic/Cargo.toml`（`homeway-core`/`homeway-cli`/
  `homeway-capi` 的 manifest **零命中**）；
- `crates/homeway-quic/src/**` 里 `tokio::|quinn|rustls|async fn|\.await` **只在 `driver.rs`**——
  `lib.rs`/`cmd.rs`/`sync_util.rs` 零命中（这条正是设计门 1.2 指出的原稿漏面；`IslandTx` 的私有字段
  因此定义在 `driver.rs`，见 §3.1）；
- 岛内 `std::thread::sleep` 零命中、`block_on` 只在 `driver.rs`；
- `aws-lc` 在 manifest 与 `Cargo.lock` 零命中（防默认 features 回归）；
- **`crates/` 内 `dangerous()` / `with_custom_certificate_verifier` 零命中**（harness 的 `SkipVerify`
  永不得进产品面——设计门 8.1；该命中的唯一合法位置是 `tools/quic-ab/`，且必须带
  `// SECURITY: harness-only` 标记）。

接入 `ci.yml`（新 job `check-quic-isolation`）+ `tools/ci-local.sh` 步骤 3。

**层 4（文档 + 评审）**：模块头写明契约；每期设计门/代码门 checklist 恒含「异步/同步边界」条。

### 3.5 与世代生命周期对接

**M0 = 零接线**（这是本批「行为零改动」的构造性表达）：

- 岛不被任何生产路径构造；出货判据 = `grep -rn "homeway_quic::\|crate::quic" crates/homeway-core crates/homeway-cli crates/homeway-capi`
  为空（`homeway-core` 的依赖表里有 `homeway-quic`，但源码零引用）；
- **第三判据（设计门 4.1 采纳）**：改动前后各跑 `cargo tree --workspace -e features`（存 `/tmp` 两份）
  + `git diff Cargo.lock`，**逐条核对既有 crate 的版本与 feature 集合只增不改**；任何差异必须逐条解释；
- 产物面判据 = OHOS `.so` 体积增量 ≈ 0（未接线的 pub 面被死码消除链式回收；实测值入册，见 §5.1）。

**M1 起的契约**（写死供 M1 设计门复用）：

- 构造点 = `gen_loop`（`facade/tun_exec.rs:878`）内、`Client::start`（`:1100`）之后、attach 之前；
  构造后立即装 `SetOnUnhealthy` 回调（`mark_unhealthy_if_current(gen, reason)`）；
- 停止点 = 世代收尾链，与 `wgcore::Client` **同址**：`patrol` 收尾 / `request_stop` / `Finish::drop`
  三处 `island.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET)`（**量级 = 2s**，与 `wgcore/mod.rs:63`
  同值），**一律在 `finish_generation`（`tun_shared.rs:140`）之前**；
- **收工顺序 = 与今日同址同序**（实测 `facade/tun_exec.rs:1449-1456`）：
  `pf.stop_all()` → `bridge.stop()` → **岛 `stop_within`**（= 今日 `client.stop()` 的位置）
  → 缓存终写 → `finish_generation`。TUN 读线程随岛的收工（通道断/stop 位）自行退出；
- 线程名：`homeway-quic`（驱动）／`hw-quic-reap`（到点收割，镜像 `hw-engine-reap`）；
  spawn 失败走岛侧 `sync_util::log_spawn_failed`（与 `homeway-core` 的记行同文）；
- `Drop for Island` 调无界 `stop()`（镜像 `Drop for Client`）；Drop→stop 链上锁一律 `lock_unpoison`；
- **岛内 spawned task 纪律**：不允许裸 `tokio::spawn` 无监管任务——用 `JoinSet`，并**在收工路径上
  显式 abort + join**（否则收工后仍有任务持 quinn 状态；detach 形态的残余见 §8.1）。

### 3.6 panic 边界（本设计门专项）

**先核实前提**（本棒实测 + 设计门独立复核，双方一致）：

- 根 `Cargo.toml`、三个 member、`fuzz/`、`tools/ring-shim/` **均无 `[profile.*]`**（`grep -n "profile\|panic\|lto\|strip"` 零命中）
  ⇒ dev 与 release 都是默认：**`panic = "unwind"`、`lto = false`**（后者见 §5 的档位错配 R-C）；
- `tools/build-app-core.sh` 未传任何 `-Cpanic=` ⇒ **OHOS cdylib 也是 unwind**；
- tier `tools/tailcat/build-core.sh`（只读核对）无 panic/profile/CC 面，且**委托**调用
  `./tools/build-app-core.sh` ⇒ §2.5 只修核侧脚本即覆盖 App 出包路径；
- 仓内既有先例互证：`term/service.rs:351` 的注释已写「全链无 `panic = "abort"`」；
- 实验台 `/tmp/quic-lab` 用的是 `panic = "abort" + lto` 的**自己的 profile**（harness ≠ 产品）。

由此定稿六条：

1. **岛线程体套 `catch_unwind`，就地分类（采纳仓内先例——原稿「不用 catch_unwind、它只在 FFI 边界」
   与仓内事实相反，已订正）**：`facade/tun_exec.rs:1402/1481`（`spawn_derived`：巡检/pusher/stats）、
   `facade/service_exec.rs:219`、`term/service.rs:354` 都对线程体套 `catch_unwind(AssertUnwindSafe(…))`，
   panic 落日志 + `mark_unhealthy("panic")`（Go `recover` 同义）——因为 `unhealthyReason` 的取值集
   `{patrol, fd, panic, stop}` 是判据语义，**`panic` 分类必须可达**。岛照同办：驱动循环体套
   `catch_unwind`，`Err` ⇒ 就地记行 + 经 `SetOnUnhealthy` 回调 `mark_unhealthy_if_current(gen,"panic")`
   ⇒ **退出循环**（不复用该线程：runtime/连接状态已不可信）。
2. **同步面绝不因岛死亡而挂死**（覆盖清单写全）：①reply 取回 `rx.recv().map_err(|_| IslandErr::EngineGone)`
   （岛线程 panic 时栈上 reply sender 被 drop ⇒ `RecvError` ⇒ 立刻归错；**禁止 `unwrap()`**）；
   ②快照轮询无阻塞；③`stop_within` 有界；④**TUN 读线程**：阻塞读点靠 `cmd_tx.send(...).is_err() ⇒ return`
   自行退出（镜像 `wgcore/mod.rs:1876` 的既有分支；tokio `UnboundedSender::send` 在 receiver drop 后同样 Err）。
3. **join 结果必须检查**（兜底面）：`stop`/`stop_within` 里 `h.join()` 的 `Err` = 岛线程 panic ⇒
   记行 `quic: 岛线程 panic（{msg}）—— 本世代 QUIC 面已死` + 再走一次 `mark_unhealthy_if_current`。
   **口径精化（设计门 2.5）**：`wgcore` 现版在 `:1524`/`:1551`/`:1564` 三处 `let _ = h.join();`，
   吞掉的是 **`Err` 分支 ⇒ 不记行、不置 `unhealthyReason=panic`**（panic hook 仍会往 stderr 打，
   但 App/cdylib 内通常无落点）；岛不复刻该形态。
4. **谁在哪个线程记 panic 行**（写死，防测试与排障两处落空）：预算内收工 ⇒ `stop_within` 的 join
   分支记；到点 detach ⇒ 收割线程 `hw-quic-reap` 的 join 记；**即时分类**已由第 1 条给出（不依赖 join）。
5. **跨仓约束登记**：若 tier 侧将来给核加 `panic = "abort"`（或外部 `RUSTFLAGS=-Cpanic=abort`），
   第 1–3 条全部失效（一次岛内 panic = App 数据面整体死）⇒ 写入 M0 交付说明，M7 tier 触点复核。
6. **不允许在 async 上下文做阻塞 syscall**：TUN fd 读留在专用 std 线程（`homeway-tun-read` 同形），
   岛内只做非阻塞；分配失败 = abort（`syncutil` 已登记的 carve-out，岛不另设处置）。

### 3.7 M0 骨架的 dead-code 策略

- 骨架的**公面 `pub`**（crate 根 `lib.rs` 的 `pub` 面）⇒ 未接线不触发 `dead_code`；
- 骨架的私有件必须在非 test 构建下也被公面调用链触达（**不许出现"只有测试才用"的 `pub(crate)`**）；
- **测试缝形式定死（设计门 6.2）**：挂起/panic 注入缝一律用 **`#[cfg(test)]` 门控**（写在 `driver.rs`
  内的 `#[cfg(test)]` 项，`tests.rs` 经 `super::` 触达）——`cargo test` 下编译、release/cdylib 里完全消失
  ⇒ 零 `dead_code`；**不用** cargo feature（`test-seams` 会经 CLI 透传成生产可注入面，代价大于收益）；
  若实现中发现必须用 feature，须在代码门登记并说明理由；
- **不用 `#[allow(dead_code)]` 静默**（仓内先例：`nodestate.rs:45`、`capi/lib.rs:366` 都带理由）；
  确需时附理由 + 在代码门登记；
- 依赖面「真活着」的活体证据（单测，非生产路径）：
  `dep_face_alive_quinn_client_endpoint` —— 在 `current_thread` runtime 上下文里
  `quinn::Endpoint::client("127.0.0.1:0")` + 读回 `local_addr()` 后 drop（**不建连接、不做身份**）；
  它证明 quinn/tokio/quinn-udp 在真机器上可用，而不是"只出现在 `Cargo.lock` 里"。端口 = 0（内核实
  分配）⇒ 无固定端口 flake。

---

## 4. `tools/quic-ab.sh` 转正设计

### 4.1 形态与纪律（对齐 `tools/qi-ab.sh` 先例）

- `#!/bin/zsh`；`set -uo pipefail`；`REPO_ROOT="${0:h:A:h}"`；产物落
  `${QUIC_AB_DIR:-/tmp/quic-ab/$(date +%Y%m%d-%H%M%S)}`；`trap cleanup EXIT INT TERM`（清进程）；
  loadavg 1Hz 带时间戳落盘（`loadavg.tsv`）+ 轮首/轮末标记；轮序平衡（`--rounds 3 ⇒ A,B,B,A,A,B`）；
  逐轮产物 + `summary.txt` + `bins.sha256`。
- **与 qi-ab 的关键差别（写清）**：本 harness **完全不碰 `homeway-cli` 与任何生产实例**——四臂都是
  独立探针二进制、只绑 `127.0.0.1:0`，无 state 目录、无出口、无 token。因此不需要臂切换原子替换、
  不需要 wipe_session。
- **不接 CI**（四臂全量重建成本高）；M0 由实现棒手动跑留证。是否 CI 化（smoke 档）列为 M1 候选。

### 4.2 子命令

| 子命令 | 作用 | 关键开关 |
|---|---|---|
| `cpu` | 三臂每包 CPU 矩阵（默认 `raw,wg-shim,quic`） | `--arms a,b,c`（含 `wg-ring` 诊断臂）、`--payload 1280`、`--n 60000`、`--rounds 3`、`--mtu 1400`、`--profile lab\|product` |
| `overhead` | 线开销（WG 线上字节 + QUIC oneway 服务端 `udp_rx` 口径） | `--n 60000`、`--mtu 1400` |
| `size` | OHOS cdylib 体积矩阵（4 档） | `--profile lab,product`（默认两档都跑） |
| `mem` | footprint 采样 | `--mode steady\|load\|conns`、`--rounds 3`、`--conns 5` |
| `all` | 顺序跑 `cpu → overhead → size → mem` | 透传上述 |

### 4.3 与 `/tmp/quic-lab/` 的逐项口径映射表

| lab 资产 | quic-ab 落点 | 口径（逐项照搬，勿漂） | 产物 |
|---|---|---|---|
| `run_matrix.sh`（三臂 ×3 轮交替） | `cpu` | `N=60000`；`PAYLOAD=1280`；**QUIC 臂 `MTU=1400`**（lab 矩阵缺省 1200 ⇒ 载荷被压到 `max_datagram_size=1162`——转正须与 WG 臂同载荷）；每轮先起 server、读 `PORT`、再跑 client；`pkill` 收工 | `cpu-<arm>-r<N>.json`（**本批新增：lab 的 client JSON 只打屏幕未落盘**）、`srv-<arm>-r<N>.out` |
| `quic/src/bin/quic.rs`（含 `--oneway`、`IDLE_SECS`） | `overhead` / `mem` | `--oneway` = 服务端不回显、客户端 `send_datagram_wait`；线上字节 = 服务端 `conn.stats().udp_rx.bytes/datagrams`；收尾 `sleep 1500ms` 等 ACK 窗 | `oneway-<mtu>.json` |
| `fp_probe.sh` | `mem --mode steady` | 每轮：起臂 → 暖机 1.2s → `IDLE_SECS=9` hold → 等 `IDLE 模式` 行 → 采样 8 次 `vmmap -summary` 的 **Physical footprint** → 取中位；三轮再取中位 | `fp-<arm>.med`（K） |
| `peak_probe.sh` | `mem --mode load` | **`N=300000`**（lab 的负载态数字实测出自 300k——脚本默认 200k 被覆盖，`/tmp/pk-*-cli.out` 的 `"pkts":300000` 为证；转正后默认取 300k 与基线同口径）；每 250ms 采 `Physical footprint` 与 `(peak)`；取 max。**该档只登记绝对值 + 抖动，不设 ±10% 硬判**（max 受采样相位影响） | `peak-<arm>.txt` |
| `rss_probe2.sh` | `mem --mode rss`（**诊断档，不作判据**） | `ps -o rss=` 16 点中位；**登记为非判据口径**（lab 已证同机两臂差 4.3MB 而二进制差 176B） | `rss-<arm>.med` |
| `multiconn`（多连接标定） | `mem --mode conns` | 客户端 N=1,3,5 点拟合 base+每连接边际 | `conns-fit.txt` |
| `size-probe2` + `q.log`（成功构建日志；**`quic.log` 是失败调用**：`error: unexpected argument '--features quic' found`，不得引） | `size` | 4 档：空壳 cdylib／+QUIC 全栈（死码消除）／+真实引用／现役 `.so` 对照；`--features quic` 开关；OHOS 目标 + NDK `llvm-strip` | `size-matrix.txt` |
| `certs/gen_certs.sh` + `cert.der/key.der` | 转入 `tools/quic-ab/certs/`（**只转生成脚本，DER 现场生成不入库**，`.gitignore` 加 `/tools/quic-ab/certs/*.der`） | `openssl req -x509 … -days 3650`（实验用自签；产品走 M2 的 RPK 钉定）。**安全边界（设计门 8.1）**：探针里的 `.dangerous().with_custom_certificate_verifier(Arc::new(SkipVerify))`（lab 的 `quic.rs:177`、`multiconn.rs:57`、`size-probe2` 的 cdylib 与 bin 各一处）**只允许存在于 `tools/quic-ab/`**，且每处带 `// SECURITY: harness-only` 标记；`crates/` 内零命中由 `check-quic-isolation.sh` 兜（§3.4 层 3）；M2 设计门把「SkipVerify → RPK 钉定校验」列为门的一项 | `certs/` |
| `results/SUMMARY.md` | 口径文档 → `tools/quic-ab/README.md` | 四臂表 + 口径说明 + 未测项（空口/GSO/DPI/多连接）搬迁，**搬迁时必须套用 §0.3 的三条订正**（尤其 README「真 ring 0.17」那句会重新写错：wg-ring 臂实为 ring 0.16.20）；另写明「中位 = **下中位**」（lab 脚本 `a[int((NR+1)/2)]` 在偶数点取下中位——照搬，勿当 bug 修） | 入库 |

### 4.4 探针入库形态（**独立 workspace，别污染主依赖图**）

```
tools/quic-ab.sh                 # 唯一入口（zsh；与 qi-ab.sh/perf-ab.sh 同层）
tools/quic-ab/
  README.md                      # 口径 + 复现判据 + 未测项（源自 lab README/SUMMARY）
  certs/gen_certs.sh             # 现场生成自签（DER 不入库；*.der 已在 .gitignore 的 *.so/*.a 之外的注意项：显式加 /tools/quic-ab/certs/*.der）
  arms/                          # ← 独立 workspace ①（name = quic-ab-arms）
    Cargo.toml [workspace] members = ["common","raw","wg-ring","quic","size"]
    common/   # ippkt（IPv4 包构造）+ rusage CPU 采样 + JSON 行输出（三臂共用）
    raw/      # 地板臂（纯 UDP 往返）
    wg-ring/  # boringtun + 真 ring 0.16.20（上游 asm；诊断臂）
    quic/     # quinn 0.11 + rustls(ring) + tokio（DATAGRAM 臂 + multiconn）
    size/     # cdylib 体积探针（lib + real_quic bin，`--features quic`）
  wg-shim/                       # ← 独立 workspace ②（必须独立：[patch] 是 workspace 级，与 arms 冲突）
    Cargo.toml [patch.crates-io] ring = { path = "../../tools/ring-shim" }
```

- 两个 workspace 的 `Cargo.lock` **入库**（可复现）；根 `Cargo.toml` 的 `exclude` 加 `"tools/quic-ab"`
  （**必需**：否则 arms 里的 `wg-ring` 会被根 `[patch]` 换成垫片，"真 ring 诊断臂"当场失真）。
- lib 源码全部**从 lab 转入**（`raw 94` / `wg 190` / `quic 309` / `multiconn 123` / `ippkt 23` /
  `size lib 143` / `real_quic 116` 行，共约 1.2k 行）——只做三处改动：①`MTU` 默认 1400（与 WG 臂同载荷）；
  ②client JSON 落盘；③把 `quic` 臂的 `certs` 相对路径改成仓内路径。**不重写、不美化**（口径可比优先）。
- **profile 分档（设计门 5.1/5.2 采纳，落点必须写对）**：cargo **忽略非 workspace 根的 `[profile]`**
  （只打一条 warning）⇒
  - **lab 档**：`[profile.release] opt-level=3, lto=true, codegen-units=1, panic="abort", strip=true`
    **写在 `arms/Cargo.toml`（workspace 根）**；`wg-shim/`（自己的 workspace 根）自带一份同款；
  - **product 档**：跟随仓内 workspace 默认 release（无 LTO / unwind / cgu=16）+ NDK `llvm-strip`。
    产生机制（不能只说"另跑一档"）：在 `arms/Cargo.toml` 定义
    `[profile.product] inherits = "release"`（显式 `lto=false`、`panic="unwind"`、`codegen-units=16`、
    `strip="none"`），用 `cargo build --profile product` 构建；**构建后断言本次档位**（`cargo metadata`
    读 profile 值 + 记录到 `summary.txt`），并对同一份源码比"两档 `.so` 尺寸差"防静默失效；
  - 自检：构建日志不得出现 `profiles for the non root package will be ignored`。
- **中位口径**：lab 脚本的"中位"在偶数采样点取的是**下中位**（`a[int((NR+1)/2)]`）——照搬，勿当 bug 修。

### 4.5 一键复现判据（附录 A 数字 ±10%）

| 指标 | 附录 A 目标值 | 判据 | 允许的差异来源 |
|---|---|---|---|
| 每包 CPU（raw / wg-shim / quic） | 4.85 / 14.64 / 12.5 µs（**1280B 载荷 = MTU1400 口径**，lab 档） | 各臂 ±10% | 机器代际（M2 系列）、loadavg（用每包 CPU 而非墙钟，见 PERF-AB §9.15.1）、**同机同档实测偏差已达 1.3–5.9%**（§0.3 订正 3 的两套独立测量对比）⇒ ±10% 是实测带宽而非冗余 |
| 线开销 | WG 32B / QUIC 30.16B（MTU1400 + 1280B 载荷） | ±10% | 协议实现不变 ⇒ 应逐字节同 |
| `max_datagram_size` | MTU1200→1162 / MTU1400→1362 | 精确（打印值） | 无 |
| footprint（steady） | raw 944K / wg-shim 1008K / quic 1216K | ±10% | 系统版本页账；QUIC 稳态有 16K 级轮间抖动（§5.3） |
| 体积（**lab 档三格**） | 323,632 / 778,272 / 1,825,088 B | ±10% | profile（见 §4.4） |
| 体积（**product 档两格**） | 现役 2,213,744 B + M0 增量 | **只登记，不设判据** | 档位不同（无 LTO/unwind），与 lab 档不可比 —— 门槛判定按 product 档（§7） |

**复测语义（设计门 7.2 采纳）**：M0 的 `quic-ab.sh` 实测结果 = **重新登记基线**（写入
`docs/QUIC-BASELINE.md`）；附录 A 的旧值只作**量级对照**（±10% 是"证明口径没漂"的松门，
不是"必须复现旧数"的紧门）——因为附录 A 的 CPU 原始证据只有手抄 `SUMMARY.md`（见 §0.3 订正 3）。

前置：`rustup target add aarch64-unknown-linux-ohos`（仅 `size`）；`openssl`（certs）；NDK（`size`）；
**`size` 子命令必须显式用 NDK 包装 clang**：`CC_aarch64_unknown_linux_ohos="${NDK}/llvm/bin/aarch64-unknown-linux-ohos-clang"`
**且断言 `CFLAGS_aarch64_unknown_linux_ohos` 不含 `-nostdlibinc`**（fail-closed，设计门 3.5）——
`size` 是**真链接**路径，抄 §2.2 的 check 档 flags 会产出「无 libc 的 ring 对象」再链接失败
（或在更坏的情形下静默成一锅杂烩）。

---

## 5. 三基线登记表（M0 交付物 ④：入册）

> 落点建议：新建 `docs/QUIC-BASELINE.md`（三张表 + 出处 + 复测命令），`docs/QUIC-ROADMAP.md`
> 的 M0 小节与「下一步指针」加一行指针。数字口径见每行的「复测」列。

### 5.1 体积基线

| 项 | 值 | 日期 | 来源 | 本期复测 |
|---|---|---|---|---|
| 现役 `libclientcore.so`（生产档：默认 release + NDK strip） | **2,213,744 B**（sha256 `03177a91…ebe807`） | 2026-10-07 12:26 构建 | 主检出 `target/aarch64-unknown-linux-ohos/release/libclientcore.so`（本轮复验重算大小与 hash 一致） | M0 实现后跑 `tools/build-app-core.sh` 取新值，登记**增量**（预期 ≈0，未接线） |
| 空壳 cdylib | 323,632 B | 2026-10-08 | lab `size-probe2`（lab 档；**该档产物已被后续构建覆盖，文件系统无留存，只有构建日志 `q.log`**） | `tools/quic-ab.sh size` |
| 空壳 + QUIC 全栈（死码消除后） | 778,272 B | 同上 | 同上（**同上，无留存产物**） | 同上 |
| + 真实引用全路径 | 1,825,088 B | 同上 | lab `size-probe2/target/…/libclientcore2.so`（**唯一有留存产物的一档**；本轮设计门复称复核一致） | 同上 |
| 预算 | ≤ 3.8MB（M5 判据；= 现役 + QUIC ≈1.5MB − 删除面，**按 product 档判**） | — | 路线文件 | M5 |

（设计门 7.5 订正：lab 三档里只有第三档有物证；`size-probe2/quic.log` 是一次**失败调用**
`error: unexpected argument '--features quic' found`，不得作为任何数字的来源。⇒ 四档数字由
M0 的 `quic-ab.sh size` **实测重出**后写入 `docs/QUIC-BASELINE.md`。）

### 5.2 每包 CPU 基线（µs/往返包，1280B 载荷，lab 档 release+LTO+abort+strip，Mac M2）

| 臂 | 值 | 相对地板 | 说明 |
|---|---|---|---|
| `raw`（纯 UDP 往返） | **4.85** | 1.00× | 地板 |
| `wg-ring`（boringtun + **真 ring 0.16.20** asm） | **10.69** | 2.20× | 诊断臂（见 §0.3 订正 1） |
| `wg-shim`（boringtun + `tools/ring-shim` RustCrypto） | **14.64** | 3.02× | **现役手机形态** |
| `quic`（quinn 0.11 + rustls(ring 0.17) + tokio，DATAGRAM） | **12.5** | 2.58× | M1 门槛参照：≤ 现役 WG+shim ×1.0 |

**来源（已按设计门 7.2 订正）**：`/tmp/quic-lab/results/SUMMARY.md` §1（**手抄汇总**——矩阵的客户端
JSON 只打屏幕未落盘，`m-*.out` 全部只有 11 字节的 `PORT` 行）；第二份独立测量 =
`/tmp/pk-*-cli.out`（`peak_probe.sh`，N=300k：wg-ring 10.910 / wg-shim 14.835 / quic 13.241〔1162B 载荷〕
——与手抄值差 1.3–5.9%，见 §0.3 订正 3）。`raw` 的 4.85 无第二来源。
M0 复测 = `tools/quic-ab.sh cpu`（**以实测值重新登记基线**，旧值作量级对照）。

### 5.3 内存基线（`vmmap` Physical footprint，三轮中位）

| 场景 | raw | wg-shim | quic | 来源 |
|---|---|---|---|---|
| 稳态（单连接 hold） | **944K** | **1008K** | **1216K** | `fp-{raw,wgshim,quic}.med` 三轮值 = 944/944/944、1008/1008/1008、**1216/1216/1232**（中位仍 1216，第三轮是 16K 离群——故 §4.5 的 ±10% 对这一格是必要的而非冗余） |
| 负载态（N=300k 传输中 max） | — | 1008K | 1264K | `peak_probe.sh`（N=300k，见 §4.3） |
| 每连接边际 | — | 11,040 B/peer（boringtun 结构体） | ≈37.6K/连接（服务端 5 点拟合 base≈1399K） | `multiconn` |

**口径登记**：`ps -o rss=` **不作判据**（同机两臂差 4.3MB 而二进制差 176B——lab 已证该口径不可用）；
判据一律 `vmmap` physical footprint。M1 门槛：单连接 ≤ +256K、每设备 ≤ 64K、32 设备 ≤ +2MB。

---

## 6. 判据行影响：**零变更**（含实测确认）

**结论**：本批不产出、不修改任何判据行（`docs/INTEROP-CRITERIA.md` 的 C/E/X/DC/CA 家族一行不动），
**无需登记**。理由与取证：

1. M0 不改任何产品行为（§3.5 零接线 + §2 只加 check 门的 env）⇒ 结构性判据面为空集。
2. **词表门（`tools/check-vocab.sh`）零影响**，逐条核对：
   - 门① 的 ledger 锚定：`docs/BASELINE.md` 锚定值 `0ae1d104ec1eeb546d61d7352052fccb062d654232e17f0b03adbe4323c2469f`，
     与主检出 `baseline/homeway/contracts/ledger.jsonl` **实测 sha256 逐字符相同**（本轮复核）⇒ 输入未漂移；
   - 门② 的 Rust 声明集来自 `crates/homeway-core/tests/vocab_dump.rs` 的编译期 dump（5 个 unit：
     `speedtest-reason/reason`、`portfwd/err`、`event-payload/via`、`event-payload/state`、
     `files-proto/code`）——M0 不新增/不改这些 unit 的任何值，也不动该测试文件；
   - 门③ tier manifest（`../tier/tools/gen/vocab-manifest.json`，只读）不动；
   - 结论：门的四个输入（ledger / dump / manifest / BASELINE 锚）与本批改动面**无交集**。
3. **本棒无法在本 worktree 完整跑该门**（如实登记）：worktree 不带 gitignore 目录 ⇒ 无 `baseline/`、
   无 `bin/`。⇒ **第 2 棒必须在主检出（合并后）跑 `zsh tools/check-vocab.sh` 并把 PASS 输出贴进
   `docs/reviews/M0.md`**；同时 `tools/ci-local.sh` 的步骤 1/4/5/7 也只能在主检出跑
   （这条对 M0 收口节奏有影响，见 §10 的「收口顺序」）。
4. 本批新增的 `CC_*`/`CFLAGS_*` 是**构建环境**，不进任何判据行、不进 `fixtures/`。

---

## 7. 预算与可测性

| 维度 | 门槛（来源） | M0 登记值 | 测法（可测性） | 判定期 |
|---|---|---|---|---|
| 体积（product 档） | ≤ 3.8MB（路线文件 M5） | 现役 2,213,744 B | `tools/quic-ab.sh size`（**product 档** + NDK strip）+ `tools/build-app-core.sh` 的 `[size]` 行 | M0 登记 / M5 终值 |
| 体积（M0 增量） | ≈0（未接线 ⇒ 死码消除） | 待测 | `tools/build-app-core.sh`（**M0 硬判据**：出 `.so` + 三道门过 + 体积增量入册）；**若 > +64KB（+3%）⇒ 视为"骨架被链接进产物"，必须在 M0 收口记录里解释** | M0 |
| 体积（lab 档三格） | 复现附录 A ±10%（仅口径校验） | 323,632 / 778,272 / 1,825,088 B | `quic-ab.sh size --profile lab` | M0 |
| 性能 | 每包 CPU ≤ 现役 WG+shim ×1.0（M1） | 14.64（现役）/12.5（QUIC） | `quic-ab.sh cpu`（每包 CPU，非墙钟；≥3 轮交替） | M1 |
| 线开销 | ≤ 40B/包（M1） | WG 32B / QUIC 30.16B | `quic-ab.sh overhead`（服务端 `udp_rx` 口径） | M1 |
| 内存 | 单连接 ≤ +256K；每设备 ≤64K；32 设备 ≤+2MB（M1/M6） | 稳态 944/1008/1216K；每连接 srv 37.6K | `quic-ab.sh mem`（footprint 三轮中位；`ps RSS` 不作判据） | M1/M6 |
| 复现性 | 附录 A 量级 ±10%（M0 判据；语义 = 口径不漂，非"必须复现旧数"） | 见 §4.5 表 | `quic-ab.sh` 一键（`cpu/overhead/mem/size`） | M0 |
| **OHOS 真链接** | **M0 硬判据**（设计门 3.1：唯一能挡「OHOS 不可构建」的门） | 现役 `.so` 2,213,744 B | `tools/build-app-core.sh`（NDK 真 sysroot）；`ci-local.sh` 步骤 3 同跑，NDK 缺 ⇒ 显式 SKIP | M0 |
| 测试/静态门 | `cargo test --workspace` 全绿、clippy 0、三目标 check 全绿（M0 判据） | 724 passed/17 ignored；clippy exit 0 | 仓内命令 + §2 配方 + `check-quic-isolation.sh` | M0 |

---

## 8. 风险与未决

### 8.1 附录 D 风险 1（tokio 进核的线程模型/panic 与世代生命周期冲突）——正面回答

| 子风险 | 本设计的处置 | 残余 |
|---|---|---|
| 线程模型冲突（多线程 runtime 与「世代 = 一条生命线」不符） | **单线程 `current_thread`**：岛只有一枚线程，`rt-multi-thread` feature **不启用**（编译期约束）；阻塞面（TUN 读）留在既有 std 线程，岛内只做非阻塞 | 无（结构决定） |
| 岛内 task 逃逸出世代生命周期 | 岛内禁裸 `tokio::spawn`；`JoinSet` + 收工显式 abort+join（§3.5） | 待 M1 代码门核 |
| panic 穿透杀进程 | 实测 profile = unwind ⇒ 只死岛线程；岛内 `catch_unwind` 就地分类（§3.6-1）；`capi` 的 FFI `guard()` 仍在最外层兜底 | **若 tier 加 `panic=abort` 则失效** ⇒ 跨仓约束登记（§3.6-5） |
| panic ⇒ 同步面挂死 | 所有 reply 走通道 + `RecvError → IslandErr::EngineGone`（禁 `unwrap()`）；快照轮询无阻塞；TUN 读线程靠 `send Err ⇒ return` 自退 | 无 |
| **panic 分类延迟**（设计门 2.2） | **已消解**：岛内 `catch_unwind` 就地 `mark_unhealthy_if_current(gen,"panic")`（不依赖下一次收尾的 join） | 无 |
| panic 的 join 侧被静默吞掉（今日 wgcore 形态） | `stop`/`stop_within`/收割线程**检查 `join()` 的 Err** ⇒ 记行 + `mark_unhealthy_if_current`（§3.6-3/4） | WG 侧同形态残留（`:1524/:1551/:1564` 三处 `let _ = h.join();`）登记为 M5 删除面，本批不动 |
| 收工超预算（岛卡死） | `stop_within(now + CLIENT_CLOSE_BUDGET=2s)` + 到点 detach + `hw-quic-reap` 收割（镜像既有形态） | **两条 tokio 特有残余（设计门 2.4）**：①detach 后老世代线程仍持 runtime + quinn `Endpoint`（UDP fd + 缓冲）⇒ 可能继续对出口发包、`Endpoint::drop`（发 `CONNECTION_CLOSE`）晚到；M2 起出口按连接记账，老连接滞留直接影响设备表 ⇒ **M1 判据面加一条「detach 后老世代 UDP 源端口/连接数可观测」**；②`JoinSet` 未 abort 时收尾后仍有 task 持 quinn 状态（已由 §3.5 的 abort+join 纪律覆盖） |

### 8.2 新发现风险（本棒实测）

| # | 风险 | 证据 | 处置 |
|---|---|---|---|
| R-A | **`tools/build-app-core.sh` 不设 CC ⇒ M0 落地后 App 出包当场断** | 实测缺 CC 时 `cc-rs` 报 `assert.h not found`；且 `.cargo/config.toml` 的 linker **不被采用** | M0 必修（§2.5）；列为实施清单第 1 项 |
| R-B | `tools/ci-local.sh` 步骤 3 的 OHOS check 同样缺 CC | 同上 | M0 必修 |
| R-C | **体积基线口径错配**：lab 数字是 `lto+panic=abort+strip`，产品是**默认 release（无 LTO/unwind）+NDK strip** ⇒ 直接拿 1,825,088B 推「+1.5MB」会低估 | 仓内 `[profile.*]` 零命中；`build-app-core.sh` 无任何 `-C` 开关 | `quic-ab.sh size` **双档**（lab/product）；M5 门槛按 product 档判（§4.4/§5.1） |
| R-D | check 垫片（`-nostdlibinc` + `stdlib.h`）误入真实构建 ⇒ 产出一个「无 libc 的 ring 对象」 | 机制上只放在 ci.yml env 与 ci-local 的 check 步骤 | 纪律写进 §2.3 + 代码门核对；`build-app-core.sh` **只设 CC、不带 flags** |
| R-E | CI runner 若不再预装 `clang` ⇒ 三目标 check 全红 | ubuntu-latest 镜像当前自带 clang；但镜像会演进 | ci.yml 加 `which clang \|\| sudo apt-get install -y clang`（§2.6） |
| R-F | `Cargo.lock` 变更与 `--locked` 门 | CI 用 `--locked`；新依赖必须同批把 lock 推进去 | 实施清单第 2 项：`cargo update -p quinn` 类改动后**提交 lock** |
| R-G | OHOS **运行期**（不是编译期）tokio/mio 行为未验（epoll 在 OHOS 上的可用性） | 本轮只验编译（`target_os="linux"` ⇒ mio 走 linux 路径） | M1 首次真机/真机模拟器验证；M0 只登记为未验项 |
| R-H | 岛的命令通道 **unbounded**：若岛停滞，`TunPacket` 队列可无限增长（今日 wgcore 同形，不是新引入的洞） | `wgcore::Cmd::TunPacket(Vec<u8>)` 也是 unbounded | M0 骨架只登记；**M1 必须给数据面加上界/丢弃计数**（与「DATAGRAM 超限丢弃可观测」同一判据面） |
| R-I | `quinn`/`rustls` 小版本漂移（0.11.x/0.23.x）改变默认 features 或行为 | 本设计锚 0.11.12/0.23.45（lock 为准） | lock 入库 + CI `--locked`；升级单独成批 |
| R-J | worktree 缺 `baseline/`、`bin/` ⇒ 本地-only 门（词表/向量/矩阵）**在本 worktree 跑不了** | 实测 `baseline: No such file or directory`、`bin` 缺失 | 收口顺序写进 §10；这些门在主检出跑 |
| R-K | **`-nostdlibinc` 爆炸半径**：`CFLAGS_<target>` 是 cc-rs 全局的，将来任何引入 C 的依赖会静默继承（报与自身无关的 `'string.h' file not found`） | 今天安全（设计门复核：`Cargo.lock` 里 `cc` 零命中，唯一 C 依赖 = ring 0.17）；但没有门拦住未来 | ci.yml 加 fail-closed 断言：`cargo check -v` 里每条含 `-nostdlibinc` 的命令行必须匹配 `ring-`（§2.6） |
| R-L | **harness 的 `dangerous()`/`SkipVerify` + 自签证书入仓**（lab 的 `quic.rs:177`、`multiconn.rs:57`、`size-probe2` 两处；其中 size-probe2 是 **cdylib**），M2 做 RPK 时是天然的复制源 | 设计门 8.1 实测点名 | ①`crates/` 内零命中由 `check-quic-isolation.sh` 断言；②harness 命中处必须带 `// SECURITY: harness-only`；③M2 设计门把「SkipVerify → RPK 校验」列为门的一项（§3.4 层 3 / §4.3） |
| R-M | **构建期代码执行面扩大**：ring 0.17 引入 `cc` 并在 build script 里编 C/asm（`cc` 今天不在锁里 = 本批新增的构建期执行面） | `cargo tree -i cc` 只在 ring 下 | 登记（上游常态）；与 R-D/R-K 的断言同批覆盖 |
| R-N | 本批新增的 `tools/cc-check-shim/stdlib.h` 是**自制** C 头（虽然只用于 check-only） | 设计门 3.2 复核了爆炸半径 | 头文件内写明用途 + §2.3 纪律；不进任何真实构建（R-D） |

---

## 9. 测试计划（M0 新增）与 flake 口径

### 9.1 新增测试清单（全部在 `crates/homeway-quic/src/tests.rs`，走公面）

| # | 用例 | 断言（判绿证据） | 证伪/退出条件 |
|---|---|---|---|
| 1 | `island_starts_and_stops_within_budget` | 起岛 → `Stop` → `stop_within(now+2s)` = true；线程已 join（`is_finished()`） | 超 2s 未收 ⇒ 红（并触发收割路径，见 4） |
| 2 | `tun_attach_replies_and_rejects_second` | 首次 attach → `Ok(())`；二次 → `Err(TunAlreadyAttached)` | 二次返回 Ok ⇒ 语义错 |
| 3 | `tun_packet_is_accepted_without_reply` | 投 `TunPacket` 后仍可正常收工（热路径不阻塞、无回执通道需求）；**快照计数 +1 以 `now+2s` 为界轮询**（无 reply ⇒ 跨线程观测量必须给有界等待，设计门 4.2） | 投递阻塞/panic/超时未观测到 +1 ⇒ 红 |
| 4 | `stop_within_detaches_when_island_stuck`（**`#[cfg(test)]` 挂起注入缝**） | 到点返回 **false**，且 `hw-quic-reap` 收割线程接手（镜像 `wgcore` 的同名测试形态） | 若到点仍 true ⇒ 有界预算失效 |
| 5a | **`island_panic_is_classified_in_place_and_surfaces_as_engine_gone`**（panic 面专项，`#[cfg(test)]` panic 注入缝） | ①`SetOnUnhealthy` 回调**立即**收到 `"panic"`（不等收尾）；②在途命令的 `reply.recv()` 得 `Err(IslandErr::EngineGone)`（**不挂死**）；③`stop_within` 返回 **true**（panic 后线程已 finished ⇒ `wait_finished` 立即真；**返回 false 是"卡死"的断言，不是"panic"的**——原稿此处自相矛盾，设计门 2.3 已订正）；④panic 记行**在 `stop_within` 的 join 分支**产生 | 挂死 / 静默 / 分类延迟 ⇒ 红 |
| 5b | 卡死注入 ⇒ 返 **false** + `hw-quic-reap` 接手 + **panic/卡死记行由收割线程产生**（与 5a 的"谁记行"口径对应，设计门 2.3②） | 见 §3.6-4 的分工表 | 记行重复或缺席 ⇒ 红 |
| 6 | `snapshot_is_pollable_after_stop` | 收工后 `snapshot()` 可读（不 panic、值冻结） | — |
| 7 | `public_types_are_send_static`（**定位 = 可搬运性断言，不是泄漏检出**，设计门 1.4） | 编译期断言 `Cmd`/`IslandTx`/`IslandErr`/`IslandSnapshot: Send + 'static`；`IslandTx: Send + Sync`（**M0 无 `IslandEvent`**，见 §3.3） | 编译失败即红 |
| 8 | `dep_face_alive_quinn_client_endpoint` | `current_thread` runtime 内建 `quinn::Endpoint::client(127.0.0.1:0)`，`local_addr()` 可读，drop 干净 | 依赖面假就位（只有 lock 条目）⇒ 红 |
| 9 | 源码门（非单测）`tools/check-quic-isolation.sh` | 见 §3.4 层 3 的五条断言（含 `dangerous()` 零命中） | 任一命中 ⇒ 红 |
| 10 | **公面签名钉定**（编译期，设计门 1.2 的廉价构造性补充） | 在测试里对 M0 公面逐项写**显式 std 类型签名**（如 `let _: fn(&Island, Instant) -> bool = Island::stop_within;`）——签名一旦夹带 tokio/quinn 类型即编译失败 | 签名改动未同步钉定 ⇒ 编译失败（迫使显式过门） |

### 9.2 flake 口径登记（路线文件要求：QUIC 岛测试的 flake 口径）

1. **不钉固定端口**：一切回环端口用 `bind("127.0.0.1:0")` + 读回实际端口（仓内已知 flake 族「双 `cargo test` 并发撞固定端口」的根治手法）。
2. **时间断言禁用精确墙钟**：①纯定时语义用 `tokio::time` + **`start_paused = true`** 断言「虚拟时钟推进 N」（该形态需 `#[tokio::test]` 与暂停时钟 ⇒ `tokio` 的 **dev-dependency** 加 `["macros", "test-util"]`；生产依赖不含 `test-util`）；②必须走真实线程/IO 的用例只断言**上界**（`elapsed < 预算 × 4`）且判绿口径 = 「预算内收工」而不是「耗时 ∈ [a,b]」；③panic/卡死注入用例（9.1 的 5a/5b）**不设墙钟下界**，只断言「回执不挂死 + 记行到达 + 返回值形态」。
3. **不依赖 loadavg**：M0 岛内无吞吐断言（岛骨架不搬数据）；一切「性能」判据由 `quic-ab.sh` 承担（每包 CPU 口径，见 §4.5）。
4. **隔离复跑纪律**（沿用仓内既有口径）：若用例红，先隔离单跑（`--test-threads=1` 且独占 target）再判是否回归；与改动面无交集的既有时序族（`daemon::tests::*`）不与本批混判。
5. **登记动作**：实现棒把这五条写进 `docs/QUIC-ROADMAP.md` 的「已知 flake 登记」节（该节已预留「本程序新增（实现时登记）」一行）。

---

## 10. M0 实施清单（第 2 棒交付物 ①–④ 的形态与判据）

| # | 交付物 | 具体动作 | 完成判据 |
|---|---|---|---|
| ①-1 | 依赖落地 | 根 `Cargo.toml`：`members` 加 `crates/homeway-quic`、`[workspace.dependencies]` 加 quinn/rustls/tokio 三条、`exclude` 加 `tools/quic-ab`；新建 `crates/homeway-quic/Cargo.toml`（§1.2 草案）；`homeway-core/Cargo.toml` **只加** `homeway-quic` 一行；提交 `Cargo.lock` | `cargo tree -i ring@0.17.14`/`@0.16.20` 各见反依赖；`grep -c aws-lc Cargo.lock` = 0；三条「行为零改动」判据（§3.5）全过 |
| ①-2 | 交叉工具链 | 新增 `tools/cc-check-shim/stdlib.h`；`ci.yml` 三目标 env + clang 前置 + `-p homeway-quic` + `-nostdlibinc` 只落 ring 的断言；`ci-local.sh` 步骤 3 导出 CC + 真 OHOS link 步骤；`build-app-core.sh` 导出 `CC_aarch64_unknown_linux_ohos`（**不带 nostdlibinc**）；`.cargo/config.toml` 补注释；根 `.gitignore` 加探针 target 与 certs DER | 三目标 `cargo check` 本机全绿；`tools/build-app-core.sh` 出 `.so` 且三道门过（**M0 硬判据**） |
| ② | QUIC 岛骨架 | 新建 `crates/homeway-quic/src/{lib,cmd,driver,sync_util,tests}.rs`（§3 定稿形态；**零接线**）；`tools/check-quic-isolation.sh` | `cargo test --workspace` 全绿（含新增 10 条）；`cargo clippy --all-targets -D warnings` 0；`homeway-core`/`cli`/`capi` 源码零引用 `homeway_quic::` |
| ③ | 实验台转正 | `tools/quic-ab.sh` + `tools/quic-ab/**`（§4 形态；两独立 workspace；lock 入库；certs 现场生成） | `tools/quic-ab.sh all` 一键跑通；`cpu/overhead/mem/size` 落在 §4.5 判据内（product 档只登记） |
| ④ | 三基线入册 | 新建 `docs/QUIC-BASELINE.md`（§5 三表 + 出处 + 复测命令，**用 harness 实测值重新登记**）；`docs/QUIC-ROADMAP.md` M0 小节与状态总览加指针 | 文档内每个数字都有「来源 + 日期 + 复测命令」；lab 旧值只作量级对照 |
| ⑤ | 记录与登记 | `docs/reviews/M0.md`（代码门：意见 + 处置 + 测试/判据/实测证据）；路线文件 flake 节补 §9.2 五条；`docs/QUIC-ROADMAP.md` 「M0 范围」里的 `.cargo/config.toml/CI 补 CC` 执行完毕 | 证据齐 + 判据面零改动核对（主检出跑 `check-vocab.sh` PASS） |

**范围登记（设计门 9.3）**：下列三项**超出路线文件 M0 范围的字面**，但属其**必要细化**（非范围扩张）——
①`tools/cc-check-shim/stdlib.h`（路线文件只写「`.cargo/config.toml`/CI 补 CC」，未写"C 侧无 sysroot 怎么办"）；
②`tools/quic-ab/` 采用**两个独立 workspace**（`[patch]` 是 workspace 级，一个 workspace 装不下"真 ring"与"垫片"两臂）；
③`docs/QUIC-BASELINE.md`（三基线入册需要落点，路线文件未指位置）。实现棒在 `docs/reviews/M0.md` 里照抄这三条登记。

**收口顺序（受 worktree 限制，务必照做）**：本 worktree 缺 `baseline/`/`bin/` ⇒
①在本 worktree 完成代码 + `cargo test --workspace` + clippy + 三目标 check；
②合入 main 后在**主检出**跑 `tools/check-vocab.sh`、`tools/ci-local.sh`（含向量/矩阵门）并留证；
③最后写 `docs/reviews/M0.md` 与基线指针。

---

## 11. 设计门记录

### 11.0 轮次目录与指路

- **轮次目录**：`/tmp/dsh-review/r10.25ZI6g/`（`prompt.txt` / `output.md` 416 行 / `stderr.log`）；
  本轮**无子轮**（评审者内部未另起轮次，与 QI 先例不同）。
- prompt 指路（不喂结论）：设计文档 + `docs/QUIC-ROADMAP.md`（M0 小节 / 每期执行协议 / 评审协议 /
  附录 A/D/E）+ `AGENTS.md`（硬规则、工程原则）+ `docs/INTEROP-CRITERIA.md`（判据变更记录）+
  先例 `docs/reviews/QI-design.md`/`QFB-design.md` + 代码入口（`wgcore/mod.rs`、`facade/**`、
  `syncutil.rs`、`capi/lib.rs`、`tools/{ci-local,build-app-core}.sh`、`ci.yml`、`.cargo/config.toml`）+
  上游 crate 源码（ring/rustls/quinn/quinn-proto/tokio）+ 实验台 `/tmp/quic-lab/`；
  评审重点 10 项（异步/同步边界、panic 面、交叉编译与 CI、功能等价隐含假设、残留 WG 依赖、地道 Rust、
  预算可测性、安全面、路线文件一致性、§11 形态），并要求「看过的方面没问题也明确说明」。
- **仓内副作用文件检查**：评审后 `git status --porcelain` = 仅 `?? docs/reviews/M0-design.md`（本设计文档）
  ⇒ **无 dsh 副作用文件**（无需转存/删除）。

### 11.1 结论

- **dsh exit code = 0**（成功；成败只认 exit code）。
- 评审意见 **39 条**：**高 1** / **中 22** / **低-中 5** / **低 11**；另 5 行为记录性条目
  （§2.0 与 §3.0 两节独立复核、「看过，没发现问题」三行）——**逐条列在 11.2**。
- **逐条处置：认同 39 / 部分认同 0 / 不认同 0**（其中 2.1 采纳了评审给的**更强**选项——沿用
  `spawn_derived` 先例而非仅重写理由句）。
- **未发现误报**：全部 39 条经本棒独立复核（重跑实验/读源码/读 lab 原始文件）均成立；评审者亦独立复现了
  三目标 check（含 NDK 真 sysroot 路径）、双 ring 共存、体积与 hash、绿基线日志。
- **过门结论：通过（v1 → v2）**。评审给的是「有条件通过（6 硬 + 4 软）」，10 条条件**全部已落到 v2**
  （逐条见 11.3），故升 v2 并判通过。
- 评审独立复核对我方关键结论的**确认/推翻**：确认——三目标配方（含 `file` 物证与 `.a` 体积）、
  `[profile.*]` 零命中（unwind 前提）、`links` 无冲突（并订正尾下划线）、体积/hash 逐字符一致、
  绿基线 724/17/12 与 clippy 0、订正 1（wg-ring 实为 ring 0.16.20）成立；**推翻**——订正 2 的末句
  「少任何一条都会拉 aws-lc」（见 11.3 的 9.2）、§5.3「三值逐轮完全一致」、§9.1 用例 5 的 `false` 组合、
  §3.6-1 的「catch_unwind 只在 FFI 边界」。

### 11.2 评审原文摘要（逐条）

| # | 评审意见（摘要） | 严重度 |
|---|---|---|
| 1.1 | **`select!` 需 `macros`**：§1.1 却写「不启 macros」⇒ 骨架按设计写不出来（`cfg_macros!` 门控；已实测 `error[E0433]: cannot find \`select\` in \`tokio\``）；且原稿把 `#[tokio::main]` 与 `select!` 混为一谈 | **高** |
| 1.2 | **第 3 层 grep 方向反了**：只查 `src/quic/` 之外，恰不查岛的公面——`pub fn … -> tokio::…` 写在岛里可五层全过 | 中 |
| 1.3 | **依赖方向不是独立一层**；把 tokio 声明在 `homeway-core` 才是泄漏面本身。建议岛进**独立 crate**（`homeway-core` 里写不出 `tokio::`＝E0433，构造性） | 中 |
| 1.4 | 编译期 `Send + 'static` 断言对泄漏**零检出**（所有 tokio/quinn 类型都满足）——定位应改为「可搬运性断言」 | 中 |
| 1.5 | 「通道形态零 fd、**零收割期资源**」与 §3.5/§8.1 自相矛盾：detach 后岛线程仍持 runtime + quinn `Endpoint`（UDP fd）；「sender 掉光 ⇒ 自然退出」不是实践中的退出路径 | 中 |
| 1.6 | `IslandTx` newtype 对 M0 够用，但要把「不许破封」（不 `Deref`、不给取内件 accessor、`Clone` 返自己）写成契约 | 低 |
| 2.0 | **前提独立复核：事实成立**——`[profile.*]` 零命中（4 处）、`build-app-core.sh` 无 `-Cpanic`、tier `build-core.sh` **委托**核侧脚本、`term/service.rs:351` 先例互证 | — |
| 2.1 | **§3.6-1 理由句与仓内事实相反**：`catch_unwind` 不只 FFI 边界——`spawn_derived:1481`、`service_exec:219`、`term/service:354` 都对线程体套它并转 `mark_unhealthy("panic")` | 中 |
| 2.2 | **panic 分类延迟**：join-Err 检测挂在收尾 ⇒ 世代存活期 panic 无人 join，`unhealthyReason=panic` 可能延迟或永不产生（而它是判据语义） | 中 |
| 2.3 | **§9.1 用例 5 自相矛盾**：panic(unwind) 后线程已 finished ⇒ `stop_within` 返 **true**；`false` 只属"卡死"。且「谁记 panic 行」必须写明（预算内 = `stop_within`；超预算 = reaper） | 中 |
| 2.4 | `stop_within` 到点 detach 的 **tokio 特有残余**未登记：老世代持 `Endpoint` ⇒ 可能继续发包/`CONNECTION_CLOSE` 晚到（M2 起按连接记账，影响设备表）；`JoinSet` 未 abort 的残留 | 低-中 |
| 2.5 | 「wgcore 吞掉 panic 面」**属实**（`:1524`/`:1551`/`:1564`）；但口径应精化为「吞掉的是 `Err` 分支 ⇒ 不记行、不置 `unhealthyReason=panic`」 | 低 |
| 2.6 | TUN 读线程的存活依赖「`send Err ⇒ return`」（`wgcore/mod.rs:1876`）——应写进 §3.6-2 的覆盖清单 | 低 |
| 3.0 | **交叉配方独立复现全绿**（三目标 + NDK 真 sysroot；`file`/`.a` 物证；ring build.rs 引用逐字一致；YAML 可解析；零告警） | — |
| 3.1 | 「不覆盖链接期」诚实，但**链接期没挂到任何门上**（全仓唯一路径是手动 `build-app-core.sh`）⇒ 升为 M0 硬判据 + `ci-local` 加一步（NDK 缺则显式 SKIP） | 中 |
| 3.2 | `-nostdlibinc` **爆炸半径**：今天安全（`cc` 零命中，唯一 C 依赖 = ring），但没有门拦住未来 C 依赖静默继承 ⇒ 加廉价 fail-closed 断言 | 中 |
| 3.3 | §2.2 第 4 行的交叉引用悬空（§2.5 无实构证据） | 低 |
| 3.4 | §2.5 的 `.gitignore`「现状」与事实不符（实含 `/baseline`、`/bin`、`*.so`、`*.a`、`fuzz/*` 等）；建议带尾斜杠 `/tools/quic-ab/**/target/` | 低 |
| 3.5 | `size` 子命令措辞没说"哪个 clang"：**必须** NDK clang 且**断言** `CFLAGS_…` 不含 `-nostdlibinc`（否则真链接会炸或静默错） | 低-中 |
| 4.1 | 「行为零改动」的两条判据不足以证伪，漏了**依赖图/feature 统一**这条：建议加「前后 `cargo tree -e features` + `Cargo.lock` diff，逐一核对只增不改」 | 中 |
| 4.2 | §9.1 用例 3 的「计数 +1」是异步观测 ⇒ 必须写明有界等待（否则正是自己声明要根除的 flake） | 中 |
| 5.0 | 双 ring 共存/`links`/`[patch]`/探针隔离/体积口径——**看过，没发现问题**；小订正：`links` 实为 `ring_core_0_17_14_`（带尾下划线） | 低 |
| 5.1 | **lab 档 profile 会静默失效**：cargo 忽略非 workspace 根的 `[profile]` ⇒ 必须写在 `arms/Cargo.toml`（根）；并加「构建日志不得出现 non root package 警告」自检 | 中 |
| 5.2 | **product 档产生机制未定义**（"另跑 product 档"不可执行）⇒ 写明覆盖命令与档位断言；product 数字只登记不作 ±10% 判据 | 中 |
| 6.0 | `thiserror`/`#[non_exhaustive]`/模块边界/`Box<[u8]>`/公面 `pub` 抑制 dead_code——**看过，没发现问题** | — |
| 6.1 | 依赖矩阵**漏 `bytes`**（M1 发 DATAGRAM 必需；quinn 不 re-export） | 低 |
| 6.2 | **测试缝与「不许只有测试才用的私件」直接冲突** ⇒ 必须选定 `#[cfg(test)]` 或 `test-seams` feature（否则 clippy 0 不可达） | 中 |
| 6.3 | `IslandEvent`/`IslandSnapshot` 在 M0 的形态未定义（三处引用，用例 7 还会编译不过） | 低-中 |
| 7.0 | 口径映射逐项一致、`vmmap`/`ps` 判据口径、体积/hash、附录 A 其余数字——**看过，没发现问题** | — |
| 7.1 | §5.3「三值逐轮完全一致」**不属实**：`fp-quic.med` = **1216/1216/1232** | 中 |
| 7.2 | **CPU 基线的原始证据链是错的**：`m-*.out` 全是 11 字节空壳，四个数只在手抄 SUMMARY；真留存证据是 `/tmp/pk-*.out`（N=300k），与 SUMMARY 差 **1.3–5.9%** ⇒ 改引用 + 改「复测语义」+ 立第 3 条订正 | 中 |
| 7.3 | `mem --mode load` 的 `N=200000` 与 §5.3「N=300k」冲突，且**实际执行的是 300k**（脚本默认被覆盖） | 中 |
| 7.4 | §4.5 体积「4 档（lab 档）」把 product 档的 `2,213,744` 混进来了（该格 ±10% 无法定义） | 中 |
| 7.5 | `size` 的 lab 证据不全：三档里只有「+真实引用」有留存产物；`size-probe2/quic.log` 是一次**失败调用** | 低-中 |
| 7.6 | 「中位」是**下中位**（`a[int((NR+1)/2)]`）——照搬，但要在 README 写明，防将来被当 bug「修」掉 | 低 |
| 8.0 | 身份面未引入、垫片只进 check、探针 `exclude`、DER 不入库——**看过，没发现问题** | — |
| 8.1 | **`dangerous()`/`SkipVerify` + 自签证书入仓无边界无门**（lab 四处，含一个 cdylib）⇒ 加零命中断言 + `SECURITY: harness-only` 标记 + M2 设计门列项 | 中 |
| 8.2 | build script 执行面扩大（`cc` 本批新增）应登记 | 低 |
| 9.1 | 订正 1（wg-ring = ring 0.16.20）**成立**；补充：§4.3 说 SUMMARY「原样搬迁」会把已订正的错误重新写进仓内 | 低-中 |
| 9.2 | 订正 2 **方向成立但结论句错**：A/B 对照（quinn 默认开 + rustls 默认关 ⇒ aws-lc **0**）⇒「少任何一条都会拉 aws-lc」不成立；aws-lc 的唯一开关是 rustls 关默认 | 中 |
| 9.3 | 范围照旧（无缩无扩）；三项超出字面的落地物建议显式标注「必要细化，非范围扩张」 | 低 |
| 9.4 | §3.5「收工顺序」与仓内实际**相反**（实际 `pf → 桥 → client → 缓存 → finish_generation`） | 低 |
| 10.1 | 文档头写「设计门已过」而 §11 写「回填前未过门」——自相矛盾（正是门要防的"门做假"） | 中 |
| 10.2 | §11 字段不全 ⇒ 按 QI 先例补 11.0–11.7（轮次/结论/摘要/处置/不认同/残余登记/覆盖度/回填复验） | 中 |

### 11.3 逐条处置表

| # | 处置 | 落到 v2 的位置/证据 |
|---|---|---|
| 1.1 | **认同**——tokio features 改 `["rt","time","sync","macros"]`，写明「`macros` 只为 `select!`，仍不用 `#[tokio::main]`」；并附实测错误串 | §1.1 tokio 行 |
| 1.2 | **认同**——新增层 3 断言「`lib.rs`/`cmd.rs`（公面文件）零 `tokio::/quinn::/rustls::/async fn/.await`」+ 测试用例 10「公面签名钉定」（显式 std 类型签名，夹带即编译失败） | §3.4 层 3、§9.1-10 |
| 1.3 | **认同（采纳最强解）**——岛改独立 crate `crates/homeway-quic`（leaf-ward，不得依赖 core）；`homeway-core` 只加一行路径依赖；代价三条（syncutil 自持 / M1 边界只传 std 类型 / CI `-p` 清单）一并写明 | §1.2、§3.1、§3.4 层 0 |
| 1.4 | **认同**——层 2 定位改「可搬运性断言，不检出泄漏」；`IslandErr` 加「变体不得携带非 std 载荷 / `source()` 不得返 quinn 错误」契约；用例 7 同步收窄 | §3.4 层 2、§9.1-7 |
| 1.5 | **认同**——删「零 fd、零收割期资源」；改为「净减唤醒 fd 一族；detach 后仍持 runtime + `Endpoint`（UDP fd）」+「收发路径实为 `Cmd::Stop`/stop 位」；并把机制实验（`island_boundary.rs`）写进正文 | §3.2、§8.1 |
| 1.6 | **认同**——`IslandTx` 三条封印契约（不 `Deref`/不给 accessor/`Clone` 返自身）写进层 1 | §3.4 层 1 |
| 2.0 | 记录（4 项复核结论与我方一致；tier 委托调用一条已并入 §2.4/§3.6 前提） | §2.4、§3.6 |
| 2.1 | **认同（按更强选项落地）**——采纳仓内 `spawn_derived` 先例：岛线程体套 `catch_unwind(AssertUnwindSafe)`，就地处 `mark_unhealthy_if_current(gen,"panic")` 后退线程；删除原「只在 FFI 边界」的错误理由句 | §3.6-1 |
| 2.2 | **认同**——panic 分类**就地即时化**（不依赖收尾 join）；§8.1 该行残余由「无」改为「已消解」并给出机制 | §3.6-1、§8.1 |
| 2.3 | **认同**——用例拆 5a（panic ⇒ `stop_within` 返 **true** + 记行 + 分类即时）/5b（卡死 ⇒ 返 false + reaper 接手）；「谁记行」分工写进 §3.6-4 | §9.1-5a/5b、§3.6-4 |
| 2.4 | **认同**——§8.1「收工超预算」行补两条 tokio 特有残余（老世代 `Endpoint`/发包/`CONNECTION_CLOSE` 晚到；`JoinSet` 残留）＋ M1 判据面加「detach 后老世代 UDP 源端口/连接数可观测」 | §8.1 |
| 2.5 | **认同**——口径改「吞掉的是 `Err` 分支 ⇒ 不记行、不置 `unhealthyReason=panic`」并补 `:1524/:1551/:1564` 三处行号 | §3.6-3 |
| 2.6 | **认同**——§3.6-2 覆盖清单加第④条（TUN 读线程靠 `send Err ⇒ return` 自退，镜像 `:1876`） | §3.6-2 |
| 3.0 | 记录（独立复现全绿，已并入 §2.2 表第 4 行与 §3.0 结论） | §2.2 |
| 3.1 | **认同**——`build-app-core.sh` 出 `.so`（三道门 + 体积增量）升为 **M0 硬判据**，写进 §2.4/§7/§10；`ci-local.sh` 步骤 3 加真 OHOS link 步骤（NDK 缺 ⇒ 显式 SKIP，不静默） | §2.4、§7、§10 ①-2 |
| 3.2 | **认同**——ci.yml 加 `-nostdlibinc` 落点断言（每条含该 flag 的命令行必须匹配 `ring-`），落为风险 R-K | §2.6、§8.2 R-K |
| 3.3 | **认同**——§2.2 第 4 行改为「本轮设计门独立复现」（不再指向 §2.5） | §2.2 |
| 3.4 | **认同**——`.gitignore` 现状格订正为实际内容；新增模式带尾斜杠 + certs DER 行 | §2.5 |
| 3.5 | **认同**——§4.5 前置写死「NDK 包装 clang + 断言 CFLAGS 不含 `-nostdlibinc`」 | §4.5 |
| 4.1 | **认同**——§3.5 增第三判据（`cargo tree -e features` 前后对比 + lock diff，逐条核对只增不改） | §3.5 |
| 4.2 | **认同**——用例 3 写死「以 `now+2s` 为界轮询快照」 | §9.1-3 |
| 5.0 | **认同（笔误订正）**——`links` 改 `ring_core_0_17_14_`（附 `Cargo.toml:18`） | §1.3 |
| 5.1 | **认同**——lab 档 profile 落 `arms/Cargo.toml`（workspace 根）+ `wg-shim` 自带 + 「不得出现 non root profile 警告」自检 | §4.4 |
| 5.2 | **认同**——product 档机制写死：`[profile.product] inherits = "release"` + `cargo build --profile product` + 档位断言（`cargo metadata`）+ 两档尺寸差对照；product 数字只登记 | §4.4、§4.5、§7 |
| 6.1 | **认同**——矩阵加 `bytes` 行（M1 必需 / M0 不引入，附「quinn 不 re-export」依据） | §1.1 |
| 6.2 | **认同**——测试缝定死 `#[cfg(test)]` 门控（release 消失 ⇒ 零 dead_code）；不用 `test-seams` feature 并写明理由 | §3.7、§9.1-4/5a/5b |
| 6.3 | **认同**——`IslandSnapshot` 定义 M0 字段（`attached`/`packets_in`）；**M0 不引入 `IslandEvent`**，用例 7 同步收窄 | §3.3、§9.1-7 |
| 7.1 | **认同**——§5.3 改为「1216/1216/1232（中位 1216，第三轮 16K 离群）」并把该事实作为 ±10% 判据的依据 | §5.3、§4.5 |
| 7.2 | **认同**——①§5.2 来源列改「手抄 SUMMARY + `/tmp/pk-*.out`（N=300k，偏差 1.3–5.9%）」；②§4.5 复测语义改「以 harness 实测**重新登记基线**，旧值作量级对照」；③新增 **§0.3 订正 3**；④差异来源列补实测带宽 | §0.3-3、§4.5、§5.2 |
| 7.3 | **认同**——`mem --mode load` 默认改 `N=300000`（与基线同口径），注明该档只登记不设硬判 | §4.3、§5.3 |
| 7.4 | **认同**——§4.5 拆「lab 档三格（±10%）」与「product 档两格（只登记）」，§7 同步拆行；lab 档只作口径校验 | §4.5、§7 |
| 7.5 | **认同**——§4.3/§5.1 注明 lab 三档只有一档有留存产物、`quic.log` 是失败调用不得引用；四档由 harness 实测重出 | §4.3、§5.1 |
| 7.6 | **认同**——§4.4 与 §4.3（README 行）写明「中位 = 下中位，照搬勿修」 | §4.3、§4.4 |
| 8.1 | **认同**——层 3 加第 5 条断言（`crates/` 内 `dangerous()`/`with_custom_certificate_verifier` 零命中）；harness 命中处带 `// SECURITY: harness-only`；M2 设计门列「SkipVerify → RPK」为门项；立风险 R-L | §3.4 层 3、§4.3、§8.2 R-L |
| 8.2 | **认同**——立风险 R-M（`cc` = 本批新增构建期执行面，与 R-D/R-K 同批覆盖） | §8.2 R-M |
| 9.1 | **认同**——§4.3 的 README 行加「搬迁时必须套用 §0.3 三条订正（README 的『真 ring 0.17』会重新写错）」 | §4.3 |
| 9.2 | **认同**——§0.3 订正 2 重写：补 A/B 对照表（A：quinn 默认开 + rustls 关 ⇒ aws-lc **0**），结论句改「aws-lc 的唯一开关 = 本仓 rustls 关默认 features」 | §0.3-2 |
| 9.3 | **认同**——§10 增「范围登记」段：三项（cc-check-shim / 探针两 workspace / QUIC-BASELINE.md）标「路线文件 M0 范围的必要细化，非范围扩张」 | §10 |
| 9.4 | **认同**——收工顺序改为「`pf.stop_all()` → `bridge.stop()` → 岛 → 缓存终写 → `finish_generation`」（附实测行号）；§0.2 增「收工顺序」接缝行 | §0.2、§3.5 |
| 10.1 | **认同**——文档头改 v2（设计门已过 @ 轮次目录 + 日期）；回填前已在 §11 明写「未过门」 | 文档头、§11.1 |
| 10.2 | **认同**——本节按 11.0–11.7 补齐（含「看过没发现问题」的逐条记录与 11.5 门后残余登记） | §11 全节 |

### 11.4 不认同项

**无。** 39 条意见逐条复核后全部成立（关键几条由本棒重跑实验/读源码独立验证：`select!` 的 E0433、
`catch_unwind` 的四处调用点、ring `links` 尾下划线、`fp-quic.med` = 1216/1216/1232、`m-*.out` = 11 B、
`pk-*-cli.out` = 300000 pkts、`size-probe2/quic.log` 的失败调用、quinn 默认开 + rustls 关默认 ⇒ aws-lc 计数 0、
`Finish::drop` 的收工顺序）。两处「我方原稿结论被推翻」（9.2 的 aws-lc 结论句、7.2 的证据链）均按评审意见订正。

### 11.5 门后残余与不做项登记（防静默漏做）

| 项 | 结论 | 理由/承接 |
|---|---|---|
| `syncutil` 三小件在岛侧自持（两份极小重复） | **接受重复** | 不引第三个 crate、不动既有模块可见性；若 M5 后仍嫌重复再评估 |
| `IslandEvent` / 事件回调面 | **M0 不做** | 无成员的空 enum 只会空转；M1 设计门引入时定形态（§3.3） |
| `bytes` 依赖 | **M0 不引入** | M0 不发 DATAGRAM；防"声明了不用"（§1.1） |
| `-nostdlibinc` 垫片 | **只作 check 门** | 真构建（build-app-core/tier/`size`）一律真 toolchain；有三重门（§2.3/§2.5/§4.5） |
| ring 0.16.20（垫片）退役 | **M0 不实删** | 路线文件明令「先共存，M5 随 WG 删除收口」（§1.3） |
| OHOS **运行期** tokio/mio/epoll 行为 | **未验（R-G）** | 本批只验编译；M1 首次真机验证 |
| 老世代 detach 后仍可能对出口发包 | **M1 判据面**（登记） | §8.1；M2 起按连接记账，届时评估影响 |
| `wgcore` 三处 `let _ = h.join();` 吞 panic Err | **本批不动**（WG 侧） | M5 删除面；岛侧已按正确形态实现（§3.6-3） |
| `tools/ci-local.sh` 的 OHOS link 步骤在无 NDK 机器上 SKIP | **显式登记** | SKIP 必须打印档位（fail-loud），不许静默绿 |

### 11.6 覆盖度声明

- **本轮覆盖**：依赖矩阵与 feature 逐条理由（含上游 manifest 对照）、双 ring 共存与 `links`/`[patch]`、
  交叉编译三目标配方（独立复现 + NDK 真路径）、CI 改动与保真度语义、岛结构（模块/crate 边界/命令通道/
  通道-唤醒机制实验）、异步·同步边界五层（被评审改为"层 0 构造性 + 其余三层补充"）、panic 面六条
  （前提独立复核）、生命周期与收工顺序、dead-code 与测试缝、`quic-ab.sh` 与 lab 的逐项口径映射、
  三基线数字与证据链、判据行零变更、预算与可测性、测试计划与 flake 口径、安全面（含 harness 的
  `dangerous()`）、路线文件一致性（两条订正 + 一条新增）、§11 形态。
- **本轮未覆盖（如实声明）**：①OHOS **运行期**行为（tokio/mio/epoll、真机空口）——R-G，M1/M6；
  ②M2 身份与准入面（RPK/token/抗放大）——M2 设计门；③`quic-ab.sh` 的**真机复现结果**（本设计只定形态）；
  ④tier 侧 App 构建链的完整回归（本棒只读核对了 `build-core.sh` 的委托关系）；⑤`/tmp/quic-lab` 中
  `multiconn` 多点拟合的原始文件级复核（采信 `SUMMARY.md` 的 §7.3 表）。
- **看过的方面没发现问题（评审者明确记录）**：§1.1 feature 逐条理由（除缺 `macros`/`bytes`）、
  §1.3 双 ring 共存论证、§2.2 配方与 §2.3 垫片、§2.6 CI env 语义、§3.2 机制偏离的结论方向、
  §3.4 层 1/层 5、§3.5 零接线判据方向、§4.3 与 lab 脚本的逐项映射、§5.1/§5.3 数字与 hash、
  §6 判据零变更论证（含 ledger 锚 `0ae1d104…` 与主检出实测 sha256 逐字符相同）、§7 的
  `ps RSS` 非判据/`vmmap` 判据、§9.2 flake 五条、§10 收口顺序（worktree 缺 `baseline/`/`bin/` 属实）。

### 11.7 回填后的再复验声明

- 本轮回填**只改文档**（`docs/reviews/M0-design.md`），**不改任何产品代码** ⇒ 无需重跑
  `cargo test`/clippy/交叉 check；§0.1 的绿基线（724 passed / 17 ignored / clippy exit 0）仍是本 worktree 的
  当前实测状态（回填未触碰 `crates/`）。
- 回填期间**新增的实测**（供第 2 棒复核）：`/tmp/m0-probe/tests/island_boundary.rs`（通道跨同步/异步
  边界机制）、`/tmp/m0-probe3`（quinn 默认开 + rustls 关默认 ⇒ `aws-lc` 计数 0）、`select!` 的 E0433 复现、
  `fp-quic.med` / `pk-*-cli.out` / `size-probe2/quic.log` / `m-*.out` 的原始文件复核。
- **回填后 git status**：仅 `?? docs/reviews/M0-design.md`（无其它副作用文件）。
