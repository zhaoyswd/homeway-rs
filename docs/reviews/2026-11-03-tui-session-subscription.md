# TUI 全会话元数据订阅（2026-11-03）

## 范围与根因

本轮接续用户明确要求，把整个当前主机会话列表改为事件驱动。此前 500ms LIST 已改善
刷新，但未打开会话仍需重复连接/全表查询；当前 surface 的 STATE 只属于一条 PTY 腿，
不能作为其它会话的变化源。订阅必须独立于终端 attach、输入/几何、探活及用户通知。

当前目标主机只开一条 SUBSCRIBE 元数据连接。服务端初始发送完整精简列表，创建/结束、
标题/OSC7、有效 OS cwd、agent/state/agentLabel 实际变化立即唤醒订阅者，之后推送完整
最新列表；不新增高频全表比较，不跟随 lastActiveMs、普通输出、clients 或几何变化。
cwd/agent/state 的既有生产采样 SAMPLE_PERIOD=1s 不变，需区分检测到变化后推送与
实际 cd 到下一次采样的时间。订阅不 HELLO、不注册 PTY 腿、不消费屏幕/接管尺寸。

## 技术评审与协议选择（开工前）

非实现者 design_review 子代理独立技术门通过，core_subscription 调查实际生产点后
实施 core/wire，tui_live_sessions 实施 CLI。主要评审决策：

- 同锁捕获 metadataRevision 和列表。旧 LIST 顶层追加该键，订阅 JSON/header 值一致；
  两来源按服务端 revision 单调合并。只比较本机收到顺序或“LIST 请求后是否有推送”
  仍会被迟到的旧推送回滚，因此没有采用。
- 支持订阅时左侧以版本化 watch/LIST 为唯一权威；STATE/SNAPSHOT 继续更新右侧。
  两条独立连接没有共同 STATE revision，不能声称本地事件计数解决跨流因果。旧端无
  订阅时保留上一单元 STATE 同拍左侧更新和逐字段 LiveMetadata 保护。
- revision 仅同服务生命周期单调。每次新订阅连接和首完整基线均失效之前的 LIST epoch，
  不用过渡 LIST 建立 watch 基线，服务重启低 revision 可恢复。切主机丢弃旧订阅对象、
  队列与连接，旧 target/epoch 仍过滤；surface generation/PID/恢复门不变。
- 服务端最多 16 订阅，仍受原总连接额度约束；慢消费者无待发组队列，写完当前组后仅取
  最新 revision。服务锁内只捕材质，序列化/分片/网络写均在锁外；完整组共享原 10 秒
  WRITE_TIMEOUT，空闲最多 200ms 检查 EOF/异常上行/stop，取消不等待 5 秒心跳。
- 5 秒同 revision 完整心跳，客户端只在完整合法 JSON 及 revision 双确认后延续 15 秒
  下行静默预算；片段不续命，不宣称完整双向探活。当前 surface 原同腿 LIST 探活不变。

只增 feature SESSION_SUBSCRIPTION=1<<8、SUBSCRIBE=0x18（空首帧）、SESSION_LIST=0x19。
每片 `[revision:u64LE][flags:u8][JSON chunk]`，bit0=MORE、chunk≤32KiB，最多32片、
完整原始 JSON≤1MiB，不 gzip、不依赖65535字节帧截断。新 typed assembler 拒绝零/
回退/组中变换 revision、未知 flags、超大片/组及非法头；重连重建 assembler。精简字段
仅 name/title/agent/agentLabel/stateV2/cwd/createdMs/pid，创建排序字段与稳定 PID 保留。

兼容边界：旧客户端忽略未知 features 和 LIST 新键，旧 wire 字段语义不变；新客户端只在
协商通过后发送 SUBSCRIBE。旧服务器缺能力时不收到新 op，回退500ms LIST，每30秒再次
检查能力。健康订阅每30秒独立 LIST 校验，持续事件不能后移该周期；只以服务端 revision
合并，旧单帧 LIST 的失败不覆盖健康分片列表或操作提示。订阅拒绝/失败时转独立500ms
LIST 降级域并重试，重试等待在途 LIST 完成，避免1秒重试反复作废最长3秒查询。

常量真源：core session_watch.rs 的1MiB/32KiB/16订阅/5s/15s；service.rs 原10秒写预算
及新增200ms取消检查；TUI mod.rs 的30秒校验/能力复查、1秒订阅重试、旧500ms查询和
2秒查询失败退避。clone/JSON各有字节闸，编码还存在分片/Vec临时材质，不能把“16×1MiB”
误称全部进程内存上限；这些临时材质有界、无持续排队。协议及行为变更已同批登记于
INTEROP-CRITERIA「2026-11-03：会话元数据订阅与 LIST revision」，更新 GREETING golden、
新session-subscription夹具、MANIFEST/SHA256SUMS、fuzz入口及派生种子摘要。

## 代码评审与整改

独立跨层代码门通过，无剩高/中危阻断，意见全部处置：

1. **H1**：新订阅开始时增 epoch 还不够，首基线前发出的旧服务 LIST 可在新基线rev1
   之后晚到rev99、抬高版本并永久压制新服务rev2…98。接受并修复：首基线也增epoch，
   并补“基线先到、旧查询后到”的反向竞态回归。
2. **M1**：每次推送重置30秒 due 会让持续事件永远推迟兜底。接受并修：只有首基线和
   实际查询设置 due，普通变化不后移；连续revision更新跨due仍发LIST有回归。
3. **M2**：容量满/订阅失败静默卡住初始列表。接受并修：失败转新LIST-only域，500ms
   降级读取；查询失败仍走既有可见错误。随后发现1秒重试可能反复失效耗时更长的查询，
   再修为等待该在途查询完成，due重试也优先于新自动LIST。无混合旧域数据。
4. **M3**：>64KiB列表订阅可分片成功，但旧LIST仍会截断，不能让周期性解析失败永久
   污染健康列表提示。接受并修：健康watch下旧LIST只是best-effort校验，其错误不替换
   数据或操作notice；新版本化推送仍权威，断线后的LIST降级保留旧单帧能力限制。
5. **L1**：同revision心跳中，本机ps新确认self_session不属于服务端revision。接受并修：
   等revision仅可补同PID本机新确认的承载会话禁选标记，不重放标题/state或重复通知。

独立门同时核创建/结束/OSC7/title/effective cwd/agentLabel早退路径/state全部hook、
锁与Condvar不丢唤醒、慢消费者/额度RAII/取消、严格组预算、无HELLO/屏幕/探活混淆，
以及Rust形态：无逐行移植、字符串错误或无界队列。订阅是单独可取消对象，latest槽只留
一个全量；相等revision心跳不重复触发用户通知。技术/代码门均为独立只读审查，实际
运行由实现子任务取回退出码，不以日志存在替代命令成功。

最后限定增量门亦通过：独立评审者实际读144/0/1 ignored CLI日志，核同revision本机
self_session修正/serde skip边界、测试as_chunks lint替换、新烟囱最终5 PASS及完整
de65709a件SHA、legacy仅代理模拟、原PID/TTY/严格清理，以及600KiB慢读单target
verified 1/0。无待处置项，无追加产品或全套重跑要求。

## 实际验证

Core针对性 `cargo test -p homeway-core term:: --offline` 实际exit0，**229 passed /
0 failed**；日志 `/tmp/homeway-subscription-core-term.log`。订阅3项真实socketpair测试含
初始空列表、无附着create、真实PTY title/OSC7、sample state/agentLabel、exit、同rev
5秒心跳、EOF名额回收、16额度/非空payload/异常上行。codec另4项覆盖完整1MiB跨帧、
同rev心跳、坏组/复位、序列/片数/字节闸、JSON转义展开限额及定值夹具；scan独立metadata
dirty测试不改变原STATE发布语义。fuzz_term_frames的ignored回放已显式执行exit0。

CLI初次既有测试实际exit0 **134/0/1 ignored**，新增订阅后140/0；评审逐项补回归后
最终 `cargo test -p homeway-cli tui:: --offline` 实际exit0 **144/0/1 ignored**，日志
`/tmp/homeway-session-subscription-cli-verified.log`。新增测试含>64KiB真实FrameIo分片、
无HELLO、旧feature不发送命令、drop立即断读、半组持续滴流不续期限、两源乱序、低rev
重启/旧target、首基线后迟到旧LIST、固定30秒due、通知一次、故障LIST-only/慢查询
不被重试饿死、健康大列表校验失败不污染notice及同rev本机self_session保护。

完整默认quick门首轮 `/tmp/homeway-session-subscription-ci-local.log` 实际exit1：workspace
全部通过后，clippy拒新codec测试的chunks_exact(2)，要求新as_chunks API。仅修测试解析
为as_chunks::<2>().0.iter()，保留原失败，不降lint。最终完整重跑日志
`/tmp/homeway-session-subscription-ci-local-verified.log`，实际取回 **exit0**，末尾
**2026-11-03 00:51:17 quick全绿**：

- workspace顶层 **1491 passed / 0 failed / 41 ignored**；日志另有1条子进程sentinel
  成功摘要，不重复计入。CLI最终目标验证144/0/1 ignored也已实际exit0。
- workspace/all-targets clippy、OHOS交叉check与真link通过；libclientcore.so
  **2,943,792 B**、20面导出及版本注入核验通过。
- QUIC隔离11条、向量确定性/fixtures SUMS、178件派生种子摘要、词表及WG残留门全过。
- release构建与私有RRR **316秒**完成；100MB双侧SHA一致、E10往返及FB-files通过，
  既有E11收口节流WARN和DNS形态不适用SKIP如实保留。未跑--full。

最终debug build实际exit0，日志 `/tmp/homeway-session-subscription-build-verified.log`；
私有烟囱使用前后校验同件SHA，不安装到PATH或生产目录。

## 私有端到端与最终收口

新 `tools/tui-session-subscription-smoke.py` 复用既有私有PTY/故障代理工具。代理区分
首帧LIST/SUBSCRIBE与当前surface同腿探活LIST，能够只断元数据流、hold新订阅、模拟
旧feature并删除LIST新增revision键。所有state在/tmp，serve/relay关闭；OSC通知进入
私有PTY捕获，不发送真实系统通知；ExitStack回收自有PTY/代理线程/daemon。

第一轮推送段通过，随后工具把CLI删除动词误写kill（实际delete）失败，保留
`/tmp/homeway-session-subscription-smoke-initial.log`。只修工具动词，第二轮实际exit0、
**5 PASS**，日志 `/tmp/homeway-session-subscription-smoke-verified.log`，同件SHA
`8f248cab40e350125290dd4ae329cb11e15678e95798961eb18ef88f145f8d1a`。背景四标题
本地观测0.088–0.138秒，后台create/title/state/cwd无需LIST轮询或后台TUI HELLO；
只断watch后恢复全量补齐title/create/exit；模拟旧端周期LIST回退；TTY恢复且PTY保留；
资源清理和SHA一致。此件早于最后慢查询/self_session修正，最终件增强断言再跑并在此
补录，不能拿早件冒充最终源码验证。

最终增强烟囱实际 **exit0、5 PASS**，日志
`/tmp/homeway-session-subscription-smoke-final.log`；最终件前后 SHA256 同为
`de65709a20aa8f60fb2c6fafa588f54be5fdfc9156b3d9284a773c5c1c6f586c`。背景四标题
本地观测 **0.071–0.178秒**；健康首帧 **LIST 1→1、订阅1、TUI HELLO1**，证明没有
500ms全表轮询和后台会话attach。仅断元数据流后，新订阅的完整baseline必须实际包含
gap-title/gap-new、缺少已删background；surface HELLO次数保持，不用降级LIST先刷屏
冒充订阅恢复。模拟旧端首帧 **LIST 7→10、新SUBSCRIBE0**；这是代理去feature及新键的
旧wire模拟，未运行历史二进制。两TUI退出后TTY恢复、原active PID及gap-new仍在，代理
线程join、自有PTY/daemon回收及SHA前后对账通过。所有时延是本地样本，不推广为远端承诺。

按本轮要求补显式慢读服务回归，仅新增test、在quick的workspace阶段之后单独运行：
`subscription_slow_large_group_keeps_state_and_fast_peer_live_then_coalesces` 最终实际
**exit0、1 passed / 0 failed**，0.10秒；日志
`/tmp/homeway-subscription-slow-reader-verified.log`。600KiB JSON跨多片，慢端用
SO_SNDBUF=4096、读首片后停读形成真实阻写；其间三次元数据更新仍能取state锁、快端
完整取得最新rev；慢端恢复后先完成旧组、下一完整组直接最新rev，无中间组排队。
只补同产品运行证据，无产品修改；最终debug/SHA及已过烟囱关系保持，不重跑完整quick。
实现子任务已实际取回该target session退出码0。随后quick因新增cfg(test)源码重建debug，
当前产物SHA变为 `b2fcf566d15134fb8326ace36aee6c281050e49206625851d953809f5971388d`；
不能把当前文件误称仍是de65709a。烟囱当时de65709a前后相等的证据有效，产品行为源码
在该烟囱后未变，新增仅测试；不为调试信息/重新构建摘要变化扩测或安装此本地构建。

本轮未推送、建tag、发Release、安装或部署；不触默认state、生产出口、tier跟踪文件。
README已同步最终用户行为，主会话负责ROADMAP及中文单元提交。
