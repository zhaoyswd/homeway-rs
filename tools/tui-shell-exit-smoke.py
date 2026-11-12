#!/usr/bin/env python3
"""Check shell exit clears the real TUI using only a private /tmp daemon."""

import argparse
from contextlib import ExitStack
import hashlib
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parent.parent
helpers = runpy.run_path(str(REPO / "tools/tui-smoke.py"))
Tui, wait_until, sessions, stop_process, snapshot = (
    helpers[name] for name in ("Tui", "wait_until", "sessions", "stop_process", "snapshot")
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=REPO / "target/debug/homeway")
    parser.add_argument("--snapshot-dir", type=Path)
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    print("INFO: binary SHA256=" + digest, flush=True)
    with tempfile.TemporaryDirectory(prefix="homeway-tui-exit-", dir="/tmp") as directory:
        state = Path(directory)
        (state / "config.toml").write_text("[serve]\nenabled=false\n[relay]\nenabled=false\n")
        env = os.environ.copy()
        for key in ("HOMEWAY_TERM", "HOMEWAY_TERM_SHELL", "HOMEWAY_TERM_VT", "HOMEWAY_TERM_DETECT"):
            env.pop(key, None)
        with (state / "daemon.log").open("wb") as log, ExitStack() as cleanup:
            daemon = subprocess.Popen([str(binary), "--state", str(state)], stdout=log, stderr=log, env=env)
            cleanup.callback(stop_process, daemon)
            wait_until(lambda: daemon.poll() is None and (state / "local-term.sock").exists(), "private daemon")
            tui = Tui(binary, state)
            cleanup.callback(tui.close)
            tui.wait("想在这台主机上做些什么？")
            for index, command in enumerate((b"exit\r", b"exit 7\r")):
                tui.send(b"\x02n")
                tui.wait_terminal()
                entries = sessions(binary, state)
                assert len(entries) == 1
                name = entries[0]["name"]
                marker = f"EXIT_OLD_SCREEN_{index}"
                tui.send(f"printf '{marker}\\n'\r")

                def old_visible():
                    tui.drain()
                    return marker in tui.screen_text()

                wait_until(old_visible, "old terminal marker")
                tui.send(command)
                wait_until(lambda: not sessions(binary, state), "shell removed from service list")

                def empty_visible():
                    tui.drain()
                    assert tui.process.poll() is None, "shell exit must preserve the TUI"
                    width = helpers["sidebar_width"](tui.screen.columns)
                    rows = []
                    for y in range(1, tui.screen.lines - 1):
                        row, x = "", width
                        while x < tui.screen.columns - 1:
                            data = tui.screen.buffer[y][x].data or " "
                            row += data
                            x += max(1, helpers["wcwidth"](data[0]))
                        rows.append(row)
                    text = "\n".join(rows)
                    sidebar = "\n".join(line[:width] for line in tui.screen_lines())
                    return ("想在这台主机上做些什么？" in text and marker not in text
                            and "[ X ]" not in tui.screen_lines()[0] and name not in sidebar)

                try:
                    wait_until(empty_visible, "current empty content and hidden close button after shell exit", 3)
                except AssertionError:
                    print("INFO: current TUI after the shell disappeared:\n" + tui.screen_text(), flush=True)
                    raise
                if args.snapshot_dir:
                    snapshot(tui, args.snapshot_dir, f"shell-exit-{index}-empty")
                print(f"PASS: exit {'0' if index == 0 else '7'} removes session, clears old content and keeps TUI usable", flush=True)
            tui.send(b"\x02q")
            tui.wait_exit()
            tui.check_restored()
            assert daemon.poll() is None and not sessions(binary, state)
            print("PASS: TUI exits normally and restores TTY", flush=True)
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest
    print("PASS: private resources cleaned; binary unchanged", flush=True)


if __name__ == "__main__":
    main()
