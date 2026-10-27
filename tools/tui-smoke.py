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
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

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


class Tui:
    def __init__(self, binary, state):
        self.master, self.slave = pty.openpty()
        self.before = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self._stub_diagnostics = set()
        self.screen = pyte.Screen(100, 30)
        self.stream = pyte.ByteStream(self.screen)
        self.attributes = state / f"smoke-termios-{self.master}"
        pidfile = state / f"smoke-tui-pid-{self.master}"
        self.set_size(100, 30)
        env = os.environ.copy()
        env["TERM"] = "xterm-256color"
        env["COLORTERM"] = "truecolor"
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
    print(f"INFO: idle TUI CPU={used/elapsed*100:.2f}% over {elapsed:.2f}s; 100x30 window / 66x25 shell; one logical CPU=100%")


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
                    return project in first.screen_lines()[16][:33]

                wait_until(project_visible, "OSC7 project name in session sidebar")
                print("PASS: OSC7 current directory reaches LIST and session project label")
                first.send("printf 'HW_SIZE:%s\\n' \"$(stty size)\"\r")
                first.wait("HW_SIZE:25 66")
                snapshot(first, args.snapshot_dir, "shell-dark")
                print("PASS: create shell, execute command, viewport matches PTY 66x25")
                idle_cpu(first)

                first.set_size(120, 36)
                first.signal(signal.SIGWINCH)
                first.drain(0.5)
                first.send("printf 'HW_RESIZE:%s\\n' \"$(stty size)\"\r")
                first.wait("HW_RESIZE:31 81")
                print("PASS: resize propagates to PTY 81x31")

                # Switch theme through the TUI; this is private state only.
                first.send(b"\x02t")
                wait_until(lambda: (state / "tui-theme").exists(), "theme saved")
                assert (state / "tui-theme").read_text().strip() == "light"
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
                print("PASS: theme persists, detach restores terminal and preserves shell")

                second = Tui(binary, state)
                second.wait("主题：浅色")
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
                    return closing_name in lines[1] and "已接入" in lines[-2]

                wait_until(second_shell_attached, "second shell attached in TUI")
                second.send(b"\x02s\x1bOQ")  # Session sidebar, F2 context menu.
                second.wait("关闭会话")
                second.send(b"\r")

                def second_shell_closed():
                    second.drain()
                    entries = sessions(binary,state)
                    return len(entries) == 1 and entries[0]["name"] == name

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
