#!/usr/bin/env python3
"""summarize_probe.py RUNS_DIR: table of probe-herdr.jsonl / probe-direct.jsonl (median [min-max] over reps)."""
import json, os, statistics, sys
d = sys.argv[1]
TARGET = {"trackpad": 95.0, "held_j": 80.0, "held_j_upload": 80.0}
rows = {}
for label in ("herdr", "direct"):
    p = os.path.join(d, f"probe-{label}.jsonl")
    if not os.path.exists(p):
        continue
    for line in open(p):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        rows.setdefault((label, r["scenario"], r["density"]), []).append(r)
def mr(xs, f="{:.1f}"):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "-"
    return f"{f.format(statistics.median(xs))} [{f.format(min(xs))}–{f.format(max(xs))}]"
print("## Resident-page probe (PROXY metric: share of 60 Hz frames whose latest placement update was acknowledged")
print("by the terminal - DA1 round-trip - within the frame, with no Kitty error reply. Under herdr the ack comes from")
print("herdr's pane emulator, not from Ghostty's screen; see README.md)\n")
print("| where | scenario | density | n | good frames % | target | verdict | ack p50 ms | ack p90 ms | ack max ms | write p90 ms | kitty errors | initial upload ack ms | pane |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
order = {"trackpad": 0, "held_j": 1, "held_j_upload": 2}
for key in sorted(rows, key=lambda k: (k[0] != "herdr", order.get(k[1], 9), k[2])):
    rs = rows[key]
    g = [r["good_pct"] for r in rs]
    t = TARGET.get(key[1], 0)
    verdict = "PASS" if statistics.median(g) >= t else "FAIL"
    if key[1] == "held_j_upload":
        verdict += " (contrast: re-uploads every press)"
    pane = rs[0]["pane"]
    print(f"| {key[0]} | {key[1]} | {key[2]} | {len(rs)} | {mr(g)} | ≥{t:.0f} | {verdict} | "
          f"{mr([r['ack_ms']['p50'] for r in rs])} | {mr([r['ack_ms']['p90'] for r in rs])} | {mr([r['ack_ms']['max'] for r in rs])} | "
          f"{mr([r['write_ms']['p90'] for r in rs])} | {sum(r['kitty_errors'] for r in rs)} | {mr([r['upload']['ack_ms'] for r in rs], '{:.0f}')} | "
          f"{pane['cols']}x{pane['rows']} {pane['xpx']}x{pane['ypx']} |")
errs = sorted({e for rs in rows.values() for r in rs for e in r.get("error_samples", []) + r["upload"].get("errors", [])})
if errs:
    print("\nKitty error replies seen (first few): " + "; ".join(errs[:6]))
print("\nretina = pages rendered at 2596 px wide (2 pages = 76.3 MB raw RGBA, above herdr's 64 MiB per-pane cap);"
      " d075 = 0.75x (1947 px wide, 42.9 MB raw), scaled up to the pane by the terminal.")
if not rows:
    print("\n(no probe results found)")
