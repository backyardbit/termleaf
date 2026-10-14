#!/usr/bin/env python3
"""Resident-page scroll probe (throwaway, stdlib only).

Uploads two pre-rendered pages once as Kitty images, then scrolls at 60 Hz by
changing only the placement source rect (x/y/w/h) - no re-upload - and measures,
per 60 Hz frame, whether the terminal acknowledged every update issued so far
within 16.7 ms (DA1 round-trip after each update) with no Kitty error reply.

This is a PROXY for "page fully drawn at the right position": the ack proves the
terminal (or, under herdr, herdr's pane emulator) has parsed the update, not
that the GPU presented it. See README.md.

Scenarios:
  trackpad       momentum flicks, pixel-granular offset change every frame
  held_j         30 Hz page flips between the two resident pages (placement only)
  held_j_upload  30 Hz page flips that re-transmit the page PNG each press
                 (the current approach, for contrast)
"""
import argparse, base64, fcntl, json, os, re, select, struct, sys, termios, threading, time, tty, zlib

FRAME = 1.0 / 60.0
ESC = "\x1b"

def png_size(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("not a PNG")
    w, h = struct.unpack(">II", data[16:24])
    return w, h

def winsize(fd):
    rows, cols, xpx, ypx = struct.unpack("HHHH", fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8))
    return cols, rows, xpx, ypx

class Term:
    def __init__(self, fd_in, fd_out):
        self.fi, self.fo = fd_in, fd_out
        self.lock = threading.Lock()
        self.acks = []      # monotonic times of DA1 replies, in order
        self.errors = []    # (time, text)
        self.oks = []
        self.buf = b""
        self.stop = False
        self.t = threading.Thread(target=self.reader, daemon=True)

    def write(self, s):
        b = s.encode() if isinstance(s, str) else s
        mv = memoryview(b)
        while mv:
            n = os.write(self.fo, mv)
            mv = mv[n:]

    def reader(self):
        da1 = re.compile(rb"\x1b\[\?[0-9;]*c")
        apc = re.compile(rb"\x1b_G([^\x1b]*)\x1b\\")
        while not self.stop:
            r, _, _ = select.select([self.fi], [], [], 0.05)
            if not r:
                continue
            chunk = os.read(self.fi, 65536)
            now = time.monotonic()
            if not chunk:
                break
            self.buf += chunk
            while True:
                m1, m2 = da1.search(self.buf), apc.search(self.buf)
                ms = [m for m in (m1, m2) if m]
                if not ms:
                    break
                m = min(ms, key=lambda m: m.start())
                with self.lock:
                    if m is m1:
                        self.acks.append(now)
                    else:
                        body = m.group(1).decode(errors="replace")
                        (self.oks if body.endswith(";OK") else self.errors).append((now, body))
                self.buf = self.buf[m.end():]
            if len(self.buf) > 1 << 16:
                self.buf = self.buf[-4096:]

def cell_query(fi, fo, cols, rows):
    """TIOCGWINSZ has no pixels: ask CSI 16t (cell size) then DA1 as a sentinel."""
    old = termios.tcgetattr(fi)
    try:
        tty.setraw(fi)
        os.write(fo, b"\x1b[16t\x1b[c")
        buf, end = b"", time.monotonic() + 3
        while time.monotonic() < end and not re.search(rb"\x1b\[\?[0-9;]*c", buf):
            r, _, _ = select.select([fi], [], [], 0.1)
            if r:
                buf += os.read(fi, 4096)
    finally:
        termios.tcsetattr(fi, termios.TCSADRAIN, old)
    m = re.search(rb"\x1b\[6;(\d+);(\d+)t", buf)
    if not m:
        raise SystemExit("terminal reports no pixel size (TIOCGWINSZ and CSI 16t); cannot lay out")
    return cols * int(m.group(2)), rows * int(m.group(1))

def upload_cmd(img_id, png, chunk=4096):
    b64 = base64.standard_b64encode(png)
    parts = []
    for i in range(0, len(b64), chunk):
        more = 1 if i + chunk < len(b64) else 0
        ctl = f"a=t,f=100,t=d,i={img_id},q=1,m={more}" if i == 0 else f"m={more}"
        parts.append(f"{ESC}_G{ctl};".encode() + b64[i:i + chunk] + f"{ESC}\\".encode())
    return b"".join(parts)

class Layout:
    """Maps a scroll offset (in display px from the top of page 1) to placements."""
    def __init__(self, pages, cols, rows, xpx, ypx):
        self.pages = pages  # list of (img_id, w, h) in source px
        self.cols, self.rows = cols, rows
        self.cw, self.ch = xpx / cols, ypx / rows
        self.scale = xpx / pages[0][1]           # display px per source px (width-fit)
        self.view_h = rows * self.ch             # display px
        self.disp_h = [h * self.scale for _, _, h in pages]
        self.total = sum(self.disp_h)
        self.max_off = max(0.0, self.total - self.view_h)

    def placements(self, off):
        out, top, cursor = [], 0.0, 0.0
        for (img, w, h), dh in zip(self.pages, self.disp_h):
            a, b = max(off, top), min(off + self.view_h, top + dh)
            if b - a >= self.ch:  # skip slivers thinner than a cell (keeps maths integral)
                sy = int(round((a - top) / self.scale))
                sh = int((b - a) / self.scale)
                sh = max(1, min(sh, h - sy))
                d = a - off
                row = int(d // self.ch)
                yoff = int(round(d - row * self.ch))
                if yoff >= int(self.ch):
                    row, yoff = row + 1, 0
                out.append((img, row, yoff, sy, w, sh))
            top += dh
        return out

def place_cmd(img, row, yoff, sy, w, sh, cols):
    return (f"{ESC}[{row + 1};1H{ESC}_Ga=p,i={img},p=1,x=0,y={sy},w={w},h={sh},"
            f"c={cols},Y={yoff},C=1,q=1{ESC}\\")

def delete_cmd(img):
    return f"{ESC}_Ga=d,d=i,i={img},p=1,q=1{ESC}\\"

def trackpad_offsets(n, max_off, seed=1):
    """Momentum flicks: 4000 px/s kicks every 0.8 s, exp decay tau 0.35 s, bounce at ends."""
    off, v, d, out = 0.0, 0.0, 1.0, []
    for k in range(n):
        t = k * FRAME
        if k % 48 == 0:
            v = 4000.0 * d
        off += v * FRAME
        v *= pow(2.718281828, -FRAME / 0.35)
        if off >= max_off:
            off, d, v = max_off, -1.0, -abs(v)
        if off <= 0:
            off, d, v = 0.0, 1.0, abs(v)
        out.append(off)
    return out

def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))]

def run(args):
    fi, fo = sys.stdin.fileno(), sys.stdout.fileno()
    cols, rows, xpx, ypx = winsize(fo)
    if not xpx or not ypx:
        xpx, ypx = cell_query(fi, fo, cols, rows)
    pngs = [open(p, "rb").read() for p in args.pages]
    pages = [(i + 1, *png_size(d)) for i, d in enumerate(pngs)]
    lay = Layout(pages, cols, rows, xpx, ypx)
    old = termios.tcgetattr(fi)
    term = Term(fi, fo)
    res = dict(images=len(pngs), label=args.label, scenario=args.scenario, density=args.density, seconds=args.seconds,
               pane=dict(cols=cols, rows=rows, xpx=xpx, ypx=ypx), pages=[dict(w=w, h=h, png_bytes=len(d)) for (_, w, h), d in zip(pages, pngs)],
               raw_rgba_bytes_total=sum(w * h * 4 for _, w, h in pages), env={k: os.environ.get(k) for k in ("TERM", "TERM_PROGRAM", "HERDR_ENV", "HERDR_PANE_ID")})
    try:
        tty.setraw(fi)
        term.t.start()
        term.write(f"{ESC}[?1049h{ESC}[?25l{ESC}[2J{ESC}_Ga=d,d=A,q=2{ESC}\\")
        # 1. upload both pages once
        t0 = time.monotonic()
        for (img, _, _), png in zip(pages, pngs):
            term.write(upload_cmd(img, png))
        tw = time.monotonic()
        term.write(f"{ESC}[c")
        while len(term.acks) < 1 and time.monotonic() - t0 < 30:
            time.sleep(0.001)
        res["upload"] = dict(write_ms=(tw - t0) * 1e3, ack_ms=((term.acks[0] - t0) * 1e3) if term.acks else None,
                             errors=[e[1] for e in term.errors])
        base_acks = len(term.acks)
        nerr0 = len(term.errors)
        # 2. scenario
        n = int(args.seconds * 60)
        updates = []  # (frame index, issue time, write done time)
        cur = set()
        if args.scenario == "trackpad":
            offs = trackpad_offsets(n, lay.max_off)
            plan = {k: offs[k] for k in range(n)}
        else:
            plan = {}
            for k in range(0, n, 2):  # 30 Hz presses
                plan[k] = 0.0 if (k // 2) % 2 == 0 else sum(lay.disp_h[:len(pages) // 2])
        start = time.monotonic() + 0.1
        for k in range(n):
            tk = start + k * FRAME
            now = time.monotonic()
            if now < tk:
                time.sleep(tk - now)
            if k not in plan:
                continue
            now = time.monotonic()
            if now > tk + FRAME:  # we are a full frame behind: skip this update (counted as miss)
                continue
            out = []
            if args.scenario == "held_j_upload":
                half = len(pages) // 2
                sel = range(0, half) if plan[k] == 0.0 else range(half, len(pages))
                for idx in sel:
                    out.append(upload_cmd(pages[idx][0], pngs[idx]))
            pl = lay.placements(plan[k])
            want = {p[0] for p in pl}
            for img in cur - want:
                out.append(delete_cmd(img).encode())
            for p in pl:
                out.append(place_cmd(*p, cols).encode())
            out.append(f"{ESC}[c".encode())
            ti = time.monotonic()
            term.write(b"".join(out))
            updates.append((k, ti, time.monotonic()))
            cur = want
        deadline = time.monotonic() + 3
        while len(term.acks) - base_acks < len(updates) and time.monotonic() < deadline:
            time.sleep(0.005)
    finally:
        term.write(f"{ESC}_Ga=d,d=A,q=2{ESC}\\{ESC}[?25h{ESC}[?1049l")
        time.sleep(0.05)
        term.stop = True
        termios.tcsetattr(fi, termios.TCSADRAIN, old)
    acks = term.acks[base_acks:]
    errs = term.errors[nerr0:]
    lat = []
    ack_of = []
    for i, (k, ti, tw) in enumerate(updates):
        a = acks[i] if i < len(acks) else None
        ack_of.append(a)
        if a is not None:
            lat.append((a - ti) * 1e3)
    # error -> earliest update not yet acked at error time
    bad = set()
    for te, _ in errs:
        for i, a in enumerate(ack_of):
            if a is None or a >= te:
                bad.add(i)
                break
    # frame k is good iff every update issued for frames <= k exists, is acked by tk+16.7ms, error-free
    planned = sorted(plan)
    issued = {k: i for i, (k, _, _) in enumerate(updates)}
    good = 0
    pi = 0
    last_needed = None
    for k in range(n):
        while pi < len(planned) and planned[pi] <= k:
            last_needed = planned[pi]
            pi += 1
        tk_end = start + (k + 1) * FRAME
        if last_needed is None:
            good += 1
            continue
        ok = True
        # all updates up to last_needed must be acked by tk_end; a skipped update is a miss until a later one lands
        i = issued.get(last_needed)
        if i is None or ack_of[i] is None or ack_of[i] > tk_end or i in bad:
            ok = False
        good += ok
    wr = [(tw - ti) * 1e3 for _, ti, tw in updates]
    res.update(frames=n, good_frames=good, good_pct=round(100.0 * good / n, 1), updates_planned=len(plan),
               updates_sent=len(updates), updates_acked=len(lat), kitty_errors=len(errs), error_samples=[e[1] for e in errs[:5]],
               ack_ms=dict(p50=pct(lat, 50), p90=pct(lat, 90), p99=pct(lat, 99), max=max(lat) if lat else None),
               write_ms=dict(p50=pct(wr, 50), p90=pct(wr, 90), max=max(wr) if wr else None),
               scale=lay.scale, view_display_px=lay.view_h)
    return res

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pages", nargs="+", required=True, help="2 pages, or each page as equal-count bands stacked top to bottom (page 1 bands first)")
    ap.add_argument("--scenario", choices=["trackpad", "held_j", "held_j_upload"], required=True)
    ap.add_argument("--seconds", type=float, default=3.0)
    ap.add_argument("--density", default="")
    ap.add_argument("--label", default="")
    ap.add_argument("--out", required=True, help="append one JSON line here")
    a = ap.parse_args()
    res = run(a)
    with open(a.out, "a") as f:
        f.write(json.dumps(res) + "\n")
    print(f"{a.label} {a.scenario} {a.density}: good {res['good_pct']}% of {res['frames']} frames, "
          f"ack p50/p90 {res['ack_ms']['p50'] and round(res['ack_ms']['p50'],1)}/{res['ack_ms']['p90'] and round(res['ack_ms']['p90'],1)} ms, "
          f"errors {res['kitty_errors']}, upload ack {res['upload']['ack_ms'] and round(res['upload']['ack_ms'])} ms")

if __name__ == "__main__":
    main()
