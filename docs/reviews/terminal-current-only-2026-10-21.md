# 终端仅保留当前协议（2026-10-21）

本批遵用户新指令：尚未发布，不保留旧APP/旧出口兼容。不使用review skill；禁止提交、
推送、打tag、装机或部署。Rust范围为frames/service、注册表腿类型、按键平台声明及
core一次性终端操作客户端；CLI与APP由各自执行者同步，当前夹具迁移由renderer执行者负责。

## 门一：技术评审

- 保持当前协议版本值与尾帧基本结构，不再协商是否携带当前必需字段。
- GREETING必须恰5B、版本匹配，各消费模式要求其所需功能位；额外能力位不视为旧版，
  不错误要求 raw CLI 声明surface能力。
- HELLO尾必须是 `[capLen=1][caps][ver][idLen][id]`。caps的SURFACE/RAW_TERMINAL
  必须恰一位，PROTO_VER必有，KEY_ALT两个平台位必须恰一位。实例ID为1..64B合法UTF-8。
  缺失、混合、歧义、旧多字节OR能力块、裸ID、尾随垃圾一律拒收，不推断出口宿主平台。
- malformed映射既有bad_capability，当前结构的错误版本映射既有term_version；不新增wire
  错误码，不删除版本安全检查。保留raw终端当前能力与正常断线/故障收尾。
- 移除不可达旧APP腿类型Legacy。注册表直接接收App/Host枚举，不能由两项false能力
  猜测raw腿。LIST持续输出clients数组与stateV2，空数组也不可省略。
- 不误删终端ANSI/DEC兼容模式、键鼠协议、raw背压回放或HOMEWAY_TERM_VT=off的合法
  raw-only故障路径。去旧wire与VT标准仿真是两回事。

结论：按当前单版本严格字段实施；不引入迁移适配器、双协议解码器或猜测默认值。

## 门二：代码评审

- HelloTail删除caps_present/ver_present；成功解码已保证字段存在。encoder删除可选flag，
  始终编码版本声明和版本字节，不再静默丢弃ID；调用方显式声明终端类型和按键平台。
- 服务端直接使用当前枚举与平台声明，删除宿主默认、歧义计数/一次性告警及裸ID容忍。
  受影响service测试构造明确增加当前平台位，不在测试解码前偷偷补字段。
- 注册表LegDescriptor持有LegKind，删除Legacy与相关JSON词面，实例替换不再跳过空ID
  特例（边界层先拒空ID）。raw应答归属直接按Host枚举判断，去除重复能力bool。
- keyenc生产host_default删除；标准平台测试显式检查AltEscPrefix/AltNoEscPrefix。
  ANSI/DEC模式与协议字节逻辑未改，不能误把“legacy”标准协议名称当旧APP兼容删除。
- Rust core唯一终端客户端为facade/term_op的一次性LIST/KILL，不生产HELLO；现要求
  精确GREETING结构/版本和LIST|PROTO_VER能力，旧缺功能出口不再接受。无应答文案删除
  “升级增强版tailcat”历史指引，仍返回原有term_unreachable/term_version归因码。
- 当前raw CLI由独立执行者同步完整HELLO与raw所需GREETING位；APP由主会话同步。
  frames fixture由renderer执行者迁移为当前完整HELLO，不保留fixture专用生产兼容。
- Rust形态：类型表达App/Host两态，错误仍为FrameError/TermOpError；没有多余Arc/Mutex、
  运行时依赖、字符串错误替代枚举或Go接口仿写。

## 验证记录

- frames专项10/0；facade终端操作专项6/0，包含缺版本能力、尾随GREETING字节与错版拒绝。
- service严格握手测试覆盖缺caps/空caps/裸ID、缺PROTO声明/版本/ID、空ID、无平台/双平台、
  双终端类型/无终端类型；全部应为bad_capability且不能创建会话。
- CLI执行者回报专项29/0及clippy通过；不将其输出冒充本执行者运行结果。
- 首轮全term并行出现既有真实PTY环境波动：alt平台测试单独复跑1/0；attach_size触发60s
  登录shell硬期限，单线程复跑通过；clear_screen_skips_backlog收尾挂在child.wait/__wait4。
  私有进程采样在 `/tmp/homeway-term-current-sample.txt`，没有改PTY源码或触碰生产。
  挂住的本轮私有测试进程已终止，不把该轮登记为全绿；后续受影响门结果另行补录。
- 排除该收尸单例后单线程term **196/0**（34.21s），这是显式skip1的覆盖轮，不是全门。
- 最终功能冻结后重新核对：frames **10/0**、session **8/0**、keyenc **10/0**、严格HELLO
  服务回归 **1/0**、混合raw/surface应答归属 **1/0**、facade终端操作 **6/0**；
  `cargo clippy -p homeway-core --all-targets -- -D warnings` 与 `git diff --check` 通过。
- 残留审计：生产Rust源码无caps_present/ver_present、key_flavor_ambiguous、LegKind::Legacy、
  LegKind::of、host_default或混合部署兼容开关；LIST的clients与stateV2均无省略属性。

后续：主会话汇总APP与当前surface夹具迁移、跨仓构建门；本记录不授权发布部署。
