# 互操作矩阵最近一次运行（生成：2026-10-04 01:58:44；工具 tools/matrix.sh）

| 链路 | 判据 | 结果 | 摘录/耗时 | 备注 |
|---|---|---|---|---|
| L1 | R-ready | PASS | /tmp/homeway-rs-matrix/L1/relay/cache/relay.log:2026-10-04 01:43:53.060 [relay] 中继控制面：TCP 0.0.0.0:42 |  |
| L1 | E1 | PASS | 2026-10-04 01:43:54.165 [homewayd] serve 就绪：wg=:42661（配置端口；被占用会自动退让）tunnel=100.64.255.1 files=7802 t |  |
| L1 | X1-reg | PASS | 2026-10-04 01:43:54.166 [homewayd] 中继：注册成功（腿 42661 → 127.0.0.1:42751）—— 客户端可经它到达本机 |  |
| L1 | R3-backend | PASS | 2026-10-04 01:43:54.166 [relay] 中继：后端 5a220b64a9e51f47 注册成功（腿 127.0.0.1:42661） |  |
| L1 | C-ready | PASS | 已添加主机 mL1（7c014cec…）——直连可达 ep=127.0.0.1:42661 rtt=0ms |  |
| L1 | C-via-direct | PASS | 2026/10/04 01:44:56 服务会话: link: via=direct ep=192.168.3.12:42661 rtt=2ms（服务会话巡检） |  |
| L1 | E7 | PASS | 2026-10-04 01:43:56.690 [homewayd] peer: + dev=7835177f pub=e85d991e ip=100.64.74.84 n=1/32 |  |
| L1 | E13-speedtest | PASS |   精确值：down=42872997B/s（342.98Mbps，40.89MB/s） up=41031855B/s（328.25Mbps，39.13MB/s） 用量 ↓491.9MB ↑492.2 | （复核第 0 轮命中） |
| L1 | F-100MB | PASS | sha256 双侧一致（ee03cfc85fda4904…） |  |
| L1 | E10 | PASS | 2026-10-04 01:51:59.596 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.74.84:30630（di |  |
| L1 | E11 | PASS | 2026-10-04 01:51:59.596 [homewayd] intercept: tcp transit 192.168.3.12:42801 ← 100.64.74.84:30630 关闭 |  |
| L1 | FB-files | PASS | list 2 行（并发闸不误伤；满员拒绝面见单测） |  |
| L1 | DNS | PASS | （未纳入——Go 客户端无 DNS 拨号动词，设计 §1.6） |  |
| L1 | MP-n2 | PASS | 2026-10-04 01:51:59.653 [homewayd] peer: + dev=e9e33ca3 pub=e6faf7c0 ip=100.64.52.107 n=2/32 |  |
| L1 | RL-via | PASS | 2026/10/04 01:52:11 服务会话: 路径确立：中继 127.0.0.1:42751（首个回包来源） |  |
| L1 | RL-rreg | PASS | 2026/10/04 01:53:11 服务会话: RREG 注册刷新 → 127.0.0.1:42751（dev=7835177f，中继=true） |  |
| L1 | RL-speedtest | SKIP | （Go 客户端设计拒绝中继 speedtest——⚠️ 告警行在册；数据面证据=RL-files5MB） | GO-DESIGN |
| L1 | RL-files5MB | PASS | sha256 双侧一致（4aac1dc94639bced…） |  |
| L1 | TOTAL | PASS | 886s |  |

**结论：全绿**（豁免：WARN 0 / SKIP 1——各降档的独立证据绑定见备注列）
