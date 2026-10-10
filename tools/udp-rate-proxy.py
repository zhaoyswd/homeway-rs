#!/usr/bin/env python3
"""udp-rate-proxy.py — UDP 下行限速代理（M6.7 拥塞控制对比批的 lab 仪器）。

为什么需要它：回环上没有瓶颈 ⇒ 拥塞控制的选择量不出差别（M6.7 的真实难题是
「空口 44.8 MB/s 的 L0 锚 + 发送瞬时 47–59 MB/s 过载 ⇒ 丢包 ⇒ 退避」）。本代理给
回环补一条**有瓶颈、有队列、丢尾**的下行链路，复刻该形态的最小要素。

⚠️ **实测限制（M6.7 拥塞控制对比批踩到，先读再决定用不用）**：让出口把 `--public-endpoint`
指向本代理**不足以**把流量逼过来——客户端岛会**候选赛跑**，直连候选（出口 LAN 地址）仍会被
选中并绕过代理。判据 = 代理退出行：`fwd=2`（绕过）vs `fwd≈298k`（真过代理）。
**用法前提**：先把直连候选打掉（例如只留中继候选，或让出口只公布代理地址且直连地址不可达）。
未经此处理时，读数会在「整形」与「未整形」两态间随机混——**不可作对比读数**。

形态（与 `udp-delay-proxy.py` 同构，**只在 `tools/`（harness-only）**）：
  · 单 socket 绑 `listen`；按来源分流：客户端 → 出口**原样直发**（上行不限速）；
  · 出口 → 客户端走**令牌桶 + 丢尾队列**（`down_kbps` 限速，`queue_kb` 队列上限，
    超限即丢——丢包对发送端可见，这是本仪器与「纯延迟代理」的关键区别）；
  · 只跟踪最近一个客户端地址（一连接一源，同上游脚本的取舍）。
  · 静默丢非上述两方的包（不放大、不回错）；stdout 打一行就绪（供脚本 wait）。

用法：
    udp-rate-proxy.py <listen_ip:port> <upstream_ip:port> <down_kbps> [queue_kb=1024] [tick_ms=1]

读数：SIGTERM 时把 `fwd= / drop= / queue_drop=` 打到 stdout（作废/验证用）。
"""

import selectors
import signal
import socket
import sys
import time

READY = "udp-rate-proxy: ready"


class Bucket:
    """令牌桶 + 丢尾队列（字节口径）。"""

    def __init__(self, rate_bps: float, queue_bytes: int):
        self.rate = rate_bps
        self.tokens = 0.0
        self.cap = max(rate_bps * 0.05, 64 * 1024.0)  # 5% 突发额度（≈50ms @ 1Mbps 量级）
        self.queue = []  # [(data, addr)]
        self.qbytes = 0
        self.qlimit = queue_bytes
        self.last = time.monotonic()
        self.dropped = 0
        self.forwarded = 0

    def refill(self, now: float) -> None:
        dt = now - self.last
        if dt > 0:
            self.tokens = min(self.cap, self.tokens + self.rate * dt)
            self.last = now

    def push(self, data: bytes, addr) -> None:
        if self.tokens >= len(data):
            self.tokens -= len(data)
            return ("send", data, addr)
        if self.qbytes + len(data) <= self.qlimit:
            self.queue.append((data, addr))
            self.qbytes += len(data)
            return None
        self.dropped += 1
        return None

    def pop_ready(self):
        out = []
        while self.queue and self.tokens >= len(self.queue[0][0]):
            data, addr = self.queue.pop(0)
            self.qbytes -= len(data)
            self.tokens -= len(data)
            out.append((data, addr))
        return out


def main() -> None:
    listen = sys.argv[1]
    upstream = sys.argv[2]
    rate_kbps = float(sys.argv[3])
    queue_kb = int(sys.argv[4]) if len(sys.argv) > 4 else 1024
    tick_ms = int(sys.argv[5]) if len(sys.argv) > 5 else 1

    lip, lport = listen.rsplit(":", 1)
    uip, uport = upstream.rsplit(":", 1)

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((lip, int(lport)))
    up = (socket.gethostbyname(uip), int(uport))
    s.setblocking(False)

    bucket = Bucket(rate_kbps * 1000.0 / 8.0, queue_kb * 1024)
    client = None
    fwd = 0
    up_bytes = 0

    sel = selectors.DefaultSelector()
    sel.register(s, selectors.EVENT_READ)

    stop = {"v": False}

    def on_sig(_sig, _frm):
        stop["v"] = True

    signal.signal(signal.SIGTERM, on_sig)
    signal.signal(signal.SIGINT, on_sig)

    print(READY, flush=True)

    while not stop["v"]:
        for key, _ in sel.select(timeout=tick_ms / 1000.0):
            while True:
                try:
                    data, addr = key.fileobj.recvfrom(65535)
                except BlockingIOError:
                    break
                except OSError:
                    break
                if addr == up:
                    # 下行（出口 → 客户端）：限速 + 丢尾
                    if client is None:
                        continue
                    act = bucket.push(data, client)
                    if act is not None:
                        _, payload, dst = act
                        s.sendto(payload, dst)
                        bucket.forwarded += 1
                        fwd += 1
                else:
                    # 上行（客户端 → 出口）：原样直发，并记住客户端地址
                    client = addr
                    s.sendto(data, up)
                    up_bytes += len(data)
        now = time.monotonic()
        bucket.refill(now)
        for payload, dst in bucket.pop_ready():
            s.sendto(payload, dst)
            bucket.forwarded += 1
            fwd += 1

    print(
        f"udp-rate-proxy: fwd={fwd} up={up_bytes} B queue_drop={bucket.dropped} "
        f"rate={rate_kbps}kbps queue={queue_kb}KB",
        flush=True,
    )


if __name__ == "__main__":
    main()
