#!/usr/bin/env python3
"""差分渲染 TUI 探针（term-attach-resilience e2e）：全屏画一次（静态行 + 计数行），
之后每 100ms 只重写计数行（行定位 + 行清——bubbletea 式按行差分），不重画静态内容。
SIGWINCH（哨兵两跳会触发）⇒ 全屏重画。周期发 DA1 查询（驱动侧模拟应答）。
输出标记：STATIC-TOP（静态内容在场的唯一证据——只有全屏重绘能带出来）。"""
import sys, os, time, signal, struct, fcntl, termios

def size():
    ws = struct.unpack('HHHH', fcntl.ioctl(0, termios.TIOCGWINSZ, b'\0' * 8))
    return ws[1], ws[0]  # cols, rows

def full(c, r):
    out = ['\x1b[?1049h\x1b[2J\x1b[H']
    for i in range(r):
        if i == 0:
            out.append(f'STATIC-TOP full-frame c={c} r={r}')
        elif i == r - 2:
            out.append(f'SPINNER n=0 t=0.0')
        else:
            out.append(f'static row {i:02d} ' + 'x' * max(0, c - 14))
        out.append('\x1b[K' + ('\r\n' if i < r - 1 else ''))
    sys.stdout.write(''.join(out))
    sys.stdout.flush()

n = 0
t0 = time.time()

def redraw(signum, frame):
    c, r = size()
    full(c, r)

signal.signal(signal.SIGWINCH, redraw)
c, r = size()
full(c, r)
last = (c, r)
while True:
    time.sleep(0.1)
    n += 1
    c2, r2 = size()
    if (c2, r2) != last:
        full(c2, r2)
        last = (c2, r2)
        continue
    sys.stdout.write(f'\x1b[{r2 - 1};1HSPINNER n={n} t={time.time() - t0:.1f}\x1b[K')
    sys.stdout.flush()
    if n % 3 == 0:  # DA1 查询（驱动侧模拟应答——退出排空的靶子）
        sys.stdout.write('\x1b[c')
        sys.stdout.flush()
