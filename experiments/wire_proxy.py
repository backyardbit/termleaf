import os, pty, re, select, sys, time, tty, fcntl, termios, struct, signal

log = open(os.environ["WIRE_LOG"], "a", buffering=1)
pid, fd = pty.fork()
if pid == 0:
    os.execvp(sys.argv[1], sys.argv[1:])


def sync_size(*_):
    size = fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, size)
    try:
        os.kill(pid, signal.SIGWINCH)
    except OSError:
        pass


sync_size()
signal.signal(signal.SIGWINCH, sync_size)
tty.setraw(0)
controls = re.compile(rb"\x1b_G([^;\x1b]*)")
while True:
    try:
        ready, _, _ = select.select([0, fd], [], [])
    except InterruptedError:
        continue
    if fd in ready:
        try:
            data = os.read(fd, 1 << 20)
        except OSError:
            break
        if not data:
            break
        found = controls.findall(data)
        heads = [c.decode(errors="replace") for c in found if not c.startswith(b"m=")]
        log.write(f"{time.monotonic():.4f} out {len(data)} {len(found)} {' | '.join(heads)[:600]}\n")
        view = memoryview(data)
        while view:
            written = os.write(1, view)
            view = view[written:]
    if 0 in ready:
        data = os.read(0, 1 << 16)
        if not data:
            break
        if b"\x1b_G" in data:
            log.write(f"{time.monotonic():.4f} in {data[:200]!r}\n")
        os.write(fd, data)
