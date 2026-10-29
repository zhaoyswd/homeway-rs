#!/usr/bin/env python3
"""Private PTY fault injection for TUI liveness and original-session reconnect.

A Unix proxy faults only HELLO client IDs starting with tui-. Real raw terminals,
control LIST calls, and private daemon state keep running. No production service,
network role, desktop clipboard, or external server is used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import select
import shlex
import socket
import struct
import subprocess
import tempfile
import threading
import time


REPO = Path(__file__).resolve().parent.parent
application = runpy.run_path(str(REPO / 'tools/tui-application-clipboard-smoke.py'))
selection = application['selection']
Tui = selection['helpers']['Tui']
Term = application['Term']
cli = application['cli']
entry = selection['entry']
wait_until = application['wait_until']
stop_process = application['stop_process']
attach_target = application['attach_target']


class Link:
    def __init__(self, proxy, client):
        self.proxy = proxy
        self.client = client
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.name = None
        self.tui = False
        self.flags = None
        self.drop_up = False
        self.drop_down = False
        self.held = False
        self.closed = threading.Event()
        self.probes = 0
        self.replies = 0
        self.snapshot_done = 0
        self.surface_frames = 0
        self.reply_pid = None
        self.dropped_probes = 0
        self.thread = threading.Thread(target=self.run, daemon=True)

    def close(self):
        self.closed.set()
        for stream in (self.client, self.server):
            try:
                stream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            stream.close()

    def observe(self, up, op, payload):
        if up and op == 0 and len(payload) >= 6:
            offset = 6 + payload[5]
            tail = payload[offset:]
            if len(tail) >= 4 and tail[0] == 1:
                identifier = tail[4:4 + tail[3]]
                if identifier.startswith(b'tui-'):
                    self.tui = True
                    self.name = payload[6:offset].decode('utf-8')
                    self.flags = payload[4]
                    assert self.flags & 8 and not self.flags & 5, 'reconnect HELLO created/took over a session'
                    assert tail[1] & 1 and not tail[1] & 2, 'TUI is not a surface observer'
                    if self.proxy.hold_new:
                        self.held = self.drop_up = self.drop_down = True
                    with self.proxy.lock:
                        self.proxy.hellos.append(self)
        dropped = self.drop_up if up else self.drop_down
        if self.tui:
            if up and op == 4:
                self.probes += 1
                if dropped:
                    self.dropped_probes += 1
            elif not up and not dropped:
                if op == 4:
                    decoded = json.loads(payload)
                    found = next((item for item in decoded['sessions'] if item['name'] == self.name), None)
                    self.reply_pid = found['pid'] if found else None
                    self.replies += 1
                elif op == 0x0e:
                    self.snapshot_done += 1
                elif op in (0x0d, 0x0f, 7):
                    self.surface_frames += 1
        return not dropped

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
                        length = int.from_bytes(buffer[1:3], 'little')
                        if len(buffer) < 3 + length:
                            break
                        frame = bytes(buffer[:3 + length])
                        del buffer[:3 + length]
                        up = source is self.client
                        if self.observe(up, frame[0], frame[3:]):
                            (self.server if up else self.client).sendall(frame)
        except (BrokenPipeError, ConnectionResetError, OSError, ValueError):
            # Intentional fault/close and connect-and-close liveness probes are normal.
            pass
        except BaseException as error:
            with self.proxy.lock:
                self.proxy.errors.append(repr(error))
        finally:
            self.close()


class Proxy:
    def __init__(self, state):
        self.front = state / 'local-term.sock'
        self.upstream = state / 'reconnect-upstream.sock'
        self.front.rename(self.upstream)
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(self.front))
        self.listener.listen()
        self.listener.settimeout(0.1)
        self.links = []
        self.hellos = []
        self.errors = []
        self.lock = threading.Lock()
        self.stopped = threading.Event()
        self.hold_new = False
        self.thread = threading.Thread(target=self.accept, daemon=True)
        self.thread.start()

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

    def count(self, name):
        with self.lock:
            return sum(link.name == name for link in self.hellos)

    def latest(self, name):
        with self.lock:
            return next((link for link in reversed(self.hellos) if link.name == name), None)

    def restore_new(self):
        self.hold_new = False
        with self.lock:
            held = [link for link in self.links if link.held]
        # A held HELLO was intentionally discarded. Close it; recovery must open
        # a fresh connection rather than replay that failed socket's input.
        for link in held:
            link.close()

    def close(self):
        self.stopped.set()
        self.listener.close()
        self.thread.join(2)
        with self.lock:
            links = list(self.links)
        for link in links:
            link.close()
        for link in links:
            link.thread.join(2)
        assert not self.thread.is_alive() and all(not link.thread.is_alive() for link in links), 'private proxy threads leaked'
        self.front.unlink(missing_ok=True)
        if self.upstream.exists():
            self.upstream.rename(self.front)


def pump(tui, wide, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        tui.drain(0.03)
        wide.drain(0.01)
        assert tui.process.poll() is None, 'TUI exited during private fault'


def ready(tui, wide, proxy, name, after, pid, timeout=35):
    def restored():
        tui.drain(0.03)
        wide.drain(0.01)
        link = proxy.latest(name)
        assert not proxy.errors, repr(proxy.errors)
        return (proxy.count(name) > after and link is not None and not link.closed.is_set()
                and link.replies > 0 and link.snapshot_done > 0 and link.reply_pid == pid
                and tui.terminal_ready(name) and link)
    link = wait_until(restored, 'new same-PID LIST and snapshot on original session', timeout)
    pump(tui, wide, 0.3)
    return link


def assert_observer(binary, state, name, pid):
    current = entry(binary, state, name)
    assert current and current['pid'] == pid, 'reconnect replaced or lost the original PTY PID'
    assert (current['cols'], current['rows']) == (140, 40), 'passive reconnect changed authoritative geometry'
    assert any(client['kind'] == 'host' and client['active'] for client in current['clients']), 'original raw active owner lost'
    apps = [client for client in current['clients'] if client['kind'] == 'app']
    assert apps and all(not client['active'] for client in apps), 'observer reconnect took over activity'


def marker(tui, wide, path, value, expected):
    tui.send("echo " + value + " >> hits; printf 'DONE_%s\\n' " + value + "\r")
    wait_until(lambda: (tui.drain(0.03) or wide.drain(0.01) or
                        (path.exists() and path.read_bytes() == expected)), 'new user input exactly once: ' + value)
    pump(tui, wide, 0.3)
    assert path.read_bytes() == expected, 'new user input was duplicated'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, default=REPO / 'target/debug/homeway')
    args = parser.parse_args()
    binary = args.bin.resolve()
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    print('INFO: binary SHA256=' + binary_hash, flush=True)
    with tempfile.TemporaryDirectory(prefix='homeway-tui-reconnect-', dir='/tmp') as temporary:
        state = Path(temporary)
        (state / 'config.toml').write_text('[serve]\nenabled=false\n[relay]\nenabled=false\n')
        daemon = wide = tui = proxy = None
        with (state / 'daemon.log').open('wb') as log:
            try:
                env = os.environ.copy()
                env.pop('HOMEWAY_TERM', None)
                daemon = subprocess.Popen([str(binary), '--state', str(state)], stdout=log, stderr=log, env=env)
                wait_until(lambda: (state / 'control.sock').exists() and (state / 'local-term.sock').exists(), 'private daemon')
                name = 'reconnect'
                cli(binary, state, 'term', 'new', name, '-d')
                wide = Term(binary, state, name)
                wide.send('cd ' + shlex.quote(str(state)) + "; printf '\\033[2J\\033[HKEEP_%s\\n' SCREEN\r")
                wide.wait('KEEP_SCREEN')
                wide.drain(0.6)
                pid = entry(binary, state, name)['pid']
                proxy = Proxy(state)
                tui = Tui(binary, state)
                attach_target(tui, name)
                first = ready(tui, wide, proxy, name, 0, pid)
                assert_observer(binary, state, name, pid)
                count = proxy.count(name)
                replies = first.replies
                pump(tui, wide, 12)
                assert proxy.count(name) == count, 'healthy silent shell was reconnected'
                assert first.replies >= replies + 2, 'silent shell was not actively probed across two periods'
                assert_observer(binary, state, name, pid)
                print('PASS: silent shell answers repeated same-leg LIST probes without reconnect, input, or observer takeover', flush=True)

                first.close()
                current = ready(tui, wide, proxy, name, count, pid)
                assert 'KEEP_SCREEN' in tui.screen_text(), 'EOF recovery lost the original screen'
                assert_observer(binary, state, name, pid)
                marker(tui, wide, state / 'hits', 'EOF', b'EOF\n')
                assert entry(binary, state, name)['pid'] == pid
                print('PASS: real socket EOF automatically reattaches original PID with observer-only HELLO; fresh input executes once', flush=True)

                # Restore the raw active owner. Its real foreground shell keeps
                # emitting while only the TUI uplink is blackholed: downstream
                # surface traffic cannot masquerade as a LIST round-trip.
                wide.send("i=0; while [ $i -lt 100 ]; do printf 'BURST_%03d\\n' $i; i=$((i+1)); sleep 0.2; done; printf 'BURST_%s\\n' DONE\r")
                wait_until(lambda: (tui.drain() or 'BURST_000' in tui.screen_text()), 'real continuous PTY output')
                assert_observer(binary, state, name, pid)
                count = proxy.count(name)
                current = proxy.latest(name)
                before_output = current.surface_frames
                proxy.hold_new = True
                current.drop_up = True

                def dead_detected():
                    tui.drain(0.03)
                    wide.drain(0.01)
                    return proxy.count(name) > count and proxy.latest(name).held

                wait_until(dead_detected, 'uplink blackhole detected despite continuing surface output', 30)
                assert current.dropped_probes > 0, 'fault did not discard a same-leg LIST probe'
                assert current.surface_frames > before_output + 5, 'downstream was not live during the uplink fault'
                assert current.closed.is_set(), 'old blackholed attach was not cancelled'
                assert 'BURST_' in tui.screen_text(), 'failure cleared the preserved terminal screen'
                # The next attempt is deliberately held, so these are definitely
                # submitted while offline; restoration must not replay them.
                tui.send('echo KEY >> lost\r')
                tui.send(b'\x1b[200~echo PASTE >> lost\n\x1b[201~\r')
                pump(tui, wide, 2)
                assert not (state / 'lost').exists(), 'offline input already reached the original shell'
                held_count = proxy.count(name)
                proxy.restore_new()
                current = ready(tui, wide, proxy, name, held_count, pid)
                assert_observer(binary, state, name, pid)
                wait_until(lambda: (tui.drain(0.03) or wide.drain(0.01) or 'BURST_DONE' in wide.screen_text()), 'original foreground loop completes', 15)
                marker(tui, wide, state / 'hits', 'BLACKHOLE', b'EOF\nBLACKHOLE\n')
                assert not (state / 'lost').exists(), 'offline key/paste input was replayed after recovery'
                assert entry(binary, state, name)['pid'] == pid
                assert not proxy.errors, repr(proxy.errors)
                print('PASS: uplink blackhole times out despite live output, retries original PID, discards offline keys/paste, and accepts fresh input once', flush=True)

                proxy.hold_new = True
                count = proxy.count(name)
                proxy.latest(name).close()
                wait_until(lambda: (tui.drain(0.03) or proxy.count(name) > count), 'new retry before quitting', 15)
                tui.send(b'\x02q')
                tui.wait_exit()
                assert tui.process.returncode == 0
                tui.check_restored()
                count = proxy.count(name)
                proxy.restore_new()
                wide.drain(1)
                assert proxy.count(name) == count, 'retry continued after TUI exit'
                assert entry(binary, state, name)['pid'] == pid
                print('PASS: quitting during a real retry cancels recovery, restores TTY, and preserves the original live PTY', flush=True)
            except BaseException:
                if tui is not None:
                    tui.drain()
                    print('FAILURE SCREEN:\n' + tui.screen_text(), flush=True)
                if daemon is not None and daemon.poll() is None:
                    print('FAILURE SESSIONS: ' + cli(binary, state, 'term', 'list', '--json').decode(errors='replace'), flush=True)
                if proxy is not None:
                    print('FAILURE PROXY: ' + repr([(link.name, link.flags, link.probes, link.replies, link.surface_frames,
                                                    link.drop_up, link.drop_down, link.closed.is_set()) for link in proxy.hellos]), flush=True)
                raise
            finally:
                errors = []
                for terminal in (tui, wide):
                    if terminal is not None:
                        try:
                            terminal.close()
                        except BaseException as error:
                            errors.append(error)
                if proxy is not None:
                    try:
                        proxy.close()
                    except BaseException as error:
                        errors.append(error)
                stop_process(daemon)
                assert daemon is None or daemon.poll() is not None
                assert hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash, 'binary changed during smoke'
                if errors:
                    raise AssertionError('private cleanup failed: ' + repr(errors))
    print('PASS: all private PTYs/proxy/daemon reclaimed; binary SHA256 unchanged', flush=True)


if __name__ == '__main__':
    main()
