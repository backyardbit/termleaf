#!/bin/sh
# Runs the probe matrix in the current terminal. Env: PROBE_OUT PROBE_LABEL PROBE_REPS PROBE_SECONDS
set -eu
OLDPWD_E=$PWD; cd "$(dirname "$0")"
r=1
while [ "$r" -le "$PROBE_REPS" ]; do
  for d in retina d075; do
    for s in trackpad held_j held_j_upload; do
      python3 probe.py --pages "$d-100.png" "$d-101.png" --scenario "$s" --seconds "$PROBE_SECONDS" \
        --density "$d" --label "$PROBE_LABEL" --out "$OLDPWD_E/$PROBE_OUT" || echo "probe failed: $s $d rep $r" >&2
      sleep 0.3
    done
  done
  r=$((r + 1))
done
