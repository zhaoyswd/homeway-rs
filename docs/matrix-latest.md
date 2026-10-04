# 互操作矩阵最近一次运行（生成：2026-10-05 04:18:30；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| RRR | R-ready | PASS | /tmp/homeway-rs-matrix/RRR/relay/cache/relay.log:2026-10-05 04:12:56.467 [relay] 中继控制面：TCP 0.0.0.0:4 |  |
| RRR | E1 | PASS | serve 就绪：wg=:42667（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| RRR | X1-reg | PASS | 中继：注册成功（腿 42667 → 127.0.0.1:42757）—— 客户端可经它到达本机 |  |
| RRR | R3-backend | PASS | 2026-10-05 04:12:57.033 [relay] 中继：后端 c12a1f47ecfcd77d 注册成功（腿 127.0.0.1:42667） |  |
| RRR | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| RRR | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42667（首个回包来源） |  |
| RRR | E7 | PASS | peer: + dev=9ac4dc36 pub=2ec836dc ip=100.64.237.203 n=1/32 |  |
| RRR | E13-speedtest | PASS | speedtest: 摘要 down=250Mbps up=366Mbps； | （复核第 0 轮命中） |
| RRR | F-100MB | PASS | sha256 双侧一致（f8cf9f915b557983…） |  |
| RRR | E10 | PASS | intercept: tcp transit 192.168.3.12:42807 ← 100.64.237.203:34902（dialok） |  |
| RRR | E11 | PASS | intercept: tcp transit 192.168.3.12:42807 ← 100.64.237.203:34902 关闭 |  |
| RRR | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| RRR | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| RRR | TOTAL | PASS | 330s |  |

**结论：全绿**（豁免：WARN 0 / SKIP 0——各降档的独立证据绑定见备注列）
