#!/usr/bin/env python3
"""m6-serve.py — M6 真机终验的计时 server（设计 §1.4(f) 的读数面）。

用途：真机浏览器 →（TUN 隧道）→ 出口 intercept 过境 → 本 server；本 server 逐请求记
TSV：t_first_byte / t_last_byte / 字节 / 客户端地址 / path —— **轮次归集以 path 的 `?r=`
标签为主判据**（每轮唯一，与客户端地址无关；客户端地址只作辅助）。

用法：
    python3 tools/m6-serve.py <port> <file|dir> [--log /tmp/m6-ab/srv.log]

  · `file` = 单个文件 → 以 /<basename> 提供；`dir` = 目录 → 目录内每个文件按名提供；
  · 另供 `/ping`：~1KB 自动刷新页（meta refresh 3s）= §2.4 的「在用档」发生器；
  · 支持 Range（206，浏览器断点续传/多段请求）；日志逐段各记一行；
  · 绑定 0.0.0.0（TUN 目标候选如 bridge100/utun4 上都能被出口 dial 到）。

只读、无依赖（stdlib）；不写任何仓内文件。
"""
import argparse
import datetime
import http.server
import os
import socket
import socketserver
import sys
import threading
import time
from urllib.parse import urlparse, unquote

LOG_LOCK = threading.Lock()
LOGFILE = None


def log_tsv(*fields):
    line = "\t".join(str(f) for f in fields)
    with LOG_LOCK:
        if LOGFILE:
            with open(LOGFILE, "a", encoding="utf-8") as fh:
                fh.write(line + "\n")
        print(line, flush=True)


def now():
    t = time.time()
    return datetime.datetime.fromtimestamp(t).strftime("%Y-%m-%dT%H:%M:%S") + f".{int((t % 1) * 1000):03d}"


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "m6-serve/1.0"

    def log_message(self, fmt, *args):  # noqa: D401 - 静音默认 stderr 行（我们用 TSV）
        pass

    def _root(self):
        return self.server.m6_root, self.server.m6_isfile

    def _resolve(self, path):
        root, isfile = self._root()
        name = unquote(urlparse(path).path)
        if name == "/ping":
            return None, "ping"
        base = os.path.basename(name) if isfile else name.lstrip("/")
        if not base or "/" in base or base.startswith("."):
            return None, "bad"
        full = os.path.join(root, base) if not isfile else root
        if isfile and name not in ("/", "/" + os.path.basename(root)):
            # 单文件模式下只认该文件名（或 /）
            return None, "bad"
        if not os.path.isfile(full):
            return None, "missing"
        return full, "file"

    def _send_ping(self):
        body = (
            "<!doctype html><meta charset=utf-8>"
            "<meta http-equiv='refresh' content='3'>"
            "<title>m6 ping</title><h1>m6 ping</h1>"
            "<p>在用档发生器：本页每 3s 自动刷新一次，产生 TUN 常驻小流量。</p>"
            "<p>" + "padding " * 90 + "</p>"
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)
        return len(body)

    def _serve_range(self, full, label):
        size = os.path.getsize(full)
        rng = self.headers.get("Range")
        start, end = 0, size - 1
        status = 200
        if rng and rng.startswith("bytes="):
            spec = rng[len("bytes="):].split(",")[0].strip()
            a, _, b = spec.partition("-")
            try:
                if a == "":
                    start = max(0, size - int(b))
                else:
                    start = int(a)
                    if b:
                        end = min(int(b), size - 1)
                if 0 <= start <= end < size:
                    status = 206
                else:
                    start, end, status = 0, size - 1, 200
            except ValueError:
                start, end, status = 0, size - 1, 200
        length = end - start + 1
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(length))
        self.send_header("Accept-Ranges", "bytes")
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        sent = 0
        t_first = None
        t0 = time.time()
        with open(full, "rb") as fh:
            fh.seek(start)
            remaining = length
            while remaining > 0:
                chunk = fh.read(min(1 << 20, remaining))
                if not chunk:
                    break
                try:
                    self.wfile.write(chunk)
                except (BrokenPipeError, ConnectionResetError):
                    break
                if t_first is None:
                    t_first = time.time()
                sent += len(chunk)
                remaining -= len(chunk)
        t_last = time.time()
        log_tsv(
            self.server.m6_label,
            "req",
            now(),
            f"{t_first - t0:.6f}" if t_first else "-",
            f"{t_last - t0:.6f}",
            sent,
            self.client_address[0] + ":" + str(self.client_address[1]),
            self.path,
            status,
        )
        return sent

    def do_GET(self):
        full, kind = self._resolve(self.path)
        t0 = time.time()
        if kind == "ping":
            n = self._send_ping()
            log_tsv(self.server.m6_label, "ping", now(), f"{time.time()-t0:.6f}", n,
                    self.client_address[0] + ":" + str(self.client_address[1]), self.path, 200)
            return
        if kind != "file":
            body = b"m6-serve: not found\n"
            self.send_response(404)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            log_tsv(self.server.m6_label, "404", now(), "-", 0,
                    self.client_address[0] + ":" + str(self.client_address[1]), self.path, 404)
            return
        self._serve_range(full, self.path)

    def do_HEAD(self):
        full, kind = self._resolve(self.path)
        if kind != "file":
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        size = os.path.getsize(full)
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, addr, handler, root, isfile, label):
        super().__init__(addr, handler)
        self.m6_root = root
        self.m6_isfile = isfile
        self.m6_label = label


def main():
    global LOGFILE
    ap = argparse.ArgumentParser()
    ap.add_argument("port", type=int)
    ap.add_argument("path", help="file 或目录")
    ap.add_argument("--log", default=None, help="TSV 日志路径")
    args = ap.parse_args()

    isfile = os.path.isfile(args.path)
    root = args.path if isfile else os.path.abspath(args.path)
    if not os.path.exists(args.path):
        print(f"!! 路径不存在：{args.path}", file=sys.stderr)
        return 2
    LOGFILE = args.log
    label = socket.gethostname()
    srv = Server(("0.0.0.0", args.port), Handler, root, isfile, label)
    print(f"m6-serve: 0.0.0.0:{args.port} root={root} file={isfile} log={LOGFILE}", flush=True)
    log_tsv(label, "start", now(), 0, 0, "-", f"port={args.port}", 200)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
        log_tsv(label, "stop", now(), 0, 0, "-", "-", 200)
    return 0


if __name__ == "__main__":
    sys.exit(main())
