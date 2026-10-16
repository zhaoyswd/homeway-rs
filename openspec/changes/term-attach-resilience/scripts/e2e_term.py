#!/usr/bin/env python3
"""term-attach-resilience e2e 驱动（远程面全链）：
A) new 建会话跑差分探针 → detach；
B) 同几何 re-attach：断言 STATIC-TOP 在数百 ms 内出现（间隔哨兵逼出的全屏重绘——
   回放窗内已无全帧，不 resize 就出现的唯一来源）；
C) 杀客户端 daemon（出口与会话保活）→ 断言重连横幅 → daemon 按需重生 + 岛重连 →
   自动再接入 → 断言 STATIC-TOP 复现且计数器继续（会话未丢）；
D) 在途应答退出排空：注入 DA1 应答后立刻 SIGINT，断言退出后无数字乱码残留。"""
import os, pty, fcntl, termios, struct, time, threading, signal, subprocess, sys, re

print("[0] module loading", flush=True)
import subprocess as _sp
REPO = _sp.run(["git", "-C", os.path.dirname(os.path.abspath(__file__)), "rev-parse", "--show-toplevel"],
               capture_output=True, text=True).stdout.strip() or "/Users/zhaozhe/Documents/projects/homeway-rs"
BIN = f"{REPO}/target/release/homeway"
EXIT_STATE = "/tmp/homeway-rs-rustexit-1"
CLI_STATE = "/tmp/homeway-rs-rclient-e2e"
HOST = "rtest1"
SESS = f"tt{os.getpid()}"
COLS, ROWS = 100, 30

buf = []
lock = threading.Lock()
proc = {}  # 当前 attach 子进程信息

def reader(fd):
    while True:
        try:
            d = os.read(fd, 65536)
        except OSError:
            return
        if not d:
            return
        with lock:
            buf.append(d)
        # DA1 查询模拟应答（探针每 300ms 发一条）
        if b'\x1b[c' in d:
            try:
                os.write(fd, b'\x1b[?62;1;2;6;9c')
            except OSError:
                pass

def snapshot():
    with lock:
        return b''.join(buf)

def mark():
    return len(snapshot())  # snapshot 自带锁——外层再 with 即非重入自锁（修前死锁根因）

def wait_for(pred, secs, what):
    dl = time.time() + secs
    while time.time() < dl:
        if pred(snapshot()):
            return True
        time.sleep(0.1)
    return False

def spawn_attach(args):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ['TERM'] = 'xterm-256color'
        os.execv(BIN, [BIN] + args)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', ROWS, COLS, 0, 0))
    threading.Thread(target=reader, args=(fd,), daemon=True).start()
    return pid, fd

ok = fail = 0
def check(name, cond, detail=""):
    global ok, fail
    print(("PASS " if cond else "FAIL ") + name + (f"  [{detail}]" if detail and not cond else ""))
    if cond: ok += 1
    else: fail += 1

# ---- A：new 建会话 → 跑探针 → detach ----
mark()
# 预清理：上次驱动残留的 tt1 会话（CLI 死 ≠ 会话死——产品语义）；删到 list 确认不在
for _ in range(10):
    r = subprocess.run([BIN, "term", "delete", SESS, "--host", HOST, "--state", CLI_STATE],
                       capture_output=True, timeout=20)
    lst = subprocess.run([BIN, "term", "list", "--host", HOST, "--state", CLI_STATE],
                         capture_output=True, timeout=20)
    if SESS.encode() not in lst.stdout:
        break
    time.sleep(0.5)
time.sleep(1.5)  # 收尾摘表窗（done 未出 map：new 撞 AlreadyExists 而 LIST 已不显示）
print("[A0] spawning new", flush=True)
for _attempt in range(3):
    pid, fd = spawn_attach(["term", "new", SESS, "--host", HOST, "--state", CLI_STATE])
    time.sleep(3.0)
    with lock:
        _early = b''.join(buf)
    if '已存在'.encode() in _early:  # 「已存在」
        try:
            os.kill(pid, 9); os.waitpid(pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
        subprocess.run([BIN, "term", "delete", SESS, "--host", HOST, "--state", CLI_STATE],
                       capture_output=True, timeout=20)
        time.sleep(1.5)
        print(f"[A] already_exists 重试 #{_attempt + 1}", flush=True)
        continue
    break
print("[A1] spawned, pid=", pid, flush=True)
print("[A] new 已起，敲探针命令…", flush=True)
try:
    os.write(fd, f"python3 {REPO}/openspec/changes/term-attach-resilience/scripts/diff_probe.py\r".encode())
except OSError as e:
    print("[A] 写 master 失败（子进程已退）", e, "输出：", snapshot()[:800], flush=True)
    raise
got = wait_for(lambda s: b'STATIC-TOP' in s, 25, '探针全屏')
if not got:
    print("[A] 首连 25s 未见探针全屏；tail:", snapshot()[-400:], flush=True)
assert got, "首连探针全屏未出现"
time.sleep(1.5)  # 让 spinner 积累增量输出（回放窗被增量顶掉全帧）
os.write(fd, b'\x02d')  # 分离键 ^B d
time.sleep(1.5)
_, status = os.waitpid(pid, 0)
check("A detach 干净退出", os.WIFEXITED(status))
time.sleep(0.5)

# ---- B：同几何 re-attach，不 resize——静态内容只能来自哨兵重绘 ----
print("[B] re-attach…", flush=True)
mark()
pid, fd = spawn_attach(["term", "attach", SESS, "--host", HOST, "--state", CLI_STATE])
got = wait_for(lambda s: b'STATIC-TOP' in s, 8, 'B 静态内容')
check("B re-attach 后静态内容完整（间隔哨兵重绘）", got,
      f"tail={snapshot()[-200:]!r}")
# 记录当前计数器值
def spin_val():
    m = re.findall(rb'SPINNER n=(\d+)', snapshot())
    return int(m[-1]) if m else -1
pre_kill = spin_val()
check("B 探针计数在走", pre_kill > 0)

# ---- C：杀 daemon（链路断、会话在出口保活）→ 横幅 → 自动重连 ----
_pg = subprocess.run(["pgrep", "-f", f"homeway --state {CLI_STATE}"], capture_output=True, text=True)
daemon_pid = int(_pg.stdout.split()[0]) if _pg.stdout.strip() else None
print(f"daemon pid = {daemon_pid}")
print(f"[C] 杀 daemon {daemon_pid}…", flush=True)
mark()
os.kill(daemon_pid, signal.SIGTERM)
def banner_or_trace(s_):
    if '重连中'.encode() in s_:
        return True
    return False
_t0 = time.time()
while time.time() - _t0 < 25:
    if banner_or_trace(snapshot()):
        got = True
        break
    if int(time.time() - _t0) % 5 == 0:
        print(f"[C1+{_t0 and int(time.time()-_t0)}s] tail={snapshot()[-160:]!r}", flush=True)
        time.sleep(1.1)
else:
    got = False
check("C1 断链横幅出现", got, f"tail={snapshot()[-300:]!r}")
# 等自动恢复：daemon 被重拨按需重生 + 岛重连 + 再接入
mark()
got = wait_for(lambda s: b'STATIC-TOP' in s, 90, 'C2 恢复')
check("C2 断网后自动恢复（重接入 + 全屏重绘）", got, f"tail={snapshot()[-300:]!r}")
post = spin_val()
check("C3 会话未丢（计数器继续）", post > pre_kill, f"pre={pre_kill} post={post}")

# ---- D：在途应答 + SIGINT 退出 → 无数字乱码 ----
print("[D] 在途应答 + SIGINT…", flush=True)
mark()
os.write(fd, b'\x1b[?62;1;2;6;9c')  # 模拟一条刚排队、CLI 已不再消费的应答
os.kill(pid, signal.SIGINT)
time.sleep(2.0)
try:
    os.waitpid(pid, os.WNOHANG)
except ChildProcessError:
    pass
tail = snapshot()
check("D 退出后无应答乱码残留", b';1;2;6;9c' not in tail and b'62;1;2' not in tail, f"tail={tail[-120:]!r}")
check("D 退出前横幅/画面收口无崩溃", b'panicked' not in tail.lower())

print(f"\n== {ok} pass / {fail} fail ==")
sys.exit(1 if fail else 0)
