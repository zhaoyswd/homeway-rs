#!/usr/bin/env python3
"""Private real-PTY metadata subscription, reconnect, and legacy fallback smoke.

A frame proxy counts first-frame LIST/SUBSCRIBE independently from same-surface
liveness LIST, and can mask the new feature plus strip LIST's new JSON key.
Only owned /tmp state, PTYs, proxy threads and a network-disabled daemon are used.
"""

import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import runpy
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parent.parent
old = runpy.run_path(str(REPO / "tools/tui-auto-reconnect-smoke.py"))
Tui, Term, cli, wait_until, stop_process, attach_target = (
    old[name] for name in ("Tui", "Term", "cli", "wait_until", "stop_process", "attach_target")
)
sidebar_width = old["selection"]["helpers"]["sidebar_width"]


class Link(old["Link"]):
    def __init__(self, proxy, client):
        super().__init__(proxy, client)
        self.first_op = None
        self.legacy = proxy.legacy
        self.watch_held = False
        self.complete = 0
        self.snapshot = None
        self.metadata = bytearray()

    def transform(self, up, op, payload):
        if up and self.first_op is None:
            with self.proxy.lock:
                self.first_op = op
                self.watch_held = op == 0x18 and self.proxy.hold_watch
        if not up and op == 0x0C and self.legacy:
            payload = payload[:1] + (int.from_bytes(payload[1:5], "little") & ~256).to_bytes(4, "little")
        elif not up and op == 4 and self.legacy:
            value = json.loads(payload)
            value.pop("metadataRevision", None)
            payload = json.dumps(value, separators=(",", ":")).encode()
        elif not up and op == 0x19:
            self.metadata.extend(payload[9:])
            assert len(self.metadata) <= 1 << 20
            if payload[8] == 0:
                self.snapshot = json.loads(self.metadata)
                self.metadata.clear()
                self.complete += 1
        if self.watch_held:
            return None
        return payload

    def run(self):
        try:
            self.server.connect(str(self.proxy.upstream))
            for stream in (self.client, self.server):
                stream.settimeout(2)
            buffers = {self.client: bytearray(), self.server: bytearray()}
            while not self.closed.is_set() and not self.proxy.stopped.is_set():
                readable, _, _ = select.select([self.client, self.server], [], [], 0.1)
                for source in readable:
                    chunk = source.recv(65536)
                    if not chunk:
                        return
                    buffer = buffers[source]
                    buffer.extend(chunk)
                    while len(buffer) >= 3:
                        length = int.from_bytes(buffer[1:3], "little")
                        if len(buffer) < length + 3:
                            break
                        frame = bytes(buffer[:length + 3])
                        del buffer[:length + 3]
                        up, op = source is self.client, frame[0]
                        payload = self.transform(up, op, frame[3:])
                        if payload is not None and self.observe(up, op, payload):
                            target = self.server if up else self.client
                            target.sendall(bytes([op]) + len(payload).to_bytes(2, "little") + payload)
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        except BaseException as error:
            with self.proxy.lock:
                self.proxy.errors.append(repr(error))
        finally:
            self.close()


class Proxy(old["Proxy"]):
    def __init__(self, state):
        self.legacy = False
        self.hold_watch = False
        super().__init__(state)

    def accept(self):
        while not self.stopped.is_set():
            try:
                client, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            link = Link(self, client)
            with self.lock:
                self.links.append(link)
            link.thread.start()

    def query_count(self):
        with self.lock:
            return sum(link.first_op == 4 for link in self.links)

    def watches(self):
        with self.lock:
            return [link for link in self.links if link.first_op == 0x18]

    def latest_watch(self):
        links = self.watches()
        return links[-1] if links else None


def sidebar_rows(tui):
    tui.drain(0.02)
    width = sidebar_width(tui.screen.columns)
    return ["".join(tui.screen.buffer[y][x].data or " " for x in range(width))
            for y in range(1, tui.screen.lines - 1)]


def sidebar_has(tui, title):
    return any(title in row for row in sidebar_rows(tui))


def sidebar_state(tui, title, state):
    rows = sidebar_rows(tui)
    return any(title in row and index + 1 < len(rows) and state in rows[index + 1]
               for index, row in enumerate(rows))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=REPO / "target/debug/homeway")
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    print("INFO: binary SHA256=" + digest, flush=True)
    with tempfile.TemporaryDirectory(prefix="homeway-watch-smoke-", dir="/tmp") as directory:
        state = Path(directory)
        (state / "config.toml").write_text("[serve]\nenabled=false\n[relay]\nenabled=false\n")
        env = os.environ.copy()
        for key in ("HOMEWAY_TERM", "HOMEWAY_TERM_SHELL", "HOMEWAY_TERM_VT", "HOMEWAY_TERM_DETECT"):
            env.pop(key, None)
        with (state / "daemon.log").open("wb") as log, ExitStack() as cleanup:
            daemon = subprocess.Popen([str(binary), "--state", str(state)], stdout=log, stderr=log, env=env)
            cleanup.callback(stop_process, daemon)
            wait_until(lambda: daemon.poll() is None and (state / "local-term.sock").exists(), "private daemon")
            cli(binary, state, "term", "new", "active", "-d")
            active = Term(binary, state, "active")
            cleanup.callback(active.close)
            active.drain(0.3)
            original = json.loads(cli(binary, state, "term", "list", "--json"))["sessions"][0]
            proxy = Proxy(state)
            cleanup.callback(proxy.close)
            tui = Tui(binary, state)
            cleanup.callback(tui.close)
            try:
                attach_target(tui, "active")
                wait_until(lambda: proxy.latest_watch() and proxy.latest_watch().complete, "initial metadata snapshot")
                baseline = proxy.query_count()
                cli(binary, state, "term", "new", "background", "-d")
                wait_until(lambda: sidebar_has(tui, "background"), "background create push", 2)
                background = Term(binary, state, "background")
                cleanup.callback(background.close)
                background.drain(0.3)
                for index in range(4):
                    title = f"other-{index}"
                    started = time.monotonic()
                    background.send("printf '\\033]0;" + title + "\\007'\r")
                    wait_until(lambda: sidebar_has(tui, title), "background title push " + title, 1.5)
                    print(f"INFO: background {title} {time.monotonic() - started:.3f}s", flush=True)
                wait_until(lambda: sidebar_state(tui, "other-3", "shell"), "sampled background agent label", 5)
                background.send("printf '\\033]21337;status=blocked\\007'\r")
                wait_until(lambda: sidebar_state(tui, "other-3", "blocking"), "background blocking state push", 4)
                background.send("printf '\\033]21337;status=idle\\007\\033]7;file://localhost/q7\\007'\r")
                wait_until(lambda: sidebar_state(tui, "other-3", "idle") and sidebar_state(tui, "other-3", "q7"),
                           "background idle and cwd push", 4)
                assert proxy.query_count() == baseline, "healthy subscription kept polling first-frame LIST"
                assert all(link.name == "active" for link in proxy.hellos), "TUI attached a background PTY"
                assert not proxy.errors, proxy.errors
                print(f"INFO: healthy first-frame LIST {baseline}->{proxy.query_count()}, subscriptions={len(proxy.watches())}, TUI HELLO={len(proxy.hellos)}", flush=True)
                print("PASS: background create/title/state/cwd push without LIST polling or background TUI HELLO", flush=True)

                proxy.hold_watch = True
                before = len(proxy.watches())
                surface_count = len(proxy.hellos)
                proxy.latest_watch().close()
                wait_until(lambda: len(proxy.watches()) > before and proxy.latest_watch().watch_held,
                           "held subscription retry", 8)
                active.send("printf '\\033]0;gap-title\\007'\r")
                cli(binary, state, "term", "new", "gap-new", "-d")
                cli(binary, state, "term", "delete", "background")
                wait_until(lambda: (tui.drain(0.02) or "gap-title" in tui.screen_lines()[0]), "surface remains live during watch fault", 3)
                proxy.hold_watch = False
                before = len(proxy.watches())
                for link in proxy.watches():
                    if link.watch_held:
                        link.close()
                def reconciled():
                    link = proxy.latest_watch()
                    if not link or not link.complete or link.watch_held or len(proxy.watches()) <= before:
                        return False
                    values = {item["name"]: item for item in link.snapshot["sessions"]}
                    return (set(values) == {"active", "gap-new"} and values["active"]["title"] == "gap-title"
                            and sidebar_has(tui, "gap-title") and sidebar_has(tui, "gap-new")
                            and not sidebar_has(tui, "other-3"))
                wait_until(reconciled, "new watch full snapshot includes missed title/create/exit", 8)
                assert len(proxy.hellos) == surface_count, "metadata retry reattached the surface leg"
                print("PASS: dropping only metadata stream reconnects and fully reconciles missed title/create/exit", flush=True)
                tui.send(b"\x02q")
                tui.wait_exit()
                tui.check_restored()
                assert tui.process.returncode == 0

                proxy.legacy = True
                old_watch_count = len(proxy.watches())
                legacy = Tui(binary, state)
                cleanup.callback(legacy.close)
                wait_until(lambda: sidebar_has(legacy, "gap-title"), "legacy LIST baseline", 8)
                baseline = proxy.query_count()
                active.send("printf '\\033]0;legacy-title\\007'\r")
                wait_until(lambda: sidebar_has(legacy, "legacy-title"), "legacy title refresh", 2)
                legacy.drain(1.2)
                assert proxy.query_count() >= baseline + 2, "missing feature did not restore periodic LIST"
                assert len(proxy.watches()) == old_watch_count, "legacy capability received an unknown SUBSCRIBE"
                print(f"INFO: legacy first-frame LIST {baseline}->{proxy.query_count()}, new SUBSCRIBE=0", flush=True)
                legacy.send(b"\x02q")
                legacy.wait_exit()
                legacy.check_restored()
                assert legacy.process.returncode == 0
                assert daemon.poll() is None
                remaining = {item["name"]: item for item in json.loads(cli(binary, state, "term", "list", "--json"))["sessions"]}
                assert set(remaining) == {"active", "gap-new"}
                assert remaining["active"]["pid"] == original["pid"], "active PTY identity changed"
                assert not proxy.errors, proxy.errors
                print("PASS: masked legacy greeting and old LIST shape use periodic LIST without SUBSCRIBE", flush=True)
                print("PASS: both TUI exits restore TTY while active and gap PTYs remain alive", flush=True)
            except BaseException:
                tui.drain()
                print("FAILURE SCREEN:\n" + tui.screen_text(), flush=True)
                print("FAILURE PROXY: " + repr([(link.first_op, link.name, link.complete, link.watch_held,
                                                link.closed.is_set()) for link in proxy.links]), flush=True)
                raise
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest
    print("PASS: all owned PTYs/proxy/daemon cleaned; binary SHA256 unchanged", flush=True)


if __name__ == "__main__":
    main()
