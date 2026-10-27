#!/usr/bin/env python3
"""Real TUI -> private exit -> SOCKS/terminal smoke; no production state or clipboard.

Build first: cargo build -p homeway-cli
Run: python3 tools/tui-remote-smoke.py [--bin target/debug/homeway]
Requires the same pyte dependency as tui-smoke.py. The exit is started and stopped
through tools/local-rust-exit.sh. All script-owned state is removed on completion.
"""

import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import runpy
import secrets
import shlex
import shutil
import socket
import struct
import subprocess
import tempfile
import threading


REPO = Path(__file__).resolve().parent.parent
helpers = runpy.run_path(str(REPO / "tools/tui-smoke.py"))
Tui = helpers["Tui"]
wait_until = helpers["wait_until"]
stop_process = helpers["stop_process"]
sessions = helpers["sessions"]
EXIT_TOOL = REPO / "tools/local-rust-exit.sh"


def cli(binary, state, *args):
    result = subprocess.run(
        [str(binary), *args, "--state", str(state)],
        cwd=REPO, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25,
    )
    if result.returncode:
        raise AssertionError(f"private CLI {args[:2]} failed: {result.stderr.decode(errors='replace')[-800:]}")
    return result.stdout


def screen_wait(tui, text, timeout=25):
    def seen():
        tui.drain()
        if tui.process.poll() is not None:
            raise AssertionError(f"private TUI exited: {tui.process.returncode}")
        return text in tui.screen_text()
    try:
        wait_until(seen, f"current TUI screen {text!r}", timeout)
    except AssertionError as error:
        raise AssertionError(f"{error}\n" + tui.screen_text()) from error


def claim_exit():
    for _ in range(100):
        instance = 1000 + secrets.randbelow(9000)
        state = Path(f"/tmp/homeway-rs-rustexit-{instance}")
        if state.exists():
            continue
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
            try:
                probe.bind(("127.0.0.1", 42651 + instance))
            except PermissionError:
                raise
            except OSError:
                continue
        try:
            state.mkdir(mode=0o700)
        except FileExistsError:
            continue
        # Also lets the existing term CLI identify this standalone exit as local.
        (state / "config.toml").write_text("[serve]\nenabled = true\n[relay]\nenabled = false\n")
        return instance, state
    raise AssertionError("could not reserve a private exit instance")


@contextlib.contextmanager
def mock_clipboard_environment(directory):
    capture = directory / "clipboard.txt"
    helpers_dir = directory / "helpers"
    helpers_dir.mkdir()
    for name in ("pbcopy", "wl-copy", "xclip", "xsel"):
        command = helpers_dir / name
        command.write_text("#!/bin/sh\ncat > " + shlex.quote(str(capture)) + "\n")
        command.chmod(0o700)
    names = ("PATH", "SSH_CONNECTION", "SSH_TTY")
    previous = {name: os.environ.get(name) for name in names}
    os.environ["PATH"] = str(helpers_dir) + os.pathsep + previous.get("PATH", "")
    os.environ.pop("SSH_CONNECTION", None)
    os.environ.pop("SSH_TTY", None)
    try:
        yield capture
    finally:
        for name, value in previous.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value


def receive(sock, size):
    out = bytearray()
    while len(out) < size:
        chunk = sock.recv(size - len(out))
        if not chunk:
            raise AssertionError("unexpected EOF during private SOCKS round trip")
        out.extend(chunk)
    return bytes(out)


def unused_udp_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_clipboard(tui, capture, prefix):
    def copied():
        tui.drain()
        return capture.exists() and capture.read_text().startswith(prefix)
    wait_until(copied, "private mock clipboard delivery", 25)


def role_state(binary, state, role):
    status = json.loads(cli(binary, state, "status", "--json"))
    return next(entry["state"] for entry in status["roles"] if entry["name"] == role)


def local_roles(tui, binary, state, clipboard):
    tui.send(b"\x02n")
    screen_wait(tui, "已接入")
    tui.send("printf 'HW_LOCAL_%s\\n' ROLES\r")
    screen_wait(tui, "HW_LOCAL_ROLES")
    local_name = sessions(binary, state)[0]["name"]
    tui.send(b"\x02h\x02m")
    screen_wait(tui, "启动出口")
    clipboard.unlink(missing_ok=True)
    tui.send(b"\r")
    wait_clipboard(tui, clipboard, "hmw2")
    assert role_state(binary, state, "serve") == "running"
    tui.send(b"\x02m")
    screen_wait(tui, "关闭出口")
    tui.send(b"\r")
    wait_until(lambda: role_state(binary, state, "serve") == "stopped", "TUI stops private exit")
    assert [entry["name"] for entry in sessions(binary, state)] == [local_name], "serve stop killed the local shell"
    print("PASS: TUI local exit start copies private token; exit stop preserves local shell")

    tui.send(b"\x02m")
    screen_wait(tui, "启动中继")
    clipboard.unlink(missing_ok=True)
    tui.send(b"\x1b[B\r")
    wait_clipboard(tui, clipboard, "rl1")
    assert role_state(binary, state, "relay") == "running"
    tui.send(b"\x02m")
    screen_wait(tui, "关闭中继")
    tui.send(b"\x1b[B\r")
    wait_until(lambda: role_state(binary, state, "relay") == "stopped", "TUI stops private relay")
    assert [entry["name"] for entry in sessions(binary, state)] == [local_name]
    print("PASS: TUI local relay start copies private token; relay stop preserves local shell")
    return local_name


def socks_echo(proxy_port):
    errors = []
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(10)
        echo_port = listener.getsockname()[1]

        def echo():
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(10)
                    while True:
                        data = connection.recv(65536)
                        if not data:
                            break
                        connection.sendall(data)
            except Exception as error:
                errors.append(error)

        worker = threading.Thread(target=echo, daemon=True)
        worker.start()
        with socket.create_connection(("127.0.0.1", proxy_port), timeout=10) as connection:
            connection.settimeout(10)
            connection.sendall(b"\x05\x01\x00")
            assert receive(connection, 2) == b"\x05\x00", "SOCKS no-auth handshake failed"
            connection.sendall(b"\x05\x01\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", echo_port))
            response = receive(connection, 4)
            assert response[:2] == b"\x05\x00", f"SOCKS CONNECT failed: {response!r}"
            if response[3] == 1:
                receive(connection, 6)
            elif response[3] == 4:
                receive(connection, 18)
            elif response[3] == 3:
                receive(connection, receive(connection, 1)[0] + 2)
            else:
                raise AssertionError("unknown SOCKS bound address family")
            payload = bytes(range(256)) * 256
            connection.sendall(payload)
            assert receive(connection, len(payload)) == payload, "SOCKS tunnel changed bytes"
        worker.join(timeout=12)
        assert not worker.is_alive(), "private echo worker did not stop"
        assert not errors, errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=REPO / "target/debug/homeway")
    args = parser.parse_args()
    binary = args.bin.resolve()
    if not binary.is_file():
        parser.error("build the CLI binary first")
    if binary.name != "homeway":
        parser.error("private exit lifecycle helper requires the binary basename homeway")
    instance, exit_state = claim_exit()
    exit_env = os.environ.copy()
    exit_env["HOMEWAY_BIN"] = str(binary)
    daemon = tui = None
    client_workspace = None
    try:
        started = subprocess.run([str(EXIT_TOOL), "start", str(instance)], cwd=REPO, env=exit_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=40)
        assert started.returncode == 0, f"private exit tool failed: {started.stderr.decode(errors='replace')[-800:]}"
        revealed = cli(binary, exit_state, "serve", "token")
        match = re.search(rb"hmw[0-9][A-Za-z0-9+/=_-]+", revealed)
        assert match is not None, "private exit returned no token"
        token = match.group().decode()
        # Keep sockets/state until the finally block has stopped their owner.
        with contextlib.nullcontext(tempfile.mkdtemp(prefix="hw-tui-remote-", dir="/tmp")) as temporary:
            directory = Path(temporary)
            client_workspace = directory
            state = directory / "client"
            state.mkdir()
            serve_port = unused_udp_port()
            relay_port = unused_udp_port()
            while relay_port == serve_port:
                relay_port = unused_udp_port()
            (state / "config.toml").write_text(
                f'[serve]\nenabled = false\nlisten = {serve_port-1}\nquic_listen = {serve_port}\n'
                f'bind_interface = "127.0.0.1"\nupnp = false\nstun = ""\nstun6 = ""\n'
                f'public_endpoint = "127.0.0.1:{serve_port}"\n'
                f'[relay]\nenabled = false\nlisten = "127.0.0.1:{relay_port}"\nadvertise = "127.0.0.1:{relay_port}"\n'
            )
            with (state / "daemon.log").open("wb") as log:
                daemon_env = os.environ.copy()
                daemon_env.pop("HOMEWAY_TERM", None)
                daemon = subprocess.Popen([str(binary), "--state", str(state)], cwd=REPO, env=daemon_env, stdout=log, stderr=log)
                wait_until(lambda: (state / "control.sock").exists() and (state / "local-term.sock").exists(), "private client sockets")
                with mock_clipboard_environment(directory) as clipboard:
                    tui = Tui(binary, state)
                    screen_wait(tui, "想在这台主机上做些什么？")
                    tui.send(b"\x02a")
                    screen_wait(tui, "添加主机")
                    tui.send("tui-remote\t")
                    tui.send(b"\x1b[200~" + token.encode() + b"\x1b[201~")
                    tui.send(b"\r")
                    screen_wait(tui, "tui-remote")
                    hosts = json.loads(cli(binary, state, "host", "list", "--json"))["hosts"]
                    assert len(hosts) == 1
                    assert token.encode() not in tui.output, "add-host dialog exposed its token"
                    tui.send(b"\x02h\x1b[B")
                    tui.drain(0.2)
                    tui.send(b"\x02n")
                    screen_wait(tui, "已接入")
                    tui.send("printf 'HW_REMOTE_%s\\n' SHELL\r")
                    screen_wait(tui, "HW_REMOTE_SHELL")
                    remote_sessions = sessions(binary, exit_state)
                    assert len(remote_sessions) == 1
                    session_name = remote_sessions[0]["name"]
                    print("PASS: TUI add-host dialog, select remote host, create shell and execute marker")

                    tui.send(b"\x02h\x02m")
                    screen_wait(tui, "启动代理")
                    tui.send(b"\r")
                    wait_until(lambda: clipboard.exists() and clipboard.read_text().startswith("socks5h://"), "mock system clipboard SOCKS URL")
                    copied = clipboard.read_text().strip()
                    proxy = re.fullmatch(r"socks5h://127\.0\.0\.1:(\d+)", copied)
                    assert proxy, "TUI did not copy a usable SOCKS URL"
                    socks_echo(int(proxy[1]))
                    print("PASS: TUI starts SOCKS, mock clipboard captures actual URL, 65,536 bytes echo through QUIC")

                    tui.drain(0.2)
                    tui.send(b"\x02m")
                    screen_wait(tui, "关闭代理")
                    tui.send(b"\r")
                    def proxy_off():
                        tui.drain()
                        return all(not entry["on"] for entry in json.loads(cli(binary, state, "socks", "status", "--json")))
                    wait_until(proxy_off, "TUI stops SOCKS")
                    try:
                        unexpected = socket.create_connection(("127.0.0.1", int(proxy[1])), timeout=1)
                    except OSError:
                        pass
                    else:
                        unexpected.close()
                        raise AssertionError("SOCKS listener survived the TUI stop action")
                    tui.send(b"\x02m")
                    screen_wait(tui, "删除主机")
                    tui.send(b"\x1b[B\r")
                    def host_removed():
                        tui.drain()
                        return json.loads(cli(binary, state, "host", "list", "--json"))["hosts"] == []
                    wait_until(host_removed, "TUI deletes private remote host")
                    screen_wait(tui, "主机 1")
                    screen_wait(tui, "想在这台主机上做些什么？")
                    assert [entry["name"] for entry in sessions(binary, exit_state)] == [session_name], "deleting a host killed its remote terminal"
                    print("PASS: TUI stops proxy/deletes host, exit CLI LIST confirms remote shell survives")
                    local_name = local_roles(tui, binary, state, clipboard)
                    tui.send(b"\x02q")
                    tui.wait_exit()
                    tui.check_restored()
                    assert tui.process.returncode == 0
                    cli(binary, exit_state, "term", "delete", session_name)
                    cli(binary, state, "term", "delete", local_name)
                    print("PASS: TUI detach restores terminal; local and remote shells cleaned through CLI")
    finally:
        if tui is not None:
            tui.close()
        stop_process(daemon)
        if client_workspace is not None:
            shutil.rmtree(client_workspace)
        stopped = subprocess.run([str(EXIT_TOOL), "stop", str(instance)], cwd=REPO, env=exit_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        if stopped.returncode:
            raise AssertionError("could not stop the script-owned private exit")
        shutil.rmtree(exit_state)
    print("PASS: private remote TUI smoke complete; only script-owned processes/state used")


if __name__ == "__main__":
    main()
