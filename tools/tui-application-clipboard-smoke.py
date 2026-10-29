#!/usr/bin/env python3
"""Private PTY application OSC52 -> surface CLIPBOARD -> TUI -> private sink.

Native helpers are mocks; terminal OSC52 is captured in a PTY. No real clipboard,
default state, network role, or production process is used.
"""
import argparse
import base64
import hashlib
import os
from pathlib import Path
import re
import runpy
import socket
import struct
import subprocess
import tempfile
import time


REPO = Path(__file__).resolve().parent.parent
selection = runpy.run_path(str(REPO / 'tools/tui-selection-smoke.py'))
Term = selection['Term']
cli = selection['cli']
wait_until = selection['wait_until']
stop_process = selection['stop_process']
private_tui = selection['private_tui']
clipboard_count = selection['clipboard_count']
assert_passive = selection['assert_passive']


class SurfaceProbe:
    """A second passive surface leg checks the server's actual wire payload."""

    def __init__(self, state, name):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.socket.settimeout(10)
            self.socket.connect(str(state / 'local-term.sock'))
            op, greeting = self.frame()
            assert op == 0x0c and len(greeting) == 5 and greeting[0] == 1
            assert int.from_bytes(greeting[1:], 'little') & 0xa0 == 0xa0
            name = name.encode()
            client = b'clipboard-wire-probe'
            # HELLO observer, surface + key-alt-escape + version; no input/resize.
            payload = struct.pack('<HHBB', 66, 27, 8, len(name)) + name
            payload += bytes([1, 0x89, 1, len(client)]) + client
            self.socket.sendall(bytes([0]) + struct.pack('<H', len(payload)) + payload)
            op, _ = self.frame()
            assert op == 0x09, 'probe did not receive ATTACHED'
        except BaseException:
            self.socket.close()
            raise

    def read(self, length):
        data = bytearray()
        while len(data) < length:
            chunk = self.socket.recv(length - len(data))
            assert chunk, 'surface probe closed'
            data.extend(chunk)
        return bytes(data)

    def frame(self):
        header = self.read(3)
        return header[0], self.read(int.from_bytes(header[1:], 'little'))

    def clipboard(self, expected):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            self.socket.settimeout(max(0.001, deadline - time.monotonic()))
            op, payload = self.frame()
            if op == 0x13:
                assert payload == b'\0' + struct.pack('<H', len(expected)) + expected, 'server CLIPBOARD bytes differ'
                return
        raise AssertionError('server did not forward application CLIPBOARD')

    def close(self):
        self.socket.close()


def emit(wide, expected, label):
    encoded = base64.b64encode(expected).decode('ascii')
    # The real shell writes OSC52. Its own echoed command has escaped controls.
    wide.send("printf '\\033]52;c;%s\\007\\r\\nAPP_DONE_%s\\r\\n' '" + encoded + "' '" + label + "'\r")


def attach_target(tui, name):
    # Panel headings appear before the asynchronous LIST reply. Enter on that
    # empty list creates a shell, so wait for this exact sidebar item and click.
    def target_row():
        tui.drain()
        left = selection['helpers']['sidebar_width'](tui.screen.columns)
        for row in range(1, tui.screen.lines - 2):
            text = ''.join(tui.screen.buffer[row][column].data or ' ' for column in range(left))
            if '● ' + name in text:
                return row
        return None
    row = wait_until(target_row, 'named session sidebar item ' + name)
    tui.send(f'\x1b[<0;5;{row + 1}M\x1b[<0;5;{row + 1}m')
    tui.wait_terminal(name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, default=REPO / 'target/debug/homeway')
    args = parser.parse_args()
    binary = args.bin.resolve()
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    print('INFO: binary SHA256=' + binary_hash, flush=True)
    with tempfile.TemporaryDirectory(prefix='homeway-tui-app-clipboard-', dir='/tmp') as temporary:
        state = Path(temporary)
        (state / 'config.toml').write_text('[serve]\nenabled=false\n[relay]\nenabled=false\n')
        directory, copy_path, count_path = selection['mock_clipboard'](state)
        daemon = wide = tui = probe = None
        with (state / 'daemon.log').open('wb') as log:
            try:
                env = os.environ.copy()
                env.pop('HOMEWAY_TERM', None)
                daemon = subprocess.Popen([str(binary), '--state', str(state)], stdout=log, stderr=log, env=env)
                wait_until(lambda: (state / 'control.sock').exists() and (state / 'local-term.sock').exists(), 'private daemon')
                name = 'application-copy'
                cli(binary, state, 'term', 'new', name, '-d')
                wide = Term(binary, state, name)
                wide.send("printf 'APP_%s\\r\\n' READY\r")
                wide.wait('APP_READY')
                wide.drain(0.6)
                probe = SurfaceProbe(state, name)
                tui = private_tui(binary, state, directory, copy_path, count_path)
                attach_target(tui, name)
                baseline = assert_passive(binary, state, name)
                expected = 'APP_COPY:中é\nsecond\t\0\x1bcontrol'.encode()
                for number in (1, 2):
                    emit(wide, expected, str(number))
                    probe.clipboard(expected)
                    wait_until(lambda: (tui.drain() or clipboard_count(count_path) == number), 'one exact application helper write')
                    assert copy_path.read_bytes() == expected, 'private helper bytes differ'
                    tui.drain(0.2)
                    assert clipboard_count(count_path) == number, 'one OSC52 wrote clipboard more than once'
                    assert_passive(binary, state, name, baseline)
                print('PASS: real PTY OSC52 reaches exact surface WRITE and mock helper twice for identical UTF-8/control text; observer stays passive', flush=True)

                emit(wide, b'', 'empty')
                probe.clipboard(b'')
                wait_until(lambda: (tui.drain() or clipboard_count(count_path) == 3), 'empty application clipboard write')
                assert copy_path.read_bytes() == b'', 'empty WRITE did not clear the private sink'
                assert_passive(binary, state, name, baseline)
                print('PASS: empty application WRITE clears only the private clipboard sink', flush=True)
                tui.send(b'\x02q')
                tui.wait_exit()
                assert tui.process.returncode == 0
                tui.check_restored()
                tui.close()
                tui = None

                # Snapshot/reconnect must not replay any prior application's copy.
                tui = private_tui(binary, state, directory, copy_path, count_path,
                                  term_session_id='tailcat-application-clipboard-smoke')
                attach_target(tui, name)
                baseline = assert_passive(binary, state, name)
                tui.drain(0.3)
                assert clipboard_count(count_path) == 3, 'reconnect replayed an application clipboard write'
                assert not re.findall(rb'\x1b]52;c;([A-Za-z0-9+/=]*)\x07', bytes(tui.output)), 'reconnect replayed a terminal clipboard write'
                expected = 'TERM_APPLICATION_COPY:中é\nline2'.encode()
                output_start = len(tui.output)
                emit(wide, expected, 'terminal')
                probe.clipboard(expected)
                encoded = base64.b64encode(expected)

                def terminal_request():
                    tui.drain()
                    return re.findall(rb'\x1b]52;c;([A-Za-z0-9+/=]*)\x07', bytes(tui.output[output_start:]))

                wait_until(terminal_request, 'captured application terminal OSC52')
                tui.drain(0.3)
                assert terminal_request() == [encoded], 'terminal application clipboard bytes/count differ'
                assert clipboard_count(count_path) == 3 and copy_path.read_bytes() == b'', 'terminal route invoked a native helper'
                assert '已复制' not in tui.screen_text(), 'terminal request claimed confirmed system copy'
                assert_passive(binary, state, name, baseline)
                print('PASS: reconnect does not replay writes; homeway term route emits one exact application OSC52 and bypasses native helpers', flush=True)
                tui.send(b'\x02q')
                tui.wait_exit()
                assert tui.process.returncode == 0
                tui.check_restored()
            except BaseException:
                if tui is not None:
                    tui.drain()
                    print('FAILURE SCREEN:\n' + tui.screen_text(), flush=True)
                print('FAILURE SESSIONS: ' + cli(binary, state, 'term', 'list', '--json').decode(errors='replace'), flush=True)
                raise
            finally:
                errors = []
                if probe is not None:
                    probe.close()
                for terminal in (tui, wide):
                    if terminal is not None:
                        try:
                            terminal.close()
                        except BaseException as error:
                            errors.append(error)
                stop_process(daemon)
                assert daemon is None or daemon.poll() is not None
                assert hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash, 'binary changed during smoke'
                if errors:
                    raise AssertionError('private PTY cleanup failed: ' + repr(errors))
    print('PASS: all private PTYs/daemon reclaimed, TTY restored, binary SHA256 unchanged', flush=True)


if __name__ == '__main__':
    main()
