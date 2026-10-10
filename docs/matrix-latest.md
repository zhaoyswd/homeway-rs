# 互操作矩阵最近一次运行（生成：2026-10-10 14:59:18；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| RRR | R-ready | PASS | /tmp/homeway-rs-matrix/RRR/relay/cache/relay.log:2026-10-10 14:53:59.542 [relay] 中继控制面：TCP 0.0.0.0:4 |  |
| RRR | E1 | PASS | 2026-10-10 14:53:59.986 [homeway] serve 就绪：quic=:42668（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802  |  |
| RRR | X1-reg | PASS | 2026-10-10 14:53:59.987 [homeway] 中继：注册成功（腿 42668 → 127.0.0.1:42757）—— 客户端可经它到达本机 |  |
| RRR | R3-backend | PASS | 2026-10-10 14:53:59.987 [relay] 中继：后端 518bb76c43cd3591 注册成功（腿 127.0.0.1:42668） |  |
| RRR | C-ready | PASS | warmup pong: 就绪（判据=quic） |  |
| RRR | C-via | PASS | 服务会话: 路径确立：直连 192.168.3.12:42668（首个完成握手） | （胜者耗时 0s） |
| RRR | C-direct-usable | PASS | 赛跑胜者本身即直连（无需打死中继对照） |  |
| RRR | E7 | PASS | 2026-10-10 14:54:01.561 [homeway] peer: + dev=475afc66 pub=6749f26b ip=100.64.197.156 n=1/32 |  |
| RRR | E13-speedtest | PASS | speedtest: 摘要 down=620Mbps up=622Mbps； | （复核第 0 轮命中） |
| RRR | F-100MB | PASS | sha256 双侧一致（2202ae730c6c88a0…） |  |
| RRR | E10 | PASS | transit: 经隧道拨 192.168.3.12:42807 成功（收 58 字节） | （QUIC dial 腿：客户端往返行；出口 intercept transit 行随 S4 收窄） |
| RRR | E11 | WARN | （QUIC dial 腿收口行受出口 #17 节流——证据 = M4 quic-pf-e2e 流收口/泄漏判据 + E10 客户端往返行） | E11-QUIC-DIAL |
| RRR | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| RRR | DNS | SKIP | （dnstest 已退役——M5 §2.6-G6：岛无 UDP socket 服务面；无判据面可采） |  |
| RRR | DBG-log | PASS | 2026-10-10 14:54:01.561 [homeway] peer: + dev=475afc66 pub=6749f26b ip=100.64.197.156 n=1/ |  |
| RRR | TOTAL | PASS | 315s |  |

**结论：全绿**（豁免：WARN 1 / SKIP 1——各降档的独立证据绑定见备注列）
