# 互操作矩阵最近一次运行（生成：2026-10-10 04:01:35；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L5 | R-ready | PASS | /tmp/homeway-rs-matrix/L5/relay/cache/relay.log:2026-10-10 03:59:55.747 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L5 | E1 | PASS | 2026-10-10 03:59:56.833 [homeway] serve 就绪：quic=:42666（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802  |  |
| L5 | X1-reg | PASS | 2026-10-10 03:59:56.834 [homeway] 中继：注册成功（腿 42666 → 127.0.0.1:42755）—— 客户端可经它到达本机 |  |
| L5 | R3-backend | PASS | 2026-10-10 03:59:56.834 [relay] 中继：后端 579a499c38bfde9a 注册成功（腿 127.0.0.1:42666） |  |
| L5 | X1-okmac | PASS | 2026-10-10 03:59:56.834 [homeway] 中继控制面：中继身份已认证（OK-MAC 通过） |  |
| L5 | C-ready | FAIL | 25s 内未见「就绪（会话在位）」 |  |
| L5 | MP-n2 | FAIL | 60s 内未见 n=2/32 |  |
| L5 | RL-via | FAIL | host add 变体被拒（验证档问题——设计 §9 兜底） |  |
| L5 | DBG-log | PASS | 2026-10-10 03:59:59.464 [homeway] peer: + dev=771d539a pub=4309daea ip=100.64.23.118 n=1/3 |  |
| L5 | TOTAL | FAIL | 95s（本链路有判据红项） |  |

**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log；豁免未计入红：WARN 0 / SKIP 0）
