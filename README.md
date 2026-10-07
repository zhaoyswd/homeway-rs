# homeway-rs

homeway 的 Rust 实现。最初是 Go 版 homeway 的**平行实现**，2026-10-05 Go 版整体退役后
**已转正为唯一实现**——鸿蒙端「回家」VPN 的**出口 / 客户端核 / 中继**三角色都由本仓供给。

- **三角色同一二进制**：`homeway-cli` 既是出口（WG 端点 + 过境拦截 + files/term/DNS 等服务）、
  也是客户端（直连/中继腿 + 恢复阶梯）、也是中继（信封转发 + 准入）；
- **客户端核**：`crates/homeway-capi` 提供 C-ABI 壳，产出 `libclientcore.so` 供鸿蒙 App 的
  VPN 扩展进程加载（20 个 `ClientCore*` 导出，与 tier 仓原生契约四处同步）；
- **行为对齐**：wire 字节 / 行为常量 / 判据行与 Go 基线（冻结锚 `d4148f6`）逐一对齐，
  对照件 = `fixtures/`（golden 夹具 + 派生向量）+ `docs/INTEROP-CRITERIA.md`。

## 架构

```
鸿蒙 App（tier 仓）
  └─ VpnExtensionAbility（VPN 扩展进程）
       └─ libclientcore.so（crates/homeway-capi → homeway-core）
            ├─ wtransport/   自管 UDP Bind：候选镜像 / 来源采纳 / 漫游 / 中继腿 + 端点学习缓存
            ├─ wgcore/       数据面装配：隧道侧栈（smoltcp）+ 上游 WG（boringtun）
            ├─ session/      连接生命周期与恢复阶梯 R1–R3（常量对齐 tier connection-lifecycle）
            └─ facade/       App 桥面：tun 生命周期 / 探测 / 服务会话 / files / term / portfwd / speedtest
                                 │  WG（UDP，直连或经中继）
                                 ▼
             出口（homeway-cli，Rust 统一进程）
               ├─ server/      WG 端点 + 动态 peer 表（token 凭证）+ 绑卡看护
               ├─ intercept/   过境拦截：TCP/UDP 五元组终结 + 本机 socket 重拨（L3 直通）
               ├─ term/        终端服务（alacritty_terminal + 自建编码器/应答器）
               ├─ files / dns / speedtest / ddns / upnp / stun
               └─ daemon/      统一进程：serve + relay + client 角色 + 控制面（control.sock）
                                     │
                                     ▼
                                   中继（homeway-cli 的 relay 角色：信封转发 + 准入）
```

两台生产出口（Mac launchd / 阿里云 nohup）与中继均跑本仓 `homeway-cli`，滚动升级记录见
`docs/DEPLOY-RUST-EXIT.md`。

## 构建

```bash
cargo build --release                 # 本机（macOS / Linux）
cargo build --release -p homeway-cli  # 只出 CLI
```

- 工具链：`rust-toolchain.toml` 钉 stable `1.99.0`。
- **鸿蒙核**（`libclientcore.so`，OHOS 交叉）：需要 DevEco NDK 与
  `rustup target add aarch64-unknown-linux-ohos`，配方见 `AGENTS.md`「技术底座」与
  `.cargo/config.toml`；核的产物构建/落盘由 tier 仓 `tools/tailcat/build-core.sh` 驱动
  （本仓 `tools/build-app-core.sh` 为核侧自检门）。
- 发版产物（四目标：darwin-arm64/amd64、linux-amd64/arm64-musl 静态）走 `v*` tag 触发的
  Release CI，见 `.github/workflows/release.yml`。

## 快速开始（本地起出口 + 取 token）

```bash
# 1. 起一台本地 Rust 出口（私有实例：/tmp state、端口 4265x，绝不会碰现役出口）
cargo build --release -p homeway-cli
tools/local-rust-exit.sh start 1

# 2. 取客户端 token（粘进 App 的「添加主机」，或喂给 connect/speedtest 的 --token）
tools/local-rust-exit.sh token 1

# 3. 用命令行客户端连它（服务会话形态，不带 TUN）
target/release/homeway-cli connect --token '<上一步的 token>'

# 收尾
tools/local-rust-exit.sh stop 1
```

其他常用入口：

```bash
# 零参 = 统一进程（client/control 恒开 + serve/relay 按 config 期望态装配）
target/release/homeway-cli --state /tmp/hw-local

# 对照 Go 基线起本地 Go 出口（需先 tools/make-baseline.sh 建快照克隆）
tools/local-exit.sh start 1            # 出口 4264x
tools/local-exit.sh client-start 1     # 其统一进程客户端（serve/relay 双关）

# 互操作矩阵 / 性能 A/B / 本地 CI 一键门
tools/matrix.sh --smoke
tools/perf-ab.sh
tools/ci-local.sh
```

token 是**凭证**：不内置、不进仓库、不随包分发（前缀 `hmw1`；中继凭据前缀 `rl1`）。

## 文档地图

| 想了解 | 看 |
|---|---|
| **新会话入口 / 硬规则 / 工程原则 / 技术底座** | `AGENTS.md` |
| **程序主体进度真源**（期次范围、判据、退出口、下一步指针） | `ROADMAP.md` |
| **整改批进度真源**（2026-10-07 评审后的 Q 批分批范围与执行协议） | `docs/REVIEW-ROADMAP.md` |
| **评审发现清单**（P0×5 + P1 全量 + 复核标注） | `docs/reviews/AUDIT-2026-10-07.md` |
| 版本历史（每版改了什么） | `CHANGELOG.md` |
| 生产出口部署与滚动升级实录 | `docs/DEPLOY-RUST-EXIT.md` |
| 基线锚定（冻结 `d4148f6`）与漂移登记 | `docs/BASELINE.md` |
| 互操作判据行真源（含「判据变更记录」） | `docs/INTEROP-CRITERIA.md` |
| golden 夹具与对照向量 | `fixtures/`（`MANIFEST.md` + `SHA256SUMS`） |
| 性能 A/B 报告 | `docs/PERF-AB.md` |
| 全量对齐缺口审计（与基线逐特性对照） | `docs/GAP-AUDIT.md` |
| 各期评审记录 | `docs/reviews/` |
| 本地 CI / 构建 / 向量生成脚本 | `tools/`（`ci-local.sh` / `gen-vectors.sh` / `local-*.sh`） |

## 许可

本仓**暂未声明许可**（无 `LICENSE` 文件）。在明确声明之前，请勿假定任何再分发或商用条款
——有需要请先联系仓库所有者。
