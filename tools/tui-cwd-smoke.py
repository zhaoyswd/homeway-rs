#!/usr/bin/env python3
"""Verify real default-shell cwd and cd without OSC7, using only private state.

No shell rc or prompt is modified and no pwd command is injected by homeway.
The test types a normal printf to a private file to observe the shell's own PWD.
Requires the same pyte/Pillow tools as tui-smoke.py.
"""

import argparse
import hashlib
import os
from pathlib import Path
import runpy
import shlex
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parent.parent
helpers = runpy.run_path(str(REPO / "tools/tui-smoke.py"))
Term = runpy.run_path(str(REPO / "tools/tui-observer-smoke.py"))["Term"]
Tui, wait_until, sessions = (helpers[name] for name in ("Tui", "wait_until", "sessions"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path("target/debug/homeway"))
    parser.add_argument("--snapshot-dir", type=Path)
    parser.add_argument("--detect-off", action="store_true")
    parser.add_argument("--raw-only", action="store_true")
    parser.add_argument("--project-name", default="a b", help="private basename; keep it short enough for narrow unknown/unknown rows")
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    print("INFO: binary SHA256=" + hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)
    with tempfile.TemporaryDirectory(prefix="homeway-tui-cwd-", dir="/tmp") as directory:
        state = Path(directory)
        (state / "config.toml").write_text("[serve]\nenabled=false\n[relay]\nenabled=false\n")
        env = os.environ.copy()
        for key in ("HOMEWAY_TERM", "HOMEWAY_TERM_SHELL", "HOMEWAY_TERM_VT", "HOMEWAY_TERM_DETECT"):
            env.pop(key, None)
        if args.detect_off:
            env["HOMEWAY_TERM_DETECT"] = "off"
        if args.raw_only:
            env["HOMEWAY_TERM_VT"] = "off"
        daemon = tui = raw = None
        with (state / "daemon.log").open("wb") as log:
            try:
                daemon = subprocess.Popen([str(binary), "--state", str(state)], stdout=log, stderr=log, env=env)
                wait_until(lambda: daemon.poll() is None and (state / "local-term.sock").exists(), "private daemon ready")
                assert sessions(binary, state) == []
                if args.raw_only:
                    result = subprocess.run([str(binary), "term", "new", "cwd-shell", "-d", "--state", str(state)],
                                            capture_output=True, timeout=12)
                    assert result.returncode == 0, result.stderr.decode(errors="replace")
                else:
                    tui = Tui(binary, state)
                    tui.wait("想在这台主机上做些什么？")
                    tui.send(b"\x02n")
                    tui.wait("已接入")
                name = sessions(binary, state)[0]["name"]
                raw = Term(binary, state, name)
                raw.drain(0.5)
                assert b"\x1b]7;" not in raw.output, "default login shell unexpectedly emits OSC7; fixture is not OSC-free"
                sender = raw if args.raw_only else tui
                report = state / "shell-pwd"

                def observe(command, expected=None):
                    report.unlink(missing_ok=True)
                    sender.send(command + f"printf '%s' \"$PWD\" > {shlex.quote(str(report))}\r")

                    def reported():
                        raw.drain()
                        if tui:
                            tui.drain()
                        return report.read_text() if report.exists() and report.stat().st_size else None

                    actual = wait_until(reported, "default shell PWD file")
                    if expected is not None:
                        assert actual == str(expected), (actual, expected)
                    # macOS /tmp is a symlink to /private/tmp. OS process cwd
                    # reports the physical path; the shell PWD may be logical.
                    physical = str(Path(actual).resolve())

                    def listed():
                        raw.drain()
                        if tui:
                            tui.drain()
                        entry = next(entry for entry in sessions(binary, state) if entry["name"] == name)
                        return entry if entry.get("cwd") == physical else None

                    wait_until(listed, f"LIST cwd matches real shell directory {physical!r} without OSC7", 8)
                    assert b"\x1b]7;" not in raw.output, "test default shell emitted OSC7"
                    if tui:
                        project = Path(actual).name or "/"
                        try:
                            wait_until(lambda: (tui.drain() or any(project in line[:helpers["sidebar_width"](tui.screen.columns)]
                                                                   for line in tui.screen_lines())), "real cwd basename in sidebar")
                        except AssertionError as error:
                            helpers["snapshot"](tui, args.snapshot_dir, "cwd-failure")
                            raise AssertionError(f"{error}; expected basename {project!r}; current screen:\n{tui.screen_text()}") from error
                    return actual

                initial = observe("")
                print("PASS: default login shell initial cwd reaches LIST" + (" and TUI basename" if tui else " in raw-only mode"), flush=True)
                assert args.project_name not in ("", ".", "..") and Path(args.project_name).name == args.project_name
                project = state / args.project_name
                project.mkdir()
                observe("cd " + shlex.quote(str(project)) + "; ", project)
                print("PASS: cd updates real cwd without OSC7; legal basename space preserved", flush=True)
                if tui:
                    helpers["snapshot"](tui, args.snapshot_dir, "cwd-no-osc7")
                # Only now intentionally emit OSC7, to prove precedence over the
                # otherwise different process cwd. No directory is guessed.
                sender.send("printf '\\033]7;file://localhost/osc-priority\\007'\r")

                def preferred():
                    raw.drain()
                    if tui:
                        tui.drain()
                    return next(entry for entry in sessions(binary, state) if entry["name"] == name).get("cwd") == "/osc-priority"

                wait_until(preferred, "explicit OSC7 remains authoritative", 8)
                print("PASS: valid OSC7 keeps precedence over sampled OS cwd", flush=True)
                assert daemon.poll() is None and initial
            finally:
                try:
                    if raw:
                        raw.close()
                finally:
                    try:
                        if tui:
                            tui.close()
                    finally:
                        helpers["stop_process"](daemon)
    print("PASS: private cwd resources cleaned", flush=True)


if __name__ == "__main__":
    main()
