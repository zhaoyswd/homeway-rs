# 互操作矩阵最近一次运行（生成：2026-10-03 02:41:47；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L3 | R-ready | PASS | /tmp/homeway-rs-matrix/L3/relay/cache/relay.log:2026-10-03 02:27:12.504 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L3 | E1 | PASS | 2026-10-03 02:27:13.165 [homewayd] serve 就绪：wg=:42663（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L3 | X1-reg | PASS | 2026-10-03 02:27:13.165 [homewayd] 中继：注册成功（腿 42663 → 127.0.0.1:42753）—— 客户端可经它到达本机 |  |
| L3 | R3-backend | PASS |  |  |
| L3 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L3 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42663（首个回包来源） |  |
| L3 | E7 | PASS | 2026-10-03 02:27:14.703 [homewayd] peer: + dev=20777e2e pub=b6a2c05c ip=100.64.118.10 n=1/32 |  |
| L3 | E13-speedtest | PASS | round 1/1: down=416Mbps up=335Mbps； |  |
| L3 | F-100MB | PASS | sha256 双侧一致（e4cc364f1f4931e7…） |  |
| L3 | E10 | PASS | 2026-10-03 02:33:17.915 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.118.10:48958（d |  |
| L3 | E11 | PASS | 2026-10-03 02:33:17.919 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.118.10:48958 关 |  |
| L3 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L3 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L3 | MP-n2 | PASS | 2026-10-03 02:34:22.084 [homewayd] peer: + dev=a2441da1 pub=508e9ce8 ip=100.64.24.0 n=2/32 |  |
| L3 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42753（首个回包来源） |  |
| L3 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42753（dev=20777e2e，中继=true） |  |
| L3 | RL-speedtest | FAIL | 经中继 speedtest 无产出（75s） |  |
| L3 | RL-files5MB | PASS | sha256 双侧一致（2b6e92e485c59885…） |  |
| L3 | TOTAL | PASS | 872s |  |

**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log）
