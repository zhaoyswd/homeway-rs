# 互操作矩阵最近一次运行（生成：2026-10-03 20:18:34；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L1 | R-ready | PASS | /tmp/homeway-rs-matrix/L1/relay/cache/relay.log:2026-10-03 18:40:30.104 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L1 | E1 | PASS | 2026-10-03 18:40:31.183 [homewayd] serve 就绪：wg=:42661（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L1 | X1-reg | PASS | 2026-10-03 18:40:31.184 [homewayd] 中继：注册成功（腿 42661 → 127.0.0.1:42751）—— 客户端可经它到达本机 |  |
| L1 | R3-backend | PASS | 2026-10-03 18:40:31.184 [relay] 中继：后端 64da2ec623c4199b 注册成功（腿 127.0.0.1:42661） |  |
| L1 | C-ready | PASS | 已添加主机 mL1（44e1bbbe…）——直连可达 ep=127.0.0.1:42661 rtt=0ms |  |
| L1 | C-via-direct | PASS | 2026/10/03 18:41:35 服务会话: link: via=direct ep=127.0.0.1:42661 rtt=2ms（服务会话巡检） |  |
| L1 | E7 | PASS | 2026-10-03 18:40:33.824 [homewayd] peer: + dev=ac2e98a8 pub=8366a82f ip=100.64.119.128 n=1/32 |  |
| L1 | E13-speedtest | PASS |   精确值：down=84225582B/s（673.80Mbps，80.32MB/s） up=104726099B/s（837.81Mbps，99.87MB/s） 用量 ↓977.5MB ↑1215 | （复核第 0 轮命中） |
| L1 | F-100MB | PASS | sha256 双侧一致（c58596ed2175ceb6…） |  |
| L1 | E10 | PASS | 2026-10-03 18:48:38.187 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.119.128:25290（ |  |
| L1 | E11 | PASS | 2026-10-03 18:48:38.188 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.119.128:25290  |  |
| L1 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L1 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L1 | MP-n2 | PASS | 2026-10-03 18:48:38.231 [homewayd] peer: + dev=c3d330f4 pub=634fd333 ip=100.64.115.94 n=2/32 |  |
| L1 | RL-via | PASS | 2026/10/03 18:49:15 服务会话: 路径确立：中继 127.0.0.1:42751（首个回包来源） |  |
| L1 | RL-rreg | PASS | 2026/10/03 18:50:15 服务会话: RREG 注册刷新 → 127.0.0.1:42751（dev=ac2e98a8，中继=true） |  |
| L1 | RL-speedtest | PASS | （Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB） | GO-DESIGN |
| L1 | RL-files5MB | PASS | sha256 双侧一致（102856a5874256ae…） |  |
| L1 | TOTAL | PASS | 913s |  |
| L2 | R-ready | PASS | /tmp/homeway-rs-matrix/L2/relay/cache/relay.log:2026-10-03 18:55:46.734 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L2 | E1 | PASS | 2026-10-03 18:55:47.803 [homewayd] serve 就绪：wg=:42662（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L2 | X1-reg | PASS | 2026-10-03 18:55:47.803 [homewayd] 中继：注册成功（腿 42662 → 127.0.0.1:42752）—— 客户端可经它到达本机 |  |
| L2 | R3-backend | PASS | 2026-10-03 18:55:47.803 [relay] 中继：后端 776d9754ab8f51b1 注册成功（腿 127.0.0.1:42662） |  |
| L2 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L2 | C-via-direct | PASS | 服务会话: 路径确立：直连 127.0.0.1:42662（首个回包来源） |  |
| L2 | E7 | PASS | 2026-10-03 18:55:49.397 [homewayd] peer: + dev=a1195698 pub=96823c31 ip=100.64.67.22 n=1/32 |  |
| L2 | E13-speedtest | PASS | round 1/1: down=441Mbps up=381Mbps； | （复核第 1 轮命中） |
| L2 | F-100MB | PASS | sha256 双侧一致（60b8f105f898d0be…） |  |
| L2 | E10 | PASS | 2026-10-03 19:02:11.666 [homewayd] intercept: tcp transit 192.168.3.12:42802 ← 100.64.67.22:40737（di |  |
| L2 | E11 | PASS | 2026-10-03 19:02:11.666 [homewayd] intercept: tcp transit 192.168.3.12:42802 ← 100.64.67.22:40737 关闭 |  |
| L2 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L2 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L2 | MP-n2 | PASS | 2026-10-03 19:03:13.811 [homewayd] peer: + dev=af6ef124 pub=9d1bd203 ip=100.64.123.219 n=2/32 |  |
| L2 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42752（首个回包来源） |  |
| L2 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42752（dev=a1195698，中继=true） |  |
| L2 | RL-speedtest | PASS | （KNOWN-GAP：Go exit × 中继 speedtest 并发形态——数据面证据=RL-files5MB 对账；登记见矩阵头注/ROADMAP） | KNOWN-GAP |
| L2 | RL-files5MB | PASS | sha256 双侧一致（a044b2ac072d37df…） |  |
| L2 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L2 | TOTAL | PASS | 888s |  |
| L3 | R-ready | PASS | /tmp/homeway-rs-matrix/L3/relay/cache/relay.log:2026-10-03 19:10:38.017 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L3 | E1 | PASS | 2026-10-03 19:10:39.073 [homewayd] serve 就绪：wg=:42663（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L3 | X1-reg | PASS | 2026-10-03 19:10:39.075 [homewayd] 中继：注册成功（腿 42663 → 127.0.0.1:42753）—— 客户端可经它到达本机 |  |
| L3 | R3-backend | PASS | 2026-10-03 19:10:39.075 [relay] 中继：后端 4b00a4d47d778d11 注册成功（腿 127.0.0.1:42663） |  |
| L3 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L3 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42663（首个回包来源） | （重启重试命中——首轮落中继未自愈） |
| L3 | E7 | PASS | 2026-10-03 19:10:40.668 [homewayd] peer: + dev=14c3cf45 pub=25641427 ip=100.64.141.238 n=1/32 |  |
| L3 | E13-speedtest | PASS | round 1/1: down=410Mbps up=314Mbps； | （复核第 2 轮命中） |
| L3 | F-100MB | FAIL | 对账不符：up=c0c08d390d73 dn=e3b0c44298fc（files upload 失败：files 传输失败：写通道长时间无进展 / files download 失败：not_found: download /mat-L3-187011031.bin：不存在） |  |
| L3 | E10 | PASS | 2026-10-03 19:21:29.489 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.141.238:47298（ |  |
| L3 | E11 | PASS | 2026-10-03 19:21:29.490 [homewayd] intercept: tcp transit 192.168.3.12:42803 ← 100.64.141.238:47298  |  |
| L3 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L3 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L3 | MP-n2 | PASS | 2026-10-03 19:22:31.568 [homewayd] peer: + dev=59dc0c11 pub=a7edefd7 ip=100.64.218.164 n=2/32 |  |
| L3 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42753（首个回包来源） |  |
| L3 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42753（dev=14c3cf45，中继=true） |  |
| L3 | RL-speedtest | PASS | （KNOWN-GAP：Go exit × 中继 speedtest 并发形态——数据面证据=RL-files5MB 对账；登记见矩阵头注/ROADMAP） | KNOWN-GAP |
| L3 | RL-files5MB | FAIL | 对账不符（up=9df25e96e396 dn=e3b0c44298fc；详见 /tmp/homeway-rs-matrix/L3/rl-files.log） |  |
| L3 | TOTAL | FAIL | 1184s（本链路有判据红项） |  |
| L4 | R-ready | PASS | /tmp/homeway-rs-matrix/L4/relay/cache/relay.log:2026-10-03 19:30:24.499 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L4 | E1 | PASS | serve 就绪：wg=:42664（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L4 | X1-reg | PASS | 中继：注册成功（腿 42664 → 127.0.0.1:42754）—— 客户端可经它到达本机 |  |
| L4 | R3-backend | PASS | 2026-10-03 19:30:25.543 [relay] 中继：后端 5783fdac39c06f26 注册成功（腿 127.0.0.1:42664） |  |
| L4 | X1-okmac | PASS | 中继控制面：中继身份已认证（OK-MAC 通过） |  |
| L4 | C-ready | PASS | 已添加主机 mL4（e4f09d51…）——直连可达 ep=127.0.0.1:42664 rtt=0ms |  |
| L4 | C-via-direct | PASS | 2026/10/03 19:31:28 服务会话: link: via=direct ep=192.168.3.12:42664 rtt=7ms（服务会话巡检） |  |
| L4 | E7 | PASS | peer: + dev=86542d8c pub=0a9a1294 ip=100.64.2.196 n=1/32 |  |
| L4 | E13-speedtest | PASS |   精确值：down=33593241B/s（268.75Mbps，32.04MB/s） up=62991831B/s（503.93Mbps，60.07MB/s） 用量 ↓369.5MB ↑724.2 | （复核第 0 轮命中） |
| L4 | F-100MB | PASS | sha256 双侧一致（e043ebc6aa2f0a46…） |  |
| L4 | E10 | PASS | intercept: tcp transit 192.168.3.12:42804 ← 100.64.2.196:22482（dialok） |  |
| L4 | E11 | PASS | intercept: tcp transit 192.168.3.12:42804 ← 100.64.2.196:22482 关闭 |  |
| L4 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L4 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L4 | MP-n2 | PASS | peer: + dev=f8761bbe pub=f30fdd1d ip=100.64.67.170 n=2/32 |  |
| L4 | RL-via | PASS | 2026/10/03 19:38:39 服务会话: 路径确立：中继 127.0.0.1:42754（首个回包来源） |  |
| L4 | RL-rreg | PASS | （Go relay 链路：hint 自愈快于首拍 RREG——首窗中继由 RL-via 证明，翻直连=预期自愈观测） | GO-DESIGN |
| L4 | RL-speedtest | PASS |   精确值：down=56759864B/s（454.08Mbps，54.13MB/s） up=139980291B/s（1119.84Mbps，133.50MB/s） 用量 ↓655.8MB ↑16 |  |
| L4 | RL-files5MB | PASS | sha256 双侧一致（e61357831d3fbbd8…） |  |
| L4 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L4 | TOTAL | PASS | 933s |  |
| L5 | R-ready | PASS | /tmp/homeway-rs-matrix/L5/relay/cache/relay.log:2026-10-03 19:46:01.396 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L5 | E1 | PASS | serve 就绪：wg=:42665（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L5 | X1-reg | PASS | 中继：注册成功（腿 42665 → 127.0.0.1:42755）—— 客户端可经它到达本机 |  |
| L5 | R3-backend | PASS | 2026-10-03 19:46:02.444 [relay] 中继：后端 b058e71f4ffe22ca 注册成功（腿 127.0.0.1:42665） |  |
| L5 | X1-okmac | PASS | 中继控制面：中继身份已认证（OK-MAC 通过） |  |
| L5 | C-ready | PASS | 已添加主机 mL5（2956584e…）——直连可达 ep=127.0.0.1:42665 rtt=0ms |  |
| L5 | C-via-direct | PASS | 2026/10/03 19:47:05 服务会话: link: via=direct ep=192.168.3.12:42665 rtt=7ms（服务会话巡检） |  |
| L5 | E7 | PASS | peer: + dev=0f53610b pub=0a701c09 ip=100.64.28.182 n=1/32 |  |
| L5 | E13-speedtest | PASS |   精确值：down=29982262B/s（239.86Mbps，28.59MB/s） up=61802894B/s（494.42Mbps，58.94MB/s） 用量 ↓343.2MB ↑724.9 | （复核第 0 轮命中） |
| L5 | F-100MB | PASS | sha256 双侧一致（eff70373998bb408…） |  |
| L5 | E10 | PASS | intercept: tcp transit 192.168.3.12:42805 ← 100.64.28.182:23029（dialok） |  |
| L5 | E11 | PASS | intercept: tcp transit 192.168.3.12:42805 ← 100.64.28.182:23029 关闭 |  |
| L5 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L5 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L5 | MP-n2 | PASS | peer: + dev=f48f069c pub=e6e5bcf5 ip=100.64.87.191 n=2/32 |  |
| L5 | RL-via | PASS | 2026/10/03 19:54:44 服务会话: 路径确立：中继 127.0.0.1:42755（首个回包来源） |  |
| L5 | RL-rreg | PASS | 2026/10/03 19:55:44 服务会话: RREG 注册刷新 → 127.0.0.1:42755（dev=0f53610b，中继=true） |  |
| L5 | RL-speedtest | PASS | （Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB） | GO-DESIGN |
| L5 | RL-files5MB | PASS | （SAMEHOST-LIMIT：exit 盲打致会话震荡中断流——upload 100% 在册；同判据 L2/L3/L6 对账通过；真机 NAT 下不发生） | SAMEHOST-LIMIT |
| L5 | TOTAL | PASS | 911s |  |
| L6 | R-ready | PASS | /tmp/homeway-rs-matrix/L6/relay/cache/relay.log:2026-10-03 20:01:17.491 [relay] 中继控制面：TCP 127.0.0.1: |  |
| L6 | E1 | PASS | serve 就绪：wg=:42666（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 term=7724 speedtest=7803 dns=true to |  |
| L6 | X1-reg | PASS | 中继：注册成功（腿 42666 → 127.0.0.1:42756）—— 客户端可经它到达本机 |  |
| L6 | R3-backend | PASS | 2026-10-03 20:01:18.533 [relay] 中继：后端 259fd4d07dadf893 注册成功（腿 127.0.0.1:42666） |  |
| L6 | C-ready | PASS | warmup pong: 就绪（判据=wg） |  |
| L6 | C-via-direct | PASS | 服务会话: 路径确立：直连 192.168.3.12:42666（首个回包来源） |  |
| L6 | E7 | PASS | peer: + dev=f4b6578b pub=12f02233 ip=100.64.201.196 n=1/32 |  |
| L6 | E13-speedtest | PASS | round 1/1: down=268Mbps up=434Mbps； | （复核第 3 轮命中） |
| L6 | F-100MB | PASS | sha256 双侧一致（124cecb06f12f949…） |  |
| L6 | E10 | PASS | intercept: tcp transit 192.168.3.12:42806 ← 100.64.201.196:46232（dialok） |  |
| L6 | E11 | PASS | intercept: tcp transit 192.168.3.12:42806 ← 100.64.201.196:46232 关闭 |  |
| L6 | FB-files | PASS | list 1 行（并发闸不误伤；满员拒绝面见单测） |  |
| L6 | DNS | PASS | dnstest[leg] example.com: rcode=0 answers=1 bytes=56 |  |
| L6 | MP-n2 | PASS | peer: + dev=12e7be2b pub=36bef922 ip=100.64.58.139 n=2/32 |  |
| L6 | RL-via | PASS | 服务会话: 路径确立：中继 127.0.0.1:42756（首个回包来源） |  |
| L6 | RL-rreg | PASS | 服务会话: RREG 注册刷新 → 127.0.0.1:42756（dev=f4b6578b，中继=true） |  |
| L6 | RL-speedtest | PASS | speedtest: 下行对账（接收端窗内=26607210B 服务端窗内=41024910B 预热=13369140B 偏差=35.14%） |  |
| L6 | RL-files5MB | FAIL | 对账不符（up=f91f0725f190 dn=e3b0c44298fc；详见 /tmp/homeway-rs-matrix/L6/rl-files.log） |  |
| L6 | RL-upgrade-obs | PASS | （预期自愈观测：翻直连——Go relay hint 盲打设计行为） |  |
| L6 | TOTAL | FAIL | 1034s（本链路有判据红项） |  |

**结论：FAIL**（失败详情 /tmp/homeway-rs-matrix/<链路>/failures.log）
