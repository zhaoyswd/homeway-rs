# 互操作矩阵最近一次运行（生成：2026-10-03 21:57:42；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L1 | R-ready | PASS | /tmp/homeway-rs-matrix/L1/relay/cache/relay.log:2026-10-03 20:21:33.825 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L1 | E1 | PASS | 2026-10-03 20:21:34.484 [homewayd] serve 就绪：wg=:42661（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L1 | X1-reg | PASS | 2026-10-03 20:21:34.484 [homewayd] 中继：注册成功（腿 42661 → 127.0.0.1:42751）—— 客户端可经它到达本机 |  |
| L1 | R3-backend | PASS | 2026-10-03 20:21:34.484 [relay] 中继：后端 65252f90d69eaf05 注册成功（腿 127.0.0.1:42661） |  |
| L1 | C-ready | PASS | 已添加主机 mL1（fb527fc3…）——直连可达 ep=127.0.0.1:42661 rtt=0ms |  |
| L1 | C-via-direct | PASS | 2026/10/03 20:22:39 服务会话: link: via=direct ep=127.0.0.1:42661 rtt=0ms（服务会话巡检） |  |
| L1 | E7 | PASS | 2026-10-03 20:21:37.085 [homewayd] peer: + dev=a812161b pub=bb2cda4e ip=100.64.234.36 n=1/32 |  |
| L1 | E13-speedtest | PASS |   精确值：down=87725151B/s（701.80Mbps，83.66MB/s） up=95563501B/s（764.51Mbps，91.14MB/s） 用量 ↓1013.1MB ↑1125 | （复核第 0 轮命中） |
| L1 | F-100MB | PASS | sha256 双侧一致（c8c77e558aff617f…） |  |
| L1 | E10 | PASS | 2026-10-03 20:29:41.454 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.234.36:21003（d |  |
| L1 | E11 | PASS | 2026-10-03 20:29:41.454 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.234.36:21003 关 |  |
| L1 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L1 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L1 | MP-n2 | PASS | 2026-10-03 20:29:41.498 [homewayd] peer: + dev=600d12c9 pub=d8216a26 ip=100.64.251.201 n=2/32 |  |
| L1 | RL-via | PASS | 2026/10/03 20:30:18 服务会话: 路径确立：中继 127.0.0.1:42751（首个回包来源） |  |
| L1 | RL-rreg | PASS | 2026/10/03 20:31:18 服务会话: RREG 注册刷新 → 127.0.0.1:42751（dev=a812161b，中继=true） |  |
| L1 | RL-speedtest | PASS | （Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB） | GO-DESIGN |
| L1 | RL-files5MB | PASS | sha256 双侧一致（c838625ac10e3b73…） |  |
| L1 | TOTAL | PASS | 912s |  |
| L2 | R-ready | PASS | /tmp/homeway-rs-matrix/L2/relay/cache/relay.log:2026-10-03 20:36:49.804 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L2 | E1 | PASS | 2026-10-03 20:36:50.870 [homewayd] serve 就绪：wg=:42662（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L2 | X1-reg | PASS | 2026-10-03 20:36:50.870 [homewayd] 中继：注册成功（腿 42662 → 127.0.0.1:42752）—— 客户端可经它到达本机 |  |
| L2 | R3-backend | PASS | 2026-10-03 20:36:50.870 [relay] 中继：后端 43e383440b747eae 注册成功（腿 127.0.0.1:42662） |  |
| L2 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L2 | C-via-direct | PASS | 服务会话: 路径确立：直连 127.0.0.1:42662（首个回包来源） |  |
| L2 | E7 | PASS | 2026-10-03 20:36:52.442 [homewayd] peer: + dev=31eaf072 pub=384877d0 ip=100.64.70.9 n=1/32 |  |
| L2 | E13-speedtest | PASS | round 1/1: down=455Mbps up=393Mbps； | （复核第 2 轮命中） |
| L2 | F-100MB | PASS | sha256 双侧一致（21eb3520b02e0560…） |  |
| L2 | E10 | PASS | 2026-10-03 20:45:04.682 [homewayd] intercept: tcp transit 192.168.3.12:42802 ← 100.64.70.9:36235（dia |  |
| L2 | E11 | PASS | 2026-10-03 20:45:04.682 [homewayd] intercept: tcp transit 192.168.3.12:42802 ← 100.64.70.9:36235 关闭 |  |
| L2 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L2 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L2 | MP-n2 | PASS | 2026-10-03 20:46:06.807 [homewayd] peer: + dev=46cc42c4 pub=ff012505 ip=100.64.25.133 n=2/32 |  |
| L2 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42752（首个回包来源） |  |
| L2 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42752（dev=31eaf072，中继=true） |  |
| L2 | RL-speedtest | PASS | （KNOWN-GAP：Go exit × 中继 speedtest 并发形态——数据面证据=RL-files5MB 对账；登记见矩阵头注/ROADMAP） | KNOWN-GAP |
| L2 | RL-files5MB | PASS | sha256 双侧一致（a273f2433bde4d33…） |  |
| L2 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L2 | TOTAL | PASS | 999s |  |
| L3 | R-ready | PASS | /tmp/homeway-rs-matrix/L3/relay/cache/relay.log:2026-10-03 20:53:32.073 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L3 | E1 | PASS | 2026-10-03 20:53:33.153 [homewayd] serve 就绪：wg=:42663（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L3 | X1-reg | PASS | 2026-10-03 20:53:33.154 [homewayd] 中继：注册成功（腿 42663 → 127.0.0.1:42753）—— 客户端可经它到达本机 |  |
| L3 | R3-backend | PASS | 2026-10-03 20:53:33.154 [relay] 中继：后端 d2cb9570eaef782f 注册成功（腿 127.0.0.1:42663） |  |
| L3 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L3 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42663（首个回包来源） | （重启重试命中——首轮落中继未自愈） |
| L3 | E7 | PASS | 2026-10-03 20:53:34.743 [homewayd] peer: + dev=d1dee668 pub=1cd5e5ab ip=100.64.128.130 n=1/32 |  |
| L3 | E13-speedtest | PASS | speedtest: 下行对账（接收端窗内=7208850B 服务端窗内=10157925B 预热=4849590B 偏差=29.03%）； | （复核第 0 轮命中） |
| L3 | F-100MB | FAIL | 对账不符：up=dcfcb07a3ae5 dn=e3b0c44298fc（files upload 失败：files 传输失败：写通道长时间无进展 / files download 失败：not_found: download /mat-L3-3047931576.bin：不存在） |  |
| L3 | E10 | PASS | 2026-10-03 21:00:12.538 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.128.130:39416（ |  |
| L3 | E11 | PASS | 2026-10-03 21:00:12.538 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.128.130:39416  |  |
| L3 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L3 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L3 | MP-n2 | PASS | 2026-10-03 21:01:14.606 [homewayd] peer: + dev=0a9b5a84 pub=aa8790ee ip=100.64.247.198 n=2/32 |  |
| L3 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42753（首个回包来源） |  |
| L3 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42753（dev=d1dee668，中继=true） |  |
| L3 | RL-speedtest | PASS | （KNOWN-GAP：Go exit × 中继 speedtest 并发形态——数据面证据=RL-files5MB 对账；登记见矩阵头注/ROADMAP） | KNOWN-GAP |
| L3 | RL-files5MB | PASS | sha256 双侧一致（256a9f1110eb9118…） |  |
| L3 | TOTAL | FAIL | 932s（本链路有判据红项） |  |
| L4 | R-ready | PASS | /tmp/homeway-rs-matrix/L4/relay/cache/relay.log:2026-10-03 21:09:06.994 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L4 | E1 | PASS | serve 就绪：wg=:42664（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L4 | X1-reg | PASS | 中继：注册成功（腿 42664 → 127.0.0.1:42754）—— 客户端可经它到达本机 |  |
| L4 | R3-backend | PASS | 2026-10-03 21:09:08.032 [relay] 中继：后端 a9a7b38b98bc993b 注册成功（腿 127.0.0.1:42664） |  |
| L4 | X1-okmac | PASS | 中继控制面：中继身份已认证（OK-MAC 通过） |  |
| L4 | C-ready | PASS | 已添加主机 mL4（ebe1a2d0…）——直连可达 ep=127.0.0.1:42664 rtt=0ms |  |
| L4 | C-via-direct | PASS | 2026/10/03 21:10:10 服务会话: link: via=direct ep=192.168.3.12:42664 rtt=8ms（服务会话巡检） |  |
| L4 | E7 | PASS | peer: + dev=b95d8729 pub=1a4992ec ip=100.64.129.123 n=1/32 |  |
| L4 | E13-speedtest | PASS |   精确值：down=31791028B/s（254.33Mbps，30.32MB/s） up=62338329B/s（498.71Mbps，59.45MB/s） 用量 ↓348.3MB ↑728.7 | （复核第 0 轮命中） |
| L4 | F-100MB | PASS | sha256 双侧一致（5f6bacdedfbaa02c…） |  |
| L4 | E10 | PASS | intercept: tcp transit 192.168.3.12:42804 ← 100.64.129.123:39971（dialok） |  |
| L4 | E11 | PASS | intercept: tcp transit 192.168.3.12:42804 ← 100.64.129.123:39971 关闭 |  |
| L4 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L4 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L4 | MP-n2 | PASS | peer: + dev=301b41b7 pub=3e2d2636 ip=100.64.112.169 n=2/32 |  |
| L4 | RL-via | PASS | 2026/10/03 21:17:22 服务会话: 路径确立：中继 127.0.0.1:42754（首个回包来源） |  |
| L4 | RL-rreg | PASS | （Go relay 链路：hint 自愈快于首拍 RREG——首窗中继由 RL-via 证明，翻直连=预期自愈观测） | GO-DESIGN |
| L4 | RL-speedtest | PASS |   精确值：down=52349358B/s（418.79Mbps，49.92MB/s） up=133663065B/s（1069.30Mbps，127.47MB/s） 用量 ↓604.1MB ↑15 |  |
| L4 | RL-files5MB | PASS | sha256 双侧一致（dffa4bb9dc50c84c…） |  |
| L4 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L4 | TOTAL | PASS | 932s |  |
| L5 | R-ready | PASS | /tmp/homeway-rs-matrix/L5/relay/cache/relay.log:2026-10-03 21:24:42.532 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L5 | E1 | PASS | serve 就绪：wg=:42665（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L5 | X1-reg | PASS | 中继：注册成功（腿 42665 → 127.0.0.1:42755）—— 客户端可经它到达本机 |  |
| L5 | R3-backend | PASS | 2026-10-03 21:24:43.580 [relay] 中继：后端 4ef68258cb06b1d8 注册成功（腿 127.0.0.1:42665） |  |
| L5 | X1-okmac | PASS | 中继控制面：中继身份已认证（OK-MAC 通过） |  |
| L5 | C-ready | PASS | 已添加主机 mL5（0bad8048…）——直连可达 ep=127.0.0.1:42665 rtt=0ms |  |
| L5 | C-via-direct | PASS | 2026/10/03 21:25:45 服务会话: link: via=direct ep=192.168.3.12:42665 rtt=7ms（服务会话巡检） |  |
| L5 | E7 | PASS | peer: + dev=855388ed pub=f2af0e87 ip=100.64.47.112 n=1/32 |  |
| L5 | E13-speedtest | PASS |   精确值：down=28429083B/s（227.43Mbps，27.11MB/s） up=63771438B/s（510.17Mbps，60.82MB/s） 用量 ↓334.2MB ↑735.1 | （复核第 0 轮命中） |
| L5 | F-100MB | PASS | sha256 双侧一致（ecaac8d4b436331f…） |  |
| L5 | E10 | PASS | intercept: tcp transit 192.168.3.12:42805 ← 100.64.47.112:20405（dialok） |  |
| L5 | E11 | PASS | intercept: tcp transit 192.168.3.12:42805 ← 100.64.47.112:20405 关闭 |  |
| L5 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L5 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L5 | MP-n2 | PASS | peer: + dev=e4291540 pub=21ab7928 ip=100.64.151.34 n=2/32 |  |
| L5 | RL-via | PASS | 2026/10/03 21:33:27 服务会话: 路径确立：中继 127.0.0.1:42755（首个回包来源） |  |
| L5 | RL-rreg | PASS | 2026/10/03 21:34:27 服务会话: RREG 注册刷新 → 127.0.0.1:42755（dev=855388ed，中继=true） |  |
| L5 | RL-speedtest | PASS | （Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB） | GO-DESIGN |
| L5 | RL-files5MB | PASS | （SAMEHOST-LIMIT：exit 盲打致会话震荡中断流——upload 100% 在册；同判据经中继对账见本结果表；真机 NAT 下不发生） | SAMEHOST-LIMIT |
| L5 | TOTAL | PASS | 936s |  |
| L6 | R-ready | PASS | /tmp/homeway-rs-matrix/L6/relay/cache/relay.log:2026-10-03 21:40:23.580 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L6 | E1 | PASS | serve 就绪：wg=:42666（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L6 | X1-reg | PASS | 中继：注册成功（腿 42666 → 127.0.0.1:42756）—— 客户端可经它到达本机 |  |
| L6 | R3-backend | PASS | 2026-10-03 21:40:24.626 [relay] 中继：后端 5a1982966b433481 注册成功（腿 127.0.0.1:42666） |  |
| L6 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L6 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42666（首个回包来源） |  |
| L6 | E7 | PASS | peer: + dev=93fab7ae pub=4d47a910 ip=100.64.25.151 n=1/32 |  |
| L6 | E13-speedtest | PASS | round 1/1: down=270Mbps up=434Mbps； | （复核第 3 轮命中） |
| L6 | F-100MB | PASS | sha256 双侧一致（827f5adda5067c1f…） |  |
| L6 | E10 | PASS | intercept: tcp transit 192.168.3.12:42806 ← 100.64.25.151:44137（dialok） |  |
| L6 | E11 | PASS | intercept: tcp transit 192.168.3.12:42806 ← 100.64.25.151:44137 关闭 |  |
| L6 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L6 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L6 | MP-n2 | PASS | peer: + dev=9c62925f pub=cb7f762b ip=100.64.61.102 n=2/32 |  |
| L6 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42756（首个回包来源） |  |
| L6 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42756（dev=93fab7ae，中继=true） |  |
| L6 | RL-speedtest | PASS | speedtest: 下行对账（接收端窗内=26541675B 服务端窗内=31850010B 预热=9043830B 偏差=16.67%） |  |
| L6 | RL-files5MB | PASS | sha256 双侧一致（5e87e0c8634a7a2d…） |  |
| L6 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L6 | TOTAL | PASS | 1034s |  |

**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log）
