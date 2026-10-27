#!/usr/bin/env python3
"""Real private PTYs: passive surface geometry and a TUI inside its carrier shell.

Build homeway first, then run with --bin target/debug/homeway. Uses the pyte
dependency and terminal model from tui-smoke.py. All state is private under /tmp;
only script-owned processes are stopped. No network roles or clipboard are used.
"""

import argparse
import fcntl
import hashlib
import os
from pathlib import Path
import pty
import runpy
import shlex
import signal
import subprocess
import sys
import tempfile
import termios


REPO = Path(__file__).resolve().parent.parent
helpers = runpy.run_path(str(REPO / "tools/tui-smoke.py"))
Tui = helpers["Tui"]
wait_until = helpers["wait_until"]
stop_process = helpers["stop_process"]
sessions = helpers["sessions"]
snapshot = helpers["snapshot"]
pyte = helpers["pyte"]


class Term(Tui):
    """The actual homeway term CLI, with detach prefix disabled for nested TUI."""

    def __init__(self, binary, state, name):
        self.master, self.slave = pty.openpty()
        self.before = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self.background = "rgb:0000/0000/0000"
        self._probe_buffer = b""
        self._stub_diagnostics = set()
        self.screen = pyte.Screen(140, 40)
        self.stream = pyte.ByteStream(self.screen)
        self.attributes = state / f"observer-termios-{self.master}"
        pidfile = state / f"observer-term-pid-{self.master}"
        self.set_size(140, 40)
        env = os.environ.copy()
        env["TERM"] = "xterm-256color"
        env["COLORTERM"] = "truecolor"

        def controlling_tty():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            os.tcsetpgrp(0, os.getpgrp())

        self.process = subprocess.Popen(
            [sys.executable, "-c", helpers["LAUNCHER"], str(self.attributes), str(pidfile),
             str(binary), "term", "attach", name, "--detach-key", "none", "--state", str(state)],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env,
            preexec_fn=controlling_tty, close_fds=True,
        )
        try:
            wait_until(pidfile.exists, "term child PID")
            self.tui_pid = int(pidfile.read_text())
        except BaseException:
            stop_process(self.process)
            os.close(self.master)
            os.close(self.slave)
            raise


def cli(binary, state, *args):
    result = subprocess.run([str(binary), *args, "--state", str(state)], cwd=REPO,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
    if result.returncode:
        raise AssertionError(f"private CLI {args[:2]}: {result.stderr.decode(errors='replace')[-600:]}")
    return result.stdout


def entry(binary, state, name):
    return next((session for session in sessions(binary, state) if session["name"] == name), None)


def current_screen(tui, needle):
    def visible():
        tui.drain()
        assert tui.process.poll() is None, f"PTY process exited: {tui.process.returncode}"
        return needle in tui.screen_text()
    wait_until(visible, f"current screen {needle!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=REPO / "target/debug/homeway")
    parser.add_argument("--snapshot-dir", type=Path, help="save private PTY screenshots/text for review")
    args = parser.parse_args()
    binary = args.bin.resolve()
    before_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    print(f"INFO: binary SHA256={before_hash}", flush=True)
    with tempfile.TemporaryDirectory(prefix="homeway-tui-observer-", dir="/tmp") as temporary:
        state = Path(temporary)
        (state / "config.toml").write_text("[serve]\nenabled=false\n[relay]\nenabled=false\n")
        daemon = None
        wide = None
        tui = None
        carrier = None
        with (state / "daemon.log").open("wb") as log:
            try:
                env = os.environ.copy()
                env.pop("HOMEWAY_TERM", None)
                daemon = subprocess.Popen([str(binary), "--state", str(state)], stdout=log, stderr=log, env=env)
                wait_until(lambda: (state / "local-term.sock").exists() and (state / "control.sock").exists(), "private daemon")
                cli(binary, state, "term", "new", "wide", "-d")
                wide = Term(binary, state, "wide")
                wide.send("printf 'WIDE_%s_%0100d_WIDE_%s\\n' BEGIN 0 END; printf 'WIDE_%s\\n' READY\r")
                wide.wait("WIDE_READY")
                wide.drain(0.6)  # The existing raw CLI attach repaint sentinel must settle.
                assert (entry(binary, state, "wide")["cols"], entry(binary, state, "wide")["rows"]) == (140, 40)
                original_pid = entry(binary, state, "wide")["pid"]
                # An inherited, matching name is not proof of self identity.
                previous = os.environ.get("TERM_SESSION_ID")
                try:
                    os.environ["TERM_SESSION_ID"] = "tailcat-wide"
                    tui = Tui(binary, state)
                finally:
                    if previous is None:
                        os.environ.pop("TERM_SESSION_ID", None)
                    else:
                        os.environ["TERM_SESSION_ID"] = previous
                current_screen(tui, "会话")
                tui.send(b"\x02s\x1b[A\r")
                tui.wait("已接入")
                tui.drain(0.3)
                session = entry(binary, state, "wide")
                assert (session["cols"], session["rows"]) == (140, 40), "viewing stole active raw geometry"
                assert any(client["kind"] == "host" and client["active"] for client in session["clients"])
                assert "WIDE_END" in wide.screen_text(), "wide raw terminal must show the full 120-column line"
                assert "WIDE_BEGIN" in tui.screen_text(), "observer must show the authoritative line prefix"
                assert "WIDE_END" not in tui.screen_text(), "cropped tail must not reflow into the next pane row"
                snapshot(wide, args.snapshot_dir, "raw-wide-140x40")
                snapshot(tui, args.snapshot_dir, "observer-cropped-pane")
                print("PASS: real 140x40 raw attach stays active while narrow TUI observes; inherited name does not block", flush=True)

                tui.set_size(80, 24)
                tui.signal(signal.SIGWINCH)

                def observer_size():
                    tui.drain()
                    session = entry(binary, state, "wide")
                    assert (session["cols"], session["rows"]) == (140, 40), "observer resize stole shared geometry"
                    return next(((client["cols"], client["rows"]) for client in session["clients"]
                                 if client["kind"] == "app" and client["cols"] < 66), None)

                cols, rows = wait_until(observer_size, "observer pane preference after terminal resize")
                tui.send("printf 'OBSERVER_SIZE:%s\\n' \"$(stty size)\"\r")
                tui.wait(f"OBSERVER_SIZE:{rows} {cols}")
                session = entry(binary, state, "wide")
                assert (session["cols"], session["rows"]) == (cols, rows)
                assert any(client["kind"] == "app" and client["active"] for client in session["clients"])
                print("PASS: real TUI input activates its pane size before shell stty reads geometry", flush=True)

                wide.send(" ")  # Explicit activity restores raw geometry; let its legacy sentinel finish.
                wide.drain(0.6)
                wide.send("printf 'RAW_SIZE:%s\\n' \"$(stty size)\"\r")
                wide.wait("RAW_SIZE:40 140")
                tui.drain(0.3)
                assert (entry(binary, state, "wide")["cols"], entry(binary, state, "wide")["rows"]) == (140, 40)
                # The program is on row 35 of a 40-row authoritative grid. The
                # 21-row pane crops it, but a real click first activates/resizes
                # to that pane. Its SGR coordinates must therefore be pane-local.
                wide.send("saved=$(stty -g); stty raw -echo; printf '\\033[?1000h\\033[?1006h\\033[35;1HMOUSE_%s' WAIT; "
                          "got=$(dd bs=1 count=18 2>/dev/null | od -An -tx1 | tr -d ' \\n'); "
                          "printf '\\033[?1000l\\033[?1006l\\r\\nMOUSE_HEX:%s\\r\\nMOUSE_SIZE:%s\\r\\n' \"$got\" \"$(stty size)\"; stty \"$saved\"\r")
                tui.wait("MOUSE_WAIT")
                tui.drain(0.2)
                assert (entry(binary, state, "wide")["cols"], entry(binary, state, "wide")["rows"]) == (140, 40)
                sidebar = min(max(tui.screen.columns // 3, 20), 38)
                # panel inner origin=(sidebar,1), click local (2,3), SGR is 1-based.
                tui.send(f"\x1b[<0;{sidebar + 3};5M\x1b[<0;{sidebar + 3};5m")
                tui.wait("MOUSE_HEX:1b5b3c303b333b344d1b5b3c303b333b346d")  # Both SGR M/m use local row 4.
                tui.wait(f"MOUSE_SIZE:{rows} {cols}")
                print("PASS: consecutive cropped-pane press/release reach PTY as local SGR row 4 after activation resize", flush=True)
                tui.send(b"\x02q")
                tui.wait_exit()
                assert tui.process.returncode == 0
                tui.check_restored()
                tui.close()
                tui = None
                assert entry(binary, state, "wide")["pid"] == original_pid
                print("PASS: original raw leg takes back 140x40 on input; TUI detach preserves the same shell PID", flush=True)

                cli(binary, state, "term", "new", "carrier", "-d")
                carrier = Term(binary, state, "carrier")
                carrier.send("printf 'CARRIER_%s\\n' READY\r")
                carrier.wait("CARRIER_READY")
                carrier.drain(0.6)
                carrier_pid = entry(binary, state, "carrier")["pid"]
                carrier.send(f"{shlex.quote(str(binary))} tui --state {shlex.quote(str(state))}; printf 'NESTED_%s\\n' RETURN\r")
                current_screen(carrier, "·自身")
                carrier.send(b"\x02s\x1b[A\r")
                current_screen(carrier, "不能重复接入")
                snapshot(carrier, args.snapshot_dir, "nested-self-session-blocked")
                assert entry(binary, state, "carrier")["pid"] == carrier_pid
                assert all(client["kind"] != "app" for client in entry(binary, state, "carrier")["clients"])
                print("PASS: term -> real carrier shell -> TUI marks its true ancestor session and blocks recursive attach", flush=True)

                before_names = {session["name"] for session in sessions(binary, state)}
                carrier.send(b"\x02n")
                current_screen(carrier, "已接入")
                carrier.send("printf 'NESTED_NEW_%s\\n' WORKS\r")
                carrier.wait("NESTED_NEW_WORKS")
                new_names = {session["name"] for session in sessions(binary, state)} - before_names
                assert len(new_names) == 1
                carrier.send(b"\x02q")
                carrier.wait("NESTED_RETURN")
                assert entry(binary, state, "carrier")["pid"] == carrier_pid
                assert new_names <= {session["name"] for session in sessions(binary, state)}
                print("PASS: nested TUI can create/control another shell and detach back to the surviving carrier", flush=True)
            finally:
                cleanup_errors = []
                for process in (tui, carrier, wide):
                    if process is not None:
                        try:
                            process.close()
                        except BaseException as error:
                            cleanup_errors.append(error)
                stop_process(daemon)
                assert daemon is None or daemon.poll() is not None
                assert hashlib.sha256(binary.read_bytes()).hexdigest() == before_hash, "binary changed during verification"
                if cleanup_errors:
                    raise AssertionError(f"PTY cleanup failed: {cleanup_errors!r}")
        print("PASS: all private PTYs/daemon reclaimed; binary SHA256 unchanged", flush=True)


if __name__ == "__main__":
    main()
