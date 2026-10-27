#!/usr/bin/env python3
"""Real private-PTY checks for compact TUI, OSC11, authoritative history and clipping.

Build: cargo build -p homeway-cli
Run: python3 tools/tui-usability-smoke.py [--bin target/debug/homeway]
Requires pyte, as does tui-smoke.py. No production state or clipboard is accessed.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import re
import runpy
import shlex
import signal
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parent.parent
HELPERS = runpy.run_path(str(REPO / "tools/tui-smoke.py"))
BaseTui = HELPERS["Tui"]
sessions = HELPERS["sessions"]
wait_until = HELPERS["wait_until"]
stop_process = HELPERS["stop_process"]
wcwidth = HELPERS["wcwidth"]
SNAPSHOT_DIR = None

def shot(tui, name, light=False):
    if SNAPSHOT_DIR is not None: HELPERS["snapshot"](tui, SNAPSHOT_DIR, name, light=light)


class ProbeTui(BaseTui):
    """Answer only exact outer OSC11 queries, with controllable BEL/ST replies."""
    def __init__(self, binary, state):
        self.probe_tail = b""
        self.armed = None
        self.default_reply = b"\x1b]11;rgb:0000/0000/0000\x1b\\"
        self.query_count = 0
        super().__init__(binary, state, background=None)

    def drain(self, seconds=0.05):
        before = len(self.output)
        super().drain(seconds)
        pending = self.probe_tail + self.output[before:]
        queries = (b"\x1b]11;?\x1b\\", b"\x1b]11;?\x07")
        while True:
            found = [(pending.find(query), query) for query in queries if query in pending]
            if not found:
                break
            index, query = min(found)
            pending = pending[index+len(query):]
            reply = self.armed if self.armed is not None else self.default_reply
            self.armed = None
            self.query_count += 1
            self.send(reply)
        self.probe_tail = pending[-16:]

    def arm(self, reply, default):
        self.drain()
        self.armed = reply
        self.default_reply = default
        self.send(b"\x1b[I")  # Focus-in asks the sole event reader to query again.


def sidebar(tui):
    return min(max(tui.screen.columns//3, 20), 38)


def body_rows(tui):
    rows = []
    for y in range(1, tui.screen.lines-2):
        data = []
        x = sidebar(tui)
        while x < tui.screen.columns-1:
            symbol = tui.screen.buffer[y][x].data or " "
            data.append(symbol)
            x += 2 if wcwidth(symbol[0]) == 2 else 1
        rows.append("".join(data))
    return rows


def body(tui):
    return "\n".join(body_rows(tui))


def footer(tui):
    return tui.screen_lines()[-1]


def pump_until(tuis, predicate, description, timeout=20):
    def tick():
        for tui in tuis:
            tui.drain(0.02)
            if tui.process.poll() is not None:
                raise AssertionError(f"TUI exited {tui.process.returncode} during {description}:\n{tui.screen_text()}")
        return predicate()
    try:
        return wait_until(tick, description, timeout)
    except AssertionError as error:
        raise AssertionError(f"{error}\n" + "\n".join(tui.screen_text() for tui in tuis)) from error


def command(tui, value, tag, tuis=None):
    tui.send(value + "; printf 'US_%s\\n' " + shlex.quote(tag) + "\r")
    pump_until(tuis or [tui], lambda: "US_"+tag in body(tui), tag)


def python_command(source):
    encoded=base64.b64encode(source.encode()).decode("ascii")
    loader="import base64;exec(base64.b64decode("+repr(encoded)+"))"
    return shlex.quote(sys.executable) + " -c " + shlex.quote(loader)


def wheel(tui, up=True, shift=False):
    button = (64 if up else 65) + (4 if shift else 0)
    tui.send(f"\x1b[<{button};{sidebar(tui)+8};8M")


def theme_is(tui, color):
    return tui.screen.buffer[0][tui.screen.columns-1].bg == color


def appearance_check(tui, state):
    capture = state / "appearance-input.hex"
    ready_file = state / "appearance-reader-ready"
    paste = b"\x1b]11;rgb:abcd/abcd/abcd\x07"
    expected = [b"p"+paste+b"q", b"rs", b"\x1b]x"]
    ready = b"\r\nAPPEARANCE_READER_READY\r\n"
    program = f"""
import json,os,select,termios,time,tty
old=termios.tcgetattr(0)
os.write(1,b'\\x1b[?1000l\\x1b[?1002l\\x1b[?1003l\\x1b[?1004l\\x1b[?2004l')
try:
 tty.setraw(0)
 open({str(ready_file)!r},'w').write('ready')
 os.write(1,bytes.fromhex({ready.hex()!r}))
 for index,count in enumerate({[len(value) for value in expected]!r}):
  result=b''
  deadline=time.monotonic()+12
  while len(result)<count:
   if time.monotonic()>=deadline: raise RuntimeError('input capture timeout')
   if select.select([0],[],[],.1)[0]: result+=os.read(0,count-len(result))
  with open({str(capture)!r},'a') as output: output.write(result.hex()+'\\n')
  os.write(1,('\\r\\nAPPEARANCE_STEP_%d\\r\\n'%index).encode())
finally:
 termios.tcsetattr(0,termios.TCSANOW,old)
"""
    tui.send(python_command(program)+"\r")
    pump_until([tui], lambda: ready_file.exists() and "APPEARANCE_READER_READY" in body(tui), "input capture ready")
    light = b"\x1b]11;rgb:ffff/ffff/ffff\x07"
    # Valid replies are atomic byte sequences. A neighboring Paste event must
    # retain its literal OSC-looking contents, while adjacent keys keep order.
    mixed = b"p" + light + b"\x1b[200~" + paste + b"\x1b[201~q"
    tui.arm(mixed, light)
    pump_until([tui], lambda: theme_is(tui,"f8fafd") and capture.exists()
               and len(capture.read_text().splitlines()) >= 1, "BEL theme plus adjacent keys/paste")
    dark = b"\x1b]11;rgb:0000/0000/0000\x1b\\"
    tui.arm(b"r"+dark+b"s", dark)
    pump_until([tui], lambda: theme_is(tui,"12161d")
               and len(capture.read_text().splitlines()) >= 2, "ST theme plus adjacent keys")
    tui.send(b"\x1b]x")
    pump_until([tui], lambda: len(capture.read_text().splitlines()) == 3, "unmatched OSC prefix")
    actual = [bytes.fromhex(line) for line in capture.read_text().splitlines()]
    assert actual == expected, f"OSC11 swallowed user input or leaked report bytes: {actual!r}"
    pump_until([tui], lambda: "APPEARANCE_STEP_2" in body(tui), "reader restored normal shell")
    print("PASS: auto OSC11 BEL/ST, adjacent keys, literal paste and unmatched Alt+] preserve input")


def application_theme_queries(tui, state):
    for label, screen_color, reply, expected in [
        ("light", "f8fafd", b"\x1b]11;rgb:ffff/ffff/ffff\x07", b"\x1b]11;rgb:f8f8/fafa/fdfd"),
        ("dark", "12161d", b"\x1b]11;rgb:0000/0000/0000\x1b\\", b"\x1b]11;rgb:1212/1616/1d1d"),
    ]:
        tui.arm(reply, reply)
        pump_until([tui], lambda: theme_is(tui, screen_color), label+" outer background detected")
        result_file=state/("program-osc11-"+label+".hex")
        source=f"""
import os,select,termios,time,tty
old=termios.tcgetattr(0)
try:
 tty.setraw(0)
 os.write(1,bytes.fromhex('1b5d31313b3f07'))
 result=b''
 deadline=time.monotonic()+6
 while not (result.endswith(bytes([7])) or result.endswith(bytes.fromhex('1b5c'))):
  if time.monotonic()>deadline: raise RuntimeError('program OSC11 query timeout')
  if select.select([0],[],[],.1)[0]: result+=os.read(0,128)
 open({str(result_file)!r},'w').write(result.hex())
finally:
 termios.tcsetattr(0,termios.TCSANOW,old)
"""
        command(tui,python_command(source),"PROGRAM_THEME_"+label.upper())
        answer=bytes.fromhex(result_file.read_text())
        assert answer.startswith(expected) and answer.endswith((b"\x07",b"\x1b\\")), f"{label} application saw wrong active theme: {answer!r}"
    print("PASS: actual session program OSC11 queries follow active light/dark TUI theme")


def history_check(tui, state):
    # Clear earlier transcript so Home has an exact known absolute first row.
    history_ready, history_release, appended = state/"history-ready", state/"history-release", state/"append-done"
    program = f"""
import os,pathlib,time
os.write(1,b'\\x1b[3J\\x1b[2J\\x1b[H')
for i in range(180): print('HIST_%03d_'%i+'x'*25,flush=True)
pathlib.Path({str(history_ready)!r}).touch()
deadline=time.monotonic()+20
while not pathlib.Path({str(history_release)!r}).exists():
 if time.monotonic()>deadline: raise RuntimeError('history release timeout')
 time.sleep(.02)
print('APPENDED_'+'LIVE',flush=True)
pathlib.Path({str(appended)!r}).touch()
"""
    tui.send(python_command(program)+"\r")
    pump_until([tui], lambda: history_ready.exists() and "HIST_179_" in body(tui), "history tail visible")

    def first_history():
        match = re.search(r"HIST_(\d{3})_", body(tui))
        return int(match.group(1)) if match else None

    tail_first = first_history()
    wheel(tui)
    pump_until([tui], lambda: first_history() is not None and first_history() < tail_first,
               "ordinary wheel uses local history when application mouse is off")
    wheel_first = first_history()
    tui.send(b"\x1b[5;2~")  # Shift+PageUp
    pump_until([tui], lambda: first_history() is not None and first_history() < wheel_first,
               "Shift+PageUp moves a page")
    tui.send(b"\x1b[1;2H")  # Shift+Home
    pump_until([tui], lambda: "HIST_000_" in body(tui), "Home fetches authoritative oldest rows")
    shot(tui,"history-oldest")
    frozen = body_rows(tui)
    history_release.touch()  # External output, without a user key that intentionally resumes live follow.
    pump_until([tui], appended.exists, "new output produced while history is anchored")
    tui.drain(0.2)
    assert body_rows(tui) == frozen, "new output moved or rewrote an absolute history anchor"
    tui.send(b"\x1b[1;2F")  # Shift+End
    pump_until([tui], lambda: "APPENDED_LIVE" in body(tui) and "↑" not in footer(tui),
               "End returns to live output")
    shot(tui,"history-live")
    print("PASS: wheel/page/Home FETCH history, stable anchor during output, End resumes live follow")

    # The application receives plain wheel when mouse reporting is enabled.
    mouse_capture = state / "mouse-input.hex"
    program = f"""
import os,select,termios,time,tty
old=termios.tcgetattr(0)
try:
 tty.setraw(0)
 os.write(1,b'\\x1b[?1000h\\x1b[?1006h\\r\\n'+bytes.fromhex({b'MOUSE_READER_READY'.hex()!r})+b'\\r\\n')
 result=b''
 deadline=time.monotonic()+12
 while not result.endswith(b'M'):
  if time.monotonic()>=deadline: raise RuntimeError('mouse capture timeout')
  if select.select([0],[],[],.1)[0]: result+=os.read(0,1)
 open({str(mouse_capture)!r},'w').write(result.hex())
finally:
 os.write(1,b'\\x1b[?1000l\\x1b[?1006l')
 termios.tcsetattr(0,termios.TCSANOW,old)
"""
    tui.send(python_command(program)+"\r")
    pump_until([tui], lambda: "MOUSE_READER_READY" in body(tui), "application mouse mode enabled")
    wheel(tui, shift=True)
    pump_until([tui], lambda: "↑" in footer(tui), "Shift+wheel forces local history")
    assert not mouse_capture.exists(), "Shift+wheel leaked to the application"
    tui.send(b"\x1b[1;2F")
    wheel(tui)
    pump_until([tui], mouse_capture.exists, "ordinary wheel reaches mouse-reporting application")
    actual = bytes.fromhex(mouse_capture.read_text())
    assert actual.startswith(b"\x1b[<64;") and actual.endswith(b"M"), f"wrong mouse event: {actual!r}"
    print("PASS: application mouse wheel passthrough and Shift+wheel local override")

    alt_ready, alt_release = state/"alt-ready", state/"alt-release"
    program = f"""
import os,time,pathlib
os.write(1,bytes.fromhex({(b'\x1b[?1049h\x1b[2J\x1b[HALT_ONLY_SCREEN\r\n').hex()!r}))
pathlib.Path({str(alt_ready)!r}).touch()
deadline=time.monotonic()+12
while not pathlib.Path({str(alt_release)!r}).exists():
 if time.monotonic()>deadline: raise RuntimeError('alternate-screen timeout')
 time.sleep(.02)
os.write(1,bytes.fromhex({(b'\x1b[?1049l\r\nALT_EXIT_DONE\r\n').hex()!r}))
"""
    tui.send(python_command(program)+"\r")
    pump_until([tui], lambda: alt_ready.exists() and "ALT_ONLY_SCREEN" in body(tui), "alternate screen entered")
    tui.send(b"\x1b[1;2H")
    wheel(tui,shift=True)
    tui.drain(0.2)
    assert "ALT_ONLY_SCREEN" in body(tui) and "HIST_" not in body(tui), "primary history leaked into alternate screen"
    alt_release.touch()
    pump_until([tui], lambda: "ALT_EXIT_DONE" in body(tui), "primary screen restored")
    tui.send(b"\x1b[1;2H")
    pump_until([tui], lambda: "HIST_000_" in body(tui), "primary history remains after alternate screen")
    command(tui, "printf '\\033[3J\\033[2J\\033[HEPOCH_%s\\n' NEW", "EPOCH_READY")
    tui.send(b"\x1b[1;2H")
    tui.drain(0.2)
    assert "EPOCH_NEW" in body(tui) and "HIST_000_" not in body(tui), "stale epoch history was reused"
    print("PASS: alternate screen isolation and history-epoch reset")


def clipping_check(binary, state, tui, tuis):
    observer = ProbeTui(binary,state)
    tuis.append(observer)
    observer.set_size(80,24)
    observer.signal(signal.SIGWINCH)
    name = sessions(binary,state)[0]["name"]
    pump_until([tui,observer],lambda: name in observer.screen_text(),"observer lists existing session")
    before = sessions(binary,state)[0]["cols"]
    observer.send(b"\x02s\x1b[B\r")
    pump_until([tui,observer],lambda: "已接入" in footer(observer),"observer attached without input")
    assert sessions(binary,state)[0]["cols"] == before, "opening observer changed authoritative columns"
    width = observer.screen.columns-sidebar(observer)-1
    assert before > width+1, "clipping fixture needs an authoritative row wider than the observer"
    edge = "EDGE_"+"x"*(width-6)
    program = f"import os;os.write(1,b'\\x1b[2J\\x1b[H');print({edge!r}+'中W');print('CLIP_'+'NEXT_ROW')"
    command(tui,python_command(program),"CLIP_READY",[tui,observer])
    pump_until([tui,observer],lambda:"CLIP_NEXT_ROW" in body(observer),"observer receives authoritative clipped rows")
    rows=body_rows(observer)
    index=next(i for i,row in enumerate(rows) if row.startswith("EDGE_"))
    assert rows[index].endswith(" "), "wide glyph crossing the right edge must leave a blank tail"
    assert "中" not in rows[index] and "W" not in rows[index], "offscreen glyph leaked into the clipped row"
    assert rows[index+1].startswith("CLIP_NEXT_ROW"), "display cropped content was incorrectly reflowed"
    assert sessions(binary,state)[0]["cols"] == before, "passive observer geometry stole the active session"
    observer.send(b"\x02q")
    observer.wait_exit()
    observer.check_restored()
    shot(observer,"wide-authority-cropped")
    print("PASS: observer preserves authoritative columns; right-edge wide glyph clips without reflow")


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin",type=Path,default=REPO/"target/debug/homeway")
    parser.add_argument("--phase",choices=["all","appearance","history","clipping"],default="all")
    parser.add_argument("--snapshot-dir",type=Path)
    args=parser.parse_args()
    global SNAPSHOT_DIR
    SNAPSHOT_DIR = args.snapshot_dir
    binary=args.bin.resolve()
    tuis=[]
    daemon=None
    with tempfile.TemporaryDirectory(prefix="homeway-tui-usability-",dir="/tmp") as temporary:
        state=Path(temporary)
        (state/"config.toml").write_text("[serve]\nenabled = false\n[relay]\nenabled = false\n")
        with (state/"daemon.log").open("wb") as log:
            try:
                daemon=subprocess.Popen([str(binary),"--state",str(state)],stdout=log,stderr=log)
                wait_until(lambda:(state/"local-term.sock").exists() and (state/"control.sock").exists(),"private sockets")
                tui=ProbeTui(binary,state)
                tuis.append(tui)
                pump_until([tui],lambda:"主机" in tui.screen_lines()[0],"compact first-row panels")
                first=tui.screen_lines()[0]
                assert "homeway" not in first and "主题" not in first and "终端" in first
                assert "┘" in tui.screen_lines()[-2] and "^B" in footer(tui), "footer must occupy exactly one line"
                tui.send(b"\x02n")
                pump_until([tui],lambda:"已接入" in footer(tui),"new shell attached")
                assert len(sessions(binary,state)) == 1
                print("PASS: compact panel-first layout, one-line footer, no automatic extra shell")
                if args.phase in ("all","appearance"):
                    appearance_check(tui,state)
                    application_theme_queries(tui,state)
                if args.phase in ("all","history"): history_check(tui,state)
                if args.phase in ("all","clipping"): clipping_check(binary,state,tui,tuis)
                tui.send(b"\x02q")
                tui.wait_exit()
                tui.check_restored()
                assert len(sessions(binary,state)) == 1
            finally:
                diagnostic={
                    "appearance_capture": (state/"appearance-input.hex").read_text() if (state/"appearance-input.hex").exists() else None,
                    "reader_ready": (state/"appearance-reader-ready").exists(),
                    "tuis": [{"queries":tui.query_count,"background":tui.screen.buffer[0][tui.screen.columns-1].bg,
                              "exit":tui.process.poll()} for tui in tuis],
                }
                Path("/tmp/homeway-tui-usability-diagnostics.json").write_text(json.dumps(diagnostic,indent=2))
                cleanup_errors=[]
                try:
                    for tui in reversed(tuis):
                        try:
                            tui.close()
                        except Exception as error:
                            cleanup_errors.append(error)
                finally:
                    stop_process(daemon)
                if cleanup_errors:
                    raise RuntimeError(f"PTY cleanup failed: {cleanup_errors!r}")
    print("PASS: private usability smoke complete; all owned resources cleaned")


if __name__=="__main__":
    main()
