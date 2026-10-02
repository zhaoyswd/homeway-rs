# 互操作矩阵最近一次运行（生成：2026-10-03 01:58:32；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L3 | R-ready | PASS | /tmp/homeway-rs-matrix/L3/relay/cache/relay.log:2026-10-03 01:45:16.541 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L3 | E1 | PASS | 2026-10-03 01:45:17.649 [homewayd] serve 就绪：wg=:42663（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L3 | X1-reg | PASS | 2026-10-03 01:45:17.650 [homewayd] 中继：注册成功（腿 42663 → 127.0.0.1:42753）—— 客户端可经它到达本机 |  |
| L3 | R3-backend | PASS |  |  |
| L3 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L3 | C-via-direct | FAIL | 30s 内未见路径确立/via=direct |  |
| L3 | E7 | PASS | 2026-10-03 01:45:19.198 [homewayd] peer: + dev=8e1eb93a pub=aa6fd1d0 ip=100.64.2.146 n=1/32 |  |
| L3 | E13-speedtest | PASS | speedtest: 下行对账（接收端窗内=5570475B 服务端窗内=6225825B 预热=4718520B 偏差=10.53%）； |  |
| L3 | F-100MB | PASS | sha256 双侧一致（879766d463987330…） |  |
| L3 | E10 | FAIL | 25s 内未见 transit dialok |  |
| L3 | E11 | FAIL | 未见 transit 关闭行 |  |
| L3 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L3 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L3 | MP-n2 | PASS | 2026-10-03 01:51:22.294 [homewayd] peer: + dev=8a4a30eb pub=4793a444 ip=100.64.74.185 n=2/32 |  |
| L3 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42753（首个回包来源） |  |
| L3 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42753（dev=8e1eb93a，中继=true） |  |
| L3 | RL-speedtest | FAIL | 经中继 speedtest 摘要未产出（60s） |  |
| L3 | RL-files5MB | FAIL | 对账不符（up=b0d033a1f1b9 dn=；详见 /tmp/homeway-rs-matrix/L3/rl-files.log） |  |
| L3 | TOTAL | PASS | 792s |  |

**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log）
