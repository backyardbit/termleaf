import json, os, socket, subprocess, sys, time

SESSION = "exp"
WORK = os.path.abspath(sys.argv[1])
TERMLEAF = os.path.abspath(sys.argv[2])
PDF = os.path.abspath(sys.argv[3])
os.makedirs(WORK, exist_ok=True)


def herdr(*args):
    r = subprocess.run(["herdr", "--session", SESSION, *args], capture_output=True, text=True, timeout=30)
    return r.stdout


def jherdr(*args):
    try:
        return json.loads(herdr(*args))["result"]
    except Exception:
        return None


def screenshot():
    raw = subprocess.run(["import", "-window", "root", "ppm:-"], capture_output=True).stdout
    parts = raw.split(b"\n", 3)
    return parts[3]


def difference(a, b):
    if len(a) != len(b):
        return 1.0
    step = 3 * 7
    total = changed = 0
    for i in range(0, len(a) - 3, step):
        total += 1
        if a[i:i + 3] != b[i:i + 3]:
            changed += 1
    return changed / max(total, 1)


def wait_until(predicate, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    return None


def settled(seconds=1.5, timeout=20):
    last = screenshot()
    stable_since = time.monotonic()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        time.sleep(0.15)
        now = screenshot()
        if difference(last, now) > 0.0005:
            stable_since = time.monotonic()
            last = now
        elif time.monotonic() - stable_since >= seconds:
            return now
    return last


def tab_ids():
    tabs = jherdr("tab", "list")["tabs"]
    return [tab["tab_id"] for tab in tabs]


def measure(label, home_tab, away_tab, rounds=5):
    herdr("tab", "focus", home_tab)
    reference = settled()
    results = []
    for round in range(rounds):
        herdr("tab", "focus", away_tab)
        away = settled(1.0)
        if difference(reference, away) < 0.01:
            print(f"{label}: switching away did not change the screen", flush=True)
        start = time.monotonic()
        MARKS.append((label, round, start))
        herdr("tab", "focus", home_tab)
        while True:
            shot = screenshot()
            elapsed = time.monotonic() - start
            if difference(reference, shot) < 0.002:
                break
            if elapsed > 30:
                elapsed = float("inf")
                break
        results.append(elapsed)
        print(f"{label} round {round}: back to the reference image after {elapsed:.2f}s", flush=True)
    finite = sorted(r for r in results if r != float("inf"))
    print(f"RESULT {label}: median {finite[len(finite)//2] if finite else float('inf'):.2f}s all={['%.2f' % r for r in results]}", flush=True)


def graphics_socket():
    return os.path.expanduser(f"~/.config/herdr/sessions/{SESSION}/herdr.sock")


def request(sock, method, params):
    sock.sendall((json.dumps({"id": method, "method": method, "params": params}) + "\n").encode())
    return readline(sock)


def readline(sock):
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = sock.recv(1)
        if not chunk:
            break
        buf += chunk
    return json.loads(buf) if buf.strip() else None


def open_socket():
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(graphics_socket())
    return s


def page_pixels(width, height):
    row = bytearray()
    for x in range(width):
        row += b"\x20\x20\x20\xff" if (x // 3) % 5 == 0 else b"\xff\xff\xff\xff"
    blank = b"\xff\xff\xff\xff" * width
    out = bytearray()
    for y in range(height):
        out += row if (y // 20) % 2 == 0 and (y % 20) < 12 else blank
    return bytes(out)


def main():
    env = dict(os.environ, GDK_BACKEND="x11", LIBGL_ALWAYS_SOFTWARE="1", WIRE_LOG=os.path.join(WORK, "wire.log"))
    env.pop("DBUS_SESSION_BUS_ADDRESS", None)
    ghostty = subprocess.Popen(
        ["ghostty", "--gtk-single-instance=false", "--config-default-files=false",
         "--window-decoration=false", "--font-size=18", "--window-width=150", "--window-height=46",
         "-e", "python3", os.path.join(os.path.dirname(os.path.abspath(__file__)), "wire_proxy.py"), "herdr", "--session", SESSION],
        env=env, stdout=subprocess.DEVNULL, stderr=open(os.path.join(WORK, "ghostty.log"), "w"))
    try:
        pane = wait_until(lambda: (jherdr("pane", "list") or {}).get("panes", [{}])[0].get("pane_id"), 60)
        print("pane", pane, flush=True)
        herdr("pane", "run", pane, f"'{TERMLEAF}' --no-pinch '{PDF}'")
        time.sleep(4)
        settled()
        herdr("tab", "create")
        time.sleep(1)
        tabs = tab_ids()[:2]
        herdr("tab", "create")
        time.sleep(1)
        text_tab = tab_ids()[-1]
        text_pane = [p["pane_id"] for p in jherdr("pane", "list")["panes"] if p["tab_id"] == text_tab][0]
        herdr("pane", "run", text_pane, "ls -la /usr/bin | head -60")
        time.sleep(1)
        measure("control-text-only", text_tab, tabs[-1])
        print("tabs", tabs, flush=True)
        home, away = tabs[0], tabs[-1]
        measure("termleaf-tiles", home, away)

        herdr("pane", "send-text", pane, "q")
        time.sleep(1)
        herdr("pane", "run", pane, "clear; sleep 100000")
        time.sleep(1)
        info = request(open_socket(), "pane.graphics.info", {"pane_id": pane})
        print("info", json.dumps(info), flush=True)
        result = info["result"]
        layout = jherdr("pane", "layout")
        print("layout", json.dumps(layout)[:400], flush=True)
        cols = layout["layout"]["panes"][0]["rect"]["width"] - 2
        rows = layout["layout"]["panes"][0]["rect"]["height"] - 2
        width, height = cols * result["cell_width_px"], rows * result["cell_height_px"]
        print(f"frame {cols}x{rows} cells = {width}x{height} px = {width*height*4} bytes", flush=True)
        pixels = page_pixels(width, height)
        stream = open_socket()
        print("stream", request(stream, "pane.graphics.stream", {"pane_id": pane, "z_index": 0}), flush=True)
        header = {"format": "rgba", "image_width": width, "image_height": height,
                  "placement": {"viewport_col": 0, "viewport_row": 0, "grid_cols": cols, "grid_rows": rows}}
        directory = result.get("file_frame_directory")
        if directory and result.get("file_frame_transport") == "direct-kitty":
            path = os.path.join(directory, "frame-1.rgba")
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.write(fd, pixels)
            os.close(fd)
            header.update(sequence=1, revision=1, transport="direct-kitty", file={"path": path})
            label = "direct-file-frame"
            started = time.monotonic()
            stream.sendall((json.dumps(header) + "\n").encode())
        else:
            header["data_length"] = len(pixels)
            label = "inline-frame"
            started = time.monotonic()
            stream.sendall((json.dumps(header) + "\n").encode() + pixels)
        print("ack", json.dumps(readline(stream)), f"after {time.monotonic()-started:.3f}s", flush=True)
        time.sleep(2)
        measure(label, home, away)
        if label == "direct-file-frame":
            throughput(stream, header, pixels, directory, width)

    finally:
        herdr("server", "stop")
        ghostty.kill()
        summarise()


def summarise():
    marks = []
    for line in open(os.path.join(WORK, "stdout.log")) if os.path.exists(os.path.join(WORK, "stdout.log")) else []:
        pass
    path = os.path.join(WORK, "wire.log")
    if not os.path.exists(path):
        print("no wire log"); return
    lines = [l.split(" ", 4) for l in open(path)]
    for label, round, start in MARKS:
        window = [l for l in lines if l[1] == "out" and start <= float(l[0]) <= start + 3]
        total = sum(int(l[2]) for l in window)
        heads = [h for l in window for h in (l[4].strip().split(" | ") if len(l) > 4 and l[4].strip() else [])]
        kinds = {}
        for h in heads:
            key = ",".join(p for p in h.split(",") if p.split("=")[0] in ("a", "t", "f", "q"))
            kinds[key] = kinds.get(key, 0) + 1
        print(f"WIRE {label} {round}: {total} bytes out in 3s, commands {kinds}", flush=True)
    for l in lines:
        if l[1] == "in":
            print("WIRE reply", " ".join(l)[:200].strip(), flush=True)


MARKS = []


def throughput(stream, header, pixels, directory, width, frames=60):
    previous = []
    timings = []
    started = time.monotonic()
    for sequence in range(2, 2 + frames):
        shift = (sequence * 4 * width * 3) % len(pixels)
        frame = pixels[shift:] + pixels[:shift]
        path = os.path.join(directory, f"frame-{sequence}.rgba")
        t0 = time.monotonic()
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.write(fd, frame)
        os.close(fd)
        written = time.monotonic()
        stream.sendall((json.dumps(dict(header, sequence=sequence, revision=sequence, file={"path": path})) + "\n").encode())
        reply = readline(stream)
        acked = time.monotonic()
        if not reply or "result" not in reply:
            print("throughput frame failed", reply, flush=True)
            break
        timings.append((written - t0, acked - written))
        previous.append(path)
        while len(previous) > 3:
            os.unlink(previous.pop(0))
    total = time.monotonic() - started
    writes = sorted(t[0] for t in timings)
    acks = sorted(t[1] for t in timings)
    print(f"THROUGHPUT {len(timings)} frames in {total:.2f}s = {len(timings)/total:.1f} fps; "
          f"write median {writes[len(writes)//2]*1000:.1f}ms; ack median {acks[len(acks)//2]*1000:.1f}ms p90 {acks[int(len(acks)*0.9)]*1000:.1f}ms", flush=True)


main()
