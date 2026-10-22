# v0.3.20 本机出口部署评审（2026-10-22）

## 范围与依据

用户新指令「部署在这台电脑」授权更新版本、提交 / 推 main、在 main 头打 tag、GitHub Release、官方件核验、私有验收及本机滚动换装。目标仅 Mac `/opt/homebrew/bin/homeway`，由 `me.zhaozhe.homeway-exit` launchd 零参托管；阿里云和手机不操作。

已读两仓 `AGENTS.md`、homeway-rs `ROADMAP.md` 评审协议、`docs/DEPLOY-RUST-EXIT.md` §13.17、`docs/QUALITY.md`，并核对 Release workflow、`tools/local-rust-exit.sh`、`tools/quality-e2e.sh` 和现存 `/tmp/homeway-v0.3.19-private-acceptance.py`。本子任务只做评审与记录，不提交 / 推送、不操作生产。

## 技术评审（部署前）

结论：**方案通过，可以按下列既有发布门顺序执行。** 版本范围为 v0.3.19 → v0.3.20；质量功能为被动观测，无 wire / NAPI / 手机版本协同升级要求。沿用默认 state 和原监督方式；失败回退官方旧 binary，保留当前身份、配置与凭证。

必要检查与风险处置：

1. **来源与版本闭环**：Cargo workspace / lock 内本仓 package 版本及 CHANGELOG 同步；质量代码及修正、评审、文档在发布提交中。推 main 后校验远端头和 tag 目标一致，再推 tag；等正式 Release 所有目标成功。下载官方 darwin-arm64 归档及同 Release 的 SHA256SUMS，校验归档，解包核对 Mach-O arm64、codesign、`--version = v0.3.20`，记录 binary SHA256。代码与产物身份均不能只凭文件名认定。
2. **官方件必须实际参加私有验收**：现有 `tools/quality-e2e.sh` 无条件构建并设置 `HOMEWAY_BIN=target/debug/homeway`，直接执行不能算官方 Release 件验收。部署执行者应以已校验的官方 binary 显式覆盖 `HOMEWAY_BIN`，调用 `local-rust-exit.sh start/token 94` 和真实 `quality_history_e2e` ignored 测试，再用官方 binary 的 `serve quality --state 私有目录` 读回 JSON / 文本；临时封装可以复用脚本但不得保留覆盖到 debug 件的步骤。需记录私有进程路径或 hash，避免误连残留实例。端口退让必须按脚本硬门拒绝。
3. **验证默认装配而非只有 serve 子命令**：另外以隔离的私有默认 state / HOME、独立端口与关闭 UPnP/STUN/relay 的私有配置执行 unified 零参启动，确认 term.sock 和质量采样；对私有 term.sock 运行现存原生终端验收脚本，保留通过计数和存活证据。避免默认端口 / 公网公布碰到正式实例；测试 state 明确位于 `/tmp`，不得复用生产密钥 / token。
4. **备份可回退且不泄露凭证**：换件前备份现役官方 binary 和 state（排除 cache / socket），目录 0700、敏感文件 0600。身份 key、config、tokens 等清单仅私有保存；公开记录只写一致性结果和 hash，不输出 token / 私钥。换装后 key / config 必须保持；tokens 至少保留原字节前缀，合法追加与整本改写分开核对。
5. **替换与单次重启**：确认目标 binary 是普通文件还是符号链接、权限与 launchd ProgramArguments 符合预期；在目标同目录暂存校验后的新件，再原子 rename。成功路径只 `launchctl kickstart -k` 一次，不额外启动第二个正式进程、不更改 plist / state 参数。记录新 PID、执行路径和目标 SHA256 与官方件一致。重启有短暂中断，保持单机窗口有界。
6. **正式验收保持被动**：等进程稳定、41641 UDP、files / term 等既有服务装配，至少等一拍 5 秒质量历史。只读 status / 日志 / quality，核对文件权限、heartbeat / 覆盖时间与新日志错误。自然手机 peer 可作为现场连接观测；不人为注册额外客户端，不把只读 status 或无目标样本当作真机双向流量证明。无自然业务流时明确报告当前只验证采集运行。
7. **错误立即回退**：官方件校验或私有验收失败不进入换装；换装后若启动、端口、原服务或采集失败，使用已备份且 hash 核实的 v0.3.19 官方件同目录原子恢复并再次 kickstart，保留现行 state。若质量正常但暂时没有自然业务，不因无目标样本误判失败；反之质量持续写盘失败必须明确定位，不能以进程存活代替功能通过。

## 代码 / 执行复核清单

质量实现两道门和不可用字段修正已在 `2026-10-22-quality-{report,quic,flow,cli,integration-review}.md`，部署批不重复实现评审。当前功能基线 `3ed7ff9`，尚需根会话把版本 / lock / CHANGELOG 与本记录纳入正式 v0.3.20 发布提交。

- 核对发布改动只涉及所需版本和记录，不引入临时路径、凭证、debug 构建产物或生产 config。
- 核对私有验收进程确为 Release binary；质量测试客户端从本仓构建可接受，但出口端必须官方件。
- 核对停止私有实例、清理仅限其明确 PID / state，不使用泛名 pkill。
- 核对生产 swap 前有备份及旧件哈希，swap 后新件哈希 / PID / launchd / 端口 / 服务 / 采样成立，凭证保全结果有私有清单支撑。
- Rust / Go 直译项：本部署批只改发版元信息与记录，不新增实现；沿用单所有者 worker、有界队列、typed Option 语义，无新增并发结构或协议分叉。

执行结果由根会话完成后补录；本记录的技术通过结论不表示正式换装已经完成。

执行校准：会话跨至2026-10-23，Release记录使用该日。根会话按现行部署手册§5补齐正式终端验收：沿用原生18例，在私有实例1000次断开通过后，正式term.sock只运行32次断开及唯一命名空间的独立PTY/surface/resize/KILL测试，随后核对本次会话归零；不注册额外VPN客户端，不操作他人会话，LIST输出不记录他人名称。质量采集、端口与状态验收仍为只读，且不冒充手机L3实测。

### 发版代码门结果

已独立检查工作区实际 diff：`Cargo.toml` workspace version 从 0.3.19 → 0.3.20；`Cargo.lock` 的 homeway-capi / cli / core / quic 四个本仓 package 同步，未发生第三方依赖变化；CHANGELOG 仅追加本批质量功能、不可用字段、测量边界及仅 Mac 部署范围。无临时路径、凭证、binary 产物、服务配置或 wire / NAPI 改动。**代码门通过，无阻碍发版的发现。** 官方产物验收与正式换装属于后续执行证据，由根会话入部署记录，不混同代码审查结论。
