# term-attach-resilience · 端到端验证记录（2026-10-16）

环境：私有 Rust 出口（`tools/local-rust-exit.sh start 1`，`HOMEWAY_TERM_REPLAY=8192`
收小回放窗制造「环内无全帧」的 opencode 形态）+ 本地 Rust daemon 客户端
（`--state /tmp/homeway-rs-rclient-e2e`，host add rtest1，QUIC 直连 127.0.0.1:42652）+
自造差分渲染 TUI 探针（`scripts/diff_probe.py`：全屏画一次，之后仅行定位重写计数行，
周期发 DA1 查询）。

| # | 项 | 结果 | 证据 |
|---|---|---|---|
| A | new 建会话 → detach 干净退出 | PASS | CLI 正常退出码；出口侧会话保活 |
| B | 同几何 re-attach 静态内容完整（**间隔哨兵重绘**） | PASS（严谨版） | 探针先跑 28s 把全帧顶出 8KB 回放窗 ⇒ re-attach 后 **0.3s** 内 STATIC-TOP 重现（与 SENTINEL_GAP=300ms 吻合；回放不可能提供，唯一来源 = 哨兵两跳逼出的全屏重绘）；回放仍在喂探针计数 |
| C | 杀 daemon（链路断、会话在出口保活）→ 横幅 → 自动恢复 | PASS | 杀后 **1s** 出横幅「与主机的连接断开，重连中…（Ctrl-C 退出）」；**12s** 后 daemon 按需重生 + 岛重连 + 自动重接入 + 全屏重绘；探针 `t=15.6` 连续计时 = 会话全程未丢 |
| D | 在途应答 + SIGINT 退出无乱码 | PASS | 排队一条 DA1 应答后立刻 SIGINT：退出输出仅「收到信号退出（会话仍在运行）」，无 `62;1;2;6;9c` 数字残串 |

已知未决（不阻塞本批）：
- `scripts/e2e_term.py` 的 B→C 组合段曾出现一次「已分离」异常路径与探针重启迹象
  （B 段疑似连到了重生 daemon 的非预期会话）；手工拆段验证（上表 B/C/D）均正常，
  判为驱动脚本的组合状态问题。真机档复核项（见 tasks 5.2）。
- e2e 过程抓出并已修一个真 bug：连接段开头对空 poke 管道的裸 `read` 永久阻塞
  （CLI 卡死在主循环前，双向全哑）——修为 poll(0) 可读才清。

真机/用户触点档（5.2，不进 CI）：opencode attach 画面完整性、真网络中断
（关 Wi-Fi/断 VPN）→ 自动恢复、退出无乱码。
