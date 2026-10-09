#!/usr/bin/env python3
"""udp-delay-proxy.py — UDP 双向延迟代理（M2 S5-3「300ms RTT 冷启动预算」的注入缝）。

为什么需要它：设计 §8 的「冷启动/恢复」判据要在**高 RTT** 下测准入（两往返 + 期限），
而本机回环 RTT ≈ 0.05ms ⇒ 必须人为注入。真值口径 = 「注入 300ms RTT 的本地代理下
≥3 轮成功」（S2-3 / S5-2）——`--delay-ms 150` 即单向 150ms、往返 300ms。

形态：单 socket（绑 `listen`）+ 按**来源**分流（QUIC 客户端 ↔ 出口各一地址），逐包
进优先队列按到期时间转发（`delay_ms` 单向）。跟踪最近一个客户端地址（QUIC 一连接一源；
探针/m1-ab 都是单实例）。**静默丢弃**非上述两方的包（不放大、不回错）。

用法：udp-delay-proxy.py <listen_ip:port> <upstream_ip:port> [delay_ms=150]
静默运行，SIGTERM/SIGINT 退出；stdout 打一行就绪（好让脚本 wait）。
"""

import heapq
import selectors
import signal
import socket
import sys
import time

READY = "udp-delay-proxy: ready"


def main() -> None:
    listen = sys.argv[1]
    upstream = sys.argv[2]
    delay_ms = int(sys.argv[3]) if len(sys.argv) > 3 else 150
    lip, lport = listen.rsplit(":", 1)
    uip, uport = upstream.rsplit(":", 1)

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((lip, int(lport)))
    up = (socket.gethostbyname(uip), int(uport))
    s.setblocking(False)

    sel = selectors.DefaultSelector()
    sel.register(s, selectors.EVENT_READ)
    pending: list = []  # (due, seq, data, dst)
    seq = 0
    client = None  # 最近一次上行包的来源（回程目标）

    stop = False

    def on_sig(*_a):
        nonlocal stop
        stop = True

    signal.signal(signal.SIGTERM, on_sig)
    signal.signal(signal.SIGINT, on_sig)
    print(READY, flush=True)

    delay = delay_ms / 1000.0
    while not stop:
        if pending:
            wait = max(0.0, pending[0][0] - time.time())
        else:
            wait = 5.0
        for key, _ in sel.select(timeout=min(wait, 5.0)):
            try:
                data, src = s.recvfrom(65535)
            except (BlockingIOError, OSError):
                continue
            if src[:2] == up:
                if client is not None:
                    seq += 1
                    heapq.heappush(pending, (time.time() + delay, seq, data, client))
            else:
                client = src
                seq += 1
                heapq.heappush(pending, (time.time() + delay, seq, data, up))
        now = time.time()
        while pending and pending[0][0] <= now:
            _, _, data, dst = heapq.heappop(pending)
            try:
                s.sendto(data, dst)
            except OSError:
                pass
    s.close()


if __name__ == "__main__":
    main()
