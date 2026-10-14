#!/usr/bin/env python3
"""host.py: GPU-less stand-in for the host terminal, for measuring herdr's relay alone.

Runs CMD in a pty of COLSxROWS cells at CWxCH px, answers the queries herdr makes (DA1, CSI 14t/16t/18t,
XTVERSION, DECRQM, Kitty a=q), consumes Kitty t=t temp files like Ghostty does (read, then unlink), and
writes per-second accounting to OUT (JSON lines): bytes from herdr, Kitty inline payload bytes, t=t file
bytes, upload count, placement count. It draws nothing, so it says nothing about rendering.
usage: host.py OUT COLS ROWS CW CH -- CMD...
"""
import base64, fcntl, json, os, pty, re, select, struct, sys, termios, time
out, cols, rows, cw, ch = sys.argv[1], *map(int, sys.argv[2:6])
cmd = sys.argv[sys.argv.index("--") + 1:]
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM_PROGRAM"] = "ghostty"; os.environ["TERM"] = "xterm-ghostty"
    os.execvp(cmd[0], cmd)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * cw, rows * ch))
pat = re.compile(rb"\x1b\[0?c|\x1b\[(14|16|18)t|\x1b_G([^;\x1b]*)(?:;([^\x1b]*))?\x1b\\|\x1b\[>0?q|\x1b\[\?(\d+)\$p")
f = open(out, "w"); buf = b""; sec = int(time.time())
acc = dict(bytes=0, inline_payload=0, file_bytes=0, uploads=0, placements=0, deletes=0)
def flush(now):
    global sec, acc
    f.write(json.dumps(dict(t=sec, **acc)) + "\n"); f.flush()
    sec = int(now); acc = {k: 0 for k in acc}
while True:
    r, _, _ = select.select([fd], [], [], 0.5)
    now = time.time()
    if int(now) != sec: flush(now)
    if not r: continue
    try: d = os.read(fd, 1 << 20)
    except OSError: break
    if not d: break
    acc["bytes"] += len(d); buf += d
    pos = 0
    for m in pat.finditer(buf):
        pos = m.end(); g = m.group(0)
        if g.startswith(b"\x1b[") and g.endswith(b"c"): os.write(fd, b"\x1b[?62;22;52c")
        elif m.group(1) == b"14": os.write(fd, f"\x1b[4;{rows*ch};{cols*cw}t".encode())
        elif m.group(1) == b"16": os.write(fd, f"\x1b[6;{ch};{cw}t".encode())
        elif m.group(1) == b"18": os.write(fd, f"\x1b[8;{rows};{cols}t".encode())
        elif g.startswith(b"\x1b[>"): os.write(fd, b"\x1bP>|ghostty 1.3.1\x1b\\")
        elif m.group(4): os.write(fd, b"\x1b[?" + m.group(4) + b";2$y")
        elif m.group(2) is not None:
            kv = dict(x.split("=", 1) for x in m.group(2).decode().split(",") if "=" in x)
            a, t, payload = kv.get("a", "t"), kv.get("t", "d"), m.group(3) or b""
            if t in ("t", "f") and payload:
                try:
                    path = base64.b64decode(payload).decode()
                    acc["file_bytes"] += os.path.getsize(path)
                    if t == "t" and ("tty-graphics-protocol" in path): os.unlink(path)
                except Exception: pass
            elif payload: acc["inline_payload"] += len(payload)
            if "a" in kv and a in ("t", "T"): acc["uploads"] += 1
            if a in ("p", "T"): acc["placements"] += 1
            if a == "d": acc["deletes"] += 1
            if "i" in kv and a == "q": os.write(fd, f"\x1b_Gi={kv['i']};OK\x1b\\".encode())
    buf = buf[pos:] if pos else buf[-(1 << 16):]
flush(time.time())
