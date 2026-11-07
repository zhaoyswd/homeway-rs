# 独立 WebServe 技术门增量复核（2026-11-07）

- 外评：`dsh --profile headless`，本仓根目录，真实退出码 **0**。
- 原始材料：`/tmp/dsh-review/r2-webserve-design.r9l79m3u/{prompt.txt,output.md,stderr.log,exit.txt}`。
- 已完整读取本轮输出；下面原文逐字保留。
- 本轮核查 B1–B8/P1/P2 整改及 C1–C12 映射，不把技术方案通过当作产品实现/测试通过。

## 第二轮外评原文

# 结论

**技术门通过，可开工。** 复核时 `HEAD=9a844f8`，产品代码零改动；仅有规划/评审与 `ROADMAP.md` 变更，本轮只读、未写文件。`openspec validate independent-webserve --strict` 通过。B1–B8 已在规划层闭合，未发现新的阻塞性矛盾；C1–C12 均有设计/任务落点。

## B1–B8

| 项 | 结论 | 复核依据 |
|---|---|---|
| B1 | 闭合 | 独立严格 `<state>/webserve.toml`，旧 `FileConfig`/`config.toml` 不加字段；基线 strict 兼容列入 `design:26,93`、`tasks:4.1`。 |
| B2 | 闭合 | `protocol.md` 固定 hww1（116B/159字符）、key（53B）、四帧握手、PTY、HTTP/WS 时限与关闭码；`tasks:4.4` 列入夹具/SUMS/判据，未伪验。 |
| B3 | 闭合 | `design:6,70` 要求 `RelayProc` 暴露实际 UDP `SocketAddr`、epoch、wildcard 归一；`tasks:3.3` 覆盖退让、`:0`、wildcard。 |
| B4 | 闭合 | `design:52,54` 统一 8 个 RAII 界面名额、未认证 WS 另 8 个、250ms 清理、票据持连接、消费复验及 rotate/目标关闭/relay epoch 撤销；`tasks:4.2,5.1`。 |
| B5 | 闭合 | `design:62` 四结果、原 generation 下 20ms `LegMissing` 重试、其余立即拒绝、仅成功才 Accept；`tasks:3.3` 覆盖交错/错代/配额。 |
| B6 | 闭合 | `design:54` + `protocol:4` 规定头 16KiB/32字段/5s、体 4KiB/5s、请求总 15s、禁 keep-alive、WS 64KiB、首帧 2KiB/5s、ping/背压；`tasks:4.2`。 |
| B7 | 闭合 | `design:60` 恒装配 worker + 可空可热换 `WebAuth`，首次 token 经控制面热安装、不重启出口、不启 HTTP；`tasks:4.1,5.1`。 |
| B8 | 闭合 | `design:76,78` 强制 `--no-spawn`、非阻塞 poll 取消、SIGHUP→500ms→SIGKILL→有界 wait、不遗留守护；`tasks:2.2,5.1`。 |

## P1/P2

- **P1**：`docs/reviews/2026-11-07-webserve-design.md:20` 已记录实际 `exit=0`、完整原文链接及 B1–B8 逐项处置；`tasks.md:3` 仍未勾选，正确。当前文档仍写“待独立复核”，需把**本轮通过结论及复核记录**补入后再勾 1.1；这是记录动作，不是技术返工。
- **P2**：默认 false 的事实已在 `docs/reviews/...:15`、`design:28` 订正；`unified_cli.rs:1217` 的 true 事件串仍在（产品代码零改动），设计已安排订正。`tasks:4.1` 只写“验证实际默认 false”，未显式列“改日志”，补一行即可，非阻塞。

## C 映射核查

C1 采用 `design:6/70 + tasks:3.3` 的 Handover/CtlConn 保留已验 fullpub，合法 ctl-only 可见；C3 在 `design:86 + tasks:5.2` 如实保留共享 TLS 握手前资源边界。C2/C4–C12 均映射到设计及任务：C2→6/72+5.1，C4→30+4.1，C5→42+4.4，C6→78+2.2，C7→100+2.1，C8→46+4.2，C9→64+5.2，C10→104+4.4，C11→102+5.2，C12→103+5.3。未发现影响 B 项的缺口。

## 新阻塞

**无。**

## 实施阶段检查项（非阻塞）

1. 任务 4.1 补“订正 `unified_cli.rs:1217` 事件串”，并加“默认配置 + 普通启动不装配出口/中继”断言。
2. C10 拆分/前移 4.4：判据登记先于 4.1–4.3，fixtures 在协议实现后生成。
3. 固定关闭码映射：`confirm_client` 的 `CapacityFull`→`0x75`，worker/入口额度→`0x73`；补 8 管理会话与 formal 32 交互测试。
4. `webserve token` 在 control 未就绪但实例锁被占时禁止离线写，避免在线热安装分叉。
5. 补 PTY 逐字节顺序、输出延迟、全部 `--no-spawn` 拨号路径，以及 wildcard v4/v6 实际地址选择测试。


## 独立补充复核

外评明确技术门通过。审后提出的管理关闭资源精确销账补充已由lead写入 design.md §5.1（68–76行）与 tasks 3.4/5.1；本节为独立评审者对该新增补充的静态复核，并非声称第二轮外部dsh曾读取尚未落文的补充。


### 审后补充独立复核与最终放行

- 实际复核时间：`2026-11-08T00:06:01+08:00`（文件名沿用批次开始日2026-11-07）。
- 已完整读取修订 design.md 的命令/认证/管理与 §5.1 清理方案、全部 tasks；独立执行 `openspec validate independent-webserve --strict`，实际 **exit 0**。工作区仍无产品代码修改。
- **目标侧清理通过**：conn_id 对应 alive RAII 活位，由叶子所有退出路径置false，同步引擎≤200ms sweep；不依赖可能丢弃的关闭通知。固定 remote+原 generation 下区分 network_owned 与管理 owner；只回收最后管理 owner 且非联网所有的同代腿。旧 lease 不能重取当前代际误删新腿；管理 owner 受全局8额度约束。实施时入队/回执失败、Accept写失败和重试期间也必须检查并撤销 lease，失活请求不能后到提升。
- **网关侧清理通过**：guard持 relay epoch/完整label/实际client源SocketAddr；显式绑定归一目的本机IP以避免wildcard源键。关闭时刻捕获 cutoff，relay按精确AssocKey与created≤cutoff清理；新代或端口复用的新assoc不被旧清理误删。队列满必须可见，目标活位清理独立有效。线上RELEASE方向保持。
- **验收覆盖通过**：tasks 3.4/5.1要求40轮连续登录/OPEN/关闭恢复assoc/正式腿/PTY基线，另验普通与多管理owner保护、同代移交/跨代晚清理/端口复用/满队列；没有把尚未实施的测试写作已通过。
- **r2非阻塞项已纳入**：2.0判据前置，2.1实例锁占用禁止离线写，2.2逐字节/输出延迟/全部no-spawn路径，3.3关闭码与配额，4.1修陈旧日志及默认false，5.1 v4/v6实际地址。

**最终技术门结论：通过，允许按本方案开工。** B1–B8、P1/P2已在规划层闭合，审后清理补充已独立核实，未发现剩余开工阻塞。本结论只放行开发，不代替产品测试、最终代码门或发布部署授权。

规划校验锚：
- `openspec/changes/independent-webserve/design.md` SHA256 `e45a1e41fe96bdfd57b3c3730220426ed0a9b95984f3b3377d6b14eeb0ebe13f`
- `openspec/changes/independent-webserve/protocol.md` SHA256 `07f8db3bd98b6d241186d0f2f4192274db64e7b3f470f8ebf8bc83ea17114e1d`
- `openspec/changes/independent-webserve/tasks.md` SHA256 `b4ff07889cf3ffe991a0a4914eabe026acedffc34be3e427481a251cb05ec55f`
- `openspec/changes/independent-webserve/proposal.md` SHA256 `2fb0ef526b5fa1cb855a0ae261e9880890f2e9e7dca34a517f488dbfe748a03c`
- `openspec/changes/independent-webserve/specs/webserve/spec.md` SHA256 `fc4e5cdef2562bb41e403c07d953dcffb24fb4760008f89fe798f3363cfa1247`
