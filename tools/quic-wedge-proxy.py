#!/usr/bin/env python3
"""quic-wedge-proxy.py — UDP「楔子」代理（M3 S4 故障注入缝：黑洞窗口 / 持续黑洞）。

为什么需要它：S4 的两条**负向**用例要求「对端不回显但**连接不关**」——
①瞬时黑洞（窗口内丢光 ⇒ 复探应当成功 ⇒ 只记抖动、**不动作**）；
②服务面卡死形态（回显永不返回、连接仍活 ⇒ **必有动作**，不得无限静默）。
真出口的回显任务挂不起来（那是产品代码），故用**外向丢包**复刻客户端可见的同一谓词：
「写出去的探测/握手全无回音，而 QUIC 层没收到任何 CONNECT 帧 ⇒ `close_reason()` 为空」。

形态：单 socket 绑 `listen` + 按来源分流（QUIC 客户端 ↔ 出口各一地址），**默认双向转发**；
控制文件存在 ⇒ **静默丢弃一切**（不放大、不回错）。控制文件由用例创建/删除 ⇒ 窗口可精确投放。

用法：quic-wedge-proxy.py <listen_ip:port> <upstream_ip:port> <ctrl_file>
就绪时 stdout 打一行（harness/用例 wait）；SIGTERM/SIGINT 退出。
"""

import os
import selectors
import signal
import socket
import sys

READY = "quic-wedge-proxy: ready"


def main() -> None:
    listen = sys.argv[1]
    upstream = sys.argv[2]
    ctrl = sys.argv[3]
    lip, lport = listen.rsplit(":", 1)
    uip, uport = upstream.rsplit(":", 1)

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((lip, int(lport)))
    up = (socket.gethostbyname(uip), int(uport))
    s.setblocking(False)

    sel = selectors.DefaultSelector()
    sel.register(s, selectors.EVENT_READ)
    client = None  # 最近一次上行包的来源（回程目标；rebind 后自动更新）

    stop = False

    def on_sig(*_a):
        nonlocal stop
        stop = True

    signal.signal(signal.SIGTERM, on_sig)
    signal.signal(signal.SIGINT, on_sig)
    print(READY, flush=True)

    while not stop:
        for key, _mask in sel.select(timeout=0.2):
            data, from_ = s.recvfrom(65535)
            if os.path.exists(ctrl):
                continue  # 黑洞窗口：静默丢（不转发、不回错）
            if from_ == up or (from_[0], from_[1]) == up:
                # 出口 → 客户端
                if client is not None:
                    s.sendto(data, client)
            else:
                # 客户端 → 出口（跟踪最近来源）
                client = from_
                s.sendto(data, up)


if __name__ == "__main__":
    main()
