#!/usr/bin/env python3
"""Exercise the real TUI in private PTYs; never connects to the default state.

Build first: cargo build -p homeway-cli
Run: python3 tools/tui-smoke.py [--bin target/debug/homeway]
Test dependency: pyte (python3 -m pip install pyte).
Only processes spawned by this script are stopped. No clipboard action is used.
"""

import argparse
import ast
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

try:
    import pyte
    from wcwidth import wcwidth
except ImportError as error:
    raise SystemExit("tui smoke needs pyte: python3 -m pip install pyte") from error


TIMEOUT = 15
ANSI = re.compile(rb"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\)|[@-_])")
# Keep the controlling-terminal session leader alive until after termios is read.
# macOS revokes the slave when that leader exits, making post-exit tcgetattr fail.
LAUNCHER = """
import pathlib, subprocess, sys, termios
attributes, pidfile = map(pathlib.Path, sys.argv[1:3])
child = subprocess.Popen(sys.argv[3:])
pidfile.write_text(str(child.pid))
result = child.wait()
attributes.write_text(repr(termios.tcgetattr(0)))
sys.exit(result)
"""


def wait_until(predicate, description, timeout=TIMEOUT):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.025)
    raise AssertionError(f"timed out: {description}")


def stop_process(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=3)


def sidebar_width(columns):
    return min(max(columns * 3 // 10, 18), 34)


class Tui:
    def __init__(self, binary, state, background="rgb:0000/0000/0000"):
        self.master, self.slave = pty.openpty()
        self.before = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self.background = background
        self._probe_buffer = b""
        self._stub_diagnostics = set()
        self.screen = pyte.Screen(100, 30)
        self.stream = pyte.ByteStream(self.screen)
        self.attributes = state / f"smoke-termios-{self.master}"
        pidfile = state / f"smoke-tui-pid-{self.master}"
        self.set_size(100, 30)
        env = os.environ.copy()
        env["TERM"] = "xterm-256color"
        env["COLORTERM"] = "truecolor"
        env["COLORFGBG"] = "15;0"
        env.pop("NO_COLOR", None)  # The automation shell sets this; emulate a color-capable terminal.
        env.pop("HOMEWAY_TERM", None)

        def controlling_tty():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            os.tcsetpgrp(0, os.getpgrp())

        self.process = subprocess.Popen(
            [sys.executable, "-c", LAUNCHER, str(self.attributes), str(pidfile), str(binary), "tui", "--state", str(state)],
            stdin=self.slave,
            stdout=self.slave,
            stderr=self.slave,
            env=env,
            preexec_fn=controlling_tty,
            close_fds=True,
        )
        try:
            wait_until(pidfile.exists, "TUI child PID")
            self.tui_pid = int(pidfile.read_text())
        except BaseException:
            stop_process(self.process)
            os.close(self.master)
            os.close(self.slave)
            raise

    def set_size(self, cols, rows):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.screen.resize(rows, cols)

    def screen_lines(self):
        """Render pyte cells safely when an overwritten wide head leaves a stub.

        pyte's display indexes data[0] even for an orphan empty wide-tail cell.
        Keep its normal wide-character skip and all real cell contents, treating
        only a reachable empty stub as a blank at that physical column.
        """
        rendered = []
        for y in range(self.screen.lines):
            line = self.screen.buffer[y]
            cells = []
            x = 0
            while x < self.screen.columns:
                data = line[x].data
                if not data:
                    self._record_empty_stub(y, x)
                    data = " "
                assert sum(wcwidth(char) for char in data[1:]) == 0
                cells.append(data)
                x += 2 if wcwidth(data[0]) == 2 else 1
            rendered.append("".join(cells))
        return rendered

    def screen_text(self):
        return "\n".join(self.screen_lines())

    def _record_empty_stub(self, row, column):
        diagnostic = os.environ.get("HOMEWAY_TUI_PYTE_DIAGNOSTICS")
        if not diagnostic or (row, column) in self._stub_diagnostics:
            return
        self._stub_diagnostics.add((row, column))

        def neighbor(offset):
            if not 0 <= offset < self.screen.columns:
                return None
            data = self.screen.buffer[row][offset].data
            return {"first_codepoint": f"U+{ord(data[0]):04X}" if data else None,
                    "width": wcwidth(data[0]) if data else 0, "length": len(data)}

        # Coordinates and adjacent first codepoints only; never tokens or raw PTY output.
        with Path(diagnostic).open("a") as output:
            output.write(json.dumps({"row": row, "column": column, "data_length": 0,
                                     "previous": neighbor(column - 1), "next": neighbor(column + 1)}) + "\n")

    def drain(self, seconds=0.05):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            readable, _, _ = select.select([self.master], [], [], min(0.025, max(0, deadline-time.monotonic())))
            if not readable:
                continue
            try:
                chunk = os.read(self.master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            self.output.extend(chunk)
            self.stream.feed(chunk)
            query = b"\x1b]11;?\x1b\\"
            probes = self._probe_buffer + chunk
            while query in probes:
                _, probes = probes.split(query, 1)
                if self.background is not None:
                    os.write(self.master, b"\x1b]11;" + self.background.encode("ascii") + b"\x1b\\")
            self._probe_buffer = probes[-(len(query)-1):]
            # A real terminal answers these crossterm capability/cursor probes.
            if b"\x1b[6n" in chunk:
                os.write(self.master, b"\x1b[1;1R")
            if b"\x1b[c" in chunk:
                os.write(self.master, b"\x1b[?1;2c")

    def wait(self, needle, timeout=TIMEOUT):
        encoded = needle.encode() if isinstance(needle, str) else needle

        def seen():
            self.drain()
            # The backend moves the cursor between wide CJK cells, so a label is
            # not necessarily contiguous in the raw byte stream.
            if encoded in self.screen_text().encode() or encoded in ANSI.sub(b"", self.output):
                return True
            if self.process.poll() is not None:
                raise AssertionError(f"TUI exited {self.process.returncode} before {needle!r}: {self.output[-800:]!r}")
            return False

        try:
            wait_until(seen, f"TUI output {needle!r}", timeout)
        except AssertionError as error:
            raise AssertionError(f"{error}; screen:\n" + self.screen_text()) from error

    def send(self, value):
        os.write(self.master, value.encode() if isinstance(value, str) else value)

    def signal(self, value):
        try:
            os.kill(self.tui_pid, value)
        except ProcessLookupError:
            pass  # The wrapper may still be saving termios after the TUI exited.

    def wait_exit(self):
        def exited():
            self.drain()
            return self.process.poll() is not None
        wait_until(exited, "TUI exit", 8)

    def check_restored(self):
        assert ast.literal_eval(self.attributes.read_text()) == self.before, "TUI did not restore PTY termios"
        assert b"\x1b[?1049l" in self.output, "TUI did not leave alternate screen"
        assert b"\x1b[?25h" in self.output, "TUI did not show cursor on exit"

    def close(self):
        try:
            if self.process.poll() is None:
                self.signal(signal.SIGTERM)
                deadline = time.monotonic() + 8
                while self.process.poll() is None and time.monotonic() < deadline:
                    self.drain(0.05)
                if self.process.poll() is None:
                    self.signal(signal.SIGKILL)
                    self.drain(0.2)
                    stop_process(self.process)
        finally:
            self.drain()
            os.close(self.master)
            os.close(self.slave)


def sessions(binary, state):
    result = subprocess.run(
        [str(binary), "term", "list", "--json", "--state", str(state)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=6,
    )
    if result.returncode:
        raise AssertionError(f"term list failed ({result.returncode}): {result.stderr.decode(errors='replace')}")
    return json.loads(result.stdout)["sessions"]


def snapshot(tui, directory, name, light=False):
    if directory is None:
        return
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"{name}.txt").write_text(tui.screen_text() + "\n")
    # Optional visual artifact; pyte has already reconstructed the real PTY screen.
    from PIL import Image, ImageDraw, ImageFont
    mono_path = Path("/System/Library/Fonts/Menlo.ttc")
    cjk_path = Path("/System/Library/Fonts/STHeiti Medium.ttc")
    if not mono_path.exists() or not cjk_path.exists():
        return  # Text snapshots remain portable on hosts with different fonts.
    mono = ImageFont.truetype(str(mono_path), 18)
    cjk = ImageFont.truetype(str(cjk_path), 20)
    default_bg = "f8fafd" if light else "12161d"
    default_fg = "202b3b" if light else "dde2ea"
    named = {"black":"000000", "red":"cc5555", "green":"55aa77", "brown":"c9a15b", "blue":"5577cc", "magenta":"bb66bb", "cyan":"55aaaa", "white":"dddddd"}

    def color(value, default):
        value = named.get(value, value)
        if len(value) != 6 or any(c not in "0123456789abcdefABCDEF" for c in value):
            value = default
        return "#" + value

    image = Image.new("RGB", (tui.screen.columns*12+32, tui.screen.lines*24+32), "#"+default_bg)
    draw = ImageDraw.Draw(image)
    # Paint every background first so the following cell cannot erase the right
    # half of a CJK glyph drawn in the previous two-column cell.
    for y in range(tui.screen.lines):
        for x in range(tui.screen.columns):
            cell = tui.screen.buffer[y][x]
            fg, bg = color(cell.fg, default_fg), color(cell.bg, default_bg)
            if cell.reverse:
                fg, bg = bg, fg
            left, top = 16+x*12, 16+y*24
            draw.rectangle((left,top,left+11,top+23), fill=bg)
    for y in range(tui.screen.lines):
        for x in range(tui.screen.columns):
            cell = tui.screen.buffer[y][x]
            fg = color(cell.bg, default_bg) if cell.reverse else color(cell.fg, default_fg)
            left, top = 16+x*12, 16+y*24
            if cell.data:
                font = cjk if any(ord(c)>0x2fff for c in cell.data) else mono
                draw.text((left,top+2), cell.data, font=font, fill=fg)
    image.save(directory / f"{name}.png")


def idle_cpu(tui, seconds=4):
    def cumulative():
        result = subprocess.run(["ps", "-p", str(tui.tui_pid), "-o", "time="], capture_output=True, text=True, timeout=3, check=True)
        total = 0.0
        for part in result.stdout.strip().split(":"):
            total = total*60 + float(part)
        return total

    before = cumulative()
    started = time.monotonic()
    tui.drain(seconds)
    elapsed = time.monotonic()-started
    used = cumulative()-before
    print(f"INFO: idle TUI CPU={used/elapsed*100:.2f}% over {elapsed:.2f}s; 100x30 window / 69x27 shell; one logical CPU=100%")


def terminal_rows(tui):
    """Read terminal contents by physical cell coordinates, excluding the sidebar."""
    sidebar = sidebar_width(tui.screen.columns)
    rows = []
    for y in range(2, tui.screen.lines - 3):
        pieces = []
        x = sidebar
        while x < tui.screen.columns - 1:
            data = tui.screen.buffer[y][x].data or " "
            pieces.append(data)
            x += 2 if wcwidth(data[0]) == 2 else 1
        rows.append("".join(pieces))
    return rows


def exercise_ls(tui, binary, state, snapshot_dir):
    """Run real ls variants; verify rendered names, colors, and live session state."""
    fixture = state / "ls-fixtures"
    short = fixture / "short"
    styled = fixture / "styled"
    scroll = fixture / "scroll"
    for directory in (short, styled, scroll):
        directory.mkdir(parents=True)
    short_names = [f"a{index:02}" for index in range(48)]
    for name in short_names:
        (short / name).touch()
    long_name = "long-" + "x" * 128 + ".txt"
    styled_names = ["cafe\u0301.txt", "终端-🦀.txt", long_name, "run-me", "色彩目录"]
    for name in styled_names[:-1]:
        (styled / name).touch()
    (styled / "run-me").chmod(0o700)
    (styled / "色彩目录").mkdir()
    for index in range(180):
        (scroll / f"scroll_{index:03}").touch()
    entries = sessions(binary, state)
    assert len(entries) == 1
    name = entries[0]["name"]
    locale = "en_US.UTF-8" if sys.platform == "darwin" else "C.UTF-8"
    ls = Path("/bin/ls")
    assert ls.is_file(), "real /bin/ls is required by this regression"
    home = Path.home()
    project = Path(__file__).resolve().parent.parent

    def listed_names(directory):
        result = subprocess.run([str(ls), "-1", str(directory)], capture_output=True,
                                text=True, timeout=5, check=True,
                                env={**os.environ, "LC_ALL": locale})
        return [line for line in result.stdout.splitlines() if line]

    def run_case(tag, command, expected, require_all=False, color=False):
        # %s keeps the completion marker out of the echoed command itself.
        marker = "HW_LS_" + tag
        tui.send(command + "; printf 'HW_LS_%s\\n' " + shlex.quote(tag) + "\r")

        def completed():
            tui.drain()
            if tui.process.poll() is not None:
                raw = ANSI.sub(b"", tui.output[-16000:]).decode(errors="replace")
                raise AssertionError(f"real ls case {tag} crashed TUI ({tui.process.returncode}):\n{raw}")
            return marker in "\n".join(terminal_rows(tui))

        wait_until(completed, f"real ls case {tag}", 20)
        tui.drain(0.15)
        assert tui.process.poll() is None, f"TUI exited after rendering ls case {tag}"
        rows = terminal_rows(tui)
        text = unicodedata.normalize("NFC", "\n".join(rows))
        # A long ls filename can wrap across physical rows; do not insert a
        # logical newline into that name when checking the visible terminal grid.
        joined = unicodedata.normalize("NFC", "".join(rows))
        seen = [item for item in expected
                if unicodedata.normalize("NFC", item) in text
                or unicodedata.normalize("NFC", item) in joined]
        if require_all:
            assert len(seen) == len(expected), f"{tag}: missing real ls names {set(expected)-set(seen)!r}\n{text}"
        else:
            assert len(seen) >= min(2, len(expected)), f"{tag}: real ls filenames absent from screen\n{text}"
        live = sessions(binary, state)
        assert any(entry["name"] == name and entry.get("attached") for entry in live), f"{tag}: session/attach lost"
        if color:
            sidebar = sidebar_width(tui.screen.columns)
            colored = [cell for y in range(2, tui.screen.lines-3)
                       for x in range(sidebar, tui.screen.columns-1)
                       if (cell := tui.screen.buffer[y][x]).data == "色"]
            assert colored and all(cell.fg not in ("default", "dde2ea") for cell in colored), f"{tag}: ls directory color lost"
        print(f"PASS: real ls {tag}; filenames rendered and original session/attach alive")

    for cols, rows, inner_cols, inner_rows in ((100, 30, 69, 27), (160, 40, 125, 37)):
        tui.set_size(cols, rows)
        tui.signal(signal.SIGWINCH)
        tui.drain(0.2)
        tui.send(f"printf 'HW_LS_SIZE_{cols}:%s\\n' \"$(stty size)\"\r")
        tui.wait(f"HW_LS_SIZE_{cols}:{inner_rows} {inner_cols}")
        control = subprocess.run([str(ls), "-C", str(short)], capture_output=True,
                                 timeout=5, check=True,
                                 env={**os.environ, "COLUMNS": str(inner_cols), "LC_ALL": locale})
        assert b"\t" in control.stdout, f"short-name ls fixture at {cols} columns did not produce TAB"
        run_case(f"{cols}_HOME", f"command ls -C {shlex.quote(str(home))}", listed_names(home))
        run_case(f"{cols}_PROJECT", f"{ls} -C {shlex.quote(str(project))}", listed_names(project))
        run_case(f"{cols}_SHORT_TAB", f"{ls} -C {shlex.quote(str(short))}", short_names, require_all=True)
        if sys.platform == "darwin":
            colored_command = f"LC_ALL={locale} CLICOLOR_FORCE=1 LSCOLORS=exfxcxdxbxegedabagacad {ls} -G -C"
        else:
            colored_command = f"LC_ALL={locale} LS_COLORS=di=34:ex=32 {ls} --color=always -C"
        run_case(f"{cols}_COLOR_UNICODE_LONG", colored_command + " " + shlex.quote(str(styled)),
                 styled_names, require_all=True, color=True)
        if cols == 160:
            snapshot(tui, snapshot_dir, "ls-wide-color")
        run_case(f"{cols}_SCROLL", f"{ls} -1 {shlex.quote(str(scroll))}",
                 ["scroll_178", "scroll_179"], require_all=True)
        if cols == 160:
            snapshot(tui, snapshot_dir, "ls-wide-scroll")
    tui.set_size(100, 30)
    tui.signal(signal.SIGWINCH)
    tui.drain(0.2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path(__file__).resolve().parent.parent / "target/debug/homeway")
    parser.add_argument("--snapshot-dir", type=Path, help="save private PTY text snapshots (and PNG with Pillow/macOS fonts)")
    args = parser.parse_args()
    binary = args.bin.resolve()
    if not binary.is_file():
        parser.error(f"binary missing: {binary}; run cargo build -p homeway-cli")
    with tempfile.TemporaryDirectory(prefix="homeway-tui-smoke-", dir="/tmp") as temporary:
        state = Path(temporary)
        # Existing zero-argument daemon bootstrap otherwise creates serve.enabled=true.
        (state / "config.toml").write_text("[serve]\nenabled = false\n\n[relay]\nenabled = false\n")
        daemon = None
        first = None
        second = None
        with (state / "daemon.log").open("wb") as log:
            try:
                env = os.environ.copy()
                env.pop("HOMEWAY_TERM", None)
                daemon = subprocess.Popen([str(binary), "--state", str(state)], stdout=log, stderr=log, env=env)

                def ready():
                    if daemon.poll() is not None:
                        raise AssertionError(f"private daemon exited {daemon.returncode}: {(state/'daemon.log').read_text()[-1000:]}")
                    return (state / "control.sock").exists() and (state / "local-term.sock").exists()

                wait_until(ready, "private daemon sockets")
                assert sessions(binary, state) == [], "new private state unexpectedly has sessions"
                first = Tui(binary, state)
                first.wait("主机")
                first.wait("会话")
                first.wait("想在这台主机上做些什么？")
                snapshot(first, args.snapshot_dir, "empty-dark")
                first.send(b"\x02n")
                first.wait("已接入")
                first.send("printf 'HW_TUI_%s\\n' FIRST\r")
                first.wait("HW_TUI_FIRST")
                # Report the actual current directory through the existing OSC7
                # contract; the TUI must not guess a project from process defaults.
                first.send("printf '\\033]7;file://localhost%s\\007' \"$PWD\"\r")

                def reported_cwd():
                    first.drain()  # Keep consuming PTY diff output while the command is typed.
                    return next((entry.get("cwd") for entry in sessions(binary,state) if entry.get("cwd")), None)

                try:
                    cwd = wait_until(reported_cwd, "OSC7 cwd in authoritative LIST")
                except AssertionError as error:
                    first.drain()
                    raise AssertionError(f"{error}; LIST={sessions(binary,state)!r}; screen:\n" + first.screen_text()) from error
                project = Path(cwd).name

                def project_visible():
                    first.drain()
                    return any(project in line[:sidebar_width(first.screen.columns)] for line in first.screen_lines())

                wait_until(project_visible, "OSC7 project name in session sidebar")
                print("PASS: OSC7 current directory reaches LIST and session project label")
                first.send("printf 'HW_SIZE:%s\\n' \"$(stty size)\"\r")
                first.wait("HW_SIZE:27 69")
                snapshot(first, args.snapshot_dir, "shell-dark")
                print("PASS: create shell, execute command, viewport matches PTY 69x27")
                # One local host has a six-line panel, including two blank
                # content lines. Click second lines: local y=2, session y=7.
                mouse_name = sessions(binary, state)[0]["name"]
                first.send(b"\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<0;5;8M\x1b[<0;5;8m")
                first.send("printf 'HW_SIDEBAR_%s\\n' MOUSE\r")
                first.wait("HW_SIDEBAR_MOUSE")
                assert any(entry["name"] == mouse_name and entry.get("attached")
                           for entry in sessions(binary, state))
                print("PASS: mouse hits compact local/session second lines and returns input to the same shell")
                idle_cpu(first)
                exercise_ls(first, binary, state, args.snapshot_dir)

                first.set_size(120, 36)
                first.signal(signal.SIGWINCH)
                first.drain(0.5)
                first.send("printf 'HW_RESIZE:%s\\n' \"$(stty size)\"\r")
                first.wait("HW_RESIZE:33 85")
                print("PASS: resize propagates to PTY 85x33")

                # Emulate the outer terminal changing its background, then focus-in.
                first.background = "rgb:ffff/ffff/ffff"
                first.send(b"\x1b[I")
                wait_until(lambda: (first.drain() or first.screen.buffer[29][0].bg == "f8fafd"), "automatic light theme")
                first.drain(0.15)
                snapshot(first, args.snapshot_dir, "shell-light", light=True)
                first.send(b"\x02q")
                first.wait_exit()
                first.drain(0.1)
                assert first.process.returncode == 0
                first.check_restored()
                entries = sessions(binary, state)
                assert len(entries) == 1, f"detaching lost session: {entries!r}"
                name = entries[0]["name"]
                print("PASS: automatic theme follows terminal, detach restores terminal and preserves shell")

                (state / "tui-theme").write_text("dark\n")  # Obsolete preference cannot override terminal background.
                second = Tui(binary, state, background="rgb:ffff/ffff/ffff")
                second.wait(name)
                # Sidebar session focus, move to the first existing session, then attach.
                second.send(b"\x02s\x1b[B\r")
                second.wait("已接入")
                second.send("printf 'HW_TUI_%s\\n' SECOND\r")
                second.wait("HW_TUI_SECOND")
                # Close a disposable second shell through the actual TUI menu;
                # the original detached session must remain untouched.
                second.send(b"\x02n")

                def second_shell_created():
                    second.drain()
                    return next((entry["name"] for entry in sessions(binary,state) if entry["name"] != name), None)

                closing_name = wait_until(second_shell_created, "second shell created by TUI")

                def second_shell_attached():
                    second.drain()
                    lines = second.screen_lines()
                    return closing_name in lines[0] and "已接入" in lines[-1]

                wait_until(second_shell_attached, "second shell attached in TUI")
                snapshot(second, args.snapshot_dir, "terminal-close-ready-light", light=True)
                # The close button is five cells on the top border; its X is at
                # zero-based columns-4, independently of the sidebar split.
                column = second.screen.columns - 3  # SGR uses one-based coordinates.
                second.send(f"\x1b[<0;{column};1M\x1b[<0;{column};1m")

                def second_shell_closed():
                    second.drain()
                    entries = sessions(binary, state)
                    return len(entries) == 1 and entries[0]["name"] == name

                wait_until(second_shell_closed, "header X closes only current second shell")
                wait_until(lambda: (second.drain() or "想在这台主机上做些什么？" in second.screen_text()), "closed selection clears to the current empty view")
                wait_until(lambda: (second.drain() or second.screen.buffer[0][second.screen.columns-6].fg == "627083"), "empty close button becomes disabled")
                snapshot(second, args.snapshot_dir, "terminal-close-empty-light", light=True)
                assert second.process.poll() is None, "X exited the TUI"
                print("PASS: real header X press/release closes only selected shell; original remains and empty X is disabled")
                second.send(b"\x02s\x1b[B\r")
                def original_reattached():
                    second.drain()
                    lines = second.screen_lines()
                    return name in lines[0] and "已接入" in lines[-1]
                wait_until(original_reattached, "original shell reattached after header close")
                second.send("printf 'HW_AFTER_%s\\n' CLOSE\r")
                second.wait("HW_AFTER_CLOSE")
                print("PASS: TUI remains usable after X; original shell reattaches and executes input")

                # Retain the existing context-menu regression using a fresh
                # disposable shell, so the header path cannot mask menu issues.
                second.send(b"\x02n")
                closing_name = wait_until(second_shell_created, "disposable menu shell created by TUI")
                wait_until(second_shell_attached, "disposable menu shell attached in TUI")
                second.send(b"\x02s\x1bOQ")  # Session sidebar, F2 context menu.
                second.wait("关闭会话")
                second.send(b"\r")

                wait_until(second_shell_closed, "context menu closed only second shell")
                print("PASS: TUI session context menu closes second shell and preserves original")
                second.signal(signal.SIGTERM)
                second.wait_exit()
                second.drain(0.1)
                assert second.process.returncode == 0
                second.check_restored()
                assert [entry["name"] for entry in sessions(binary, state)] == [name]
                print("PASS: reattach to same shell; SIGTERM restores terminal and preserves session")
            finally:
                try:
                    if second is not None:
                        second.close()
                finally:
                    try:
                        if first is not None:
                            first.close()
                    finally:
                        stop_process(daemon)
    print("PASS: private TUI smoke complete; all script-owned processes stopped")


if __name__ == "__main__":
    main()
