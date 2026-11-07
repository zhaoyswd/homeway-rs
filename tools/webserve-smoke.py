#!/usr/bin/env python3
"""Real WebServe/QUIC/PTY smoke in helper-owned /tmp unified instances.

Build homeway first. No production/default state is opened. Evidence remains in
the printed private directory. Optional --browser runs Chrome with a new profile;
--extended also checks real 30-second ticket expiry. No third-party server code.
"""
import argparse
import base64
from contextlib import ExitStack
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import shlex
import signal
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "crates/homeway-cli/src/webserve/assets"


def check_assets():
    sources = json.loads((ASSETS / "SOURCES.json").read_text())
    for source in sources:
        assert source["integrity"].startswith("sha512-") and source["tarball"].startswith("https://registry.npmjs.org/")
        for name, expected in source["files_sha256"].items():
            assert hashlib.sha256((ASSETS / name).read_bytes()).hexdigest() == expected, "vendored source hash: " + name
    code = """const fs=require('fs'),vm=require('vm');
      for(const name of ['xterm.js','fit.js','unicode11.js','app.js']){
        try{new vm.Script(fs.readFileSync(process.argv[1]+'/'+name,'utf8'));}
        catch(e){console.error(name+': '+e.name+': '+e.message);process.exit(1);}}
    """
    subprocess.run(["node", "-e", code, str(ASSETS)], check=True)
    print("PASS: pinned npm asset SHA256 and JavaScript parser checks", flush=True)


class ClipboardGuard:
    """Preserve every macOS pasteboard item's data in RAM; never log contents."""
    def __init__(self):
        self.pool = None
        self.saved = []
        if sys.platform != "darwin":
            return
        import ctypes
        self.c = ctypes
        ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
        self.objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
        self.objc.objc_getClass.argtypes = [ctypes.c_char_p]
        self.objc.objc_getClass.restype = ctypes.c_void_p
        self.objc.sel_registerName.argtypes = [ctypes.c_char_p]
        self.objc.sel_registerName.restype = ctypes.c_void_p
        self.sender = ctypes.cast(self.objc.objc_msgSend, ctypes.c_void_p).value
        self.pool = self.send(self.cls("NSAutoreleasePool"), "new")
        try:
            self.board = self.send(self.cls("NSPasteboard"), "generalPasteboard")
            self.count = self.send(self.board, "changeCount", ctypes.c_long)
            self.saved = self.read()
        except BaseException:
            self.send(self.pool, "drain", None)
            self.pool = None
            raise

    def cls(self, name):
        return self.objc.objc_getClass(name.encode())

    def send(self, obj, method, result=None, args=()):
        c = self.c
        if result is None and method != "drain":
            result = c.c_void_p
        fn = c.CFUNCTYPE(result, c.c_void_p, c.c_void_p, *(kind for kind, _ in args))(self.sender)
        return fn(obj, self.objc.sel_registerName(method.encode()), *(value for _, value in args))

    def read(self):
        c = self.c
        items = self.send(self.board, "pasteboardItems")
        result = []
        for i in range(self.send(items, "count", c.c_ulong)):
            item = self.send(items, "objectAtIndex:", args=((c.c_ulong, i),))
            types = self.send(item, "types")
            saved = []
            for j in range(self.send(types, "count", c.c_ulong)):
                kind = self.send(types, "objectAtIndex:", args=((c.c_ulong, j),))
                name = self.send(kind, "UTF8String", c.c_char_p)
                data = self.send(item, "dataForType:", args=((c.c_void_p, kind),))
                if not data:
                    raise RuntimeError("Cannot safely snapshot a promised clipboard format")
                length = self.send(data, "length", c.c_ulong)
                pointer = self.send(data, "bytes")
                saved.append((bytes(name), c.string_at(pointer, length) if length else b""))
            result.append(saved)
        return result

    def finish(self, touched):
        if self.pool is None:
            return {"host_clipboard_guard": "not-macOS", "clipboard_test_reached": touched}
        c = self.c
        try:
            changed = self.send(self.board, "changeCount", c.c_long) != self.count
            restored = False
            if touched and changed:
                array = self.send(self.cls("NSMutableArray"), "array")
                for saved in self.saved:
                    item = self.send(self.cls("NSPasteboardItem"), "new")
                    for name, payload in saved:
                        kind = self.send(self.cls("NSString"), "stringWithUTF8String:", args=((c.c_char_p, name),))
                        buffer = c.create_string_buffer(payload)
                        data = self.send(self.cls("NSData"), "dataWithBytes:length:",
                                         args=((c.c_void_p, c.cast(buffer, c.c_void_p)), (c.c_ulong, len(payload))))
                        assert self.send(item, "setData:forType:", c.c_bool,
                                         ((c.c_void_p, data), (c.c_void_p, kind))), "clipboard restoration failed"
                    self.send(array, "addObject:", args=((c.c_void_p, item),))
                    self.send(item, "release", None)
                self.send(self.board, "clearContents", c.c_long)
                assert not self.saved or self.send(self.board, "writeObjects:", c.c_bool,
                                                   ((c.c_void_p, array),)), "clipboard restoration failed"
                assert [dict(x) for x in self.read()] == [dict(x) for x in self.saved], "clipboard restoration verification failed"
                restored = True
            return {"host_clipboard_guard": "all-formats-in-memory", "clipboard_test_reached": touched,
                    "host_change_observed": changed, "original_restored": restored,
                    "headless_clipboard_isolated_observed": touched and not changed}
        finally:
            self.saved.clear()
            self.send(self.pool, "drain", None)
            self.pool = None


def wait_until(test, label, timeout=15, interval=0.1):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if test():
                return
        except (OSError, ValueError, AssertionError, http.client.HTTPException):
            pass
        time.sleep(interval)
    raise AssertionError("deadline: " + label)


def free_port():
    for _ in range(100):
        with socket.socket() as sock, socket.socket(type=socket.SOCK_DGRAM) as udp:
            sock.bind(("0.0.0.0", 0))
            port = sock.getsockname()[1]
            if port == 65535:
                continue
            try:
                udp.bind(("0.0.0.0", port))
            except OSError:
                continue
            return port
    raise AssertionError("could not reserve a private TCP/UDP port")


def request(base, path="/", token=None, origin=None, host=None, raw=None):
    parsed = urlsplit(base)
    cls = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    options = {"timeout": 15}
    if parsed.scheme == "https":
        options["context"] = ssl._create_unverified_context()  # This harness's own self-signed certificate only.
    conn = cls(parsed.hostname, parsed.port, **options)
    headers = {"Origin": origin or base}
    if host:
        headers["Host"] = host
    body = raw if raw is not None else (json.dumps({"token": token}).encode() if token is not None else None)
    if body is not None:
        headers["Content-Type"] = "application/json"
    try:
        conn.request("POST" if body is not None else "GET", path, body=body, headers=headers)
        response = conn.getresponse()
        return response.status, dict(response.getheaders()), response.read()
    finally:
        conn.close()


def login(base, token):
    status, _, body = request(base, "/api/login", token=token)
    assert status == 200, f"login HTTP {status} (response retained only by caller)"
    data = json.loads(body)
    assert len(data["ticket"]) == 43 and data["expires_in"] == 30
    return data["ticket"]


class WebSocket:
    """Small independent RFC6455 test peer; never used by the service."""
    def __init__(self, base, origin=None):
        parsed = urlsplit(base)
        self.sock = socket.create_connection((parsed.hostname, parsed.port), 10)
        if parsed.scheme == "https":
            self.sock = ssl._create_unverified_context().wrap_socket(self.sock, server_hostname=parsed.hostname)
        self.sock.settimeout(10)
        self.buffer = bytearray()
        self.output = bytearray()
        self.screen = None
        self.vt = None
        self.closed = False
        key = base64.b64encode(os.urandom(16)).decode()
        host = parsed.netloc
        self.sock.sendall((f"GET /ws HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin or base}\r\n"
                           "Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n"
                           f"Sec-WebSocket-Key: {key}\r\n\r\n").encode())
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.sock.recv(4096)
            if not chunk:
                self.sock.close()
                raise AssertionError("WS upgrade closed without a response")
            self.buffer.extend(chunk)
            assert len(self.buffer) <= 32768, "unbounded WS handshake"
        header, rest = self.buffer.split(b"\r\n\r\n", 1)
        self.buffer = bytearray(rest)
        assert header.startswith(b"HTTP/1.1 101"), f"WS upgrade failed: {header[:100]!r}"
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        assert accept.lower() in header.lower()

    def send(self, payload, op=2):
        if isinstance(payload, str):
            payload, op = payload.encode(), 1
        n = len(payload)
        size = bytes([0x80 | n]) if n < 126 else (b"\xfe" + struct.pack(">H", n) if n <= 65535 else b"\xff" + struct.pack(">Q", n))
        mask = os.urandom(4)
        self.sock.sendall(bytes([0x80 | op]) + size + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

    def exact(self, n, deadline):
        while len(self.buffer) < n:
            left = deadline - time.monotonic()
            if left <= 0:
                raise TimeoutError("WS deadline")
            self.sock.settimeout(left)
            chunk = self.sock.recv(max(4096, n - len(self.buffer)))
            if not chunk:
                self.closed = True
                raise EOFError("WS EOF")
            self.buffer.extend(chunk)
        value = bytes(self.buffer[:n])
        del self.buffer[:n]
        return value

    def recv(self, timeout=10):
        deadline = time.monotonic() + timeout
        while True:
            first, second = self.exact(2, deadline)
            assert first & 0x80 and not second & 0x80, "unexpected fragmented/masked server frame"
            n = second & 127
            if n == 126:
                n = struct.unpack(">H", self.exact(2, deadline))[0]
            elif n == 127:
                n = struct.unpack(">Q", self.exact(8, deadline))[0]
            assert n <= 65536, "oversized server frame"
            payload = self.exact(n, deadline)
            op = first & 15
            if op == 9:
                self.send(payload, 10)
                continue
            if op == 2:
                self.output.extend(payload)
                if self.vt is not None:
                    self.vt.feed(payload)
            if op == 8:
                self.closed = True
            return op, payload

    def authenticate(self, ticket, cols=120, rows=36):
        import pyte
        self.screen = pyte.Screen(cols, rows)
        self.vt = pyte.ByteStream(self.screen)
        self.send(json.dumps({"type": "auth", "ticket": ticket, "cols": cols, "rows": rows}))
        op, payload = self.recv()
        assert op == 1 and json.loads(payload)["type"] == "ready", f"WS authentication failed: {payload[:100]!r}"
        return self

    def output_contains(self, needle, timeout=10):
        from wcwidth import wcwidth
        def text():
            if self.screen is None:
                return ""
            rows = []
            for y in range(self.screen.lines):
                cells, x = [], 0
                while x < self.screen.columns:
                    data = self.screen.buffer[y][x].data or " "
                    cells.append(data)
                    x += max(1, wcwidth(data[0]))
                rows.append("".join(cells))
            return "\n".join(rows)
        deadline = time.monotonic() + timeout
        while needle not in text():
            op, payload = self.recv(max(0.1, deadline - time.monotonic()))
            assert op != 8, f"WS closed before expected output ({payload[:80]!r})"
            if time.monotonic() >= deadline:
                raise AssertionError("missing terminal output: " + needle)

    def expect_closed(self, timeout=7):
        deadline = time.monotonic() + timeout
        try:
            while time.monotonic() < deadline:
                op, payload = self.recv(max(0.1, deadline - time.monotonic()))
                if op == 8:
                    return struct.unpack(">H", payload[:2])[0] if len(payload) >= 2 else 1005
        except (EOFError, ConnectionError):
            return None
        raise AssertionError("WS did not close")

    def close(self):
        try:
            if not self.closed:
                self.send(struct.pack(">H", 1000), 8)
        except OSError:
            pass
        self.sock.close()
        self.closed = True


class Lab:
    def __init__(self, binary):
        source = binary.resolve(strict=True)
        self.path = Path(tempfile.mkdtemp(prefix="homeway-web-", dir="/tmp")).resolve()
        (self.path / "bin").mkdir(mode=0o700)
        self.binary = self.path / "bin/homeway"
        shutil.copy2(source, self.binary)
        digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        assert digest == hashlib.sha256(source.read_bytes()).hexdigest(), "binary changed while snapshotting"
        (self.path / "binary-receipt.json").write_text(json.dumps({
            "source": str(source), "private_binary": str(self.binary), "sha256": digest}, indent=2))
        self.cleanup = ExitStack()
        self.processes = []
        self.tokens = []
        self.log = (self.path / "commands.log").open("w")
        print("EVIDENCE:", self.path, flush=True)
        print("BINARY_SHA256:", digest, flush=True)

    def cli(self, state, *args, ok=True):
        result = subprocess.run([str(self.binary), *args, "--state", str(state)], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        self.log.write(f"{args[:2]} exit={result.returncode}\n")
        self.log.flush()
        if ok:
            assert result.returncode == 0, result.stderr.decode(errors="replace")[-400:]
        return result

    def start(self, name, role="web", relay_token=None):
        state = self.path / name
        state.mkdir(mode=0o700)
        port = free_port()
        if role == "relay":
            config = f'[serve]\nenabled=false\n[relay]\nenabled=true\nlisten="127.0.0.1:{port}"\nadvertise="127.0.0.1:{port}"\n'
        elif role == "exit":
            config = (f'[serve]\nenabled=true\nlisten={port}\nquic_listen={port}\nbind_interface="none"\n'
                      f'upnp=false\nstun=""\nstun6=""\npublic_endpoint="127.0.0.1:{port}"\n')
            if relay_token:
                config += "relay=" + json.dumps(relay_token) + "\n"
            config += '[relay]\nenabled=false\n'
        else:
            config = '[serve]\nenabled=false\n[relay]\nenabled=false\n'
        (state / "config.toml").write_text(config)
        env = os.environ.copy()
        env.update(HOMEWAY_UNIFIED="1", HOMEWAY_TEST_STATE=str(state), HOMEWAY_TEST_PORT=str(port), HOMEWAY_BIN=str(self.binary))
        for key in ("HOMEWAY_TERM", "HOMEWAY_TERM_SHELL", "HOMEWAY_TERM_VT", "HOMEWAY_TERM_DETECT"):
            env.pop(key, None)
        script = ROOT / "tools" / ("local-rust-relay.sh" if role == "relay" else "local-rust-exit.sh")
        result = subprocess.run([str(script), "start", "991"], cwd=ROOT, env=env, capture_output=True, timeout=30)
        (state / "helper.log").write_bytes(result.stdout + result.stderr)
        assert result.returncode == 0, "private helper failed: " + str(state)
        self.cleanup.callback(lambda: subprocess.run([str(script), "stop", "991"], cwd=ROOT, env=env,
                                                     capture_output=True, timeout=25))
        if role == "exit":
            wait_until(lambda: (state / "cache/listen_port.txt").exists(), "exit QUIC ready", 25)
            assert int((state / "cache/listen_port.txt").read_text()) == port
        return state, port

    def token(self, state, rotate=False):
        args = ("webserve", "token", "rotate") if rotate else ("webserve", "token")
        result = self.cli(state, *args)
        match = re.search(rb"hww1[A-Za-z0-9_-]{155}", result.stdout)
        assert match, "CLI did not return hww1 token"
        token = match[0].decode()
        self.tokens.append(token)
        return token

    def status(self, state):
        return json.loads(self.cli(state, "webserve", "status", "--json").stdout)

    def sessions(self, state):
        data = json.loads(self.cli(state, "term", "list", "--json").stdout)
        entries = data.get("sessions") if isinstance(data, dict) else data
        assert isinstance(entries, list), "invalid terminal session list"
        return entries

    def concurrent_tokens(self, state, all_success=False):
        processes = [subprocess.Popen([str(self.binary), "webserve", "token", "--state", str(state)],
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(8)]
        values, codes = [], []
        for process in processes:
            stdout, _ = process.communicate(timeout=25)
            codes.append(process.returncode)
            if process.returncode == 0:
                match = re.search(rb"hww1[A-Za-z0-9_-]{155}", stdout)
                assert match, "concurrent token response format"
                values.append(match[0].decode())
        assert values and len(set(values)) == 1, "concurrent successful token reads diverged"
        if all_success:
            assert all(code == 0 for code in codes), "running RPC token read failed"
        self.tokens.extend(values)
        return values[0], codes

    def web(self, state, origin=None):
        port = free_port()
        args = ["webserve", "start", "--port", str(port)]
        if origin:
            args += ["--origin", origin]
        self.cli(state, *args)
        base = f"http://127.0.0.1:{port}"
        wait_until(lambda: request(base)[0] == 200, "Web HTTP ready")
        pid = int((state / "pid").read_text())
        listener = subprocess.run(["lsof", "-n", "-P", "-a", "-p", str(pid), f"-iTCP:{port}",
                                   "-sTCP:LISTEN", "-Fn"], capture_output=True, text=True, check=True)
        names = [line[1:] for line in listener.stdout.splitlines() if line.startswith("n")]
        assert names and set(names) == {f"127.0.0.1:{port}"}, f"HTTP listener is not strictly loopback: {names}"
        for source in json.loads((ASSETS / "SOURCES.json").read_text()):
            for name, expected in source["files_sha256"].items():
                if name.endswith((".js", ".css")):
                    status, _, body = request(base, "/" + name)
                    assert status == 200 and hashlib.sha256(body).hexdigest() == expected, "embedded asset: " + name
        return base

    def tui_pids(self, state):
        result = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True, check=True)
        return [int(line.split()[0]) for line in result.stdout.splitlines()
                if str(state) in line and " tui " in line and "--no-spawn" in line]

    def udp_fds(self, state):
        pid = int((state / "pid").read_text())
        result = subprocess.run(["lsof", "-n", "-P", "-a", "-p", str(pid), "-iUDP", "-Ff"], capture_output=True, text=True)
        assert result.returncode in (0, 1), "lsof failed"
        return sum(line.startswith("f") for line in result.stdout.splitlines())

    def nginx(self, upstream, port):
        folder = self.path / "nginx"
        folder.mkdir(mode=0o700)
        key, cert = folder / "key.pem", folder / "cert.pem"
        result = subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                                 "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
                                 "-keyout", str(key), "-out", str(cert)], capture_output=True)
        assert result.returncode == 0, "private certificate generation failed"
        key.chmod(0o600)
        config = folder / "nginx.conf"
        config.write_text(f'''pid {folder / "nginx.pid"};
error_log {folder / "error.log"};
events {{ worker_connections 128; }}
http {{
    access_log off;
    map $http_upgrade $connection_upgrade {{ default upgrade; '' close; }}
    server {{
        listen 127.0.0.1:{port} ssl;
        server_name localhost;
        ssl_certificate {cert};
        ssl_certificate_key {key};
        location / {{
            proxy_pass {upstream};
            proxy_http_version 1.1;
            proxy_set_header Host $http_host;
            proxy_set_header Upgrade $http_upgrade;
            proxy_set_header Connection $connection_upgrade;
            proxy_read_timeout 75s;
            proxy_buffering off;
        }}
    }}
}}
''')
        log = (folder / "process.log").open("wb")
        proc = subprocess.Popen(["nginx", "-p", str(folder) + "/", "-c", str(config), "-g", "daemon off;"], stdout=log, stderr=log)
        def stop():
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
            log.close()
        self.cleanup.callback(stop)
        base = f"https://localhost:{port}"
        wait_until(lambda: proc.poll() is None and request(base)[0] == 200, "private HTTPS proxy ready")
        return base

    def close(self):
        self.cleanup.close()
        self.log.close()


def check_policy(base, token):
    assert request(base, "/api/login", token=token, origin="https://untrusted.invalid")[0] in (400, 401, 403)
    assert request(base, "/api/login", token=token, host="untrusted.invalid")[0] in (400, 401, 403)
    assert request(base, "/api/login", token=token,
                   host="user@127.0.0.1:" + str(urlsplit(base).port))[0] in (400, 403)
    for status, headers, _ in (request(base, host="untrusted.invalid"), request(base, "/?forbidden=1")):
        assert status in (400, 403)
        assert {key.lower(): value for key, value in headers.items()}.get("cache-control") == "no-store"
    assert request(base, "/api/login", raw=b"x" * 4097)[0] in (400, 413)
    ws = WebSocket(base)
    try:
        started = time.monotonic()
        ws.expect_closed(7)
        assert 4 <= time.monotonic() - started < 7
    finally:
        ws.close()
    ws = WebSocket(base)
    try:
        ws.send(b"x" * 65537)
        code = ws.expect_closed()
        assert code == 1009, f"oversized pre-auth WS frame close code {code!r}, expected 1009"
    finally:
        ws.close()
    parsed = urlsplit(base)
    if parsed.scheme == "http":
        with socket.create_connection((parsed.hostname, parsed.port), 3) as slow:
            started = time.monotonic()
            slow.sendall(b"POST /api/login HTTP/1.1\r\nHost: ")
            slow.settimeout(7)
            data = slow.recv(4096)
            assert not data or b"408" in data or b"400" in data
            assert time.monotonic() - started < 7
    print("PASS: Origin/Host/body/WS-frame/first-frame/slow-header policy", flush=True)


def browser_large_paste(page, screen_text, artifact):
    """Use a real foreground raw reader, avoiding shell line-size limits."""
    # One emoji crosses 65536 with either zero or six bracketed-paste prefix bytes.
    middle = "🙂AA🙂回家".encode()
    payload = b"A" * 65529 + middle + b"B" * (81920 - 65529 - len(middle))
    capture = artifact / "browser-large-paste.bin"
    ready = artifact / "browser-large-paste-ready"
    extra = artifact / "browser-overlimit-extra.bin"
    source = ("import os,tty,termios,hashlib,time,select\n"
              "old=termios.tcgetattr(0)\n"
              "try:\n"
              " tty.setraw(0)\n"
              f" open({str(ready)!r},'w').close()\n"
              " print('LARGE_READER_READY',flush=True)\n"
              " data=bytearray()\n"
              f" while len(data)<{len(payload)}: data.extend(os.read(0,{len(payload)}-len(data)))\n"
              f" open({str(capture)!r},'wb').write(data)\n"
              " print('LARGE_PASTE_OK',flush=True)\n"
              " extra=bytearray();deadline=time.monotonic()+8\n"
              " while time.monotonic()<deadline:\n"
              "  if select.select([0],[],[],max(0,deadline-time.monotonic()))[0]: extra.extend(os.read(0,65536))\n"
              f" open({str(extra)!r},'wb').write(extra)\n"
              "finally: termios.tcsetattr(0,termios.TCSADRAIN,old)\n"
              "print('OVERLIMIT_READER_DONE',flush=True)\n")
    # Keep the launch command on one shell line; insert_text normalizes newlines.
    launch = "exec(bytes.fromhex(" + repr(source.encode().hex()) + "))"
    page.keyboard.insert_text("python3 -c " + shlex.quote(launch))
    page.keyboard.press("Enter")
    wait_until(lambda: ready.exists() and "LARGE_READER_READY" in page.evaluate(screen_text),
               "foreground raw paste reader ready")
    start = page.evaluate("() => window.__webqa.binarySizes.length")
    page.evaluate("text => navigator.clipboard.writeText(text)", payload.decode())
    page.locator("#paste").click()
    wait_until(lambda: capture.exists() and "LARGE_PASTE_OK" in page.evaluate(screen_text),
               "80KiB clipboard delivered to raw target", 15)
    actual = capture.read_bytes()
    sizes = page.evaluate("start => window.__webqa.binarySizes.slice(start)", start)
    assert actual == payload
    # The outer TUI enables bracketed paste; its 12 framing bytes may be present.
    # Tiny concurrent terminal query replies are independent from clipboard data.
    paste_sizes = [size for size in sizes if size > 1024]
    assert sum(paste_sizes) in (len(payload), len(payload) + 12)
    assert sizes and all(size <= 65536 for size in sizes)
    prefix = (sum(paste_sizes) - len(payload)) // 2
    boundaries = [sum(paste_sizes[:index]) for index in range(1, len(paste_sizes))]
    assert any(offset + prefix < boundary < offset + prefix + 4
               for offset in (65529, 65535) for boundary in boundaries)
    # Actual clipboard input must be rejected before any payload frame is sent.
    # Button focus reports and the TUI's exact OSC11 background query response
    # are independent from clipboard data; retain every excluded byte in evidence.
    allowed_controls = ("\x1b[I", "\x1b[O", "\x1b]11;rgb:1010/1414/1919\x1b\\")
    cases = [("utf16-early-reject", "OVERSIZED_WEB_PASTE_" + "Z" * (2*1024*1024+1)),
             ("utf8-byte-reject", "OVERSIZED_UTF8_PASTE_" + "🙂" * 524289),
             ("controlled-buffered-amount", "BUFFERED_INPUT_MUST_NOT_SEND")]
    rejected = []
    for name, over_payload in cases:
        page.evaluate("text => navigator.clipboard.writeText(text)", over_payload)
        if name == "controlled-buffered-amount":
            page.evaluate("() => Object.defineProperty(window.__webqa.sockets.at(-1),'bufferedAmount',{configurable:true,value:2*1024*1024})")
        start = page.evaluate("() => window.__webqa.binarySizes.length")
        page.locator("#paste").click()
        wait_until(lambda: "输入未发送" in page.locator("#status").inner_text()
                   and page.evaluate("() => document.activeElement === document.querySelector('.xterm-helper-textarea')"),
                   "paste refused: " + name)
        page.wait_for_timeout(100)
        over_frames = page.evaluate("start => window.__webqa.binary.slice(start)", start)
        assert all(frame in allowed_controls for frame in over_frames), repr(over_frames)
        assert page.evaluate("() => window.__webqa.sockets.at(-1).readyState") == 1
        if name == "controlled-buffered-amount":
            page.evaluate("() => delete window.__webqa.sockets.at(-1).bufferedAmount")
        rejected.append({"case": name, "utf16_units": len(over_payload.encode('utf-16-le'))//2,
                         "utf8_bytes": len(over_payload.encode()), "clipboard_frames": 0,
                         "control_reports_hex": [frame.encode().hex() for frame in over_frames]})
    assert rejected[1]["utf16_units"] < 2*1024*1024 < rejected[1]["utf8_bytes"]
    wait_until(lambda: extra.exists(), "raw target overlimit observation completed", 10)
    assert extra.read_bytes() == b"" and capture.read_bytes() == payload
    (artifact / "browser-large-paste.json").write_text(json.dumps({
        "payload_bytes": len(payload), "received_bytes": len(actual),
        "expected_sha256": hashlib.sha256(payload).hexdigest(),
        "received_sha256": hashlib.sha256(actual).hexdigest(), "binary_frame_sizes": sizes,
        "paste_frame_sizes": paste_sizes,
        "utf8_emoji_crosses_frame_boundary": True,
        "rejected_inputs": rejected,
        "over_limit_target_extra_bytes": extra.stat().st_size,
        "target_extra_observation_seconds": 8,
        "wire_emoji_offsets": [65529 + prefix, 65535 + prefix],
        "wire_frame_boundaries": boundaries,
        "socket_remained_open": True}, indent=2))
    print("PASS: real clipboard 80KiB target bytes/SHA and >2MiB pre-send refusal", flush=True)


def browser_check(base, token, state, artifact, chrome, alternate_token=None, alternate_session=None):
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(executable_path=chrome, headless=True)
        clipboard, clipboard_touched = None, False
        try:
            clipboard = ClipboardGuard()
            context = browser.new_context(ignore_https_errors=True, viewport={"width": 1280, "height": 900},
                                          permissions=["clipboard-read", "clipboard-write"])
            page = context.new_page()
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.add_init_script('''window.__webqa={incoming:[],outgoing:[],resizes:[],sockets:[],binary:[],binarySizes:[]};
let RealTerminal;
Object.defineProperty(window,'Terminal',{configurable:true,get(){return RealTerminal;},set(value){
 RealTerminal=class extends value{constructor(...args){super(...args);window.__webqa.terminal=this;}};
}});
const RealWS=window.WebSocket;
window.WebSocket=class extends RealWS {
 constructor(...args){super(...args);window.__webqa.sockets.push(this);this.addEventListener('message',e=>{
 if(typeof e.data==='string')window.__webqa.incoming.push(e.data);
 else if(e.data instanceof ArrayBuffer)window.__webqa.incoming.push(new TextDecoder().decode(e.data));
 else e.data.arrayBuffer().then(b=>window.__webqa.incoming.push(new TextDecoder().decode(b)));
 });}
 send(data){if(typeof data==='string'){window.__webqa.outgoing.push(data);
 try{const v=JSON.parse(data);if(v.type==='resize')window.__webqa.resizes.push(v);}catch{}}
 else if(ArrayBuffer.isView(data)){window.__webqa.binary.push(new TextDecoder().decode(data));window.__webqa.binarySizes.push(data.byteLength);}
 super.send(data);}
};''')
            page.goto(base, wait_until="networkidle")
            field = page.locator("#token")
            field.fill(token)
            page.locator("#connect").click()
            page.wait_for_selector(".xterm-screen", timeout=15000)
            screen_text = """() => {const t=window.__webqa.terminal;let s='';
              for(let y=0;y<t.rows;y++)s+=t.buffer.active.getLine(t.buffer.active.viewportY+y)?.translateToString(false)+'\\n';return s;}"""
            wait_until(lambda: "主机" in page.evaluate(screen_text), "browser TUI rendered")
            page.locator(".xterm-helper-textarea").focus()
            page.keyboard.press("Control+b")
            page.keyboard.press("n")
            page.wait_for_timeout(800)
            page.keyboard.insert_text("printf 'WEB%s\\nCOPY_%s\\n' '浏览器回家🙂' 'QA'")
            page.keyboard.press("Enter")
            wait_until(lambda: "WEB浏览器回家🙂" in page.evaluate(screen_text), "browser rendered CJK and emoji")
            glyph_widths = page.evaluate("""() => {const t=window.__webqa.terminal,out=[];
              for(let y=0;y<t.rows;y++){const l=t.buffer.active.getLine(t.buffer.active.viewportY+y);
                for(let x=0;x<t.cols;x++){const c=l.getCell(x);if(['回','家','🙂'].includes(c.getChars()))
                  out.push({symbol:c.getChars(),width:c.getWidth()});}}return out;}""")
            (artifact / "browser-glyph-widths.json").write_text(json.dumps(glyph_widths, ensure_ascii=False, indent=2))
            assert any(item["symbol"] == "🙂" for item in glyph_widths)
            assert all(item["width"] == 2 for item in glyph_widths), "CJK/emoji cell width differs from the Rust VT"
            cell = page.evaluate("""() => {const t=window.__webqa.terminal;
              const r=document.querySelector('.xterm-screen').getBoundingClientRect();
              for(let y=0;y<t.rows;y++){const line=t.buffer.active.getLine(t.buffer.active.viewportY+y);
                for(let x=0;x<t.cols-7;x++){let s='';for(let k=0;k<7;k++)s+=line.getCell(x+k).getChars();
                  if(s==='COPY_QA')return {x:r.x+(x+.05)*r.width/t.cols,y:r.y+(y+.5)*r.height/t.rows,w:r.width/t.cols};}}
              throw new Error('copy marker not rendered');}""")
            selection_modifier = "Alt" if sys.platform == "darwin" else "Shift"
            page.keyboard.down(selection_modifier)
            page.mouse.move(cell["x"], cell["y"])
            page.mouse.down()
            page.mouse.move(cell["x"] + 7 * cell["w"], cell["y"], steps=8)
            page.mouse.up()
            page.keyboard.up(selection_modifier)
            selection = page.evaluate("window.__webqa.terminal.getSelection()")
            assert "COPY_QA" in selection, "mouse selection did not cover the rendered copy marker"
            clipboard_touched = True
            page.locator("#copy").click()
            wait_until(lambda: "已复制" in page.locator("#status").inner_text(), "browser explicit copy")
            assert "COPY_QA" in page.evaluate("navigator.clipboard.readText()")
            page.evaluate("navigator.clipboard.writeText(\"printf 'PASTE_%s\\\\n' '回家'\")")
            page.locator("#paste").click()
            wait_until(lambda: page.evaluate("() => document.activeElement === document.querySelector('.xterm-helper-textarea')")
                       and "PASTE_%s" in page.evaluate(screen_text), "paste delivered and terminal refocused")
            page.keyboard.press("Enter")
            wait_until(lambda: "PASTE_回家" in page.evaluate(screen_text), "browser real clipboard paste")
            page.evaluate("() => navigator.clipboard.writeText('OSC52_BEFORE_CONFIRM')")
            osc_value = "WEB_OSC52_回家"
            encoded = base64.b64encode(osc_value.encode()).decode()
            page.keyboard.insert_text("printf '\\033]52;c;" + encoded + "\\007'")
            page.keyboard.press("Enter")
            wait_until(lambda: "终端请求复制" in page.locator("#status").inner_text(), "actual TUI OSC52 staged")
            assert page.evaluate("navigator.clipboard.readText()") == "OSC52_BEFORE_CONFIRM"
            page.locator("#copy").click()
            wait_until(lambda: page.evaluate("navigator.clipboard.readText()") == osc_value,
                       "OSC52 requires explicit page confirmation")
            browser_large_paste(page, screen_text, artifact)
            page.mouse.click(cell["x"], cell["y"])
            page.set_viewport_size({"width": 1100, "height": 760})
            wait_until(lambda: page.evaluate("() => window.__webqa.resizes.length > 0"), "browser resize", 5)
            page.screenshot(path=str(artifact / "browser-tui.png"))
            if alternate_token is not None:
                # A real paste action is held across an actual A -> B reconnect;
                # then deliver controlled late A callbacks to the original object.
                page.evaluate("""() => {window.__oldClipboardRead=navigator.clipboard.readText.bind(navigator.clipboard);
                  navigator.clipboard.readText=()=>new Promise(resolve=>{window.__releaseOldPaste=resolve;});}""")
                page.locator("#paste").click()
                wait_until(lambda: page.evaluate("() => typeof window.__releaseOldPaste === 'function'"), "old paste pending")
                page.locator("#disconnect").click()
                page.locator("#token").fill(alternate_token)
                page.locator("#connect").click()
                page.wait_for_selector(".xterm-screen", timeout=15000)
                wait_until(lambda: alternate_session in page.evaluate(screen_text), "new target B rendered")
                page.evaluate("""async () => {
                  window.__releaseOldPaste("printf 'STALE_INPUT'\\r");await Promise.resolve();await Promise.resolve();
                  const old=window.__webqa.sockets[0];
                  old.dispatchEvent(new MessageEvent('message',{data:new TextEncoder().encode('STALE_TARGET_A').buffer}));
                  old.dispatchEvent(new MessageEvent('message',{data:JSON.stringify({type:'error',message:'STALE_TARGET_A'})}));
                  old.dispatchEvent(new Event('close'));
                  navigator.clipboard.readText=window.__oldClipboardRead;
                }""")
                page.wait_for_timeout(100)
                assert not page.evaluate("() => window.__webqa.binary.some(x=>x.includes('STALE_INPUT'))"), "old paste reached another target"
                assert "STALE_TARGET_A" not in page.evaluate(screen_text)
                assert "STALE_TARGET_A" not in page.locator("#status").inner_text()
                assert alternate_session in page.evaluate(screen_text)
                page.screenshot(path=str(artifact / "browser-rapid-reconnect.png"))
                print("PASS: real target A/B reconnect ignores controlled old paste/output/error/close callbacks", flush=True)
            assert not errors, errors
            (artifact / "browser-observations.json").write_text(json.dumps({
                "page_errors": errors, "resize_messages": page.evaluate("window.__webqa.resizes"),
                "actual_ui": True, "rendered_cjk_emoji": True, "mouse_selection": True,
                "actual_clipboard_copy_paste": True, "emoji_width_two": True,
                "controlled_old_callbacks_rejected": alternate_token is not None,
                "state": str(state)}, ensure_ascii=False, indent=2))
            context.close()
            print("PASS: real HTTPS browser login/TUI/CJK+emoji/mouse-selection/clipboard copy-paste/resize; screenshot saved", flush=True)
        except BaseException:
            if "page" in locals():
                page.screenshot(path=str(artifact / "browser-failed.png"))
                (artifact / "browser-failed-dom.html").write_text(page.content())
            raise
        finally:
            browser.close()
            if clipboard is not None:
                receipt = clipboard.finish(clipboard_touched)
                (artifact / "browser-clipboard-receipt.json").write_text(json.dumps(receipt, indent=2))


def browser_failure_checks(lab, base, token, gateway, artifact, chrome):
    """Real browser/socket failures; controlled responses are named in evidence."""
    from playwright.sync_api import sync_playwright
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(executable_path=chrome, headless=True)
        try:
            context = browser.new_context(ignore_https_errors=True)
            context.add_init_script('''window.__failureQA={sockets:[],binary:[]};let T;
Object.defineProperty(window,'Terminal',{configurable:true,get(){return T;},set(V){T=class extends V{constructor(...a){super(...a);window.__failureQA.terminal=this;}};}});
const W=window.WebSocket;window.WebSocket=class extends W{constructor(...a){super(...a);window.__failureQA.sockets.push(this);}send(d){
 if(ArrayBuffer.isView(d))window.__failureQA.binary.push(Array.from(d));super.send(d);}};''')
            def connect(page):
                page.goto(base)
                page.locator("#token").fill(token)
                page.locator("#connect").click()
                wait_until(lambda: not page.locator("#paste").is_disabled(), "failure QA real ready")
            first, second = context.new_page(), context.new_page()
            connect(first)
            connect(second)
            first.set_viewport_size({"width": 1000, "height": 700})
            second.set_viewport_size({"width": 1300, "height": 850})
            first.wait_for_timeout(150)
            first_size = first.evaluate("() => [window.__failureQA.terminal.cols,window.__failureQA.terminal.rows]")
            second_size = second.evaluate("() => [window.__failureQA.terminal.cols,window.__failureQA.terminal.rows]")
            assert first_size != second_size
            # A fixed error/exit event on a real old socket must survive its close.
            first.evaluate("""() => {const w=window.__failureQA.sockets.at(-1);
              w.dispatchEvent(new MessageEvent('message',{data:JSON.stringify({type:'error',code:'busy',message:'QA_TARGET_BUSY'})}));w.close();}""")
            wait_until(lambda: "重新连接" in first.locator("#disconnect").inner_text(), "error followed by close")
            assert "QA_TARGET_BUSY" in first.locator("#status").inner_text()
            assert first.locator("#paste").is_disabled()
            assert first.evaluate("() => window.__failureQA.terminal.options.disableStdin")
            assert not second.locator("#paste").is_disabled(), "one page closure changed another page"
            exited = context.new_page()
            connect(exited)
            exited.evaluate("""() => {const w=window.__failureQA.sockets.at(-1);
              w.dispatchEvent(new MessageEvent('message',{data:JSON.stringify({type:'exit',status:7})}));w.close();}""")
            wait_until(lambda: "重新连接" in exited.locator("#disconnect").inner_text(), "exit followed by close")
            assert "TUI 已退出（7）" in exited.locator("#status").inner_text()
            second.locator(".xterm-helper-textarea").focus()
            second.keyboard.press("Escape")
            # Actual gateway restart aborts the transport after nonempty input.
            lab.cli(gateway, "webserve", "restart")
            wait_until(lambda: second.locator("#paste").is_disabled()
                       and "部分内容" in second.locator("#status").inner_text(), "asynchronous close partial-input warning")
            before = second.evaluate("() => window.__failureQA.binary.length")
            second.keyboard.insert_text("MUST_NOT_SEND_WHILE_CLOSED")
            second.wait_for_timeout(100)
            assert second.evaluate("() => window.__failureQA.binary.length") == before
            second.screenshot(path=str(artifact / "browser-readonly-failure.png"))
            interrupted = context.new_page()
            connect(interrupted)
            interrupted.evaluate("""() => {const w=window.__failureQA.sockets.at(-1),send=w.send.bind(w);
              w.send=d=>{if(ArrayBuffer.isView(d))throw new Error('controlled QA send failure');return send(d);};}""")
            interrupted.locator(".xterm-helper-textarea").focus()
            interrupted.keyboard.press("Escape")
            wait_until(lambda: interrupted.locator("#paste").is_disabled()
                       and "部分内容" in interrupted.locator("#status").inner_text(), "synchronous send exception closes read-only")
            # Controlled malformed ticket, with a real server 1008 response.
            broken = context.new_page()
            broken.add_init_script('''const ParentWS=window.WebSocket;window.WebSocket=class extends ParentWS{send(d){if(typeof d==='string'){try{let a=JSON.parse(d);if(a.type==='auth'){a.ticket='INVALID_QA_TICKET';d=JSON.stringify(a);}}catch{}}super.send(d);}};''')
            broken.goto(base)
            broken.locator("#token").fill(token)
            broken.locator("#connect").click()
            wait_until(lambda: broken.locator("#login").is_visible()
                       and "授权已失效" in broken.locator("#status").inner_text(), "pre-ready 1008 returns to login")
            assert broken.locator("#paste").is_disabled()
            # Empty HTTP error from a controlled reverse-proxy-like response.
            empty = context.new_page()
            empty.route("**/api/login", lambda route: route.fulfill(status=503, body="", content_type="text/plain"))
            empty.goto(base)
            empty.locator("#token").fill(token)
            empty.locator("#connect").click()
            wait_until(lambda: "目标管理能力暂不可用" in empty.locator("#status").inner_text(), "non-JSON 503 fixed message")
            (artifact / "browser-failures.json").write_text(json.dumps({
                "two_live_page_sizes": [first_size, second_size], "separate_page_close": True,
                "controlled_error_preserved": "QA_TARGET_BUSY", "actual_restart_partial_warning": True,
                "controlled_exit_preserved": 7, "controlled_send_exception": True,
                "closed_stdin_emitted_bytes": 0, "actual_invalid_ticket_close": 1008,
                "controlled_empty_http_status": 503}, indent=2))
            context.close()
            print("PASS: two real browser pages isolate sizes; failure/read-only/relogin/partial-input states", flush=True)
        finally:
            browser.close()


def parent_loss_check(lab):
    """Kill only a fresh helper-owned parent; its Web TUI must not spawn a daemon."""
    state, _ = lab.start("parent-loss")
    token = lab.token(state)
    with socket.socket() as occupied:
        occupied.bind(("127.0.0.1", 0))
        occupied.listen()
        denied = lab.cli(state, "webserve", "start", "--port", str(occupied.getsockname()[1]), ok=False)
        assert denied.returncode != 0 and lab.status(state)["state"] == "failed"
        lab.cli(state, "webserve", "stop")
        disabled = lab.status(state)
        assert disabled["state"] == "disabled" and disabled["reason"] is None
    base = lab.web(state)
    ws = WebSocket(base).authenticate(login(base, token))
    ws.output_contains("主机")
    pid = int((state / "pid").read_text())
    children = lab.tui_pids(state)
    assert children, "real Web PTY child must be running before parent loss"
    expected_child = str(lab.binary) + " tui --no-spawn --state " + str(state)
    def cleanup_known_children():
        for child in children:
            command = subprocess.run(["ps", "-p", str(child), "-o", "command="],
                                     capture_output=True, text=True).stdout.strip()
            if command != expected_child:
                continue
            try:
                os.kill(child, signal.SIGTERM)
            except ProcessLookupError:
                continue
            time.sleep(.2)
            command = subprocess.run(["ps", "-p", str(child), "-o", "command="],
                                     capture_output=True, text=True).stdout.strip()
            if command == expected_child:
                try:
                    os.kill(child, signal.SIGKILL)
                except ProcessLookupError:
                    pass
    lab.cleanup.callback(cleanup_known_children)
    os.kill(pid, signal.SIGKILL)
    try:
        ws.expect_closed(8)
    finally:
        ws.close()
    try:
        wait_until(lambda: not lab.tui_pids(state), "parent loss TUI exits", 8)
    except AssertionError:
        processes = subprocess.run(["ps", "-axo", "pid=,ppid=,stat=,command="],
                                   capture_output=True, text=True, check=True).stdout
        (lab.path / "parent-loss-failure.json").write_text(json.dumps({
            "parent_pid": pid, "initial_tui_pids": children,
            "private_processes": [line for line in processes.splitlines() if str(lab.binary) in line]}, indent=2))
        if sys.platform == "darwin":
            for child in lab.tui_pids(state):
                subprocess.run(["sample", str(child), "1", "1", "-file",
                                str(lab.path / f"parent-loss-{child}-sample.txt")], capture_output=True, timeout=10)
        raise
    snapshots = []
    for _ in range(10):
        rows = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True,
                              text=True, check=True).stdout.splitlines()
        current = [row.strip() for row in rows if str(lab.binary) in row and str(state) in row]
        snapshots.append(current)
        assert not current, "orphan daemon or interface after private parent loss"
        try:
            connection = socket.create_connection(("127.0.0.1", urlsplit(base).port), timeout=.3)
        except OSError:
            pass
        else:
            connection.close()
            raise AssertionError("Web listener restarted after parent loss")
        time.sleep(.2)
    (lab.path / "parent-loss.json").write_text(json.dumps({"parent_pid": pid,
        "initial_tui_pids": children, "process_observations": snapshots,
        "web_listener_restarted": False, "private_state": str(state)}, indent=2))
    print("PASS: forced private parent/control loss reaps Web TUI without daemon/listener restart", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path("target/debug/homeway"))
    parser.add_argument("--browser", action="store_true")
    parser.add_argument("--extended", action="store_true")
    parser.add_argument("--chrome", default="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
    args = parser.parse_args()
    check_assets()
    lab = Lab(args.bin)
    try:
        offline = lab.path / "offline"
        first = lab.token(offline)
        assert first == lab.token(offline) and first != lab.token(offline, rotate=True)
        assert not (offline / "config.toml").exists() and not (offline / "control.sock").exists()
        assert len((offline / "web/auth.key").read_bytes()) == 53
        assert (offline / "web/auth.key").stat().st_mode & 0o777 == 0o600
        concurrent, offline_codes = lab.concurrent_tokens(lab.path / "offline-race")
        assert concurrent == lab.token(lab.path / "offline-race")
        locked = lab.path / "locked"
        locked.mkdir(mode=0o700)
        with (locked / "lock").open("w+") as held:
            held.write(f"{os.getpid()}\nunified\n")
            held.flush()
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            denied = lab.cli(locked, "webserve", "token", ok=False)
            assert denied.returncode != 0
            assert not (locked / "web/auth.key").exists() and not (locked / "config.toml").exists()
            assert not (locked / "control.sock").exists()
        assert lab.token(locked)
        print("PASS: offline stable/rotated credential without config or service bootstrap", flush=True)
        local, _ = lab.start("local")
        token = lab.token(local)
        assert token == lab.token(local)
        live_token, live_codes = lab.concurrent_tokens(local, all_success=True)
        assert live_token == token
        (lab.path / "credential-races.json").write_text(json.dumps({"offline_exit_codes": offline_codes,
            "live_exit_codes": live_codes, "lock_without_control_denied": True}, indent=2))
        print("PASS: offline/live concurrent token stability and occupied-lock no-write boundary", flush=True)
        assert not (local / "cache/listen_port.txt").exists()
        local_base = lab.web(local)
        check_policy(local_base, token)
        ticket = login(local_base, token)
        ws = WebSocket(local_base).authenticate(ticket)
        try:
            ws.output_contains("主机")
        finally:
            ws.close()
        wait_until(lambda: not lab.tui_pids(local), "local TUI reaped")
        replay = WebSocket(local_base)
        try:
            replay.send(json.dumps({"type": "auth", "ticket": ticket, "cols": 100, "rows": 30}))
            replay.expect_closed()
        finally:
            replay.close()
        print("PASS: Web-only local TUI, stable token, one-use ticket, interface reap", flush=True)

        gateway, relay_port = lab.start("gateway", "relay")
        relay_output = lab.cli(gateway, "relay", "token").stdout
        relay_token = re.search(rb"rl1[A-Za-z0-9_-]+", relay_output)[0].decode()
        target, _ = lab.start("target", "exit", relay_token)
        target2, _ = lab.start("target2", "exit", relay_token)
        before_install = {name: {"pid": (state / "pid").read_text(),
                                 "port": (state / "cache/listen_port.txt").read_text(),
                                 "configured": lab.status(state)["remote_agent_available"]}
                          for name, state in [("target", target), ("target2", target2)]}
        assert not any(item["configured"] for item in before_install.values())
        target_token, target_token2 = lab.token(target), lab.token(target2)
        for name, state in [("target", target), ("target2", target2)]:
            assert lab.status(state)["remote_agent_available"]
            assert (state / "pid").read_text() == before_install[name]["pid"]
            assert (state / "cache/listen_port.txt").read_text() == before_install[name]["port"]
        (lab.path / "hot-install.json").write_text(json.dumps({
            "before": before_install, "after_available_without_restart": True}, indent=2))
        assert not (target / "webserve.toml").exists(), "target HTTP must remain disabled"
        before = (lab.udp_fds(gateway), lab.udp_fds(target))
        tls_port = free_port()
        public = f"https://localhost:{tls_port}"
        gateway_http = lab.web(gateway, public)
        gateway_base = lab.nginx(gateway_http, tls_port)
        wait_until(lambda: request(gateway_base, "/api/login", token=target_token)[0] == 200,
                   "registered target login", 40, 1)
        # The successful readiness login owns a ticket; restart only this Web entry to release it.
        lab.cli(gateway, "webserve", "restart")
        wait_until(lambda: request(gateway_base)[0] == 200, "Web restart")
        assert request(local_base, "/api/login", token=target_token)[0] != 200
        wrong = bytearray(base64.urlsafe_b64decode(target_token[4:] + "="))
        wrong[80] ^= 1
        wrong[112:] = hashlib.sha256(b"hww1" + wrong[:112]).digest()[:4]
        wrong_token = "hww1" + base64.urlsafe_b64encode(wrong).rstrip(b"=").decode()
        assert request(gateway_base, "/api/login", token=wrong_token)[0] == 401
        unknown = bytearray(wrong)
        unknown[0] ^= 128
        unknown[112:] = hashlib.sha256(b"hww1" + unknown[:112]).digest()[:4]
        unknown_token = "hww1" + base64.urlsafe_b64encode(unknown).rstrip(b"=").decode()
        assert request(gateway_base, "/api/login", token=unknown_token)[0] != 200
        for item, name in [(target_token, "target"), (target_token2, "target2")]:
            ws = WebSocket(gateway_base).authenticate(login(gateway_base, item))
            ws.output_contains("主机")
            ws.close()
            wait_until(lambda: not lab.tui_pids(lab.path / name), "target TUI reap")
        print("PASS: real HTTPS/WSS relay to two registered exits with HTTP disabled; wrong token rejected", flush=True)

        ws = WebSocket(gateway_base).authenticate(login(gateway_base, target_token))
        try:
            ws.output_contains("主机")
            wait_until(lambda: bool(lab.tui_pids(target)), "real remote TUI PID")
            time.sleep(11)  # Beyond the real unadmitted relay-leg TTL; no injected timer.
            ws.send(b"\x02n")
            time.sleep(0.8)
            ws.send(b"printf 'LONG_%s\\n' 'QA'\r")
            ws.output_contains("LONG_QA")
        finally:
            ws.close()
        wait_until(lambda: not lab.tui_pids(target), "long-lived remote TUI reap")
        assert lab.sessions(target)
        print("PASS: real relay interaction after 11 seconds; closing interface retains work session", flush=True)

        wait_until(lambda: (lab.udp_fds(gateway), lab.udp_fds(target)) == before, "initial UDP FD baseline", 5)
        wait_until(lambda: lab.status(gateway)["relay_counts"][1] == 0, "initial association baseline", 5)
        resources = []
        for index in range(40):
            ws = WebSocket(gateway_base).authenticate(login(gateway_base, target_token))
            ws.output_contains("主机")
            ws.close()
            wait_until(lambda: not lab.tui_pids(target), f"refresh {index + 1} TUI reap")
            wait_until(lambda: (lab.udp_fds(gateway), lab.udp_fds(target)) == before,
                       f"refresh {index + 1} UDP FD baseline", 5)
            wait_until(lambda: lab.status(gateway)["relay_counts"][1] == 0,
                       f"refresh {index + 1} association baseline", 5)
            resources.append({"round": index + 1, "gateway_associations": 0,
                              "gateway_udp_fds": lab.udp_fds(gateway), "target_udp_fds": lab.udp_fds(target),
                              "target_tui_pids": lab.tui_pids(target)})
        (lab.path / "resource-cycles.json").write_text(json.dumps(resources, indent=2))
        print("PASS: 40 relay login/OPEN/close cycles restore target/gateway UDP FD and TUI baselines", flush=True)

        ws = WebSocket(gateway_base).authenticate(login(gateway_base, target_token))
        old_target_token = target_token
        target_token = lab.token(target, rotate=True)
        ws.expect_closed()
        ws.close()
        assert request(gateway_base, "/api/login", token=old_target_token)[0] != 200
        ws = WebSocket(gateway_base).authenticate(login(gateway_base, target_token))
        lab.cli(gateway, "relay", "restart")
        ws.expect_closed()
        ws.close()
        lab.cli(gateway, "webserve", "restart")
        wait_until(lambda: request(gateway_base, "/api/login", token=target_token)[0] == 200,
                   "target re-register after relay restart", 40, 1)
        lab.cli(gateway, "webserve", "restart")
        print("PASS: target rotation and relay restart revoke old management connections", flush=True)

        gateway_token = lab.token(gateway)
        lab.cli(gateway, "relay", "stop")
        assert request(gateway_base)[0] == 200
        assert request(gateway_base, "/api/login", token=target_token)[0] != 200
        local_ws = WebSocket(gateway_base).authenticate(login(gateway_base, gateway_token))
        local_ws.output_contains("主机")
        local_ws.close()
        lab.cli(gateway, "relay", "start")
        wait_until(lambda: request(gateway_base, "/api/login", token=target_token)[0] == 200,
                   "target re-register after relay start", 40, 1)
        lab.cli(gateway, "webserve", "restart")
        print("PASS: relay stop preserves local WebUI and rejects remote targets", flush=True)

        if args.extended:
            ticket = login(gateway_base, target_token)
            time.sleep(31)
            ws = WebSocket(gateway_base)
            ws.send(json.dumps({"type": "auth", "ticket": ticket, "cols": 100, "rows": 30}))
            ws.expect_closed()
            ws.close()
            print("PASS: actual 30-second ticket expiry", flush=True)
        tickets = [login(gateway_base, target_token) for _ in range(8)]
        assert len(set(tickets)) == 8 and request(gateway_base, "/api/login", token=target_token)[0] == 429
        lab.cli(gateway, "webserve", "restart")
        print("PASS: shared login/ticket/session quota rejects ninth reservation", flush=True)
        if args.browser:
            lab.cli(target, "term", "new", "qa-target-one", "-d")
            lab.cli(target2, "term", "new", "qa-target-two", "-d")
            previous_sessions = {entry["name"] for entry in lab.sessions(target)}
            browser_check(gateway_base, target_token, target, lab.path, args.chrome, target_token2, "qa-target-two")
            wait_until(lambda: not lab.tui_pids(target), "browser TUI reap")
            wait_until(lambda: not lab.tui_pids(target2), "second target browser TUI reap")
            after_sessions = {entry["name"] for entry in lab.sessions(target)}
            assert previous_sessions < after_sessions, "browser must create and preserve a real new working session"
            print("PASS: browser close keeps real working session", flush=True)
            browser_failure_checks(lab, gateway_base, target_token, gateway, lab.path, args.chrome)
        parent_loss_check(lab)
        for path in lab.path.rglob("*.log"):
            content = path.read_bytes()
            assert all(token.encode() not in content for token in lab.tokens), "webtoken leaked in " + str(path)
        print("PASS: no complete Web credential in private service logs", flush=True)
    finally:
        lab.close()


if __name__ == "__main__":
    main()
