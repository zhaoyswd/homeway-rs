# 互操作矩阵最近一次运行（生成：2026-10-06 11:23:30；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| RRR | R-ready | PASS | /tmp/homeway-rs-matrix/RRR/relay/cache/relay.log:2026-10-06 11:17:55.579 [relay] 中继控制面：TCP 0.0.0.0:4 |  |
| RRR | E1 | PASS | 2026-10-06 11:17:56.152 [homeway] serve 就绪：wg=:42667（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 te |  |
| RRR | X1-reg | PASS | 2026-10-06 11:17:56.153 [homeway] 中继：注册成功（腿 42667 → 127.0.0.1:42757）—— 客户端可经它到达本机 |  |
| RRR | R3-backend | PASS | 2026-10-06 11:17:56.153 [relay] 中继：后端 40dd91adfc3adbe2 注册成功（腿 127.0.0.1:42667） |  |
| RRR | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| RRR | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42667（首个回包来源） |  |
| RRR | E7 | PASS | 2026-10-06 11:17:57.725 [homeway] peer: + dev=32f868f0 pub=40e65a67 ip=100.64.185.182 n=1/32 |  |
| RRR | E13-speedtest | PASS | speedtest: 摘要 down=184Mbps up=328Mbps； | （复核第 0 轮命中） |
| RRR | F-100MB | PASS | sha256 双侧一致（dd8dc688e407884e…） |  |
| RRR | E10 | PASS | 2026-10-06 11:22:25.049 [homeway] intercept: tcp transit 192.168.3.12:42807 ← 100.64.185.182:43715（d |  |
| RRR | E11 | PASS | 2026-10-06 11:22:25.055 [homeway] intercept: tcp transit 192.168.3.12:42807 ← 100.64.185.182:43715 关 |  |
| RRR | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| RRR | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| RRR | DBG-log | PASS | 2026-10-06 11:17:57.725 [homeway] peer: + dev=32f868f0 pub=40e65a67 ip=100.64.185.182 n=1/ |  |
| RRR | TOTAL | PASS | 331s |  |

**结论：全绿**（豁免：WARN 0 / SKIP 0——各降档的独立证据绑定见备注列）
