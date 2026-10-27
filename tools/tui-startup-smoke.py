#!/usr/bin/env python3
"""Check the real empty TUI startup footer using only a fresh private state.

Run after building: python3 tools/tui-startup-smoke.py --bin target/debug/homeway
Uses the same pyte/Pillow dependencies and PTY helpers as tui-smoke.py.
"""

import argparse
import hashlib
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
helpers = runpy.run_path(str(Path(__file__).with_name("tui-smoke.py")))
Tui = helpers["Tui"]
wait_until = helpers["wait_until"]
sessions = helpers["sessions"]
snapshot = helpers["snapshot"]
stop_process = helpers["stop_process"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path("target/debug/homeway"))
    parser.add_argument("--snapshot-dir", type=Path)
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    print("INFO: binary SHA256=" + hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)
    with tempfile.TemporaryDirectory(prefix="homeway-tui-startup-", dir="/tmp") as temporary:
        state = Path(temporary)
        (state / "config.toml").write_text("[serve]\nenabled = false\n\n[relay]\nenabled = false\n")
        daemon = None
        tui = None
        with (state / "daemon.log").open("wb") as log:
            try:
                env = os.environ.copy()
                env.pop("HOMEWAY_TERM", None)
                daemon = subprocess.Popen([str(binary), "--state", str(state)], stdout=log, stderr=log, env=env)

                def ready():
                    if daemon.poll() is not None:
                        raise AssertionError(f"private daemon exited {daemon.returncode}")
                    return (state / "control.sock").exists() and (state / "local-term.sock").exists()

                wait_until(ready, "private control and local terminal sockets")
                assert sessions(binary, state) == [], "fresh private state must have no sessions"
                tui = Tui(binary, state)
                tui.wait("想在这台主机上做些什么？")
                # Two complete periodic refresh intervals prove this is a settled
                # empty view, rather than a transient frame before the response.
                tui.drain(4.5)
                assert daemon.poll() is None and tui.process.poll() is None
                assert sessions(binary, state) == [], "startup must not create a session"
                footer = tui.screen_lines()[-1].strip()
                print("INFO: settled empty footer=" + footer, flush=True)
                snapshot(tui, args.snapshot_dir, "startup-empty")
                assert "正在连接" not in footer and "正在读取" not in footer, footer
                assert "已连接" in footer or "已就绪" in footer, footer
                assert "创建" in footer, footer
                print("PASS: live private empty startup completes connection/list progress and keeps create guidance after periodic refresh", flush=True)
                tui.send(b"\x02q")
                tui.wait_exit()
                assert tui.process.returncode == 0
                tui.check_restored()
                print("PASS: clean TUI detach restores terminal; private daemon remains alive with zero sessions", flush=True)
                assert daemon.poll() is None and sessions(binary, state) == []
            finally:
                if tui is not None:
                    tui.close()
                stop_process(daemon)
    print("PASS: private startup resources cleaned", flush=True)


if __name__ == "__main__":
    main()
