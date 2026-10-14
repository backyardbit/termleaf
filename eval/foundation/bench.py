#!/usr/bin/env python3
import argparse, fcntl, json, math, os, pty, re, resource, select, signal, struct, sys, termios, time, tty

ap = argparse.ArgumentParser()
ap.add_argument("--binary", required=True)
ap.add_argument("--pdf", required=True)
ap.add_argument("--mode", choices=["pty", "proxy"], default="pty")
ap.add_argument("--cols", type=int, default=160)
ap.add_argument("--rows", type=int, default=50)
ap.add_argument("--cell", default="18x36")
ap.add_argument("--protocol", choices=["kitty", "sixel", "iterm2"], default="kitty")
ap.add_argument("--scenario", required=True)
ap.add_argument("--start-page", type=int, default=1)
ap.add_argument("--trace")
ap.add_argument("--prefix", default="")
ap.add_argument("--out", required=True)
ap.add_argument("--quiet", type=float, default=0.6)
ap.add_argument("--presses", type=int, default=45)
ap.add_argument("--force-size", action="store_true", help="proxy mode: give termleaf --cols x --rows instead of the outer size")
ap.add_argument("--cell-px", default="", help="with --force-size: cell pixels WxH instead of the outer terminal's")
a = ap.parse_args()
cw, ch = map(int, (a.cell_px or a.cell).split("x"))

MARK = re.compile(rb"\x1b\]7777;f;(\d+);(\d+);(\d+);(\d+)\x07")
GFX = {"kitty": re.compile(rb"\x1b_G.*?\x1b\\", re.S), "sixel": re.compile(rb"\x1bP.*?\x1b\\", re.S),
       "iterm2": re.compile(rb"\x1b\]1337;File=.*?\x07", re.S)}[a.protocol]
WHEEL_DOWN = b"\x1b[<65;80;25M"
WHEEL_UP = b"\x1b[<64;80;25M"
FRAME = 1000.0 / 60.0
PH = "\U0010EEEE".encode()


def schedule(name):
    ev = []
    t = 0.0
    if name == "hold_j" or name == "hold_k":
        key = b"j" if name == "hold_j" else b"k"
        for _ in range(a.presses):
            ev.append((t, key)); t += 1000 / 30
    elif name == "pagedown_bursts":
        for _ in range(4):
            for _ in range(3):
                ev.append((t, b"j")); t += 2
            t += 500
    elif name == "wheel_trackpad":
        for _ in range(6):
            gaps = [4 + 12 * (i / 39) ** 2 for i in range(40)]
            for g in gaps:
                ev.append((t, WHEEL_DOWN)); t += g
            t += 300
    elif name == "wheel_trackpad_up":
        for _ in range(6):
            gaps = [4 + 12 * (i / 39) ** 2 for i in range(40)]
            for g in gaps:
                ev.append((t, WHEEL_UP)); t += g
            t += 300
    else:
        raise SystemExit(f"unknown scenario {name}")
    return ev


env = dict(os.environ)
env.pop("TERMLEAF_TRACE", None)
if a.trace:
    env["TERMLEAF_TRACE"] = a.trace
env["TERMLEAF_FRAME_MARKS"] = "1"
env.pop("NO_COLOR", None)
if a.mode == "pty":
    env["TERM"] = "xterm-kitty" if a.protocol == "kitty" else "xterm-256color"
    for k in ("TERM_PROGRAM", "LC_TERMINAL", "WEZTERM_EXECUTABLE", "KONSOLE_VERSION", "TMUX", "ITERM_SESSION_ID", "KITTY_WINDOW_ID", "GHOSTTY_RESOURCES_DIR", "ZELLIJ", "HERDR"):
        env.pop(k, None)
    if a.protocol == "iterm2":
        env["TERM_PROGRAM"] = "iTerm.app"
argv = a.prefix.split() + [a.binary, "--no-pinch"] + (["--graphics", a.protocol] if a.mode == "pty" and a.protocol != "kitty" else []) + [a.pdf]

if a.mode == "proxy":
    rows, cols, xpix, ypix = struct.unpack("HHHH", fcntl.ioctl(1, termios.TIOCGWINSZ, b"\0" * 8))
    winsize = (rows, cols, xpix, ypix)
    if a.force_size and cols and rows:
        fw, fh = map(int, a.cell_px.split("x")) if a.cell_px else (xpix // cols, ypix // rows)
        winsize = (a.rows, a.cols, a.cols * fw, a.rows * fh)
    saved = termios.tcgetattr(0)
    tty.setraw(0)
else:
    winsize = (a.rows, a.cols, a.cols * cw, a.rows * ch)

t_spawn = time.monotonic()
pid, fd = pty.fork()
if pid == 0:
    os.execvpe(argv[0], argv, env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", *winsize))

pending = bytearray()
segment = bytearray()
frames = []
samples = []
last_out = [t_spawn]
total = {"bytes": 0}
last_handled = [0]
query_buf = bytearray()
answered = [a.mode == "proxy" and not (a.force_size and a.cell_px)]
KITTY_REPLY = b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;OK\x1b\\"
DA1 = b"\x1b[?62;22c" if a.protocol == "iterm2" else b"\x1b[?62;4c"
REPLY = (KITTY_REPLY if a.protocol == "kitty" else b"") + DA1 + b"\x1b[6;%d;%dt" % (ch, cw) + b"\x1b[0n"


APC = re.compile(rb"\x1b_G([^;\x1b]*)")
images = {"held": {}, "peak": 0, "tail": b"", "sent": 0, "max_px": 0}


def track_images(data):
    buf = images["tail"] + data
    keep = len(buf)
    cut = buf.rfind(b"\x1b_G")
    if cut >= 0 and buf.find(b";", cut) < 0 and buf.find(b"\x1b\\", cut + 3) < 0:
        keep = cut
    else:
        for k in (2, 1):
            if buf.endswith(b"\x1b_G"[:k]):
                keep = len(buf) - k
                break
    images["tail"], buf = buf[keep:], buf[:keep]
    for m in APC.finditer(buf):
        kv = dict(p.split(b"=", 1) for p in m.group(1).split(b",") if b"=" in p)
        act, ident = kv.get(b"a"), kv.get(b"i")
        if act in (b"T", b"t") and ident and b"s" in kv and b"v" in kv:
            images["held"][ident] = int(kv[b"s"]) * int(kv[b"v"]) * 4
            images["sent"] += 1
            images["max_px"] = max(images["max_px"], int(kv[b"s"]) * int(kv[b"v"]))
        elif act == b"d" and ident and kv.get(b"d") in (b"I", b"i"):
            images["held"].pop(ident, None)
        elif act == b"d" and kv.get(b"d") in (b"A", b"a", None):
            images["held"].clear()
        tot = sum(images["held"].values())
        images["peak"] = max(images["peak"], tot)
        if os.environ.get("BENCH_IMGLOG"):
            with open(os.environ["BENCH_IMGLOG"], "a") as f:
                f.write(f"{time.monotonic():.4f} {act.decode() if act else '-'} {ident.decode() if ident else '-'} {images['held'].get(ident, 0)} {tot}\n")


def forward(data):
    if a.mode == "proxy":
        view = memoryview(data)
        while view:
            n = os.write(1, view)
            view = view[n:]


def pump(timeout):
    fds = [fd] + ([0] if a.mode == "proxy" else [])
    r, _, _ = select.select(fds, [], [], max(0.0, timeout))
    if 0 in r:
        data = os.read(0, 65536)
        if data:
            os.write(fd, data)
    if fd not in r:
        return False
    try:
        data = os.read(fd, 1 << 20)
    except OSError:
        raise EOFError
    if not data:
        raise EOFError
    t_read = time.monotonic()
    if not answered[0] and a.mode == "proxy":
        query_buf.extend(data)
        i = query_buf.find(b"\x1b[5n")
        if i < 0:
            return True
        os.write(fd, REPLY)
        answered[0] = True
        data = bytes(query_buf[i + 4:])
        query_buf.clear()
        if not data:
            return True
    elif not answered[0]:
        query_buf.extend(data)
        if b"\x1b[5n" in query_buf:
            os.write(fd, REPLY)
            answered[0] = True
    pending.extend(data)
    while True:
        m = MARK.search(pending)
        if not m:
            cut = pending.rfind(b"\x1b]")
            keep = len(pending) - cut if cut >= 0 and len(pending) - cut < 64 else 0
            body = bytes(pending[: len(pending) - keep])
            del pending[: len(pending) - keep]
            if body:
                track_images(body)
                forward(body)
                segment.extend(body)
                total["bytes"] += len(body)
                last_out[0] = time.monotonic()
            break
        groups = [int(g) for g in m.groups()]
        last_handled[0] = groups[1]
        body = bytes(pending[: m.start()])
        del pending[: m.end()]
        if body:
            track_images(body)
            forward(body)
            segment.extend(body)
            total["bytes"] += len(body)
            last_out[0] = time.monotonic()
        t_done = time.monotonic()
        seg = bytes(segment)
        segment.clear()
        gfx = sum(len(x.group(0)) for x in GFX.finditer(seg))
        if 0 < len(seg) < 200 and len(samples) < 8:
            samples.append(repr(seg))
        frames.append({"t_read": t_read, "t_done": t_done, "frame": groups[0], "handled": groups[1],
                       "holes": groups[2], "cells": groups[3], "bytes": len(seg), "gfx_bytes": gfx,
                       "text_bytes": len(seg) - gfx, "visual": (PH in seg) if a.protocol == "kitty" else gfx > 0})
    return True


def settle(quiet, limit=60.0):
    end = time.monotonic() + limit
    while time.monotonic() < end:
        pump(0.05)
        if time.monotonic() - last_out[0] >= quiet:
            return True
    return False


def send(data):
    os.write(fd, data)


result = {"args": vars(a), "winsize": winsize}
try:
    settle(max(a.quiet, 1.0))
    result["first_view_s"] = round(last_out[0] - t_spawn, 3)
    if a.start_page > 1:
        send(b":%d\r" % a.start_page)
        settle(a.quiet)
    settle(a.quiet)
    frames.clear()
    sent_before = images["sent"]
    base_handled = last_handled[0]
    ev = schedule(a.scenario)
    sent = []
    t0 = time.monotonic() + 0.05
    for offset, data in ev:
        at = t0 + offset / 1000
        while True:
            now = time.monotonic()
            if now >= at:
                break
            pump(at - now)
        sent.append(time.monotonic())
        send(data)
    t_last_input = sent[-1]
    settle(a.quiet)
    t_settled = last_out[0]
finally:
    if a.mode == "proxy":
        pass

kitty_cpu = None
peak_image_bytes = images["peak"]
send(b"q")
t_q = time.monotonic()
try:
    while time.monotonic() - t_q < 5:
        pump(0.2)
except EOFError:
    pass
try:
    os.kill(pid, signal.SIGKILL)
except ProcessLookupError:
    pass
_, status = os.waitpid(pid, 0)
if a.mode == "proxy":
    termios.tcsetattr(0, termios.TCSADRAIN, saved)
ru = resource.getrusage(resource.RUSAGE_CHILDREN)

handled0 = base_handled  # inputs the app had handled when the scenario started
visual = [f for f in frames if f["visual"] and f["t_done"] >= sent[0]]
lat = []
for i, ts in enumerate(sent, start=1):
    hit = next((f for f in visual if f["handled"] - handled0 >= i), None)
    lat.append(None if hit is None else (hit["t_done"] - ts) * 1000)


def pct(xs, p):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    k = (len(xs) - 1) * p / 100
    lo, hi = math.floor(k), math.ceil(k)
    return round(xs[lo] + (xs[hi] - xs[lo]) * (k - lo), 2)


active_end = max([f["t_done"] for f in visual if f["handled"] - handled0 >= len(sent)], default=t_settled)
times = [sent[0]] + [f["t_done"] for f in visual if f["t_done"] <= active_end]
intervals = [(b - c) * 1000 for c, b in zip(times, times[1:])]
dropped = sum(max(0, math.ceil(d / FRAME - 1e-9) - 1) for d in intervals)  # naive, includes idle gaps
# Starved intervals only: the gap counts from when the next not-yet-shown input was sent.
vis_active = [f for f in visual if f["t_done"] <= active_end]
starved = []
prev_t, prev_h = sent[0], 0
for f in vis_active:
    pend = prev_h  # index of first input not reflected
    if pend < len(sent):
        start = max(prev_t, sent[pend])
        if f["t_done"] > start:
            starved.append(((f["t_done"] - start) * 1000, round((start - sent[0]) * 1000, 1)))
    prev_t, prev_h = f["t_done"], max(prev_h, f["handled"] - handled0)
dropped_busy = sum(max(0, math.ceil(d / FRAME - 1e-9) - 1) for d, _ in starved)
slots = max(1, math.ceil((active_end - sent[0]) * 1000 / FRAME))
blank_time = 0.0
for f, nxt in zip(visual, visual[1:] + [None]):
    if f["t_done"] > active_end:
        break
    end = nxt["t_done"] if nxt else active_end
    end = min(end, active_end)
    if f["cells"]:
        blank_time += max(0.0, end - f["t_done"]) * f["holes"] / f["cells"]
stalls = sorted(({"ms": round(d, 1), "at_ms": round((times[i] - sent[0]) * 1000, 1)} for i, d in enumerate(intervals)), key=lambda x: -x["ms"])[:5]
summary = {
    "inputs": len(sent),
    "visual_frames": len([f for f in visual if f["t_done"] <= active_end]),
    "latency_ms": {"p50": pct(lat, 50), "p95": pct(lat, 95), "p99": pct(lat, 99), "max": pct(lat, 100), "missing": lat.count(None)},
    "frame_interval_ms": {"p50": pct(intervals, 50), "p95": pct(intervals, 95), "p99": pct(intervals, 99), "max": pct(intervals, 100)},
    "dropped_60hz": dropped_busy, "dropped_60hz_naive": dropped, "slots_60hz": slots,
    "busy_interval_ms": {"p50": pct([d for d, _ in starved], 50), "p95": pct([d for d, _ in starved], 95), "p99": pct([d for d, _ in starved], 99), "max": pct([d for d, _ in starved], 100)},
    "worst_busy_stalls": [{"ms": round(d, 1), "at_ms": t} for d, t in sorted(starved, key=lambda x: -x[0])[:5]],
    "worst_stalls": stalls,
    "frames_with_holes": sum(1 for f in visual if f["holes"] > 0 and f["t_done"] <= active_end),
    "blank_fraction_time_weighted": round(blank_time / max(1e-9, active_end - sent[0]), 4),
    "text_bytes_per_frame": {"p50": pct([f["text_bytes"] for f in visual], 50), "p95": pct([f["text_bytes"] for f in visual], 95), "max": pct([f["text_bytes"] for f in visual], 100)},
    "gfx_bytes_total": sum(f["gfx_bytes"] for f in frames if f["t_done"] >= sent[0]),
    "bytes_total": sum(f["bytes"] for f in frames if f["t_done"] >= sent[0]),
    "bytes_per_input": round(sum(f["bytes"] for f in frames if f["t_done"] >= sent[0]) / len(sent)),
    "settle_after_last_input_ms": round((t_settled - t_last_input) * 1000, 1),
    "active_ms": round((active_end - sent[0]) * 1000, 1),
    "child_cpu_s": round(ru.ru_utime + ru.ru_stime, 3),
    "peak_image_mib": round(peak_image_bytes / 1048576, 1),
    "peak_image_bytes": peak_image_bytes,
    "tiles_sent": images["sent"] - sent_before,
    "max_tile_px": images["max_px"],
    "child_maxrss_mb": round(ru.ru_maxrss / 1024 / (1024 if sys.platform == "darwin" else 1), 1),
}
result["summary"] = summary
result["first_input_t"] = sent[0]
result["inputs_t"] = [round((t - sent[0]) * 1000, 2) for t in sent]
result["latency_ms"] = [None if x is None else round(x, 2) for x in lat]
result["frames"] = [dict(f, t_ms=round((f["t_done"] - sent[0]) * 1000, 2), read_ms=round((f["t_read"] - sent[0]) * 1000, 2)) for f in frames]
for f in result["frames"]:
    f.pop("t_done"); f.pop("t_read")
result["handled0"] = handled0
result["small_segments"] = samples
result["exit"] = status
json.dump(result, open(a.out, "w"), indent=1)
if a.mode == "pty":
    print(json.dumps({"scenario": a.scenario, "pdf": os.path.basename(a.pdf), **{k: summary[k] for k in ("latency_ms", "frame_interval_ms", "dropped_60hz", "frames_with_holes", "blank_fraction_time_weighted", "bytes_per_input")}}))
