# R6 前置批评审记录（①⑤ 深挖/修复的代码评审；2026-10-04）

## 渠道与对象

- **渠道**：`dsh --profile headless` 外部评审（reviewer skill 流程；轮次目录
  `/tmp/dsh-review/r6pre.X8uSxs`，prompt/output/stderr 三件套存档）。
- **对象**：R6 前置批 ① rekey stall P0 与 ⑤ 下行吞吐 0.42× 的根因分析与修复
  （附 ② KNOWN-GAP 转正、③ 判据第三态、④ perf-ab 口径、⑥ 词表低危）。
  commit 范围 804382a..ef59930（6 个 commit）+ tools/rekey-check.sh。
- **说明**：dsh 会话在等待其自建的时序验证实验（/tmp/rv-il-exp，42648 实例）预测
  窗口时超时终止，最终报告未落 output.md；但其 1945 行推理流（stderr.log）完成了
  对全部五项评审重点的独立验证，评审发现以推理流为准整理如下。评审者**独立复现了
  互踢实验**（自建拓扑）并验证了 wireguard-go 端点翻转路径与 120.0s 精确 rekey。

## 评审发现与处置

| # | 发现 | 严重度 | 处置 | 落点 |
|---|---|---|---|---|
| 1 | rekey-check.sh 判据②两个假绿向量：(a) 同轮竞速腿可独立握手 ⇒ 初始即 2 条「Received handshake initiation」；(b) 共享 exit 实例上并发第三者的 initiation 计入（评审者自身的 files rm 实验无意间实证——exit 9 混入了另一 peer 的握手行） | **高** | **修**：判据②按本轮 peer 标签过滤（日志行 `peer(X…)`）+ **rekey 时间窗断言**（首握手后 100–170s 内必有 initiation——两向量都无法伪造该分布；REKEY_AFTER=120s 邻域实测 120.0s 精确触发） | `68e9076`；run3 实跑 PASS（initiation ×2 + 时间窗命中） |
| 2 | rekey-check.sh 上传腿 `$(tmo … 2>&1)` 无管道形态：zsh 命令替换嵌套 tmo 的 wait 不返回 ⇒ 脚本空转到 BUDGET=687s；DT 计时失真 3.3 倍（682s vs 实际 212s）——**会掩盖真实减速**（「改判据口径」类风险） | **高** | **修**：直白后台 + 1s 轮询收尸 + 到点强杀；下载腿同款；tmo 函数删除防再踩 | `68e9076`；run3 DT=215s 计时准确 |
| 3 | rekey-check.sh 首跑实测「start 返回 0 但进程未存活」的环境竞态 → token 取不到 | 中 | **修**：启动后显式验活（sleep 1 + status 复查，失败红） | `68e9076` |
| 4 | perf-ab.sh 中止/失败路径泄漏 RSS 轮询进程（v1 中止轮的 poller 存活混入下一轮——rss.tsv 行率翻倍污染口径，评审者从进程 etime 实证） | 中 | **修**：EXIT/INT/TERM 收口 trap（幂等） | `68e9076` |
| 5 | Rust CLI 的 files/speedtest 动词与常驻 connect 同 identity 并发 = **产品面 foot-gun**（CLI 无检测/警告；Go 侧 daemon 单会话结构上不存在）——R6 term 期继续用 CLI，R7 后 App 常驻核无此面 | 中 | **登记**：INTEROP-CRITERIA 的 KNOWN-GAP 族注记已有形态描述；CLI 侧加锁/警告归 R7 CLI 面治理（与 hostsession/常驻会话设计一并定） | ROADMAP R6 前置批处置表 + R7 范围注记 |
| 6 | 「L2 能过 L3 不能过」的概率解释自洽但未量化（评审者独立推算 P(hit)≈40%/链路，与轮 4 的 2/3 Rust 链路过吻合；建议的反证实验——旧脚本反复跑 L2/L3/L6 统计失败率——因修复已消除隐患只能对旧脚本跑，未执行） | 低 | **接受现状**：互踢机制已有实验 A 直接复现（时序+日志逐字对齐），概率模型只是补充解释 | 本文件记录 |
| 7 | §8 的「随机采纳」措辞：评审者认为更可能是**按客户端实现的系统性差异**（Go/Rust 的候选排序或竞速实现差异导致各自稳定采纳某一端点）而非纯随机——不影响结论（artifact → 强制同路径对照） | 低 | **措辞接受**：PERF-AB §8 措辞为「赛跑随机采纳」——保留（两轮实测确有轮次间差异），系统性成分不改变修复方向 | — |

## 评审者独立验证面（看过、确认成立的部分）

- **① 根因主张成立**：wireguard-go `BeginSymmetricSession`/`ReceivedWithKeypair` 的
  keypair 链顶替路径（noise-protocol.go:678-720 + receive.go:458）与 boringtun 15s
  自愈时序（timers.rs KEEPALIVE+REKEY_TIMEOUT）的互踢推演复核通过；实验 C 的
  120.0s 精确 rekey 在评审者自建实验中复现。
- **⑤ 根因主张成立**：GGG/RRR 轮数据与 rounds.tsv 复核；§8 表格数据与产物一致。
- **② 转正主张**：两侧客户端共有失败的实验证据链完整。
- **③ 第三态**：SAMEHOST-LIMIT 的动态交叉引用逻辑（TABLE_ROWS grep）无结构性漏洞。
- **⑥ 词表门**：死分支删除未改变门禁语义（两臂同文的 if/else）。
- matrix.sh 停 c-main 窗口对后续判据无破坏（E10/E11 的 --dial 重启承接复核）。

## 本轮评审结论

**过**（2 高 + 3 中全处置：2 高与 2 中即改即验，1 中登记 R7；2 低接受现状/措辞）。
评审过程中评审者自身的实验无意间成为判据②假绿向量 (b) 的活体实证——评审产出
直接转化为修复依据。
