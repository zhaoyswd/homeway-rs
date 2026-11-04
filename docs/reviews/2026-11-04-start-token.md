# start 成功后显示 token（2026-11-04）

## 需求与技术评审

用户要求 `homeway serve start`、`homeway relay start` 直接打印 token，省去随后
执行 token 命令。基线 `e483eaa4fa33dfb4b3a4ce9a10c933340e425f95`。

按 ROADMAP fallback 由非实现者 `design_review` 子代理独立只读技术评审。
reviewer skill 自身仅在用户明确请求 review 时触发，本轮未调用外部 dsh。
原方案是成功 start（含 already）后用同一个控制连接请求现有 token op，复用输出，
只对空台账重试。技术评审发现首启引擎会先落一条空端点、无 RPK 的占位 token，
所以还必须等待 serve 结果出现非空端点，避免立即打印不可用凭据。该高危项与帮助
仍宣称完整凭证只经 token 命令的低危项已按意见修改，技术门通过。

最终设计：start 原 action 行保留；接着读对应 token op，成功沿用独立 token 命令
的完整串与端点/来源渲染。serve 空端点结果、精确空台账错误可重试，其它错误立即
提示；绝对 20 秒预算含控制请求、100ms 轮询间隔，仅为 CLI 等待预算，不改变引擎
既有 15 秒探测兜底、签发时序或协议时间窗。取 token 失败清楚提示角色已启动及
稍后可执行的 token 命令，保留启动成功的 exit0。start 失败不取 token；restart、
stop、status 不新增凭据输出，独立 token 命令的失败退出和离线回落保持。

输出来源仍是既有 serve 台账末行 / relay 运行态优先契约。start 与 token 是两个
独立控制请求，不保证并发停止、重建或端点变化时的原子快照；本轮不改变旧的非空
台账可能在新一轮发布前暂时陈旧的语义，也不新增运行态 token API。

## 验证

- `cargo test -p homeway-cli --offline` **exit0**：CLI 单元 **228 passed / 0 failed /
  2 ignored**，既有配置/参数集成 **5 passed / 0 failed**。日志
  `/tmp/homeway-start-token-tests.log`。配置 fail-fast 测试内四次实际首启因等待既有
  15 秒 token 兜底，集成总计 63.43 秒，未跳过或缩短产品等待窗。
- `cargo clippy -p homeway-cli --all-targets --offline -- -D warnings` **exit0**。
  日志 `/tmp/homeway-start-token-clippy.log`。
- 临时专项 `python3 /tmp/homeway-start-token-smoke.py` **exit0，7 PASS**，日志
  `/tmp/homeway-start-token-smoke.log`。后台 start 需要 control.sock，现有
  `local-rust-exit.sh` / `local-rust-relay.sh` 只起前台单角色，无法覆盖本路径；因此
  本轮沿用既有 `qh_config_failfast.rs` 的私有统一进程夹具形态：全新 `/tmp` state、
  随机回环端口、禁 STUN/UPnP、临时文件根、退出清理，未接触生产实例。
  - 首启 serve 真实等待 **15.13 秒**，输出单条完整 token，与独立 token 命令一致，
    没有打印空端点占位；重复 start 的 already 输出同一凭据。
  - relay 首启与 already 均输出凭据，与独立 token 命令一致。
  - 两角色 status / restart / stop 不出现完整凭据；坏 config 的 start 均 exit1，
    不显示 token。
  - 单独私有进程仅用既有 `HOMEWAY_TOKEN_FALLBACK_WAIT_MS=60000` 测试缝延迟签发，
    实测 **20.07 秒**结束等待、给出稍后查看提示、start 保持 exit0，角色仍 running。
  - 两个私有进程均 SIGTERM 优雅退出，临时 state 已删除；日志不记录完整凭据。
- `git diff --check` **exit0**。未新增持久测试或通用起服务脚本；未跑完整
  ci-local/RRR，未安装、推送、发版或部署。

## 代码评审

同一非实现者 `design_review` 子代理独立完整复核最终 diff，代码门通过，无新增
高/中/低问题。确认首启占位过滤与帮助整改落实；绝对截止涵盖锁等待、帧写入和
应答；其它控制错误不重试；只在成功 start 后取凭据且 already 覆盖；无新 wire
字段、锁、额外 Arc 或无谓拷贝，无逐行移植痕迹。静态评审不替代下述实际验证。

预算边界：截止恰好耗尽时可能走 request_deadline 的 timeout 提示，而非“仍在
准备中”提示，两种分支都清楚表达角色已启动、token 未取得及稍后查看方式。
