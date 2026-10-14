#!/usr/bin/env python3
"""summarize_ab.py runs/PREFIX [...]: A (cf54baf) vs B (master 70bbc34) hold_j side by side, from runs/PREFIX-A and runs/PREFIX-B."""
import glob, json, math, os, statistics, sys

def pct(xs, p):
    xs = sorted(x for x in xs if x is not None and not (isinstance(x, float) and math.isnan(x)))
    if not xs: return float("nan")
    k = (len(xs) - 1) * p / 100; lo, hi = math.floor(k), math.ceil(k)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)

def stat(xs, nd=1):
    xs = [x for x in xs if x is not None]
    return f"{statistics.median(xs):.{nd}f} [{min(xs):.{nd}f}–{max(xs):.{nd}f}]" if xs else "n/a"

def blank(d, until=None):
    s = d["summary"]; end = s["active_ms"] if until is None else min(until, s["active_ms"])
    fr = [f for f in d["frames"] if f["t_ms"] >= 0 and f["cells"]]
    tot = 0.0
    for f, n in zip(fr, fr[1:] + [None]):
        if f["t_ms"] > end: break
        tot += max(0.0, min(n["t_ms"] if n else end, end) - f["t_ms"]) * f["holes"] / f["cells"]
    return 100 * tot / max(end, 1e-9)

def vsettle(d):
    t = d["inputs_t"][-1]; fr = [f for f in d["frames"] if f["cells"]]
    holed = [i for i, f in enumerate(fr) if f["holes"]]
    if not holed or holed[-1] + 1 >= len(fr): return float("nan") if holed else 0.0
    return max(0.0, fr[holed[-1] + 1]["t_ms"] - t)

def load_trace(p):
    ev = []
    for l in open(p):
        l = l.strip().rstrip(",")
        if l and l not in "[]":
            try: ev.append(json.loads(l))
            except ValueError: pass
    return ev

def stages(bench, ev):
    bf = {f["frame"]: f for f in bench["frames"]}
    tf = {int(e["args"]["frame"]): e["ts"] / 1000 for e in ev if e["name"] == "frame"}
    common = [k for k in tf if k in bf]
    if not common: return {}
    off = min(bf[k]["t_ms"] - tf[k] for k in common); T = lambda us: us / 1000 + off
    sent = bench["inputs_t"]; t0, t1 = sent[0], sent[-1]
    E = lambda n, ph=None: [e for e in ev if e["name"] == n and (ph is None or e["ph"] == ph)]
    keyh = sorted(T(e["ts"]) for e in E("handle") if e["args"].get("kind") == "key")
    inputs = [T(e["ts"]) for e in E("input")][-len(sent) - 1:-1]
    out = {"key read → UI handles it": [min([h for h in keyh if h >= i - 0.05] or [float("nan")]) - i for i in inputs]}
    req = {}
    for e in E("render_request"): req.setdefault(int(e["args"]["page"]), T(e["ts"]))
    R = [(int(e["args"]["page"]), T(e["ts"]), e["dur"] / 1000, int(e["args"]["w"]) * int(e["args"]["h"])) for e in E("render", "X")]
    EN = [(int(e["args"]["page"]), T(e["ts"]), e["dur"] / 1000) for e in E("encode", "X")]
    A = [(int(e["args"]["page"]), T(e["ts"])) for e in E("encoded_arrival")]
    Q = [(int(e["args"]["page"]), T(e["ts"] + e["dur"]), int(e["args"]["bytes"])) for e in E("transmit_queue", "X")]
    FL = sorted((T(e["ts"]), T(e["ts"] + e["dur"])) for e in E("flush_tx", "X"))
    cols = {k: [] for k in ("handle → render request", "request → raster starts", "raster, all tiles of the page", "raster, largest tile",
                            "tiles per page", "raster end → encoded", "encoded → UI picks it up", "UI → write done", "key handled → page written", "gfx KB per page")}
    for p, tr in req.items():
        if not (t0 - 1 <= tr <= t1 + 50): continue
        rs = [r for r in R if r[0] == p and r[1] >= tr - 0.5]; qs = [q for q in Q if q[0] == p and q[1] >= tr]
        if not rs or not qs: continue
        tk = max([h for h in keyh if h <= tr] or [tr]); rs0 = min(r[1] for r in rs); rs1 = max(r[1] + r[2] for r in rs)
        es = [e for e in EN if e[0] == p and e[1] >= rs0 - 0.5]; e1 = max((e[1] + e[2] for e in es), default=float("nan"))
        a1 = max([a[1] for a in A if a[0] == p and a[1] >= rs0] or [float("nan")]); q1 = max(q[1] for q in qs)
        f1 = next((b for a, b in FL if b >= q1), float("nan"))
        for k, v in (("handle → render request", tr - tk), ("request → raster starts", rs0 - tr), ("raster, all tiles of the page", rs1 - rs0),
                     ("raster, largest tile", max(rs, key=lambda r: r[3])[2]), ("tiles per page", len(rs)), ("raster end → encoded", e1 - rs1),
                     ("encoded → UI picks it up", a1 - e1), ("UI → write done", f1 - a1), ("key handled → page written", f1 - tk), ("gfx KB per page", sum(q[2] for q in qs) / 1000)):
            cols[k].append(v)
    out.update(cols)
    return out

def reps(d):
    per, traced = {}, {}
    for f in sorted(glob.glob(os.path.join(d, "*__hold_j__r*.json"))):
        if f.endswith(".trace.json"): continue
        j = json.load(open(f)); pdf = os.path.basename(f).split("__")[0]
        if os.path.exists(f[:-5] + ".trace.json"): traced[pdf] = (j, f[:-5] + ".trace.json")
        else: per.setdefault(pdf, []).append(j)
    return per, traced

for prefix in sys.argv[1:]:
    A, TA = reps(prefix + "-A"); B, TB = reps(prefix + "-B")
    ws = next((js[0]["winsize"] for js in list(A.values()) + list(B.values())), None)
    print("\nvisible settle = last input → first frame with every visible tile drawn (after the last frame with holes); output settle = last input → last byte termleaf wrote (includes prefetch).")
    print(f"\n### {os.path.basename(prefix)}: hold_j, A = cf54baf, B = master 70bbc34 (#51 + #52); median [min–max] over untraced reps" + (f"; pane {ws[1]}x{ws[0]} cells, {ws[2]}x{ws[3]} px" if ws else ""))
    print("\nSide by side, median [min–max]:\n")
    print("| pdf | blank % held A | blank % held B | blank % whole A | blank % whole B | visible settle ms A | visible settle ms B | output settle ms A | output settle ms B | first-content p95 ms A | first-content p95 ms B |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for pdf in sorted(set(A) | set(B)):
        c = []
        for f in (lambda j: blank(j, j["inputs_t"][-1]), blank, vsettle, lambda j: j["summary"]["settle_after_last_input_ms"], lambda j: j["summary"]["latency_ms"]["p95"]):
            for R in (A, B):
                js = [j for j in R.get(pdf, []) if any(fr["cells"] for fr in j["frames"])]
                c.append(stat([f(j) for j in js], 1 if f in (blank,) or c == [] or len(c) < 4 else 0) if js else "n/a")
        print(f"| {pdf} | " + " | ".join(c) + " |")
    print("\nDetail:")
    print("\n| pdf | build | n | blank % whole | blank % held | settle ms | first-content p95 ms | peak image MiB | RSS MiB | tiles sent per press | max tile px | gfx KB per tile | CPU s |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for pdf in sorted(set(A) | set(B)):
        for name, R in (("A", A), ("B", B)):
            js = R.get(pdf, [])
            if not js: continue
            S = [j["summary"] for j in js]
            if not any(f["cells"] for j in js for f in j["frames"]):
                print(f"| {pdf} | {name} | {len(js)} | NO FRAME MARKS (not a trace.patch build) ||||||||||"); continue
            tps = [s.get("tiles_sent", 0) / max(1, s["inputs"]) for s in S]
            kbt = [s["gfx_bytes_total"] / 1000 / max(1, s.get("tiles_sent", 0)) for s in S if s.get("tiles_sent")]
            print(f"| {pdf} | {name} | {len(js)} | {stat([blank(j) for j in js])} | {stat([blank(j, j['inputs_t'][-1]) for j in js])} | "
                  f"{stat([s['settle_after_last_input_ms'] for s in S], 0)} | {stat([s['latency_ms']['p95'] for s in S], 0)} | "
                  f"{max(s['peak_image_bytes'] for s in S) / 1048576:.2f} | {max(s.get('child_maxrss_mb', 0) for s in S):.0f} | {stat(tps, 2)} | {max(s.get('max_tile_px', 0) for s in S)} | {stat(kbt, 0)} | {stat([s['child_cpu_s'] for s in S], 2)} |")
    rows = {}
    for name, T in (("A", TA), ("B", TB)):
        for pdf, (j, t) in T.items():
            for k, v in stages(j, load_trace(t)).items():
                rows.setdefault(k, {})[(pdf, name)] = f"{pct(v, 50):.1f} / {pct(v, 90):.1f}"
    if rows:
        keys = sorted({k for r in rows.values() for k in r})
        print("\nTraced rep, ms p50 / p90 per page written during the hold:\n")
        print("| stage | " + " | ".join(f"{p} {b}" for p, b in keys) + " |")
        print("|---|" + "---|" * len(keys))
        for k, r in rows.items():
            print(f"| {k} | " + " | ".join(r.get(c, "–") for c in keys) + " |")
