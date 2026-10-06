# 互操作矩阵最近一次运行（生成：2026-10-07 01:23:12；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| RRR | R-ready | PASS | /tmp/homeway-rs-matrix/RRR/relay/cache/relay.log:2026-10-07 01:17:37.880 [relay] 中继控制面：TCP 0.0.0.0:4 |  |
| RRR | E1 | PASS | 2026-10-07 01:17:38.302 [homeway] serve 就绪：wg=:42667（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 te |  |
| RRR | X1-reg | PASS | 2026-10-07 01:17:38.303 [homeway] 中继：注册成功（腿 42667 → 127.0.0.1:42757）—— 客户端可经它到达本机 |  |
| RRR | R3-backend | PASS | 2026-10-07 01:17:38.303 [relay] 中继：后端 aa35d3b3271cc871 注册成功（腿 127.0.0.1:42667） |  |
| RRR | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| RRR | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42667（首个回包来源） |  |
| RRR | E7 | PASS | 2026-10-07 01:17:39.849 [homeway] peer: + dev=03a84367 pub=55d2db46 ip=100.64.31.38 n=1/32 |  |
| RRR | E13-speedtest | PASS | speedtest: 摘要 down=184Mbps up=302Mbps； | （复核第 0 轮命中） |
| RRR | F-100MB | PASS | sha256 双侧一致（054ece9fc86d3405…） |  |
| RRR | E10 | PASS | 2026-10-07 01:22:07.240 [homeway] intercept: tcp transit 192.168.3.12:42807 ← 100.64.31.38:33845（dia |  |
| RRR | E11 | PASS | 2026-10-07 01:22:07.247 [homeway] intercept: tcp transit 192.168.3.12:42807 ← 100.64.31.38:33845 关闭 |  |
| RRR | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| RRR | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| RRR | DBG-log | PASS | 2026-10-07 01:17:39.849 [homeway] peer: + dev=03a84367 pub=55d2db46 ip=100.64.31.38 n=1/32 |  |
| RRR | TOTAL | PASS | 330s |  |

**结论：全绿**（豁免：WARN 0 / SKIP 0——各降档的独立证据绑定见备注列）
