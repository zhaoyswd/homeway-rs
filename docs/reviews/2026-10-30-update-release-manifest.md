# 2026-10-30 `homeway update` Release 清单更新批

## 范围与技术门

需求：消除匿名 GitHub REST API 额度耗尽导致更新检查失败的问题。更新入口改为官方
`github.com/zhaoyswd/homeway-rs/releases/latest/download/SHA256SUMS`，从清单确定版本和
本平台产物，随后固定 tag 下载，使用第一次清单的 hash 校验。保留同版本提前返回、
解包、`--version`、备份、原子替换及进程重启的既有行为。

开工前设计检查：

- 不新增依赖、Token、镜像、配置或 HTTP 能力；GitHub 公共文件下载不使用 REST API。
- 清单严格 UTF-8、64 位十六进制、双字段、五种官方文件名；拒绝重复文件、多版本、
  不安全 tag 和缺平台，避免将清单内容拼成任意路径。
- 第一次清单是本次更新的唯一版本/hash 来源，后续不再访问 latest，避免发布时混版。
- 以可注入 GET 的下载准备函数做离线流程验证；任何下载/清单/hash 错误在暂存和替换前返回。
- 403/429 如实报告下载站点的 HTTP 状态，不将所有 403 判定为匿名 API 限流。
- 不改 wire/NAPI/时间窗/互操作判据、解包及进程编排，也不接触生产。

仓库要求的两道门在子任务中完成。读取了本机 reviewer skill，其触发要求是用户明确请求
外部评审，本轮没有发起 dsh；采用独立子 agent 的技术/代码审查。

独立技术门（`update_release_manifest/design_review`）已读 `update_cli.rs`、
`minhttp.rs`、`release.yml`，通过，无高/中级阻断。采纳的实施建议：全清单合法且 tag
唯一后才判断同版，同版在缺平台判断前返回；404 不推断仓库没有 Release；403 不断言
匿名 API 限流，429 才提示请求过频；兼容 CRLF、空行、大小写 hash 和 `*filename`。

## 验证与代码门

原始日志目录：`/tmp/homeway-update-manifest.PsamC5/`。

| 实际命令 | 结果 | 日志 |
|---|---|---|
| `cargo test -p homeway-cli update_cli::tests -- --nocapture`（首轮） | exit 0，13/0 | `update-tests.log` |
| `cargo test -p homeway-cli` | exit 0，CLI 210/0、2 ignored；集成 5/0 | `cli-tests.log` |
| `cargo test -p homeway-core minhttp::tests -- --nocapture` | exit 0，10/0、1 ignored | `minhttp-tests.log` |
| `cargo test -p homeway-cli update_cli::tests::github_release_preparation_over_real_tls -- --ignored --exact --nocapture` | exit 0，1/0；本机产品 minhttp 真 TLS 读取官方清单、下载 v0.3.24 darwin-arm64 归档 4,483,622 B 并校验通过，随后同版提前结束 | `update-real-tls.log` |
| `cargo test -p homeway-core minhttp::tests::github_latest_release_over_real_tls -- --ignored --exact --nocapture` | exit 0，1/0，产品 minhttp 真 TLS 读取官方清单 | `minhttp-real-tls.log` |
| `./tools/check-quic-isolation.sh` | exit 0，十一条全绿 | `isolation-zsh.log` |
| `cargo clippy --workspace --all-targets -- -D warnings`（修后） | exit 0 | `clippy-fixed.log` |
| `cargo test -p homeway-cli update_cli::tests -- --nocapture`（最终一行修正后） | exit 0，13/0、1 ignored | `update-tests-final.log` |
| `git diff --check` | exit 0 | 主会话工具回执 |

离线流程测试覆盖：五平台和校验格式、UTF-8/非法 hex/额外字段/不安全文件名及 tag、
重复和冲突产物、多版本、缺平台；请求 host 均为 `github.com`，首请求仅为 latest 清单，
后续严格固定首次 tag，latest 前移不会重新取清单；同版只查一次且仍验证完整清单；
两个下载阶段的 403/404/429/500 和截止错误；哈希不符不产出已准备归档。

真实网络烟囱仅调用下载准备阶段，不写二进制、不读取生产 state、不换装、不重启服务。
本批未跑完整 `ci-local.sh`/RRR（数据面和换装尾部未变），未推送、发版或部署。

### 失败与纠正

- 首次 `cargo fmt --all --check` exit 1（`fmt-first.log`）；随后误跑全仓 `cargo fmt --all`
  引入既有代码格式漂移。工作区初始干净，已逐文件恢复所有任务外格式改动，并恢复
  目标文件原有解包/进程编排/旧测试格式。最终只有预期文件变更；不将全仓 fmt 当作本批门。
- 首次将 zsh 脚本用 `bash tools/check-quic-isolation.sh` 执行，输出 `h: unbound variable`
  （`isolation.log`；shell 返回 0 不能代表门成功）。按 shebang 执行脚本后十一条实际全绿。
- clippy 首轮 exit 101（`clippy.log`），要求以 `as_chunks::<2>()` 替代 `chunks_exact(2)`；
  已作一行等价修正，并复验全 workspace/all-targets clippy 及 update 针对性测试。

### 独立代码门

独立审查者 `update_release_manifest/design_review` 读取全部 diff 与本记录，代码门通过，
未发现高/中/低功能问题。确认：清单借用、`HashSet<&str>`、`[u8; 32]` 与 thiserror 合适；
hex 转换前已有完整 ASCII 校验；tag 无路径/查询/片段/转义注入；所有下载准备错误在
暂存/换装前退出；minhttp 生产实现未变。无 Go 直译、多余 Arc/Mutex 或新增字符串错误链，
任务外格式漂移无残留。审查者只读，不将其结论冒充重复测试。
审查者最终补审确认 `as_chunks::<2>().0` 在 64 位 ASCII hex 前提下产生同样 32 对字节，
该一行 clippy 修正行为等价，包含在最终代码门通过结论中。
