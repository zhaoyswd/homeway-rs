#!/usr/bin/env python3
"""Real private PTYs: local drag copy, frozen highlights, history and observer size.

Build first, then run with --bin target/debug/homeway. Clipboard helpers are
private mocks; no real clipboard, default state or network role is touched.
Uses the pyte/Pillow helpers already used by tui-smoke.py.
"""
import argparse
import base64
import hashlib
import os
from pathlib import Path
import re
import runpy
import signal
import sys
import tempfile
import unicodedata


REPO = Path(__file__).resolve().parent.parent
helpers = runpy.run_path(str(REPO / 'tools/tui-smoke.py'))
observer = runpy.run_path(str(REPO / 'tools/tui-observer-smoke.py'))
Tui = helpers['Tui']
Term = observer['Term']
cli = observer['cli']
entry = observer['entry']
wait_until = helpers['wait_until']
stop_process = helpers['stop_process']
snapshot = helpers['snapshot']
wcwidth = helpers['wcwidth']


def grid_row(tui, row):
    """Return NFC text and its physical cell boundaries, including wide glyphs."""
    pieces, starts, ends = [], [], []
    left = helpers['sidebar_width'](tui.screen.columns)
    column = left
    while column < tui.screen.columns - 1:
        data = tui.screen.buffer[row][column].data or ' '
        width = max(1, wcwidth(data[0]))
        data = unicodedata.normalize('NFC', data)
        pieces.append(data)
        starts.extend([column] * len(data))
        ends.extend([column + width] * len(data))
        column += width
    return ''.join(pieces), starts, ends


def locate(tui, needle):
    needle = unicodedata.normalize('NFC', needle)
    for row in range(1, tui.screen.lines - 2):
        text, starts, ends = grid_row(tui, row)
        index = text.find(needle)
        if index >= 0:
            return (starts[index], row), (ends[index + len(needle) - 1] - 1, row)
    return None


def visible(tui, needle):
    def ready():
        tui.drain()
        assert tui.process.poll() is None, 'TUI exited while waiting for visible material'
        return locate(tui, needle)
    return wait_until(ready, 'visible ' + repr(needle))


def mouse(tui, point, suffix, code=0):
    x, y = point
    tui.send(f'\x1b[<{code};{x + 1};{y + 1}{suffix}')


def begin_drag(tui, start, end, color):
    mouse(tui, start, 'M')
    mouse(tui, end, 'M', code=32)
    def highlighted():
        tui.drain()
        return tui.screen.buffer[start[1]][start[0]].bg == color
    wait_until(highlighted, 'physical selected cell highlight')
    assert '拖选中，松开复制' in tui.screen_lines()[-1], 'selection footer absent'


def clipboard_count(path):
    return int(path.read_text()) if path.exists() else 0


def release_copy(tui, end, count_path, copy_path, previous, expected):
    mouse(tui, end, 'm')
    def copied():
        tui.drain()
        return clipboard_count(count_path) == previous + 1
    wait_until(copied, 'one private clipboard write after release')
    assert copy_path.read_bytes() == expected.encode(), 'clipboard bytes differ from the selected visible text'
    tui.wait('已复制到系统剪贴板')
    assert tui.process.poll() is None


def assert_passive(binary, state, name, baseline=None):
    current = entry(binary, state, name)
    assert current is not None and (current['cols'], current['rows']) == (140, 40), 'selection changed authoritative geometry'
    assert any(client['kind'] == 'host' and client['active'] for client in current['clients']), 'raw active leg lost ownership'
    assert all(not client['active'] for client in current['clients'] if client['kind'] == 'app'), 'local selection activated the observer'
    identity = current['pid'], tuple(sorted((client['kind'], client['sinceMs'], client['active']) for client in current['clients']))
    if baseline is not None:
        assert identity == baseline, 'local selection changed session or leg ownership'
    return identity


def mock_clipboard(root):
    directory = root / 'mock-bin'
    directory.mkdir(mode=0o700)
    copy_path = root / 'clipboard.bytes'
    count_path = root / 'clipboard.count'
    program = ('#!' + sys.executable + '\n'
               'import os, pathlib, sys\n'
               'target=pathlib.Path(os.environ["HOMEWAY_SELECTION_CAPTURE"])\n'
               'count=pathlib.Path(os.environ["HOMEWAY_SELECTION_COUNT"])\n'
               'temporary=target.with_name(target.name+".tmp")\n'
               'temporary.write_bytes(sys.stdin.buffer.read())\n'
               'os.replace(temporary,target)\n'
               'number=(int(count.read_text()) if count.exists() else 0)+1\n'
               'temporary=count.with_name(count.name+".tmp")\n'
               'temporary.write_text(str(number))\n'
               'os.replace(temporary,count)\n')
    for name in ('pbcopy', 'wl-copy', 'xclip', 'xsel'):
        path = directory / name
        path.write_text(program)
        path.chmod(0o700)
    return directory, copy_path, count_path


def private_tui(binary, state, directory, copy_path, count_path, term_session_id=None):
    # Only the new TUI child receives these mocks/markers. Restore the harness
    # environment immediately; the shell and any real clipboard stay untouched.
    keys = ('PATH', 'HOMEWAY_SELECTION_CAPTURE', 'HOMEWAY_SELECTION_COUNT',
            'SSH_CONNECTION', 'SSH_TTY', 'SSH_CLIENT', 'TERM_SESSION_ID', 'TERM_PROGRAM')
    old = {key: os.environ.get(key) for key in keys}
    try:
        os.environ['PATH'] = str(directory) + os.pathsep + os.environ.get('PATH', '')
        os.environ['HOMEWAY_SELECTION_CAPTURE'] = str(copy_path)
        os.environ['HOMEWAY_SELECTION_COUNT'] = str(count_path)
        for key in ('SSH_CONNECTION', 'SSH_TTY', 'SSH_CLIENT', 'TERM_SESSION_ID'):
            os.environ.pop(key, None)
        if term_session_id is not None:
            os.environ['TERM_SESSION_ID'] = term_session_id
            os.environ['TERM_PROGRAM'] = 'Tailcat'
        return Tui(binary, state)
    finally:
        for key, value in old.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def history_pair(tui):
    tui.drain()
    found = []
    for row in range(1, tui.screen.lines - 2):
        text, starts, ends = grid_row(tui, row)
        match = re.search(r'HIST_(\d{3})_中é', text)
        if match:
            found.append((int(match.group(1)), (starts[match.start()], row), (ends[match.end() - 1] - 1, row)))
    for first, second in zip(found, found[1:]):
        if second[0] == first[0] + 1 and second[1][1] == first[1][1] + 1:
            return first, second
    return None


def main():
    import subprocess
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, default=REPO / 'target/debug/homeway')
    parser.add_argument('--snapshot-dir', type=Path)
    args = parser.parse_args()
    binary = args.bin.resolve()
    before_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    print('INFO: binary SHA256=' + before_hash, flush=True)
    with tempfile.TemporaryDirectory(prefix='homeway-tui-selection-', dir='/tmp') as temporary:
        state = Path(temporary)
        (state / 'config.toml').write_text('[serve]\nenabled=false\n[relay]\nenabled=false\n')
        directory, copy_path, count_path = mock_clipboard(state)
        daemon = wide = tui = remote = None
        with (state / 'daemon.log').open('wb') as log:
            try:
                env = os.environ.copy()
                env.pop('HOMEWAY_TERM', None)
                daemon = subprocess.Popen([str(binary), '--state', str(state)], stdout=log, stderr=log, env=env)
                wait_until(lambda: (state / 'control.sock').exists() and (state / 'local-term.sock').exists(), 'private daemon')
                cli(binary, state, 'term', 'new', 'selection', '-d')
                wide = Term(binary, state, 'selection')
                long_line = 'SEL_PREFIX_' + 'A' * 90 + '_PANE_HIDDEN_TAIL'
                # The physical TAB starts at column11 and ends at column16.
                wide.send("printf '\\033[2J\\033[H%s\\r\\nSEL_CJK:中é\\tZ_END\\r\\nSEL_READY\\r\\n' '" + long_line + "'\r")
                wide.wait('SEL_READY')
                wide.drain(0.6)
                tui = private_tui(binary, state, directory, copy_path, count_path)
                tui.wait('会话')
                tui.send(b'\x02s\x1b[B\r')
                tui.wait('已接入')
                start, end = visible(tui, 'SEL_CJK:中é     Z_END')
                baseline = assert_passive(binary, state, 'selection')
                mouse(tui, start, 'M')
                mouse(tui, start, 'm')
                tui.drain(0.3)
                assert clipboard_count(count_path) == 0, 'ordinary click unexpectedly copied'
                assert_passive(binary, state, 'selection', baseline)
                print('PASS: ordinary click produces no clipboard write and does not activate the observer', flush=True)

                begin_drag(tui, start, end, '6db9ff')
                assert_passive(binary, state, 'selection', baseline)
                snapshot(tui, args.snapshot_dir, 'selection-dark-cjk-tab')
                release_copy(tui, end, count_path, copy_path, 0, 'SEL_CJK:中é     Z_END')
                assert_passive(binary, state, 'selection', baseline)
                print('PASS: dark physical highlight and release copy preserve CJK, combining text and displayed TAB spaces', flush=True)

                line_start, _ = visible(tui, 'SEL_PREFIX_')
                left = helpers['sidebar_width'](tui.screen.columns)
                line_start = (left, line_start[1])
                line_end = (tui.screen.columns - 2, line_start[1])
                begin_drag(tui, line_start, line_end, '6db9ff')
                snapshot(tui, args.snapshot_dir, 'selection-dark-cropped-wide')
                width = tui.screen.columns - left - 1
                release_copy(tui, line_end, count_path, copy_path, 1, long_line[:width])
                assert b'PANE_HIDDEN_TAIL' not in copy_path.read_bytes()
                assert_passive(binary, state, 'selection', baseline)
                print('PASS: copied pane excludes the offscreen wide-row tail; raw 140x40 owner remains active', flush=True)

                wide.send("i=0; while [ $i -lt 120 ]; do printf 'HIST_%03d_中é\\r\\n' \"$i\"; i=$((i+1)); done; printf 'HIST_READY\\r\\n'\r")
                wide.wait('HIST_READY')
                visible(tui, 'HIST_119_中é')
                tui.send(b'\x1b[5;2~')  # Shift+PageUp: use the existing FETCH history path.
                wait_until(lambda: (tui.drain() or '↑' in tui.screen_lines()[-1]), 'history anchor')
                first, second = wait_until(lambda: history_pair(tui), 'two loaded adjacent history rows')
                tui.background = 'rgb:ffff/ffff/ffff'
                tui.send(b'\x1b[I')
                wait_until(lambda: (tui.drain() or tui.screen.buffer[tui.screen.lines - 1][0].bg == 'f8fafd'), 'automatic light theme')
                # Re-read the physical positions after the terminal theme query.
                first, second = wait_until(lambda: history_pair(tui), 'stable loaded history rows')
                begin_drag(tui, second[2], first[1], '1866ad')
                snapshot(tui, args.snapshot_dir, 'selection-light-history-reverse')
                expected = f'HIST_{first[0]:03d}_中é\nHIST_{second[0]:03d}_中é'
                release_copy(tui, first[1], count_path, copy_path, 2, expected)
                assert_passive(binary, state, 'selection', baseline)
                print('PASS: light history reverse drag copies two exact visible lines without resizing or activating', flush=True)

                tui.send(b'\x02q')
                tui.wait_exit()
                assert tui.process.returncode == 0
                tui.check_restored()
                assert entry(binary, state, 'selection')['pid'] == baseline[0]
                print('PASS: detach restores the TTY and preserves the same raw-attached shell', flush=True)

                # A homeway term PTY filters SSH variables and sets tailcat-*.
                # Capture its OSC52 request in our private PTY, never a desktop terminal.
                remote = private_tui(binary, state, directory, copy_path, count_path,
                                     term_session_id='tailcat-selection-smoke')
                remote.wait('会话')
                remote.send(b'\x02s\x1b[B\r')
                remote.wait('已接入')
                expected = 'TERM_COPY:中é'
                wide.send("printf 'TERM_COPY:中é\\r\\n'\r")
                remote_start, remote_end = visible(remote, 'TERM_COPY:中é')
                remote_baseline = assert_passive(binary, state, 'selection')
                native_count = clipboard_count(count_path)
                native_bytes = copy_path.read_bytes()
                begin_drag(remote, remote_start, remote_end, '6db9ff')
                output_start = len(remote.output)
                mouse(remote, remote_end, 'm')
                remote.wait('已提交终端剪贴板请求')
                remote.drain(0.3)
                requests = re.findall(rb'\x1b]52;c;([A-Za-z0-9+/=]*)\x07',
                                      bytes(remote.output[output_start:]))
                assert requests == [base64.b64encode(expected.encode())], 'terminal clipboard request payload differs'
                assert clipboard_count(count_path) == native_count and copy_path.read_bytes() == native_bytes, 'homeway term route called a native clipboard helper'
                assert '已复制到系统剪贴板' not in remote.screen_text(), 'terminal request falsely claims confirmed system copy'
                assert_passive(binary, state, 'selection', remote_baseline)
                remote.send(b'\x02q')
                remote.wait_exit()
                assert remote.process.returncode == 0
                remote.check_restored()
                assert entry(binary, state, 'selection')['pid'] == baseline[0]
                print('PASS: homeway term marker emits one exact OSC52 request, bypasses native clipboard and preserves passive ownership', flush=True)
            finally:
                errors = []
                for terminal in (remote, tui, wide):
                    if terminal is not None:
                        try:
                            terminal.close()
                        except BaseException as error:
                            errors.append(error)
                stop_process(daemon)
                assert daemon is None or daemon.poll() is not None
                assert hashlib.sha256(binary.read_bytes()).hexdigest() == before_hash, 'binary changed during verification'
                if errors:
                    raise AssertionError('private PTY cleanup failed: ' + repr(errors))
    print('PASS: all private PTYs/daemon reclaimed; binary SHA256 unchanged', flush=True)


if __name__ == '__main__':
    main()
