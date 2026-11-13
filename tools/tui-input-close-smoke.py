#!/usr/bin/env python3
"""Check terminal-loss handling using only helper-owned private processes/PTYs.

Build: cargo build -p homeway-cli
Run: python3 tools/tui-input-close-smoke.py --bin target/debug/homeway
The same command is expected to fail against unpatched crossterm 0.29.0.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]


class Viewer:
    def __init__(self, binary, state, web=False, stderr_log=None):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        env = os.environ.copy()
        env.update(TERM="xterm-256color", COLORTERM="truecolor", COLORFGBG="15;0")
        env.pop("HOMEWAY_WEB_TUI", None)
        args = [str(binary), "tui", "--state", str(state)]
        if web:
            args.append("--no-spawn")
            env["HOMEWAY_WEB_TUI"] = "1"

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        diagnostic = None
        try:
            if stderr_log:
                diagnostic = stderr_log.open("wb")
            self.child = subprocess.Popen(args, stdin=self.slave, stdout=self.slave,
                                          stderr=diagnostic if diagnostic else self.slave,
                                          env=env, preexec_fn=setup)
        except BaseException:
            self.close_fds()
            raise
        finally:
            if diagnostic:
                diagnostic.close()
        self.output = bytearray()

    def drain(self, seconds):
        deadline = time.monotonic() + seconds
        while self.master is not None and time.monotonic() < deadline:
            if select.select([self.master], [], [], min(.025, max(0, deadline-time.monotonic())))[0]:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    break
                if not data:
                    break
                self.output.extend(data)

    def send(self, data):
        # Bounded duplex pumping avoids blocking this harness behind PTY output.
        flags = fcntl.fcntl(self.master, fcntl.F_GETFL)
        fcntl.fcntl(self.master, fcntl.F_SETFL, flags | os.O_NONBLOCK)
        deadline = time.monotonic() + 8
        try:
            while data:
                assert time.monotonic() < deadline, "private PTY input stalled"
                try:
                    count = os.write(self.master, data[:4096])
                    data = data[count:]
                except BlockingIOError:
                    pass
                self.drain(.005)
        finally:
            fcntl.fcntl(self.master, fcntl.F_SETFL, flags)

    def ready(self):
        self.drain(1)
        assert self.child.poll() is None and len(self.output) > 1000, "TUI did not render"
        cc = termios.tcgetattr(self.slave)[6]
        assert cc[termios.VMIN] == 1 and cc[termios.VTIME] == 0, "unexpected raw TTY mode"

    def close_fds(self):
        errors = []
        for name in ("master", "slave"):
            fd = getattr(self, name, None)
            if fd is not None:
                # Never retry a close against a potentially reused descriptor.
                setattr(self, name, None)
                try:
                    os.close(fd)
                except OSError as error:
                    errors.append(f"{name}: {error!r}")
        if errors:
            raise RuntimeError("; ".join(errors))

    def wait_exit(self, budget=2):
        started = time.monotonic()
        while self.child.poll() is None and time.monotonic()-started < budget:
            self.drain(.025)
            time.sleep(.005)
        return {"pid": self.child.pid, "exited": self.child.poll() is not None,
                "returncode": self.child.returncode, "wait_seconds": time.monotonic()-started,
                "live_cpu": subprocess.run(["ps", "-p", str(self.child.pid), "-o", "%cpu=,time=,state="],
                                           capture_output=True, text=True).stdout.strip()}

    def cleanup(self):
        try:
            if self.child.poll() is None:
                self.child.kill()
            self.child.wait(timeout=3)
        finally:
            self.close_fds()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=ROOT / "target/debug/homeway")
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    assert binary.name == "homeway", "private helper requires binary basename homeway"
    evidence = Path(tempfile.mkdtemp(prefix="homeway-input-close-", dir="/tmp")).resolve()
    state = evidence / "state"
    state.mkdir(mode=0o700)
    (state / "config.toml").write_text("[serve]\nenabled=false\n[relay]\nenabled=false\n")
    env = os.environ.copy()
    env.update(HOMEWAY_UNIFIED="1", HOMEWAY_TEST_STATE=str(state), HOMEWAY_BIN=str(binary))
    helper = ROOT / "tools/local-rust-exit.sh"
    receipt = {"binary": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "state": str(state), "cases": [], "cleanup": {}}
    viewers = []
    print("EVIDENCE:", evidence, flush=True)

    expected_daemon = f"{binary} --state {state}"

    def daemon_command(pid):
        result = subprocess.run(["ps", "-p", str(pid), "-o", "command="],
                                capture_output=True, text=True, timeout=3)
        return result.stdout.strip() if result.returncode == 0 else None

    def cli(*words):
        return subprocess.run([str(binary), *words, "--state", str(state)],
                              capture_output=True, timeout=10, check=True).stdout

    def record(name, viewer, expected_code):
        item = {"case": name, **viewer.wait_exit()}
        item["expected_returncode"] = expected_code
        item["passed"] = item["exited"] and item["returncode"] == expected_code
        receipt["cases"].append(item)
        print(json.dumps(item), flush=True)

    try:
        started = subprocess.run([str(helper), "start", "992"], env=env,
                                 capture_output=True, timeout=30, check=True)
        (evidence / "helper-start.log").write_bytes(started.stdout + started.stderr)
        daemon_pid = int((state / "pid").read_text())
        receipt["daemon_pid"] = daemon_pid
        cli("term", "new", "input-close-survivor", "-d")
        before = json.loads(cli("term", "list", "--json"))["sessions"]
        assert len(before) == 1

        # Normal key input followed by idle must not get stuck in a blocking read.
        for sig in (signal.SIGTERM, signal.SIGHUP):
            viewer = Viewer(binary, state)
            viewers.append(viewer)
            viewer.ready()
            os.write(viewer.master, b"\x02s\x1b[B\r")
            viewer.drain(.3)
            assert any(s.get("attached") for s in json.loads(cli("term", "list", "--json"))["sessions"]), "viewer did not attach"
            os.write(viewer.master, b"printf 'INPUT_IDLE_OK\\n'\r")
            viewer.drain(.5)
            assert b"INPUT_IDLE_OK" in viewer.output, "normal key input stalled"
            if sig == signal.SIGTERM:
                # Mouse selection, resize, and a >1KiB UTF-8 Paste all use the
                # real input parser. No extra keystroke may be needed to drain it.
                viewer.send(b"\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<0;5;8M\x1b[<0;5;8m")
                fcntl.ioctl(viewer.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
                viewer.child.send_signal(signal.SIGWINCH)
                viewer.drain(.2)
                pasted = "中🙂" * 1024
                paste_file, size_file = state / "paste.txt", state / "size.txt"
                command = f"printf '%s' '{pasted}' > {paste_file}; stty size > {size_file}"
                viewer.send(b"\x1b[200~" + command.encode() + b"\x1b[201~\r")
                deadline = time.monotonic() + 8
                while not size_file.exists() and time.monotonic() < deadline:
                    viewer.drain(.05)
                assert paste_file.read_bytes() == pasted.encode(), "large UTF-8 paste differs"
                assert size_file.read_text().strip() == "34 92", "resize did not reach shell PTY"
                receipt["mouse_resize_paste"] = {"utf8_bytes": len(pasted.encode()),
                    "sha256": hashlib.sha256(paste_file.read_bytes()).hexdigest(), "pty_size": "34 92"}
            viewer.child.send_signal(sig)
            record("key-idle-" + sig.name, viewer, 0)
            viewer.drain(.05)
            assert b"\x1b[?1049l" in viewer.output and b"\x1b[?25h" in viewer.output, "terminal restoration missing"
            viewer.cleanup()

        # A partial CSI leaves crossterm inside its terminal-read loop. Closing
        # this PTY deterministically exercises the previous EOF/EIO busy loop.
        viewer = Viewer(binary, state)
        viewers.append(viewer)
        viewer.ready()
        os.write(viewer.master, b"\x1b[")
        time.sleep(.15)
        viewer.close_fds()
        record("partial-csi-pty-loss", viewer, 1)
        if viewer.child.poll() is None:
            viewer.child.send_signal(signal.SIGTERM)
            record("pty-loss-followed-by-SIGTERM", viewer, 1)
        viewer.cleanup()

        diagnostic_path = evidence / "input-error.log"
        viewer = Viewer(binary, state, stderr_log=diagnostic_path)
        viewers.append(viewer)
        viewer.ready()
        os.write(viewer.master, b"\x1b[")
        time.sleep(.15)
        viewer.close_fds()
        record("pty-loss-with-readable-diagnostic", viewer, 1)
        viewer.cleanup()
        diagnostic = diagnostic_path.read_text()
        assert "homeway tui：" in diagnostic and "panicked" not in diagnostic, diagnostic

        after = json.loads(cli("term", "list", "--json"))["sessions"]
        receipt["work_session_preserved_before_parent_exit"] = (
            len(after) == 1 and after[0]["name"] == "input-close-survivor"
            and after[0]["name"] == before[0]["name"])
        assert receipt["work_session_preserved_before_parent_exit"], "viewer exit killed work session"

        # Use the real Web input-worker selection; lose its owned control parent
        # while it is reading partial input. This never targets a production PID.
        viewer = Viewer(binary, state, web=True)
        viewers.append(viewer)
        viewer.ready()
        os.write(viewer.master, b"\x1b[")
        time.sleep(.15)
        assert daemon_command(daemon_pid) == expected_daemon, "private daemon identity changed; refusing signal"
        os.kill(daemon_pid, signal.SIGTERM)
        record("web-control-parent-exit", viewer, 1)
        viewer.cleanup()
        receipt["passed"] = all(item["passed"] for item in receipt["cases"])
    finally:
        cleanup_errors = []
        for viewer in viewers:
            try:
                viewer.cleanup()
            except BaseException as error:
                cleanup_errors.append(f"viewer {viewer.child.pid}: {error!r}")
        helper_result = None
        already_absent = False
        try:
            pidfile = state / "pid"
            private_pid = int(pidfile.read_text()) if pidfile.exists() else None
            actual = daemon_command(private_pid) if private_pid is not None else None
            if actual is None:
                already_absent = True
                pidfile.unlink(missing_ok=True)
            else:
                assert actual == expected_daemon, "private daemon identity changed; refusing helper stop"
                stopped = subprocess.run([str(helper), "stop", "992"], env=env,
                                         capture_output=True, timeout=25)
                (evidence / "helper-stop.log").write_bytes(stopped.stdout + stopped.stderr)
                helper_result = stopped.returncode
                if helper_result != 0:
                    cleanup_errors.append(f"private helper stop exit {helper_result}")
        except BaseException as error:
            cleanup_errors.append(f"private helper: {error!r}")
        receipt["cleanup"] = {"helper_exit": helper_result,
                              "helper_already_absent": already_absent,
                              "all_viewers_reaped": all(v.child.poll() is not None for v in viewers),
                              "errors": cleanup_errors}
        (evidence / "receipt.json").write_text(json.dumps(receipt, indent=2))
    assert not cleanup_errors, f"private cleanup failed: {cleanup_errors}"
    assert receipt.get("passed"), "a TUI failed to exit after terminal/control loss; see receipt"
    print("PASS: key/idle signals, partial-input PTY loss, Web control loss; all private viewers reaped", flush=True)


if __name__ == "__main__":
    main()
