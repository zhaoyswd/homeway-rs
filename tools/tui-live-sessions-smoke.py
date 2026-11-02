#!/usr/bin/env python3
"""Real PTY title/cwd refresh in a private /tmp daemon, without network roles.

Uses the existing tui-smoke.py pyte helpers; build homeway first. Samples are
end-to-end observations, not a latency guarantee for arbitrary remote hosts.
"""

import argparse
from contextlib import ExitStack
import hashlib
import os
from pathlib import Path
import runpy
import shlex
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parent.parent
helpers = runpy.run_path(str(REPO / "tools/tui-smoke.py"))
observer = runpy.run_path(str(REPO / "tools/tui-observer-smoke.py"))
Tui, wait_until, sessions, stop_process = (
    helpers[name] for name in ("Tui", "wait_until", "sessions", "stop_process")
)
Term, cli = observer["Term"], observer["cli"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path("target/debug/homeway"))
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    print("INFO: binary SHA256=" + digest, flush=True)
    with tempfile.TemporaryDirectory(prefix="homeway-tui-live-", dir="/tmp") as directory:
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
            tui.send(b"\x02n")
            tui.wait_terminal()
            name = sessions(binary, state)[0]["name"]
            active = Term(binary, state, name)
            cleanup.callback(active.close)
            active.drain(0.3)
            cli(binary, state, "term", "new", "background", "-d")
            background = Term(binary, state, "background")
            cleanup.callback(background.close)
            background.drain(0.3)

            def title_sample(sender, title, selected):
                started = time.monotonic()
                sender.send("printf '\\033]0;" + title + "\\007'\r")

                def visible():
                    sender.drain()
                    tui.drain()
                    width = helpers["sidebar_width"](tui.screen.columns)
                    rows = ["".join(tui.screen.buffer[y][x].data or " " for x in range(width))
                            for y in range(1, tui.screen.lines - 1)]
                    header = tui.screen_lines()[0]
                    return any(title in row for row in rows) and (not selected or title in header)

                wait_until(visible, "title visible in sidebar" + (" and header" if selected else ""), 1.5)
                elapsed = time.monotonic() - started
                print(f"INFO: {'active' if selected else 'background'} {title} {elapsed:.3f}s", flush=True)
                return elapsed

            for index in range(4):
                title_sample(active, f"live-{index}", True)
            print("PASS: four selected-session title changes reach both panes within 1.5s each", flush=True)
            for index in range(4):
                title_sample(background, f"other-{index}", False)
            print("PASS: four unattached-session title changes reach sidebar within 1.5s each", flush=True)
            project = state / "q7"
            project.mkdir()
            background.send("cd " + shlex.quote(str(project)) + "; printf '\\033]7;file://localhost" + str(project) + "\\007'\r")
            wait_until(lambda: (tui.drain() or any("q7" in line[:helpers["sidebar_width"](tui.screen.columns)]
                                                  for line in tui.screen_lines())), "background cwd in sidebar", 1.5)
            assert tui.terminal_ready("live-3"), "background refresh must preserve selected terminal"
            width = helpers["sidebar_width"](tui.screen.columns)
            sidebar = "\n".join("".join(tui.screen.buffer[y][x].data or " " for x in range(width))
                                for y in range(1, tui.screen.lines - 1))
            assert sidebar.index("live-3") < sidebar.index("other-3"), "sidebar reordered sessions"
            assert [entry["name"] for entry in sorted(sessions(binary, state), key=lambda e: (e["createdMs"], e["name"]))] == [name, "background"]
            print("PASS: background cwd refresh preserves active title and stable creation order", flush=True)
            tui.send(b"\x02q")
            tui.wait_exit()
            tui.check_restored()
            assert daemon.poll() is None and len(sessions(binary, state)) == 2
            print("PASS: TUI exit restores TTY and preserves both PTYs", flush=True)
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest
    print("PASS: private resources cleaned; binary unchanged", flush=True)


if __name__ == "__main__":
    main()
