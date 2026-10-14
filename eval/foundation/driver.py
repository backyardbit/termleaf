#!/usr/bin/env python3
"""Run the scroll benchmark matrix sequentially. Used by run.sh (box) and run-mac.sh (Mac).

driver.py --label NAME --mode pty|proxy [--protocol kitty] [--reps 2] [--trace-rep] [--only paper,progit]
Writes runs/NAME/<pdf>__<scenario>__r<rep>.json (+ .trace.json for the traced rep).
"""
import argparse, os, shutil, subprocess, sys, time, json
os.environ.pop("NO_COLOR", None)

HERE = os.path.dirname(os.path.abspath(__file__))
# pdf key -> (file, [(scenario, start_page, presses)])
MATRIX = {
    "paper": ("paper-llama-2302.13971v1.pdf", [("hold_j", 2, 20), ("hold_k", 25, 20), ("pagedown_bursts", 2, 0), ("wheel_trackpad", 2, 0)]),
    "progit": ("progit-2.1.450.pdf", [("hold_j", 100, 45), ("hold_k", 200, 45), ("pagedown_bursts", 100, 0), ("wheel_trackpad", 100, 0), ("wheel_trackpad_up", 120, 0)]),
    "pgfplots": ("pgfplots-manual.pdf", [("hold_j", 150, 45), ("hold_k", 250, 45), ("pagedown_bursts", 150, 0), ("wheel_trackpad", 150, 0), ("wheel_trackpad_up", 170, 0)]),
}

ap = argparse.ArgumentParser()
ap.add_argument("--label", required=True)
ap.add_argument("--mode", default="pty")
ap.add_argument("--protocol", default="kitty")
ap.add_argument("--reps", type=int, default=2)
ap.add_argument("--trace-rep", action="store_true", help="add one extra rep with TERMLEAF_TRACE")
ap.add_argument("--only", default="paper,progit,pgfplots")
ap.add_argument("--scenarios", default="")
ap.add_argument("--binary", default=os.path.join(HERE, "bin", "termleaf-trace"))
ap.add_argument("--pdfdir", default=os.path.join(HERE, "pdfs"))
ap.add_argument("--extra", default="", help="extra args for bench.py, e.g. '--force-size --cols 140 --rows 46'")
ap.add_argument("--max-load", type=float, default=8.0)
ap.add_argument("--rep-start", type=int, default=1, help="number of the first rep (for interleaving several driver calls)")
ap.add_argument("--presses", type=int, default=0, help="override the matrix key-press count (smoke tests)")
ap.add_argument("--rep-timeout", type=float, default=180.0)
a = ap.parse_args()

out = os.path.join(HERE, "runs", a.label)
os.makedirs(out, exist_ok=True)
log = open(os.path.join(out, "driver.log"), "a")


def wait_load():
    while True:
        try:
            l1 = os.getloadavg()[0]
        except OSError:
            return
        if l1 <= a.max_load:
            return
        log.write(f"load {l1:.1f} > {a.max_load}, waiting\n"); log.flush()
        time.sleep(15)


want = [s for s in a.scenarios.split(",") if s]
for key in a.only.split(","):
    fname, scen = MATRIX[key]
    for scenario, start, presses in scen:
        if want and scenario not in want:
            continue
        reps = [(a.rep_start + r - 1, False) for r in range(1, a.reps + 1)] + ([(a.rep_start + a.reps, True)] if a.trace_rep else [])
        for rep, traced in reps:
            wait_load()
            stem = os.path.join(out, f"{key}__{scenario}__r{rep}")
            cmd = [sys.executable, os.path.join(HERE, "bench.py"), "--binary", a.binary, "--pdf", os.path.join(a.pdfdir, fname),
                   "--mode", a.mode, "--protocol", a.protocol, "--scenario", scenario, "--start-page", str(start), "--out", stem + ".json"]
            if a.presses or presses:
                cmd += ["--presses", str(a.presses or presses)]
            if traced:
                cmd += ["--trace", stem + ".trace.json"]
            cmd += a.extra.split()
            t = time.monotonic()
            try:
                tmo = shutil.which("timeout") or shutil.which("gtimeout")  # macOS: coreutils gtimeout, else none
                pre = [tmo, "--foreground", "150"] if tmo else []
                rc = subprocess.run(pre + cmd, stdout=None if a.mode == "proxy" else log, stderr=log, timeout=a.rep_timeout).returncode
            except subprocess.TimeoutExpired:
                rc = "timeout"
            except Exception as e:  # keep going
                rc = repr(e)
            log.write(f"{time.strftime('%H:%M:%S')} {key} {scenario} r{rep} traced={traced} rc={rc} {time.monotonic()-t:.1f}s\n"); log.flush()
            time.sleep(0.5)
log.write("DONE\n"); log.flush()
