# 互操作矩阵最近一次运行（生成：2026-10-10 02:57:33；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| RRR | R-ready | PASS | /tmp/homeway-rs-matrix/RRR/relay/cache/relay.log:2026-10-10 02:42:40.323 [relay] 中继控制面：TCP 0.0.0.0:4 |  |
| RRR | E1 | PASS | 2026-10-10 02:42:41.409 [homeway] serve 就绪：wg=:42667（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 te |  |
| RRR | X1-reg | PASS | 2026-10-10 02:42:41.410 [homeway] 中继：注册成功（腿 42667 → 127.0.0.1:42757）—— 客户端可经它到达本机 |  |
| RRR | R3-backend | PASS | 2026-10-10 02:42:41.410 [relay] 中继：后端 f2103be06c706ba1 注册成功（腿 127.0.0.1:42667） |  |
| RRR | C-ready | PASS | warmup pong: 就绪（判据=quic） |  |
| RRR | C-via-direct | FAIL | 两轮（150s+60s）均未见直连 |  |
| RRR | E7 | PASS | 2026-10-10 02:42:42.986 [homeway] peer: + dev=6bbaa15f pub=727dd778 ip=100.64.215.154 n=1/32 |  |
| RRR | E13-speedtest | WARN | （环境抖动降档：四轮 timeout——独立证据 = 同轮 F-100MB/RL-files5MB sha256 硬对账行 + R5.md 在册最小复现 + PERF-AB 多轮中位） | E13-JITTER |
| RRR | F-100MB | FAIL | 对账不符：up=781b944ef656 dn=（会话建立失败：会话装配失败：岛建连失败：无候选可用（全候选未在预算内完成握手） / 会话建立失败：会话装配失败：岛建连失败：无候选可用（全候选未在预算内完成握手）） |  |
| RRR | E10 | FAIL | 25s 内客户端未见 transit 往返行 |  |
| RRR | E11 | WARN | （QUIC dial 腿收口行受出口 #17 节流——证据 = M4 quic-pf-e2e 流收口/泄漏判据 + E10 客户端往返行） | E11-QUIC-DIAL |
| RRR | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| RRR | DNS | PASS | （dnstest 已退役——M5 §2.6-G6：岛无 UDP socket 服务面；登记归 D 棒 S5） |  |
| RRR | DBG-log | PASS | 2026-10-10 02:42:42.986 [homeway] peer: + dev=6bbaa15f pub=727dd778 ip=100.64.215.154 n=1/ |  |
| RRR | TOTAL | FAIL | 890s（本链路有判据红项） |  |

**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log；豁免未计入红：WARN 2 / SKIP 0）
