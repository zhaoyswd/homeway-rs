# WebServe 本地 CI 发现的准入取消竞态：独立技术核对

实际核对时间：`2026-11-08T00:59:27+08:00`。此项由本批 `ci-local` 的旧 QUIC 测试失败调查引出；不是把该测试
直接标记为已知 flake，也不把下面受控复现与原 CI `RegistrationFailed` 宣称为已证同因。

## 核对材料

- 产品基线 `bd00c0c` 的私有树 `/tmp/homeway-web-ladder-baseline-f3jtmxkb`。
- 原受控复现 `/tmp/homeway-web-ladder-baseline-controlled.log`，实际 exit 101：故意暂扣已超时
  旧 Proof，在新 Proof 后按 new→old 回执；新连接先成功，随后被 `replaced by newer registration`
  关闭。原诊断要求匹配 RegistrationFailed，实际签名不同，所以该退出码不能包装成原 CI 根因证明。
- 仅在旧连接 bind 前加 close guard 的私有实验 `/tmp/homeway-web-ladder-baseline-guard.log`，
  实际 exit 0，受控新连接不再被旧裁决替换；此单点 guard 仍未保护引擎触表副作用。
- 实读 `exit/bridge.rs` 的 Reg4Request/bind、`exit/conn.rs` 的 ask_engine/Accepted、
  core `server/engine.rs::on_quic_inbound/admit_reg4/apply_dev_ops` 和隔离门白名单。

## 独立意见

**可纳入本批 CI 发现修复，采用两层最小取消保护；不能只修 bind 前一个 if。**

1. `exit/bridge.rs` 原本就在异步白名单，已经持有私有 `quinn::Connection` 和 oneshot。
   Reg4Request 私存 Connection clone，公面只提供 `is_active() -> bool`，检查回执接收端仍开且
   `close_reason` 为空，不会让 core 引入 quinn/tokio 类型；8192 条有界队列限制 handle 寿命。
2. core 在 `admit_reg4`、`table.register`、`apply_dev_ops` **之前**丢弃已知 inactive 请求。
   这保护已取消的排队 Proof/Refresh，不把无消费者旧裁决重新落表。
3. `ask_engine` 同时等待 reply 与 `conn.closed()`，关闭后尽早 drop 接收端；两者同时 ready 时
   不依赖 select 分支先后决定安全性，Accepted 后还需检查关闭状态。
4. 最后检查宜放在 `Bridge.bind` 取得 bindings 锁后、任何移除/关闭现任绑定之前，并返回
   类型化/布尔成功给准入调用者。只在调用前检查会留下“检查后等待 bindings 锁”的窗口。
5. 保留真实边界：活性检查后已经开始的同步表事务不做回滚，不能宣传全局原子取消；quinn
   连接也可能在最后一次检查后被其他线程关闭。此修复保证已知失活请求不入队列触表/复活绑定，
   不改变有效活连接准入、现有协议字节、认证期限、MAC 试秘或原测试期望。

实施须验证：已关闭且 MAC 合法的 Proof/Refresh 不改变表/ops，close 与 reply 同时就绪，
活请求原行为不变，受控 fresh 连接不被旧裁决 `replaced`；隔离门、相关单测与完整测试仍需实际执行。
本记录为开工前独立技术意见，不是修复完成、测试通过或最终代码门的替代证据。
