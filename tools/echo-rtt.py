#!/usr/bin/env python3
"""echo-rtt.py — 矩阵/性能档的回显往返延迟分布测量（R5-5c；评审 ③-2 整改：
真延迟指标 = 经隧道单连接 N 次回显往返，**测量器同一脚本两链路公平**——python 连
客户端本地转发口（Rust 链路 = portfwd 监听；Go 链路 = forward 监听），目标 = 矩阵
echo 服务；两链路差异只在隧道两侧实现栈）。

用法：python3 tools/echo-rtt.py <host> <port> [N=200]
输出：JSON 到 stdout：{"samples": N, "p50_ms": x, "p95_ms": x, "p99_ms": x, "max_ms": x, "fails": n}
"""
import json
import socket
import sys
import time


def main() -> None:
    host = sys.argv[1]
    port = int(sys.argv[2])
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 200
    payload = b"rtt-probe-" + b"x" * 54  # 64B 载荷

    lat = []
    fails = 0
    s = None
    try:
        s = socket.create_connection((host, port), timeout=10)
        s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        # 预热 5 次不在样本里（建连后首往返含调度冷启动）
        for _ in range(5):
            s.sendall(payload)
            s.recv(len(payload))
        for _ in range(n):
            t0 = time.perf_counter()
            s.sendall(payload)
            got = b""
            while len(got) < len(payload):
                chunk = s.recv(len(payload) - len(got))
                if not chunk:
                    raise ConnectionError("echo 断开")
                got += chunk
            lat.append((time.perf_counter() - t0) * 1000.0)
    except (OSError, ConnectionError):
        fails += 1
    finally:
        if s is not None:
            s.close()

    lat.sort()

    def pct(p: float) -> float:
        if not lat:
            return -1.0
        i = min(int(len(lat) * p), len(lat) - 1)
        return round(lat[i], 3)

    print(json.dumps({
        "samples": len(lat),
        "p50_ms": pct(0.50),
        "p95_ms": pct(0.95),
        "p99_ms": pct(0.99),
        "max_ms": round(lat[-1], 3) if lat else -1,
        "fails": fails,
    }))


if __name__ == "__main__":
    main()
