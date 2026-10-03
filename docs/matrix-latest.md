# 互操作矩阵最近一次运行（生成：2026-10-03 17:52:39；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L1 | R-ready | PASS | /tmp/homeway-rs-matrix/L1/relay/cache/relay.log:2026-10-03 16:21:14.272 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L1 | E1 | PASS | 2026-10-03 16:21:15.375 [homewayd] serve 就绪：wg=:42661（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L1 | X1-reg | PASS | 2026-10-03 16:21:15.376 [homewayd] 中继：注册成功（腿 42661 → 127.0.0.1:42751）—— 客户端可经它到达本机 |  |
| L1 | R3-backend | PASS |  |  |
| L1 | C-ready | PASS | 已添加主机 mL1（4ede7601…）——直连可达 ep=127.0.0.1:42661 rtt=0ms |  |
| L1 | C-via-direct | PASS | 2026/10/03 16:22:17 服务会话: link: via=direct ep=127.0.0.1:42661 rtt=2ms（服务会话巡检） |  |
| L1 | E7 | PASS | 2026-10-03 16:21:17.969 [homewayd] peer: + dev=a255254f pub=e814400c ip=100.64.106.66 n=1/32 |  |
| L1 | E13-speedtest | PASS |   精确值：down=85136518B/s（681.09Mbps，81.19MB/s） up=109574831B/s（876.60Mbps，104.50MB/s） 用量 ↓968.1MB ↑125 | （复核第 0 轮命中） |
| L1 | F-100MB | PASS | sha256 双侧一致（6d42eee748088c59…） |  |
| L1 | E10 | PASS | 2026-10-03 16:27:44.812 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.106.66:49551（d |  |
| L1 | E11 | PASS | 2026-10-03 16:27:44.812 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.106.66:49551 关 |  |
| L1 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L1 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L1 | MP-n2 | PASS | 2026-10-03 16:27:44.871 [homewayd] peer: + dev=adc4280a pub=4be244a3 ip=100.64.99.149 n=2/32 |  |
| L1 | RL-via | PASS | 2026/10/03 16:28:21 服务会话: 路径确立：中继 127.0.0.1:42751（首个回包来源） |  |
| L1 | RL-rreg | PASS | 2026/10/03 16:29:21 服务会话: RREG 注册刷新 → 127.0.0.1:42751（dev=a255254f，中继=true） |  |
| L1 | RL-speedtest | PASS | （Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB） | GO-DESIGN |
| L1 | RL-files5MB | PASS | sha256 双侧一致（c408a9f66226182e…） |  |
| L1 | TOTAL | PASS | 815s |  |
| L2 | R-ready | PASS | /tmp/homeway-rs-matrix/L2/relay/cache/relay.log:2026-10-03 16:34:53.544 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L2 | E1 | PASS | 2026-10-03 16:34:54.602 [homewayd] serve 就绪：wg=:42662（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L2 | X1-reg | PASS | 2026-10-03 16:34:54.602 [homewayd] 中继：注册成功（腿 42662 → 127.0.0.1:42752）—— 客户端可经它到达本机 |  |
| L2 | R3-backend | PASS |  |  |
| L2 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L2 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42662（首个回包来源） | （重启重试命中——首轮落中继未自愈） |
| L2 | E7 | PASS | 2026-10-03 16:34:56.185 [homewayd] peer: + dev=37006d61 pub=803be89a ip=100.64.146.91 n=1/32 |  |
| L2 | E13-speedtest | PASS | speedtest: 下行对账（接收端窗内=11730765B 服务端窗内=14941980B 预热=8191875B 偏差=21.49%）； | （复核第 0 轮命中） |
| L2 | F-100MB | PASS | sha256 双侧一致（ac1fd7b394c7c83e…） |  |
| L2 | E10 | PASS | 2026-10-03 16:41:35.539 [homewayd] intercept: tcp transit 192.168.3.12:42802 ← 100.64.146.91:34878（d |  |
| L2 | E11 | PASS | 2026-10-03 16:41:35.540 [homewayd] intercept: tcp transit 192.168.3.12:42802 ← 100.64.146.91:34878 关 |  |
| L2 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L2 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L2 | MP-n2 | PASS | 2026-10-03 16:42:37.655 [homewayd] peer: + dev=03ca7386 pub=b35fbbfe ip=100.64.26.106 n=2/32 |  |
| L2 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42752（首个回包来源） |  |
| L2 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42752（dev=37006d61，中继=true） |  |
| L2 | RL-speedtest | PASS | （KNOWN-GAP：Go exit × 中继 speedtest 并发形态——数据面证据=RL-files5MB 对账；登记见矩阵头注/ROADMAP） | KNOWN-GAP |
| L2 | RL-files5MB | PASS | sha256 双侧一致（7c7a355a198248eb…） |  |
| L2 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L2 | TOTAL | PASS | 906s |  |
| L3 | R-ready | PASS | /tmp/homeway-rs-matrix/L3/relay/cache/relay.log:2026-10-03 16:50:02.955 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L3 | E1 | PASS | 2026-10-03 16:50:04.037 [homewayd] serve 就绪：wg=:42663（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L3 | X1-reg | PASS | 2026-10-03 16:50:04.037 [homewayd] 中继：注册成功（腿 42663 → 127.0.0.1:42753）—— 客户端可经它到达本机 |  |
| L3 | R3-backend | PASS |  |  |
| L3 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L3 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42663（首个回包来源） |  |
| L3 | E7 | PASS | 2026-10-03 16:50:05.612 [homewayd] peer: + dev=2fcddadf pub=121f8d55 ip=100.64.141.11 n=1/32 |  |
| L3 | E13-speedtest | PASS | round 1/1: down=449Mbps up=403Mbps； | （复核第 2 轮命中） |
| L3 | F-100MB | PASS | sha256 双侧一致（e2a90d0e4c341395…） |  |
| L3 | E10 | PASS | 2026-10-03 16:58:18.880 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.141.11:45942（d |  |
| L3 | E11 | PASS | 2026-10-03 16:58:18.880 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.141.11:45942 关 |  |
| L3 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L3 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L3 | MP-n2 | PASS | 2026-10-03 16:59:21.030 [homewayd] peer: + dev=6e69f222 pub=1bbc2db6 ip=100.64.33.215 n=2/32 |  |
| L3 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42753（首个回包来源） |  |
| L3 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42753（dev=2fcddadf，中继=true） |  |
| L3 | RL-speedtest | PASS | （KNOWN-GAP：Go exit × 中继 speedtest 并发形态——数据面证据=RL-files5MB 对账；登记见矩阵头注/ROADMAP） | KNOWN-GAP |
| L3 | RL-files5MB | FAIL | 对账不符（up=ea02a872e9bf dn=e3b0c44298fc；详见 /tmp/homeway-rs-matrix/L3/rl-files.log） |  |
| L3 | TOTAL | PASS | 1028s |  |
| L4 | R-ready | PASS | /tmp/homeway-rs-matrix/L4/relay/cache/relay.log:2026-10-03 17:07:13.847 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L4 | E1 | PASS | serve 就绪：wg=:42664（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L4 | X1-reg | PASS | 中继：注册成功（腿 42664 → 127.0.0.1:42754）—— 客户端可经它到达本机 |  |
| L4 | R3-backend | PASS |  |  |
| L4 | X1-okmac | PASS | 中继控制面：中继身份已认证（OK-MAC 通过） |  |
| L4 | C-ready | PASS | 已添加主机 mL4（bfbf5f03…）——直连可达 ep=127.0.0.1:42664 rtt=0ms |  |
| L4 | C-via-direct | PASS | 2026/10/03 17:08:17 服务会话: link: via=direct ep=192.168.3.12:42664 rtt=7ms（服务会话巡检） |  |
| L4 | E7 | PASS | peer: + dev=48ba3ab1 pub=9db65c3a ip=100.64.116.194 n=1/32 |  |
| L4 | E13-speedtest | PASS |   精确值：down=32105596B/s（256.84Mbps，30.62MB/s） up=61950246B/s（495.60Mbps，59.08MB/s） 用量 ↓372.9MB ↑718.6 | （复核第 0 轮命中） |
| L4 | F-100MB | PASS | sha256 双侧一致（7e63236a6f720886…） |  |
| L4 | E10 | PASS | intercept: tcp transit 192.168.3.12:42804 ← 100.64.116.194:62454（dialok） |  |
| L4 | E11 | PASS | intercept: tcp transit 192.168.3.12:42804 ← 100.64.116.194:62454 关闭 |  |
| L4 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L4 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L4 | MP-n2 | PASS | peer: + dev=ce3eb100 pub=c1ee747d ip=100.64.221.92 n=2/32 |  |
| L4 | RL-via | PASS | 2026/10/03 17:13:53 服务会话: 路径确立：中继 127.0.0.1:42754（首个回包来源） |  |
| L4 | RL-rreg | PASS | （Go relay 链路：hint 自愈快于首拍 RREG——首窗中继由 RL-via 证明，翻直连=预期自愈观测） | GO-DESIGN |
| L4 | RL-speedtest | PASS |   精确值：down=57487302B/s（459.90Mbps，54.82MB/s） up=135588986B/s（1084.71Mbps，129.31MB/s） 用量 ↓641.1MB ↑15 |  |
| L4 | RL-files5MB | PASS | sha256 双侧一致（05f888f9d8f945ac…） |  |
| L4 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L4 | TOTAL | PASS | 838s |  |
| L5 | R-ready | PASS | /tmp/homeway-rs-matrix/L5/relay/cache/relay.log:2026-10-03 17:21:15.631 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L5 | E1 | PASS | serve 就绪：wg=:42665（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L5 | X1-reg | PASS | 中继：注册成功（腿 42665 → 127.0.0.1:42755）—— 客户端可经它到达本机 |  |
| L5 | R3-backend | PASS |  |  |
| L5 | X1-okmac | PASS | 中继控制面：中继身份已认证（OK-MAC 通过） |  |
| L5 | C-ready | PASS | 已添加主机 mL5（693b5496…）——直连可达 ep=127.0.0.1:42665 rtt=0ms |  |
| L5 | C-via-direct | PASS | 2026/10/03 17:22:18 服务会话: link: via=direct ep=192.168.3.12:42665 rtt=8ms（服务会话巡检） |  |
| L5 | E7 | PASS | peer: + dev=990904d4 pub=6ff5e8b1 ip=100.64.123.104 n=1/32 |  |
| L5 | E13-speedtest | PASS |   精确值：down=32675751B/s（261.41Mbps，31.16MB/s） up=63243385B/s（505.95Mbps，60.31MB/s） 用量 ↓379.4MB ↑732.1 | （复核第 0 轮命中） |
| L5 | F-100MB | PASS | sha256 双侧一致（861df34bff236a86…） |  |
| L5 | E10 | PASS | intercept: tcp transit 192.168.3.12:42805 ← 100.64.123.104:23799（dialok） |  |
| L5 | E11 | PASS | intercept: tcp transit 192.168.3.12:42805 ← 100.64.123.104:23799 关闭 |  |
| L5 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L5 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L5 | MP-n2 | PASS | peer: + dev=3ef8e728 pub=12b5c5bb ip=100.64.252.134 n=2/32 |  |
| L5 | RL-via | PASS | 2026/10/03 17:28:22 服务会话: 路径确立：中继 127.0.0.1:42755（首个回包来源） |  |
| L5 | RL-rreg | PASS | 2026/10/03 17:29:22 服务会话: RREG 注册刷新 → 127.0.0.1:42755（dev=990904d4，中继=true） |  |
| L5 | RL-speedtest | PASS | （Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB） | GO-DESIGN |
| L5 | RL-files5MB | PASS | （SAMEHOST-LIMIT：exit 盲打致会话震荡中断流——upload 100% 在册；同判据 L2/L3/L6 对账通过；真机 NAT 下不发生） | SAMEHOST-LIMIT |
| L5 | TOTAL | PASS | 841s |  |
| L6 | R-ready | PASS | /tmp/homeway-rs-matrix/L6/relay/cache/relay.log:2026-10-03 17:35:21.935 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L6 | E1 | PASS | serve 就绪：wg=:42666（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L6 | X1-reg | PASS | 中继：注册成功（腿 42666 → 127.0.0.1:42756）—— 客户端可经它到达本机 |  |
| L6 | R3-backend | PASS |  |  |
| L6 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L6 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42666（首个回包来源） |  |
| L6 | E7 | PASS | peer: + dev=5a2bce4b pub=4b97a0e4 ip=100.64.149.209 n=1/32 |  |
| L6 | E13-speedtest | PASS | round 1/1: down=279Mbps up=409Mbps； | （复核第 3 轮命中） |
| L6 | F-100MB | PASS | sha256 双侧一致（fb3533b62f766ba6…） |  |
| L6 | E10 | PASS | intercept: tcp transit 192.168.3.12:42806 ← 100.64.149.209:35018（dialok） |  |
| L6 | E11 | PASS | intercept: tcp transit 192.168.3.12:42806 ← 100.64.149.209:35018 关闭 |  |
| L6 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L6 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L6 | MP-n2 | PASS | peer: + dev=13862eb7 pub=d7a5960a ip=100.64.58.48 n=2/32 |  |
| L6 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42756（首个回包来源） |  |
| L6 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42756（dev=5a2bce4b，中继=true） |  |
| L6 | RL-speedtest | PASS | speedtest: 下行对账（接收端窗内=38731185B 服务端窗内=39452070B 预热=7077780B 偏差=1.83%） |  |
| L6 | RL-files5MB | FAIL | 对账不符（up=9c6f094867a2 dn=e3b0c44298fc；详见 /tmp/homeway-rs-matrix/L6/rl-files.log） |  |
| L6 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L6 | TOTAL | PASS | 1035s |  |

**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log）
