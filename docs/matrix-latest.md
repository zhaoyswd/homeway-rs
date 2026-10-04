# 互操作矩阵最近一次运行（生成：2026-10-04 21:06:56；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| RRR | R-ready | PASS | /tmp/homeway-rs-matrix/RRR/relay/cache/relay.log:2026-10-04 20:59:26.200 [relay] 中继控制面：TCP 0.0.0.0:4 |  |
| RRR | E1 | PASS | serve 就绪：wg=:42667（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| RRR | X1-reg | PASS | 中继：注册成功（腿 42667 → 127.0.0.1:42757）—— 客户端可经它到达本机 |  |
| RRR | R3-backend | PASS | 2026-10-04 20:59:26.791 [relay] 中继：后端 c21990d8996fecd3 注册成功（腿 127.0.0.1:42667） |  |
| RRR | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| RRR | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42667（首个回包来源） |  |
| RRR | E7 | PASS | peer: + dev=a7ab1d3e pub=5dc62511 ip=100.64.235.1 n=1/32 |  |
| RRR | E13-speedtest | PASS | round 1/1: down=278Mbps up=416Mbps； | （复核第 1 轮命中） |
| RRR | F-100MB | PASS | sha256 双侧一致（386962d46b7a78e3…） |  |
| RRR | E10 | PASS | intercept: tcp transit 192.168.3.12:42807 ← 100.64.235.1:43380（dialok） |  |
| RRR | E11 | PASS | intercept: tcp transit 192.168.3.12:42807 ← 100.64.235.1:43380 关闭 |  |
| RRR | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| RRR | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| RRR | TOTAL | PASS | 446s |  |

**结论：全绿**（豁免：WARN 0 / SKIP 0——各降档的独立证据绑定见备注列）
