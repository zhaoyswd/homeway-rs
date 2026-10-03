# 互操作矩阵最近一次运行（生成：2026-10-03 22:31:33；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L3 | R-ready | PASS | /tmp/homeway-rs-matrix/L3/relay/cache/relay.log:2026-10-03 22:17:27.983 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L3 | E1 | PASS | 2026-10-03 22:17:29.063 [homewayd] serve 就绪：wg=:42663（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L3 | X1-reg | PASS | 2026-10-03 22:17:29.063 [homewayd] 中继：注册成功（腿 42663 → 127.0.0.1:42753）—— 客户端可经它到达本机 |  |
| L3 | R3-backend | PASS | 2026-10-03 22:17:29.063 [relay] 中继：后端 c74e4228c428d254 注册成功（腿 127.0.0.1:42663） |  |
| L3 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L3 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42663（首个回包来源） |  |
| L3 | E7 | PASS | 2026-10-03 22:17:30.649 [homewayd] peer: + dev=3c68a111 pub=a46ac48a ip=100.64.132.138 n=1/32 |  |
| L3 | E13-speedtest | PASS | round 1/1: down=396Mbps up=385Mbps； | （复核第 1 轮命中） |
| L3 | F-100MB | PASS | sha256 双侧一致（920bd5f293144ca5…） |  |
| L3 | E10 | PASS | 2026-10-03 22:23:53.845 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.132.138:46619（ |  |
| L3 | E11 | PASS | 2026-10-03 22:23:53.845 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.132.138:46619  |  |
| L3 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L3 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L3 | MP-n2 | PASS | 2026-10-03 22:24:55.988 [homewayd] peer: + dev=5c5d890f pub=4e924102 ip=100.64.163.40 n=2/32 |  |
| L3 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42753（首个回包来源） |  |
| L3 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42753（dev=3c68a111，中继=true） |  |
| L3 | RL-speedtest | PASS | speedtest: 下行对账（接收端窗内=5898150B 服务端窗内=7143315B 预热=4915125B 偏差=17.43%） |  |
| L3 | RL-files5MB | PASS | sha256 双侧一致（89a9a29c9d45b4ab…） |  |
| L3 | TOTAL | PASS | 842s |  |

**结论：全绿**
