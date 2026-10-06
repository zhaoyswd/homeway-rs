# 互操作矩阵最近一次运行（生成：2026-10-07 02:16:51；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| RRR | R-ready | PASS | /tmp/homeway-rs-matrix/RRR/relay/cache/relay.log:2026-10-07 02:11:47.070 [relay] 中继控制面：TCP 0.0.0.0:4 |  |
| RRR | E1 | PASS | 2026-10-07 02:11:47.633 [homeway] serve 就绪：wg=:42667（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 te |  |
| RRR | X1-reg | PASS | 2026-10-07 02:11:47.634 [homeway] 中继：注册成功（腿 42667 → 127.0.0.1:42757）—— 客户端可经它到达本机 |  |
| RRR | R3-backend | PASS | 2026-10-07 02:11:47.634 [relay] 中继：后端 46b09b25fd6fb94d 注册成功（腿 127.0.0.1:42667） |  |
| RRR | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| RRR | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42667（首个回包来源） |  |
| RRR | E7 | PASS | 2026-10-07 02:11:49.171 [homeway] peer: + dev=22679f19 pub=bb5faf00 ip=100.64.164.168 n=1/32 |  |
| RRR | E13-speedtest | PASS | speedtest: 摘要 down=196Mbps up=336Mbps； | （复核第 0 轮命中） |
| RRR | F-100MB | PASS | sha256 双侧一致（fa8f6655bc69a6c7…） |  |
| RRR | E10 | PASS | 2026-10-07 02:16:16.563 [homeway] intercept: tcp transit 192.168.3.12:42807 ← 100.64.164.168:45996（d |  |
| RRR | E11 | PASS | 2026-10-07 02:16:16.569 [homeway] intercept: tcp transit 192.168.3.12:42807 ← 100.64.164.168:45996 关 |  |
| RRR | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| RRR | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| RRR | DBG-log | PASS | 2026-10-07 02:11:49.171 [homeway] peer: + dev=22679f19 pub=bb5faf00 ip=100.64.164.168 n=1/ |  |
| RRR | TOTAL | PASS | 301s |  |

**结论：全绿**（豁免：WARN 0 / SKIP 0——各降档的独立证据绑定见备注列）
