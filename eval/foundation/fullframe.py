#!/usr/bin/env python3
"""fullframe.py PREFIX: share of 60 Hz slots during the hold (first press -> last press + 0, and whole run to settle)
in which termleaf's latest drawn frame had every visible tile present (holes == 0). termleaf-side, from bench
frame marks; it does not see the terminal's own presentation."""
import glob, json, os, statistics, sys
def share(js, until_settle):
    fr = list(js["frames"])  # every frame mark, incl. the clearing frame written right after a key
    ins = js.get("inputs_t") or []
    if not fr or not ins:
        return None
    start = 0.0
    end = ins[-1] - ins[0]  # ms, relative to the first press like t_ms
    if until_settle:
        end = max(end, max(f["t_ms"] for f in fr))
    slots, good, k, cur = 0, 0, 0, None
    fr.sort(key=lambda f: f["t_ms"])
    t = start
    while t <= end:
        while k < len(fr) and fr[k]["t_ms"] <= t:
            cur = fr[k]; k += 1
        slots += 1
        good += bool(cur and cur["holes"] == 0 and cur["cells"] > 0)
        t += 1000 / 60
    return 100.0 * good / slots if slots else None
scen = os.environ.get("SCEN", "hold_j")
for prefix in sys.argv[1:]:
    print(f"\n### {os.path.basename(prefix)}: {scen} full-frame 60 Hz % (termleaf-side), median [min-max]\n")
    print("| pdf | build | n | held | whole | settle ms | bench blank % |\n|---|---|---|---|---|---|---|")
    for b in ("A", "B"):
        per = {}
        for f in sorted(glob.glob(f"{prefix}-{b}/*__{scen}__r*.json")):
            if f.endswith(".trace.json"):
                continue
            js = json.load(open(f))
            if js["args"].get("trace"):
                continue
            pdf = os.path.basename(f).split("__")[0]
            per.setdefault(pdf, []).append((share(js, False), share(js, True), js["summary"]["settle_after_last_input_ms"], 100 * js["summary"]["blank_fraction_time_weighted"]))
        for pdf, v in sorted(per.items()):
            h = [x[0] for x in v if x[0] is not None]; w = [x[1] for x in v if x[1] is not None]
            st = [x[2] for x in v]; bl = [x[3] for x in v]
            fmt = lambda xs: f"{statistics.median(xs):.1f} [{min(xs):.1f}–{max(xs):.1f}]" if xs else "-"
            print(f"| {pdf} | {b} | {len(v)} | {fmt(h)} | {fmt(w)} | {fmt(st)} | {fmt(bl)} |")
